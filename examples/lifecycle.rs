//! Redis 7.0+: reload configuration and reset transient state on every load.
//! See docs/UPGRADING-MODULES.md. No workers, timers or retained callbacks.
use redis_module::{
    Context, RedisGILGuard, RedisResult, RedisString, RedisValue, RegistrationContext, Status,
    configuration::ConfigurationFlags, redis_command, redis_module,
};
use std::sync::atomic::{AtomicI64, Ordering};

static PREFIX: RedisGILGuard<String> = RedisGILGuard::new(String::new());
static LIMIT: AtomicI64 = AtomicI64::new(0);
static VISITS: RedisGILGuard<i64> = RedisGILGuard::new(0);

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    // Configurations have already been applied. Reset application state even
    // if the platform loader retained the image from an earlier module load.
    *VISITS.lock(ctx) = 0;
    Status::Ok
}

fn state(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(vec![
        PREFIX.lock(ctx).clone().into(),
        LIMIT.load(Ordering::Relaxed).into(),
        (*VISITS.lock(ctx)).into(),
    ]))
}

fn visit(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut visits = VISITS.lock(ctx);
    *visits = visits.saturating_add(1);
    Ok((*visits).into())
}

redis_module! {
    name: "lifecycle",
    version: redis_module::module_version!(),
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        redis_command!("lifecycle.state", state).read_only(),
        redis_command!("lifecycle.visit", visit).read_only(),
    ],
    configurations: [
        i64: [
            ["limit", &LIMIT, 4, 1, 64, ConfigurationFlags::DEFAULT, None],
        ],
        string: [
            ["prefix", &PREFIX, "items:", ConfigurationFlags::IMMUTABLE, None],
        ],
        module_args_as_configuration: true,
    ]
}
