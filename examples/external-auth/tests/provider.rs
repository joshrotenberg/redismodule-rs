use external_auth::{Endpoint, Provider, Role, valid_credentials};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Stub {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    seen: Receiver<Vec<u8>>,
    thread: Option<JoinHandle<()>>,
}
fn request(stream: &mut TcpStream) -> Vec<u8> {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut data = Vec::new();
    let mut byte = [0];
    while !data.ends_with(b"\r\n\r\n") {
        assert!(data.len() < 4096);
        stream.read_exact(&mut byte).unwrap();
        data.push(byte[0]);
    }
    let headers = std::str::from_utf8(&data).unwrap();
    assert!(headers.starts_with("POST /verify HTTP/1.1\r\n"));
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(str::to_owned)
        })
        .unwrap()
        .parse()
        .unwrap();
    assert!(length <= 2048);
    let start = data.len();
    data.resize(start + length, 0);
    stream.read_exact(&mut data[start..]).unwrap();
    data[start..].to_vec()
}
impl Stub {
    fn new(handler: impl FnOnce(&mut TcpStream, Arc<AtomicBool>) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let (tx, seen) = mpsc::channel();
        let thread = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !cancelled.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let body = request(&mut stream);
                        tx.send(body).unwrap();
                        handler(&mut stream, cancelled);
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("owned stub accept: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            seen,
            thread: Some(thread),
        }
    }
    fn reply(status: u16, body: Vec<u8>, headers: &'static str) -> Self {
        Self::new(move |stream, _| {
            let header = format!(
                "HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
        })
    }
    fn provider(&self) -> Provider {
        Provider::new(Endpoint::parse(&self.address.to_string()).unwrap())
    }
    fn verify(&self) -> bool {
        self.provider().verify(
            b"alice",
            b"fake-password",
            Role::Reader,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_millis(500),
        )
    }
}
impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Err(payload) = self.thread.take().unwrap().join() {
            if !thread::panicking() {
                std::panic::resume_unwind(payload);
            }
        }
    }
}
fn allowed() -> Vec<u8> {
    br#"{"version":1,"allowed":true,"username":"alice","role":"reader"}"#.to_vec()
}

#[test]
fn allow_is_bound_to_requested_identity_and_configured_role() {
    let stub = Stub::reply(200, allowed(), "");
    assert!(stub.verify());
    let body: serde_json::Value =
        serde_json::from_slice(&stub.seen.recv_timeout(Duration::from_secs(1)).unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"version":1,"username":"alice","password":"fake-password"})
    );
    for body in [
        br#"{"version":1,"allowed":false,"username":"alice","role":"reader"}"#.as_slice(),
        br#"{"version":1,"allowed":true,"username":"bob","role":"reader"}"#,
        br#"{"version":1,"allowed":true,"username":"alice","role":"writer"}"#,
        br#"{"version":1,"allowed":true,"username":"alice","role":"admin"}"#,
        br#"{"version":2,"allowed":true,"username":"alice","role":"reader"}"#,
        br#"{"version":1,"allowed":true,"username":"alice","role":"reader","acl":"+@all"}"#,
        br#"{"version":1,"allowed":true,"username":"alice","username":"alice","role":"reader"}"#,
        br#"{"version":1,"allowed":true}"#,
        b"not json",
        b"\xff",
    ] {
        assert!(!Stub::reply(200, body.to_vec(), "").verify());
    }
}
#[test]
fn status_redirect_compression_and_oversize_are_denied() {
    for status in [201, 302, 401, 500] {
        assert!(
            !Stub::reply(
                status,
                allowed(),
                "Location: http://127.0.0.1:1/forbidden\r\n"
            )
            .verify()
        );
    }
    assert!(!Stub::reply(200, allowed(), "Content-Encoding: identity\r\n").verify());
    assert!(!Stub::reply(200, vec![b' '; 1025], "").verify());
    let stub = Stub::new(|stream, _| {
        let header = format!(
            "HTTP/1.1 200 OK\r\nX-Long: {}\r\nContent-Length: 0\r\n\r\n",
            "a".repeat(5000)
        );
        let _ = stream.write_all(header.as_bytes());
    });
    assert!(!stub.verify());
    // A followed redirect would reach this second owned stub. It remains idle.
    let destination = Stub::reply(200, allowed(), "");
    let address = destination.address;
    let redirect = Stub::new(move |stream, _| {
        let _ = write!(
            stream,
            "HTTP/1.1 302 Redirect\r\nLocation: http://{address}/verify\r\nContent-Length: 0\r\n\r\n"
        );
    });
    assert!(!redirect.verify());
    assert!(
        destination
            .seen
            .recv_timeout(Duration::from_millis(30))
            .is_err()
    );
}
#[test]
fn credential_and_endpoint_bounds_precede_io() {
    for endpoint in [
        "localhost:1234",
        "192.0.2.1:1234",
        "0.0.0.0:1",
        "127.0.0.1:0",
        "http://127.0.0.1:1/verify",
        "[::ffff:127.0.0.1]:1",
    ] {
        assert!(Endpoint::parse(endpoint).is_none());
    }
    assert!(Endpoint::parse("[::1]:1234").is_some());
    let stub = Stub::reply(200, allowed(), "");
    for (username, password) in [
        (b"".as_slice(), b"x".as_slice()),
        (b"alice", b""),
        (b"\xff", b"x"),
        (b"alice", b"\xff"),
        (b"bad\nname", b"x"),
        (&[b'a'; 65], b"x"),
        (b"alice", &[b'x'; 257]),
    ] {
        assert!(!stub.provider().verify(
            username,
            password,
            Role::Reader,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(1)
        ));
    }
    assert!(stub.seen.recv_timeout(Duration::from_millis(30)).is_err());
    assert!(valid_credentials("é".as_bytes(), "密碼".as_bytes()));
    assert!(valid_credentials(b"alice", b"zero\0and\ncontrol"));
}
#[test]
fn reset_connection_and_expired_deadline_deny() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let provider =
        Provider::new(Endpoint::parse(&listener.local_addr().unwrap().to_string()).unwrap());
    // Keep ownership of the port throughout: shutting down acceptance is not
    // possible portably, so expiry tests the pre-connect path without probing.
    assert!(!provider.verify(
        b"alice",
        b"fake",
        Role::Reader,
        &AtomicBool::new(false),
        Instant::now()
    ));
    let stub = Stub::new(|stream, _| {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    });
    assert!(!stub.verify());
}
#[test]
fn read_and_global_timeouts_bound_stalled_and_trickled_responses() {
    let stalled = Stub::new(|_, stop| {
        while !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
    });
    let start = Instant::now();
    assert!(!stalled.verify());
    assert!(start.elapsed() >= Duration::from_millis(300));
    assert!(start.elapsed() < Duration::from_secs(2));
    let trickled = Stub::new(|stream, stop| {
        let header =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n";
        stream.write_all(header).unwrap();
        let start = Instant::now();
        let mut tick = 0;
        while !stop.load(Ordering::Acquire) {
            if start.elapsed().as_millis() >= tick {
                if stream.write_all(b" ").is_err() {
                    break;
                }
                tick += 40;
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    let start = Instant::now();
    assert!(!trickled.verify());
    assert!(start.elapsed() >= Duration::from_millis(400));
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn cancellation_interrupts_read_and_cannot_accept_close_delimited_json() {
    for partial in [false, true] {
        let stub = Stub::new(move |stream, stop| {
            if partial {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(&allowed());
                // A cancelled close-delimited body could otherwise parse as allow.
            }
            while !stop.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
        });
        let provider = stub.provider();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_provider = provider.clone();
        let worker_cancelled = cancelled.clone();
        let worker = thread::spawn(move || {
            worker_provider.verify(
                b"alice",
                b"fake-password",
                Role::Reader,
                &worker_cancelled,
                Instant::now() + Duration::from_millis(500),
            )
        });
        stub.seen.recv_timeout(Duration::from_secs(1)).unwrap();
        cancelled.store(true, Ordering::Release);
        provider.cancel();
        assert!(!worker.join().unwrap());
        assert!(!provider.verify(
            b"alice",
            b"fake",
            Role::Reader,
            &cancelled,
            Instant::now() + Duration::from_secs(1)
        ));
    }
}
#[test]
fn proxy_environment_is_ignored() {
    if std::env::var("EXTERNAL_AUTH_PROXY_CHILD").as_deref() == Ok("1") {
        let provider = Provider::new(
            Endpoint::parse(&std::env::var("EXTERNAL_AUTH_ENDPOINT").unwrap()).unwrap(),
        );
        assert!(provider.verify(
            b"alice",
            b"fake-password",
            Role::Reader,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_millis(500)
        ));
        return;
    }
    let target = Stub::reply(200, allowed(), "");
    let proxy = Stub::reply(500, Vec::new(), "");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "proxy_environment_is_ignored", "--nocapture"])
        .env("EXTERNAL_AUTH_PROXY_CHILD", "1")
        .env("EXTERNAL_AUTH_ENDPOINT", target.address.to_string())
        .env("HTTP_PROXY", format!("http://{}", proxy.address))
        .env("http_proxy", format!("http://{}", proxy.address))
        .env("ALL_PROXY", format!("http://{}", proxy.address))
        .env("all_proxy", format!("http://{}", proxy.address))
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(proxy.seen.recv_timeout(Duration::from_millis(30)).is_err());
}
