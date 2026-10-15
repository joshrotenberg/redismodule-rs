//! Corrected call-result conversion fixtures, used only in legal host callbacks.
use redis_module::{
    configuration::{ConfigurationContext, ConfigurationFlags},
    redis_module, ArgCursor, CallOptionResp, CallOptionsBuilder, CallReply, CallResult,
    ConfigurationValue, Context, RedisError, RedisResult, RedisString, RedisValue,
};
use std::sync::Mutex;

fn forward(ctx: &Context, args: Vec<RedisString>, command: &str) -> RedisResult {
    let bytes: Vec<_> = args.iter().skip(1).map(RedisString::as_slice).collect();
    ctx.call(command, bytes.as_slice())
}
fn echo(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "ECHO")
}
fn get(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "GET")
}
fn dump(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "DUMP")
}
fn mget(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "MGET")
}
fn status(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    forward(ctx, args, "SET")
}
fn explicit(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::SimpleStringStatic("OK"))
}

fn owned(ctx: &Context, args: Vec<RedisString>, command: &str) -> RedisResult {
    let mut cursor = ArgCursor::new(&args[1..]);
    let key = cursor.next_bytes()?;
    cursor.done()?;
    let options = CallOptionsBuilder::new()
        .resp(CallOptionResp::Resp3)
        .build();
    let reply: CallResult = ctx.call_ext(command, &options, &[key]);
    let reply = reply.map_err(RedisError::from)?;
    if !matches!(
        (command, &reply),
        ("GET", CallReply::String(_) | CallReply::Null(_))
            | ("HGETALL", CallReply::Map(_))
            | ("SMEMBERS", CallReply::Set(_))
    ) {
        return Err(RedisError::Str("ERR unexpected typed native reply family"));
    }
    let value = RedisValue::from(&reply);
    // The conversion must own every copied string, including nested map keys.
    drop(reply);
    match &value {
        RedisValue::Map(map) => {
            for key in map.keys() {
                check_key(key)?;
            }
        }
        RedisValue::Set(set) => {
            for key in set {
                check_key(key)?;
            }
        }
        _ => {}
    }
    Ok(value)
}
fn check_key(key: &redis_module::redisvalue::RedisValueKey) -> Result<(), RedisError> {
    use redis_module::redisvalue::RedisValueKey;
    match key {
        RedisValueKey::String(_) => Ok(()),
        RedisValueKey::BulkString(bytes) if std::str::from_utf8(bytes).is_err() => Ok(()),
        _ => Err(RedisError::Str("ERR unexpected copied string-key variant")),
    }
}
fn get_owned(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    owned(ctx, args, "GET")
}
fn map_owned(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    owned(ctx, args, "HGETALL")
}
fn set_owned(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
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

static TEXT: Mutex<String> = Mutex::new(String::new());
struct Text;
static TEXT_CONFIG: Text = Text;
impl ConfigurationValue<RedisString> for Text {
    fn get(&self, _: &ConfigurationContext) -> RedisString {
        // Text can include NUL: use binary creation, not a CString producer.
        RedisString::from_bytes(TEXT.lock().unwrap().as_bytes())
    }
    fn set(&self, _: &ConfigurationContext, value: RedisString) -> Result<(), RedisError> {
        *TEXT.lock().unwrap() = value.try_as_str()?.to_owned();
        Ok(())
    }
}

redis_module! {
    name: "callreply", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["callreply.echo", echo, "readonly", 0, 0, 0, ""],
        ["callreply.get", get, "readonly", 1, 1, 1, ""],
        ["callreply.dump", dump, "readonly", 1, 1, 1, ""],
        ["callreply.mget", mget, "readonly", 1, -1, 1, ""],
        ["callreply.status", status, "write", 1, 1, 1, ""],
        ["callreply.explicit", explicit, "readonly", 0, 0, 0, ""],
        ["callreply.owned", get_owned, "readonly", 1, 1, 1, ""],
        ["callreply.map", map_owned, "readonly", 1, 1, 1, ""],
        ["callreply.set", set_owned, "readonly", 1, 1, 1, ""],
        ["callreply.version", version, "readonly", 0, 0, 0, ""],
    ],
    configurations: [
        i64: [],
        string: [["text", &TEXT_CONFIG, "default", ConfigurationFlags::DEFAULT, None]],
        bool: [],
        enum: [],
        module_args_as_configuration: false,
        module_config_get: "callreply.cfgget",
    ]
}
