//! A worker checks its stop flag between nonblocking Redis lock attempts.
//! `MODULE UNLOAD` holds the Redis lock while deinit stops and joins the worker.
use redis_module::{
    Context, EventContext, NextArg, RedisError, RedisResult, RedisString, Status,
    ThreadSafeContext, WorkerContext, redis_module,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// The command holds Redis's lock until this worker has attempted both APIs.
fn contended(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let owner = ThreadSafeContext::new();
    let results = thread::spawn(move || {
        vec![
            i64::from(owner.try_lock().is_none()),
            i64::from(redis_module::MODULE_CONTEXT.try_lock().is_none()),
        ]
    })
    .join()
    .map_err(|_| RedisError::Str("ERR contention worker panicked"))?;
    Ok(results.into())
}

fn increment(ctx: &WorkerContext) {
    ctx.call("INCR", &["try_lock:count"])
        .expect("worker INCR failed");
}

/// `try_lock.start thread|detached` increments a counter until stopped.
fn start(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let mode = args.next_string()?;
    args.done()?;
    let owner = match mode.as_str() {
        "thread" => Some(ThreadSafeContext::new()),
        "detached" => None,
        _ => return Err(RedisError::Str("ERR expected thread or detached")),
    };
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR worker already started"));
    }
    STOP.store(false, Ordering::Release);
    *worker = Some(thread::spawn(move || {
        while !STOP.load(Ordering::Acquire) {
            if let Some(owner) = &owner {
                if let Some(guard) = owner.try_lock() {
                    increment(&guard);
                }
            } else if let Some(guard) = redis_module::MODULE_CONTEXT.try_lock() {
                increment(&guard);
            }
            // Each guard is dropped before sleeping or checking the stop flag.
            thread::sleep(Duration::from_millis(1));
        }
    }));
    Ok("OK".into())
}

fn stop_worker() -> Result<(), RedisError> {
    STOP.store(true, Ordering::Release);
    if let Some(worker) = WORKER.lock().unwrap().take() {
        // Redis's lock is held here. A blocking lock in the worker would
        // deadlock this join; try_lock lets it reach the stop flag instead.
        worker
            .join()
            .map_err(|_| RedisError::Str("ERR worker panicked"))?;
    }
    Ok(())
}

fn stop(_: &Context, _: Vec<RedisString>) -> RedisResult {
    stop_worker()?;
    Ok("OK".into())
}

fn deinit(_: &EventContext) -> Status {
    match stop_worker() {
        Ok(()) => Status::Ok,
        Err(_) => Status::Err,
    }
}

redis_module! {
    name: "try_lock",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    deinit: deinit,
    commands: [
        ["try_lock.contended", contended, "", 0, 0, 0, ""],
        ["try_lock.start", start, "write", 0, 0, 0, ""],
        ["try_lock.stop", stop, "", 0, 0, 0, ""],
    ],
}
