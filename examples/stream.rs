use redis_module::raw::{KeyType, RedisModuleStreamID};
use redis_module::{
    Context, NextArg, RedisError, RedisResult, RedisString, RedisValue, redis_module,
};

fn stream_read_from(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);

    let stream_key = args.next_arg()?;

    // End the iterator and the read key before opening the key for writing.
    let (element, id_to_keep) = {
        let stream = ctx.open_key(&stream_key);
        let key_type = stream.key_type();

        if key_type != KeyType::Stream {
            return Err(RedisError::WrongType);
        }

        let mut iter = stream.get_stream_iterator(false)?;
        let element = iter.next();
        let id_to_keep = iter.next().as_ref().map_or_else(
            || RedisModuleStreamID {
                ms: u64::MAX,
                seq: u64::MAX,
            },
            |e| e.id,
        );
        (element, id_to_keep)
    };

    // An empty stream has nothing to trim.
    let Some(element) = element else {
        return Ok(RedisValue::Null);
    };
    let stream = ctx.open_key_writable(&stream_key);
    stream.trim_stream_by_id(id_to_keep, false)?;
    Ok(RedisValue::BulkString(format!(
        "{}-{}",
        element.id.ms, element.id.seq
    )))
}

/// Reads the first entry with one iterator, drops it, and reads the first
/// entry again with a second iterator on the same key.
fn stream_first_twice(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let stream_key = args.next_arg()?;
    args.done()?;

    let stream = ctx.open_key(&stream_key);
    if stream.key_type() != KeyType::Stream {
        return Err(RedisError::WrongType);
    }
    let first_id = || -> RedisResult {
        let mut iter = stream.get_stream_iterator(false)?;
        Ok(iter.next().map_or(RedisValue::Null, |e| {
            RedisValue::BulkString(format!("{}-{}", e.id.ms, e.id.seq))
        }))
    };
    Ok(RedisValue::Array(vec![first_id()?, first_id()?]))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "stream",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["STREAM_POP", stream_read_from, "write", 1, 1, 1, ""],
        ["STREAM_FIRST_TWICE", stream_first_twice, "readonly", 1, 1, 1, ""],
    ],
}
