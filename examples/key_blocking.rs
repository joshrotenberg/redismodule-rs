//! Key-based blocking with owned waiters and guarded callbacks.
//!
//! `keyblock.wait timeout_ms minimum mode key [key ...]` waits for a list to
//! contain `minimum` entries. Zero timeout means no deadline. `mode` is normally
//! `normal`; the other modes exercise callback and destructor refusal paths.

use redis_module::{
    Context, KeyBlockingResult, KeyBlockingTimeout, NextArg, RedisError, RedisResult, RedisString,
    RedisValue, redis_module,
};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

static LIVE: AtomicI64 = AtomicI64::new(0);
static DROPS: AtomicI64 = AtomicI64::new(0);
static READY: AtomicI64 = AtomicI64::new(0);
static TIMEOUTS: AtomicI64 = AtomicI64::new(0);
static DELETE_FLAG: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    PanicReady,
    PanicTimeout,
    PanicDrop,
    NoReplyReady,
    NoReplyTimeout,
}

struct Waiter {
    keys: Vec<Vec<u8>>,
    minimum: usize,
    mode: Mode,
}

struct BadPanicPayload;
impl Drop for BadPanicPayload {
    fn drop(&mut self) {
        panic!("the caught panic payload must not be dropped");
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
        DROPS.fetch_add(1, Ordering::SeqCst);
        if matches!(self.mode, Mode::PanicDrop) {
            std::panic::panic_any(BadPanicPayload);
        }
    }
}

fn read_ready(ctx: &Context, waiter: &Waiter) -> Result<Option<RedisValue>, RedisError> {
    for key in &waiter.keys {
        let len = match ctx.call("LLEN", &[key.as_slice()])? {
            RedisValue::Integer(len) => len,
            _ => return Err(RedisError::Str("ERR unexpected LLEN reply")),
        };
        if usize::try_from(len).is_ok_and(|len| len >= waiter.minimum) {
            let args: [&[u8]; 3] = [key.as_slice(), b"0", b"-1"];
            return Ok(Some(RedisValue::Array(vec![
                RedisValue::StringBuffer(key.clone()),
                ctx.call("LRANGE", &args)?,
            ])));
        }
    }
    Ok(None)
}

fn on_ready(ctx: &Context, waiter: &mut Waiter) -> Result<Option<RedisValue>, RedisError> {
    READY.fetch_add(1, Ordering::SeqCst);
    let ready_key = ctx
        .blocked_client_ready_key()?
        .ok_or(RedisError::Str("ERR ready callback has no ready key"))?;
    if !waiter.keys.iter().any(|key| key == ready_key.as_slice()) {
        return Err(RedisError::Str("ERR unexpected ready key"));
    }
    match waiter.mode {
        Mode::PanicReady => std::panic::panic_any(BadPanicPayload),
        Mode::NoReplyReady => return Ok(Some(RedisValue::NoReply)),
        _ => {}
    }
    if matches!(ctx.call("EXISTS", &[&ready_key])?, RedisValue::Integer(0)) {
        return Err(RedisError::Str("ERR the blocked key was deleted"));
    }
    read_ready(ctx, waiter)
}

fn on_timeout(_: &Context, waiter: &mut Waiter) -> RedisResult {
    TIMEOUTS.fetch_add(1, Ordering::SeqCst);
    match waiter.mode {
        Mode::PanicTimeout => std::panic::panic_any(BadPanicPayload),
        Mode::NoReplyTimeout => Ok(RedisValue::NoReply),
        _ => Ok(RedisValue::NullArray),
    }
}

fn wait(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let timeout = KeyBlockingTimeout::try_from(args.next_i64()?)?;
    let minimum = usize::try_from(args.next_u64()?)
        .ok()
        .filter(|minimum| *minimum > 0)
        .ok_or(RedisError::Str("ERR minimum list length must be positive"))?;
    let mode = match args.next_string()?.as_str() {
        "normal" => Mode::Normal,
        "panic-ready" => Mode::PanicReady,
        "panic-timeout" => Mode::PanicTimeout,
        "panic-drop" => Mode::PanicDrop,
        "no-reply-ready" => Mode::NoReplyReady,
        "no-reply-timeout" => Mode::NoReplyTimeout,
        _ => return Err(RedisError::Str("ERR unknown waiter mode")),
    };
    let keys: Vec<_> = args.collect();
    let waiter = Waiter {
        keys: keys.iter().map(|key| key.as_slice().to_vec()).collect(),
        minimum,
        mode,
    };
    LIVE.fetch_add(1, Ordering::SeqCst);
    if let Some(reply) = read_ready(ctx, &waiter)? {
        return Ok(reply);
    }
    let keys: Vec<_> = keys.iter().collect();
    match ctx.block_client_on_keys(&keys, timeout, true, waiter, on_ready, on_timeout)? {
        KeyBlockingResult::Blocked { unblock_on_delete } => {
            DELETE_FLAG.store(unblock_on_delete, Ordering::SeqCst);
            Ok(RedisValue::NoReply)
        }
        KeyBlockingResult::Denied(waiter) => {
            // Native blocking reads are non-blocking in MULTI and scripts.
            Ok(read_ready(ctx, &waiter)?.unwrap_or(RedisValue::NullArray))
        }
    }
}

fn signal(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    args.done()?;
    ctx.signal_key_as_ready(&key)?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

fn call_wait(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    args.done()?;
    let args: [&[u8]; 4] = [b"0", b"1", b"normal", key.as_slice()];
    ctx.call("keyblock.wait", &args)
}

fn ready_outside(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Bool(ctx.blocked_client_ready_key()?.is_some()))
}

fn stats(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        LIVE.load(Ordering::SeqCst),
        DROPS.load(Ordering::SeqCst),
        READY.load(Ordering::SeqCst),
        TIMEOUTS.load(Ordering::SeqCst),
        i64::from(DELETE_FLAG.load(Ordering::SeqCst)),
    ]
    .into())
}

redis_module! {
    name: "key_blocking",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["keyblock.wait", wait, "readonly", 4, -1, 1, ""],
        ["keyblock.signal", signal, "readonly", 1, 1, 1, ""],
        ["keyblock.call_wait", call_wait, "readonly", 1, 1, 1, ""],
        ["keyblock.ready_outside", ready_outside, "readonly", 0, 0, 0, ""],
        ["keyblock.stats", stats, "readonly", 0, 0, 0, ""],
    ],
}
