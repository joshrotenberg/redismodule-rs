use redis_module::{
    AclCategory, AclChannelPermissions, AclLogReason, AclPermissions, Context, NextArg, RedisError,
    RedisResult, RedisString, RedisValue, redis_module,
};

fn verify_key_access_for_user(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let user = args.next_arg()?;
    let key = args.next_arg()?;
    ctx.acl_check_key_permission(&user, &key, &AclPermissions::all())?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `verify_command_access_for_user user command [arg ...]`
fn verify_command_access_for_user(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let user = args.next_arg()?;
    let command: Vec<RedisString> = args.collect();
    ctx.acl_check_command_permission(&user, &command)?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `verify_channel_access_for_user user channel publish|subscribe|psubscribe`
fn verify_channel_access_for_user(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let user = args.next_arg()?;
    let channel = args.next_arg()?;
    let permissions = match args.next_string()?.as_str() {
        "publish" => AclChannelPermissions::PUBLISH,
        "subscribe" => AclChannelPermissions::SUBSCRIBE,
        "psubscribe" => AclChannelPermissions::SUBSCRIBE | AclChannelPermissions::PATTERN,
        _ => {
            return Err(RedisError::Str(
                "ERR expected publish, subscribe or psubscribe",
            ));
        }
    };
    args.done()?;
    ctx.acl_check_channel_permission(&user, &channel, permissions)?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `log_channel_denial user channel`
fn log_channel_denial(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let user = args.next_arg()?;
    let channel = args.next_arg()?;
    args.done()?;
    ctx.acl_add_log_entry(&user, &channel, AclLogReason::Channel)?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

fn get_current_user(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    Ok(ctx
        .get_current_user()
        .map_or(RedisValue::Null, RedisValue::BulkRedisString))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "acl",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    acl_categories: [AclCategory::from("acl"), ],
    commands: [
        ["verify_key_access_for_user", verify_key_access_for_user, "", 0, 0, 0, AclCategory::Read, AclCategory::from("acl")],
        ["get_current_user", get_current_user, "", 0, 0, 0, vec![AclCategory::Read, AclCategory::Fast], AclCategory::from("acl")],
        ["verify_command_access_for_user", verify_command_access_for_user, "", 0, 0, 0, AclCategory::Read, AclCategory::from("acl")],
        ["verify_channel_access_for_user", verify_channel_access_for_user, "", 0, 0, 0, AclCategory::Read, AclCategory::from("acl")],
        ["log_channel_denial", log_channel_denial, "", 0, 0, 0, AclCategory::Read, AclCategory::from("acl")],
    ],
}
