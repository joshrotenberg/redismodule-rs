//! Module-owned users and privileged identity attachment. Commands here are an
//! administrative demonstration; production modules must authorize callers.
use redis_module::{
    ClientChangeCallback, ClientId, Context, EventContext, ModuleUserHandle, RedisError,
    RedisGILGuard, RedisResult, RedisString, RedisValue, RegistrationContext, Status,
    ThreadSafeContext, redis_command, redis_module,
};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

static USERS: RedisGILGuard<BTreeMap<String, ModuleUserHandle>> =
    RedisGILGuard::new(BTreeMap::new());
static CHANGED: AtomicI64 = AtomicI64::new(0);
static DROPPED: AtomicI64 = AtomicI64::new(0);
static REFUSE: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

struct Probe(bool);
impl Drop for Probe {
    fn drop(&mut self) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        assert!(!self.0, "auth cleanup destructor panic");
    }
}
fn callback(mode: &str) -> Option<ClientChangeCallback> {
    if mode == "none" {
        return None;
    }
    let probe = Probe(mode == "drop-panic");
    let panic = mode == "panic";
    Some(Box::new(move |client| {
        CHANGED.fetch_add(1, Ordering::Relaxed);
        eprintln!("auth_users cleanup {}", client.0);
        // Drop separately before the callback panic to avoid a double panic.
        drop(probe);
        assert!(!panic, "auth cleanup callback panic");
    }))
}
fn arg(args: &[RedisString], n: usize) -> Result<&str, RedisError> {
    args.get(n).ok_or(RedisError::WrongArity)?.try_as_str()
}
fn user(ctx: &Context, name: &str) -> Result<ModuleUserHandle, RedisError> {
    USERS
        .lock(ctx)
        .get(name)
        .cloned()
        .ok_or(RedisError::Str("ERR unknown example user"))
}
fn create(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let name = arg(&args, 1)?;
    let handle = ctx.create_module_user(name)?;
    USERS.lock(ctx).insert(name.to_owned(), handle);
    Ok("OK".into())
}
fn defaults(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let handle = user(ctx, arg(&args, 1)?)?;
    for directive in ["on", "+@all", "~*"] {
        ctx.set_module_user_acl(&handle, directive)?;
    }
    Ok("OK".into())
}
fn acl(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ctx.redact_client_command_argument(2)?;
    ctx.set_module_user_acl_string(&user(ctx, arg(&args, 1)?)?, arg(&args, 2)?)?;
    Ok("OK".into())
}
fn rule(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ctx.redact_client_command_argument(2)?;
    ctx.set_module_user_acl(&user(ctx, arg(&args, 1)?)?, arg(&args, 2)?)?;
    Ok("OK".into())
}
fn remove(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    // Retain the stale handle to demonstrate rejection on repeated operations.
    ctx.remove_module_user(&user(ctx, arg(&args, 1)?)?)?;
    Ok("OK".into())
}
fn attach(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let id =
        ctx.authenticate_client_with_user(&user(ctx, arg(&args, 1)?)?, callback(arg(&args, 2)?))?;
    Ok((id.0 as i64).into())
}
fn attach_acl(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let id =
        ctx.authenticate_client_with_acl_user(arg(&args, 1)?.as_bytes(), callback(arg(&args, 2)?))?;
    Ok((id.0 as i64).into())
}
fn stats(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        CHANGED.load(Ordering::Relaxed),
        DROPPED.load(Ordering::Relaxed),
    ]
    .into())
}
fn refuse(_: &Context, _: Vec<RedisString>) -> RedisResult {
    REFUSE.store(true, Ordering::Relaxed);
    Ok("OK".into())
}
fn certificate(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.get(2).is_some_and(|arg| arg.as_slice() == b"auto") {
        // SAFETY: this command owns no pending call promises or raw values.
        unsafe {
            ctx.auto_memory();
        }
    }
    let id = ClientId(
        arg(&args, 1)?
            .parse()
            .map_err(|_| RedisError::Str("ERR invalid ID"))?,
    );
    Ok(ctx
        .client_certificate(id)?
        .map_or(RedisValue::Null, RedisValue::StringBuffer))
}
fn redact(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let position = arg(&args, 1)?
        .parse()
        .map_err(|_| RedisError::Str("ERR invalid position"))?;
    ctx.redact_client_command_argument(position)?;
    Ok("OK".into())
}
fn revoke(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ctx.deauthenticate_and_close_client(ClientId(
        arg(&args, 1)?
            .parse()
            .map_err(|_| RedisError::Str("ERR invalid ID"))?,
    ))?;
    Ok("OK".into())
}
fn worker(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let client = ClientId(
        arg(&args, 1)?
            .parse()
            .map_err(|_| RedisError::Str("ERR invalid ID"))?,
    );
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR worker already exists"));
    }
    *worker = Some(std::thread::spawn(move || {
        let owner = ThreadSafeContext::new();
        let ctx = owner.lock();
        ctx.deauthenticate_and_close_client(client).unwrap();
    }));
    Ok("OK".into())
}
fn finish(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap();
    if worker.as_ref().is_some_and(|w| w.is_finished()) {
        worker.take().unwrap().join().unwrap();
        return Ok(1_i64.into());
    }
    Ok(0_i64.into())
}
fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    USERS.lock(ctx).clear();
    CHANGED.store(0, Ordering::Relaxed);
    DROPPED.store(0, Ordering::Relaxed);
    REFUSE.store(false, Ordering::Relaxed);
    assert!(ctx.create_module_user("init-denied").is_err());
    if args.first().is_some_and(|a| a.as_slice() == b"fail") {
        Status::Err
    } else {
        Status::Ok
    }
}
fn deinit(ctx: &EventContext) -> Status {
    assert!(ctx.create_module_user("unload-denied").is_err());
    if WORKER.lock().unwrap().is_some() || REFUSE.swap(false, Ordering::Relaxed) {
        Status::Err
    } else {
        Status::Ok
    }
}
redis_module! {
    name: "auth_users",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    deinit: deinit,
    commands: [
        redis_command!("auth_users.create", create).write(),
        redis_command!("auth_users.acl", acl).write(),
        redis_command!("auth_users.defaults", defaults).write(),
        redis_command!("auth_users.rule", rule).write(),
        redis_command!("auth_users.remove", remove).write(),
        redis_command!("auth_users.attach", attach).write(),
        redis_command!("auth_users.attach_acl", attach_acl).write(),
        redis_command!("auth_users.stats", stats).read_only(),
        redis_command!("auth_users.refuse", refuse).write(),
        redis_command!("auth_users.certificate", certificate).read_only(),
        redis_command!("auth_users.redact", redact).read_only(),
        redis_command!("auth_users.revoke", revoke).write(),
        redis_command!("auth_users.worker", worker).write(),
        redis_command!("auth_users.finish", finish).read_only(),
    ],
}
