//! Fluent and legacy command definitions may share the same module.
use redis_module::{
    AclCategory, CommandFlags, Context, KeyRange, RedisError, RedisResult, RedisString, RedisValue,
    Status, redis_command, redis_module,
};

fn callback(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok("PONG".into())
}
fn invoke(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok("PONG".into())
}

fn ping(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok("PONG".into())
}

fn get(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let key = args.get(1).ok_or(RedisError::WrongArity)?;
    ctx.call("GET", &[key])
}

fn keys(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if ctx.is_keys_position_request() {
        for position in (1..args.len()).step_by(2) {
            ctx.key_at_pos(position as i32);
        }
        return Ok(RedisValue::NoReply);
    }
    Ok(args.iter().skip(1).step_by(2).collect::<Vec<_>>().into())
}

fn panic_command(_: &Context, _: Vec<RedisString>) -> RedisResult {
    panic!("builder command panicked");
}

fn init(ctx: &Context, args: &[RedisString]) -> Status {
    let Some(mode) = args.first() else {
        return Status::Ok;
    };
    let command = redis_command!("builder.invalid", ping);
    let command = match mode.try_as_str().unwrap() {
        "key-zero" => command.key(0),
        "key-negative" => command.key(-1),
        "step-zero" => command.keys(KeyRange::new(1).last(-1).step(0)),
        "step-negative" => command.keys(KeyRange::new(1).last(-1).step(-2)),
        "last-before-first" => command.keys(KeyRange::new(3).last(2)),
        "last-zero" => command.keys(KeyRange::new(1).last(0)),
        "unknown-flags" => command.flags(CommandFlags::from_bits_retain(1 << 31)),
        "required-acl" => command.acl_categories("nonexistent_builder_category"),
        "optional-empty" => command.optional_acl_categories(""),
        "nul-acl" => command.acl_categories("read\0write"),
        "empty-name" => redis_command!("", ping),
        "nul-name" => redis_command!("builder\0invalid", ping),
        "duplicate" => redis_command!("builder.ping", ping),
        _ => return Status::Err,
    };
    match command.register(ctx) {
        Ok(()) => Status::Ok,
        Err(error) => {
            ctx.log_warning(&format!("builder load rejected: {error}"));
            Status::Err
        }
    }
}

redis_module! {
    name: "command_builder",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        ["builder.legacy", ping, "readonly fast", 0, 0, 0, ""],
        ["builder.legacy_callback", callback, "", 0, 0, 0, ""],
        ["builder.legacy_keyless", ping, "", 0, -1, 0, ""],
        redis_command!("builder.callback", callback),
        redis_command!("builder.invoke", invoke),
        redis_command!("builder.ping", ping).read_only().fast(),
        redis_command!("builder.get", get)
            .flags(CommandFlags::READ_ONLY | CommandFlags::DENY_SCRIPT)
            .fast().key(1),
        redis_command!("builder.keys", keys)
            .read_only().keys(KeyRange::new(1).last(-1).step(2)),
        ["builder.legacy_after", ping, "readonly", 0, 0, 0, ""],
        ["builder.legacy_acl", ping, "", 0, 0, 0, AclCategory::Read, "nonexistent_builder_category"],
        [&b"builder.binary_\xff"[..], ping, "readonly", 0, 0, 0, ""],
        redis_command!("builder.dynamic", keys).flags(CommandFlags::GETKEYS_API),
        redis_command!("builder.acl", ping).read_only()
            .acl_categories(AclCategory::Read)
            .optional_acl_categories("nonexistent_builder_category"),
        redis_command!("builder.optional", ping)
            .optional_acl_categories("nonexistent_builder_category"),
        redis_command!("builder.panic", panic_command),
    ],
}
