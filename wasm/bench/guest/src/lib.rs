//! The wasm side of the benchmark. `functions.lua` has the same functions in
//! Lua, written the same way, and `run.sh` checks that both give the same
//! results before timing them.

wit_bindgen::generate!({
    path: "../../wit",
    world: "library",
});

use redis_module_wasm::host::redis::call;

const FUNCTIONS: [&str; 5] = ["incr_twice", "nested", "adler32", "median", "primes"];

struct Bench;

impl Guest for Bench {
    fn functions() -> Vec<String> {
        FUNCTIONS.map(String::from).to_vec()
    }

    fn invoke(name: String, keys: Vec<Vec<u8>>, args: Vec<Vec<u8>>) -> Vec<Node> {
        let result = match name.as_str() {
            "incr_twice" => key(&keys).map(incr_twice),
            "nested" => Ok(nested()),
            "adler32" => key(&keys).map(adler32),
            "median" => key(&keys).and_then(median),
            "primes" => number(args.first()).map(primes),
            _ => Err(format!("ERR unknown function '{name}'")),
        };
        result.unwrap_or_else(|message| vec![Node::Error(message)])
    }
}

fn key(keys: &[Vec<u8>]) -> Result<&[u8], String> {
    keys.first()
        .map(Vec::as_slice)
        .ok_or_else(|| "ERR this function needs a key".to_owned())
}

fn number(arg: Option<&Vec<u8>>) -> Result<i64, String> {
    arg.and_then(|arg| std::str::from_utf8(arg).ok()?.parse().ok())
        .ok_or_else(|| "ERR expected an integer argument".to_owned())
}

/// Two INCRs of the key, as an array.
fn incr_twice(key: &[u8]) -> Vec<Node> {
    let mut reply = vec![Node::Array(2)];
    reply.extend(call("INCR", &[key.to_vec()]));
    reply.extend(call("INCR", &[key.to_vec()]));
    reply
}

/// An array holding a map, a nil and an error; no calls into Redis.
fn nested() -> Vec<Node> {
    vec![
        Node::Array(3),
        Node::Map(1),
        Node::Bulk(b"answer".to_vec()),
        Node::Integer(42),
        Node::Nil,
        Node::Error("ERR an error inside an array".into()),
    ]
}

/// The Adler-32 checksum of the key's value, one byte at a time, as the Lua
/// version computes it.
fn adler32(key: &[u8]) -> Vec<Node> {
    let value = match call("GET", &[key.to_vec()]).into_iter().next() {
        Some(Node::Bulk(bytes)) => bytes,
        _ => Vec::new(),
    };
    let (mut a, mut b) = (1u32, 0u32);
    for byte in value {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    vec![Node::Integer(i64::from(b) * 65536 + i64::from(a))]
}

/// The lower median of the integers in the list at the key.
fn median(key: &[u8]) -> Result<Vec<Node>, String> {
    let mut numbers = Vec::new();
    for node in call("LRANGE", &[key.to_vec(), b"0".to_vec(), b"-1".to_vec()]) {
        if let Node::Bulk(item) = node {
            numbers.push(number(Some(&item))?);
        }
    }
    numbers.sort_unstable();
    Ok(vec![match numbers.len() {
        0 => Node::Nil,
        len => Node::Integer(numbers[(len - 1) / 2]),
    }])
}

/// How many primes there are up to `n`, with a sieve, in loops like the Lua
/// version's.
fn primes(n: i64) -> Vec<Node> {
    let n = usize::try_from(n).unwrap_or(0);
    let mut composite = vec![false; n + 1];
    let mut count = 0;
    let mut i = 2;
    while i <= n {
        if !composite[i] {
            count += 1;
            // i * i overflows usize on wasm32 past 65535; there is nothing to
            // mark then anyway.
            if i <= n / i {
                let mut multiple = i * i;
                while multiple <= n {
                    composite[multiple] = true;
                    multiple += i;
                }
            }
        }
        i += 1;
    }
    vec![Node::Integer(count)]
}

export!(Bench);
