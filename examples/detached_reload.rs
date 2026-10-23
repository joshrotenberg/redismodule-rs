//! Uses the module's detached context, `MODULE_CONTEXT`, across `MODULE UNLOAD`
//! and `MODULE LOAD`: the SDK logger and a worker both use it, and a reload of
//! a retained image must give them a fresh one.
use redis_module::{
    Context, RedisError, RedisResult, RedisString, RedisValue, RegistrationContext, Status,
    ThreadSafeContext, logging, redis_module,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::thread::{self, JoinHandle};

static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// Loads of this image, failed ones included; statics survive a reload only
/// while the image stays mapped.
static LOADS: AtomicI64 = AtomicI64::new(0);

/// A thread local with a destructor, touched on the main thread, keeps glibc
/// from unmapping the image at unload. macOS keeps Rust dylibs mapped anyway.
struct Retain;

impl Drop for Retain {
    fn drop(&mut self) {}
}

thread_local! {
    static RETAIN: Retain = const { Retain };
}

/// Installs the SDK logger on the detached context. On a reload of a retained
/// image the logger is already installed and adopts the new context. With the
/// argument `fail`, the load fails after that.
fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    RETAIN.with(|_| ());
    LOADS.fetch_add(1, Ordering::Relaxed);
    if let Err(error) = logging::setup() {
        ctx.log_warning(&format!("detached_reload: {error}"));
        return Status::Err;
    }
    if matches!(args.first().map(RedisString::try_as_str), Some(Ok("fail"))) {
        return Status::Err;
    }
    Status::Ok
}

/// `detached.loads` returns how many times this image was loaded.
fn loads(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(LOADS.load(Ordering::Relaxed).into())
}

/// `detached.log <message>` logs through the SDK logger.
fn log_message(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let [_, message] = args.as_slice() else {
        return Err(RedisError::WrongArity);
    };
    log::info!("detached.log {}", message.try_as_str()?);
    Ok("OK".into())
}

/// `detached.worker` runs `PING` from a worker holding `MODULE_CONTEXT`'s lock
/// and replies through the blocked client.
fn worker(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut task = WORKER.lock().unwrap();
    if task.is_some() {
        return Err(RedisError::Str("ERR worker already started"));
    }
    let blocked = ctx.block_client()?;
    *task = Some(thread::spawn(move || {
        let reply = {
            let guard = redis_module::MODULE_CONTEXT.lock();
            guard.call("PING", &[] as &[&str])
        };
        ThreadSafeContext::with_blocked_client(blocked).reply(reply);
    }));
    Ok(RedisValue::NoReply)
}

/// Joins a finished worker: 1 when joined, 0 while it still runs. Never joins
/// a live worker while holding Redis's lock.
fn finish(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut task = WORKER.lock().unwrap();
    if !task.as_ref().is_some_and(JoinHandle::is_finished) {
        return Ok(0i64.into());
    }
    task.take()
        .unwrap()
        .join()
        .map_err(|_| RedisError::Str("ERR worker panicked"))?;
    Ok(1i64.into())
}

redis_module! {
    name: "detached_reload",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        ["detached.log", log_message, "", 0, 0, 0, ""],
        ["detached.worker", worker, "", 0, 0, 0, ""],
        ["detached.finish", finish, "", 0, 0, 0, ""],
        ["detached.loads", loads, "", 0, 0, 0, ""],
    ],
}
