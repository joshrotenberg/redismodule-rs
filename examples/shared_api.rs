//! Exports a function for other modules to call: `shared_api.add` adds two
//! integers. `examples/shared_api_user.rs` looks it up and calls it.
//!
//! A shared API is an untyped pointer, so its signature is a protocol between
//! the modules: here `extern "C" fn(i64, i64) -> i64`. Redis refuses to unload
//! a module while another module uses an API it exports.
use std::ffi::{CStr, c_void};

use redis_module::{Context, RedisResult, RedisString, RegistrationContext, Status, redis_module};

/// The name other modules look the function up by.
const NAME: &CStr = c"shared_api.add";

/// The exported function.
extern "C" fn add(a: i64, b: i64) -> i64 {
    a.wrapping_add(b)
}

fn export(ctx: &redis_module::EventContext) -> Status {
    let func = add as extern "C" fn(i64, i64) -> i64 as *const c_void;
    // SAFETY: ctx is the context Redis passed this callback, on the main
    // thread; NAME is static, as Redis keeps the pointer; and `add` stays
    // loaded while another module can call it, since Redis refuses to unload
    // this module while one uses the API.
    unsafe { ctx.export_shared_api(func, NAME.as_ptr()) }
}

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    export(ctx)
}

/// `shared_api.export_again`: 1 if exporting the name again succeeds, 0 if
/// Redis refuses it because the name is taken.
fn export_again(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(i64::from(export(ctx) == Status::Ok).into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "shared_api",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        ["shared_api.export_again", export_again, "", 0, 0, 0, ""],
    ],
}
