//! Key handles that outlive a keyspace change: each command opens a handle,
//! changes the key another way (a second handle, a call, a scan), and then
//! uses the first handle again. The SDK closes open keys before each change
//! and re-opens them on their next use, so every handle sees the key as a
//! fresh open would, instead of the value Redis freed.
use redis_module::raw::KeyType;
use redis_module::{
    Context, KeysCursor, NextArg, RedisResult, RedisString, RedisValue, ScanKeyCursor, redis_module,
};

fn key_type_name(key_type: KeyType) -> &'static str {
    match key_type {
        KeyType::Empty => "none",
        KeyType::String => "string",
        KeyType::List => "list",
        KeyType::Hash => "hash",
        KeyType::Stream => "stream",
        _ => "other",
    }
}

fn read_reply(value: Option<Vec<u8>>) -> RedisValue {
    value.map_or(RedisValue::Null, RedisValue::StringBuffer)
}

/// `stale.two_reads key`: reads an int- or embstr-encoded string through two
/// handles. String DMA through the first replaces the value the second opened.
fn two_reads(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_arg()?;
    args.done()?;
    let first = ctx.open_key(&name);
    let second = ctx.open_key(&name);
    let a = first.read()?;
    let b = second.read()?;
    Ok(RedisValue::Array(vec![read_reply(a), read_reply(b)]))
}

/// `stale.call_then_read key command [arg ...]`: opens `key`, runs the
/// command, and reads the key through the handle: its type and string value.
fn call_then_read(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_arg()?;
    let command = args.next_string()?;
    let rest: Vec<RedisString> = args.collect();
    let key = ctx.open_key(&name);
    let before = key_type_name(key.key_type());
    let rest: Vec<&RedisString> = rest.iter().collect();
    ctx.call(&command, rest.as_slice())?;
    let after = key_type_name(key.key_type());
    let value = if key.key_type() == KeyType::String {
        read_reply(key.read()?)
    } else {
        RedisValue::Null
    };
    Ok(RedisValue::Array(vec![before.into(), after.into(), value]))
}

/// `stale.del_then_write key value`: opens `key` for writing, deletes it with
/// DEL, and writes `value` through the handle, which creates the key again.
fn del_then_write(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_arg()?;
    let value = args.next_string()?;
    args.done()?;
    let key = ctx.open_key_writable(&name);
    ctx.call("DEL", &[&name])?;
    let after_del = key_type_name(key.key_type());
    key.write(&value)?;
    Ok(RedisValue::Array(vec![
        after_del.into(),
        key_type_name(key.key_type()).into(),
    ]))
}

/// `stale.pop_other key`: pops the only element of a list through one handle,
/// which deletes the key, and reads its type through another.
fn pop_other(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_arg()?;
    args.done()?;
    let reader = ctx.open_key(&name);
    let before = key_type_name(reader.key_type());
    let writer = ctx.open_key_writable(&name);
    let popped = writer.list_pop_head();
    Ok(RedisValue::Array(vec![
        before.into(),
        popped.map_or(RedisValue::Null, RedisValue::BulkRedisString),
        key_type_name(reader.key_type()).into(),
    ]))
}

/// `stale.other_db key value`: opens `key` in the current database, selects
/// database 1, sets `key` there, and reads the first handle, which re-opens
/// in its own database. Replies with that read and the selected database.
fn other_db(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_arg()?;
    let value = args.next_arg()?;
    args.done()?;
    let original = ctx.get_selected_db();
    let key = ctx.open_key(&name);
    ctx.select_db(1)?;
    let result = ctx.call("SET", &[&name, &value]);
    let read = key.read();
    let selected = ctx.get_selected_db();
    ctx.select_db(original)?;
    result?;
    Ok(RedisValue::Array(vec![
        read_reply(read?),
        i64::from(selected).into(),
    ]))
}

/// `stale.stream_call key`: iterates a stream and runs PING in the loop body;
/// the iterator's next step panics.
fn stream_call(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_arg()?;
    args.done()?;
    let key = ctx.open_key(&name);
    let mut entries: i64 = 0;
    for _ in key.get_stream_iterator(false)? {
        entries += 1;
        ctx.call("PING", &[] as &[&str])?;
    }
    Ok(entries.into())
}

/// `stale.scan_del key`: scans a hash and tries DEL on it from the callback,
/// which the SDK refuses while Redis scans the key. Replies with the fields
/// seen and the first refusal.
fn scan_del(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let name = args.next_arg()?;
    args.done()?;
    let cursor = ScanKeyCursor::new(ctx.open_key(&name));
    let mut fields: i64 = 0;
    let mut refusal = None;
    cursor.for_each(|_, _, _| {
        fields += 1;
        if let Err(error) = ctx.call("DEL", &[&name]) {
            refusal.get_or_insert(error.to_string());
        }
    });
    Ok(RedisValue::Array(vec![
        fields.into(),
        refusal.map_or(RedisValue::Null, RedisValue::BulkString),
    ]))
}

/// `stale.keys_scan_call other`: scans the keyspace, and from the callback
/// sets `other` and then uses the key handle Redis lent the callback, which
/// panics.
fn keys_scan_call(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let other = args.next_arg()?;
    args.done()?;
    let cursor = KeysCursor::new();
    while cursor.scan(ctx, &mut |ctx, _, key| {
        let _ = ctx.call("SET", &[&other, &other]);
        if let Some(key) = key {
            let _ = key.key_type();
        }
    }) {}
    Ok("done".into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "stale_keys",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["stale.two_reads", two_reads, "write", 1, 1, 1, ""],
        ["stale.call_then_read", call_then_read, "write", 1, 1, 1, ""],
        ["stale.del_then_write", del_then_write, "write", 1, 1, 1, ""],
        ["stale.pop_other", pop_other, "write", 1, 1, 1, ""],
        ["stale.other_db", other_db, "write", 1, 1, 1, ""],
        ["stale.stream_call", stream_call, "readonly", 1, 1, 1, ""],
        ["stale.scan_del", scan_del, "write", 1, 1, 1, ""],
        ["stale.keys_scan_call", keys_scan_call, "write", 1, 1, 1, ""],
    ],
}
