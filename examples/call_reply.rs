//! Forwards native command replies through `Context::call`, so tests can check
//! the framing and bytes that reach the client.
use redis_module::{
    CallOptionResp, CallOptionsBuilder, CallReply, CallResult, Context, RedisError, RedisResult,
    RedisString, RedisValue, redis_module, redisvalue::RedisValueKey,
};

fn forward(ctx: &Context, args: Vec<RedisString>, command: &str) -> RedisResult {
    let args: Vec<_> = args.iter().skip(1).map(RedisString::as_slice).collect();
    ctx.call(command, args.as_slice())
}

fn echo(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "ECHO")
}

/// Classifies failed calls without decoding or copying the error message.
fn error_code(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let command = args.get(1).ok_or(RedisError::WrongArity)?.try_as_str()?;
    let call_args: Vec<_> = args.iter().skip(2).map(RedisString::as_slice).collect();
    let options = CallOptionsBuilder::new().errors_as_replies().build();
    let result: CallResult = ctx.call_ext(command, &options, call_args.as_slice());
    Ok(match result {
        Ok(_) => RedisValue::Null,
        Err(error) => RedisValue::StringBuffer(error.code().to_vec()),
    })
}

/// `call_reply.protocol key [2|3|auto ...]`: report HGETALL's native reply
/// kind after applying protocol selections in order. The label is a simple
/// string so both RESP2 and RESP3 clients can read it.
fn protocol(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let key = args.get(1).ok_or(RedisError::WrongArity)?;
    let mut options = CallOptionsBuilder::new().errors_as_replies();
    for selection in &args[2..] {
        options = options.resp(match selection.try_as_str()? {
            "2" => CallOptionResp::Resp2,
            "3" => CallOptionResp::Resp3,
            "auto" => CallOptionResp::Auto,
            _ => return Err(RedisError::Str("ERR expected 2, 3 or auto")),
        });
    }
    let reply: CallResult = ctx.call_ext("HGETALL", &options.build(), &[key]);
    match reply.map_err(RedisError::from)? {
        CallReply::Array(_) => Ok(RedisValue::SimpleStringStatic("array")),
        CallReply::Map(_) => Ok(RedisValue::SimpleStringStatic("map")),
        _ => Err(RedisError::Str("ERR unexpected HGETALL reply kind")),
    }
}

fn get(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "GET")
}

fn mget(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "MGET")
}

fn dump(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "DUMP")
}

fn eval(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "EVAL")
}

fn status(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "SET")
}

fn explicit_status(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// Calls `command` with RESP3 replies, converts the typed reply, and drops the
/// native reply before replying, so the result must own every copied string.
fn owned(ctx: &Context, args: Vec<RedisString>, command: &str) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    let options = CallOptionsBuilder::new()
        .resp(CallOptionResp::Resp3)
        .build();
    let reply: CallResult = ctx.call_ext(command, &options, &[args[1].as_slice()]);
    let reply = reply.map_err(RedisError::from)?;
    if !matches!(
        (command, &reply),
        ("HGETALL", CallReply::Map(_)) | ("SMEMBERS", CallReply::Set(_))
    ) {
        return Err(RedisError::Str("ERR unexpected native reply type"));
    }
    let value = RedisValue::from(&reply);
    drop(reply);
    let keys: Vec<&RedisValueKey> = match &value {
        RedisValue::Map(map) => map.keys().collect(),
        RedisValue::Set(set) => set.iter().collect(),
        _ => Vec::new(),
    };
    for key in keys {
        match key {
            RedisValueKey::String(_) => {}
            RedisValueKey::BulkString(bytes) if std::str::from_utf8(bytes).is_err() => {}
            _ => return Err(RedisError::Str("ERR unexpected string key variant")),
        }
    }
    Ok(value)
}

fn hgetall(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    owned(ctx, args, "HGETALL")
}

fn smembers(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    owned(ctx, args, "SMEMBERS")
}

fn version(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let version = ctx.get_redis_version_rm_call()?;
    Ok(vec![
        i64::from(version.major),
        i64::from(version.minor),
        i64::from(version.patch),
        i64::from(ctx.is_enterprise()),
    ]
    .into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "call_reply",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["call_reply.echo", echo, "readonly", 0, 0, 0, ""],
        ["call_reply.error_code", error_code, "", 0, 0, 0, ""],
        ["call_reply.protocol", protocol, "readonly", 1, 1, 1, ""],
        ["call_reply.get", get, "readonly", 1, 1, 1, ""],
        ["call_reply.mget", mget, "readonly", 1, -1, 1, ""],
        ["call_reply.dump", dump, "readonly", 1, 1, 1, ""],
        ["call_reply.status", status, "write", 1, 1, 1, ""],
        ["call_reply.explicit_status", explicit_status, "readonly", 0, 0, 0, ""],
        ["call_reply.hgetall", hgetall, "readonly", 1, 1, 1, ""],
        ["call_reply.smembers", smembers, "readonly", 1, 1, 1, ""],
        ["call_reply.version", version, "readonly", 0, 0, 0, ""],
        ["call_reply.eval", eval, "", 0, 0, 0, ""],
    ],
}
