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
const FUNCTIONS: [&str; 22] = [
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
    "grow",
    "run_ro",
    "set_oom",
    "stale_ok",
    "local_only",
    "get_args",
    "get_args_cross",
    "run",
    "acl_check",
    "resp_shape",
    "oob",
    "run_flat",
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

/// The module, built once for all tests: `cargo test` does not refresh the
/// module's dylib, so a test could otherwise load a stale one.
fn module_path() -> PathBuf {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "-p", "redis-module-wasm"])
            .status()
            .expect("run cargo for the module");
        assert!(status.success(), "building the module failed");
    });
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

fn server_dir(port: u16) -> PathBuf {
    target_dir().join(format!("wasm-test-{port}"))
}

impl Server {
    /// Starts a server in a new, empty directory.
    fn start(port: u16, extra_args: &[&str]) -> Self {
        let dir = server_dir(port);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("server directory");
        Self::launch(port, extra_args, true)
    }

    /// Starts a server in the directory the last server on `port` used, so it
    /// loads that server's RDB or AOF.
    fn restart(port: u16, extra_args: &[&str]) -> Self {
        Self::launch(port, extra_args, true)
    }

    fn launch(port: u16, extra_args: &[&str], with_module: bool) -> Self {
        let dir = server_dir(port);
        let log = dir.join("redis.log");
        let mut command = Command::new("redis-server");
        command
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
            .args(extra_args);
        if with_module {
            command.arg("--loadmodule").arg(module_path());
        }
        let child = command
            .stdout(Stdio::null())
            .spawn()
            .expect("start redis-server");
        let server = Self { child, log, port };
        let deadline = Instant::now() + Duration::from_secs(10);
        // Redis accepts connections, and answers INFO, while it is still
        // loading an RDB or AOF, so wait for the load to finish too.
        while !server.loaded() {
            assert!(
                Instant::now() < deadline,
                "redis-server did not start and load its data; log:\n{}",
                server.log()
            );
            sleep(Duration::from_millis(50));
        }
        server
    }

    /// Whether the server accepts connections and has finished loading its
    /// RDB or AOF.
    fn loaded(&self) -> bool {
        TcpStream::connect(("127.0.0.1", self.port)).is_ok()
            && redis::Client::open(format!("redis://127.0.0.1:{}/", self.port))
                .and_then(|client| client.get_connection())
                .and_then(|mut con| redis::cmd("INFO").arg("persistence").query::<String>(&mut con))
                .is_ok_and(|info| info.lines().any(|line| line.trim_end() == "loading:0"))
    }

    fn connection(&self) -> Connection {
        redis::Client::open(format!("redis://127.0.0.1:{}/", self.port))
            .and_then(|client| client.get_connection())
            .expect("connect")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Stops the server with `SHUTDOWN`, which writes out the AOF, and waits
    /// for it to exit.
    fn shutdown(mut self) {
        let _ = redis::cmd("SHUTDOWN").query::<Value>(&mut self.connection());
        let child = &mut self.child;
        wait_for("redis-server to exit", || child.try_wait().ok().flatten());
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
    // The map key keeps its type: a simple string.
    assert_eq!(
        raw_reply(server.port, &["WASM.CALL", "nested", "0"]),
        "*3\r\n*2\r\n+answer\r\n:42\r\n$-1\r\n-ERR an error inside an array\r\n"
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
    assert_eq!(list(&mut con), listed(&["hello"]));
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
    // WASM.STATS answers while the function runs, and names it.
    let stats: Vec<Value> = redis::cmd("WASM.STATS").query(&mut con).unwrap();
    let Value::Bulk(running) = &stats[1] else {
        panic!("a running function in {stats:?}");
    };
    assert_eq!(running[1], Value::Data(b"spin".to_vec()));
    assert!(matches!(running[3], Value::Int(ms) if ms >= 0));
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
    let stats: Vec<Value> = redis::cmd("WASM.STATS").query(&mut con).unwrap();
    assert_eq!(
        stats,
        [
            Value::Status("running_function".into()),
            Value::Nil,
            Value::Status("libraries_count".into()),
            Value::Int(1),
            Value::Status("functions_count".into()),
            Value::Int(FUNCTIONS.len() as i64),
        ]
    );
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
/// when Node.js is not installed and JavaScript coverage is not required.
fn js_component() -> Option<&'static [u8]> {
    static COMPONENT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    COMPONENT.get_or_init(build_js_component).as_deref()
}

fn build_js_component() -> Option<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("guests/js");
    match Command::new("node").arg("--version").output() {
        Ok(output) => assert!(output.status.success(), "node --version failed"),
        Err(error) => {
            assert!(
                std::env::var_os("WASM_TEST_REQUIRE_JS").is_none(),
                "JavaScript guest coverage is required but node is unavailable: {error}"
            );
            eprintln!("skipping the JavaScript guest: node is not installed");
            return None;
        }
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
    // WIT flags reach the host from JavaScript objects of booleans.
    assert_eq!(flags_of(&mut con, "hello"), ["no-writes"]);
    assert!(flags_of(&mut con, "set").is_empty());

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
        "*3\r\n*2\r\n+answer\r\n:42\r\n$-1\r\n-ERR an error inside an array\r\n"
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

#[test]
fn a_slow_load_does_not_block_other_clients() {
    let Some(component) = js_component() else {
        return;
    };
    let server = Server::start(6616, &[]);
    let mut con = server.connection();

    // Two loads of the same name: both compile on threads, and the second to
    // finish finds the name taken.
    let loads: Vec<_> = (0..2)
        .map(|_| {
            let mut loader = server.connection();
            std::thread::spawn(move || -> RedisResult<Vec<String>> {
                redis::cmd("WASM.LOAD")
                    .arg("js")
                    .arg(component)
                    .query(&mut loader)
            })
        })
        .collect();

    // The 11 MB component takes seconds to compile; the server keeps
    // answering meanwhile.
    let mut answered_while_loading = 0;
    let mut slowest = Duration::ZERO;
    while loads.iter().any(|load| !load.is_finished()) {
        let start = Instant::now();
        let _: String = redis::cmd("PING").query(&mut con).unwrap();
        slowest = slowest.max(start.elapsed());
        answered_while_loading += 1;
        sleep(Duration::from_millis(20));
    }
    assert!(
        answered_while_loading > 5,
        "{answered_while_loading} PINGs answered"
    );
    assert!(
        slowest < Duration::from_millis(500),
        "slowest PING took {slowest:?}"
    );

    let results: Vec<_> = loads.into_iter().map(|load| load.join().unwrap()).collect();
    let loaded: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(loaded.len(), 1, "{results:?}");
    assert_eq!(loaded[0][0], "hello");
    let refused = results.into_iter().find_map(Result::err).unwrap();
    assert!(error_text(Err(refused)).contains("library 'js' already exists"));
    assert_eq!(
        call(&mut con, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );
}

#[test]
fn a_load_inside_multi_compiles_inline() {
    let server = Server::start(6617, &[]);
    let mut con = server.connection();
    let (functions,): (Vec<String>,) = redis::pipe()
        .atomic()
        .cmd("WASM.LOAD")
        .arg("hello")
        .arg(hello_component())
        .query(&mut con)
        .expect("WASM.LOAD in MULTI");
    assert_eq!(functions, FUNCTIONS);
    assert_eq!(counter(&mut con), 1);
}

/// Loads the example guest as library `name`.
fn load(con: &mut Connection, name: &str) {
    let functions: Vec<String> = redis::cmd("WASM.LOAD")
        .arg(name)
        .arg(hello_component())
        .query(con)
        .expect("WASM.LOAD");
    assert_eq!(functions, FUNCTIONS);
}

/// Each library with its functions and their flags.
type Listing = Vec<(String, Vec<(String, Vec<String>)>)>;

/// `WASM.LIST` as a `Listing`.
fn list_with_flags(con: &mut Connection) -> Listing {
    // redis-rs reads a Vec of pairs from a flat array, so read each pair.
    let pair =
        |value: &Value| -> (String, Value) { redis::from_redis_value(value).expect("a pair") };
    let entries: Vec<Value> = redis::cmd("WASM.LIST").query(con).expect("WASM.LIST");
    entries
        .iter()
        .map(|entry| {
            let (library, functions) = pair(entry);
            let functions: Vec<Value> = redis::from_redis_value(&functions).unwrap();
            let functions = functions
                .iter()
                .map(|function| {
                    let (name, flags) = pair(function);
                    (name, redis::from_redis_value(&flags).unwrap())
                })
                .collect();
            (library, functions)
        })
        .collect()
}

/// `WASM.LIST` as `(library, function names)` pairs.
fn list(con: &mut Connection) -> Vec<(String, Vec<String>)> {
    list_with_flags(con)
        .into_iter()
        .map(|(library, functions)| {
            (
                library,
                functions.into_iter().map(|(name, _)| name).collect(),
            )
        })
        .collect()
}

/// The flags `WASM.LIST` shows for `function`.
fn flags_of(con: &mut Connection, function: &str) -> Vec<String> {
    list_with_flags(con)
        .into_iter()
        .flat_map(|(_, functions)| functions)
        .find(|(name, _)| name == function)
        .map(|(_, flags)| flags)
        .unwrap_or_else(|| panic!("no function {function}"))
}

/// What `list` returns with the example guest loaded as each of `names`.
fn listed(names: &[&str]) -> Vec<(String, Vec<String>)> {
    names
        .iter()
        .map(|name| (name.to_string(), FUNCTIONS.map(String::from).to_vec()))
        .collect()
}

#[test]
fn libraries_survive_a_reload_and_a_restart() {
    let args = ["--enable-debug-command", "yes"];
    let server = Server::start(6618, &args);
    let mut con = server.connection();
    load(&mut con, "hello");
    assert_eq!([counter(&mut con), counter(&mut con)], [1, 2]);

    // DEBUG RELOAD saves an RDB, empties the server and loads the RDB. The
    // library comes back with a new instance.
    let _: () = redis::cmd("DEBUG").arg("RELOAD").query(&mut con).unwrap();
    assert_eq!(list(&mut con), listed(&["hello"]));
    assert_eq!(counter(&mut con), 1);

    let _: () = redis::cmd("SAVE").query(&mut con).unwrap();
    server.shutdown();
    let server = Server::restart(6618, &args);
    let mut con = server.connection();
    assert_eq!(list(&mut con), listed(&["hello"]));
    assert_eq!(
        call(&mut con, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );
    assert_eq!(counter(&mut con), 1);

    // An RDB saved with no libraries has nothing from the module, so a server
    // without the module loads it.
    let _: () = redis::cmd("WASM.DELETE")
        .arg("hello")
        .query(&mut con)
        .unwrap();
    let _: () = redis::cmd("SET").arg(&["k", "v"]).query(&mut con).unwrap();
    let _: () = redis::cmd("SAVE").query(&mut con).unwrap();
    server.shutdown();
    let server = Server::launch(6618, &[], false);
    let value: String = redis::cmd("GET")
        .arg("k")
        .query(&mut server.connection())
        .unwrap_or_else(|e| panic!("{e}; log:\n{}", server.log()));
    assert_eq!(value, "v");
}

#[test]
fn libraries_survive_a_restart_with_the_aof() {
    let args = ["--appendonly", "yes"];
    let server = Server::start(6621, &args);
    let mut con = server.connection();
    load(&mut con, "hello");

    // The AOF has the WASM.LOAD, which runs again when the server starts.
    server.shutdown();
    let server = Server::restart(6621, &args);
    let mut con = server.connection();
    assert_eq!(list(&mut con), listed(&["hello"]));

    // A rewritten AOF starts with an RDB, which has the library.
    let _: () = redis::cmd("BGREWRITEAOF").query(&mut con).unwrap();
    wait_for("the AOF rewrite", || {
        let info: String = redis::cmd("INFO").arg("persistence").query(&mut con).ok()?;
        (info.contains("aof_rewrite_in_progress:0") && info.contains("aof_rewrite_scheduled:0"))
            .then_some(())
    });
    server.shutdown();
    let server = Server::restart(6621, &args);
    let mut con = server.connection();
    assert_eq!(list(&mut con), listed(&["hello"]));
    assert_eq!(
        call(&mut con, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );
}

#[test]
fn a_replica_gets_the_libraries_and_their_changes() {
    let primary = Server::start(6619, &[]);
    let mut p = primary.connection();
    load(&mut p, "hello");
    assert_eq!(
        call(&mut p, &["incr_twice", "1", "counter"]).unwrap(),
        Value::Bulk(vec![Value::Int(1), Value::Int(2)])
    );

    // The replica has a library of its own, which the full sync replaces.
    let replica = Server::start(6620, &[]);
    let mut r = replica.connection();
    load(&mut r, "local");
    let _: () = redis::cmd("REPLICAOF")
        .arg(&["127.0.0.1", "6619"])
        .query(&mut r)
        .unwrap();
    wait_for("the replica to sync", || {
        let info: String = redis::cmd("INFO").arg("replication").query(&mut r).ok()?;
        info.contains("master_link_status:up").then_some(())
    });
    assert_eq!(list(&mut r), listed(&["hello"]));
    let value: String = redis::cmd("GET").arg("counter").query(&mut r).unwrap();
    assert_eq!(value, "2");

    // Loads and deletes replicate in order: a delete right after a load does
    // not reach the replica before the load.
    let wait = |p: &mut Connection| {
        let acked: i64 = redis::cmd("WAIT").arg(1).arg(5000).query(p).unwrap();
        assert_eq!(acked, 1, "replica log:\n{}", replica.log());
    };
    let delete = |p: &mut Connection, library: &str| {
        let _: () = redis::cmd("WASM.DELETE").arg(library).query(p).unwrap();
    };
    delete(&mut p, "hello");
    load(&mut p, "again");
    delete(&mut p, "again");
    wait(&mut p);
    assert_eq!(list(&mut r), listed(&[]));

    load(&mut p, "again");
    assert_eq!(
        call(&mut p, &["incr_twice", "1", "counter"]).unwrap(),
        Value::Bulk(vec![Value::Int(3), Value::Int(4)])
    );
    wait(&mut p);
    assert_eq!(list(&mut r), listed(&["again"]));
    let value: String = redis::cmd("GET").arg("counter").query(&mut r).unwrap();
    assert_eq!(value, "4");

    // WASM.FLUSH and WASM.RESTORE replicate too.
    let payload: Vec<u8> = redis::cmd("WASM.DUMP").query(&mut p).unwrap();
    let _: () = redis::cmd("WASM.FLUSH").query(&mut p).unwrap();
    wait(&mut p);
    assert_eq!(list(&mut r), listed(&[]));
    let _: () = redis::cmd("WASM.RESTORE")
        .arg(&payload)
        .query(&mut p)
        .unwrap();
    wait(&mut p);
    assert_eq!(list(&mut r), listed(&["again"]));
}

/// Parses each entry of an array reply on its own; see `list`.
fn entries<T: redis::FromRedisValue>(reply: Value) -> Vec<T> {
    let Value::Bulk(entries) = reply else {
        panic!("an array reply, not {reply:?}");
    };
    entries
        .iter()
        .map(|entry| redis::from_redis_value(entry).expect("an entry"))
        .collect()
}

#[test]
fn wasm_call_declares_its_keys() {
    let server = Server::start(6622, &[]);
    let mut con = server.connection();
    load(&mut con, "hello");

    let getkeys = |con: &mut Connection, args: &[&str]| -> Vec<String> {
        redis::cmd("COMMAND")
            .arg("GETKEYS")
            .arg("WASM.CALL")
            .arg(args)
            .query(con)
            .unwrap()
    };
    assert_eq!(
        getkeys(&mut con, &["hello", "2", "a", "b", "x"]),
        ["a", "b"]
    );
    assert!(getkeys(&mut con, &["hello", "0", "x"]).is_empty());

    // A function may do anything with a key, so the flags assume the worst,
    // as FCALL's do.
    let flags: Vec<(String, Vec<String>)> = entries(
        redis::cmd("COMMAND")
            .arg(&["GETKEYSANDFLAGS", "WASM.CALL", "hello", "1", "a"])
            .query(&mut con)
            .unwrap(),
    );
    assert_eq!(
        flags,
        [(
            "a".to_string(),
            vec!["RW".into(), "access".into(), "update".into()]
        )]
    );

    // ACL key patterns apply to the keys WASM.CALL declares.
    let _: () = redis::cmd("ACL")
        .arg(&["SETUSER", "limited", "on", "nopass", "~a*", "+@all"])
        .query(&mut con)
        .unwrap();
    let mut limited = server.connection();
    let _: () = redis::cmd("AUTH")
        .arg(&["limited", "any"])
        .query(&mut limited)
        .unwrap();
    assert_eq!(
        call(&mut limited, &["hello", "1", "a1"]).unwrap(),
        Value::Status("hello, world".into())
    );
    assert!(error_text(call(&mut limited, &["hello", "1", "b1"])).starts_with("NOPERM"));

    // The WASM commands are in @scripting, as FCALL and FUNCTION are.
    let _: () = redis::cmd("ACL")
        .arg(&[
            "SETUSER",
            "noscripts",
            "on",
            "nopass",
            "~*",
            "+@all",
            "-@scripting",
        ])
        .query(&mut con)
        .unwrap();
    let mut noscripts = server.connection();
    let _: () = redis::cmd("AUTH")
        .arg(&["noscripts", "any"])
        .query(&mut noscripts)
        .unwrap();
    assert!(error_text(call(&mut noscripts, &["hello", "0"])).starts_with("NOPERM"));
    assert!(error_text(redis::cmd("WASM.LIST").query(&mut noscripts)).starts_with("NOPERM"));
}

#[test]
fn a_cluster_routes_wasm_call_by_its_keys() {
    // Two primaries, each serving half the slots.
    let args = ["--cluster-enabled", "yes"];
    let a = Server::start(6623, &args);
    let b = Server::start(6624, &args);
    let mut ca = a.connection();
    let mut cb = b.connection();
    for (con, first, last) in [(&mut ca, "0", "8191"), (&mut cb, "8192", "16383")] {
        let _: () = redis::cmd("CLUSTER")
            .arg(&["ADDSLOTSRANGE", first, last])
            .query(con)
            .unwrap();
    }
    let _: () = redis::cmd("CLUSTER")
        .arg(&["MEET", "127.0.0.1", "6624"])
        .query(&mut ca)
        .unwrap();
    for con in [&mut ca, &mut cb] {
        wait_for("the cluster to form", || {
            let info: String = redis::cmd("CLUSTER").arg("INFO").query(con).ok()?;
            info.contains("cluster_state:ok").then_some(())
        });
    }
    load(&mut ca, "hello");

    // Keys and their slots: two served by a in different slots, one by b.
    let mut keys = (0..100).map(|i| {
        let key = format!("k{i}");
        let slot: i64 = redis::cmd("CLUSTER")
            .arg(&["KEYSLOT", &key])
            .query(&mut ca)
            .unwrap();
        (key, slot)
    });
    let (on_a, slot_a) = keys.by_ref().find(|(_, slot)| *slot < 8192).unwrap();
    let (on_a_too, _) = keys
        .by_ref()
        .find(|(_, slot)| *slot < 8192 && *slot != slot_a)
        .unwrap();
    let (on_b, slot_b) = keys.find(|(_, slot)| *slot >= 8192).unwrap();

    // Redis redirects or refuses before the function runs.
    assert_eq!(
        error_text(call(&mut ca, &["incr_twice", "1", &on_b])),
        format!("MOVED {slot_b} 127.0.0.1:6624")
    );
    assert!(error_text(call(&mut ca, &["hello", "2", &on_a, &on_a_too])).starts_with("CROSSSLOT"));

    // A key a serves, or no key at all, runs on a.
    assert_eq!(
        call(&mut ca, &["incr_twice", "1", &on_a]).unwrap(),
        Value::Bulk(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        call(&mut ca, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );

    // A function with no-cluster does not run on a cluster node.
    assert_eq!(
        error_text(call(&mut ca, &["local_only", "0"])),
        "ERR Can not run function on cluster, 'no-cluster' flag is set."
    );

    // Keys a function did not declare must hash to one slot, the declared
    // keys' if any, unless it has allow-cross-slot-keys. The refused GET is an
    // error inside the reply, so read it raw.
    let get_args = |function: &str, numkeys: &str, keys: &[&str]| {
        let mut args = vec!["WASM.CALL", function, numkeys];
        args.extend_from_slice(keys);
        raw_reply(6623, &args)
    };
    let refused = "-ERR Function attempted to access keys that do not hash to the same slot\r\n";
    // on_a holds 2 from incr_twice above; on_a_too is unset.
    assert_eq!(
        get_args("get_args", "0", &[&on_a_too, &on_a]),
        format!("*2\r\n$-1\r\n{refused}")
    );
    assert_eq!(
        get_args("get_args", "1", &[&on_a, &on_a_too]),
        format!("*1\r\n{refused}")
    );
    assert_eq!(
        get_args("get_args_cross", "0", &[&on_a_too, &on_a]),
        "*2\r\n$-1\r\n$1\r\n2\r\n"
    );
}

/// `INFO wasm` as field names, without the `wasm_` prefix, and values.
fn info(con: &mut Connection) -> std::collections::BTreeMap<String, i64> {
    let text: String = redis::cmd("INFO").arg("wasm").query(con).unwrap();
    text.lines()
        .filter_map(|line| line.strip_prefix("wasm_")?.split_once(':'))
        .map(|(field, value)| (field.to_owned(), value.trim().parse().unwrap()))
        .collect()
}

fn grow(con: &mut Connection, bytes: usize) -> RedisResult<Value> {
    call(con, &["grow", "0", &bytes.to_string()])
}

#[test]
fn guest_memory_is_limited_and_reported() {
    let server = Server::start(6625, &[]);
    let mut con = server.connection();
    let fresh = info(&mut con);
    assert_eq!(fresh["libraries"], 0);
    assert_eq!(fresh["instances"], 0);
    assert_eq!(fresh["instance_memory_bytes"], 0);
    assert_eq!(fresh["max_memory"], 64 << 20);

    // Loading keeps the instance that listed the functions.
    load(&mut con, "hello");
    let loaded = info(&mut con);
    assert_eq!(loaded["libraries"], 1);
    assert_eq!(loaded["functions"], FUNCTIONS.len() as i64);
    assert_eq!(loaded["component_bytes"], hello_component().len() as i64);
    assert!(loaded["code_bytes"] > 0);
    assert_eq!(loaded["instances"], 1);
    let Value::Int(memory) = grow(&mut con, 0).unwrap() else {
        panic!("grow replies with an integer");
    };
    assert_eq!(info(&mut con)["instance_memory_bytes"], memory);

    // Under the limit a guest grows; past it, the call fails and the instance
    // is dropped.
    let _: () = redis::cmd("CONFIG")
        .arg(&["SET", "wasm.max-memory", "4mb"])
        .query(&mut con)
        .unwrap();
    assert!(matches!(grow(&mut con, 1 << 20).unwrap(), Value::Int(n) if n > memory));
    assert_eq!(counter(&mut con), 1);
    assert_eq!(counter(&mut con), 2);
    let failed = error_text(grow(&mut con, 8 << 20));
    assert!(
        failed.starts_with("ERR function 'grow' failed:")
            && failed.contains("more than wasm.max-memory (4194304)"),
        "{failed}"
    );
    assert_eq!(counter(&mut con), 1);
    assert_eq!(info(&mut con)["instances"], 1);

    // Deleting the library frees its instance and its memory.
    let _: () = redis::cmd("WASM.DELETE")
        .arg("hello")
        .query(&mut con)
        .unwrap();
    let deleted = info(&mut con);
    assert_eq!(deleted["instances"], 0);
    assert_eq!(deleted["instance_memory_bytes"], 0);
    assert_eq!(deleted["code_bytes"], 0);
}

fn config_set(con: &mut Connection, name: &str, value: &str) {
    let _: () = redis::cmd("CONFIG")
        .arg(&["SET", name, value])
        .query(con)
        .unwrap();
}

fn call_ro(con: &mut Connection, args: &[&str]) -> RedisResult<Value> {
    redis::cmd("WASM.CALL_RO").arg(args).query(con)
}

#[test]
fn function_flags_work_as_in_lua() {
    let server = Server::start(6626, &["--enable-debug-command", "yes"]);
    let mut con = server.connection();
    load(&mut con, "hello");
    assert_eq!(flags_of(&mut con, "hello"), ["no-writes"]);
    assert!(flags_of(&mut con, "set").is_empty());
    assert_eq!(flags_of(&mut con, "stale_ok"), ["no-writes", "allow-stale"]);

    // WASM.CALL_RO runs only functions that do not write.
    assert_eq!(
        call_ro(&mut con, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );
    assert_eq!(
        error_text(call_ro(&mut con, &["set", "1", "k", "v"])),
        "ERR Can not execute a function with write flag using WASM.CALL_RO."
    );
    // A function with no-writes cannot write, nor run a may-replicate
    // command such as PUBLISH, from either command.
    for command in [["SET", "k", "v"], ["PUBLISH", "channel", "message"]] {
        let mut args = vec!["run_ro", "0"];
        args.extend(command);
        assert_eq!(
            error_text(call(&mut con, &args)),
            "ERR Write commands are not allowed from read-only functions."
        );
    }
    assert_eq!(
        call_ro(&mut con, &["run_ro", "0", "GET", "k"]).unwrap(),
        Value::Nil
    );

    // Over maxmemory: a function that may write does not start unless it has
    // allow-oom, and one that does not write runs.
    config_set(&mut con, "maxmemory", "1");
    assert_eq!(
        error_text(call(&mut con, &["set", "1", "k", "v"])),
        "OOM allow-oom flag is not set on the function, can not run it when used memory > 'maxmemory'"
    );
    assert_eq!(
        call(&mut con, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );
    assert_eq!(
        call(&mut con, &["set_oom", "1", "k", "v"]).unwrap(),
        Value::Okay
    );
    config_set(&mut con, "maxmemory", "0");

    // The RDB keeps the flags.
    let _: () = redis::cmd("DEBUG").arg("RELOAD").query(&mut con).unwrap();
    assert_eq!(flags_of(&mut con, "stale_ok"), ["no-writes", "allow-stale"]);
    assert!(error_text(call_ro(&mut con, &["set", "1", "k", "v"])).contains("write flag"));
}

#[test]
fn a_replica_runs_functions_by_their_flags() {
    let primary = Server::start(6627, &[]);
    let mut p = primary.connection();
    load(&mut p, "hello");
    let replica = Server::start(6628, &["--replicaof", "127.0.0.1", "6627"]);
    let mut r = replica.connection();
    wait_for("the replica to sync", || {
        let info: String = redis::cmd("INFO").arg("replication").query(&mut r).ok()?;
        info.contains("master_link_status:up").then_some(())
    });

    // A read-only replica runs a function that does not write, from WASM.CALL
    // too, and refuses one that may.
    assert_eq!(
        call(&mut r, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );
    assert_eq!(
        error_text(call(&mut r, &["set", "1", "k", "v"])),
        "READONLY Can not run function with write flag on readonly replica"
    );

    // With the primary gone and stale data not to be served, only a function
    // with allow-stale runs.
    primary.shutdown();
    config_set(&mut r, "replica-serve-stale-data", "no");
    assert_eq!(
        error_text(call(&mut r, &["hello", "0"])),
        "MASTERDOWN Link with MASTER is down, replica-serve-stale-data is set to 'no' \
         and 'allow-stale' flag is not set on the function."
    );
    assert_eq!(
        call(&mut r, &["stale_ok", "0"]).unwrap(),
        Value::Status("ok".into())
    );
}

#[test]
fn call_pcall_acl_check_and_set_resp_work_as_in_lua() {
    let server = Server::start(6629, &[]);
    let mut con = server.connection();
    load(&mut con, "hello");
    let _: () = redis::cmd("SET").arg(&["s", "x"]).query(&mut con).unwrap();

    // call fails the function with the error reply, naming the function as
    // Lua names the script; the rest of the function does not run.
    assert_eq!(
        call(&mut con, &["run", "0", "SET", "k", "v"]).unwrap(),
        Value::Status("ran".into())
    );
    assert_eq!(
        error_text(call(&mut con, &["run", "0", "INCR", "s"])),
        "ERR value is not an integer or out of range function: run."
    );
    // pcall hands the error to the function, which here replies with it.
    assert_eq!(
        error_text(call(&mut con, &["hash_pairs", "1", "s"])),
        "WRONGTYPE Operation against a key holding the wrong kind of value"
    );
    // A function that pcall handed an error succeeded, so its instance stays;
    // one that call failed did not, so its instance goes.
    assert_eq!([counter(&mut con), counter(&mut con)], [1, 2]);
    let _ = call(&mut con, &["hash_pairs", "1", "s"]);
    assert_eq!(counter(&mut con), 3);
    let _ = call(&mut con, &["run", "0", "INCR", "s"]);
    assert_eq!(counter(&mut con), 1);

    // acl_check answers for the user running the function.
    let acl_check = |con: &mut Connection, command: &[&str]| {
        let mut args = vec!["acl_check", "0"];
        args.extend_from_slice(command);
        call(con, &args)
    };
    assert_eq!(
        acl_check(&mut con, &["SET", "b1", "v"]).unwrap(),
        Value::Int(1)
    );
    let _: () = redis::cmd("ACL")
        .arg(&["SETUSER", "limited", "on", "nopass", "~a*", "+@all", "-set"])
        .query(&mut con)
        .unwrap();
    let mut limited = server.connection();
    let _: () = redis::cmd("AUTH")
        .arg(&["limited", "any"])
        .query(&mut limited)
        .unwrap();
    assert_eq!(
        acl_check(&mut limited, &["GET", "a1"]).unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        acl_check(&mut limited, &["GET", "b1"]).unwrap(),
        Value::Int(0)
    );
    assert_eq!(
        acl_check(&mut limited, &["SET", "a1", "v"]).unwrap(),
        Value::Int(0)
    );
    assert_eq!(
        error_text(acl_check(&mut limited, &["NOSUCH"])),
        "ERR unknown command"
    );

    // set_resp chooses the protocol of the replies the function gets.
    let _: () = redis::cmd("HSET")
        .arg(&["h", "a", "1"])
        .query(&mut con)
        .unwrap();
    let shape = |con: &mut Connection, resp: &str| call(con, &["resp_shape", "1", "h", resp]);
    assert_eq!(shape(&mut con, "3").unwrap(), Value::Status("map".into()));
    assert_eq!(shape(&mut con, "2").unwrap(), Value::Status("array".into()));
    assert_eq!(
        error_text(shape(&mut con, "4")),
        "ERR RESP version must be 2 or 3. function: resp_shape."
    );
}

#[test]
fn the_module_unloads_when_persistence_is_off() {
    let server = Server::start(
        6630,
        &["--enable-module-command", "yes", "--wasm.persist", "no"],
    );
    let mut con = server.connection();
    let persist: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "wasm.persist"])
        .query(&mut con)
        .unwrap();
    assert_eq!(persist, ["wasm.persist", "no"]);
    assert!(
        error_text(
            redis::cmd("CONFIG")
                .arg(&["SET", "wasm.persist", "yes"])
                .query(&mut con)
        )
        .contains("immutable")
    );
    load(&mut con, "hello");
    assert_eq!(counter(&mut con), 1);

    let module = module_path();
    let module = module.to_str().unwrap();
    let unload = |con: &mut Connection| {
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "wasm"])
            .query::<Value>(con)
    };
    assert_eq!(unload(&mut con).unwrap(), Value::Okay);
    assert!(error_text(redis::cmd("WASM.LIST").query(&mut con)).contains("unknown command"));

    // Loaded again, it starts with no libraries.
    let _: () = redis::cmd("MODULE")
        .arg(&["LOADEX", module, "CONFIG", "wasm.persist", "no"])
        .query(&mut con)
        .unwrap();
    assert!(list(&mut con).is_empty());
    load(&mut con, "hello");
    assert_eq!(counter(&mut con), 1);
    assert_eq!(unload(&mut con).unwrap(), Value::Okay);

    // Module arguments set the configuration too, with units.
    let _: () = redis::cmd("MODULE")
        .arg(&["LOAD", module, "persist", "no", "max-memory", "8mb"])
        .query(&mut con)
        .unwrap();
    let max_memory: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "wasm.max-memory"])
        .query(&mut con)
        .unwrap();
    assert_eq!(max_memory, ["wasm.max-memory", "8388608"]);
    assert_eq!(unload(&mut con).unwrap(), Value::Okay);

    // With persistence on, the default, the data type keeps it loaded.
    let _: () = redis::cmd("MODULE")
        .arg(&["LOAD", module])
        .query(&mut con)
        .unwrap();
    assert!(error_text(unload(&mut con)).contains("module-side data types"));
}

#[test]
fn an_out_of_bounds_access_traps_and_the_server_keeps_serving() {
    let server = Server::start(6631, &[]);
    let mut con = server.connection();
    load(&mut con, "hello");
    assert_eq!([counter(&mut con), counter(&mut con)], [1, 2]);
    let trapped = error_text(call(&mut con, &["oob", "0"]));
    assert!(
        trapped.starts_with("ERR function 'oob' failed:")
            && trapped.contains("out of bounds memory access"),
        "{trapped}"
    );
    // The trap dropped the instance, and the server is fine.
    assert_eq!(counter(&mut con), 1);
    let _: String = redis::cmd("PING").query(&mut con).unwrap();
}

#[test]
fn a_flat_reply_carries_the_same_reply() {
    let server = Server::start(6632, &[]);
    let mut con = server.connection();
    load(&mut con, "hello");
    let _: () = redis::cmd("RPUSH")
        .arg(&["list", "a", "", "ccc"])
        .query(&mut con)
        .unwrap();
    let _: () = redis::cmd("HSET")
        .arg(&["h", "f", "v"])
        .query(&mut con)
        .unwrap();
    let _: () = redis::cmd("SET").arg(&["s", "x"]).query(&mut con).unwrap();

    // run_flat rebuilds typed nodes from the flat reply; run_ro returns the
    // typed reply as pcall gives it.
    for command in [
        &["LRANGE", "list", "0", "-1"][..],
        &["HGETALL", "h"],
        &["PING"],
        &["GET", "missing"],
        &["INCR", "s"],
    ] {
        let mut flat = vec!["run_flat", "0"];
        flat.extend_from_slice(command);
        let mut typed = vec!["run_ro", "0"];
        typed.extend_from_slice(command);
        let mut flat_args = vec!["WASM.CALL"];
        flat_args.extend_from_slice(&flat);
        let mut typed_args = vec!["WASM.CALL"];
        typed_args.extend_from_slice(&typed);
        if command[0] == "INCR" {
            // run_ro may not write; compare with the error pcall gives.
            assert_eq!(
                raw_reply(6632, &flat_args),
                "-ERR value is not an integer or out of range\r\n"
            );
            continue;
        }
        assert_eq!(
            raw_reply(6632, &flat_args),
            raw_reply(6632, &typed_args),
            "{command:?}"
        );
    }
}

fn restore(con: &mut Connection, payload: &[u8], policy: Option<&str>) -> RedisResult<Value> {
    let mut cmd = redis::cmd("WASM.RESTORE");
    cmd.arg(payload);
    if let Some(policy) = policy {
        cmd.arg(policy);
    }
    cmd.query(con)
}

#[test]
fn dump_restore_and_flush_work_as_for_functions() {
    let source = Server::start(6633, &[]);
    let mut src = source.connection();
    load(&mut src, "hello");
    let payload: Vec<u8> = redis::cmd("WASM.DUMP").query(&mut src).unwrap();

    let target = Server::start(6634, &[]);
    let mut con = target.connection();
    assert_eq!(restore(&mut con, &payload, None).unwrap(), Value::Okay);
    assert_eq!(list(&mut con), listed(&["hello"]));
    assert_eq!(
        call(&mut con, &["hello", "0"]).unwrap(),
        Value::Status("hello, world".into())
    );

    // APPEND, the default, refuses a library that exists; REPLACE replaces
    // it, with a new instance.
    assert!(
        error_text(restore(&mut con, &payload, None)).contains("library 'hello' already exists")
    );
    assert_eq!([counter(&mut con), counter(&mut con)], [1, 2]);
    assert_eq!(
        restore(&mut con, &payload, Some("REPLACE")).unwrap(),
        Value::Okay
    );
    assert_eq!(counter(&mut con), 1);

    // A function in a library that stays fails the restore, which changes
    // nothing; FLUSH deletes that library first.
    let _: () = redis::cmd("WASM.FLUSH").query(&mut con).unwrap();
    load(&mut con, "mine");
    for policy in [None, Some("REPLACE")] {
        assert!(
            error_text(restore(&mut con, &payload, policy))
                .contains("function 'hello' already exists in library 'mine'")
        );
        assert_eq!(list(&mut con), listed(&["mine"]));
    }
    assert_eq!(
        restore(&mut con, &payload, Some("FLUSH")).unwrap(),
        Value::Okay
    );
    assert_eq!(list(&mut con), listed(&["hello"]));

    // A damaged payload or an unknown policy is refused.
    let mut damaged = payload.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 1;
    assert!(error_text(restore(&mut con, &damaged, None)).contains("checksum"));
    assert!(error_text(restore(&mut con, &payload, Some("MERGE"))).contains("policy"));

    // An empty server dumps a payload that restores nothing.
    let _: () = redis::cmd("WASM.FLUSH").query(&mut con).unwrap();
    assert!(list(&mut con).is_empty());
    let empty: Vec<u8> = redis::cmd("WASM.DUMP").query(&mut con).unwrap();
    assert_eq!(restore(&mut con, &empty, None).unwrap(), Value::Okay);
    assert!(list(&mut con).is_empty());
}

#[test]
fn a_restart_serves_while_saved_libraries_compile() {
    let Some(component) = js_component() else {
        return;
    };
    let server = Server::start(6635, &[]);
    let mut con = server.connection();
    let _: Vec<String> = redis::cmd("WASM.LOAD")
        .arg("js")
        .arg(component)
        .query(&mut con)
        .expect("WASM.LOAD of the JavaScript library");
    let _: () = redis::cmd("SAVE").query(&mut con).unwrap();
    server.shutdown();

    // The server serves as soon as it has read the RDB: compiling the 11 MB
    // component takes seconds, and happens on a thread.
    let started = Instant::now();
    let server = Server::restart(6635, &[]);
    let serving = started.elapsed();
    let mut con = server.connection();
    let wasm = info(&mut con);
    assert_eq!(wasm["compiling"], 1, "served after {serving:?}: {wasm:?}");
    assert_eq!(list(&mut con).len(), 1);

    // A call waits for the library; other clients are answered meanwhile.
    let mut caller = server.connection();
    let waiting = std::thread::spawn(move || call(&mut caller, &["hello", "0"]));
    let mut pings = 0;
    while !waiting.is_finished() {
        let _: String = redis::cmd("PING").query(&mut con).unwrap();
        pings += 1;
        sleep(Duration::from_millis(20));
    }
    assert_eq!(
        waiting.join().unwrap().unwrap(),
        Value::Status("hello, world".into())
    );
    assert!(
        pings > 1,
        "{pings} PINGs answered while the library compiled"
    );
    let compiled = info(&mut con);
    assert_eq!(compiled["compiling"], 0);
    assert!(compiled["code_bytes"] > 0);
}
