use redis_module::{
    Context, NextArg, RedisError, RedisResult, RedisString, RedisValue, redis_module,
    redisvalue::RedisValueKey,
};
use std::collections::{BTreeMap, BTreeSet};

fn map_mget(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() < 2 {
        return Err(RedisError::WrongArity);
    }

    let mut args = args.into_iter().skip(1);
    let key_name = args.next_arg()?;

    let fields: Vec<RedisString> = args.collect();

    let key = ctx.open_key(&key_name);
    let values = key.hash_get_multi(&fields)?;
    let res = match values {
        None => RedisValue::Null,
        Some(values) => {
            let mut map: BTreeMap<RedisValueKey, RedisValue> = BTreeMap::new();
            for (field, value) in values.into_iter() {
                map.insert(
                    RedisValueKey::BulkRedisString(field),
                    RedisValue::BulkRedisString(value),
                );
            }
            RedisValue::OrderedMap(map)
        }
    };

    Ok(res)
}

fn map_unique(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() < 2 {
        return Err(RedisError::WrongArity);
    }

    let mut args = args.into_iter().skip(1);
    let key_name = args.next_arg()?;

    let fields: Vec<RedisString> = args.collect();

    let key = ctx.open_key(&key_name);
    let values = key.hash_get_multi(&fields)?;
    let res = match values {
        None => RedisValue::Null,
        Some(values) => {
            let mut set: BTreeSet<RedisValueKey> = BTreeSet::new();
            for (_, value) in values.into_iter() {
                set.insert(RedisValueKey::BulkRedisString(value));
            }
            RedisValue::OrderedSet(set)
        }
    };

    Ok(res)
}

/// `map.hget key field`: one field through [`RedisKey::hash_get_by_string`], which takes the
/// field as the `RedisString` it already is instead of copying it into a C string.
fn map_hget(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 3 {
        return Err(RedisError::WrongArity);
    }

    let mut args = args.into_iter().skip(1);
    let key_name = args.next_arg()?;
    let field = args.next_arg()?;

    let key = ctx.open_key(&key_name);
    Ok(match key.hash_get_by_string(&field)? {
        None => RedisValue::Null,
        Some(value) => RedisValue::BulkRedisString(value),
    })
}

/// `map.hget_writable key field`: the same lookup through a writable handle
/// ([`RedisKeyWritable::hash_get_by_string`]).
fn map_hget_writable(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 3 {
        return Err(RedisError::WrongArity);
    }

    let mut args = args.into_iter().skip(1);
    let key_name = args.next_arg()?;
    let field = args.next_arg()?;

    let key = ctx.open_key_writable(&key_name);
    Ok(match key.hash_get_by_string(&field)? {
        None => RedisValue::Null,
        Some(value) => RedisValue::BulkRedisString(value),
    })
}

/// `map.ordered`: a map whose keys reply in the order they were added.
fn map_ordered(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::InsertionOrderedMap(
        [("zebra", 1), ("apple", 2), ("mango", 3)]
            .into_iter()
            .map(|(key, value)| (key.to_owned().into(), RedisValue::Integer(value)))
            .collect(),
    ))
}

/// `errors.in_array`: an array holding an error with a computed message.
fn errors_in_array(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let failed = 2;
    Ok(RedisValue::Array(vec![
        RedisValue::Integer(1),
        RedisValue::Error(format!("ERR item {failed} failed")),
        RedisValue::BulkString("three".to_owned()),
    ]))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "response",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["map.mget", map_mget, "readonly", 1, 1, 1, ""],
        ["map.unique", map_unique, "readonly", 1, 1, 1, ""],
        ["map.hget", map_hget, "readonly", 1, 1, 1, ""],
        ["map.hget_writable", map_hget_writable, "write", 1, 1, 1, ""],
        ["map.ordered", map_ordered, "readonly", 0, 0, 0, ""],
        ["errors.in_array", errors_in_array, "readonly", 0, 0, 0, ""],
    ],
}
