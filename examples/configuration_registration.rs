//! Registers configurations from `init` according to the module argument, so
//! tests can load it with `MODULE LOAD` and check how registration errors are
//! reported:
//!
//! - `valid` registers one configuration of each type.
//! - `i64`, `string`, `bool` or `enum` registers a configuration of that type
//!   twice under the same name; Redis rejects the second one.
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::Mutex;

use redis_module::{
    configuration::{
        register_bool_configuration, register_enum_configuration, register_i64_configuration,
        register_string_configuration, ConfigurationFlags,
    },
    enum_configuration, raw, redis_module, Context, RedisError, RedisString, Status,
};

enum_configuration! {
    enum Choice {
        One = 1,
        Two = 2,
    }
}

static NUMBER: AtomicI64 = AtomicI64::new(0);
static TOGGLE: AtomicBool = AtomicBool::new(false);
static CHOICE: Mutex<Choice> = Mutex::new(Choice::One);
static TEXT: Mutex<String> = Mutex::new(String::new());

fn register(ctx: &Context, kind: &str, name: &str) -> Result<(), RedisError> {
    let flags = ConfigurationFlags::DEFAULT;
    match kind {
        "i64" => register_i64_configuration(ctx, name, &NUMBER, 7, 0, 100, flags, None),
        "string" => register_string_configuration(ctx, name, &TEXT, "default", flags, None),
        "bool" => register_bool_configuration(ctx, name, &TOGGLE, true, flags, None),
        "enum" => register_enum_configuration(ctx, name, &CHOICE, Choice::One, flags, None),
        _ => Err(RedisError::String(format!(
            "unknown configuration type {kind}"
        ))),
    }
}

/// Applies the registered configurations. Redis requires this after the last
/// registration, and the `redis_module!` call to it runs before `init`.
fn load_configs(ctx: &Context) -> Result<(), RedisError> {
    let load_configs = unsafe { raw::RedisModule_LoadConfigs }
        .ok_or(RedisError::Str("LoadConfigs is unavailable"))?;
    match unsafe { load_configs(ctx.get_raw()) } {
        status if status == raw::REDISMODULE_OK as i32 => Ok(()),
        _ => Err(RedisError::Str("LoadConfigs failed")),
    }
}

fn init(ctx: &Context, args: &[RedisString]) -> Status {
    let result = match args.first().map(RedisString::try_as_str) {
        Some(Ok("valid")) => ["i64", "string", "bool", "enum"]
            .into_iter()
            .try_for_each(|kind| register(ctx, kind, kind))
            .and_then(|()| load_configs(ctx)),
        // LoadConfigs runs here too, so a rejection that was not reported would
        // let the module load and the test would see it.
        Some(Ok(kind)) => register(ctx, kind, "duplicate")
            .and_then(|()| register(ctx, kind, "duplicate"))
            .and_then(|()| load_configs(ctx)),
        _ => Err(RedisError::Str("expected a mode argument")),
    };
    match result {
        Ok(()) => Status::Ok,
        Err(error) => {
            ctx.log_warning(&error.to_string());
            Status::Err
        }
    }
}

//////////////////////////////////////////////////////

redis_module! {
    name: "configuration_registration",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
}
