//! Reports the SDK's `KeyType` for a key, so tests can check the mapping from
//! `RedisModule_KeyType` against each type the server has.
use redis_module::{Context, RedisError, RedisResult, RedisString, redis_module};

/// `keytype.of <key>` returns the key's `KeyType` variant name.
fn key_type(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let [_, name] = args.as_slice() else {
        return Err(RedisError::WrongArity);
    };
    Ok(format!("{:?}", ctx.open_key(name).key_type()).into())
}

redis_module! {
    name: "key_type",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["keytype.of", key_type, "readonly", 1, 1, 1, ""],
    ],
}
