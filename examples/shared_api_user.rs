//! Calls `shared_api.add`, which `examples/shared_api.rs` exports:
//! `shared_api_user.add a b`. It looks the API up when a command needs it,
//! since the exporting module may load after this one.
//!
//! The SDK has no wrapper for `RedisModule_GetSharedAPI`: the pointer it
//! returns is untyped, and only the modules know its signature.
use std::ffi::c_void;

use redis_module::{Context, NextArg, RedisError, RedisResult, RedisString, raw, redis_module};

/// The signature `shared_api` exports `shared_api.add` with.
type Add = extern "C" fn(i64, i64) -> i64;

fn lookup(ctx: &Context) -> Result<Add, RedisError> {
    // SAFETY: Redis initialized the module API before running a command.
    let get = unsafe { raw::RedisModule_GetSharedAPI }.ok_or(RedisError::Str(
        "ERR RedisModule_GetSharedAPI is unavailable",
    ))?;
    // SAFETY: ctx is this command's context, on the main thread.
    let func = unsafe { get(ctx.get_raw(), c"shared_api.add".as_ptr()) };
    if func.is_null() {
        return Err(RedisError::Str("ERR shared_api.add is not exported"));
    }
    // SAFETY: shared_api exports shared_api.add as an Add, and the lookup
    // records this module as a user, so Redis keeps shared_api loaded.
    Ok(unsafe { std::mem::transmute::<*mut c_void, Add>(func) })
}

/// `shared_api_user.add a b`
fn add(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let a = args.next_i64()?;
    let b = args.next_i64()?;
    args.done()?;
    Ok(lookup(ctx)?(a, b).into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "shared_api_user",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["shared_api_user.add", add, "", 0, 0, 0, ""],
    ],
}
