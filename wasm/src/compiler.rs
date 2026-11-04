//! Owned compiler processes, bounded admission and deadline-driven pipe IO.
//! No worker acquires Redis's lock: shutdown can kill/reap/join under that lock.
use crate::{
    compiler_engine, compiler_process::Process, compiler_protocol as protocol,
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
    os::unix::net::UnixStream,
    path::PathBuf,
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
pub(crate) static PATH: Mutex<String> = Mutex::new(String::new());
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
    control: Option<UnixStream>,
    stopped: bool,
    uncertain: Option<Process>,
}

pub(crate) struct Supervisor {
    helper: PathBuf,
    limits: Limits,
    state: Mutex<State>,
    changed: Condvar,
    workers: Mutex<Vec<JoinHandle<()>>>,
    completed: AtomicU64,
    failed: AtomicU64,
    baseline: AtomicU64,
    ceiling: AtomicU64,
    cpu_ms: AtomicU64,
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
        let configured = lock(&PATH).clone();
        let helper = if configured.is_empty() {
            helper_path()?
        } else {
            let path = PathBuf::from(configured);
            ensure!(path.is_absolute(), "compiler-path must be an absolute path");
            path
        };
        let supervisor = Self::configured(helper, limits);
        *lock(&ACTIVE) = Arc::downgrade(&supervisor);
        Ok(supervisor)
    }

    fn configured(helper: PathBuf, limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            helper,
            limits,
            state: Mutex::new(State {
                next: 0,
                queued: VecDeque::new(),
                active: None,
                control: None,
                stopped: false,
                uncertain: None,
            }),
            changed: Condvar::new(),
            workers: Mutex::new(Vec::new()),
            completed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            baseline: AtomicU64::new(0),
            ceiling: AtomicU64::new(0),
            cpu_ms: AtomicU64::new(0),
        })
    }

    /// Reserve before source spooling/copying and before creating a worker.
    /// Capacity includes one active operation and at most `queue` waiting ones.
    /// A restore/RDB batch owns one reservation and compiles sequentially.
    pub(crate) fn reserve(self: &Arc<Self>) -> anyhow::Result<Reservation> {
        let mut state = lock(&self.state);
        Self::recover(&mut state)?;
        ensure!(
            !state.stopped && state.uncertain.is_none(),
            "compiler cleanup is uncertain; new admission is refused"
        );
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

    fn recover(state: &mut State) -> anyhow::Result<()> {
        if let Some(process) = &mut state.uncertain {
            if process.poll_cleanup()? {
                state.uncertain = None;
                state.active = None;
                state.control = None;
            }
        }
        Ok(())
    }

    pub(crate) fn shutdown(&self) -> anyhow::Result<()> {
        {
            let mut state = lock(&self.state);
            state.stopped = true;
            if let Some(control) = &mut state.control {
                let _ = std::io::Write::write(control, b"X");
            }
            self.changed.notify_all();
        }
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut workers = lock(&self.workers);
        while workers.iter().any(|worker| !worker.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut running = Vec::new();
        for worker in workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                running.push(worker);
            }
        }
        *workers = running;
        let mut state = lock(&self.state);
        let recovered = Self::recover(&mut state);
        if recovered.is_err() || state.uncertain.is_some() || !workers.is_empty() {
            state.stopped = false;
            bail!("compiler cleanup is not proven complete; module unload is refused");
        }
        Ok(())
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
        let mut process = Process::spawn(
            &path,
            self.limits.memory,
            self.limits.cpu,
            self.limits.wall,
            nonce,
        )?;
        {
            let mut state = lock(&self.state);
            state.control = Some(process.control.try_clone()?);
        }
        let output = process.exchange(source, &request, fingerprint, || lock(&self.state).stopped);
        let cleaned = process
            .cleanup(Instant::now() + Duration::from_secs(5))
            .unwrap_or(false);
        if !cleaned {
            let mut state = lock(&self.state);
            state.uncertain = Some(process);
            bail!("compiler cleanup is uncertain; capacity remains reserved until exit proof");
        }
        lock(&self.state).control = None;
        let (artifact, response) = output?;
        ensure!(
            response.ceiling.checked_sub(response.baseline) == Some(self.limits.memory),
            "compiler address-space allowance mismatch"
        );
        self.baseline.store(response.baseline, Ordering::Relaxed);
        self.ceiling.store(response.ceiling, Ordering::Relaxed);
        self.cpu_ms.store(response.cpu_ms, Ordering::Relaxed);
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

    fn enter(&mut self) -> anyhow::Result<()> {
        if !self.entered {
            let mut state = lock(&self.supervisor.state);
            while !state.stopped
                && state.uncertain.is_none()
                && (state.active.is_some() || state.queued.front() != Some(&self.id))
            {
                state = self
                    .supervisor
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            ensure!(
                !state.stopped && state.uncertain.is_none(),
                "compiler operation cancelled or cleanup uncertain"
            );
            state.queued.pop_front();
            state.active = Some(self.id);
            self.entered = true;
        }
        ensure!(
            {
                let state = lock(&self.supervisor.state);
                !state.stopped && state.uncertain.is_none()
            },
            "compiler operation cancelled"
        );
        Ok(())
    }

    pub(crate) fn compile(
        &mut self,
        engine: &Engine,
        source: &Source,
    ) -> anyhow::Result<Component> {
        self.enter()?;
        self.supervisor.run(engine, source)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = lock(&self.supervisor.state);
        if state.active == Some(self.id) && state.uncertain.is_none() {
            state.active = None;
        }
        state.queued.retain(|id| *id != self.id);
        self.supervisor.changed.notify_all();
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
        if let Err(error) = supervisor.shutdown() {
            logging::log_warning(format!("wasm compiler shutdown: {error:#}"));
        }
    }
}

/// INFO can be requested in a crash report while a worker owns either mutex.
/// Never wait on a worker, a lifecycle join or the host from this snapshot.
pub(crate) fn diagnostics() -> [i64; 8] {
    let Some(supervisor) = ACTIVE.try_lock().ok().and_then(|active| active.upgrade()) else {
        return [0; 8];
    };
    let (active, queued, uncertain) = supervisor
        .state
        .try_lock()
        .map(|state| {
            (
                u64::from(state.active.is_some()),
                state.queued.len() as u64,
                u64::from(state.uncertain.is_some()),
            )
        })
        .unwrap_or_default();
    [
        active,
        queued,
        supervisor.completed.load(Ordering::Relaxed),
        supervisor.failed.load(Ordering::Relaxed),
        supervisor.baseline.load(Ordering::Relaxed),
        supervisor.ceiling.load(Ordering::Relaxed),
        supervisor.cpu_ms.load(Ordering::Relaxed),
        uncertain,
    ]
    .map(|n| i64::try_from(n).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn supervisor(queue: usize) -> Arc<Supervisor> {
        Supervisor::configured(
            PathBuf::from("/missing/compiler"),
            Limits {
                memory: 64 << 20,
                cpu: 1,
                wall: Duration::from_millis(50),
                queue,
            },
        )
    }

    #[test]
    fn admission_bounds_workers_preserves_fifo_and_releases_cancelled_reservations() {
        let supervisor = supervisor(1);
        let mut first = supervisor.reserve().unwrap();
        first.enter().unwrap();
        let second = supervisor.reserve().unwrap();
        assert!(supervisor.reserve().is_err());
        drop(second);
        let mut third = supervisor.reserve().unwrap();
        assert_eq!(lock(&supervisor.state).queued.front(), Some(&third.id));
        let (sent, received) = mpsc::channel();
        supervisor
            .spawn(move || {
                third.enter().unwrap();
                sent.send(()).unwrap();
            })
            .unwrap();
        assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
        drop(first);
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        supervisor.shutdown().unwrap();
        assert!(lock(&supervisor.state).queued.is_empty());
        assert!(lock(&supervisor.state).active.is_none());
        assert!(supervisor.reserve().is_err());
    }

    #[test]
    fn shutdown_wakes_waiting_operations_without_a_redis_lock() {
        let supervisor = supervisor(1);
        let mut active = supervisor.reserve().unwrap();
        active.enter().unwrap();
        let mut waiting = supervisor.reserve().unwrap();
        let (sent, received) = mpsc::channel();
        supervisor
            .spawn(move || {
                sent.send(waiting.enter().is_err()).unwrap();
            })
            .unwrap();
        supervisor.shutdown().unwrap();
        assert!(received.recv_timeout(Duration::from_secs(2)).unwrap());
        drop(active);
        assert!(lock(&supervisor.state).queued.is_empty());
        assert!(lock(&supervisor.workers).is_empty());
    }
}
