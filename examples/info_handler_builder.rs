use redis_module::InfoContext;
use redis_module::{Context, RedisError, RedisResult, RedisString, redis_command, redis_module};
use redis_module_macros::info_command_handler;
use std::sync::atomic::{AtomicU64, Ordering};

// This is a telemetry counter, independent of Redis-protected state. Relaxed
// ordering is enough for one scalar; it does not publish other data.
static RECORDED: AtomicU64 = AtomicU64::new(0);

fn record(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    RECORDED.fetch_add(1, Ordering::Relaxed);
    Ok("OK".into())
}

fn snapshot(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    // GetServerInfo can invoke our INFO handler inside this command too.
    // The handler still only receives an InfoContext, not this command's view.
    Ok(ctx
        .server_info("info_handler_builder")
        .field("info_handler_builder_recorded")
        .into())
}

#[info_command_handler]
fn add_info(ctx: &InfoContext, for_crash_report: bool) -> RedisResult<()> {
    // Skip ordinary state reads and output construction in the crash path.
    // This is not a guarantee that Redis or the SDK is async-signal-safe.
    if for_crash_report {
        return Ok(());
    }

    ctx.builder()
        .add_section("info")
        .field("field", "value")?
        .field("recorded", RECORDED.load(Ordering::Relaxed))?
        .add_dictionary("dictionary")
        .field("key", "value")?
        .build_dictionary()?
        .build_section()?
        .build_info()?;

    Ok(())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "info_handler_builder",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        redis_command!("info_handler_builder.record", record).read_only(),
        redis_command!("info_handler_builder.snapshot", snapshot).read_only(),
    ],
}
