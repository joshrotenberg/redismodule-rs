use redis_module::{
    Context, NextArg, RedisError, RedisResult, RedisString, RedisValue, redis_module,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// `string.collection_lookup kind query [keys ...]`: looks up a byte slice in
/// native-string collections. Maps return the last matching input index or -1;
/// sets return 1 or 0. Keys and the query may contain arbitrary bytes.
fn collection_lookup(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let kind = args.next_string()?;
    let query = args.next_arg()?;
    let keys: Vec<_> = args.collect();
    let found = match kind.as_str() {
        "hash_set" => {
            let keys: HashSet<_> = keys.into_iter().collect();
            i64::from(keys.contains(query.as_slice()))
        }
        "ordered_set" => {
            let keys: BTreeSet<_> = keys.into_iter().collect();
            i64::from(keys.contains(query.as_slice()))
        }
        "hash_map" => {
            let keys: HashMap<_, _> = keys
                .into_iter()
                .enumerate()
                .map(|(index, key)| (key, index as i64))
                .collect();
            keys.get(query.as_slice()).copied().unwrap_or(-1)
        }
        "ordered_map" => {
            let keys: BTreeMap<_, _> = keys
                .into_iter()
                .enumerate()
                .map(|(index, key)| (key, index as i64))
                .collect();
            keys.get(query.as_slice()).copied().unwrap_or(-1)
        }
        _ => {
            return Err(RedisError::Str(
                "ERR expected hash_set, ordered_set, hash_map or ordered_map",
            ));
        }
    };
    Ok(found.into())
}

/// `string.collection_order [keys ...]`: RedisString byte order as raw bytes.
fn collection_order(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let keys: BTreeSet<_> = args.into_iter().skip(1).collect();
    Ok(keys
        .into_iter()
        .map(|key| RedisValue::StringBuffer(key.as_slice().to_vec()))
        .collect::<Vec<_>>()
        .into())
}

fn string_set(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() < 3 {
        return Err(RedisError::WrongArity);
    }

    let mut args = args.into_iter().skip(1);
    let key_name = args.next_arg()?;
    let value = args.next_arg()?;

    let mut key = ctx.open_key_writable(&key_name);
    // The closure borrows the argument's bytes, not the RedisString, which
    // is not Send.
    let bytes = value.as_slice();
    key.with_string_dma(|data| {
        data.write(bytes)
            .map(|_| RedisValue::SimpleStringStatic("OK"))
    })?
}

fn string_get(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() < 2 {
        return Err(RedisError::WrongArity);
    }

    let mut args = args.into_iter().skip(1);
    let key_name = args.next_arg()?;

    let key = ctx.open_key(&key_name);
    let res = key
        .read()?
        .map_or(RedisValue::Null, RedisValue::StringBuffer);
    Ok(res)
}

/// `string.reply simple|error text`: replies with `text` as a simple string or
/// as an error.
fn string_reply(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let kind = args.next_string()?;
    let text = args.next_string()?;
    args.done()?;
    match kind.as_str() {
        "simple" => Ok(RedisValue::SimpleString(text)),
        "error" => Err(RedisError::String(text)),
        _ => Err(RedisError::Str("ERR expected simple or error")),
    }
}

/// `string.create_nul`: creates strings with an interior NUL byte.
fn string_create_nul(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(vec![
        RedisValue::BulkRedisString(ctx.create_string(b"a\0b".to_vec())),
        RedisValue::BulkRedisString(RedisString::create(None, "c\0d")),
    ]))
}

/// `string.del key` and `string.unlink key`: 1 if the key held a value.
fn string_del(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter();
    let command = args.next_string()?;
    let key = args.next_arg()?;
    args.done()?;
    let key = ctx.open_key_writable(&key);
    let deleted = if command.eq_ignore_ascii_case("string.unlink") {
        key.unlink()?
    } else {
        key.delete()?
    };
    Ok(RedisValue::Integer(i64::from(deleted)))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "string",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["string.set", string_set, "write fast deny-oom", 1, 1, 1, ""],
        ["string.get", string_get, "readonly", 1, 1, 1, ""],
        ["string.del", string_del, "write", 1, 1, 1, ""],
        ["string.unlink", string_del, "write", 1, 1, 1, ""],
        ["string.reply", string_reply, "", 0, 0, 0, ""],
        ["string.create_nul", string_create_nul, "", 0, 0, 0, ""],
        ["string.collection_lookup", collection_lookup, "readonly", 0, 0, 0, ""],
        ["string.collection_order", collection_order, "readonly", 0, 0, 0, ""],
    ],
}
