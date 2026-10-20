//! Logs a command argument through each logging entry point, so tests can
//! check the exact text that reaches the server log.
use redis_module::logging::{self, RedisLogLevel};
use redis_module::native_types::RedisType;
use redis_module::{Context, RedisError, RedisResult, RedisString, Status, raw, redis_module};
use std::os::raw::c_void;

fn init(_ctx: &Context, _args: &[RedisString]) -> Status {
    // The load callback's context is temporary. The persistent log facade uses
    // context-free logging; each command's Context is used only for that command.
    // SAFETY: a null context is always valid.
    match unsafe { logging::setup_for_context(std::ptr::null_mut()) } {
        Ok(()) => Status::Ok,
        Err(_) => Status::Err,
    }
}

fn log_message(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    let message = args[1].try_as_str()?;
    ctx.log_notice(&format!("log-context: {message}"));
    logging::log_notice(format!("log-global: {message}"));
    // SAFETY: the command's own context, during the command.
    unsafe { raw::redis_log(ctx.get_raw(), &format!("log-raw: {message}")) };
    log::info!("log-standard: {message}");
    Ok("Logged".into())
}

// This type is never stored in a key. SaveDataTypeToString supplies a real IO
// handle and calls this callback synchronously while the command owns its String.
unsafe extern "C" fn save_message(io: *mut raw::RedisModuleIO, value: *mut c_void) {
    unsafe {
        let message = &*value.cast::<String>();
        logging::log_io_error(io, RedisLogLevel::Warning, &format!("log-io: {message}"));
        raw::RedisModule_SaveStringBuffer.unwrap()(io, message.as_ptr().cast(), message.len());
    }
}

static LOG_TYPE: RedisType = RedisType::new(
    "logmsg001",
    0,
    raw::RedisModuleTypeMethods {
        version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
        rdb_save: Some(save_message),
        rdb_load: None,
        aof_rewrite: None,
        free: None,
        mem_usage: None,
        digest: None,
        aux_load: None,
        aux_save: None,
        aux_save2: None,
        aux_save_triggers: 0,
        free_effort: None,
        unlink: None,
        copy: None,
        defrag: None,
        copy2: None,
        free_effort2: None,
        mem_usage2: None,
        unlink2: None,
    },
);

fn log_io(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    let mut message = args[1].try_as_str()?.to_owned();
    // SAFETY: the registered type invokes save_message synchronously; message
    // remains alive and is only borrowed by that callback. Redis owns the IO.
    let encoded = unsafe {
        raw::RedisModule_SaveDataTypeToString.unwrap()(
            ctx.get_raw(),
            (&mut message as *mut String).cast(),
            LOG_TYPE.raw_type(),
        )
    };
    if encoded.is_null() {
        return Err(RedisError::Str("Could not serialize the log fixture"));
    }
    // SAFETY: encoded is the Redis-owned string returned above; free it once.
    unsafe { raw::RedisModule_FreeString.unwrap()(ctx.get_raw(), encoded) };
    Ok("Logged IO".into())
}

redis_module! {
    name: "log",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [LOG_TYPE],
    init: init,
    commands: [
        ["log.message", log_message, "readonly", 0, 0, 0, ""],
        ["log.io", log_io, "readonly", 0, 0, 0, ""],
    ],
}
