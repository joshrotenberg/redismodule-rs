use redis_module::{
    redis_module, Context, NextArg, NotifyEvent, RedisError, RedisResult, RedisString,
    RedisValue, Status,
};
use std::sync::Mutex;

static LAST_KEY: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn echo(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let value = args.next_arg()?;
    args.done()?;
    Ok(RedisValue::StringBuffer(value.as_slice().to_vec()))
}

fn text(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let owner = args.next_arg()?;
    args.done()?;
    let text = owner.try_as_str()?;
    // SAFETY: this live command-owned string remains unchanged for both views,
    // with initialized APIs and legal Redis server-thread access.
    let raw = unsafe { RedisString::from_ptr(owner.inner) }?;
    Ok(RedisValue::Array(vec![
        RedisValue::StringBuffer(text.as_bytes().to_vec()),
        i64::from(text.as_ptr() == raw.as_ptr()).into(),
    ]))
}

fn on_key(_: &Context, _: NotifyEvent, _: &str, key: &[u8]) {
    *LAST_KEY.lock().unwrap() = key.to_vec();
}

fn notify(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let owner = args.next_arg()?;
    args.done()?;
    if ctx.notify_keyspace_event(NotifyEvent::GENERIC, "archive.normal", &owner) != Status::Ok {
        return Err(RedisError::Str("ERR notification failed"));
    }
    Ok(RedisValue::StringBuffer(LAST_KEY.lock().unwrap().clone()))
}

redis_module! {
    name: "archive_manual",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["archive.echo", echo, "readonly", 0, 0, 0, ""],
        ["archive.view", text, "readonly", 0, 0, 0, ""],
        ["archive.notify", notify, "", 0, 0, 0, ""],
    ],
    event_handlers: [
        [@GENERIC: on_key],
    ],
}
