use redis_module::{redis_module, Context, NextArg, RedisError, RedisString};
use redis_module_macros::{command, RedisValue};

#[derive(RedisValue)]
struct Reply {
    count: i64,
    message: String,
}

// This fixture checks the public author interface at compile time. Runtime
// coverage for proc-macro command metadata is a separate baseline gate.
#[command({name: "archive.typed", flags: [ReadOnly], arity: 2, key_spec: []})]
fn typed(_: &Context, args: Vec<RedisString>) -> Result<Reply, RedisError> {
    let mut args = args.into_iter().skip(1);
    let count = args.next_i64()?;
    args.done()?;
    Ok(Reply {
        count,
        message: "packaged macros".to_owned(),
    })
}

redis_module! {
    name: "archive_proc",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [],
}
