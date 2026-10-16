//! Borrows text and bytes from RedisString owners and from raw pointers, and
//! copies keys out of a keyspace notification handler.
use std::sync::Mutex;

use redis_module::{
    redis_module, Context, NextArg, NotifyEvent, RedisError, RedisResult, RedisString, RedisValue,
    Status,
};

static LAST_KEY: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn text(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let owner = args.next_arg()?;
    args.done()?;
    let text = owner.try_as_str()?;
    // SAFETY: owner is live, retained and unchanged during this command. Redis
    // APIs are initialized and this read stays on the command's server thread.
    let raw_text = unsafe { RedisString::from_ptr(owner.as_ptr()) }?;
    Ok(RedisValue::Array(vec![
        RedisValue::StringBuffer(text.as_bytes().to_vec()),
        i64::from(text.as_ptr() == raw_text.as_ptr()).into(),
    ]))
}

fn bytes(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let owner = args.next_arg()?;
    args.done()?;
    let bytes = owner.as_slice();
    // SAFETY: the same live command-owned string/buffer guarantee as above;
    // no mutation or native alias invalidation occurs while either view is used.
    let raw_bytes = unsafe { RedisString::string_as_slice(owner.as_ptr()) };
    Ok(RedisValue::Array(vec![
        RedisValue::StringBuffer(bytes.to_vec()),
        i64::from(bytes.as_ptr() == raw_bytes.as_ptr()).into(),
    ]))
}

fn on_key(_: &Context, _: NotifyEvent, _: &str, key: &[u8]) {
    // Retain Rust-owned bytes, never the callback's borrowed native key/view.
    *LAST_KEY.lock().unwrap() = key.to_vec();
}

fn notify(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let owner = args.next_arg()?;
    args.done()?;
    if ctx.notify_keyspace_event(NotifyEvent::GENERIC, "string_views.normal", &owner) != Status::Ok
    {
        return Err(RedisError::Str("ERR notification failed"));
    }
    Ok(RedisValue::StringBuffer(LAST_KEY.lock().unwrap().clone()))
}

redis_module! {
    name: "string_views",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["views.text", text, "readonly", 0, 0, 0, ""],
        ["views.bytes", bytes, "readonly", 0, 0, 0, ""],
        ["views.notify", notify, "", 0, 0, 0, ""],
    ],
    event_handlers: [
        [@GENERIC: on_key],
    ],
}
