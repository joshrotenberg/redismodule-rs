//! Synchronous auth stub. Administrative setup/drain must be authorized by the
//! application. Fixed credentials are test data, not a provider implementation.
use redis_module::{
    AuthContext, AuthDecision, AuthRequest, AuthTarget, ClientId, Context, EventContext,
    ModuleUserHandle, PublicAuthError, RedisError, RedisGILGuard, RedisResult, RedisString,
    RegistrationContext, Status, redis_command, redis_module,
};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

static USERS: RedisGILGuard<BTreeMap<&'static str, ModuleUserHandle>> =
    RedisGILGuard::new(BTreeMap::new());
static CLIENTS: Mutex<BTreeMap<ClientId, usize>> = Mutex::new(BTreeMap::new());
static ACCEPTING: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicI64 = AtomicI64::new(0);
static CHANGED: AtomicI64 = AtomicI64::new(0);
static DROPPED: AtomicI64 = AtomicI64::new(0);
static REFUSE: AtomicBool = AtomicBool::new(false);
static NATIVE_CALLS: AtomicI64 = AtomicI64::new(0);
static PROBE_FLAGS: AtomicI64 = AtomicI64::new(0);
type FlagsFn = unsafe extern "C" fn(*mut redis_module::raw::RedisModuleCtx) -> i32;
type AttachFn = unsafe extern "C" fn(
    *mut redis_module::raw::RedisModuleCtx,
    *mut redis_module::raw::RedisModuleUser,
    redis_module::raw::RedisModuleUserChangedFunc,
    *mut std::ffi::c_void,
    *mut u64,
) -> i32;
static NATIVE_APIS: Mutex<Option<(Option<FlagsFn>, Option<AttachFn>)>> = Mutex::new(None);

unsafe extern "C" fn probe_flags(_: *mut redis_module::raw::RedisModuleCtx) -> i32 {
    PROBE_FLAGS.load(Ordering::Relaxed) as i32
}
unsafe extern "C" fn refused_attach(
    _: *mut redis_module::raw::RedisModuleCtx,
    _: *mut redis_module::raw::RedisModuleUser,
    _: redis_module::raw::RedisModuleUserChangedFunc,
    _: *mut std::ffi::c_void,
    _: *mut u64,
) -> i32 {
    NATIVE_CALLS.fetch_add(1, Ordering::Relaxed);
    redis_module::raw::REDISMODULE_ERR as i32
}
fn probe(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mode = args.get(1).ok_or(RedisError::WrongArity)?.try_as_str()?;
    if mode == "count" {
        return Ok(NATIVE_CALLS.load(Ordering::Relaxed).into());
    }
    if mode == "reset" {
        if let Some((flags, attach)) = NATIVE_APIS.lock().unwrap().take() {
            unsafe {
                redis_module::raw::RedisModule_GetContextFlags = flags;
                redis_module::raw::RedisModule_AuthenticateClientWithUser = attach;
            }
        }
        return Ok("OK".into());
    }
    let flags = match mode {
        "loading" => redis_module::raw::REDISMODULE_CTX_FLAGS_LOADING as i64,
        "async" => redis_module::raw::REDISMODULE_CTX_FLAGS_ASYNC_LOADING as i64,
        "missing" | "normal" => 0,
        _ => return Err(RedisError::Str("ERR unknown probe")),
    };
    let mut saved = NATIVE_APIS.lock().unwrap();
    if saved.is_none() {
        *saved = Some(unsafe {
            (
                redis_module::raw::RedisModule_GetContextFlags,
                redis_module::raw::RedisModule_AuthenticateClientWithUser,
            )
        });
    }
    PROBE_FLAGS.store(flags, Ordering::Relaxed);
    NATIVE_CALLS.store(0, Ordering::Relaxed);
    // Inert admission probe only: no hazardous native mutation is executed.
    unsafe {
        redis_module::raw::RedisModule_GetContextFlags = if mode == "missing" {
            None
        } else {
            Some(probe_flags)
        };
        redis_module::raw::RedisModule_AuthenticateClientWithUser = Some(refused_attach);
    }
    Ok("OK".into())
}
fn internal(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let error = ctx
        .call("AUTH", &["stub", "stub-password"])
        .expect_err("internal auth must fail");
    Ok(error.to_string().into())
}

struct Track(ClientId);
impl Drop for Track {
    fn drop(&mut self) {
        let mut clients = CLIENTS.lock().unwrap();
        let count = clients.get_mut(&self.0).unwrap();
        *count -= 1;
        if *count == 0 {
            clients.remove(&self.0);
        }
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}
fn tracked(ctx: &AuthContext<'_>, target: AuthTarget) -> AuthDecision {
    *CLIENTS.lock().unwrap().entry(ctx.client_id()).or_default() += 1;
    let state = Track(ctx.client_id());
    AuthDecision::Accept(target.on_changed(Box::new(move |_| {
        CHANGED.fetch_add(1, Ordering::Relaxed);
        drop(state);
    })))
}
fn authenticate(ctx: &AuthContext<'_>, request: AuthRequest<'_>) -> AuthDecision {
    CALLS.fetch_add(1, Ordering::Relaxed);
    let user = request.username();
    if !matches!(
        user,
        b"stub"
            | b"binary\0\xff"
            | b"no-notify"
            | b"disabled"
            | b"stale"
            | b"acl"
            | b"deny"
            | b"public"
            | b"sanitize"
            | b"panic"
            | b"error"
    ) {
        return AuthDecision::NotHandled;
    }
    if user == b"panic" {
        // The test-only quiet-hook load argument isolates the SDK diagnostic
        // from the process hook. This payload must never be formatted by it.
        std::panic::panic_any(String::from("private-provider-panic-sentinel"));
    }
    if user == b"public" {
        return AuthDecision::Deny(PublicAuthError::new("Stub denied authentication"));
    }
    if user == b"sanitize" {
        return AuthDecision::Deny(PublicAuthError::new("public\r\nmessage\0tail"));
    }
    if user == b"deny" || user == b"error" {
        return AuthDecision::Deny(PublicAuthError::default());
    }
    let expected: &[u8] = if user == b"binary\0\xff" {
        b"\0binary\xffpassword"
    } else {
        b"stub-password"
    };
    if request.password().as_bytes() != expected || !ACCEPTING.load(Ordering::Relaxed) {
        return AuthDecision::Deny(PublicAuthError::default());
    }
    if user == b"acl" {
        return tracked(ctx, AuthTarget::acl_user(b"existing".to_vec()));
    }
    let key = if user == b"disabled" {
        "disabled"
    } else if user == b"stale" {
        "stale"
    } else {
        "active"
    };
    let handle = { USERS.lock(ctx).get(key).cloned() };
    match handle {
        Some(handle) if user == b"no-notify" => {
            AuthDecision::Accept(AuthTarget::module_user(handle))
        }
        Some(handle) => tracked(ctx, AuthTarget::module_user(handle)),
        None => AuthDecision::Deny(PublicAuthError::new("Stub identity is not configured")),
    }
}
fn prepare(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    if !USERS.lock(ctx).is_empty() {
        return Err(RedisError::Str("ERR drain before preparing again"));
    }
    for key in ["active", "disabled", "stale"] {
        let handle = ctx.create_module_user(&format!("stub-{key}"))?;
        // Retain removal capability even if a later directive/setup fails.
        USERS.lock(ctx).insert(key, handle.clone());
        for rule in [
            "+ping",
            "+auth",
            "+hello",
            "+get",
            "+acl|whoami",
            "+client|id",
            "~auth:*",
        ] {
            ctx.set_module_user_acl(&handle, rule)?;
        }
        if key != "disabled" {
            ctx.set_module_user_acl(&handle, "on")?;
        }
        if key == "stale" {
            ctx.remove_module_user(&handle)?;
        }
    }
    ACCEPTING.store(true, Ordering::Relaxed);
    Ok("OK".into())
}
fn drain(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ACCEPTING.store(false, Ordering::Relaxed);
    let clients: Vec<_> = CLIENTS.lock().unwrap().keys().copied().collect();
    for client in clients {
        // Track retains only installed state or a pending target within this
        // synchronous invocation; no callback is running across this command.
        ctx.deauthenticate_and_close_client(client)?;
    }
    let users: Vec<_> = USERS
        .lock(ctx)
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    for (key, handle) in users {
        if key != "stale" {
            ctx.remove_module_user(&handle)?;
        }
        USERS.lock(ctx).remove(key);
    }
    Ok("OK".into())
}
fn stats(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        CALLS.load(Ordering::Relaxed),
        CHANGED.load(Ordering::Relaxed),
        DROPPED.load(Ordering::Relaxed),
        CLIENTS.lock().unwrap().len() as i64,
    ]
    .into())
}
fn refuse(_: &Context, _: Vec<RedisString>) -> RedisResult {
    REFUSE.store(true, Ordering::Relaxed);
    Ok("OK".into())
}
fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    USERS.lock(ctx).clear();
    CLIENTS.lock().unwrap().clear();
    ACCEPTING.store(false, Ordering::Relaxed);
    REFUSE.store(false, Ordering::Relaxed);
    CALLS.store(0, Ordering::Relaxed);
    CHANGED.store(0, Ordering::Relaxed);
    DROPPED.store(0, Ordering::Relaxed);
    if args.iter().any(|a| a.as_slice() == b"quiet-hook") {
        std::panic::set_hook(Box::new(|_| {}));
    }
    if args.iter().any(|a| a.as_slice() == b"missing-register") {
        unsafe {
            redis_module::raw::RedisModule_RegisterAuthCallback = None;
        }
    }
    if args.iter().any(|a| a.as_slice() == b"missing-audit") {
        unsafe {
            redis_module::raw::RedisModule_ACLAddLogEntryByUserName = None;
        }
    }
    ctx.log_notice("auth_callbacks initialized");
    if args.iter().any(|a| a.as_slice() == b"fail") {
        Status::Err
    } else {
        Status::Ok
    }
}
fn deinit(_: &EventContext) -> Status {
    if REFUSE.swap(false, Ordering::Relaxed) {
        Status::Err
    } else {
        Status::Ok
    }
}
redis_module! {
    name: "auth_callbacks", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    deinit: deinit,
    auth: authenticate,
    commands: [
        redis_command!("auth_callbacks.prepare", prepare).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_callbacks.drain", drain).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_callbacks.stats", stats).read_only(),
        redis_command!("auth_callbacks.refuse", refuse).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_callbacks.probe", probe).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_callbacks.internal", internal).flags(redis_module::CommandFlags::ADMIN),
    ],
}
