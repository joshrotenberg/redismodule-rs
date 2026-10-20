//! Loads the example guest in a Redis server and calls its functions.
//!
//! Needs `redis-server` on the PATH and the `wasm32-unknown-unknown` target.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use redis::{Connection, RedisResult, Value};

const PORT: u16 = 6610;

/// The functions the example guest provides, in its order.
const FUNCTIONS: [&str; 7] = [
    "hello",
    "incr_twice",
    "hash_pairs",
    "set",
    "nested",
    "fail",
    "recurse",
];

/// The workspace's `target` directory, from this test binary's path
/// (`target/debug/deps/library-...`).
fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    exe.ancestors()
        .nth(3)
        .expect("target directory")
        .to_path_buf()
}

fn module_path() -> PathBuf {
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    target_dir()
        .join("debug")
        .join(format!("libredis_module_wasm.{ext}"))
}

/// Builds the example guest and turns the core module into a component.
fn hello_component() -> Vec<u8> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("guests/hello/Cargo.toml");
    let guest_target = target_dir().join("wasm-guests");
    let status = Command::new("cargo")
        .args([
            "build",
            "--quiet",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .env("CARGO_TARGET_DIR", &guest_target)
        .status()
        .expect("run cargo for the guest");
    assert!(status.success(), "building the guest failed");
    let core = std::fs::read(guest_target.join("wasm32-unknown-unknown/release/hello_guest.wasm"))
        .expect("read the guest module");
    wit_component::ComponentEncoder::default()
        .module(&core)
        .expect("a module with component type information")
        .validate(true)
        .encode()
        .expect("encode the component")
}

/// A Redis server with the module loaded, stopped on drop.
struct Server {
    child: Child,
    log: PathBuf,
}

impl Server {
    fn start() -> Self {
        let dir = target_dir().join(format!("wasm-test-{PORT}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("server directory");
        let log = dir.join("redis.log");
        let child = Command::new("redis-server")
            .args([
                "--port",
                &PORT.to_string(),
                "--save",
                "",
                "--appendonly",
                "no",
            ])
            .arg("--dir")
            .arg(&dir)
            .arg("--logfile")
            .arg(&log)
            .arg("--loadmodule")
            .arg(module_path())
            .stdout(Stdio::null())
            .spawn()
            .expect("start redis-server");
        let server = Self { child, log };
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(("127.0.0.1", PORT)).is_err() {
            assert!(
                Instant::now() < deadline,
                "redis-server did not start; log:\n{}",
                server.log()
            );
            sleep(Duration::from_millis(50));
        }
        server
    }

    fn connection(&self) -> Connection {
        redis::Client::open(format!("redis://127.0.0.1:{PORT}/"))
            .and_then(|client| client.get_connection())
            .expect("connect")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn call(con: &mut Connection, args: &[&str]) -> RedisResult<Value> {
    let mut cmd = redis::cmd("WASM.CALL");
    for arg in args {
        cmd.arg(*arg);
    }
    cmd.query(con)
}

fn error_text(result: RedisResult<Value>) -> String {
    let error = result.expect_err("an error reply");
    format!(
        "{} {}",
        error.code().unwrap_or_default(),
        error.detail().unwrap_or_default()
    )
}

/// Sends a command in RESP and returns the raw reply, for replies the client
/// library cannot represent, such as an error inside an array.
fn raw_reply(args: &[&str]) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", PORT)).expect("connect");
    let mut request = format!("*{}\r\n", args.len());
    for arg in args {
        request.push_str(&format!("${}\r\n{arg}\r\n", arg.len()));
    }
    stream.write_all(request.as_bytes()).expect("send");
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("timeout");
    let mut reply = Vec::new();
    let mut buffer = [0; 1024];
    while let Ok(n) = stream.read(&mut buffer) {
        if n == 0 {
            break;
        }
        reply.extend_from_slice(&buffer[..n]);
    }
    String::from_utf8_lossy(&reply).into_owned()
}

#[test]
fn load_call_list_and_delete_a_library() {
    let component = hello_component();
    let server = Server::start();
    let mut con = server.connection();

    let functions: Vec<String> = redis::cmd("WASM.LOAD")
        .arg("hello")
        .arg(&component)
        .query(&mut con)
        .expect("WASM.LOAD");
    assert_eq!(functions, FUNCTIONS);

    // A simple string, and a log line from the guest.
    assert_eq!(
        call(&mut con, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );
    assert_eq!(
        call(&mut con, &["hello", "0", "redis"]).unwrap(),
        Value::Status("hello, redis".into())
    );
    assert!(
        server.log().contains("hello called for redis"),
        "{}",
        server.log()
    );

    // Calls into Redis, with their typed replies passed through.
    assert_eq!(
        call(&mut con, &["incr_twice", "1", "counter"]).unwrap(),
        Value::Bulk(vec![Value::Int(1), Value::Int(2)])
    );
    let _: () = redis::cmd("HSET")
        .arg(&["h", "a", "1"])
        .query(&mut con)
        .unwrap();
    assert_eq!(
        call(&mut con, &["hash_pairs", "1", "h"]).unwrap(),
        Value::Bulk(vec![Value::Data(b"a".to_vec()), Value::Data(b"1".to_vec())])
    );

    assert_eq!(
        call(&mut con, &["set", "1", "k", "v"]).unwrap(),
        Value::Okay
    );

    // An error reply, an error inside an array, and a guest that may not run
    // a WASM command.
    assert_eq!(
        error_text(call(&mut con, &["fail", "0"])),
        "ERR this function always fails"
    );
    // The map key goes out as a bulk string: see `to_key` in src/reply.rs.
    assert_eq!(
        raw_reply(&["WASM.CALL", "nested", "0"]),
        "*3\r\n*2\r\n$6\r\nanswer\r\n:42\r\n$-1\r\n-ERR an error inside an array\r\n"
    );
    let recurse = error_text(call(&mut con, &["recurse", "0"]));
    assert!(recurse.contains("not allowed"), "{recurse}");

    // Bad calls.
    assert_eq!(
        error_text(call(&mut con, &["nosuch", "0"])),
        "ERR function 'nosuch' not found"
    );
    assert!(error_text(call(&mut con, &["hello", "2", "only-one"])).contains("numkeys"));

    // Loading again needs REPLACE; a broken component is refused.
    let again: RedisResult<Value> = redis::cmd("WASM.LOAD")
        .arg("hello")
        .arg(&component)
        .query(&mut con);
    assert!(error_text(again).contains("already exists"));
    let _: Vec<String> = redis::cmd("WASM.LOAD")
        .arg("hello")
        .arg(&component)
        .arg("REPLACE")
        .query(&mut con)
        .expect("WASM.LOAD REPLACE");
    let broken: RedisResult<Value> = redis::cmd("WASM.LOAD")
        .arg("broken")
        .arg(b"not wasm".as_slice())
        .query(&mut con);
    assert!(error_text(broken).contains("invalid component"));

    // List, then delete.
    assert_eq!(
        redis::cmd("WASM.LIST").query::<Value>(&mut con).unwrap(),
        Value::Bulk(vec![Value::Bulk(vec![
            Value::Data(b"hello".to_vec()),
            Value::Bulk(
                FUNCTIONS
                    .iter()
                    .map(|f| Value::Data(f.as_bytes().to_vec()))
                    .collect()
            ),
        ])])
    );
    assert_eq!(
        redis::cmd("WASM.DELETE")
            .arg("hello")
            .query::<Value>(&mut con)
            .unwrap(),
        Value::Okay
    );
    assert_eq!(
        error_text(call(&mut con, &["hello", "0"])),
        "ERR function 'hello' not found"
    );
}
