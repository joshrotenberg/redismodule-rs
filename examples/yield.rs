//! `yield.busy <ms> <events|clients>`: spins for `ms` milliseconds and yields
//! to Redis as it goes, so tests can check what other clients see meanwhile.
use std::time::{Duration, Instant};

use redis_module::{
    Context, NextArg, RedisError, RedisResult, RedisString, YieldMode, redis_module,
};

fn busy(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let millis = args.next_u64()?;
    let mode = match args.next_string()?.as_str() {
        "events" => YieldMode::Events,
        "clients" => YieldMode::Clients,
        _ => return Err(RedisError::Str("ERR expected events or clients")),
    };
    args.done()?;

    let end = Instant::now() + Duration::from_millis(millis);
    let mut yields: i64 = 0;
    while Instant::now() < end {
        ctx.yield_now(mode, Some("yield.busy is running"))?;
        yields += 1;
    }
    Ok(yields.into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "yield",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["yield.busy", busy, "", 0, 0, 0, ""],
    ],
}
