use redis_module::{redis_module, Context, NextArg, RedisResult, RedisString, RedisValue};

fn echo(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let value = args.next_arg()?;
    args.done()?;
    Ok(RedisValue::StringBuffer(value.as_slice().to_vec()))
}

redis_module! {
    name: "archive_manual",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["archive.echo", echo, "readonly", 0, 0, 0, ""],
    ],
}
