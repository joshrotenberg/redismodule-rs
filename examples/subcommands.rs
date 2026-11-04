//! A command container with native subcommands, as `XGROUP` has:
//! `subcmd.kv set key value`, `subcmd.kv get key` and `subcmd.kv panic`.
//! Redis passes the container and subcommand names as the first two
//! arguments. The subcommands' key specs tell Redis how each one uses its
//! key, which `%R~` and `%W~` ACL key patterns check, and `subcmd.documented`
//! sets every field of the command info. Containers and command info need
//! Redis 7.0 or later; this example's ACL categories need 7.2.
use redis_module::commands::{
    BeginSearch, CommandArgFlags, FindKeys, KeySpec, KeySpecFlags, RedisModuleCommandArg,
};
use redis_module::{
    CommandBuilder, Context, KeyRange, NextArg, RedisResult, RedisString, redis_command,
    redis_module,
};

/// A key spec for the single key at `index`, with `flags`.
fn key_at(index: i32, flags: KeySpecFlags) -> KeySpec {
    KeySpec::new(
        None,
        flags,
        BeginSearch::new_index(index),
        FindKeys::new_range(0, 1, 0),
    )
}

/// `subcmd.kv set key value`
fn set(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(2);
    let key = args.next_arg()?;
    let value = args.next_arg()?;
    args.done()?;
    ctx.call("SET", &[&key, &value])
}

/// `subcmd.kv get key`
fn get(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(2);
    let key = args.next_arg()?;
    args.done()?;
    ctx.call("GET", &[&key])
}

/// `subcmd.documented key`: reads `key`, as `GET` does.
fn documented(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    args.done()?;
    ctx.call("GET", &[&key])
}

/// `subcmd.kv panic`: a subcommand gets the same panic guard as a command.
fn panics(_: &Context, _: Vec<RedisString>) -> RedisResult {
    panic!("subcommand failed");
}

//////////////////////////////////////////////////////

redis_module! {
    name: "subcommands",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        CommandBuilder::container("subcmd.kv")
            .summary("Reads and writes string keys")
            .subcommand(
                redis_command!("set", set)
                    .write()
                    .keys(KeyRange::new(2))
                    .arity(4)
                    .summary("Sets a key")
                    .complexity("O(1)")
                    .since("1.0.0")
                    .key_spec(key_at(2, KeySpecFlags::OVERWRITE | KeySpecFlags::UPDATE))
                    .acl_categories("write"),
            )
            .subcommand(
                redis_command!("get", get)
                    .read_only()
                    .keys(KeyRange::new(2))
                    .arity(3)
                    .summary("Gets a key")
                    .complexity("O(1)")
                    .since("1.0.0")
                    .key_spec(key_at(2, KeySpecFlags::READ_ONLY | KeySpecFlags::ACCESS))
                    .acl_categories("read"),
            )
            .subcommand(redis_command!("panic", panics).arity(2)),
        redis_command!("subcmd.documented", documented)
            .read_only()
            .keys(KeyRange::new(1))
            .arity(2)
            .summary("Reads a key")
            .complexity("O(1)")
            .since("1.0.0")
            .history("1.1.0", "Reads the value as a copy")
            .history("1.2.0", "Refuses extra arguments")
            .tip("nondeterministic_output")
            .tip("request_policy:all_shards")
            .key_spec(KeySpec::new(
                Some("the key it reads".to_owned()),
                KeySpecFlags::READ_ONLY | KeySpecFlags::ACCESS,
                BeginSearch::new_index(1),
                FindKeys::new_range(0, 1, 0),
            ))
            .arg(RedisModuleCommandArg::new(
                "key".to_owned(),
                redis_module::raw::RedisModuleCommandArgType_REDISMODULE_ARG_TYPE_KEY,
                Some(0),
                None,
                Some("The key to read".to_owned()),
                Some("1.0.0".to_owned()),
                CommandArgFlags::NONE,
                None,
                None,
                None,
            )),
    ],
}
