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
static TEXT: Mutex<String> = Mutex::new(String::new());
static TOGGLE: AtomicBool = AtomicBool::new(false);
static CHOICE: Mutex<Choice> = Mutex::new(Choice::One);
static OBSERVED: RedisGILGuard<Vec<(String, i64, bool)>> = RedisGILGuard::new(Vec::new());
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

fn observer<T: 'static>() -> impl Fn(&ConfigurationContext, &str, &'static T) + Send {
    let registering_thread = thread::current().id();
    // Owned Cell is Send but not Sync. Redis serializes these invocations.
    let calls = Cell::new(0_i64);
    move |ctx, name, _| {
        calls.set(calls.get() + 1);
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
        load_configurations(ctx)
    })();
    if let Err(error) = result {
        ctx.log_warning(&error.to_string());
        return Status::Err;
    }
    OBSERVED.lock(ctx).clear();
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

fn worker(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    if ctx.get_flags().contains(ContextFlags::DENY_BLOCKING) {
        return Err(RedisError::Str(
            "ERR worker command requires a blockable client",
        ));
    }
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR finish the earlier worker first"));
    }
    let blocked = ctx.block_client();
    *worker = Some(thread::spawn(move || {
        let owner = ThreadSafeContext::with_blocked_client(blocked);
        let result = {
            let guard = owner.lock();
            guard
                .call(
                    "CONFIG",
                    &[
                        "SET",
                        "configuration_observers.number",
                        "11",
                        "configuration_observers.text",
                        "worker",
                        "configuration_observers.toggle",
                        "no",
                        "configuration_observers.choice",
                        "One",
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
        redis_command!("config_observers.worker", worker).deny_script(),
        redis_command!("config_observers.finish", finish),
    ],
}
