use redis_module::{
    ArgCursor, CommandFlags, Context, RedisError, RedisResult, RedisString, RedisValue, cluster,
    redis_command, redis_module,
};

fn keys_pos(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    // Number of args (excluding command name) must be even
    if !(args.len() - 1).is_multiple_of(2) {
        return Err(RedisError::WrongArity);
    }

    if ctx.is_keys_position_request() {
        for i in 1..args.len() {
            if (i - 1) % 2 == 0 {
                ctx.key_at_pos(i as i32);
            }
        }
        return Ok(RedisValue::NoReply);
    }

    let reply: Vec<_> = args.iter().skip(1).step_by(2).collect();

    Ok(reply.into())
}

/// `command_keys <command> [arg ...]`: the positions of the command's keys.
fn command_keys(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let keys = ctx.get_command_keys(&args[1..])?;
    Ok(RedisValue::Array(
        keys.into_iter()
            .map(|pos| RedisValue::Integer(pos as i64))
            .collect(),
    ))
}

/// `key_slot <key>`: the key's cluster hash slot.
fn key_slot(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let key = args.get(1).ok_or(RedisError::WrongArity)?;
    Ok(RedisValue::Integer(i64::from(cluster::key_slot(
        key.as_slice(),
    ))))
}

/// `key_for_slot <slot>`: an ASCII key for a cluster slot, without a server API.
fn key_for_slot(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let slot = args.next_u64()?;
    args.done()?;
    let key = u16::try_from(slot)
        .ok()
        .and_then(cluster::key_for_slot)
        .ok_or(RedisError::Str("slot must be between 0 and 16383"))?;
    Ok(key.into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "keys_pos",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["keys_pos", keys_pos, "getkeys-api", 1, 1, 1, ""],
        ["command_keys", command_keys, "", 0, 0, 0, ""],
        ["key_slot", key_slot, "", 0, 0, 0, ""],
        redis_command!("key_for_slot", key_for_slot).flags(CommandFlags::READ_ONLY),
    ],
}
