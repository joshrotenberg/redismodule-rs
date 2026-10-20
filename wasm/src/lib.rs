//! A Redis module that runs WebAssembly components as functions, the way
//! `FUNCTION LOAD` and `FCALL` run Lua.
//!
//! A library is a component that implements the `library` world in
//! `wit/redis.wit`: it lists its functions, and runs one by name with its keys
//! and arguments. It reaches Redis through the imports `call` and `log`.
//!
//! ```text
//! WASM.LOAD <library> <component> [REPLACE]
//! WASM.CALL <function> <numkeys> [key ...] [arg ...]
//! WASM.LIST
//! WASM.DELETE <library>
//! ```

use std::sync::{Mutex, MutexGuard, PoisonError};

use redis_module::{
    ArgCursor, Context, RedisError, RedisResult, RedisString, RedisValue, Status, redis_module,
};

mod host;
mod reply;

use host::Host;

/// Set when the module loads.
static HOST: Mutex<Option<Host>> = Mutex::new(None);

fn host() -> MutexGuard<'static, Option<Host>> {
    HOST.lock().unwrap_or_else(PoisonError::into_inner)
}

fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Result<R, RedisError> {
    host()
        .as_mut()
        .map(f)
        .ok_or(RedisError::Str("ERR the wasm host is not initialized"))
}

fn error(context: &str, error: &anyhow::Error) -> RedisError {
    RedisError::String(format!("ERR {context}: {error:#}"))
}

/// `WASM.LOAD <library> <component> [REPLACE]`: replies with the library's
/// functions.
fn wasm_load(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let name = args.next_str()?;
    let component = args.next_bytes()?;
    let replace = match args.next() {
        None => false,
        Some(arg) if arg.as_slice().eq_ignore_ascii_case(b"REPLACE") => true,
        Some(_) => return Err(RedisError::Str("ERR the only option is REPLACE")),
    };
    args.done()?;
    let functions = with_host(|host| host.load(ctx, name, component, replace))?
        .map_err(|e| error("WASM.LOAD failed", &e))?;
    Ok(RedisValue::Array(
        functions.into_iter().map(RedisValue::BulkString).collect(),
    ))
}

/// `WASM.CALL <function> <numkeys> [key ...] [arg ...]`
fn wasm_call(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let function = args.next_str()?;
    let numkeys = usize::try_from(args.next_u64()?)
        .ok()
        .filter(|&n| n <= args.remaining().len())
        .ok_or(RedisError::Str(
            "ERR numkeys is larger than the number of arguments",
        ))?;
    let keys: Vec<Vec<u8>> = args.by_ref().take(numkeys).map(|k| k.to_vec()).collect();
    let rest: Vec<Vec<u8>> = args.map(|a| a.to_vec()).collect();

    // Release the host before running the guest, so a guest that calls a
    // WASM.* command fails on its command flags, not on a lock.
    let call = with_host(|host| host.prepare(function))?
        .ok_or_else(|| RedisError::String(format!("ERR function '{function}' not found")))?;
    let nodes = call
        .run(ctx, function, &keys, &rest)
        .map_err(|e| error(&format!("function '{function}' failed"), &e))?;
    reply::to_value(&nodes).map_err(|reason| {
        RedisError::String(format!(
            "ERR function '{function}' returned a malformed reply: {reason}"
        ))
    })
}

/// `WASM.LIST`: an array of `[library, [function ...]]` pairs.
fn wasm_list(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ArgCursor::new(&args[1..]).done()?;
    with_host(|host| {
        RedisValue::Array(
            host.list()
                .map(|(name, functions)| {
                    RedisValue::Array(vec![
                        RedisValue::BulkString(name.to_owned()),
                        RedisValue::Array(
                            functions
                                .iter()
                                .cloned()
                                .map(RedisValue::BulkString)
                                .collect(),
                        ),
                    ])
                })
                .collect(),
        )
    })
}

/// `WASM.DELETE <library>`
fn wasm_delete(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let name = args.next_str()?;
    args.done()?;
    if with_host(|host| host.remove(name))? {
        Ok(RedisValue::SimpleStringStatic("OK"))
    } else {
        Err(RedisError::String(format!(
            "ERR library '{name}' not found"
        )))
    }
}

fn init(ctx: &Context, _args: &[RedisString]) -> Status {
    match Host::new() {
        Ok(new_host) => {
            *host() = Some(new_host);
            Status::Ok
        }
        Err(e) => {
            ctx.log_warning(&format!("wasm host: {e:#}"));
            Status::Err
        }
    }
}

redis_module! {
    name: "wasm",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        ["wasm.load", wasm_load, "write deny-oom deny-script", 0, 0, 0, ""],
        ["wasm.call", wasm_call, "write deny-script", 0, 0, 0, ""],
        ["wasm.list", wasm_list, "readonly deny-script", 0, 0, 0, ""],
        ["wasm.delete", wasm_delete, "write deny-script", 0, 0, 0, ""],
    ],
}
