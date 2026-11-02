//! `canonical.name 0` returns Redis's exact tag (7.4+); older servers return an
//! error. Callers keep their existing fallback policy when upgrading stored keys.
use redis_module::{
    Context, EventContext, NextArg, NotifyEvent, RedisError, RedisResult, RedisString,
    RegistrationContext, RuntimeContext, Status, ThreadSafeContext, cluster, raw,
    redis_event_handler, redis_module,
};
use std::os::raw::{c_char, c_uint};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

type Lookup = Option<unsafe extern "C" fn(c_uint) -> *const c_char>;
static EVENT_OK: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<JoinHandle<Result<(), RedisError>>>> = Mutex::new(None);

fn check(ctx: &RuntimeContext) -> Result<(), RedisError> {
    // Invalid slots must succeed with None even when the native API is absent.
    assert_eq!(ctx.canonical_key_for_slot(cluster::SLOTS)?, None);
    assert_eq!(ctx.canonical_key_for_slot(u16::MAX)?, None);
    if ctx.api_available("ClusterCanonicalKeyNameInSlot")? {
        let name = ctx.canonical_key_for_slot(0)?.unwrap();
        assert_eq!(cluster::key_slot(name.as_bytes()), 0);
    } else {
        assert!(ctx.canonical_key_for_slot(0).is_err());
    }
    Ok(())
}

fn name(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let slot: u16 = args.next_string()?.parse()?;
    args.done()?;
    Ok(ctx.canonical_key_for_slot(slot)?.into())
}

fn available(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(i64::from(ctx.api_available("ClusterCanonicalKeyNameInSlot")?).into())
}

fn all_slots(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let native = unsafe { raw::RedisModule_ClusterCanonicalKeyNameInSlot }
        .ok_or(RedisError::Str("ERR canonical API unavailable"))?;
    for slot in 0..cluster::SLOTS {
        let owned = ctx.canonical_key_for_slot(slot)?.unwrap();
        // Native comparison proves we neither add braces nor substitute the
        // SDK's names. Redis owns this immutable NUL-terminated table entry.
        let expected = unsafe { std::ffi::CStr::from_ptr(native(slot.into())) };
        assert_eq!(owned.as_bytes(), expected.to_bytes());
        assert_eq!(cluster::key_slot(owned.as_bytes()), slot);
    }
    Ok(i64::from(cluster::SLOTS).into())
}

fn on_event(ctx: &EventContext, _: NotifyEvent, _: &str, key: &[u8]) {
    if key == b"canonical:trigger" {
        EVENT_OK.store(check(ctx).is_ok(), Ordering::Release);
    }
}

fn event_ok(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(i64::from(EVENT_OK.load(Ordering::Acquire)).into())
}

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    let result = check(ctx).and_then(|_| {
        EVENT_OK.store(false, Ordering::Release);
        STOP.store(false, Ordering::Release);
        ctx.subscribe_to_keyspace_events(NotifyEvent::STRING, redis_event_handler!(on_event))
    });
    match result {
        Ok(()) => Status::Ok,
        Err(error) => {
            ctx.log_warning(&error.to_string());
            Status::Err
        }
    }
}

fn worker(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR worker already started"));
    }
    let owner = ThreadSafeContext::new();
    STOP.store(false, Ordering::Release);
    *worker = Some(thread::spawn(move || {
        while !STOP.load(Ordering::Acquire) {
            if let Some(guard) = owner.try_lock() {
                return check(&guard);
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }));
    Ok("OK".into())
}

fn join(worker: JoinHandle<Result<(), RedisError>>) -> Result<(), RedisError> {
    worker
        .join()
        .map_err(|_| RedisError::Str("ERR canonical worker panicked"))?
}

fn finish(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap();
    if !worker.as_ref().is_some_and(JoinHandle::is_finished) {
        return Ok(0_i64.into());
    }
    join(worker.take().unwrap())?;
    Ok(1_i64.into())
}

fn deinit(_: &EventContext) -> Status {
    STOP.store(true, Ordering::Release);
    // A waiting worker uses try_lock to observe STOP while unload holds the
    // Redis lock. Join it before freeing its context or unloading its code.
    match WORKER.lock().unwrap().take().map(join).transpose() {
        Ok(_) => Status::Ok,
        Err(_) => Status::Err,
    }
}

// Scoped diagnostics hold Redis's lock and never yield while replacing the
// module-local pointer. Restore the binding on all paths, before replying.
struct Restore(Lookup);
impl Drop for Restore {
    fn drop(&mut self) {
        unsafe { raw::RedisModule_ClusterCanonicalKeyNameInSlot = self.0 };
    }
}

unsafe extern "C" fn null_name(_: c_uint) -> *const c_char {
    std::ptr::null()
}

unsafe extern "C" fn invalid_utf8(_: c_uint) -> *const c_char {
    c"\xff".as_ptr()
}

unsafe extern "C" fn exact_name(_: c_uint) -> *const c_char {
    c"native:{exact}:name".as_ptr()
}

fn diagnostic(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let mode = args.next_string()?;
    args.done()?;
    let binding: Lookup = match mode.as_str() {
        "missing" => None,
        "null" => Some(null_name),
        "utf8" => Some(invalid_utf8),
        "exact" => Some(exact_name),
        _ => return Err(RedisError::Str("ERR unknown diagnostic")),
    };
    let _restore = Restore(unsafe { raw::RedisModule_ClusterCanonicalKeyNameInSlot });
    unsafe { raw::RedisModule_ClusterCanonicalKeyNameInSlot = binding };
    assert_eq!(ctx.canonical_key_for_slot(cluster::SLOTS)?, None);
    assert_eq!(ctx.canonical_key_for_slot(u16::MAX)?, None);
    Ok(ctx.canonical_key_for_slot(0)?.into())
}

redis_module! {
    name: "canonical_slot_name",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    deinit: deinit,
    commands: [
        ["canonical.name", name, "readonly", 0, 0, 0, ""],
        ["canonical.available", available, "readonly", 0, 0, 0, ""],
        ["canonical.all", all_slots, "readonly", 0, 0, 0, ""],
        ["canonical.diagnostic", diagnostic, "readonly", 0, 0, 0, ""],
        ["canonical.event_ok", event_ok, "readonly", 0, 0, 0, ""],
        ["canonical.worker", worker, "readonly", 0, 0, 0, ""],
        ["canonical.finish", finish, "readonly", 0, 0, 0, ""],
    ],
}
