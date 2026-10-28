//! Replies of the RESP3 types. Redis before 7.0 has no module API for them, so
//! there the SDK sends what Redis 7 sends a RESP2 client.

use redis_module::{
    Context, NextArg, RedisResult, RedisString, RedisValue, redis_module, redisvalue::RedisValueKey,
};
use std::collections::BTreeSet;

/// `reply.bool`: true and false.
fn reply_bool(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(vec![
        RedisValue::Bool(true),
        RedisValue::Bool(false),
    ]))
}

/// `reply.big_number`: a number too large for an integer reply.
fn reply_big_number(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::BigNumber(
        "1234567890123456789012345678901234567890".to_owned(),
    ))
}

/// `reply.verbatim format text`: the text with a three-letter format.
fn reply_verbatim(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let format = args.next_string()?;
    let text = args.next_arg()?;
    args.done()?;
    // The format converts from a three-letter ASCII string.
    Ok(RedisValue::VerbatimString((
        format.as_str().try_into()?,
        text.as_slice().to_vec(),
    )))
}

/// `reply.set`: a set with two strings and a boolean.
fn reply_set(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::OrderedSet(BTreeSet::from([
        RedisValueKey::String("a".to_owned()),
        RedisValueKey::String("b".to_owned()),
        RedisValueKey::Bool(true),
    ])))
}

/// `reply.user`: the calling client's user, or nil where the server has no
/// way to tell (Redis 6.0).
fn reply_user(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(ctx
        .get_current_user()
        .map_or(RedisValue::Null, RedisValue::BulkRedisString))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "reply_types",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["reply.bool", reply_bool, "readonly", 0, 0, 0, ""],
        ["reply.big_number", reply_big_number, "readonly", 0, 0, 0, ""],
        ["reply.verbatim", reply_verbatim, "readonly", 0, 0, 0, ""],
        ["reply.set", reply_set, "readonly", 0, 0, 0, ""],
        ["reply.user", reply_user, "readonly", 0, 0, 0, ""],
    ],
}
