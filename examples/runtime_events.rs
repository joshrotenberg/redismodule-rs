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
static SUBSCRIPTION_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static EVENTS: Mutex<Events> = Mutex::new(Events {
    counts: [0; 5],
    last: None,
});

// The name also checks that the generated adapter does not shadow its handler.
fn invoke(ctx: &Context, event_type: NotifyEvent, event: &str, key: &[u8]) {
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

fn init(ctx: &Context, args: &[RedisString]) -> Status {
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

fn subscribe_on_thread(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut task = SUBSCRIPTION_THREAD.lock().unwrap();
    if task.is_some() {
        return Err(RedisError::Str("ERR subscription thread already started"));
    }
    let blocked = ctx.block_client();
    *task = Some(std::thread::spawn(move || {
        let thread_ctx = redis_module::ThreadSafeContext::new();
        let guard = thread_ctx.lock();
        let result =
            guard.subscribe_to_keyspace_events(NotifyEvent::STRING, redis_event_handler!(invoke));
        drop(guard);
        drop(thread_ctx);
        let reply = redis_module::ThreadSafeContext::with_blocked_client(blocked);
        reply.reply(result.map(|()| "OK".into()));
    }));
    Ok(RedisValue::NoReply)
}

fn finish_thread(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut task = SUBSCRIPTION_THREAD.lock().unwrap();
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
        .map_err(|_| RedisError::Str("ERR subscription thread panicked"))?;
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
        redis_command!("runtime.subscribe_on_thread", subscribe_on_thread),
        redis_command!("runtime.finish_thread", finish_thread),
        redis_command!("runtime.counts", counts).read_only(),
        redis_command!("runtime.last", last).read_only(),
        redis_command!("runtime.send", send),
    ],
}
