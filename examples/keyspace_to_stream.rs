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
static PROBE_UNLOAD: AtomicBool = AtomicBool::new(false);

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

fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
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
    PROBE_UNLOAD.store(
        args.iter().any(|arg| arg.as_slice() == b"probe-unload"),
        Ordering::Relaxed,
    );
    if args
        .iter()
        .any(|arg| matches!(arg.as_slice(), b"probe-load" | b"fail-load"))
    {
        if submit_probe(ctx, "run")[0] != 0 {
            return Status::Err;
        }
        ctx.log_notice("load job probe refused");
    }
    if args.iter().any(|arg| arg.as_slice() == b"fail-load") {
        return Status::Err;
    }
    Status::Ok
}

fn deinit(ctx: &EventContext) -> Status {
    if PROBE_UNLOAD.load(Ordering::Relaxed) {
        if submit_probe(ctx, "run")[0] != 0 {
            return Status::Err;
        }
        ctx.log_notice("unload job probe refused");
    }
    // Set this before any future teardown operation that could emit events.
    // The SDK refuses entry here while Redis still owns a notification job.
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
struct Probe(Vec<u8>, bool);
impl Drop for Probe {
    fn drop(&mut self) {
        PROBE_DROPS.fetch_add(1, Ordering::Relaxed);
        assert!(!self.1, "post-notification capture drop panic");
    }
}
fn submit_probe(ctx: &EventContext, mode: &str) -> Vec<i64> {
    let before = PROBE_DROPS.load(Ordering::Relaxed);
    let probe = Probe(vec![42; 8], mode == "drop-panic");
    let panic = mode == "panic";
    let cancel = mode == "cancel";
    let unload = mode == "unload";
    let status = ctx.add_post_notification_job(move |ctx| {
        assert_eq!(probe.0, [42; 8]);
        if cancel {
            // Logical cancellation does not release Redis's native callback.
            return;
        }
        PROBE_RUNS.fetch_add(1, Ordering::Relaxed);
        assert!(!panic, "post-notification probe panic");
        if unload {
            let options = CallOptionsBuilder::new().errors_as_replies().build();
            let reply: CallResult =
                ctx.call_ext("MODULE", &options, &["UNLOAD", "keyspace_to_stream"]);
            assert!(reply.is_err(), "executing job unloaded its module");
        }
        drop(probe);
    });
    vec![
        i64::from(status == Status::Ok),
        PROBE_DROPS.load(Ordering::Relaxed) - before,
    ]
}
fn job_probe(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mode = args.get(1).map_or(Ok("run"), RedisString::try_as_str)?;
    Ok(submit_probe(ctx, mode).into())
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
