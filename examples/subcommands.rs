//! A command container with native subcommands, as `XGROUP` has:
//! `subcmd.kv set key value`, `subcmd.kv get key` and `subcmd.kv panic`.
//! Redis passes the container and subcommand names as the first two
//! arguments. Containers need Redis 7.0 or later.
use redis_module::{
    CommandBuilder, Context, KeyRange, NextArg, RedisResult, RedisString, redis_command,
    redis_module,
};

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
            .arity(-2)
            .subcommand(
                redis_command!("set", set)
                    .write()
                    .keys(KeyRange::new(2))
                    .arity(4)
                    .summary("Sets a key")
                    .acl_categories("write"),
            )
            .subcommand(
                redis_command!("get", get)
                    .read_only()
                    .keys(KeyRange::new(2))
                    .arity(3)
                    .summary("Gets a key")
                    .acl_categories("read"),
            )
            .subcommand(redis_command!("panic", panics).arity(2)),
    ],
}
