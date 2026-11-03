use redis_module::{
    Context, NextArg, RedisError, RedisResult, RedisString, RedisValue, Status, ThreadSafeContext,
    redis_module,
};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

fn block(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    let blocked_client = ctx.block_client()?;

    thread::spawn(move || {
        let thread_ctx = ThreadSafeContext::with_blocked_client(blocked_client);
        thread::sleep(Duration::from_millis(1000));
        thread_ctx.reply(Ok("42".into()));
    });

    // We will reply later, from the thread
    Ok(RedisValue::NoReply)
}

static LIVE_VALUES: AtomicI64 = AtomicI64::new(0);
static DIRECT_BLOCKS: AtomicI64 = AtomicI64::new(0);

/// An immediate blocked-client reply with no detached worker to await.
fn direct(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let blocked = ctx.block_client()?;
    DIRECT_BLOCKS.fetch_add(1, Ordering::SeqCst);
    let reply = ThreadSafeContext::with_blocked_client(blocked);
    reply.reply(Ok("direct reply".into()));
    Ok(RedisValue::NoReply)
}

fn direct_blocks(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(DIRECT_BLOCKS.load(Ordering::SeqCst).into())
}

fn call_direct(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.call("block.direct", &[] as &[&str])
}

fn call_reply(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.call("block.reply", &["0", "ignored"])
}

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

/// Both APIs must reject reblocking before touching the native client state.
fn reblock_errors(ctx: &Context) -> RedisResult {
    let direct = ctx
        .block_client()
        .err()
        .ok_or(RedisError::Str("ERR reblocking succeeded"))?;
    let callback = ctx
        .block_client_on_reply(None, reblock_with_value, reblock_timed_out)
        .err()
        .ok_or(RedisError::Str("ERR callback reblocking succeeded"))?;
    Ok(vec![direct.to_string(), callback.to_string()].into())
}

fn reblock_with_value(ctx: &Context, _: Tracked) -> RedisResult {
    reblock_errors(ctx)
}

fn reblock_timed_out(ctx: &Context) -> RedisResult {
    reblock_errors(ctx)
}

fn reblock_reply(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let blocked = ctx.block_client_on_reply(None, reblock_with_value, reblock_timed_out)?;
    blocked.unblock(Tracked::new("reply".to_owned()));
    Ok(RedisValue::NoReply)
}

fn reblock_timeout(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let blocked = ctx.block_client_on_reply(
        Some(Duration::from_millis(1)),
        reblock_with_value,
        reblock_timed_out,
    )?;
    let value = Tracked::new("late".to_owned());
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        blocked.unblock(value);
    });
    Ok(RedisValue::NoReply)
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

/// `block.release_unreplied`: a thread drops the handle without replying, so
/// the client gets the released-without-a-reply error.
fn release_unreplied(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let blocked = ctx.block_client()?;
    thread::spawn(move || drop(blocked));
    Ok(RedisValue::NoReply)
}

/// `block.worker_panics`: a thread panics before replying; unwinding drops its
/// context, and the client gets the released-without-a-reply error.
fn worker_panics(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let blocked = ctx.block_client()?;
    thread::spawn(move || {
        let _reply = ThreadSafeContext::with_blocked_client(blocked);
        panic!("worker failed before replying");
    });
    Ok(RedisValue::NoReply)
}

/// `block.fail_after_block`: the command fails after blocking, before handing
/// the handle to a worker; its error is the only reply.
fn fail_after_block(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let _blocked = ctx.block_client()?;
    Err(RedisError::Str("ERR failed after blocking"))
}

static SECOND_REPLY_REFUSED: AtomicBool = AtomicBool::new(false);

/// `block.reply_twice`: a thread replies twice; the second reply is refused.
fn reply_twice(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let blocked = ctx.block_client()?;
    thread::spawn(move || {
        let reply = ThreadSafeContext::with_blocked_client(blocked);
        reply.reply(Ok("first".into()));
        let second = reply.reply(Ok("second".into()));
        SECOND_REPLY_REFUSED.store(second == Status::Err, Ordering::SeqCst);
    });
    Ok(RedisValue::NoReply)
}

/// `block.second_reply_refused`: whether the last `block.reply_twice` worker's
/// second reply was refused.
fn second_reply_refused(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Bool(
        SECOND_REPLY_REFUSED.load(Ordering::SeqCst),
    ))
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
        ["block.direct", direct, "", 0, 0, 0, ""],
        ["block.direct_blocks", direct_blocks, "readonly", 0, 0, 0, ""],
        ["block.call_direct", call_direct, "", 0, 0, 0, ""],
        ["block.call_reply", call_reply, "", 0, 0, 0, ""],
        ["block.reblock_reply", reblock_reply, "", 0, 0, 0, ""],
        ["block.reblock_timeout", reblock_timeout, "", 0, 0, 0, ""],
        ["block.reply", block_reply, "", 0, 0, 0, ""],
        ["block.timeout", block_timeout, "", 0, 0, 0, ""],
        ["block.dropped", block_dropped, "", 0, 0, 0, ""],
        ["block.live_values", block_live_values, "", 0, 0, 0, ""],
        ["block.release_unreplied", release_unreplied, "", 0, 0, 0, ""],
        ["block.worker_panics", worker_panics, "", 0, 0, 0, ""],
        ["block.fail_after_block", fail_after_block, "", 0, 0, 0, ""],
        ["block.reply_twice", reply_twice, "", 0, 0, 0, ""],
        ["block.second_reply_refused", second_reply_refused, "readonly", 0, 0, 0, ""],
    ],
}
