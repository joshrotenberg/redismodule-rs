use redis_module::InfoContext;
use redis_module::{
    CallOptionsBuilder, CallReply, CallResult, Context, ErrorReply, RedisError, RedisResult,
    RedisString, RedisValue, redis_module, redisvalue::RedisValueKey,
};

fn test_helper_version(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    let ver = ctx.get_redis_version()?;
    let response: Vec<i64> = vec![ver.major.into(), ver.minor.into(), ver.patch.into()];

    Ok(response.into())
}

fn test_helper_version_rm_call(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    let ver = ctx.get_redis_version_rm_call()?;
    let response: Vec<i64> = vec![ver.major.into(), ver.minor.into(), ver.patch.into()];

    Ok(response.into())
}

fn test_helper_command_name(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    Ok(ctx.current_command_name()?.into())
}

/// `test_helper.err message`: replies with `message` as an error, written
/// directly with `reply_error_string`. The message is the whole error, so it
/// starts with its code, such as `ERR`. Bytes that are not UTF-8 become
/// U+FFFD, and CR, LF and NUL become spaces.
fn test_helper_err(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let [_, message] = args.as_slice() else {
        return Err(RedisError::WrongArity);
    };
    ctx.reply_error_string(&message.to_string_lossy());
    // The error is the reply; any other value would be a second one.
    Ok(RedisValue::NoReply)
}

fn test_helper_dbsize(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Integer(ctx.dbsize() as i64))
}

/// Exercises command reply ownership without raw reply calls.
fn test_helper_reply_guard(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let [_, mode] = args.as_slice() else {
        return Err(RedisError::WrongArity);
    };
    match mode.try_as_str()? {
        "return" => return Ok(RedisValue::SimpleStringStatic("first")),
        "direct" => {
            ctx.reply_simple_string("first");
            return Ok(RedisValue::NoReply);
        }
        "value" => {
            ctx.reply(Ok(RedisValue::SimpleStringStatic("first")));
        }
        "key" => {
            ctx.reply_with_key(RedisValueKey::SimpleString("first".into()));
        }
        "error" => {
            ctx.reply_simple_string("first");
            return Err(RedisError::Str("ERR second"));
        }
        "wrongarity" => {
            ctx.reply_simple_string("first");
            return Err(RedisError::WrongArity);
        }
        "twice" => {
            ctx.reply_simple_string("first");
            ctx.reply_simple_string("second");
            return Ok(RedisValue::NoReply);
        }
        "panic" => {
            ctx.reply_simple_string("first");
            panic!("reply guard probe");
        }
        "released" => {
            drop(ctx.block_client()?);
            ctx.reply_simple_string("first");
            return Ok(RedisValue::NoReply);
        }
        "forward" => {
            let reply: CallResult =
                ctx.call_ext("ECHO", &CallOptionsBuilder::new().build(), &["first"]);
            ctx.reply_with_call_reply(&reply)?;
        }
        "forward-only" => {
            let reply: CallResult =
                ctx.call_ext("ECHO", &CallOptionsBuilder::new().build(), &["first"]);
            ctx.reply_with_call_reply(&reply)?;
            return Ok(RedisValue::NoReply);
        }
        "forward-twice" => {
            let reply: CallResult =
                ctx.call_ext("ECHO", &CallOptionsBuilder::new().build(), &["first"]);
            ctx.reply_with_call_reply(&reply)?;
            ctx.reply_with_call_reply(&reply)?;
        }
        "forward-message" => {
            let reply = Err(ErrorReply::Message("ERR first".into()));
            ctx.reply_with_call_reply(&reply)?;
        }
        "forward-fail" => {
            ctx.reply_with_call_reply(&Ok(CallReply::Unknown))?;
        }
        "error-direct" => {
            ctx.reply_error_string("ERR first");
        }
        "nested" => return ctx.call("test_helper.reply_guard", &["direct"]),
        "no-reply-first" => {
            ctx.reply(Ok(RedisValue::NoReply));
            return Ok(RedisValue::SimpleStringStatic("first"));
        }
        "array" => {
            ctx.reply(Ok(RedisValue::Array(vec![
                RedisValue::Integer(1),
                RedisValue::NoReply,
                RedisValue::Array(vec![]),
            ])));
        }
        "array-return" => {
            return Ok(RedisValue::Array(vec![
                RedisValue::Integer(1),
                RedisValue::NoReply,
                RedisValue::Array(vec![]),
            ]));
        }
        _ => return Err(RedisError::Str("ERR unknown reply guard mode")),
    }
    Ok(RedisValue::SimpleStringStatic("second"))
}

fn test_helper_milliseconds(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    Ok(redis_module::milliseconds().into())
}

fn test_helper_milliseconds_worker(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    // The clock needs no context or GIL. Join before returning so no worker
    // remains active when the module can be unloaded.
    let now = std::thread::spawn(redis_module::milliseconds)
        .join()
        .map_err(|_| RedisError::Str("clock worker panicked"))?;
    Ok(now.into())
}

fn add_info(ctx: &InfoContext, _for_crash_report: bool) {
    if let Ok(section) = ctx
        .builder()
        .add_section("test_helper")
        .field("field", "value")
    {
        let Ok(section) = section.build_section() else {
            return;
        };
        let _ = section.build_info();
    }
}

//////////////////////////////////////////////////////

redis_module! {
    name: "test_helper",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    info: add_info,
    commands: [
        ["test_helper.version", test_helper_version, "", 0, 0, 0, ""],
        ["test_helper._version_rm_call", test_helper_version_rm_call, "", 0, 0, 0, ""],
        ["test_helper.name", test_helper_command_name, "", 0, 0, 0, ""],
        ["test_helper.err", test_helper_err, "", 0, 0, 0, ""],
        ["test_helper.dbsize", test_helper_dbsize, "readonly", 0, 0, 0, ""],
        ["test_helper.reply_guard", test_helper_reply_guard, "", 0, 0, 0, ""],
        ["test_helper.milliseconds", test_helper_milliseconds, "readonly fast", 0, 0, 0, ""],
        ["test_helper.milliseconds_worker", test_helper_milliseconds_worker, "readonly", 0, 0, 0, ""],
    ],
}
