//! Query native API presence from a live context: `api.available CreateString`.
//! The diagnostic commands temporarily hide one binding under the Redis lock
//! to exercise graceful missing-API behavior on a normal server.
use redis_module::{
    Context, NextArg, RedisError, RedisResult, RedisString, RegistrationContext, Status, raw,
    redis_module,
};
use std::sync::atomic::{AtomicI64, Ordering};

fn available(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_string()?;
    args.done()?;
    Ok(i64::from(ctx.api_available(&name)?).into())
}

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    match ctx.api_available("CreateString") {
        Ok(true) => Status::Ok,
        _ => Status::Err,
    }
}

// SAFETY: each instance points to its module's binding, is scoped to one
// synchronous command holding the Redis lock, and restores it before replying.
struct Restore<T: Copy> {
    slot: *mut T,
    value: T,
}

impl<T: Copy> Drop for Restore<T> {
    fn drop(&mut self) {
        unsafe { self.slot.write(self.value) };
    }
}

fn missing_lookup(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let _restore = Restore {
        slot: std::ptr::addr_of_mut!(raw::RedisModule_GetApi),
        value: unsafe { raw::RedisModule_GetApi },
    };
    unsafe { raw::RedisModule_GetApi = None };
    ctx.api_available("CreateString")?;
    Err(RedisError::Str("ERR missing lookup unexpectedly succeeded"))
}

static DROPS: AtomicI64 = AtomicI64::new(0);
static RUNS: AtomicI64 = AtomicI64::new(0);

struct Capture(Vec<u8>);
impl Drop for Capture {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
}

fn missing_job(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let _restore = Restore {
        slot: std::ptr::addr_of_mut!(raw::RedisModule_AddPostNotificationJob),
        value: unsafe { raw::RedisModule_AddPostNotificationJob },
    };
    let before_drops = DROPS.load(Ordering::Relaxed);
    let before_runs = RUNS.load(Ordering::Relaxed);
    let capture = Capture(vec![42; 8]);
    unsafe { raw::RedisModule_AddPostNotificationJob = None };
    let result = ctx.add_post_notification_job(move |_| {
        assert_eq!(capture.0, [42; 8]);
        RUNS.fetch_add(1, Ordering::Relaxed);
        drop(capture);
    });
    #[cfg(any(
        feature = "min-redis-compatibility-version-6-0",
        feature = "min-redis-compatibility-version-6-2",
        feature = "min-redis-compatibility-version-7-0"
    ))]
    let missing = matches!(result, Err(ref error) if error == "RedisModule_AddPostNotificationJob does not exists");
    #[cfg(not(any(
        feature = "min-redis-compatibility-version-6-0",
        feature = "min-redis-compatibility-version-6-2",
        feature = "min-redis-compatibility-version-7-0"
    )))]
    let missing = result == Status::Err;
    if !missing {
        return Err(RedisError::Str(
            "ERR missing job API unexpectedly succeeded",
        ));
    }
    Ok(vec![
        DROPS.load(Ordering::Relaxed) - before_drops,
        RUNS.load(Ordering::Relaxed) - before_runs,
    ]
    .into())
}

redis_module! {
    name: "api_availability",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        ["api.available", available, "readonly", 0, 0, 0, ""],
        ["api.missing_lookup", missing_lookup, "readonly", 0, 0, 0, ""],
        ["api.missing_job", missing_job, "readonly", 0, 0, 0, ""],
    ],
}
