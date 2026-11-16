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
scoped_tls::scoped_thread_local!(static CURRENT: Context);
static RAW_PROBE: AtomicBool = AtomicBool::new(false);
static RAW_TARGET: RedisGILGuard<Option<ModuleUserHandle>> = RedisGILGuard::new(None);
static RAW_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SCOPED_CLEANUP: AtomicI64 = AtomicI64::new(0);
static LATER_CLEANUP: AtomicI64 = AtomicI64::new(0);
static REENTRY: AtomicI64 = AtomicI64::new(0);
static WORKER_RESULTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static NATIVE_WORKER_CALLS: AtomicI64 = AtomicI64::new(0);
static CHANGED: AtomicI64 = AtomicI64::new(0);
static DROPPED: AtomicI64 = AtomicI64::new(0);
static EARLY_UNLOAD: AtomicBool = AtomicBool::new(false);
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
fn capabilities(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let handle = user(ctx, arg(&args, 1)?)?;
    let acl = ctx.set_module_user_acl_string(&handle, "on");
    let redact = ctx.redact_client_command_argument(1);
    Ok(vec![acl, redact]
        .into_iter()
        .map(|r| r.map_or_else(|e| e.to_string(), |_| "OK".to_owned()))
        .collect::<Vec<_>>()
        .into())
}
fn alias(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let handle = user(ctx, arg(&args, 1)?)?;
    USERS.lock(ctx).insert(arg(&args, 2)?.to_owned(), handle);
    Ok("OK".into())
}
fn forget(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let handle = USERS.lock(ctx).remove(arg(&args, 1)?);
    drop(handle); // A passive handle drop cannot free or disconnect the user.
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
unsafe extern "C" fn raw_client_change(
    ctx: *mut redis_module::raw::RedisModuleCtx,
    _: redis_module::raw::RedisModuleEvent,
    subevent: u64,
    _: *mut std::ffi::c_void,
) {
    redis_module::panic::run_callback(
        "auth raw client event",
        || {
            if subevent != redis_module::raw::REDISMODULE_SUBEVENT_CLIENT_CHANGE_DISCONNECTED
                || !RAW_PROBE.swap(false, Ordering::Relaxed)
            {
                return;
            }
            // SAFETY: Redis supplies this module-associated event invocation with
            // the lock held. No context is fabricated for the ctx-less auth hook.
            let ctx = unsafe { EventContext::new(ctx) };
            let target = RAW_TARGET.lock(&ctx).as_ref().unwrap().clone();
            let results = vec![
                ctx.remove_module_user(&target).unwrap_err().to_string(),
                ctx.set_module_user_acl(&target, "off")
                    .unwrap_err()
                    .to_string(),
                ctx.set_module_user_acl_string(&target, "reset off")
                    .unwrap_err()
                    .to_string(),
            ];
            *RAW_ERRORS.lock().unwrap() = results;
        },
        || (),
    );
}
fn raw_probe_result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RAW_ERRORS.lock().unwrap().clone().into())
}
fn attach_pubsub_cleanup(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let kind = arg(&args, 1)?.to_owned();
    let name = arg(&args, 2)?.to_owned();
    let handle = if kind == "module" {
        Some(user(ctx, &name)?)
    } else {
        None
    };
    let owned_name = name.clone();
    let target = handle.clone();
    let probe = Probe(false);
    let callback: ClientChangeCallback = Box::new(move |_| {
        CHANGED.fetch_add(1, Ordering::Relaxed);
        if CURRENT.is_set() {
            CURRENT.with(|ctx| {
                if let Some(handle) = target {
                    ctx.remove_module_user(&handle).unwrap();
                } else {
                    ctx.call("ACL", &["DELUSER", &owned_name]).unwrap();
                }
            });
            SCOPED_CLEANUP.fetch_add(1, Ordering::Relaxed);
        } else {
            // Newer Redis schedules Pub/Sub disconnection after the setter
            // returns. That callback has no scoped context to reenter.
            LATER_CLEANUP.fetch_add(1, Ordering::Relaxed);
        }
        drop(probe);
        REENTRY.fetch_add(1, Ordering::Relaxed);
    });
    let id = if let Some(handle) = handle {
        ctx.authenticate_client_with_user(&handle, Some(callback))?
    } else {
        ctx.authenticate_client_with_acl_user(name.as_bytes(), Some(callback))?
    };
    Ok((id.0 as i64).into())
}
fn restrict_pubsub(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let kind = arg(&args, 1)?;
    let name = arg(&args, 2)?;
    CURRENT.set(ctx, || -> RedisResult {
        let args = ["SETUSER", name, "resetchannels"];
        match kind {
            "module" => {
                let target = user(ctx, name)?;
                *RAW_TARGET.lock(ctx) = Some(target.clone());
                RAW_PROBE.store(true, Ordering::Relaxed);
                let result = ctx.set_module_user_acl_string(&target, "resetchannels");
                RAW_PROBE.store(false, Ordering::Relaxed);
                RAW_TARGET.lock(ctx).take();
                result?;
            }
            "call" => {
                ctx.call("ACL", &args)?;
            }
            "typed" => {
                let reply: redis_module::CallResult = ctx.call_ext(
                    "ACL",
                    &redis_module::CallOptionsBuilder::new().build(),
                    &args,
                );
                reply.map_err(RedisError::from)?;
            }
            #[cfg(any(
                feature = "min-redis-compatibility-version-7-2",
                feature = "min-redis-compatibility-version-7-4",
                feature = "min-redis-compatibility-version-8-0"
            ))]
            "blocking" => {
                let reply: redis_module::PromiseCallReply = ctx.call_blocking(
                    "ACL",
                    &redis_module::CallOptionsBuilder::new().build_blocking(),
                    &args,
                );
                match reply {
                    redis_module::PromiseCallReply::Resolved(reply) => {
                        reply.map_err(RedisError::from)?;
                    }
                    redis_module::PromiseCallReply::Future(_) => {
                        return Err(RedisError::Str("ERR ACL unexpectedly blocked"));
                    }
                }
            }
            _ => return Err(RedisError::Str("ERR unknown call mode")),
        }
        Ok("OK".into())
    })
}
fn cleanup_timing(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        SCOPED_CLEANUP.load(Ordering::Relaxed),
        LATER_CLEANUP.load(Ordering::Relaxed),
    ]
    .into())
}
fn yield_reentry(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let ready = arg(&args, 1)?;
    CURRENT.set(ctx, || -> RedisResult {
        std::fs::write(ready, b"ready").map_err(|_| RedisError::Str("ERR yield marker failed"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while REENTRY.load(Ordering::Relaxed) == 0 {
            if std::time::Instant::now() >= deadline {
                return Err(RedisError::Str("ERR auth yield timed out"));
            }
            ctx.yield_now(redis_module::YieldMode::Clients, None)?;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok("OK".into())
    })
}
fn absent_api(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let user = user(ctx, "owner")?;
    let (with_user, with_acl) = unsafe {
        (
            redis_module::raw::RedisModule_AuthenticateClientWithUser,
            redis_module::raw::RedisModule_AuthenticateClientWithACLUser,
        )
    };
    unsafe {
        redis_module::raw::RedisModule_AuthenticateClientWithUser = None;
        redis_module::raw::RedisModule_AuthenticateClientWithACLUser = None;
    }
    let errors = vec![
        ctx.authenticate_client_with_user(&user, callback("drop-panic"))
            .unwrap_err()
            .to_string(),
        ctx.authenticate_client_with_acl_user(b"default", callback("drop-panic"))
            .unwrap_err()
            .to_string(),
    ];
    unsafe {
        redis_module::raw::RedisModule_AuthenticateClientWithUser = with_user;
        redis_module::raw::RedisModule_AuthenticateClientWithACLUser = with_acl;
    }
    Ok(errors.into())
}
fn early_unload(_: &Context, _: Vec<RedisString>) -> RedisResult {
    EARLY_UNLOAD.store(true, Ordering::Relaxed);
    Ok("OK".into())
}
fn reentry(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let action = arg(&args, 1)?.to_owned();
    let target = user(ctx, "owner")?;
    let incoming = target.clone();
    let action_copy = action.clone();
    let probe = Probe(false);
    ctx.authenticate_client_with_user(
        &target,
        Some(Box::new(move |client| {
            CHANGED.fetch_add(1, Ordering::Relaxed);
            CURRENT.with(|ctx| match action_copy.as_str() {
                "same-client" => ctx.deauthenticate_and_close_client(client).unwrap(),
                "remove-incoming" => ctx.remove_module_user(&incoming).unwrap(),
                "delete-acl" => {
                    ctx.call("ACL", &["DELUSER", "reentry-acl"]).unwrap();
                }
                "attach-again" => {
                    assert!(
                        ctx.authenticate_client_with_acl_user(b"default", None)
                            .is_err()
                    );
                }
                _ => panic!("unknown reentry action"),
            });
            drop(probe);
            REENTRY.fetch_add(1, Ordering::Relaxed);
        })),
    )?;
    let id = CURRENT.set(ctx, || {
        if action == "delete-acl" {
            ctx.authenticate_client_with_acl_user(b"reentry-acl", None)
        } else {
            ctx.authenticate_client_with_user(&target, None)
        }
    })?;
    Ok((id.0 as i64).into())
}
fn reentry_result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(REENTRY.load(Ordering::Relaxed).into())
}
unsafe extern "C" fn forbidden_worker_revoke(
    _: *mut redis_module::raw::RedisModuleCtx,
    _: u64,
) -> i32 {
    NATIVE_WORKER_CALLS.fetch_add(1, Ordering::Relaxed);
    0
}
fn worker(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let client = ClientId(
        arg(&args, 1)?
            .parse()
            .map_err(|_| RedisError::Str("ERR invalid ID"))?,
    );
    let stale = ctx.create_module_user("worker-stale")?;
    ctx.remove_module_user(&stale)?;
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR worker already exists"));
    }
    *worker = Some(std::thread::spawn(move || {
        let owner = ThreadSafeContext::new();
        let ctx = owner.lock();
        // Only inert/missing APIs are installed for worker refusal probes. A
        // regression cannot enter native connection/event-loop operations.
        let (revoke, acl, cert) = unsafe {
            (
                redis_module::raw::RedisModule_DeauthenticateAndCloseClient,
                redis_module::raw::RedisModule_SetModuleUserACLString,
                redis_module::raw::RedisModule_GetClientCertificate,
            )
        };
        unsafe {
            redis_module::raw::RedisModule_DeauthenticateAndCloseClient =
                Some(forbidden_worker_revoke);
            redis_module::raw::RedisModule_SetModuleUserACLString = None;
            redis_module::raw::RedisModule_GetClientCertificate = None;
        }
        let first = ctx
            .deauthenticate_and_close_client(client)
            .unwrap_err()
            .to_string();
        unsafe {
            redis_module::raw::RedisModule_DeauthenticateAndCloseClient = None;
        }
        let results = vec![
            first,
            ctx.deauthenticate_and_close_client(client)
                .unwrap_err()
                .to_string(),
            ctx.set_module_user_acl_string(&stale, "on")
                .unwrap_err()
                .to_string(),
            ctx.remove_module_user(&stale).unwrap_err().to_string(),
            ctx.client_certificate(client).unwrap_err().to_string(),
        ];
        unsafe {
            redis_module::raw::RedisModule_DeauthenticateAndCloseClient = revoke;
            redis_module::raw::RedisModule_SetModuleUserACLString = acl;
            redis_module::raw::RedisModule_GetClientCertificate = cert;
        }
        *WORKER_RESULTS.lock().unwrap() = results;
    }));
    Ok("OK".into())
}
fn worker_results(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut results = WORKER_RESULTS.lock().unwrap().clone();
    results.push(NATIVE_WORKER_CALLS.load(Ordering::Relaxed).to_string());
    Ok(results.into())
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
    RAW_PROBE.store(false, Ordering::Relaxed);
    RAW_TARGET.lock(ctx).take();
    RAW_ERRORS.lock().unwrap().clear();
    // A lawful raw event integration may call safe APIs using its actual
    // native context. It is independent of the SDK's auth-change trampoline.
    unsafe {
        let subscribe = redis_module::raw::RedisModule_SubscribeToServerEvent.unwrap();
        if subscribe(
            ctx.get_raw(),
            redis_module::raw::RedisModuleEvent {
                id: redis_module::raw::REDISMODULE_EVENT_CLIENT_CHANGE,
                dataver: 1,
            },
            Some(raw_client_change),
        ) != redis_module::raw::REDISMODULE_OK as i32
        {
            return Status::Err;
        }
    }
    CHANGED.store(0, Ordering::Relaxed);
    DROPPED.store(0, Ordering::Relaxed);
    REENTRY.store(0, Ordering::Relaxed);
    SCOPED_CLEANUP.store(0, Ordering::Relaxed);
    LATER_CLEANUP.store(0, Ordering::Relaxed);
    NATIVE_WORKER_CALLS.store(0, Ordering::Relaxed);
    WORKER_RESULTS.lock().unwrap().clear();
    REFUSE.store(false, Ordering::Relaxed);
    assert!(ctx.create_module_user("init-denied").is_err());
    eprintln!("auth_users initialized");
    if args.first().is_some_and(|a| a.as_slice() == b"fail") {
        Status::Err
    } else {
        Status::Ok
    }
}
fn deinit(ctx: &EventContext) -> Status {
    assert!(ctx.create_module_user("unload-denied").is_err());
    if EARLY_UNLOAD.swap(false, Ordering::Relaxed) {
        let before = DROPPED.load(Ordering::Relaxed);
        let error = ctx
            .call("auth_users.attach", &["owner", "drop-panic"])
            .unwrap_err()
            .to_string();
        assert!(error.contains("lifecycle is not active"), "{error}");
        assert_eq!(DROPPED.load(Ordering::Relaxed), before + 1);
        eprintln!("auth_users inactive callback drop contained");
    }
    if WORKER.lock().unwrap().is_some() || REFUSE.swap(false, Ordering::Relaxed) {
        Status::Err
    } else {
        // Handles are passive; dropping application state before SDK teardown
        // leaves the native owners for the successful unload commit.
        USERS.lock(ctx).clear();
        RAW_TARGET.lock(ctx).take();
        RAW_ERRORS.lock().unwrap().clear();
        WORKER_RESULTS.lock().unwrap().clear();
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
        redis_command!("auth_users.capabilities", capabilities).read_only(),
        redis_command!("auth_users.alias", alias).read_only(),
        redis_command!("auth_users.forget", forget).read_only(),
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
        redis_command!("auth_users.worker_results", worker_results).read_only(),
        redis_command!("auth_users.reentry", reentry).write(),
        redis_command!("auth_users.reentry_result", reentry_result).read_only(),
        redis_command!("auth_users.absent_api", absent_api).read_only(),
        redis_command!("auth_users.attach_pubsub_cleanup", attach_pubsub_cleanup).write(),
        redis_command!("auth_users.restrict_pubsub", restrict_pubsub).write(),
        redis_command!("auth_users.yield_reentry", yield_reentry).write(),
        redis_command!("auth_users.cleanup_timing", cleanup_timing).read_only(),
        redis_command!("auth_users.raw_probe_result", raw_probe_result).read_only(),
        redis_command!("auth_users.early_unload", early_unload).read_only(),
        redis_command!("auth_users.finish", finish).read_only(),
    ],
}
