//! Commands that set and delete hash fields through `RedisKeyWritable`, and
//! reply with the count each call returns.
use redis_module::{redis_module, Context, NextArg, RedisResult, RedisString, RedisValue};

fn count(count: usize) -> RedisResult {
    Ok(RedisValue::Integer(count as i64))
}

/// `hash.set key field value`
fn set(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    let field = args.next_string()?;
    let value = args.next_arg()?;
    args.done()?;
    count(ctx.open_key_writable(&key).hash_set(&field, value)?)
}

/// `hash.del key field`
fn del(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    let field = args.next_string()?;
    args.done()?;
    count(ctx.open_key_writable(&key).hash_del(&field)?)
}

/// `hash.set_bytes key field value`: the field may hold any bytes.
fn set_bytes(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    let field = args.next_arg()?;
    let value = args.next_arg()?;
    args.done()?;
    count(
        ctx.open_key_writable(&key)
            .hash_set_by_string(&field, &value)?,
    )
}

/// `hash.del_bytes key field`: the field may hold any bytes.
fn del_bytes(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    let field = args.next_arg()?;
    args.done()?;
    count(ctx.open_key_writable(&key).hash_del_by_string(&field)?)
}

//////////////////////////////////////////////////////

redis_module! {
    name: "hash",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["hash.set", set, "write", 1, 1, 1, ""],
        ["hash.del", del, "write", 1, 1, 1, ""],
        ["hash.set_bytes", set_bytes, "write", 1, 1, 1, ""],
        ["hash.del_bytes", del_bytes, "write", 1, 1, 1, ""],
    ],
}
