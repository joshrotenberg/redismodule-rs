//! Manual registration in init, alongside declarative command definitions.
use redis_module::{
    Context, ModuleOptions, RedisError, RedisResult, RedisString, RegistrationContext, Status,
    redis_command, redis_module,
};

fn ping(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.call("PING", &[] as &[&str])
}

// Helpers shared by commands, init, events, and workers take the shared view.
fn check_runtime(ctx: &redis_module::RuntimeContext) -> Result<(), RedisError> {
    ctx.call("PING", &[] as &[&str])?;
    ctx.log_notice("registration runtime helper succeeded");
    Ok(())
}

// Runtime-only hooks also accept the shared view through coercion.
fn runtime_init(ctx: &redis_module::RuntimeContext, _: &[RedisString]) -> Status {
    ctx.log_notice("registration runtime init hook succeeded");
    Status::Ok
}

fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    let result = (|| {
        ctx.set_module_options(ModuleOptions::HANDLE_IO_ERRORS);
        check_runtime(ctx)?;
        let name = args
            .first()
            .map_or(b"registration.manual".as_slice(), RedisString::as_slice);
        redis_command!(name, ping).read_only().fast().register(ctx)
    })();
    match result {
        Ok(()) => Status::Ok,
        Err(error) => {
            ctx.log_warning(&error.to_string());
            Status::Err
        }
    }
}

redis_module! {
    name: "registration",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    init: runtime_init,
    commands: [
        ["registration.legacy", ping, "readonly fast", 0, 0, 0, ""],
        redis_command!("registration.builder", ping).read_only().fast(),
    ],
}
