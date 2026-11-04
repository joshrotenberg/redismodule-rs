//! Logs a command argument through each logging entry point, so tests can
//! check the exact text that reaches the server log.
use redis_module::error::Error;
use redis_module::io::{RdbReader, RdbWriter};
use redis_module::logging::{self, RedisLogLevel};
use redis_module::native_types::{NativeType, RedisType};
use redis_module::{Context, RedisError, RedisResult, RedisString, Status, raw, redis_module};

fn init(_ctx: &redis_module::RegistrationContext<'_>, _args: &[RedisString]) -> Status {
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

/// A message logged through a real IO handle. It is never stored in a key:
/// `log.io` passes one to SaveDataTypeToString, which calls `rdb_save`
/// synchronously while the command owns the message.
struct LogMessage(String);

impl NativeType for LogMessage {
    fn rdb_load(_io: &mut RdbReader, _encver: i32) -> Result<Self, Error> {
        Err(Error::generic("log messages are never loaded"))
    }

    fn rdb_save(&self, io: &mut RdbWriter) {
        // SAFETY: io is the IO Redis passed to this rdb_save.
        unsafe {
            logging::log_io_error(
                io.as_ptr(),
                RedisLogLevel::Warning,
                &format!("log-io: {}", self.0),
            );
        }
        io.write_slice(self.0.as_bytes());
    }
}

static LOG_TYPE: RedisType<LogMessage> = RedisType::new("logmsg001", 0);

fn log_io(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    let mut message = LogMessage(args[1].try_as_str()?.to_owned());
    // SAFETY: the registered type invokes save_message synchronously; message
    // remains alive and is only borrowed by that callback. Redis owns the IO.
    let encoded = unsafe {
        raw::RedisModule_SaveDataTypeToString.unwrap()(
            ctx.get_raw(),
            (&mut message as *mut LogMessage).cast(),
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
