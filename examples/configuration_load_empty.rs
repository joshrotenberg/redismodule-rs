//! Present-empty configurations block; absent-block control is the manual fixture.
use redis_module::{redis_module, Context, RedisString, Status};
fn init(ctx: &Context, _: &[RedisString]) -> Status {
    ctx.log_notice("F19_EMPTY_INIT");
    Status::Ok
}
redis_module! {
    name: "cfgempty",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    configurations: [i64: [], string: [], bool: [], enum: [], module_args_as_configuration: false,]
}
