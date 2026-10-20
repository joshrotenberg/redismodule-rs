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

wit_bindgen::generate!({
    path: "../../wit",
    world: "library",
});

use redis_module_wasm::host::redis::{LogLevel, call, log};

/// The functions and their flags. A function that does not write says so, as
/// a Lua function would.
const FUNCTIONS: [(&str, FunctionFlags); 17] = [
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
        match name.as_str() {
            "hello" => {
                let who = args
                    .first()
                    .map(|name| String::from_utf8_lossy(name).into_owned())
                    .unwrap_or_else(|| "world".to_owned());
                log(LogLevel::Notice, &format!("hello called for {who}"));
                vec![Node::Simple(format!("hello, {who}"))]
            }
            "incr_twice" => match keys.first() {
                Some(key) => {
                    let mut reply = vec![Node::Array(2)];
                    reply.extend(call("INCR", std::slice::from_ref(key)));
                    reply.extend(call("INCR", std::slice::from_ref(key)));
                    reply
                }
                None => vec![Node::Error("ERR incr_twice needs a key".into())],
            },
            "hash_pairs" => match keys.first() {
                Some(key) => call("HGETALL", std::slice::from_ref(key)),
                None => vec![Node::Error("ERR hash_pairs needs a key".into())],
            },
            "set" => match (keys.first(), args.first()) {
                (Some(key), Some(value)) => call("SET", &[key.clone(), value.clone()]),
                _ => vec![Node::Error("ERR set needs a key and a value".into())],
            },
            "nested" => vec![
                Node::Array(3),
                Node::Map(1),
                Node::Simple("answer".into()),
                Node::Integer(42),
                Node::Nil,
                Node::Error("ERR an error inside an array".into()),
            ],
            "fail" => vec![Node::Error("ERR this function always fails".into())],
            "recurse" => call("WASM.LIST", &[]),
            "spin" => spin(),
            "counter" => vec![Node::Integer(
                COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
            )],
            "grow" => {
                let bytes = args
                    .first()
                    .and_then(|n| std::str::from_utf8(n).ok()?.parse().ok())
                    .unwrap_or(0);
                std::hint::black_box(vec![1u8; bytes]);
                vec![Node::Integer(
                    (core::arch::wasm32::memory_size(0) * 65536) as i64,
                )]
            }
            "run_ro" => match args.split_first() {
                Some((command, args)) => call(&String::from_utf8_lossy(command), args),
                None => vec![Node::Error("ERR run_ro needs a command".into())],
            },
            "set_oom" => match keys.first() {
                Some(key) => call(
                    "SET",
                    &[key.clone(), args.first().cloned().unwrap_or_default()],
                ),
                None => vec![Node::Error("ERR set_oom needs a key".into())],
            },
            "stale_ok" | "local_only" => vec![Node::Simple("ok".into())],
            "get_args" | "get_args_cross" => {
                let mut reply = vec![Node::Array(args.len() as u32)];
                for key in &args {
                    reply.extend(call("GET", std::slice::from_ref(key)));
                }
                reply
            }
            "write_then_spin" => match keys.first() {
                Some(key) => {
                    call("SET", &[key.clone(), b"1".to_vec()]);
                    spin()
                }
                None => vec![Node::Error("ERR write_then_spin needs a key".into())],
            },
            _ => vec![Node::Error(format!("ERR unknown function '{name}'"))],
        }
    }
}

/// Loops until the host stops the guest.
fn spin() -> Vec<Node> {
    loop {
        std::hint::black_box(());
    }
}

export!(Hello);
