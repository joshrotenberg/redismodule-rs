use redis_module::{
    ArgCursor, CommandFlags, Context, RedisError, RedisResult, RedisString, RedisValue, cluster,
    redis_command, redis_module,
};
use std::sync::{Mutex, PoisonError};

fn keys_pos(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    // Number of args (excluding command name) must be even
    if !(args.len() - 1).is_multiple_of(2) {
        return Err(RedisError::WrongArity);
    }

    if ctx.is_keys_position_request() {
        for i in 1..args.len() {
            if (i - 1) % 2 == 0 {
                ctx.key_at_pos(i as i32)?;
            }
        }
        return Ok(RedisValue::NoReply);
    }

    let reply: Vec<_> = args.iter().skip(1).step_by(2).collect();

    Ok(reply.into())
}

/// What `key_at_pos` returned for the position past the arguments in the last
/// `keys_pos.overreach` keys request.
static OVERREACH: Mutex<String> = Mutex::new(String::new());

/// `keys_pos.overreach key [arg ...]`: reports its first argument as a key and
/// then a position past its arguments, which `key_at_pos` refuses;
/// `keys_pos.overreach_result` returns that refusal.
fn overreach(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if ctx.is_keys_position_request() {
        ctx.key_at_pos(1)?;
        let past_the_end = i32::try_from(args.len())?;
        *OVERREACH.lock().unwrap_or_else(PoisonError::into_inner) =
            match ctx.key_at_pos(past_the_end) {
                Ok(()) => "OK".to_owned(),
                Err(error) => error.to_string(),
            };
        return Ok(RedisValue::NoReply);
    }
    Ok(RedisValue::SimpleStringStatic("OK"))
}

fn overreach_result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::SimpleString(
        OVERREACH
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    ))
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
        ["keys_pos.overreach", overreach, "getkeys-api", 1, 1, 1, ""],
        ["keys_pos.overreach_result", overreach_result, "readonly", 0, 0, 0, ""],
        ["command_keys", command_keys, "", 0, 0, 0, ""],
        ["key_slot", key_slot, "", 0, 0, 0, ""],
        redis_command!("key_for_slot", key_for_slot).flags(CommandFlags::READ_ONLY),
    ],
}
