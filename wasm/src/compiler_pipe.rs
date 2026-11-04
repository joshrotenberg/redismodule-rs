//! Bounded pipe transport; all three pipes are nonblocking and share a wall deadline.
use crate::{
    compiler_protocol::{self as protocol, Response},
    compiler_source::Source,
};
use anyhow::{Context as _, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::Permissions,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::PermissionsExt,
    },
    process::Child,
    time::{Duration, Instant},
};
use tempfile::{TempDir, TempPath};

fn nonblocking(fd: RawFd) -> anyhow::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "compiler nonblocking pipe: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

pub(crate) struct Artifact {
    pub(crate) path: TempPath,
    // Field order unlinks the file before removing its private directory.
    _directory: TempDir,
}

pub(crate) fn exchange_io(
    child: &mut Child,
    source: &Source,
    request: &[u8],
    nonce: [u8; 32],
    fingerprint: [u8; 32],
    wall: Duration,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<(Artifact, Response)> {
    let stdin = child.stdin.take().context("compiler stdin")?;
    let mut stdout = child.stdout.take().context("compiler stdout")?;
    let mut stderr = child.stderr.take().context("compiler stderr")?;
    nonblocking(stdin.as_raw_fd())?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let mut input = std::io::Cursor::new(request).chain(source.reader()?);
    let mut input_chunk = [0; 65536];
    let mut input_len = 0;
    let mut input_at = 0;
    let mut input_done = false;
    let mut stdin = Some(stdin);
    let mut output_eof = false;
    let mut error_eof = false;
    let mut header = [0; protocol::RESPONSE_SIZE];
    let mut header_len = 0;
    let mut response = None;
    let mut trailer = [0; protocol::TRAILER_SIZE];
    let mut trailer_len = 0;
    let mut body_len = 0;
    let mut checksum = Sha256::new();
    let mut diagnostic = VecDeque::new();
    let directory = tempfile::Builder::new()
        .prefix("redis-wasm-artifact-")
        .tempdir()?;
    let mut artifact = tempfile::NamedTempFile::new_in(directory.path())?;
    let deadline = Instant::now() + wall;
    while !(trailer_len == trailer.len() && input_done) {
        ensure!(!cancelled(), "compiler operation cancelled");
        ensure!(
            Instant::now() < deadline,
            "compiler wall timeout after {} seconds",
            wall.as_secs()
        );
        ensure!(
            !output_eof,
            "compiler output ended before its complete frame and successful-exit proof"
        );
        let mut fds = [
            libc::pollfd {
                fd: stdin.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLOUT,
                revents: 0,
            },
            libc::pollfd {
                fd: if output_eof { -1 } else { stdout.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if error_eof { -1 } else { stderr.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let polled = unsafe { libc::poll(fds.as_mut_ptr(), 3, 20) };
        if polled < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            bail!("compiler poll: {}", std::io::Error::last_os_error());
        }
        if stdin.is_some() && fds[0].revents != 0 {
            if input_at == input_len {
                input_len = input.read(&mut input_chunk)?;
                input_at = 0;
                if input_len == 0 {
                    stdin.take();
                    input_done = true;
                }
            }
            if let Some(stdin) = &mut stdin {
                match stdin.write(&input_chunk[input_at..input_len]) {
                    Ok(n) => {
                        ensure!(n > 0, "compiler input pipe closed");
                        input_at += n;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error).context("write compiler input"),
                }
            }
        }
        for (index, fd) in fds.iter().enumerate().skip(1) {
            if fd.revents == 0 {
                continue;
            }
            let mut bytes = [0; 65536];
            let result = if index == 1 {
                stdout.read(&mut bytes)
            } else {
                stderr.read(&mut bytes)
            };
            match result {
                Ok(0) => {
                    if index == 1 {
                        output_eof = true;
                    } else {
                        error_eof = true;
                    }
                }
                Ok(n) if index == 2 => {
                    for byte in &bytes[..n] {
                        if diagnostic.len() == protocol::MAX_DIAGNOSTIC {
                            diagnostic.pop_front();
                        }
                        diagnostic.push_back(*byte);
                    }
                }
                Ok(n) => {
                    let mut bytes = &bytes[..n];
                    if header_len < header.len() {
                        let count = bytes.len().min(header.len() - header_len);
                        header[header_len..header_len + count].copy_from_slice(&bytes[..count]);
                        header_len += count;
                        bytes = &bytes[count..];
                        if header_len == header.len() {
                            response = Some(Response::parse(&header, nonce, fingerprint)?);
                        }
                    }
                    if !bytes.is_empty() {
                        let response =
                            response.as_ref().context("missing compiler frame header")?;
                        let count = bytes.len().min(response.len - body_len);
                        artifact.write_all(&bytes[..count])?;
                        checksum.update(&bytes[..count]);
                        body_len += count;
                        bytes = &bytes[count..];
                        ensure!(
                            trailer_len + bytes.len() <= trailer.len(),
                            "compiler emitted excess output"
                        );
                        trailer[trailer_len..trailer_len + bytes.len()].copy_from_slice(bytes);
                        trailer_len += bytes.len();
                    }
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error).context("read compiler output"),
            }
        }
    }
    let diagnostic =
        String::from_utf8_lossy(&diagnostic.into_iter().collect::<Vec<_>>()).into_owned();
    let response =
        response.with_context(|| format!("compiler emitted no complete frame: {diagnostic}"))?;
    ensure!(
        body_len == response.len,
        "compiler emitted a partial artifact: {diagnostic}"
    );
    let actual: [u8; 32] = checksum.finalize().into();
    ensure!(
        actual == response.checksum,
        "compiler artifact checksum mismatch"
    );
    if response.failed {
        let message = std::fs::read(artifact.path())?;
        bail!(
            "compiler refused component: {}",
            String::from_utf8_lossy(&message)
        );
    }
    protocol::verify_exit_trailer(&trailer, nonce)?;
    artifact
        .as_file()
        .set_permissions(Permissions::from_mode(0o400))?;
    let path = artifact.into_temp_path(); // closes the only writable descriptor
    Ok((
        Artifact {
            path,
            _directory: directory,
        },
        response,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    struct TestChild(Child);
    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn child(mode: &str) -> TestChild {
        // Never deserialized: independent pipe backpressure/framing fixture.
        let script = r#"
import sys,struct,hashlib,time
mode=sys.argv[1]
if mode=='no-read': time.sleep(5)
request=sys.stdin.buffer.read()
if mode=='stderr':
    sys.stderr.buffer.write(b'x'*(1<<20)); sys.stderr.buffer.flush()
nonce=request[12:44]; fingerprint=request[44:76]; body=b'owned compiler artifact'
if mode=='nonce': nonce=b'x'*32
size=(1<<30) if mode=='oversized' else len(body)
checksum=hashlib.sha256(body).digest()
if mode=='checksum': checksum=b'x'*32
header=b'RWCres01'+struct.pack('<I',1)+nonce+fingerprint+struct.pack('<IQQQQ',0,size,100,1000,1)+checksum+b'\0'*4
trailer=b'RWCdone1'+struct.pack('<I',1)+nonce+struct.pack('<I',0)
if mode=='partial': body=body[:-1]
if mode=='extra': body+=b'x'
if mode=='fragmented':
    for byte in header+body+trailer:
        sys.stdout.buffer.write(bytes([byte])); sys.stdout.buffer.flush()
else:
    sys.stdout.buffer.write(header+body+trailer);sys.stdout.buffer.flush()
    if mode=='held-writer': time.sleep(0.3)
"#;
        TestChild(
            Command::new("python3")
                .args(["-c", script, mode])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }

    #[test]
    fn transport_streams_verified_output_and_removes_private_artifacts() {
        for mode in ["normal", "fragmented", "stderr", "held-writer"] {
            let mut child = child(mode);
            let source = Source::spool(&vec![1; 1 << 20]).unwrap();
            let request = protocol::request([1; 32], [2; 32], source.len()).unwrap();
            let (artifact, _) = exchange_io(
                &mut child.0,
                &source,
                &request,
                [1; 32],
                [2; 32],
                Duration::from_secs(3),
                || false,
            )
            .unwrap();
            let directory = artifact.path.parent().unwrap().to_owned();
            if mode == "held-writer" {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "transport incorrectly waited for inherited-writer EOF"
                );
            }
            assert_eq!(
                std::fs::read(&artifact.path).unwrap(),
                b"owned compiler artifact"
            );
            assert_eq!(
                std::fs::metadata(&artifact.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o400
            );
            drop(artifact);
            assert!(!directory.exists());
        }
    }

    #[test]
    fn transport_refuses_foreign_oversized_partial_and_corrupt_frames() {
        for mode in ["nonce", "oversized", "partial", "extra", "checksum"] {
            let mut child = child(mode);
            let source = Source::spool(b"component").unwrap();
            let request = protocol::request([1; 32], [2; 32], source.len()).unwrap();
            assert!(
                exchange_io(
                    &mut child.0,
                    &source,
                    &request,
                    [1; 32],
                    [2; 32],
                    Duration::from_secs(3),
                    || false
                )
                .is_err(),
                "{mode}"
            );
        }
    }

    #[test]
    fn transport_deadline_covers_stalled_input_and_cancellation() {
        for cancel in [false, true] {
            let mut child = child("no-read");
            let source = Source::spool(&vec![1; 1 << 20]).unwrap();
            let request = protocol::request([1; 32], [2; 32], source.len()).unwrap();
            let started = Instant::now();
            let error = exchange_io(
                &mut child.0,
                &source,
                &request,
                [1; 32],
                [2; 32],
                Duration::from_millis(50),
                || cancel,
            )
            .err()
            .unwrap();
            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(
                error
                    .to_string()
                    .contains(if cancel { "cancelled" } else { "wall timeout" })
            );
        }
    }
}
