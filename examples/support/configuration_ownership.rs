//! Isolated corrected-contract fixture, forwarding real initialized host APIs.
//! Counts only freshly owned getter results, not process heap or static backings.
use lazy_static::lazy_static;
use redis_module::{
    configuration::{register_string_configuration, ConfigurationContext, ConfigurationFlags},
    raw, redis_module, ConfigurationValue, Context, RedisError, RedisGILGuard, RedisResult,
    RedisString, RedisValue, Status,
};
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

type FreeFn = unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleString);
static FREE: OnceLock<FreeFn> = OnceLock::new();
static ISSUED: AtomicU64 = AtomicU64::new(0);
static RELEASED: AtomicU64 = AtomicU64::new(0);
static CHANGED: AtomicU64 = AtomicU64::new(0);
static CLOSING: AtomicBool = AtomicBool::new(false);
static REFUSE: AtomicBool = AtomicBool::new(false);
static LIVE: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());
static TEXT: Mutex<String> = Mutex::new(String::new());
static BINARY: Mutex<Vec<u8>> = Mutex::new(Vec::new());
lazy_static! {
    static ref NATIVE: RedisGILGuard<Option<RedisString>> = RedisGILGuard::new(None);
}
fn invariant(condition: bool, message: &'static str) {
    if !condition {
        raw::redis_log(
            std::ptr::null_mut(),
            &format!("F11_FIXTURE_INVARIANT {} {message}", crate::NAME),
        );
    }
    assert!(condition, "{message}");
}
fn issued(value: RedisString) -> RedisString {
    let unique = LIVE.lock().unwrap().insert(value.as_ptr() as usize);
    invariant(unique, "getter returned an already-live object");
    ISSUED.fetch_add(1, Ordering::Relaxed);
    value
}
unsafe extern "C" fn free(ctx: *mut raw::RedisModuleCtx, value: *mut raw::RedisModuleString) {
    let owned_reply = LIVE.lock().unwrap().remove(&(value as usize));
    unsafe { FREE.get().unwrap()(ctx, value) };
    if owned_reply {
        RELEASED.fetch_add(1, Ordering::Relaxed);
        if CLOSING.load(Ordering::Relaxed) {
            let count = LIVE.lock().unwrap().len();
            // Native RM_Log explicitly permits NULL ctx. Real owner/GIL phase,
            // no saved context; literal-safe raw logger forwards to the host.
            raw::redis_log(
                std::ptr::null_mut(),
                &format!("F11_REPLY_RELEASE {} live={count}", crate::NAME),
            );
        }
    }
}
enum Kind {
    Native,
    Text,
    Binary,
}
struct Tracked(Kind);
static NATIVE_CONFIG: Tracked = Tracked(Kind::Native);
static TEXT_CONFIG: Tracked = Tracked(Kind::Text);
static BINARY_CONFIG: Tracked = Tracked(Kind::Binary);
impl ConfigurationValue<RedisString> for Tracked {
    fn get(&self, ctx: &ConfigurationContext) -> RedisString {
        issued(match self.0 {
            Kind::Native => NATIVE.lock(ctx).as_ref().unwrap().clone(),
            Kind::Text => RedisString::create(None, TEXT.lock().unwrap().as_str()),
            Kind::Binary => RedisString::from_bytes(&BINARY.lock().unwrap()),
        })
    }
    fn set(&self, ctx: &ConfigurationContext, value: RedisString) -> Result<(), RedisError> {
        if value.as_slice() == b"reject" {
            return Err(RedisError::Str("ERR rejected configuration"));
        }
        match self.0 {
            Kind::Native => *NATIVE.lock(ctx) = Some(value),
            Kind::Text => *TEXT.lock().unwrap() = value.try_as_str()?.to_owned(),
            Kind::Binary => *BINARY.lock().unwrap() = value.as_slice().to_vec(),
        }
        Ok(())
    }
}
fn changed(_: &ConfigurationContext, _: &str, _: &'static Tracked) {
    CHANGED.fetch_add(1, Ordering::Relaxed);
}
fn init(ctx: &Context, args: &[RedisString]) -> Status {
    unsafe {
        // A permitted reload may retain the image/TLS on this platform. Keep
        // the original real host function and reset only verified empty state.
        if FREE.get().is_none() {
            FREE.set(raw::RedisModule_FreeString.unwrap()).unwrap();
        }
        raw::RedisModule_FreeString = Some(free);
    }
    let already_active =
        unsafe { redis_module::configuration::StringConfigurationLoadScope::begin(ctx) }.is_err();
    ctx.log_notice(&format!("F11_ALREADY_ACTIVE_REJECTED {already_active}"));
    if !already_active {
        return Status::Err;
    }
    let empty = LIVE.lock().unwrap().is_empty();
    invariant(empty, "load began with tracked reply owners");
    ISSUED.store(0, Ordering::Relaxed);
    RELEASED.store(0, Ordering::Relaxed);
    CHANGED.store(0, Ordering::Relaxed);
    CLOSING.store(false, Ordering::Relaxed);
    REFUSE.store(false, Ordering::Relaxed);
    // Explicit queued startup/LOADEX values compare against a valid initial
    // native backing before the first setter. Never expose an empty owner slot.
    {
        let mut native = NATIVE.lock(ctx);
        if native.is_none() {
            *native = Some(RedisString::create(None, "initial"));
        }
    }
    for (name, variable) in [
        ("native", &NATIVE_CONFIG),
        ("text", &TEXT_CONFIG),
        ("binary", &BINARY_CONFIG),
    ] {
        if register_string_configuration(
            ctx,
            name,
            variable,
            "default",
            ConfigurationFlags::DEFAULT,
            Some(Box::new(changed)),
        )
        .is_err()
        {
            return Status::Err;
        }
    }
    // Match the existing direct-init reference recipe and preserve ignored return.
    unsafe {
        raw::RedisModule_LoadConfigs.unwrap()(ctx.ctx);
    }
    // Legal actual OnLoad command access, creates post-callback snapshots that
    // the pending guard must close if this load is later rejected.
    let pattern = format!("{}.*", crate::NAME);
    if ctx.call("CONFIG", &["GET", pattern.as_str()]).is_err() {
        return Status::Err;
    }
    if args.first().is_some_and(|a| a.as_slice() == b"fail") {
        CLOSING.store(true, Ordering::Relaxed);
        return Status::Err;
    }
    if args.first().is_some_and(|a| a.as_slice() == b"duplicate") {
        let rejected = register_string_configuration(
            ctx,
            "text",
            &TEXT_CONFIG,
            "default",
            ConfigurationFlags::DEFAULT,
            None,
        )
        .is_err();
        CLOSING.store(true, Ordering::Relaxed);
        ctx.log_notice(&format!("F11_DUPLICATE_REJECTED {rejected}"));
        return Status::Err;
    }
    Status::Ok
}
fn deinit(ctx: &Context) -> Status {
    if REFUSE.load(Ordering::Relaxed) {
        return Status::Err;
    }
    CLOSING.store(true, Ordering::Relaxed);
    ctx.log_notice(&format!(
        "F11_CLOSE_BEGIN {} live={}",
        crate::NAME,
        LIVE.lock().unwrap().len()
    ));
    Status::Ok
}
fn stats(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    // Actual legal command context after commit; admission must reject before
    // native registration, with no fabricated pointer or API initialization.
    if register_string_configuration(
        ctx,
        "post-commit",
        &TEXT_CONFIG,
        "default",
        ConfigurationFlags::DEFAULT,
        None,
    )
    .is_ok()
    {
        return Err(RedisError::Str("ERR post-commit registration admitted"));
    }
    Ok(RedisValue::Array(vec![
        RedisValue::Integer(ISSUED.load(Ordering::Relaxed) as i64),
        RedisValue::Integer(RELEASED.load(Ordering::Relaxed) as i64),
        RedisValue::Integer(LIVE.lock().unwrap().len() as i64),
        RedisValue::Integer(CHANGED.load(Ordering::Relaxed) as i64),
    ]))
}
fn cancel(_: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    REFUSE.store(args[1].as_slice() == b"1", Ordering::Relaxed);
    Ok(RedisValue::SimpleStringStatic("OK"))
}
fn update(_: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    *TEXT.lock().unwrap() = args[1].try_as_str()?.to_owned();
    Ok(RedisValue::SimpleStringStatic("OK"))
}
redis_module! {
    name: crate::NAME,
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    deinit: deinit,
    commands: [
        [crate::STATS, stats, "", 0,0,0,""],
        [crate::CANCEL, cancel, "", 0,0,0,""],
        [crate::UPDATE, update, "", 0,0,0,""],
    ],
}
