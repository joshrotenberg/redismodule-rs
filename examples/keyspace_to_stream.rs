//! Standalone Redis 7.2+: defer notification writes and replicate their results.
//! See docs/KEYSPACE-NOTIFICATIONS.md for the lifecycle and failure model.
use redis_module::{
    CallOptionsBuilder, CallResult, Context, ContextFlags, EventContext, NotifyEvent, RedisError,
    RedisResult, RedisString, RegistrationContext, Status, redis_command, redis_module,
};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

const PREFIX: &[u8] = b"watched:";
const STREAM: &[u8] = b"watched:events";
const STREAM_DB: i32 = 0;
static QUEUED: AtomicI64 = AtomicI64::new(0);
static WRITTEN: AtomicI64 = AtomicI64::new(0);
static ERRORS: AtomicI64 = AtomicI64::new(0);
static SKIPPED: AtomicI64 = AtomicI64::new(0);
static STOPPING: AtomicBool = AtomicBool::new(false);

fn writable_primary(ctx: &EventContext) -> bool {
    let flags = ctx.get_flags();
    !STOPPING.load(Ordering::Relaxed)
        && flags.contains(ContextFlags::MASTER)
        && !flags.intersects(
            ContextFlags::LOADING | ContextFlags::ASYNC_LOADING | ContextFlags::READONLY,
        )
}

fn append(ctx: &EventContext, db: i32, event: &str, key: &[u8]) -> Result<(), RedisError> {
    ctx.with_db(STREAM_DB, |ctx| {
        let db = db.to_string();
        let args: &[&[u8]] = &[
            STREAM,
            b"*",
            b"db",
            db.as_bytes(),
            b"event",
            event.as_bytes(),
            b"key",
            key,
        ];
        let options = CallOptionsBuilder::new()
            .replicate()
            .errors_as_replies()
            .verify_oom()
            .build();
        let reply: CallResult = ctx.call_ext("XADD", &options, args);
        reply.map(|_| ()).map_err(|error| {
            RedisError::String(String::from_utf8_lossy(error.as_bytes()).into_owned())
        })
    })?
}

fn on_event(ctx: &EventContext, _: NotifyEvent, event: &str, key: &[u8]) {
    // The output stream deliberately shares the watched prefix. Exclude it
    // before scheduling, since XADD emits another STREAM notification.
    if key == STREAM || !key.starts_with(PREFIX) {
        return;
    }
    if !writable_primary(ctx) {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // Own callback data, never a context reference or an open key handle.
    let db = ctx.get_selected_db();
    let event = event.to_owned();
    let key = key.to_vec();
    let status = ctx.add_post_notification_job(move |ctx| {
        if !writable_primary(ctx) {
            SKIPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match append(ctx, db, &event, &key) {
            Ok(()) => {
                WRITTEN.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                ERRORS.fetch_add(1, Ordering::Relaxed);
                ctx.log_warning(&format!("notification stream append failed: {error}"));
            }
        }
    });
    match status {
        Status::Ok => {
            QUEUED.fetch_add(1, Ordering::Relaxed);
        }
        Status::Err => {
            ERRORS.fetch_add(1, Ordering::Relaxed);
            ctx.log_warning("notification job submission rejected");
        }
    }
}

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    if ctx.get_flags().contains(ContextFlags::CLUSTER) {
        ctx.log_warning("keyspace_to_stream requires standalone Redis");
        return Status::Err;
    }
    // An explicit compatibility floor changes the Rust return type, not the
    // running server. Check the optional native API before accepting the load.
    if unsafe { redis_module::raw::RedisModule_AddPostNotificationJob }.is_none() {
        ctx.log_warning("keyspace_to_stream requires Redis 7.2+ post-notification jobs");
        return Status::Err;
    }
    STOPPING.store(false, Ordering::Relaxed);
    Status::Ok
}

fn deinit(_: &EventContext) -> Status {
    // Set this before any future teardown operation that could emit events.
    // This example has no threads, timers or retained callbacks to drain.
    STOPPING.store(true, Ordering::Relaxed);
    Status::Ok
}

fn stats(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        QUEUED.load(Ordering::Relaxed),
        WRITTEN.load(Ordering::Relaxed),
        ERRORS.load(Ordering::Relaxed),
        SKIPPED.load(Ordering::Relaxed),
    ]
    .into())
}

// A read-only diagnostic for captured-data ownership on accepted/rejected jobs.
// It changes module-local counters, never the Redis keyspace.
static PROBE_DROPS: AtomicI64 = AtomicI64::new(0);
static PROBE_RUNS: AtomicI64 = AtomicI64::new(0);
struct Probe(Vec<u8>);
impl Drop for Probe {
    fn drop(&mut self) {
        PROBE_DROPS.fetch_add(1, Ordering::Relaxed);
    }
}
fn job_probe(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let before = PROBE_DROPS.load(Ordering::Relaxed);
    let probe = Probe(vec![42; 8]);
    let status = ctx.add_post_notification_job(move |_| {
        assert_eq!(probe.0, [42; 8]);
        PROBE_RUNS.fetch_add(1, Ordering::Relaxed);
        drop(probe);
    });
    Ok(vec![
        i64::from(status == Status::Ok),
        PROBE_DROPS.load(Ordering::Relaxed) - before,
    ]
    .into())
}
fn probe_counts(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        PROBE_DROPS.load(Ordering::Relaxed),
        PROBE_RUNS.load(Ordering::Relaxed),
    ]
    .into())
}

redis_module! {
    name: "keyspace_to_stream",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    deinit: deinit,
    commands: [
        redis_command!("notify_stream.stats", stats).read_only(),
        redis_command!("notify_stream.job_probe", job_probe).read_only(),
        redis_command!("notify_stream.probe_counts", probe_counts).read_only(),
    ],
    event_handlers: [
        [@STRING: on_event],
        [@STREAM: on_event],
        [@LOADED: on_event],
    ],
}
