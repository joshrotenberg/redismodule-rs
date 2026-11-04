//! Key-based blocking with owned waiters and guarded callbacks.
//!
//! `keyblock.wait timeout_ms minimum mode key [key ...]` waits for a list to
//! contain `minimum` entries. Zero timeout means no deadline. `mode` is normally
//! `normal`; the other modes exercise callback and destructor refusal paths.

use redis_module::{
    CommandFlags, Context, EventContext, KeyBlockingResult, KeyBlockingTimeout, KeyRange, NextArg,
    RedisError, RedisResult, RedisString, RedisValue, RegistrationContext, Status, YieldMode, raw,
    redis_command, redis_module,
};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

static LIVE: AtomicI64 = AtomicI64::new(0);
static DROPS: AtomicI64 = AtomicI64::new(0);
static READY: AtomicI64 = AtomicI64::new(0);
static TIMEOUTS: AtomicI64 = AtomicI64::new(0);
static DELETE_FLAG: AtomicBool = AtomicBool::new(false);
static LOAD_TEST: AtomicBool = AtomicBool::new(false);
static LOAD_ATTEMPT: AtomicBool = AtomicBool::new(false);

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
    let outcome = ctx.block_client_on_keys(&keys, timeout, true, waiter, on_ready, on_timeout);
    if LOAD_TEST.load(Ordering::SeqCst) {
        LOAD_ATTEMPT.store(true, Ordering::SeqCst);
    }
    match outcome? {
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

// Use the native K call deliberately: the SDK's call_blocking correctly
// refuses OnLoad before invoking the command, which would miss this regression.
// On a broken implementation abort a promise and keep the module mapped.
enum KCall {
    Unsupported,
    Denied,
    Unexpected,
}

fn native_call_wait_k(ctx: &EventContext, key: &RedisString) -> KCall {
    unsafe {
        let (Some(call), Some(kind), Some(free), Some(abort)) = (
            raw::RedisModule_Call,
            raw::RedisModule_CallReplyType,
            raw::RedisModule_FreeCallReply,
            raw::RedisModule_CallReplyPromiseAbort,
        ) else {
            return KCall::Unsupported;
        };
        let args = [
            ctx.create_string("0"),
            ctx.create_string("1"),
            ctx.create_string("normal"),
        ];
        let mut argv = [
            args[0].as_ptr(),
            args[1].as_ptr(),
            args[2].as_ptr(),
            key.as_ptr(),
        ];
        let reply = call(
            ctx.get_raw(),
            c"keyblock.wait".as_ptr(),
            c"Kv".as_ptr(),
            argv.as_mut_ptr(),
            argv.len(),
        );
        if reply.is_null() {
            return KCall::Unexpected;
        }
        let reply_type = kind(reply);
        if reply_type == raw::REDISMODULE_REPLY_PROMISE as i32 {
            abort(reply, std::ptr::null_mut());
        }
        free(reply);
        if reply_type == raw::REDISMODULE_REPLY_NULL as i32 {
            KCall::Denied
        } else {
            KCall::Unexpected
        }
    }
}

fn call_wait_k(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    args.done()?;
    match native_call_wait_k(ctx, &key) {
        KCall::Unsupported => Ok(RedisValue::SimpleStringStatic("unsupported")),
        KCall::Denied => Ok(RedisValue::NullArray),
        KCall::Unexpected => Err(RedisError::Str("ERR module call unexpectedly blocked")),
    }
}

fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    if args.iter().any(|arg| arg.as_slice() == b"fail-load-k") {
        match native_call_wait_k(ctx, &ctx.create_string("keyblock:load-k")) {
            KCall::Denied => ctx.log_notice("keyblock: load K call denied without a waiter"),
            KCall::Unsupported => ctx.log_notice("keyblock: load K call unsupported"),
            // Retain the image if the regression ever creates a native waiter.
            KCall::Unexpected => return Status::Ok,
        }
        return Status::Err;
    }
    if args.iter().any(|arg| arg.as_slice() == b"fail-load-yield") {
        LOAD_TEST.store(true, Ordering::SeqCst);
        ctx.log_notice("keyblock: waiting for a real client during load");
        let started = std::time::Instant::now();
        while !LOAD_ATTEMPT.load(Ordering::SeqCst)
            && started.elapsed() < std::time::Duration::from_secs(3)
        {
            if ctx.yield_now(YieldMode::Clients, None).is_err() {
                break;
            }
            std::thread::yield_now();
        }
        LOAD_TEST.store(false, Ordering::SeqCst);
        if LIVE.load(Ordering::SeqCst) != 0 {
            return Status::Ok;
        }
        if LOAD_ATTEMPT.load(Ordering::SeqCst) {
            ctx.log_notice(&format!(
                "keyblock: real client refused while loading; live={} drops={}",
                LIVE.load(Ordering::SeqCst),
                DROPS.load(Ordering::SeqCst)
            ));
        }
        return Status::Err;
    }
    Status::Ok
}

// Minimal native control for yielded failed-load command-cache diagnostics.
// This does not construct a waiter or call any key-blocking API.
fn load_control(_: &Context, _: Vec<RedisString>) -> RedisResult {
    LOAD_ATTEMPT.store(true, Ordering::SeqCst);
    Ok(RedisValue::SimpleStringStatic("OK"))
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
    init: init,
    commands: [
        redis_command!("keyblock.wait", wait)
            .flags(if unsafe { raw::RedisModule_Yield }.is_some() {
                CommandFlags::READ_ONLY | CommandFlags::ALLOW_BUSY
            } else {
                CommandFlags::READ_ONLY
            })
            .keys(KeyRange::new(4).last(-1)),
        redis_command!("keyblock.load_control", load_control)
            .flags(if unsafe { raw::RedisModule_Yield }.is_some() {
                CommandFlags::READ_ONLY | CommandFlags::ALLOW_BUSY
            } else {
                CommandFlags::READ_ONLY
            }),
        ["keyblock.signal", signal, "readonly", 1, 1, 1, ""],
        ["keyblock.call_wait", call_wait, "readonly", 1, 1, 1, ""],
        ["keyblock.call_wait_k", call_wait_k, "readonly", 1, 1, 1, ""],
        ["keyblock.ready_outside", ready_outside, "readonly", 0, 0, 0, ""],
        ["keyblock.stats", stats, "readonly", 0, 0, 0, ""],
    ],
}
