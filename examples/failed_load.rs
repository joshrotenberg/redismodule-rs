//! A module whose load can fail after it subscribed to events. Load it with
//! `fail` and `init` refuses the load after a keyspace handler, attribute
//! server-event handlers and a runtime server-event handler were declared or
//! subscribed, and, given `fifo:path`, after it watched a FIFO. Redis before
//! 7.2.4 keeps a failed load's native subscriptions, and every Redis keeps its
//! watched descriptors; the SDK leaves none behind, so later events cannot
//! reach the unloaded code.
use redis_module::event_loop::{self, EventLoopMask};
use redis_module::{
    Context, EventContext, NotifyEvent, RedisError, RedisResult, RedisString, RegistrationContext,
    Status, redis_module,
    server_events::{FlushSubevent, ServerEventHandler, SwapDbInfo},
};
use redis_module_macros::{cron_event_handler, flush_event_handler};
use std::sync::atomic::{AtomicI64, Ordering};

/// Keyspace, flush, cron and swapdb deliveries, in that order.
static COUNTS: [AtomicI64; 4] = [const { AtomicI64::new(0) }; 4];

fn on_string(_: &EventContext, _: NotifyEvent, _: &str, _: &[u8]) {
    COUNTS[0].fetch_add(1, Ordering::Relaxed);
}

#[flush_event_handler]
fn on_flush(_: &EventContext, _: FlushSubevent) {
    COUNTS[1].fetch_add(1, Ordering::Relaxed);
}

#[cron_event_handler]
fn on_cron(_: &EventContext, _: u64) {
    COUNTS[2].fetch_add(1, Ordering::Relaxed);
}

fn on_swapdb(_: &EventContext, _: SwapDbInfo) {
    COUNTS[3].fetch_add(1, Ordering::Relaxed);
}

/// Watches the FIFO at `path`, created here, until the load fails.
fn watch_fifo(path: &[u8]) -> Result<(), RedisError> {
    let path = std::ffi::CString::new(path)?;
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
        return Err(RedisError::Str("ERR mkfifo failed"));
    }
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return Err(RedisError::Str("ERR opening the FIFO failed"));
    }
    // SAFETY: init runs on the main thread, and the module owns the FIFO.
    unsafe { event_loop::add(fd, EventLoopMask::READABLE, |_, _, _| {}) }
}

fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    // A runtime subscription made before the load fails.
    if let Err(error) = ctx.subscribe_to_server_event(ServerEventHandler::SwapDb(on_swapdb)) {
        ctx.log_warning(&error.to_string());
        return Status::Err;
    }
    // `fifo:path` watches a FIFO before the load fails.
    for arg in args {
        if let Some(path) = arg.as_slice().strip_prefix(b"fifo:") {
            if let Err(error) = watch_fifo(path) {
                ctx.log_warning(&error.to_string());
                return Status::Err;
            }
        }
    }
    if args.iter().any(|arg| arg.as_slice() == b"fail") {
        ctx.log_notice("failed_load: refusing the load as asked");
        return Status::Err;
    }
    Status::Ok
}

/// `failed_load.counts`: keyspace, flush, cron and swapdb deliveries.
fn counts(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(COUNTS
        .iter()
        .map(|count| count.load(Ordering::Relaxed))
        .collect::<Vec<i64>>()
        .into())
}

redis_module! {
    name: "failed_load",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        ["failed_load.counts", counts, "readonly", 0, 0, 0, ""],
    ],
    event_handlers: [
        [@STRING: on_string],
    ],
}
