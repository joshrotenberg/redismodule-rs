//! Corrected LoadConfigs propagation in actual native macro OnLoad callbacks.
use redis_module::{
    configuration::{ConfigurationContext, ConfigurationFlags},
    enum_configuration, raw, redis_module, ConfigurationValue, Context, RedisError, RedisResult,
    RedisString, RedisValue, Status,
};
use std::sync::{
    atomic::{AtomicBool, AtomicI64, Ordering},
    Mutex,
};

static NUMBER: AtomicI64 = AtomicI64::new(7);
static TEXT: Mutex<String> = Mutex::new(String::new());
static GUARD: AtomicBool = AtomicBool::new(true);
static REFUSE: AtomicBool = AtomicBool::new(false);
enum_configuration! { enum Choice { One = 1, Two = 2, } }
static MODE: Mutex<Choice> = Mutex::new(Choice::One);
struct Text;
static TEXT_CONFIG: Text = Text;
struct Guard;
static GUARD_CONFIG: Guard = Guard;
fn log(message: &str) {
    raw::redis_log(std::ptr::null_mut(), message);
}
impl ConfigurationValue<RedisString> for Text {
    fn get(&self, _: &ConfigurationContext) -> RedisString {
        log("F19_GET text");
        RedisString::from_bytes(TEXT.lock().unwrap().as_bytes())
    }
    fn set(&self, _: &ConfigurationContext, value: RedisString) -> Result<(), RedisError> {
        log("F19_SET text");
        let text = value.try_as_str()?;
        if text == "reject" {
            return Err(RedisError::Str("F19 text rejected"));
        }
        *TEXT.lock().unwrap() = text.to_owned();
        Ok(())
    }
}
impl ConfigurationValue<bool> for Guard {
    fn get(&self, _: &ConfigurationContext) -> bool {
        GUARD.load(Ordering::Relaxed)
    }
    fn set(&self, _: &ConfigurationContext, value: bool) -> Result<(), RedisError> {
        log("F19_SET guard");
        if !value {
            return Err(RedisError::Str("F19 guarded boolean rejected"));
        }
        GUARD.store(value, Ordering::Relaxed);
        Ok(())
    }
}
fn init(ctx: &Context, _: &[RedisString]) -> Status {
    REFUSE.store(false, Ordering::Relaxed);
    ctx.log_notice("F19_MACRO_INIT");
    Status::Ok
}
fn deinit(ctx: &Context) -> Status {
    ctx.log_notice("F19_MACRO_DEINIT");
    if REFUSE.load(Ordering::Relaxed) {
        Status::Err
    } else {
        Status::Ok
    }
}
fn state(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(vec![
        NUMBER.load(Ordering::Relaxed).into(),
        TEXT.lock().unwrap().clone().into(),
        RedisValue::Integer(i64::from(GUARD.load(Ordering::Relaxed))),
        RedisValue::Integer(i64::from(i32::from(MODE.lock().unwrap().clone()))),
    ]))
}
fn phase(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    // A real command Context: native phase rejection, never a fabricated token.
    match redis_module::configuration::load_configurations(ctx) {
        Err(error) => Ok(error.to_string().into()),
        Ok(()) => Err(RedisError::Str(
            "LoadConfigs unexpectedly accepted command phase",
        )),
    }
}
fn refuse(_: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    REFUSE.store(args[1].as_slice() == b"yes", Ordering::Relaxed);
    Ok(RedisValue::SimpleStringStatic("OK"))
}
redis_module! {
    name: "loadconfigs",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    deinit: deinit,
    commands: [
        ["loadconfigs.state", state, "", 0,0,0,""],
        ["loadconfigs.phase", phase, "", 0,0,0,""],
        ["loadconfigs.refuse", refuse, "", 0,0,0,""],
    ],
    configurations: [
        i64: [["number", &NUMBER, 7, 1, 99, ConfigurationFlags::DEFAULT,
            Some(Box::new(|_,_,_| log("F19_CHANGED number")))]],
        string: [["text", &TEXT_CONFIG, "ready", ConfigurationFlags::DEFAULT, None]],
        bool: [["guard", &GUARD_CONFIG, true, ConfigurationFlags::DEFAULT, None]],
        enum: [["mode", &MODE, Choice::One, ConfigurationFlags::DEFAULT,
            Some(Box::new(|_,_,_| log("F19_CHANGED mode")))]],
        module_args_as_configuration: true,
    ]
}
