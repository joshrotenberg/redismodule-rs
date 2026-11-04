//! Owned compiler processes, bounded admission and deadline-driven pipe IO.
//! No worker acquires Redis's lock: shutdown can kill/reap/join under that lock.
use crate::{
    compiler_engine, compiler_pipe::exchange_io, compiler_protocol as protocol,
    compiler_source::Source,
};
use anyhow::{Context as _, bail, ensure};
use redis_module::{EventContext, logging};
use redis_module_macros::shutdown_event_handler;
use std::{
    collections::VecDeque,
    ffi::{CStr, OsStr},
    fs::File,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use wasmtime::{Engine, component::Component};

pub(crate) const DEFAULT_MEMORY: i64 = 2 << 30;
pub(crate) const DEFAULT_CPU: i64 = 60;
pub(crate) const DEFAULT_WALL: i64 = 120;
pub(crate) const DEFAULT_QUEUE: i64 = 8;
pub(crate) static MEMORY: AtomicI64 = AtomicI64::new(DEFAULT_MEMORY);
pub(crate) static CPU: AtomicI64 = AtomicI64::new(DEFAULT_CPU);
pub(crate) static WALL: AtomicI64 = AtomicI64::new(DEFAULT_WALL);
pub(crate) static QUEUE: AtomicI64 = AtomicI64::new(DEFAULT_QUEUE);
static ACTIVE: Mutex<Weak<Supervisor>> = Mutex::new(Weak::new());

#[derive(Clone, Copy)]
struct Limits {
    memory: u64,
    cpu: u64,
    wall: Duration,
    queue: usize,
}

struct State {
    next: u64,
    queued: VecDeque<u64>,
    active: Option<u64>,
    child: Option<u32>,
    stopped: bool,
}

pub(crate) struct Supervisor {
    helper: PathBuf,
    limits: Limits,
    state: Mutex<State>,
    changed: Condvar,
    workers: Mutex<Vec<JoinHandle<()>>>,
    completed: AtomicU64,
    failed: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Supervisor {
    pub(crate) fn new() -> anyhow::Result<Arc<Self>> {
        let limits = Limits {
            memory: u64::try_from(MEMORY.load(Ordering::Relaxed))?,
            cpu: u64::try_from(CPU.load(Ordering::Relaxed))?,
            wall: Duration::from_secs(u64::try_from(WALL.load(Ordering::Relaxed))?),
            queue: usize::try_from(QUEUE.load(Ordering::Relaxed))?,
        };
        let supervisor = Arc::new(Self {
            helper: helper_path()?,
            limits,
            state: Mutex::new(State {
                next: 0,
                queued: VecDeque::new(),
                active: None,
                child: None,
                stopped: false,
            }),
            changed: Condvar::new(),
            workers: Mutex::new(Vec::new()),
            completed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        });
        *lock(&ACTIVE) = Arc::downgrade(&supervisor);
        Ok(supervisor)
    }

    /// Reserve before source spooling/copying and before creating a worker.
    /// Capacity includes one active operation and at most `queue` waiting ones.
    /// A restore/RDB batch owns one reservation and compiles sequentially.
    pub(crate) fn reserve(self: &Arc<Self>) -> anyhow::Result<Reservation> {
        let mut state = lock(&self.state);
        ensure!(!state.stopped, "compiler is shut down");
        ensure!(
            state.queued.len() + usize::from(state.active.is_some()) < self.limits.queue + 1,
            "compiler queue is full"
        );
        state.next = state
            .next
            .checked_add(1)
            .context("compiler admission identity exhausted")?;
        let id = state.next;
        state.queued.push_back(id);
        Ok(Reservation {
            supervisor: Arc::clone(self),
            id,
            entered: false,
        })
    }

    pub(crate) fn spawn(
        self: &Arc<Self>,
        work: impl FnOnce() + Send + 'static,
    ) -> anyhow::Result<()> {
        // Reap finished handles during ordinary operation, avoiding an
        // ever-growing registry after many successful loads.
        let mut workers = lock(&self.workers);
        let mut pending = Vec::new();
        for worker in workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                pending.push(worker);
            }
        }
        *workers = pending;
        ensure!(!lock(&self.state).stopped, "compiler is shut down");
        workers.push(
            std::thread::Builder::new()
                .name("wasm-compiler-operation".into())
                .spawn(work)?,
        );
        Ok(())
    }

    pub(crate) fn shutdown(&self) {
        {
            let mut state = lock(&self.state);
            state.stopped = true;
            if let Some(pid) = state.child {
                // The child is unreaped and remains owned while this mutex is
                // held, so its PID cannot refer to a different process.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
            self.changed.notify_all();
        }
        // Workers only use pipe IO, Wasmtime linking, Redis's allocator and
        // thread-safe UnblockClient; none waits for the Redis lock.
        for worker in std::mem::take(&mut *lock(&self.workers)) {
            let _ = worker.join();
        }
    }

    fn run(&self, engine: &Engine, source: &Source) -> anyhow::Result<Component> {
        let started = Instant::now();
        let result = self.exchange(engine, source);
        match &result {
            Ok(_) => {
                self.completed.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                self.failed.fetch_add(1, Ordering::Relaxed);
                logging::log_warning(format!(
                    "wasm compiler failed after {} ms: {error:#}",
                    started.elapsed().as_millis()
                ));
            }
        }
        result
    }

    fn exchange(&self, engine: &Engine, source: &Source) -> anyhow::Result<Component> {
        ensure!(
            source.len() <= protocol::MAX_INPUT,
            "component exceeds compiler input bound"
        );
        let mut nonce = [0; 32];
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let fingerprint = compiler_engine::fingerprint(engine);
        let request = protocol::request(nonce, fingerprint, source.len())?;
        // Resolve a fixed sibling executable, never PATH or a command input.
        let path = self
            .helper
            .canonicalize()
            .with_context(|| format!("compiler helper is missing: {}", self.helper.display()))?;
        ensure!(path.is_file(), "compiler helper is not a regular file");
        let child = Command::new(path)
            .args([self.limits.memory.to_string(), self.limits.cpu.to_string()])
            .env("MallocNanoZone", "0")
            .env("RAYON_NUM_THREADS", "2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start bounded compiler helper")?;
        let mut child = OwnedChild {
            child,
            supervisor: self,
            reaped: false,
        };
        {
            let mut state = lock(&self.state);
            if state.stopped {
                bail!("compiler operation cancelled");
            }
            state.child = Some(child.child.id());
        }
        let result = exchange_io(
            &mut child.child,
            source,
            &request,
            nonce,
            fingerprint,
            self.limits.wall,
            || lock(&self.state).stopped,
        );
        // All success and failure paths reap before releasing PID ownership.
        // IO receives EOF only after the helper closed stdout/stderr. A helper
        // which closes them and hangs is still governed by the same deadline.
        let (artifact, response) = result?;
        ensure!(
            response.ceiling.checked_sub(response.baseline) == Some(self.limits.memory),
            "compiler address-space allowance mismatch"
        );
        let status = child.wait_until(Instant::now() + Duration::from_millis(100))?;
        ensure!(
            !response.failed,
            "compiler refused component ({status}): {}",
            std::fs::read_to_string(&artifact.path).unwrap_or_default()
        );
        ensure!(status.success(), "compiler helper exited {status}");
        logging::log_notice(format!(
            "wasm compiler: input={} artifact={} baseline_vm={} ceiling_vm={} cpu_ms={}",
            source.len(),
            response.len,
            response.baseline,
            response.ceiling,
            response.cpu_ms
        ));
        // SAFETY: only the owned executable's successful, complete, nonce- and
        // exact build/configuration-matched SHA-256-verified frame reaches this
        // point. The private file has no open writable descriptor, is 0400,
        // and is not exposed to another writer. Wasmtime owns its mmap before
        // TempPath unlinks it. External serialized artifacts have no entrypoint.
        unsafe { Component::deserialize_file(engine, &artifact.path) }
            .context("deserialize owned compiler artifact")
    }
}

pub(crate) struct Reservation {
    supervisor: Arc<Supervisor>,
    id: u64,
    entered: bool,
}

impl Reservation {
    pub(crate) fn supervisor(&self) -> Arc<Supervisor> {
        Arc::clone(&self.supervisor)
    }

    pub(crate) fn compile(
        &mut self,
        engine: &Engine,
        source: &Source,
    ) -> anyhow::Result<Component> {
        if !self.entered {
            let mut state = lock(&self.supervisor.state);
            while !state.stopped
                && (state.active.is_some() || state.queued.front() != Some(&self.id))
            {
                state = self
                    .supervisor
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            ensure!(!state.stopped, "compiler operation cancelled");
            state.queued.pop_front();
            state.active = Some(self.id);
            self.entered = true;
        }
        ensure!(
            !lock(&self.supervisor.state).stopped,
            "compiler operation cancelled"
        );
        self.supervisor.run(engine, source)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = lock(&self.supervisor.state);
        if state.active == Some(self.id) {
            state.active = None;
        }
        state.queued.retain(|id| *id != self.id);
        self.supervisor.changed.notify_all();
    }
}

struct OwnedChild<'a> {
    child: Child,
    supervisor: &'a Supervisor,
    reaped: bool,
}

impl OwnedChild<'_> {
    fn wait_until(&mut self, deadline: Instant) -> anyhow::Result<std::process::ExitStatus> {
        loop {
            let mut state = lock(&self.supervisor.state);
            if let Some(status) = self.child.try_wait()? {
                self.reaped = true;
                state.child = None;
                return Ok(status);
            }
            drop(state);
            ensure!(
                Instant::now() < deadline,
                "compiler did not exit after completing its output"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for OwnedChild<'_> {
    fn drop(&mut self) {
        if !self.reaped {
            let mut state = lock(&self.supervisor.state);
            let _ = self.child.kill();
            let _ = self.child.wait();
            state.child = None;
        }
    }
}

fn helper_path() -> anyhow::Result<PathBuf> {
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    ensure!(
        unsafe {
            libc::dladdr(
                helper_path as *const () as *const libc::c_void,
                info.as_mut_ptr(),
            )
        } != 0,
        "cannot locate wasm module's compiler helper"
    );
    let info = unsafe { info.assume_init() };
    ensure!(
        !info.dli_fname.is_null(),
        "module image path is unavailable"
    );
    use std::os::unix::ffi::OsStrExt;
    let path = PathBuf::from(OsStr::from_bytes(
        unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes(),
    ));
    Ok(path
        .canonicalize()?
        .parent()
        .context("module directory")?
        .join("redis-wasm-compiler"))
}

#[shutdown_event_handler]
fn shutdown(_ctx: &EventContext) {
    let supervisor = lock(&ACTIVE).upgrade();
    if let Some(supervisor) = supervisor {
        supervisor.shutdown();
    }
}
