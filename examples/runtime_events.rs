//! Choose subscriptions at load time or from a module command.
use redis_module::{
    Context, NotifyEvent, RedisError, RedisResult, RedisString, RedisValue, Status, redis_command,
    redis_event_handler, redis_module,
};
use std::sync::Mutex;

#[derive(Default)]
struct Events {
    counts: [i64; 5],
    last: Option<(String, Vec<u8>, i32)>,
}
static WORKER_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static EVENTS: Mutex<Events> = Mutex::new(Events {
    counts: [0; 5],
    last: None,
});

// The name also checks that the generated adapter does not shadow its handler.
fn invoke(ctx: &redis_module::EventContext, event_type: NotifyEvent, event: &str, key: &[u8]) {
    if key == b"runtime:panic" {
        panic!("runtime keyspace handler panicked");
    }
    let mut events = EVENTS.lock().unwrap();
    for (index, flag) in [
        NotifyEvent::STRING,
        NotifyEvent::MISSED,
        NotifyEvent::NEW,
        NotifyEvent::GENERIC,
        NotifyEvent::LOADED,
    ]
    .into_iter()
    .enumerate()
    {
        if event_type.contains(flag) {
            events.counts[index] += 1;
        }
    }
    // Keep owned copies, never the callback's borrowed pointers.
    events.last = Some((event.to_owned(), key.to_vec(), ctx.get_selected_db()));
}

fn mask(value: &str) -> Result<NotifyEvent, RedisError> {
    let mut mask = NotifyEvent::empty();
    for flag in value.split(',') {
        mask |= match flag {
            "none" => NotifyEvent::empty(),
            "string" => NotifyEvent::STRING,
            "missed" => NotifyEvent::MISSED,
            "new" => NotifyEvent::NEW,
            "generic" => NotifyEvent::GENERIC,
            "loaded" => NotifyEvent::LOADED,
            "unsupported" => NotifyEvent::from_bits_retain(1 << 30),
            _ => return Err(RedisError::Str("ERR unknown event mask")),
        };
    }
    Ok(mask)
}

fn init(ctx: &redis_module::RegistrationContext<'_>, args: &[RedisString]) -> Status {
    let result = (|| {
        if let Some(value) = args.first() {
            ctx.subscribe_to_keyspace_events(
                mask(value.try_as_str()?)?,
                redis_event_handler!(invoke),
            )?;
        }
        Ok::<_, RedisError>(())
    })();
    match result {
        Ok(()) => Status::Ok,
        Err(error) => {
            ctx.log_warning(&error.to_string());
            Status::Err
        }
    }
}

fn subscribe(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    ctx.subscribe_to_keyspace_events(mask(args[1].try_as_str()?)?, redis_event_handler!(invoke))?;
    Ok("OK".into())
}

#[redis_module_macros::command({ flags: [], arity: 2, key_spec: [] })]
fn runtime_subscribe_proc(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    subscribe(ctx, args)
}

// Shared helpers require no command/client capabilities.
fn worker_runtime(ctx: &redis_module::RuntimeContext) -> RedisResult {
    let selected = ctx.get_selected_db();
    ctx.with_db(3, |ctx| -> RedisResult {
        ctx.call("SET", &["worker:runtime", "value"])?;
        let name = ctx.create_string("worker:runtime");
        let value = ctx.open_key(&name).read()?.unwrap().to_vec();
        let cursor = redis_module::KeysCursor::new();
        let mut found = false;
        let mut visit = |ctx: &redis_module::RuntimeContext,
                         name: &RedisString,
                         _: Option<&redis_module::key::RedisKey>| {
            assert_eq!(ctx.get_selected_db(), 3);
            found |= name.as_slice() == b"worker:runtime";
        };
        while cursor.scan(ctx, &mut visit) {}
        assert!(found);
        assert_eq!(value, b"value");
        Ok(RedisValue::NoReply)
    })??;
    assert_eq!(ctx.get_selected_db(), selected);
    ctx.log_notice("worker runtime helper succeeded");
    ctx.call("PING", &[] as &[&str])
}

fn worker(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    start_worker(ctx, WorkerOperation::Runtime)
}

fn auto_memory_arg(args: &[RedisString]) -> Result<bool, RedisError> {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    match args[1].try_as_str()? {
        "manual" => Ok(false),
        "auto" => Ok(true),
        _ => Err(RedisError::Str("ERR expected manual or auto")),
    }
}

fn worker_info(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    start_worker(ctx, WorkerOperation::ServerInfo(auto_memory_arg(&args)?))
}

fn worker_keys(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    start_worker(ctx, WorkerOperation::Keys(auto_memory_arg(&args)?))
}

fn owned_server_version(ctx: &redis_module::RuntimeContext, auto_memory: bool) -> RedisResult {
    if auto_memory {
        ctx.auto_memory();
    }
    let info = ctx.server_info("server");
    let value = info
        .field("redis_version")
        .ok_or(RedisError::Str("ERR missing server version"))?;
    // Field results own their strings and may outlive the snapshot, while the
    // worker is still locked. Copy into Rust storage before unlocking as well.
    drop(info);
    let version = value.try_as_str()?.to_owned();
    Ok(RedisValue::BulkString(version))
}

fn owned_key_data(ctx: &redis_module::RuntimeContext, auto_memory: bool) -> RedisResult {
    use redis_module::key::KeyFlags;

    if auto_memory {
        ctx.auto_memory();
    }
    let name = ctx.create_string("worker:scoped:key");
    {
        let mut key = ctx.open_key_writable(&name);
        key.as_string_dma()?.write(b"\0\xffvalue")?;
    }
    {
        let mut key = ctx.open_key_writable_with_flags(&name, KeyFlags::empty());
        key.as_string_dma()?.append(b"!")?;
    }
    let bytes = ctx.open_key(&name).read()?.unwrap().to_vec();
    let flagged_bytes = ctx
        .open_key_with_flags(&name, KeyFlags::empty())
        .read()?
        .unwrap()
        .to_vec();

    ctx.call("HSET", &["worker:scoped:hash", "field", "value"])?;
    let hash_name = ctx.create_string("worker:scoped:hash");
    let cursor = redis_module::ScanKeyCursor::new(ctx.open_key(&hash_name));
    let mut entries = Vec::new();
    cursor.for_each(|_, field, value| {
        entries.push(field.as_slice().to_vec());
        entries.push(value.unwrap().as_slice().to_vec());
    });
    // Closing the cursor also closes its key while the worker context is live.
    drop(cursor);
    // Only Rust-owned data leaves this helper and the locked worker scope.
    Ok(RedisValue::Array(vec![
        RedisValue::StringBuffer(bytes),
        RedisValue::StringBuffer(flagged_bytes),
        RedisValue::Array(entries.into_iter().map(RedisValue::StringBuffer).collect()),
    ]))
}

enum WorkerOperation {
    Runtime,
    ServerInfo(bool),
    Keys(bool),
}

fn start_worker(ctx: &Context, operation: WorkerOperation) -> RedisResult {
    let mut task = WORKER_THREAD.lock().unwrap();
    if task.is_some() {
        return Err(RedisError::Str("ERR runtime worker already started"));
    }
    let blocked = ctx.block_client()?;
    *task = Some(std::thread::spawn(move || {
        let thread_ctx = redis_module::ThreadSafeContext::new();
        let guard = thread_ctx.lock();
        let result = match operation {
            WorkerOperation::ServerInfo(auto_memory) => owned_server_version(&guard, auto_memory),
            WorkerOperation::Keys(auto_memory) => owned_key_data(&guard, auto_memory),
            WorkerOperation::Runtime => worker_runtime(&guard),
        };
        drop(guard);
        drop(thread_ctx);
        let reply = redis_module::ThreadSafeContext::with_blocked_client(blocked);
        reply.reply(result);
    }));
    Ok(RedisValue::NoReply)
}

fn finish_worker(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut task = WORKER_THREAD.lock().unwrap();
    if !task
        .as_ref()
        .is_some_and(std::thread::JoinHandle::is_finished)
    {
        // Never join a live worker while holding Redis's lock.
        return Ok(0i64.into());
    }
    task.take()
        .unwrap()
        .join()
        .map_err(|_| RedisError::Str("ERR runtime worker panicked"))?;
    Ok(1i64.into())
}

fn counts(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(EVENTS.lock().unwrap().counts.to_vec().into())
}

fn last(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let events = EVENTS.lock().unwrap();
    let Some((event, key, db)) = &events.last else {
        return Ok(RedisValue::Null);
    };
    Ok(vec![
        event.clone().into(),
        RedisValue::StringBuffer(key.clone()),
        i64::from(*db).into(),
    ]
    .into())
}

fn send(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 3 {
        return Err(RedisError::WrongArity);
    }
    ctx.notify_keyspace_event_bytes(NotifyEvent::GENERIC, args[1].as_slice(), &args[2])?;
    Ok("OK".into())
}

redis_module! {
    name: "runtime_events",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        redis_command!("runtime.subscribe", subscribe),
        ["runtime.subscribe_legacy", subscribe, "", 0, 0, 0, ""],
        redis_command!("runtime.worker", worker),
        redis_command!("runtime.worker_info", worker_info),
        redis_command!("runtime.worker_keys", worker_keys),
        redis_command!("runtime.finish_worker", finish_worker),
        redis_command!("runtime.counts", counts).read_only(),
        redis_command!("runtime.last", last).read_only(),
        redis_command!("runtime.send", send),
    ],
}
