//! Corrected native registration fixture: real OnLoad contexts and isolated names.
use redis_module::{
    configuration::{
        register_bool_configuration, register_enum_configuration, register_i64_configuration,
        ConfigurationContext, ConfigurationFlags,
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
            assert!(unique, "F13 getter produced an already tracked object");
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
                    "F13_RELEASE live={} issued={} released={}",
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
        raw::redis_log(std::ptr::null_mut(), &format!("F13_DROP {}", self.0));
    }
}
#[expect(clippy::type_complexity)]
fn changed<T: 'static>(kind: &'static str) -> Box<dyn Fn(&ConfigurationContext, &str, &'static T)> {
    let capture = Capture(kind);
    Box::new(move |_, _, _| {
        let _ = &capture;
        CHANGES.fetch_add(1, Ordering::Relaxed);
    })
}
fn mode() -> String {
    std::env::var("F13_MODE").unwrap_or_else(|_| "success".into())
}
fn number_name() -> &'static str {
    if mode() == "macro_numeric_name" {
        "bad.name"
    } else {
        "number"
    }
}
fn number_flags() -> ConfigurationFlags {
    if mode() == "macro_numeric_flags" {
        ConfigurationFlags::BITFLAGS
    } else {
        ConfigurationFlags::DEFAULT
    }
}
fn bool_name() -> &'static str {
    if mode() == "macro_bool_duplicate" {
        "text"
    } else {
        "toggle"
    }
}
fn enum_name() -> &'static str {
    if mode() == "macro_enum_duplicate" {
        "text"
    } else {
        "choice"
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
        "F13 load began with live owners"
    );
    DROPS.store(0, Ordering::Relaxed);
    ISSUED.store(0, Ordering::Relaxed);
    RELEASED.store(0, Ordering::Relaxed);
    CLOSING.store(false, Ordering::Relaxed);
    if ctx
        .call("CONFIG", &["GET", "cfgregistration.text"])
        .is_err()
    {
        return Status::Err;
    }
    let mode = mode();
    if !mode.starts_with("direct_") {
        return Status::Ok;
    }
    CLOSING.store(true, Ordering::Relaxed);
    let duplicate = mode.ends_with("duplicate");
    let invalid_name = mode.ends_with("name");
    let result = if mode.starts_with("direct_numeric_") {
        register_i64_configuration(
            ctx,
            if duplicate {
                "number"
            } else if invalid_name {
                "bad.name"
            } else {
                "rejected"
            },
            &NUMBER_CONFIG,
            7,
            1,
            100,
            if mode.ends_with("flags") {
                ConfigurationFlags::BITFLAGS
            } else {
                ConfigurationFlags::DEFAULT
            },
            Some(changed("numeric-direct")),
        )
    } else if mode.starts_with("direct_bool_") {
        register_bool_configuration(
            ctx,
            if duplicate {
                "toggle"
            } else if invalid_name {
                "bad.name"
            } else {
                "rejected"
            },
            &TOGGLE,
            true,
            if mode.ends_with("flags") {
                ConfigurationFlags::MEMORY
            } else {
                ConfigurationFlags::DEFAULT
            },
            Some(changed("bool-direct")),
        )
    } else {
        register_enum_configuration(
            ctx,
            if duplicate {
                "choice"
            } else if invalid_name {
                "bad.name"
            } else {
                "rejected"
            },
            &CHOICE,
            Choice::One,
            if mode.ends_with("flags") {
                ConfigurationFlags::MEMORY
            } else {
                ConfigurationFlags::DEFAULT
            },
            Some(changed("enum-direct")),
        )
    };
    match result {
        Err(error) => {
            ctx.log_notice(&format!(
                "F13_REJECTED {mode} drops={} error={error}",
                DROPS.load(Ordering::Relaxed)
            ));
            Status::Err
        }
        Ok(()) => {
            ctx.log_warning("F13_UNEXPECTED_ACCEPTANCE");
            Status::Err
        }
    }
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
redis_module! {
    name: "cfgregistration", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], init: init, deinit: deinit,
    commands: [["cfgregistration.stats", stats, "", 0,0,0,""],],
    configurations: [
        i64: [[number_name(), &NUMBER_CONFIG, 7, 1, 100, number_flags(), Some(changed("numeric-macro"))],],
        string: [["text", &TEXT_CONFIG, "default", ConfigurationFlags::DEFAULT, None],],
        bool: [[bool_name(), &TOGGLE, true, ConfigurationFlags::DEFAULT, Some(changed("bool-macro"))],],
        enum: [[enum_name(), &CHOICE, Choice::One, ConfigurationFlags::DEFAULT, Some(changed("enum-macro"))],],
        module_args_as_configuration: true,
    ]
}
