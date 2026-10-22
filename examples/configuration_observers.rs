//! Observers execute on the setter's thread, with captured Send, non-Sync state.
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::thread::{self, JoinHandle};

use redis_module::configuration::{
    ConfigurationContext, ConfigurationFlags, load_configurations, register_bool_configuration,
    register_enum_configuration, register_i64_configuration, register_string_configuration,
};
use redis_module::{
    Context, ContextFlags, EventContext, RedisError, RedisGILGuard, RedisResult, RedisString,
    RedisValue, RegistrationContext, Status, ThreadSafeContext, enum_configuration, redis_command,
    redis_module,
};

enum_configuration! {
    enum Choice {
        One = 1,
        Two = 2,
    }
}

static NUMBER: AtomicI64 = AtomicI64::new(0);
static TEXT: RedisGILGuard<Vec<u8>> = RedisGILGuard::new(Vec::new());
static BINARY: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static TOGGLE: AtomicBool = AtomicBool::new(false);
static CHOICE: Mutex<Choice> = Mutex::new(Choice::One);
static OBSERVED: RedisGILGuard<Vec<(String, i64, bool)>> = RedisGILGuard::new(Vec::new());
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static SHARED_CALLS: RedisGILGuard<Cell<i64>> = RedisGILGuard::new(Cell::new(0));

fn observer<T: 'static>() -> impl Fn(&ConfigurationContext, &str, &'static T) + Send {
    let registering_thread = thread::current().id();
    // Owned Cell is Send but not Sync. Redis serializes these invocations.
    let calls = Cell::new(0_i64);
    move |ctx, name, _| {
        calls.set(calls.get() + 1);
        let total = SHARED_CALLS.lock(ctx);
        total.set(total.get() + 1);
        OBSERVED.lock(ctx).push((
            name.to_owned(),
            calls.get(),
            thread::current().id() == registering_thread,
        ));
    }
}

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    let result = (|| {
        register_i64_configuration(
            ctx,
            "number",
            &NUMBER,
            0,
            0,
            100,
            ConfigurationFlags::DEFAULT,
            Some(Box::new(observer())),
        )?;
        register_string_configuration(
            ctx,
            "text",
            &TEXT,
            "",
            ConfigurationFlags::DEFAULT,
            Some(Box::new(observer())),
        )?;
        register_bool_configuration(
            ctx,
            "toggle",
            &TOGGLE,
            false,
            ConfigurationFlags::DEFAULT,
            Some(Box::new(observer())),
        )?;
        register_enum_configuration(
            ctx,
            "choice",
            &CHOICE,
            Choice::One,
            ConfigurationFlags::DEFAULT,
            Some(Box::new(observer())),
        )?;
        register_string_configuration(
            ctx,
            "binary",
            &BINARY,
            "seed",
            ConfigurationFlags::DEFAULT,
            None,
        )?;
        load_configurations(ctx)
    })();
    if let Err(error) = result {
        ctx.log_warning(&error.to_string());
        return Status::Err;
    }
    OBSERVED.lock(ctx).clear();
    SHARED_CALLS.lock(ctx).set(0);
    Status::Ok
}

fn observed(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(
        OBSERVED
            .lock(ctx)
            .iter()
            .map(|(name, count, main_thread)| {
                RedisValue::Array(vec![
                    name.clone().into(),
                    (*count).into(),
                    i64::from(*main_thread).into(),
                ])
            })
            .collect(),
    ))
}

fn shared_calls(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(SHARED_CALLS.lock(ctx).get().into())
}

fn worker(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() > 2 {
        return Err(RedisError::WrongArity);
    }
    let bytes = args
        .get(1)
        .map_or_else(|| b"worker".to_vec(), |arg| arg.as_slice().to_vec());
    if ctx.get_flags().contains(ContextFlags::DENY_BLOCKING) {
        return Err(RedisError::Str(
            "ERR worker command requires a blockable client",
        ));
    }
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR finish the earlier worker first"));
    }
    let blocked = ctx.block_client()?;
    *worker = Some(thread::spawn(move || {
        let owner = ThreadSafeContext::with_blocked_client(blocked);
        let result = {
            let guard = owner.lock();
            guard
                .call(
                    "CONFIG",
                    &[
                        b"SET".as_slice(),
                        b"configuration_observers.number",
                        b"11",
                        b"configuration_observers.text",
                        bytes.as_slice(),
                        b"configuration_observers.toggle",
                        b"no",
                        b"configuration_observers.choice",
                        b"One",
                        b"configuration_observers.binary",
                        bytes.as_slice(),
                    ],
                )
                .map(|_| "OK".into())
        };
        owner.reply(result);
    }));
    Ok(RedisValue::NoReply)
}

fn join_finished_worker() -> Result<bool, RedisError> {
    let mut worker = WORKER.lock().unwrap();
    if worker.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return Ok(false);
    }
    if let Some(handle) = worker.take() {
        handle
            .join()
            .map_err(|_| RedisError::Str("ERR worker panicked"))?;
    }
    Ok(true)
}

fn finish(_: &Context, _: Vec<RedisString>) -> RedisResult {
    join_finished_worker().map(|done| i64::from(done).into())
}

fn deinit(_: &EventContext) -> Status {
    // Never wait for a worker that could still need the Redis lock.
    match join_finished_worker() {
        Ok(true) => Status::Ok,
        _ => Status::Err,
    }
}

redis_module! {
    name: "configuration_observers", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], init: init, deinit: deinit,
    commands: [
        redis_command!("config_observers.observed", observed).read_only(),
        redis_command!("config_observers.shared_calls", shared_calls).read_only(),
        redis_command!("config_observers.worker", worker).deny_script(),
        redis_command!("config_observers.finish", finish),
    ],
}
