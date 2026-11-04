//! Parent-side ownership, independent of Redis/Wasmtime. Cancellation and exit
//! proof use private protocol messages, never a possibly reused numeric PID.
use crate::{
    compiler_pipe::{self, Artifact},
    compiler_protocol::{self as protocol, Response},
    compiler_source::Source,
    compiler_watch::ExitWatch,
};
use anyhow::{Context as _, ensure};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) struct Process {
    child: Child,
    pub(crate) control: UnixStream,
    nonce: [u8; 32],
    pid: Option<u32>,
    watch: Option<ExitWatch>,
    exit_proof: bool,
    exit_status: Option<u32>,
    reaped: bool,
    buffer: [u8; protocol::CONTROL_SIZE],
    buffered: usize,
    confirmed: bool,
    wall: Duration,
}

impl Process {
    pub(crate) fn spawn(
        path: &Path,
        memory: u64,
        cpu: u64,
        wall: Duration,
        nonce: [u8; 32],
    ) -> anyhow::Result<Self> {
        let (control, endpoint) = UnixStream::pair()?;
        control.set_nonblocking(true)?;
        let fd = endpoint.as_raw_fd();
        let nonce_hex = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        // Capture identity before spawn, not after a possible reparenting.
        let parent = unsafe { libc::getpid() };
        let mut command = Command::new(path);
        command
            .args([
                "--launch".to_owned(),
                memory.to_string(),
                cpu.to_string(),
                wall.as_secs().to_string(),
                fd.to_string(),
                nonce_hex,
                parent.to_string(),
                env!("WASM_COMPILER_BUILD").to_owned(),
            ])
            .env("MallocNanoZone", "0")
            .env("RAYON_NUM_THREADS", "2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().context("start bounded compiler launcher")?;
        drop(endpoint);
        Ok(Self {
            child,
            control,
            nonce,
            pid: None,
            watch: None,
            exit_proof: false,
            exit_status: None,
            reaped: false,
            buffer: [0; protocol::CONTROL_SIZE],
            buffered: 0,
            confirmed: false,
            wall,
        })
    }

    fn frame(&mut self) -> anyhow::Result<Option<(u32, u32, [u8; 32])>> {
        while self.buffered < self.buffer.len() {
            match self.control.read(&mut self.buffer[self.buffered..]) {
                Ok(0) => return Ok(None),
                Ok(n) => self.buffered += n,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        self.buffered = 0;
        let frame = protocol::parse_control(&self.buffer, self.nonce)?;
        if let Some(pid) = self.pid {
            ensure!(pid == frame.1, "compiler process identity changed");
        }
        if frame.0 == protocol::HELLO && self.pid.is_none() {
            self.pid = Some(frame.1);
            self.watch = Some(ExitWatch::arm(frame.1)?);
        }
        if frame.0 == protocol::EXIT {
            self.exit_proof = true;
            self.exit_status = Some(u32::from_le_bytes(frame.2[..4].try_into().unwrap()));
        }
        Ok(Some(frame))
    }

    fn next_frame(
        &mut self,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
    ) -> anyhow::Result<(u32, u32, [u8; 32])> {
        loop {
            ensure!(!cancelled(), "compiler operation cancelled");
            if let Some(frame) = self.frame()? {
                return Ok(frame);
            }
            self.try_reap()?;
            ensure!(!self.reaped, "compiler launcher exited during handshake");
            ensure!(Instant::now() < deadline, "compiler handshake wall timeout");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn handshake(&mut self, cancelled: &impl Fn() -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + self.wall;
        let hello = self.next_frame(deadline, cancelled)?;
        ensure!(hello.0 == protocol::HELLO, "compiler hello is missing");
        // An exit watch can miss an edge if armed against a dead/zombie PID.
        // Prove the ACTUAL C is alive after arming, before releasing its gate.
        let mut challenge = [0; 33];
        challenge[0] = b'A';
        File::open("/dev/urandom")?.read_exact(&mut challenge[1..])?;
        self.control.write_all(&challenge)?;
        let alive = self.next_frame(deadline, cancelled)?;
        ensure!(
            alive.0 == protocol::ALIVE && alive.2 == challenge[1..],
            "compiler live-worker challenge failed"
        );
        self.confirmed = true;
        self.control.write_all(b"G")?;
        Ok(())
    }

    pub(crate) fn exchange(
        &mut self,
        source: &Source,
        request: &[u8],
        fingerprint: [u8; 32],
        cancelled: impl Fn() -> bool,
    ) -> anyhow::Result<(Artifact, Response)> {
        let started = Instant::now();
        self.handshake(&cancelled)?;
        ensure!(
            self.confirmed,
            "compiler gate lacks actual post-arm liveness proof"
        );
        let remaining = self
            .wall
            .checked_sub(started.elapsed())
            .context("compiler handshake exceeded wall limit")?;
        let output = compiler_pipe::exchange_io(
            &mut self.child,
            source,
            request,
            self.nonce,
            fingerprint,
            remaining,
            cancelled,
        )?;
        // The trailer is written only after L reaped C, and the explicit
        // control proof works even when a Redis fork inherited output writers.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(frame) = self.frame()? {
                ensure!(
                    frame.0 == protocol::EXIT && frame.2[..4] == [0; 4],
                    "compiler exit was not successful"
                );
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "compiler successful-exit control proof missing"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(output)
    }

    fn try_reap(&mut self) -> anyhow::Result<()> {
        if self.reaped {
            return Ok(());
        }
        match self.child.try_wait() {
            Ok(Some(_)) => self.reaped = true,
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => self.reaped = true,
            Err(error) => return Err(error.into()),
            Ok(None) => {}
        }
        Ok(())
    }

    pub(crate) fn exit_status(&self) -> Option<u32> {
        self.exit_status
    }

    pub(crate) fn poll_cleanup(&mut self) -> anyhow::Result<bool> {
        // Read a late HELLO/EXIT too: a temporarily stopped launcher may resume
        // after the original operation entered bounded-wait quarantine.
        while self.frame()?.is_some() {}
        self.try_reap()?;
        let compiler_dead = self.exit_proof
            || self
                .watch
                .as_mut()
                .map(|watch| watch.exited())
                .transpose()?
                .unwrap_or(false);
        Ok(self.reaped && compiler_dead)
    }

    pub(crate) fn cleanup(&mut self, deadline: Instant) -> anyhow::Result<bool> {
        let _ = self.control.write(b"X");
        self.child.stdin.take();
        loop {
            if self.poll_cleanup()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.control.write(b"X");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler_engine;

    fn helper() -> std::path::PathBuf {
        let path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("redis-wasm-compiler");
        assert!(
            path.is_file(),
            "build redis-module-wasm (including its helper) before tests: {}",
            path.display()
        );
        path
    }

    struct Owned(Process);
    impl Owned {
        fn spawn(memory: u64) -> Self {
            Self(Process::spawn(&helper(), memory, 5, Duration::from_secs(10), [17; 32]).unwrap())
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.cleanup(Instant::now() + Duration::from_secs(5));
        }
    }

    fn compile(process: &mut Process, bytes: &[u8]) -> anyhow::Result<(Artifact, Response)> {
        let engine = compiler_engine::engine()?;
        let fingerprint = compiler_engine::fingerprint(&engine);
        let source = Source::spool(bytes)?;
        let request = protocol::request([17; 32], fingerprint, source.len())?;
        process.exchange(&source, &request, fingerprint, || false)
    }

    #[test]
    fn actual_helper_compiles_only_owned_matching_artifacts_and_reaps() {
        let mut owned = Owned::spawn(2 << 30);
        let (artifact, response) = compile(&mut owned.0, b"\0asm\x0d\0\x01\0").unwrap();
        assert!(
            owned
                .0
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        assert!(owned.0.confirmed && owned.0.exit_proof && owned.0.reaped);
        assert_eq!(response.ceiling - response.baseline, 2 << 30);
        let engine = compiler_engine::engine().unwrap();
        // Same owned helper proof and immutable private artifact as production.
        let component =
            unsafe { wasmtime::component::Component::deserialize_file(&engine, &artifact.path) }
                .unwrap();
        drop(artifact);
        // The component retains its own mapping after the temporary file unlinks.
        assert!(component.component_type().imports(&engine).next().is_none());
    }

    #[test]
    fn actual_helper_refusal_and_pre_spawn_failure_release_all_processes() {
        let mut failed = Owned::spawn(2 << 30);
        let error = compile(&mut failed.0, b"invalid wasm").err().unwrap();
        assert!(
            format!("{error:#}").contains("invalid component"),
            "{error:#}"
        );
        assert!(
            failed
                .0
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        let mut before_spawn = Owned::spawn(1);
        assert!(before_spawn.0.handshake(&|| false).is_err());
        assert!(
            before_spawn
                .0
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        assert!(before_spawn.0.pid.is_none() && before_spawn.0.exit_proof);
        let mut cancelled = Owned::spawn(2 << 30);
        assert!(
            cancelled
                .0
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
    }

    #[test]
    fn successful_owned_compiler_proof_survives_external_launcher_reaping() {
        let mut owned = Owned::spawn(2 << 30);
        let launcher = owned.0.child.id();
        // Models Redis's independent SIGCHLD path consuming our direct child.
        // It cannot consume the compiler grandchild's status owned by L.
        let reaper = std::thread::spawn(move || {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(launcher as libc::pid_t, &mut status, 0) };
            (waited, status)
        });
        let result = compile(&mut owned.0, b"\0asm\x0d\0\x01\0");
        let (waited, status) = reaper.join().unwrap();
        result.unwrap();
        assert_eq!(waited, launcher as libc::pid_t);
        assert_eq!(status, 0);
        assert!(
            owned
                .0
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        assert!(owned.0.confirmed && owned.0.exit_proof && owned.0.reaped);
    }

    #[test]
    fn killed_launcher_never_releases_capacity_without_actual_compiler_exit() {
        let mut owned = Owned::spawn(2 << 30);
        owned.0.handshake(&|| false).unwrap();
        let compiler = owned.0.pid.unwrap() as libc::pid_t;
        let launcher = owned.0.child.id() as libc::pid_t;
        // TEST-ONLY fault injection: production never signals PIDs from Redis.
        assert_eq!(unsafe { libc::kill(compiler, libc::SIGSTOP) }, 0);
        assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
        let first = owned.0.cleanup(Instant::now() + Duration::from_millis(100));
        // Resume before asserting, so even a failed assertion cannot leak a
        // stopped worker. Linux PDEATHSIG may already have killed this C.
        let exited = owned.0.watch.as_mut().unwrap().exited().unwrap();
        if !exited {
            unsafe { libc::kill(compiler, libc::SIGCONT) };
        }
        let final_cleanup = owned.0.cleanup(Instant::now() + Duration::from_secs(3));
        #[cfg(target_os = "macos")]
        assert!(
            !first.unwrap(),
            "stopped C is alive after L dies; no exit proof yet"
        );
        #[cfg(target_os = "linux")]
        let _ = first;
        assert!(
            final_cleanup.unwrap(),
            "watchdog/PDEATHSIG must eventually prove C exit"
        );
        assert!(
            !owned.0.exit_proof,
            "L was killed, so only the stable exit watch proves cleanup"
        );
    }
}
