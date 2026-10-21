//! ACL-checked calls use a scoped context user on events and workers.
use redis_module::{
    CallOptionsBuilder, CallResult, CallUserScope, Context, EventContext, RedisError, RedisResult,
    RedisString, RedisValue, RuntimeContext, ThreadSafeContext, redis_command, redis_module,
};
use std::sync::Mutex;
use std::time::Duration;

static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static EVENT_RESULT: Mutex<Option<Result<Vec<i64>, String>>> = Mutex::new(None);

fn exercise(ctx: &RuntimeContext, username: &str) -> Result<Vec<i64>, RedisError> {
    let options = CallOptionsBuilder::new().verify_acl().build();
    let can_read = |key| {
        let reply: CallResult = ctx.call_ext("GET", &options, &[key]);
        reply.is_ok()
    };
    let before = [can_read("allowed:item"), can_read("denied:item")];
    let username = ctx.create_string(username);
    let scope: CallUserScope<'_> = ctx.scoped_call_user(&username)?;
    let allowed = can_read("allowed:item");
    let denied = !can_read("denied:item");
    drop(scope);
    let after = before == [can_read("allowed:item"), can_read("denied:item")];

    // Check the same identity cleanup when work unwinds before returning.
    let mut active_before_panic = false;
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _scope: CallUserScope<'_> = ctx.scoped_call_user(&username).unwrap();
        active_before_panic = can_read("allowed:item") && !can_read("denied:item");
        panic!("scoped context user unwind");
    }))
    .is_err();
    let after_unwind = before == [can_read("allowed:item"), can_read("denied:item")];
    Ok([
        before[0],
        before[1],
        allowed,
        denied,
        after,
        active_before_panic,
        panicked,
        after_unwind,
    ]
    .map(i64::from)
    .to_vec())
}

fn worker(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 3 {
        return Err(RedisError::WrongArity);
    }
    let detached = match args[1].try_as_str()? {
        "plain" => false,
        "detached" => true,
        _ => return Err(RedisError::Str("ERR expected plain or detached")),
    };
    let username = args[2].try_as_str()?.to_owned();
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR worker already started"));
    }
    let blocked = ctx.block_client();
    *worker = Some(std::thread::spawn(move || {
        let result = if detached {
            let guard = redis_module::MODULE_CONTEXT.lock();
            exercise(&guard, &username)
        } else {
            let owner = ThreadSafeContext::new();
            let guard = owner.lock();
            exercise(&guard, &username)
        };
        let reply = ThreadSafeContext::with_blocked_client(blocked);
        reply.reply(result.map(Into::into));
    }));
    Ok(RedisValue::NoReply)
}

fn finish(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap();
    if !worker
        .as_ref()
        .is_some_and(std::thread::JoinHandle::is_finished)
    {
        return Ok(0i64.into());
    }
    worker
        .take()
        .unwrap()
        .join()
        .map_err(|_| RedisError::Str("ERR worker panicked"))?;
    Ok(1i64.into())
}

fn timer(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    let username = args[1].try_as_str()?.to_owned();
    ctx.create_timer(Duration::from_millis(1), on_timer, username);
    Ok("OK".into())
}

fn on_timer(ctx: &EventContext, username: String) {
    *EVENT_RESULT.lock().unwrap() = Some(exercise(ctx, &username).map_err(|e| e.to_string()));
}

fn result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    match EVENT_RESULT.lock().unwrap().take() {
        Some(Ok(result)) => Ok(result.into()),
        Some(Err(error)) => Err(RedisError::String(error)),
        None => Ok(RedisValue::Null),
    }
}

redis_module! {
    name: "context_acl",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        redis_command!("context_acl.worker", worker),
        redis_command!("context_acl.finish", finish),
        redis_command!("context_acl.timer", timer),
        redis_command!("context_acl.result", result),
    ],
}
