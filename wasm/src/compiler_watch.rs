//! Stable process-exit observation. This never signals a numeric PID. Arm it
//! before the private live-worker challenge, then release compilation only
//! after that challenge proves the watch was attached while C was alive.
use anyhow::{Context as _, ensure};
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
    time::Duration,
};

pub(crate) struct ExitWatch {
    file: Option<File>,
    exited: bool,
}

impl ExitWatch {
    pub(crate) fn arm(pid: u32) -> anyhow::Result<Self> {
        ensure!(pid > 1, "invalid compiler process identity");
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(Self {
                        file: None,
                        exited: true,
                    });
                }
                return Err(error).context("arm compiler pidfd");
            }
            Ok(Self {
                file: Some(unsafe { File::from_raw_fd(fd as i32) }),
                exited: false,
            })
        }
        #[cfg(target_os = "macos")]
        {
            let fd = unsafe { libc::kqueue() };
            ensure!(
                fd >= 0,
                "compiler exit kqueue: {}",
                std::io::Error::last_os_error()
            );
            let file = unsafe { File::from_raw_fd(fd) };
            ensure!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
                "compiler exit watch close-on-exec"
            );
            let change = libc::kevent {
                ident: pid as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            if unsafe { libc::kevent(fd, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) }
                < 0
            {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(Self {
                        file: None,
                        exited: true,
                    });
                }
                return Err(error).context("arm compiler process exit watch");
            }
            Ok(Self {
                file: Some(file),
                exited: false,
            })
        }
    }

    pub(crate) fn exited(&mut self) -> anyhow::Result<bool> {
        if self.exited {
            return Ok(true);
        }
        let fd = self
            .file
            .as_ref()
            .context("compiler exit watch missing")?
            .as_raw_fd();
        #[cfg(target_os = "linux")]
        {
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 0) };
            if ready < 0 {
                return Err(std::io::Error::last_os_error()).context("poll compiler exit");
            }
            self.exited = ready > 0 && poll.revents & (libc::POLLIN | libc::POLLHUP) != 0;
        }
        #[cfg(target_os = "macos")]
        {
            let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let ready =
                unsafe { libc::kevent(fd, std::ptr::null(), 0, event.as_mut_ptr(), 1, &timeout) };
            if ready < 0 {
                return Err(std::io::Error::last_os_error()).context("poll compiler exit");
            }
            if ready > 0 {
                let event = unsafe { event.assume_init() };
                ensure!(
                    event.flags & libc::EV_ERROR == 0 && event.fflags & libc::NOTE_EXIT != 0,
                    "unexpected compiler process event"
                );
                self.exited = true;
            }
        }
        Ok(self.exited)
    }

    pub(crate) fn wait(&mut self) -> anyhow::Result<()> {
        // Retain the active admission if an externally stopped compiler cannot
        // run its liveness watchdog. Never turn a cleanup timeout into capacity
        // for an overlapping compiler; the server keeps serving other clients.
        while !self.exited()? {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        process::{Command, Stdio},
        time::Instant,
    };
    #[test]
    fn stable_exit_event_survives_reaping_on_another_path() {
        let mut child = Command::new("python3")
            .args(["-c", "import sys;sys.stdin.buffer.read(1)"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut watch = ExitWatch::arm(child.id()).unwrap();
        child.stdin.take().unwrap().write_all(b"G").unwrap();
        assert!(child.wait().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !watch.exited().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(watch.exited().unwrap());
    }
}
