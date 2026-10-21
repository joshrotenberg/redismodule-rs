//! ACL-checked calls use a scoped context user on events and workers.
use redis_module::{
    CallOptionsBuilder, CallResult, CallUserScope, Context, EventContext, RedisError, RedisResult,
    RedisString, RedisValue, RuntimeContext, ThreadSafeContext, redis_command, redis_module,
};
use std::sync::Mutex;
use std::time::Duration;

static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static EVENT_RESULT: Mutex<Option<Result<Vec<i64>, String>>> = Mutex::new(None);

fn permissions(ctx: &RuntimeContext) -> [bool; 2] {
    let options = CallOptionsBuilder::new().verify_acl().build();
    ["allowed:item", "denied:item"].map(|key| {
        let reply: CallResult = ctx.call_ext("GET", &options, &[key]);
        reply.is_ok()
    })
}

fn nested_scopes(ctx: &RuntimeContext, reader: &RedisString) -> Result<Vec<i64>, RedisError> {
    let other = ctx.create_string("context-other");
    let missing = ctx.create_string("context-missing");
    let baseline = permissions(ctx);
    let reader_permissions = [true, false];
    let other_permissions = [false, true];
    let mut flags = Vec::new();

    let outer = ctx.scoped_call_user(reader)?;
    flags.push(permissions(ctx) == reader_permissions);
    let inner = ctx.scoped_call_user(&other)?;
    flags.push(permissions(ctx) == other_permissions);
    drop(inner);
    flags.push(permissions(ctx) == reader_permissions);

    // A missing user does not disturb the enclosing identity or run the closure.
    flags.push(ctx.scoped_call_user(&missing).is_err());
    let mut ran = false;
    flags.push(ctx.with_call_user(&missing, |_| ran = true).is_err() && !ran);
    flags.push(permissions(ctx) == reader_permissions);

    let failure: Result<Result<(), RedisError>, RedisError> =
        ctx.with_call_user(&other, |inner: &RuntimeContext| {
            flags.push(permissions(inner) == other_permissions);
            inner.with_call_user(reader, |nested| {
                flags.push(permissions(nested) == reader_permissions);
            })?;
            flags.push(permissions(inner) == other_permissions);
            Err(RedisError::Str("ERR deliberate closure failure"))
        });
    flags.push(failure?.is_err());
    flags.push(permissions(ctx) == reader_permissions);

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.with_call_user(&other, |inner| {
            flags.push(permissions(inner) == other_permissions);
            panic!("nested call user unwind");
        })
        .unwrap();
    }));
    flags.push(panicked.is_err());
    flags.push(permissions(ctx) == reader_permissions);
    drop(outer);
    flags.push(permissions(ctx) == baseline);

    // Removing an outer guard cannot free a handle that a later drop restores.
    let outer = ctx.scoped_call_user(reader)?;
    let middle = ctx.scoped_call_user(&other)?;
    let inner = ctx.scoped_call_user(reader)?;
    drop(middle);
    flags.push(permissions(ctx) == reader_permissions);
    drop(inner);
    flags.push(permissions(ctx) == reader_permissions);
    let inner = ctx.scoped_call_user(&other)?;
    drop(outer);
    flags.push(permissions(ctx) == other_permissions);
    drop(inner);
    flags.push(permissions(ctx) == baseline);

    // A closure's own scope ends on return even when another guard survives it.
    let escaped = ctx.with_call_user(reader, |_| ctx.scoped_call_user(&other))??;
    flags.push(permissions(ctx) == other_permissions);
    drop(escaped);
    flags.push(permissions(ctx) == baseline);

    // Native callbacks can expose multiple Rust views of the same context.
    let outer = ctx.scoped_call_user(reader)?;
    {
        // SAFETY: same live native invocation, with the Redis lock held.
        let alias = unsafe { RuntimeContext::new(ctx.get_raw()) };
        let inner = alias.scoped_call_user(&other)?;
        flags.push(permissions(ctx) == other_permissions);
        drop(inner);
        flags.push(permissions(ctx) == reader_permissions);
        std::mem::forget(alias.scoped_call_user(&other)?);
        flags.push(permissions(ctx) == other_permissions);
        // The alias view's destructor ends even a forgotten scope.
    }
    flags.push(permissions(ctx) == reader_permissions);
    drop(outer);
    flags.push(permissions(ctx) == baseline);

    // Scan callbacks create a fresh runtime wrapper for the native context.
    let outer = ctx.scoped_call_user(reader)?;
    let cursor = redis_module::KeysCursor::new();
    let mut visited = false;
    while cursor.scan(ctx, &mut |callback, _, _| {
        visited = true;
        callback
            .with_call_user(&other, |callback| {
                flags.push(permissions(callback) == other_permissions);
            })
            .unwrap();
        flags.push(permissions(ctx) == reader_permissions);
    }) {}
    flags.push(visited);
    flags.push(permissions(ctx) == reader_permissions);
    drop(outer);
    flags.push(permissions(ctx) == baseline);

    // Native contexts have independent stacks, even under the same Redis lock.
    let outer = ctx.scoped_call_user(reader)?;
    unsafe {
        let raw =
            redis_module::raw::RedisModule_GetThreadSafeContext.unwrap()(std::ptr::null_mut());
        {
            let isolated = RuntimeContext::new(raw);
            isolated.with_call_user(&other, |isolated| {
                flags.push(permissions(isolated) == other_permissions);
                flags.push(permissions(ctx) == reader_permissions);
            })?;
        }
        redis_module::raw::RedisModule_FreeThreadSafeContext.unwrap()(raw);
    }
    flags.push(permissions(ctx) == reader_permissions);
    drop(outer);
    flags.push(permissions(ctx) == baseline);

    // Redis 8.8+ exposes a borrowed prior override. Refresh it for each new
    // group of scopes, even if this Rust view has been used before.
    // INFO advertises the Redis compatibility version on Valkey; its native
    // server version belongs to a separate version series.
    let version = ctx.get_redis_version_rm_call()?;
    let get_context_user = unsafe { redis_module::raw::RedisModule_GetContextUser };
    // A missing API on a supporting server must fail rather than silently
    // skipping the native-baseline checks below.
    flags.push((version.major, version.minor) < (8, 8) || get_context_user.is_some());
    unsafe {
        if let Some(_get) = get_context_user {
            for (name, expected) in [(&other, other_permissions), (reader, reader_permissions)] {
                let user = redis_module::raw::RedisModule_GetModuleUserFromUserName.unwrap()(
                    name.as_ptr(),
                );
                redis_module::raw::RedisModule_SetContextUser.unwrap()(ctx.get_raw(), user);
                ctx.with_call_user(reader, |ctx| {
                    flags.push(permissions(ctx) == reader_permissions);
                })?;
                flags.push(permissions(ctx) == expected);
                redis_module::raw::RedisModule_SetContextUser.unwrap()(
                    ctx.get_raw(),
                    std::ptr::null(),
                );
                redis_module::raw::RedisModule_FreeModuleUser.unwrap()(user);
            }
        }
    }
    Ok(flags.into_iter().map(i64::from).collect())
}

fn client(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    // A client closure retains its client capabilities and concrete type.
    ctx.with_call_user(&args[1], |client: &Context| client.get_selected_db())?;
    exercise(ctx, args[1].try_as_str()?).map(Into::into)
}

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
    let mut flags = [
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
    .to_vec();
    flags.extend(nested_scopes(ctx, &username)?);
    Ok(flags)
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
            let result = exercise(&guard, &username);
            let username = guard.create_string(username.as_str());
            guard
                .with_call_user(&username, |_: &redis_module::WorkerContext| ())
                .unwrap();
            std::mem::forget(guard.scoped_call_user(&username).unwrap());
            result
        } else {
            let owner = ThreadSafeContext::new();
            let guard = owner.lock();
            let result = exercise(&guard, &username);
            let username = guard.create_string(username.as_str());
            std::mem::forget(guard.scoped_call_user(&username).unwrap());
            result
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
    let user = ctx.create_string(username.as_str());
    ctx.with_call_user(&user, |_: &EventContext| ()).unwrap();
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
        redis_command!("context_acl.client", client),
        redis_command!("context_acl.worker", worker),
        redis_command!("context_acl.finish", finish),
        redis_command!("context_acl.timer", timer),
        redis_command!("context_acl.result", result),
    ],
}
