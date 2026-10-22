//! A constant UTF-8 module name exceeding the old 64-byte initialization buffer.
use redis_module::{Context, RedisResult, RedisString, redis_command, redis_module};

const MODULE_NAME: &str = concat!(
    "sdk_module_name_",
    "0123456789abcdef0123456789abcdef",
    "0123456789abcdef0123456789abcdef",
    "0123456789abcdef0123456789abcdef",
    "_café",
);

fn name(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(MODULE_NAME.into())
}

redis_module! {
    name: MODULE_NAME,
    version: redis_module::module_version!(),
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [redis_command!("module_name.name", name).read_only()],
}
