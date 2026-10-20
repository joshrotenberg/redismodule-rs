use redis_module::{
    redis_module, Context, NextArg, RedisResult, RedisString, RedisValue, ThreadSafeContext,
};
use std::sync::atomic::{AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

fn block(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    let blocked_client = ctx.block_client();

    thread::spawn(move || {
        let thread_ctx = ThreadSafeContext::with_blocked_client(blocked_client);
        thread::sleep(Duration::from_millis(1000));
        thread_ctx.reply(Ok("42".into()));
    });

    // We will reply later, from the thread
    Ok(RedisValue::NoReply)
}

static LIVE_VALUES: AtomicI64 = AtomicI64::new(0);

/// A reply value that counts how many exist, so a test can see them freed.
struct Tracked(String);

impl Tracked {
    fn new(text: String) -> Self {
        LIVE_VALUES.fetch_add(1, Ordering::SeqCst);
        Self(text)
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        LIVE_VALUES.fetch_sub(1, Ordering::SeqCst);
    }
}

fn reply_with_value(_: &Context, value: Tracked) -> RedisResult {
    Ok(RedisValue::BulkString(value.0.clone()))
}

fn reply_timed_out(_: &Context) -> RedisResult {
    Ok(RedisValue::SimpleStringStatic("timed out"))
}

/// `block.reply ms value`: a thread hands `value` back after `ms` milliseconds,
/// and the main thread replies with it.
fn block_reply(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let millis = args.next_u64()?;
    let value = args.next_string()?;
    args.done()?;

    let blocked = ctx.block_client_on_reply(None, reply_with_value, reply_timed_out)?;
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(millis));
        blocked.unblock(Tracked::new(value));
    });
    Ok(RedisValue::NoReply)
}

/// `block.timeout timeout_ms work_ms`: blocks with a timeout, and a thread
/// unblocks after `work_ms` milliseconds.
fn block_timeout(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let timeout = Duration::from_millis(args.next_u64()?);
    let work = Duration::from_millis(args.next_u64()?);
    args.done()?;

    let blocked = ctx.block_client_on_reply(Some(timeout), reply_with_value, reply_timed_out)?;
    thread::spawn(move || {
        thread::sleep(work);
        blocked.unblock(Tracked::new("done".to_owned()));
    });
    Ok(RedisValue::NoReply)
}

/// `block.dropped`: a thread drops the handle without a value.
fn block_dropped(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    let blocked = ctx.block_client_on_reply(None, reply_with_value, reply_timed_out)?;
    thread::spawn(move || drop(blocked));
    Ok(RedisValue::NoReply)
}

/// `block.live_values`: how many reply values exist.
fn block_live_values(_: &Context, _args: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Integer(LIVE_VALUES.load(Ordering::SeqCst)))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "block",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["block", block, "", 0, 0, 0, ""],
        ["block.reply", block_reply, "", 0, 0, 0, ""],
        ["block.timeout", block_timeout, "", 0, 0, 0, ""],
        ["block.dropped", block_dropped, "", 0, 0, 0, ""],
        ["block.live_values", block_live_values, "", 0, 0, 0, ""],
    ],
}
