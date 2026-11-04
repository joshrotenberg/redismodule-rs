//! Bounded System parent of the compiler. Redis cannot reap this grandchild.
use crate::{compiler_limits, compiler_protocol as protocol};
use anyhow::{Context as _, bail, ensure};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub unsafe fn inherit_fd(command: &mut Command, fd: RawFd) {
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}
fn nonblocking(fd: RawFd) -> anyhow::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "compiler nonblocking fd: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
fn fd_valid(fd: RawFd) -> anyhow::Result<()> {
    ensure!(
        fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
        "invalid private compiler descriptor"
    );
    Ok(())
}
fn status_code(status: std::process::ExitStatus) -> u32 {
    status.code().unwrap_or(128 + status.signal().unwrap_or(0)) as u32
}

struct Worker {
    child: Child,
    watch: UnixStream,
    status: Option<std::process::ExitStatus>,
}
impl Worker {
    fn status(&mut self) -> anyhow::Result<Option<std::process::ExitStatus>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status)
    }
    fn cancel(&mut self) -> anyhow::Result<std::process::ExitStatus> {
        if let Some(status) = self.status()? {
            return Ok(status);
        }
        // This standalone launcher is the child's ONLY waitpid owner. Neither
        // Redis nor another launcher can reap or reuse this waitable PID.
        let _ = self.child.kill();
        let status = self.child.wait()?;
        self.status = Some(status);
        Ok(status)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

fn read_step(
    stream: &mut UnixStream,
    bytes: &mut [u8],
    worker: &mut Worker,
    parent: libc::pid_t,
    deadline: Instant,
    cancel: Option<RawFd>,
) -> anyhow::Result<()> {
    let mut at = 0;
    while at < bytes.len() {
        ensure!(Instant::now() < deadline, "compiler gate wall timeout");
        ensure!(unsafe { libc::getppid() } == parent, "compiler host exited");
        ensure!(
            worker.status()?.is_none(),
            "compiler exited before its gate opened"
        );
        let mut fds = [
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: cancel.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, 20) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(std::io::Error::last_os_error()).context("compiler gate poll");
        }
        if fds[1].revents != 0 {
            bail!("compiler operation cancelled during gate");
        }
        if fds[0].revents != 0 {
            match stream.read(&mut bytes[at..]) {
                Ok(0) => bail!("compiler gate closed"),
                Ok(n) => at += n,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

pub fn run(
    memory: u64,
    cpu: u64,
    wall: u64,
    control_fd: RawFd,
    nonce: [u8; 32],
    parent: libc::pid_t,
    build: &str,
) -> anyhow::Result<()> {
    fd_valid(control_fd)?;
    let mut control = unsafe { UnixStream::from_raw_fd(control_fd) };
    // Every ordinary failure before spawning C proves that no worker exists.
    // Abrupt launcher death before this proof remains conservatively uncertain.
    let prepare = || -> anyhow::Result<Worker> {
        nonblocking(control_fd)?;
        ensure!(
            build == env!("WASM_COMPILER_BUILD"),
            "compiler launcher build identity mismatch"
        );
        ensure!(
            unsafe { libc::getppid() } == parent,
            "compiler host exited before launcher setup"
        );
        compiler_limits::apply_launcher(memory, cpu)?;
        ensure!(
            unsafe { libc::fcntl(control_fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
            "launcher control close-on-exec"
        );
        let (watch, endpoint) = UnixStream::pair()?;
        nonblocking(watch.as_raw_fd())?;
        let fd = endpoint.as_raw_fd();
        let launcher = unsafe { libc::getpid() };
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--compile",
                &memory.to_string(),
                &cpu.to_string(),
                &fd.to_string(),
                &launcher.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        unsafe { inherit_fd(&mut command, fd) };
        let child = command.spawn().context("spawn compiler worker")?;
        drop(endpoint);
        // Nothing fallible between successful spawn and installing its owner.
        Ok(Worker {
            child,
            watch,
            status: None,
        })
    };
    let mut worker = match prepare() {
        Ok(worker) => worker,
        Err(error) => {
            let mut detail = [0; 32];
            detail[..4].copy_from_slice(&1_u32.to_le_bytes());
            let _ = control.write_all(&protocol::control_frame(protocol::EXIT, nonce, 0, detail));
            return Err(error);
        }
    };
    let pid = worker.child.id();
    let deadline = Instant::now() + Duration::from_secs(wall);
    let result = (|| -> anyhow::Result<()> {
        control.write_all(&protocol::control_frame(
            protocol::HELLO,
            nonce,
            pid,
            [0; 32],
        ))?;
        let mut prefix = [0];
        read_step(
            &mut control,
            &mut prefix,
            &mut worker,
            parent,
            deadline,
            None,
        )?;
        ensure!(
            prefix == *b"A",
            "compiler operation cancelled before challenge"
        );
        let mut challenge = [0; 32];
        read_step(
            &mut control,
            &mut challenge,
            &mut worker,
            parent,
            deadline,
            None,
        )?;
        let mut command = [0; 33];
        command[0] = b'A';
        command[1..].copy_from_slice(&challenge);
        worker.watch.write_all(&command)?;
        let mut acknowledgement = [0; 33];
        let mut response = worker.watch.try_clone()?;
        read_step(
            &mut response,
            &mut acknowledgement,
            &mut worker,
            parent,
            deadline,
            Some(control.as_raw_fd()),
        )?;
        ensure!(
            acknowledgement[0] == b'R' && acknowledgement[1..] == challenge,
            "compiler live challenge mismatch"
        );
        // Forward C's actual response. Never synthesize a liveness ACK here.
        control.write_all(&protocol::control_frame(
            protocol::ALIVE,
            nonce,
            pid,
            acknowledgement[1..].try_into().unwrap(),
        ))?;
        read_step(
            &mut control,
            &mut prefix,
            &mut worker,
            parent,
            deadline,
            None,
        )?;
        ensure!(prefix == *b"G", "compiler operation cancelled before GO");
        worker.watch.write_all(b"G")?;
        forward_and_wait(&mut worker, &mut control, parent, deadline, nonce)
    })();
    let status = worker.cancel()?;
    let mut detail = [0; 32];
    detail[..4].copy_from_slice(&status_code(status).to_le_bytes());
    // Explicit cleanup proof, even for partial output/cancellation. EOF is
    // unreliable because Redis fork children can inherit pipe/socket handles.
    control.write_all(&protocol::control_frame(protocol::EXIT, nonce, pid, detail))?;
    result?;
    std::io::stdout().write_all(&protocol::exit_trailer(nonce, status_code(status)))?;
    std::io::stdout().flush()?;
    ensure!(status.success(), "compiler worker exited {status}");
    Ok(())
}

fn forward_and_wait(
    worker: &mut Worker,
    control: &mut UnixStream,
    parent: libc::pid_t,
    deadline: Instant,
    nonce: [u8; 32],
) -> anyhow::Result<()> {
    let mut source = std::io::stdin().lock();
    let mut input = worker.child.stdin.take();
    nonblocking(libc::STDIN_FILENO)?;
    nonblocking(input.as_ref().context("worker stdin")?.as_raw_fd())?;
    let mut bytes = [0; 65536];
    let mut available = 0;
    let mut at = 0;
    let mut total = 0;
    let mut header = [0; protocol::REQUEST_SIZE];
    let mut header_len = 0;
    let mut expected = protocol::REQUEST_SIZE;
    loop {
        ensure!(Instant::now() < deadline, "compiler launcher wall timeout");
        ensure!(unsafe { libc::getppid() } == parent, "compiler host exited");
        let mut fds = [
            libc::pollfd {
                fd: control.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if total < expected && at == available {
                    libc::STDIN_FILENO
                } else {
                    -1
                },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if at < available {
                    input.as_ref().map_or(-1, AsRawFd::as_raw_fd)
                } else {
                    -1
                },
                events: libc::POLLOUT,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 3, 20) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(std::io::Error::last_os_error()).context("launcher IO poll");
        }
        if fds[0].revents != 0 {
            bail!("compiler operation cancelled");
        }
        if fds[1].revents != 0 {
            let limit = bytes.len().min(expected - total);
            match source.read(&mut bytes[..limit]) {
                Ok(0) => bail!("compiler input truncated"),
                Ok(n) => {
                    available = n;
                    at = 0;
                    total += n;
                    let copy = n.min(header.len() - header_len);
                    header[header_len..header_len + copy].copy_from_slice(&bytes[..copy]);
                    header_len += copy;
                    if header_len == header.len() && expected == protocol::REQUEST_SIZE {
                        let (actual, len) =
                            protocol::parse_request(&header, header[44..76].try_into().unwrap())?;
                        ensure!(actual == nonce, "compiler request nonce mismatch");
                        expected += len;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        if fds[2].revents != 0 {
            if let Some(input) = &mut input {
                match input.write(&bytes[at..available]) {
                    Ok(n) => {
                        ensure!(n > 0, "compiler worker closed input");
                        at += n;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        // Declared framing, NOT EOF from Redis's possibly inherited writer.
        if total == expected && at == available {
            input.take();
        }
        if worker.status()?.is_some() {
            return Ok(());
        }
    }
}

pub fn watch_launcher(fd: RawFd, parent: libc::pid_t) -> anyhow::Result<()> {
    fd_valid(fd)?;
    #[cfg(target_os = "linux")]
    ensure!(
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } == 0,
        "compiler parent-death signal: {}",
        std::io::Error::last_os_error()
    );
    let mut control = unsafe { UnixStream::from_raw_fd(fd) };
    nonblocking(fd)?;
    ensure!(
        unsafe { libc::getppid() } == parent,
        "compiler launcher exited before gate"
    );
    let deadline = Instant::now() + Duration::from_secs(1200);
    let read_gate = |control: &mut UnixStream, bytes: &mut [u8]| -> anyhow::Result<()> {
        let mut at = 0;
        while at < bytes.len() {
            ensure!(
                Instant::now() < deadline && unsafe { libc::getppid() } == parent,
                "compiler launcher exited during gate"
            );
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 20) } < 0 {
                continue;
            }
            if poll.revents != 0 {
                match control.read(&mut bytes[at..]) {
                    Ok(0) => bail!("compiler launcher closed gate"),
                    Ok(n) => at += n,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    };
    let mut challenge = [0; 33];
    read_gate(&mut control, &mut challenge)?;
    ensure!(challenge[0] == b'A', "invalid compiler challenge");
    challenge[0] = b'R';
    control.write_all(&challenge)?;
    let mut go = [0];
    read_gate(&mut control, &mut go)?;
    ensure!(go == *b"G", "invalid compiler GO");
    std::thread::Builder::new()
        .name("compiler-launcher-liveness".into())
        .spawn(move || {
            let _control = control;
            loop {
                if unsafe { libc::getppid() } != parent {
                    unsafe { libc::_exit(137) }
                }
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut poll, 1, 20) } > 0 && poll.revents != 0 {
                    unsafe { libc::_exit(137) }
                }
            }
        })?;
    Ok(())
}
