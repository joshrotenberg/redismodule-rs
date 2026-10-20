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
//! ```

wit_bindgen::generate!({
    path: "../../wit",
    world: "library",
});

use redis_module_wasm::host::redis::{LogLevel, call, log};

const FUNCTIONS: [&str; 7] = [
    "hello",
    "incr_twice",
    "hash_pairs",
    "set",
    "nested",
    "fail",
    "recurse",
];

struct Hello;

impl Guest for Hello {
    fn functions() -> Vec<String> {
        FUNCTIONS.map(String::from).to_vec()
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
            _ => vec![Node::Error(format!("ERR unknown function '{name}'"))],
        }
    }
}

export!(Hello);
