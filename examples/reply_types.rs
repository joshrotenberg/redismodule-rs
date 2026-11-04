//! Replies of the RESP3 types. Redis before 7.0 has no module API for them, so
//! there the SDK sends what Redis 7 sends a RESP2 client.

use redis_module::{
    Context, NextArg, RedisError, RedisResult, RedisString, RedisValue, redis_module,
    redisvalue::RedisValueKey,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// `reply.nulls bulk|array|empty|nested`: distinguish RESP2 null types and
/// empty arrays, including null arrays inside arrays and each map form.
fn reply_nulls(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let kind = args.next_string()?;
    args.done()?;
    let key = || RedisValueKey::String("key".to_owned());
    match kind.as_str() {
        "bulk" => Ok(RedisValue::Null),
        "array" => Ok(RedisValue::NullArray),
        "empty" => Ok(RedisValue::Array(vec![])),
        "nested" => Ok(RedisValue::Array(vec![
            RedisValue::Null,
            RedisValue::NullArray,
            RedisValue::Array(vec![]),
            RedisValue::Map(HashMap::from([(key(), RedisValue::NullArray)])),
            RedisValue::OrderedMap(BTreeMap::from([(key(), RedisValue::NullArray)])),
            RedisValue::InsertionOrderedMap(vec![(key(), RedisValue::NullArray)]),
            RedisValue::SimpleStringStatic("after"),
        ])),
        _ => Err(RedisError::Str("ERR expected bulk, array, empty or nested")),
    }
}

/// `reply.key integer|string|redis_string|bytes|true|false|simple`: one
/// reply of that `RedisValueKey` kind, sent with `Context::reply_with_key`.
fn reply_key(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let kind = args.next_string()?;
    args.done()?;
    let key = match kind.as_str() {
        "integer" => RedisValueKey::Integer(42),
        "string" => RedisValueKey::String("text".to_owned()),
        "redis_string" => RedisValueKey::BulkRedisString(ctx.create_string("owned")),
        "bytes" => RedisValueKey::BulkString(b"\0\xff!".to_vec()),
        "true" => RedisValueKey::Bool(true),
        "false" => RedisValueKey::Bool(false),
        "simple" => RedisValueKey::SimpleString("ok".to_owned()),
        _ => {
            return Err(RedisError::Str(
                "ERR expected integer, string, redis_string, bytes, true, false or simple",
            ));
        }
    };
    ctx.reply_with_key(key);
    Ok(RedisValue::NoReply)
}

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

/// `reply.big_number_text text [ALONE]`: the text as a big number, then
/// `after`, or the big number alone. Text that is not a decimal integer makes
/// the big number an error reply.
fn reply_big_number_text(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let text = args.next_string()?;
    let alone = match args.next() {
        Some(flag) if flag.as_slice().eq_ignore_ascii_case(b"alone") => true,
        Some(_) => return Err(RedisError::Str("ERR syntax error")),
        None => false,
    };
    args.done()?;
    if alone {
        return Ok(RedisValue::BigNumber(text));
    }
    Ok(RedisValue::Array(vec![
        RedisValue::BigNumber(text),
        RedisValue::SimpleStringStatic("after"),
    ]))
}

/// `reply.nested_no_reply`: `NoReply` as an array element and as a value of
/// each kind of map, where it is replied as null.
fn reply_nested_no_reply(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let key = || RedisValueKey::String("key".to_owned());
    Ok(RedisValue::Array(vec![
        RedisValue::Integer(1),
        RedisValue::NoReply,
        RedisValue::Map(HashMap::from([(key(), RedisValue::NoReply)])),
        RedisValue::OrderedMap(BTreeMap::from([(key(), RedisValue::NoReply)])),
        RedisValue::InsertionOrderedMap(vec![(key(), RedisValue::NoReply)]),
    ]))
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
        ["reply.nulls", reply_nulls, "readonly", 0, 0, 0, ""],
        ["reply.bool", reply_bool, "readonly", 0, 0, 0, ""],
        ["reply.key", reply_key, "readonly", 0, 0, 0, ""],
        ["reply.big_number", reply_big_number, "readonly", 0, 0, 0, ""],
        ["reply.big_number_text", reply_big_number_text, "readonly", 0, 0, 0, ""],
        ["reply.nested_no_reply", reply_nested_no_reply, "readonly", 0, 0, 0, ""],
        ["reply.verbatim", reply_verbatim, "readonly", 0, 0, 0, ""],
        ["reply.set", reply_set, "readonly", 0, 0, 0, ""],
        ["reply.user", reply_user, "readonly", 0, 0, 0, ""],
    ],
}
