//! Paired free-function/builder checks in actual isolated native OnLoad phases.
use redis_module::{
    configuration::{
        config, register_bool_configuration, register_enum_configuration,
        register_i64_configuration, ConfigurationContext, ConfigurationFlags,
    },
    enum_configuration, raw, redis_module, ConfigurationValue, Context, RedisError, RedisResult,
    RedisString, RedisValue, Status,
};
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

static NUMBER: AtomicI64 = AtomicI64::new(7);
static TOGGLE: AtomicBool = AtomicBool::new(true);
static CHANGES: AtomicU64 = AtomicU64::new(0);
static DROPS: AtomicU64 = AtomicU64::new(0);
static ISSUED: AtomicU64 = AtomicU64::new(0);
static RELEASED: AtomicU64 = AtomicU64::new(0);
static CLOSING: AtomicBool = AtomicBool::new(false);
static LIVE: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());
static TEXT: Mutex<String> = Mutex::new(String::new());
enum_configuration! { enum Choice { One = 1, Two = 2, } }
static CHOICE: Mutex<Choice> = Mutex::new(Choice::One);
struct Number;
static NUMBER_CONFIG: Number = Number;
impl ConfigurationValue<i64> for Number {
    fn get(&self, _: &ConfigurationContext) -> i64 {
        NUMBER.load(Ordering::Relaxed)
    }
    fn set(&self, _: &ConfigurationContext, value: i64) -> Result<(), RedisError> {
        if value == 13 {
            return Err(RedisError::Str("ERR thirteen is rejected"));
        }
        NUMBER.store(value, Ordering::Relaxed);
        Ok(())
    }
}
struct Text;
static TEXT_CONFIG: Text = Text;
impl ConfigurationValue<RedisString> for Text {
    fn get(&self, _: &ConfigurationContext) -> RedisString {
        let value = RedisString::create(None, TEXT.lock().unwrap().as_str());
        if FREE.get().is_some() {
            let unique = LIVE.lock().unwrap().insert(value.as_ptr() as usize);
            assert!(unique, "F14 getter produced an already tracked object");
            ISSUED.fetch_add(1, Ordering::Relaxed);
        }
        value
    }
    fn set(&self, _: &ConfigurationContext, value: RedisString) -> Result<(), RedisError> {
        *TEXT.lock().unwrap() = value.try_as_str()?.to_owned();
        Ok(())
    }
}
type FreeFn = unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleString);
static FREE: OnceLock<FreeFn> = OnceLock::new();
unsafe extern "C" fn free(ctx: *mut raw::RedisModuleCtx, value: *mut raw::RedisModuleString) {
    let owned = LIVE.lock().unwrap().remove(&(value as usize));
    unsafe { FREE.get().unwrap()(ctx, value) };
    if owned {
        RELEASED.fetch_add(1, Ordering::Relaxed);
        if CLOSING.load(Ordering::Relaxed) {
            // Real host RM_Log permits NULL; release is on the legal owner/GIL path.
            raw::redis_log(
                std::ptr::null_mut(),
                &format!(
                    "F14_RELEASE live={} issued={} released={}",
                    LIVE.lock().unwrap().len(),
                    ISSUED.load(Ordering::Relaxed),
                    RELEASED.load(Ordering::Relaxed)
                ),
            );
        }
    }
}
struct Capture(&'static str);
impl Drop for Capture {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::Relaxed);
        raw::redis_log(std::ptr::null_mut(), &format!("F14_DROP {}", self.0));
    }
}
fn changed<T: 'static>(kind: &'static str) -> impl Fn(&ConfigurationContext, &str, &'static T) {
    let capture = Capture(kind);
    move |_, _, _| {
        let _ = &capture;
        CHANGES.fetch_add(1, Ordering::Relaxed);
    }
}

static LOCKED: Mutex<String> = Mutex::new(String::new());
fn mode() -> String {
    std::env::var("F14_MODE").unwrap_or_else(|_| "success".into())
}
fn builder() -> bool {
    std::env::var("F14_BACKEND").as_deref() != Ok("free")
}
fn integer(ctx: &Context, name: &str, flags: ConfigurationFlags) -> Result<(), RedisError> {
    if builder() {
        config::integer(name, &NUMBER_CONFIG, 7, 1..=100)
            .flags(flags)
            .on_changed(changed("integer"))
            .register(ctx)
    } else {
        register_i64_configuration(
            ctx,
            name,
            &NUMBER_CONFIG,
            7,
            1,
            100,
            flags,
            Some(Box::new(changed("integer"))),
        )
    }
}
fn boolean(ctx: &Context, name: &str, flags: ConfigurationFlags) -> Result<(), RedisError> {
    if builder() {
        config::boolean(name, &TOGGLE, true)
            .flags(flags)
            .on_changed(changed("boolean"))
            .register(ctx)
    } else {
        register_bool_configuration(
            ctx,
            name,
            &TOGGLE,
            true,
            flags,
            Some(Box::new(changed("boolean"))),
        )
    }
}
fn enumeration(ctx: &Context, name: &str, flags: ConfigurationFlags) -> Result<(), RedisError> {
    if builder() {
        config::enumeration(name, &CHOICE, Choice::One)
            .flags(flags)
            .on_changed(changed("enumeration"))
            .register(ctx)
    } else {
        register_enum_configuration(
            ctx,
            name,
            &CHOICE,
            Choice::One,
            flags,
            Some(Box::new(changed("enumeration"))),
        )
    }
}
fn string(
    ctx: &Context,
    name: &str,
    default: &str,
    flags: ConfigurationFlags,
) -> Result<(), RedisError> {
    if builder() {
        config::string(name, &TEXT_CONFIG, default)
            .flags(flags)
            .on_changed(changed("string"))
            .register(ctx)
    } else {
        redis_module::configuration::register_string_configuration(
            ctx,
            name,
            &TEXT_CONFIG,
            default,
            flags,
            Some(Box::new(changed("string"))),
        )
    }
}
fn init(ctx: &Context, _: &[RedisString]) -> Status {
    unsafe {
        if FREE.get().is_none() {
            FREE.set(raw::RedisModule_FreeString.unwrap()).unwrap();
        }
        raw::RedisModule_FreeString = Some(free);
    }
    assert!(
        LIVE.lock().unwrap().is_empty(),
        "F14 load began with live owners"
    );
    ISSUED.store(0, Ordering::Relaxed);
    RELEASED.store(0, Ordering::Relaxed);
    DROPS.store(0, Ordering::Relaxed);
    CHANGES.store(0, Ordering::Relaxed);
    CLOSING.store(false, Ordering::Relaxed);
    let register = || -> Result<(), RedisError> {
        integer(ctx, "number", ConfigurationFlags::DEFAULT)?;
        boolean(ctx, "toggle", ConfigurationFlags::DEFAULT)?;
        enumeration(ctx, "choice", ConfigurationFlags::DEFAULT)?;
        // Native registration copies text; local name/default owners can end
        // before defaults load and later CONFIG commands.
        let name = String::from("text");
        let default = String::from("default");
        string(ctx, &name, &default, ConfigurationFlags::DEFAULT)?;
        if builder() {
            config::string("locked", &LOCKED, "fixed")
                .flags(ConfigurationFlags::IMMUTABLE)
                .register(ctx)?;
        } else {
            redis_module::configuration::register_string_configuration(
                ctx,
                "locked",
                &LOCKED,
                "fixed",
                ConfigurationFlags::IMMUTABLE,
                None,
            )?;
        }
        Ok(())
    };
    if let Err(error) = register() {
        ctx.log_warning(&error.to_string());
        return Status::Err;
    }
    // Preserve the direct-reference manual LoadConfigs result/order policy.
    unsafe {
        raw::RedisModule_LoadConfigs.unwrap()(ctx.ctx);
    }
    if ctx.call("CONFIG", &["GET", "cfgbuilders.text"]).is_err() {
        return Status::Err;
    }
    let mode = mode();
    let rejected = match mode.as_str() {
        "integer_flags" => Some(integer(ctx, "rejected", ConfigurationFlags::BITFLAGS)),
        "boolean_flags" => Some(boolean(ctx, "rejected", ConfigurationFlags::MEMORY)),
        "integer_duplicate" => Some(integer(ctx, "number", ConfigurationFlags::DEFAULT)),
        "boolean_name" => Some(boolean(ctx, "bad.name", ConfigurationFlags::DEFAULT)),
        "enum_flags" => Some(enumeration(ctx, "rejected", ConfigurationFlags::MEMORY)),
        "string_duplicate" => Some(string(ctx, "text", "rejected", ConfigurationFlags::DEFAULT)),
        "string_flags" => Some(string(
            ctx,
            "rejected",
            "rejected",
            ConfigurationFlags::BITFLAGS,
        )),
        _ => None,
    };
    if let Some(result) = rejected {
        CLOSING.store(true, Ordering::Relaxed);
        match result {
            Err(error) => ctx.log_notice(&format!(
                "F14_REJECTED {mode} drops={} error={error}",
                DROPS.load(Ordering::Relaxed)
            )),
            Ok(()) => ctx.log_warning("F14_UNEXPECTED_ACCEPTANCE"),
        }
        return Status::Err;
    }
    Status::Ok
}
fn deinit(_: &Context) -> Status {
    CLOSING.store(true, Ordering::Relaxed);
    Status::Ok
}
fn stats(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(vec![
        RedisValue::Integer(CHANGES.load(Ordering::Relaxed) as i64),
        RedisValue::Integer(DROPS.load(Ordering::Relaxed) as i64),
        RedisValue::Integer(ISSUED.load(Ordering::Relaxed) as i64),
        RedisValue::Integer(RELEASED.load(Ordering::Relaxed) as i64),
        RedisValue::Integer(LIVE.lock().unwrap().len() as i64),
        RedisValue::Integer(i32::from(CHOICE.lock().unwrap().clone()) as i64),
    ]))
}
fn late(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    match string(ctx, "late", "late", ConfigurationFlags::DEFAULT) {
        Err(error) => Ok(RedisValue::BulkString(error.to_string())),
        Ok(()) => Err(RedisError::Str("ERR F14 post-commit admission accepted")),
    }
}
redis_module! {
    name:"cfgbuilders",version:1,
    allocator:(redis_module::alloc::RedisAlloc,redis_module::alloc::RedisAlloc),
    data_types:[],init:init,deinit:deinit,
    commands:[
        ["cfgbuilders.stats",stats,"",0,0,0,""],
        ["cfgbuilders.late",late,"",0,0,0,""],
    ],
}
