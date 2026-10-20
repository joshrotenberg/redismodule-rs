use redis_module::{
    ArgCursor, Context, RedisError, RedisResult, RedisString, RedisValue, redis_module,
};

fn info_cmd(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() < 3 {
        return Err(RedisError::WrongArity);
    }

    let mut args = ArgCursor::new(&args[1..]);

    let section = args.next_str()?;
    let field = args.next_str()?;

    let server_info = ctx.server_info(section);
    Ok(server_info
        .field(field)
        .map_or(RedisValue::Null, RedisValue::BulkRedisString))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "info",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["infoex", info_cmd, "", 0, 0, 0, ""],
    ],
}
