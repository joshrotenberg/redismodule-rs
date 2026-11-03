//! `yield.busy <ms> <events|clients>`: spins for `ms` milliseconds and yields
//! to Redis as it goes, so tests can check what other clients see meanwhile.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use redis_module::{
    Context, EventContext, NextArg, NotifyEvent, RedisError, RedisResult, RedisString, RedisValue,
    Status, ThreadSafeContext, YieldMode, redis_module,
};

static EVENT_RESULT: AtomicI64 = AtomicI64::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
type YieldWorker = JoinHandle<Result<[i64; 2], RedisError>>;
static WORKER: Mutex<Option<YieldWorker>> = Mutex::new(None);

fn once(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.yield_now(YieldMode::Events, None)?;
    Ok("OK".into())
}

fn on_event(ctx: &EventContext, _: NotifyEvent, _: &str, key: &[u8]) {
    if key == b"yield:trigger" {
        let result = match ctx.yield_now(YieldMode::Events, None) {
            Ok(()) => 1,
            Err(RedisError::Str("ERR yielding requires the Redis main thread")) => 2,
            Err(_) => 3,
        };
        EVENT_RESULT.store(result, Ordering::Release);
    }
}

fn event_result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(EVENT_RESULT.load(Ordering::Acquire).into())
}

// Exercises synchronous module commands and keyspace callbacks called from a
// locked worker. The diagnostic commands use only the fixed yield:trigger key.
fn worker(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap_or_else(|e| e.into_inner());
    if worker.is_some() {
        return Err(RedisError::Str("ERR worker result has not been collected"));
    }
    STOP.store(false, Ordering::Release);
    EVENT_RESULT.store(0, Ordering::Release);
    let owner = ThreadSafeContext::new();
    *worker = Some(thread::spawn(move || {
        while !STOP.load(Ordering::Acquire) {
            if let Some(guard) = owner.try_lock() {
                let refused = guard
                    .call("yield.once", &[] as &[&str])
                    .is_err_and(|error| {
                        error
                            .to_string()
                            .contains("yielding requires the Redis main thread")
                    });
                guard.call("SET", &["yield:trigger", "worker"])?;
                return Ok([i64::from(refused), EVENT_RESULT.load(Ordering::Acquire)]);
            }
            thread::sleep(Duration::from_millis(1));
        }
        Err(RedisError::Str("ERR worker stopped"))
    }));
    Ok("OK".into())
}

fn worker_result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap_or_else(|e| e.into_inner());
    if !worker.as_ref().is_some_and(JoinHandle::is_finished) {
        return Ok(RedisValue::Null);
    }
    let worker = worker.take().unwrap();
    let result = worker
        .join()
        .map_err(|_| RedisError::Str("ERR worker panicked"))??;
    Ok(result.to_vec().into())
}

fn deinit(_: &EventContext) -> Status {
    STOP.store(true, Ordering::Release);
    if let Some(worker) = WORKER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = worker.join();
    }
    Status::Ok
}

fn busy(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let millis = args.next_u64()?;
    let mode = match args.next_string()?.as_str() {
        "events" => YieldMode::Events,
        "clients" => YieldMode::Clients,
        _ => return Err(RedisError::Str("ERR expected events or clients")),
    };
    args.done()?;

    let end = Instant::now() + Duration::from_millis(millis);
    let mut yields: i64 = 0;
    while Instant::now() < end {
        ctx.yield_now(mode, Some("yield.busy is running"))?;
        yields += 1;
    }
    Ok(yields.into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "yield",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    deinit: deinit,
    commands: [
        ["yield.busy", busy, "", 0, 0, 0, ""],
        ["yield.once", once, "", 0, 0, 0, ""],
        ["yield.event_result", event_result, "", 0, 0, 0, ""],
        ["yield.worker", worker, "", 0, 0, 0, ""],
        ["yield.worker_result", worker_result, "", 0, 0, 0, ""],
    ],
    event_handlers: [[@STRING: on_event]],
}
