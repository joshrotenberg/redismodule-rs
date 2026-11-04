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
    control_closed: bool,
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
        Self::spawn_with_output(path, memory, cpu, wall, nonce, Stdio::piped())
    }

    fn spawn_with_output(
        path: &Path,
        memory: u64,
        cpu: u64,
        wall: Duration,
        nonce: [u8; 32],
        stdout: Stdio,
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
            .stdout(stdout)
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
            control_closed: false,
            wall,
        })
    }

    fn frame(&mut self) -> anyhow::Result<Option<(u32, u32, [u8; 32])>> {
        if self.control_closed {
            return Ok(None);
        }
        while self.buffered < self.buffer.len() {
            match self.control.read(&mut self.buffer[self.buffered..]) {
                Ok(0) => {
                    self.control_closed = true;
                    return Ok(None);
                }
                Ok(n) => self.buffered += n,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::NotConnected
                    ) =>
                {
                    self.control_closed = true;
                    return Ok(None);
                }
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
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
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
        // A transport error does not prove exit, but must not suppress valid
        // EXIT/watch evidence or reaping. Linux can reset this socket after L
        // sent EXIT if a raced CANCEL was left unread when L closed it.
        while matches!(self.frame(), Ok(Some(_))) {}
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

    struct Owned(Option<Process>);
    impl Owned {
        fn spawn(memory: u64) -> Self {
            Self(Some(
                Process::spawn(&helper(), memory, 5, Duration::from_secs(10), [17; 32]).unwrap(),
            ))
        }
        fn into_inner(mut self) -> Process {
            self.0.take().unwrap()
        }
    }
    impl std::ops::Deref for Owned {
        type Target = Process;
        fn deref(&self) -> &Process {
            self.0.as_ref().unwrap()
        }
    }
    impl std::ops::DerefMut for Owned {
        fn deref_mut(&mut self) -> &mut Process {
            self.0.as_mut().unwrap()
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            if let Some(process) = &mut self.0 {
                let _ = process.cleanup(Instant::now() + Duration::from_secs(5));
            }
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
        let (artifact, response) = compile(&mut owned, b"\0asm\x0d\0\x01\0").unwrap();
        assert!(
            owned
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        assert!(owned.confirmed && owned.exit_proof && owned.reaped);
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
        let error = compile(&mut failed, b"invalid wasm").err().unwrap();
        assert!(
            format!("{error:#}").contains("invalid component"),
            "{error:#}"
        );
        assert!(
            failed
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        let mut before_spawn = Owned::spawn(1);
        assert!(before_spawn.handshake(&|| false).is_err());
        assert!(
            before_spawn
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        assert!(before_spawn.pid.is_none() && before_spawn.exit_proof);
        let mut cancelled = Owned::spawn(2 << 30);
        assert!(
            cancelled
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
    }

    #[test]
    fn successful_owned_compiler_proof_survives_external_launcher_reaping() {
        let mut owned = Owned::spawn(2 << 30);
        let launcher = owned.child.id();
        // Models Redis's independent SIGCHLD path consuming our direct child.
        // It cannot consume the compiler grandchild's status owned by L.
        let reaper = std::thread::spawn(move || {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(launcher as libc::pid_t, &mut status, 0) };
            (waited, status)
        });
        let result = compile(&mut owned, b"\0asm\x0d\0\x01\0");
        let (waited, status) = reaper.join().unwrap();
        result.unwrap();
        assert_eq!(waited, launcher as libc::pid_t);
        assert_eq!(status, 0);
        assert!(
            owned
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        assert!(owned.confirmed && owned.exit_proof && owned.reaped);
    }

    #[test]
    fn killed_launcher_never_releases_capacity_without_actual_compiler_exit() {
        let mut owned = Owned::spawn(2 << 30);
        owned.handshake(&|| false).unwrap();
        let compiler = owned.pid.unwrap() as libc::pid_t;
        let launcher = owned.child.id() as libc::pid_t;
        // TEST-ONLY fault injection: production never signals PIDs from Redis.
        assert_eq!(unsafe { libc::kill(compiler, libc::SIGSTOP) }, 0);
        assert_eq!(unsafe { libc::kill(launcher, libc::SIGKILL) }, 0);
        let first = owned.cleanup(Instant::now() + Duration::from_millis(100));
        // Resume before asserting, so even a failed assertion cannot leak a
        // stopped worker. Linux PDEATHSIG may already have killed this C.
        let exited = owned.watch.as_mut().unwrap().exited().unwrap();
        if !exited {
            unsafe { libc::kill(compiler, libc::SIGCONT) };
        }
        let final_cleanup = owned.cleanup(Instant::now() + Duration::from_secs(3));
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
            !owned.exit_proof,
            "L was killed, so only the stable exit watch proves cleanup"
        );
    }
    #[test]
    fn full_output_pipe_with_retained_readers_cannot_trap_launcher_cleanup() {
        use std::os::fd::FromRawFd;
        let mut initial = Owned::spawn(2 << 30);
        let (_, response) = compile(&mut initial, b"\0asm\x0d\0\x01\0").unwrap();
        assert!(
            initial
                .cleanup(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        let frame_size = protocol::RESPONSE_SIZE + response.len;
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        for fd in fds {
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        let mut reader = unsafe { File::from_raw_fd(fds[0]) };
        let mut writer = unsafe { File::from_raw_fd(fds[1]) };
        let flags = unsafe { libc::fcntl(fds[1], libc::F_GETFL) };
        assert_eq!(
            unsafe { libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut capacity = 0;
        loop {
            match writer.write(&[0; 4096]) {
                Ok(n) => capacity += n,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill output pipe: {error}"),
            }
        }
        assert!(
            frame_size < capacity,
            "empty component frame must fit in one pipe"
        );
        reader.read_exact(&mut vec![0; frame_size]).unwrap();
        // Restore a blocking writer, just as normal Stdio::piped creates.
        assert_eq!(unsafe { libc::fcntl(fds[1], libc::F_SETFL, flags) }, 0);
        let inherited_reader = reader.try_clone().unwrap();
        let mut owned = Owned(Some(
            Process::spawn_with_output(
                &helper(),
                2 << 30,
                5,
                Duration::from_secs(10),
                [17; 32],
                Stdio::from(writer),
            )
            .unwrap(),
        ));
        owned.handshake(&|| false).unwrap();
        let engine = compiler_engine::engine().unwrap();
        let request =
            protocol::request([17; 32], compiler_engine::fingerprint(&engine), 8).unwrap();
        let mut input = owned.child.stdin.take().unwrap();
        input.write_all(&request).unwrap();
        input.write_all(b"\0asm\x0d\0\x01\0").unwrap();
        drop(input);
        let exited = owned
            .next_frame(Instant::now() + Duration::from_secs(5), &|| false)
            .unwrap();
        assert_eq!(exited.0, protocol::EXIT);
        assert_eq!(exited.2[..4], [0; 4]);
        // C has successfully written exactly the remaining pipe capacity. L
        // must observe X while trying to append its trailer, although these
        // otherwise-unused readers keep the full pipe alive.
        let bounded = owned.cleanup(Instant::now() + Duration::from_millis(300));
        drop(inherited_reader);
        drop(reader);
        let reaped = owned.cleanup(Instant::now() + Duration::from_secs(2));
        assert!(
            bounded.unwrap(),
            "full trailer pipe prevented bounded cancellation"
        );
        assert!(reaped.unwrap() && owned.exit_proof && owned.reaped);
        // Feed that actual compiler/launcher exit proof into admission recovery.
        let process = owned.into_inner();
        crate::compiler::tests::assert_recovery_after_exit(process);
    }
    #[test]
    fn terminal_control_reset_neither_discards_valid_exit_nor_invents_proof() {
        for proof in [false, true] {
            let (control, mut endpoint) = UnixStream::pair().unwrap();
            control.set_nonblocking(true).unwrap();
            let mut child = Command::new("true").spawn().unwrap();
            child.wait().unwrap();
            let mut process = Process {
                child,
                control,
                nonce: [17; 32],
                pid: None,
                watch: None,
                exit_proof: false,
                exit_status: None,
                reaped: false,
                buffer: [0; protocol::CONTROL_SIZE],
                buffered: 0,
                confirmed: false,
                control_closed: false,
                wall: Duration::from_secs(1),
            };
            if proof {
                endpoint
                    .write_all(&protocol::control_frame(
                        protocol::EXIT,
                        [17; 32],
                        0,
                        [0; 32],
                    ))
                    .unwrap();
            }
            // Leave a CANCEL unread when the peer closes: AF_UNIX can report
            // ECONNRESET after its already queued EXIT, instead of plain EOF.
            process.control.write_all(b"X").unwrap();
            drop(endpoint);
            assert_eq!(
                process
                    .cleanup(Instant::now() + Duration::from_millis(30))
                    .unwrap(),
                proof
            );
            assert_eq!(process.exit_proof, proof);
            assert!(process.reaped && process.control_closed);
        }
    }
}
