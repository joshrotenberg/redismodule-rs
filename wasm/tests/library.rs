//! Loads the example guest in a Redis server and calls its functions.
//!
//! Needs `redis-server` on the PATH and the `wasm32-unknown-unknown` target.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

use redis::{Connection, RedisResult, Value};

/// The functions the example guest provides, in its order.
const FUNCTIONS: [&str; 10] = [
    "hello",
    "incr_twice",
    "hash_pairs",
    "set",
    "nested",
    "fail",
    "recurse",
    "spin",
    "write_then_spin",
    "counter",
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

/// The example guest as a component, built once for all tests.
fn hello_component() -> &'static [u8] {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(build_hello_component)
}

/// Builds the example guest and turns the core module into a component.
fn build_hello_component() -> Vec<u8> {
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
    port: u16,
}

impl Server {
    fn start(port: u16, extra_args: &[&str]) -> Self {
        let dir = target_dir().join(format!("wasm-test-{port}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("server directory");
        let log = dir.join("redis.log");
        let child = Command::new("redis-server")
            .args([
                "--port",
                &port.to_string(),
                "--save",
                "",
                "--appendonly",
                "no",
            ])
            .arg("--dir")
            .arg(&dir)
            .arg("--logfile")
            .arg(&log)
            .args(extra_args)
            .arg("--loadmodule")
            .arg(module_path())
            .stdout(Stdio::null())
            .spawn()
            .expect("start redis-server");
        let server = Self { child, log, port };
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
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
        redis::Client::open(format!("redis://127.0.0.1:{}/", self.port))
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
fn raw_reply(port: u16, args: &[&str]) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
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
    let server = Server::start(6610, &[]);
    let mut con = server.connection();

    let functions: Vec<String> = redis::cmd("WASM.LOAD")
        .arg("hello")
        .arg(component)
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
        raw_reply(server.port, &["WASM.CALL", "nested", "0"]),
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
        .arg(component)
        .query(&mut con);
    assert!(error_text(again).contains("already exists"));
    let _: Vec<String> = redis::cmd("WASM.LOAD")
        .arg("hello")
        .arg(component)
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

/// Polls `f` until it returns `Some`, for up to ten seconds.
fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(20));
    }
}

/// A server where a running function counts as busy after 100 ms, with the
/// example guest loaded.
fn busy_server(port: u16) -> (Server, Connection) {
    let server = Server::start(port, &["--busy-reply-threshold", "100"]);
    let mut con = server.connection();
    let _: Vec<String> = redis::cmd("WASM.LOAD")
        .arg("hello")
        .arg(hello_component())
        .query(&mut con)
        .expect("WASM.LOAD");
    (server, con)
}

/// The `-BUSY` reply another client gets while a function runs.
fn wait_for_busy(con: &mut Connection) -> String {
    wait_for("a BUSY reply", || {
        match redis::cmd("PING").query::<Value>(con) {
            Err(error) if error.code() == Some("BUSY") => Some(error_text(Err(error))),
            _ => None,
        }
    })
}

fn kill(con: &mut Connection) -> RedisResult<Value> {
    redis::cmd("WASM.KILL").query(con)
}

#[test]
fn a_busy_function_gets_busy_replies_and_can_be_killed() {
    let (server, mut con) = busy_server(6611);
    assert_eq!(
        error_text(kill(&mut con)),
        "NOTBUSY No wasm function is running."
    );

    let mut spinner = server.connection();
    let spin = std::thread::spawn(move || error_text(call(&mut spinner, &["spin", "0"])));

    let busy = wait_for_busy(&mut con);
    assert_eq!(
        busy,
        "BUSY a wasm function is running. You can only call WASM.KILL or SHUTDOWN NOSAVE."
    );
    // Commands without allow-busy, including the module's own, get -BUSY too.
    let list: RedisResult<Value> = redis::cmd("WASM.LIST").query(&mut con);
    assert!(error_text(list).starts_with("BUSY"));

    assert_eq!(kill(&mut con).unwrap(), Value::Okay);
    assert_eq!(
        spin.join().unwrap(),
        "ERR function 'spin' failed: killed by WASM.KILL"
    );
    assert_eq!(
        redis::cmd("PING").query::<String>(&mut con).unwrap(),
        "PONG"
    );
    assert!(error_text(kill(&mut con)).starts_with("NOTBUSY"));
}

#[test]
fn a_function_that_wrote_cannot_be_killed() {
    let (mut server, mut con) = busy_server(6612);
    let mut spinner = server.connection();
    let spin = std::thread::spawn(move || call(&mut spinner, &["write_then_spin", "1", "k"]));

    wait_for_busy(&mut con);
    assert!(
        error_text(kill(&mut con))
            .starts_with("UNKILLABLE The function has already run write commands."),
    );

    // SHUTDOWN NOSAVE is the way out, as with a Lua script.
    let _ = redis::cmd("SHUTDOWN")
        .arg("NOSAVE")
        .query::<Value>(&mut con);
    let child = &mut server.child;
    wait_for("redis-server to exit", || child.try_wait().ok().flatten());
    assert!(spin.join().unwrap().is_err());
}

fn counter(con: &mut Connection) -> i64 {
    match call(con, &["counter", "0"]).unwrap() {
        Value::Int(n) => n,
        other => panic!("counter replied {other:?}"),
    }
}

#[test]
fn a_library_keeps_its_instance_until_a_call_fails_or_it_is_replaced() {
    let (server, mut con) = busy_server(6614);
    // Calls share the instance, so the guest's state carries over.
    assert_eq!(
        [counter(&mut con), counter(&mut con), counter(&mut con)],
        [1, 2, 3]
    );
    // An error reply is a successful call: the instance stays.
    assert!(error_text(call(&mut con, &["fail", "0"])).starts_with("ERR"));
    assert_eq!(counter(&mut con), 4);

    // A killed call drops the instance; the next call gets a new one.
    let mut spinner = server.connection();
    let spin = std::thread::spawn(move || error_text(call(&mut spinner, &["spin", "0"])));
    wait_for_busy(&mut con);
    assert_eq!(kill(&mut con).unwrap(), Value::Okay);
    assert!(spin.join().unwrap().contains("killed by WASM.KILL"));
    assert_eq!(counter(&mut con), 1);

    // Replacing the library starts over too.
    let _: Vec<String> = redis::cmd("WASM.LOAD")
        .arg("hello")
        .arg(hello_component())
        .arg("REPLACE")
        .query(&mut con)
        .expect("WASM.LOAD REPLACE");
    assert_eq!(counter(&mut con), 1);
}

/// The JavaScript example (`guests/js`) as a component, built once, or `None`
/// when Node.js is not installed.
fn js_component() -> Option<&'static [u8]> {
    static COMPONENT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    COMPONENT.get_or_init(build_js_component).as_deref()
}

fn build_js_component() -> Option<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("guests/js");
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping the JavaScript guest: node is not installed");
        return None;
    }
    if !dir.join("node_modules").exists() {
        let status = Command::new("npm")
            .args(["ci", "--no-audit", "--no-fund"])
            .current_dir(&dir)
            .status()
            .expect("run npm ci");
        assert!(status.success(), "npm ci failed for the JavaScript guest");
    }
    let output = target_dir().join("wasm-guests/js-library.wasm");
    std::fs::create_dir_all(output.parent().unwrap()).expect("guest output directory");
    let status = Command::new("node")
        .arg("build.mjs")
        .arg(&output)
        .current_dir(&dir)
        .status()
        .expect("run node build.mjs");
    assert!(status.success(), "building the JavaScript guest failed");
    Some(std::fs::read(&output).expect("read the JavaScript component"))
}

#[test]
fn a_javascript_library_behaves_like_the_rust_one() {
    let Some(component) = js_component() else {
        return;
    };
    let server = Server::start(6615, &["--busy-reply-threshold", "100"]);
    let mut con = server.connection();
    let functions: Vec<String> = redis::cmd("WASM.LOAD")
        .arg("js")
        .arg(component)
        .query(&mut con)
        .expect("WASM.LOAD of the JavaScript library");
    assert_eq!(
        functions,
        [
            "hello",
            "incr_twice",
            "set",
            "nested",
            "fail",
            "counter",
            "spin"
        ]
    );

    assert_eq!(
        call(&mut con, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );
    assert_eq!(
        call(&mut con, &["hello", "0", "js"]).unwrap(),
        Value::Status("hello, js".into())
    );
    assert!(
        server.log().contains("hello called for js"),
        "{}",
        server.log()
    );
    assert_eq!(
        call(&mut con, &["incr_twice", "1", "counter"]).unwrap(),
        Value::Bulk(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        call(&mut con, &["set", "1", "k", "v"]).unwrap(),
        Value::Okay
    );
    assert_eq!(
        raw_reply(server.port, &["WASM.CALL", "nested", "0"]),
        "*3\r\n*2\r\n$6\r\nanswer\r\n:42\r\n$-1\r\n-ERR an error inside an array\r\n"
    );
    assert_eq!(
        error_text(call(&mut con, &["fail", "0"])),
        "ERR this function always fails"
    );
    assert_eq!([counter(&mut con), counter(&mut con)], [1, 2]);

    // A JavaScript loop yields and can be killed like a Rust one.
    let mut spinner = server.connection();
    let spin = std::thread::spawn(move || error_text(call(&mut spinner, &["spin", "0"])));
    wait_for_busy(&mut con);
    assert_eq!(kill(&mut con).unwrap(), Value::Okay);
    assert_eq!(
        spin.join().unwrap(),
        "ERR function 'spin' failed: killed by WASM.KILL"
    );
    assert_eq!(counter(&mut con), 1);
}
