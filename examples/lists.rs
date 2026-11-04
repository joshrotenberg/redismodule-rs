//! List commands over the key API: `LPOPRPUSH source destination` and
//! `LISTS.PUSHHEAD key element [element ...]`. Writes through the key API are
//! not propagated to replicas or the AOF on their own, so each command that
//! writes replicates itself verbatim.
use redis_module::raw::KeyType;
use redis_module::{
    Context, NextArg, RedisError, RedisResult, RedisString, RedisValue, Status,
    key::RedisKeyWritable, redis_module,
};

/// Refuses a key that holds something other than a list.
fn check_list(key: &RedisKeyWritable) -> Result<(), RedisError> {
    match key.key_type() {
        KeyType::Empty | KeyType::List => Ok(()),
        _ => Err(RedisError::WrongType),
    }
}

fn pushed(status: Status) -> Result<(), RedisError> {
    match status {
        Status::Ok => Ok(()),
        Status::Err => Err(RedisError::Str("ERR the list push failed")),
    }
}

// LPOPRPUSH source destination
// Pops and returns the first element (head) of the list stored at 'source'
// and pushes the element to the last position (tail) of the list stored at
// 'destination'.
fn lpoprpush(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let src = args.next_arg()?;
    let dst = args.next_arg()?;
    args.done()?;

    // One key: rotate it through a single handle. With a second handle on the
    // same key, popping the last element deletes the key and leaves that
    // handle pointing at the freed list. Rotating a one-element list this way
    // deletes and recreates the key, so unlike LMOVE it drops the key's TTL.
    if src.as_slice() == dst.as_slice() {
        let key = ctx.open_key_writable(&src);
        check_list(&key)?;
        return match key.list_pop_head() {
            None => Ok(RedisValue::Null),
            Some(value) => {
                let reply = value.clone();
                pushed(key.list_push_tail(value))?;
                ctx.replicate_verbatim();
                Ok(RedisValue::BulkRedisString(reply))
            }
        };
    }

    let src_key = ctx.open_key_writable(&src);
    let dst_key = ctx.open_key_writable(&dst);
    check_list(&src_key)?;
    check_list(&dst_key)?;
    match src_key.list_pop_head() {
        None => Ok(RedisValue::Null),
        Some(value) => {
            let reply = value.clone();
            pushed(dst_key.list_push_tail(value))?;
            ctx.replicate_verbatim();
            Ok(RedisValue::BulkRedisString(reply))
        }
    }
}

// LISTS.PUSHHEAD key element [element ...]
// Pushes each element to the head of the list stored at 'key', as LPUSH does,
// so the last element given ends up first.
fn push_head(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_arg()?;
    let elements: Vec<RedisString> = args.collect();
    if elements.is_empty() {
        return Err(RedisError::WrongArity);
    }
    let key = ctx.open_key_writable(&name);
    check_list(&key)?;
    let count = elements.len();
    for element in elements {
        pushed(key.list_push_head(element))?;
    }
    ctx.replicate_verbatim();
    Ok(RedisValue::Integer(count as i64))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "lists",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["LPOPRPUSH", lpoprpush, "write fast deny-oom", 1, 2, 1, ""],
        ["LISTS.PUSHHEAD", push_head, "write fast deny-oom", 1, 1, 1, ""],
    ],
}
