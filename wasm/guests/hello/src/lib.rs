//! An example library for the wasm host. Load it, then call its functions:
//!
//! ```text
//! WASM.CALL hello 0 [name]       a greeting, as a simple string
//! WASM.CALL incr_twice 1 key     INCR the key twice and reply with both results
//! WASM.CALL hash_pairs 1 key     HGETALL the key, passing Redis's typed reply through
//! WASM.CALL set 1 key value      SET the key, passing the +OK status through
//! WASM.CALL nested 0             an array holding a map, a nil and an error
//! WASM.CALL fail 0               an error reply
//! WASM.CALL recurse 0            calls WASM.LIST, which a guest may not run
//! WASM.CALL spin 0               loops until WASM.KILL stops it
//! WASM.CALL write_then_spin 1 key  SETs the key, then loops; WASM.KILL refuses
//! WASM.CALL counter 0            counts the calls this instance has served
//! WASM.CALL grow 0 bytes         allocates that many bytes, then replies with
//!                                the size of the instance's memory
//! ```
//!
//! These show what each function flag does:
//!
//! ```text
//! WASM.CALL run_ro 0 cmd arg ... runs its arguments as a command, but has
//!                                no-writes: a write command fails
//! WASM.CALL set_oom 1 key value  SETs the key; allow-oom lets it run over maxmemory
//! WASM.CALL stale_ok 0           replies ok; allow-stale lets it run on a stale replica
//! WASM.CALL local_only 0         replies ok; no-cluster keeps it off cluster nodes
//! WASM.CALL get_args 0 k ...     GETs each argument as a key: one slot on a cluster node
//! WASM.CALL get_args_cross 0 k ...  the same with allow-cross-slot-keys
//! ```
//!
//! The functions above call Redis with `pcall`, so an error reply comes back
//! to them. These use the other imports:
//!
//! ```text
//! WASM.CALL run 0 cmd arg ...    runs its arguments as a command with `call`,
//!                                which fails the function on an error reply,
//!                                then replies ran
//! WASM.CALL acl_check 0 cmd arg ...  whether the caller may run the command
//! WASM.CALL resp_shape 1 key resp    HGETALL with RESP 2 or 3: replies map or array
//! WASM.CALL oob 0                reads past the end of its memory, which traps
//! WASM.CALL run_flat 0 cmd arg ...  runs a command with `pcall-flat` and
//!                                replies with what it got
//! ```
//!
//! It builds its replies as `redis_wasm_guest::Value`s (`../../guest-sdk`),
//! not as node lists.

wit_bindgen::generate!({
    path: "../../wit",
    world: "library",
    // The host's interface comes from the helper library, so its Value works
    // with these bindings' types.
    with: { "redis-module-wasm:host/redis@0.1.0": redis_wasm_guest::redis },
});

use redis_wasm_guest::redis::{acl_check_command, pcall_flat, set_resp};
use redis_wasm_guest::{LogLevel, Value, call, log, pcall};

/// The functions and their flags. A function that does not write says so, as
/// a Lua function would.
const FUNCTIONS: [(&str, FunctionFlags); 22] = [
    ("hello", FunctionFlags::NO_WRITES),
    ("incr_twice", FunctionFlags::empty()),
    ("hash_pairs", FunctionFlags::NO_WRITES),
    ("set", FunctionFlags::empty()),
    ("nested", FunctionFlags::NO_WRITES),
    ("fail", FunctionFlags::NO_WRITES),
    ("recurse", FunctionFlags::NO_WRITES),
    ("spin", FunctionFlags::NO_WRITES),
    ("write_then_spin", FunctionFlags::empty()),
    ("counter", FunctionFlags::NO_WRITES),
    ("grow", FunctionFlags::NO_WRITES),
    ("run_ro", FunctionFlags::NO_WRITES),
    ("set_oom", FunctionFlags::ALLOW_OOM),
    (
        "stale_ok",
        FunctionFlags::NO_WRITES.union(FunctionFlags::ALLOW_STALE),
    ),
    (
        "local_only",
        FunctionFlags::NO_WRITES.union(FunctionFlags::NO_CLUSTER),
    ),
    ("get_args", FunctionFlags::NO_WRITES),
    (
        "get_args_cross",
        FunctionFlags::NO_WRITES.union(FunctionFlags::ALLOW_CROSS_SLOT_KEYS),
    ),
    ("run", FunctionFlags::empty()),
    ("acl_check", FunctionFlags::NO_WRITES),
    ("resp_shape", FunctionFlags::NO_WRITES),
    ("oob", FunctionFlags::NO_WRITES),
    ("run_flat", FunctionFlags::empty()),
];

/// Calls of `counter` that this instance has served.
static COUNTER: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

struct Hello;

impl Guest for Hello {
    fn functions() -> Vec<Function> {
        FUNCTIONS
            .iter()
            .map(|&(name, flags)| Function {
                name: name.into(),
                flags,
            })
            .collect()
    }

    fn invoke(name: String, keys: Vec<Vec<u8>>, args: Vec<Vec<u8>>) -> Vec<Node> {
        run(&name, &keys, &args).into_nodes()
    }
}

fn error(message: impl Into<String>) -> Value {
    Value::Error(message.into())
}

/// The first of `items`, or an error naming what `function` needs.
fn first<'a>(items: &'a [Vec<u8>], function: &str, what: &str) -> Result<&'a [u8], Value> {
    items
        .first()
        .map(Vec::as_slice)
        .ok_or_else(|| error(format!("ERR {function} needs {what}")))
}

/// Runs a command given as the first argument, with the rest as its
/// arguments.
fn command_of(args: &[Vec<u8>]) -> Option<(String, &[Vec<u8>])> {
    let (command, rest) = args.split_first()?;
    Some((String::from_utf8_lossy(command).into_owned(), rest))
}

fn run(name: &str, keys: &[Vec<u8>], args: &[Vec<u8>]) -> Value {
    let result = match name {
        "hello" => {
            let who = args
                .first()
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .unwrap_or_else(|| "world".to_owned());
            log(LogLevel::Notice, &format!("hello called for {who}"));
            Ok(Value::Simple(format!("hello, {who}")))
        }
        "incr_twice" => first(keys, name, "a key")
            .map(|key| Value::Array(vec![pcall("INCR", &[key]), pcall("INCR", &[key])])),
        "hash_pairs" => first(keys, name, "a key").map(|key| pcall("HGETALL", &[key])),
        "set" => first(keys, name, "a key").and_then(|key| {
            let value = first(args, name, "a value")?;
            Ok(pcall("SET", &[key, value]))
        }),
        "nested" => Ok(Value::Array(vec![
            Value::Map(vec![(Value::Simple("answer".into()), Value::Integer(42))]),
            Value::Nil,
            error("ERR an error inside an array"),
        ])),
        "fail" => Ok(error("ERR this function always fails")),
        "recurse" => Ok(pcall("WASM.LIST", &[] as &[&[u8]])),
        "spin" => spin(),
        "counter" => Ok(Value::Integer(
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
        )),
        "grow" => {
            let bytes = args
                .first()
                .and_then(|n| std::str::from_utf8(n).ok()?.parse().ok())
                .unwrap_or(0);
            std::hint::black_box(vec![1u8; bytes]);
            Ok(Value::Integer(
                (core::arch::wasm32::memory_size(0) * 65536) as i64,
            ))
        }
        "run_ro" => command_of(args)
            .map(|(command, args)| pcall(&command, args))
            .ok_or_else(|| error("ERR run_ro needs a command")),
        "set_oom" => first(keys, name, "a key").map(|key| {
            let value = args.first().map(Vec::as_slice).unwrap_or_default();
            pcall("SET", &[key, value])
        }),
        "stale_ok" | "local_only" => Ok(Value::Simple("ok".into())),
        "get_args" | "get_args_cross" => Ok(Value::Array(
            args.iter().map(|key| pcall("GET", &[key])).collect(),
        )),
        "run" => command_of(args)
            .map(|(command, args)| {
                call(&command, args);
                Value::Simple("ran".into())
            })
            .ok_or_else(|| error("ERR run needs a command")),
        "acl_check" => command_of(args)
            .map(|(command, args)| match acl_check_command(&command, args) {
                Ok(allowed) => Value::Boolean(allowed),
                Err(message) => error(message),
            })
            .ok_or_else(|| error("ERR acl_check needs a command")),
        "resp_shape" => {
            let version = args
                .first()
                .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
                .unwrap_or(0);
            set_resp(version);
            let shape = match pcall("HGETALL", keys) {
                Value::Map(_) => "map",
                Value::Array(_) => "array",
                _ => "other",
            };
            Ok(Value::Simple(shape.into()))
        }
        "oob" => {
            // The last byte of the 4 GiB address space, far past the end of
            // the instance's memory.
            let byte = unsafe { core::ptr::read_volatile(usize::MAX as *const u8) };
            Ok(Value::Integer(i64::from(byte)))
        }
        "run_flat" => command_of(args)
            .map(|(command, args)| {
                let reply = pcall_flat(&command, args);
                Value::from_flat(&reply).unwrap_or_else(|| error("ERR a malformed flat reply"))
            })
            .ok_or_else(|| error("ERR run_flat needs a command")),
        "write_then_spin" => first(keys, name, "a key").and_then(|key| {
            pcall("SET", &[key, b"1"]);
            spin()
        }),
        _ => Err(error(format!("ERR unknown function '{name}'"))),
    };
    result.unwrap_or_else(|error| error)
}

/// Loops until the host stops the guest.
fn spin() -> Result<Value, Value> {
    loop {
        std::hint::black_box(());
    }
}

export!(Hello);
