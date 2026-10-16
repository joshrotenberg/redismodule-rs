//! Corrected single-field hash fixture; all owners remain in the command phase.
use redis_module::{redis_module, Context, RedisError, RedisResult, RedisString, RedisValue};

fn mutation(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() < 4 {
        return Err(RedisError::WrongArity);
    }
    let mode = args[1].try_as_str()?;
    let name = &args[2];
    let field = &args[3];
    match mode {
        "set" | "text-set" => {
            if args.len() != 5 {
                return Err(RedisError::WrongArity);
            }
            let value = &args[4];
            let (count, copy) = {
                let key = ctx.open_key_writable(name);
                let count = if mode == "set" {
                    key.hash_set_by_string(field, value)?
                } else {
                    key.hash_set(field.try_as_str()?, value.clone())?
                };
                let copy = key
                    .hash_get_by_string(field)?
                    .ok_or(RedisError::Str("missing stored value"))?;
                (count, copy)
            }; // CloseKey before reading the owned getter result, in this phase.
            Ok(RedisValue::Array(vec![
                RedisValue::Integer(count as i64),
                RedisValue::StringBuffer(copy.as_slice().to_vec()),
                RedisValue::StringBuffer(field.as_slice().to_vec()),
                RedisValue::StringBuffer(value.as_slice().to_vec()),
            ]))
        }
        "del" | "text-del" => {
            if args.len() != 4 {
                return Err(RedisError::WrongArity);
            }
            let key = ctx.open_key_writable(name);
            let count = if mode == "del" {
                key.hash_del_by_string(field)?
            } else {
                key.hash_del(field.try_as_str()?)?
            };
            Ok(RedisValue::Integer(count as i64))
        }
        _ => Err(RedisError::Str("unknown hash fixture mode")),
    }
}
redis_module! {
    name: "hash_mutation",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [["hashmutation.run", mutation, "write", 2, 2, 1, ""]],
}
