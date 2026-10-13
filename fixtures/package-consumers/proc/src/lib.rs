use redis_module::{redis_module, Context, NextArg, RedisError, RedisString};
use redis_module_macros::{command, RedisValue};

#[derive(RedisValue)]
struct Reply {
    count: i64,
    message: String,
}

// A keyless typed command, tested through the packaged public author interface.
#[command({name: "archive.typed", flags: [ReadOnly], arity: 2,
    summary: "Return the parsed integer and a fixed message.", key_spec: []})]
fn typed(_: &Context, args: Vec<RedisString>) -> Result<Reply, RedisError> {
    let mut args = args.into_iter().skip(1);
    let count = args.next_i64()?;
    args.done()?;
    Ok(Reply {
        count,
        message: "packaged macros".to_owned(),
    })
}

redis_module! {
    name: "archive_proc",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [],
}
