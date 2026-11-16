//! Module-owned users and privileged identity attachment. Commands here are an
//! administrative demonstration; production modules must authorize callers.
use redis_module::{
    ClientChangeCallback, ClientId, Context, EventContext, ModuleUserHandle, RedisError,
    RedisGILGuard, RedisResult, RedisString, RedisValue, RegistrationContext, Status,
    ThreadSafeContext, redis_command, redis_module,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

static USERS: RedisGILGuard<BTreeMap<String, ModuleUserHandle>> =
    RedisGILGuard::new(BTreeMap::new());
static CLIENTS: Mutex<BTreeSet<ClientId>> = Mutex::new(BTreeSet::new());
scoped_tls::scoped_thread_local!(static CURRENT: Context);
static FULL_SET_CALLS: AtomicI64 = AtomicI64::new(0);
static FOREIGN_CHANGED: AtomicI64 = AtomicI64::new(0);
static FOREIGN_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CHECKED_NATIVE_CALLS: AtomicI64 = AtomicI64::new(0);
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
    CLIENTS.lock().unwrap().insert(id);
    Ok((id.0 as i64).into())
}
fn attach_acl(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let id =
        ctx.authenticate_client_with_acl_user(arg(&args, 1)?.as_bytes(), callback(arg(&args, 2)?))?;
    CLIENTS.lock().unwrap().insert(id);
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
            let mut results = vec![
                ctx.set_module_user_acl(&target, "off")
                    .unwrap_err()
                    .to_string(),
            ];
            if CURRENT.is_set() {
                CURRENT.with(|command| {
                    results.push(command.remove_module_user(&target).unwrap_err().to_string());
                    results.push(
                        command
                            .set_module_user_acl_string(&target, "reset off")
                            .unwrap_err()
                            .to_string(),
                    );
                });
            }
            *RAW_ERRORS.lock().unwrap() = results;
        },
        || (),
    );
}
fn raw_probe_result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RAW_ERRORS.lock().unwrap().clone().into())
}
unsafe extern "C" fn foreign_changed(_: u64, _: *mut std::ffi::c_void) {
    redis_module::panic::run_callback(
        "foreign auth cleanup",
        || {
            FOREIGN_CHANGED.fetch_add(1, Ordering::Relaxed);
            if CURRENT.is_set() {
                CURRENT.with(|ctx| {
                    let target = RAW_TARGET.lock(ctx).as_ref().unwrap().clone();
                    let errors = vec![
                        ctx.remove_module_user(&target).unwrap_err().to_string(),
                        ctx.set_module_user_acl(&target, "off")
                            .unwrap_err()
                            .to_string(),
                        ctx.set_module_user_acl_string(&target, "reset off")
                            .unwrap_err()
                            .to_string(),
                        ctx.authenticate_client_with_acl_user(b"default", callback("track"))
                            .unwrap_err()
                            .to_string(),
                        ctx.authenticate_client_with_user(
                            &user(ctx, "other").unwrap(),
                            callback("track"),
                        )
                        .unwrap_err()
                        .to_string(),
                    ];
                    *FOREIGN_ERRORS.lock().unwrap() = errors;
                });
            }
        },
        || (),
    );
}
unsafe extern "C" fn refused_full_set(
    _: *mut redis_module::raw::RedisModuleCtx,
    _: *mut redis_module::raw::RedisModuleUser,
    _: *const std::ffi::c_char,
    _: *mut *mut redis_module::raw::RedisModuleString,
) -> i32 {
    FULL_SET_CALLS.fetch_add(1, Ordering::Relaxed);
    redis_module::raw::REDISMODULE_ERR as i32
}
unsafe extern "C" fn invalid_runtime_version() -> i32 {
    -1
}
fn full_refusal_probe(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let handle = user(ctx, arg(&args, 1)?)?;
    let (setter, version) = unsafe {
        (
            redis_module::raw::RedisModule_SetModuleUserACLString,
            redis_module::raw::RedisModule_GetServerVersion,
        )
    };
    FULL_SET_CALLS.store(0, Ordering::Relaxed);
    unsafe {
        redis_module::raw::RedisModule_SetModuleUserACLString = Some(refused_full_set);
    }
    let mut outcomes = Vec::new();
    let encoded = version.map_or(0, |get| unsafe { get() });
    if encoded > 0 && encoded < 0x00070400 {
        outcomes.push(
            ctx.set_module_user_acl_string(&handle, "reset off")
                .unwrap_err()
                .to_string(),
        );
    } else {
        outcomes.push("supported".to_owned());
    }
    unsafe {
        redis_module::raw::RedisModule_GetServerVersion = None;
    }
    outcomes.push(
        ctx.set_module_user_acl_string(&handle, "reset off")
            .unwrap_err()
            .to_string(),
    );
    unsafe {
        redis_module::raw::RedisModule_GetServerVersion = Some(invalid_runtime_version);
    }
    outcomes.push(
        ctx.set_module_user_acl_string(&handle, "reset off")
            .unwrap_err()
            .to_string(),
    );
    unsafe {
        redis_module::raw::RedisModule_SetModuleUserACLString = setter;
        redis_module::raw::RedisModule_GetServerVersion = version;
    }
    outcomes.push(FULL_SET_CALLS.load(Ordering::Relaxed).to_string());
    Ok(outcomes.into())
}
fn foreign_identity(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let name = arg(&args, 1)?.as_bytes();
    let mut id = 0;
    // A lawful raw integration installs a different native callback, outside
    // the SDK ownership registry. No borrowed account pointer is retained.
    let status = unsafe {
        redis_module::raw::RedisModule_AuthenticateClientWithACLUser.unwrap()(
            ctx.get_raw(),
            name.as_ptr().cast(),
            name.len(),
            Some(foreign_changed),
            std::ptr::null_mut(),
            &mut id,
        )
    };
    if status != redis_module::raw::REDISMODULE_OK as i32 {
        return Err(RedisError::Str("ERR foreign fixture attachment failed"));
    }
    Ok((id as i64).into())
}
fn pending_probe(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let target = user(ctx, "owner")?;
    *RAW_TARGET.lock(ctx) = Some(target.clone());
    let result = CURRENT.set(ctx, || ctx.authenticate_client_with_user(&target, None));
    RAW_TARGET.lock(ctx).take();
    let id = result?;
    CLIENTS.lock().unwrap().insert(id);
    Ok((id.0 as i64).into())
}
unsafe extern "C" fn forbidden_checked_attach(
    _: *mut redis_module::raw::RedisModuleCtx,
    _: *const std::ffi::c_char,
    _: usize,
    _: redis_module::raw::RedisModuleUserChangedFunc,
    _: *mut std::ffi::c_void,
    _: *mut u64,
) -> i32 {
    CHECKED_NATIVE_CALLS.fetch_add(1, Ordering::Relaxed);
    redis_module::raw::REDISMODULE_ERR as i32
}
fn checked_probe(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let original = unsafe { redis_module::raw::RedisModule_AuthenticateClientWithACLUser };
    unsafe {
        redis_module::raw::RedisModule_AuthenticateClientWithACLUser =
            Some(forbidden_checked_attach);
    }
    let result = ctx.authenticate_client_with_acl_user(b"default", callback("track"));
    unsafe {
        redis_module::raw::RedisModule_AuthenticateClientWithACLUser = original;
    }
    Ok(vec![
        result.unwrap_err().to_string(),
        CHECKED_NATIVE_CALLS.load(Ordering::Relaxed).to_string(),
    ]
    .into())
}
fn foreign_results(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut results = FOREIGN_ERRORS.lock().unwrap().clone();
    results.push(FOREIGN_CHANGED.load(Ordering::Relaxed).to_string());
    Ok(results.into())
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
    CLIENTS.lock().unwrap().insert(id);
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
                "same-client" => assert!(ctx.deauthenticate_and_close_client(client).is_err()),
                "remove-incoming" => assert!(ctx.remove_module_user(&incoming).is_err()),
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
    CLIENTS.lock().unwrap().insert(id);
    Ok((id.0 as i64).into())
}
fn reentry_result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(REENTRY.load(Ordering::Relaxed).into())
}
fn worker(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
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
        let cert = unsafe { redis_module::raw::RedisModule_GetClientCertificate };
        unsafe {
            redis_module::raw::RedisModule_GetClientCertificate = None;
        }
        let results = vec![ctx.client_certificate(client).unwrap_err().to_string()];
        unsafe {
            redis_module::raw::RedisModule_GetClientCertificate = cert;
        }
        *WORKER_RESULTS.lock().unwrap() = results;
    }));
    let _ = ctx;
    Ok("OK".into())
}
// Explicit command-side cleanup. Keep handles until native removal succeeds;
// aliases and disconnected/replaced client IDs may already be stale.
fn drain(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let clients: Vec<_> = CLIENTS.lock().unwrap().iter().copied().collect();
    for client in clients {
        let _ = ctx.deauthenticate_and_close_client(client);
    }
    CLIENTS.lock().unwrap().clear();
    let handles: Vec<_> = USERS.lock(ctx).values().cloned().collect();
    for handle in handles {
        match ctx.remove_module_user(&handle) {
            Ok(()) => (),
            Err(error) if error.to_string().contains("stale or foreign") => (),
            Err(error) => return Err(error),
        }
    }
    USERS.lock(ctx).clear();
    Ok("OK".into())
}
unsafe extern "C" fn refused_revoke(_: *mut redis_module::raw::RedisModuleCtx, client: u64) -> i32 {
    CHECKED_NATIVE_CALLS.fetch_add(1, Ordering::Relaxed);
    CURRENT.with(|ctx| {
        let other = user(ctx, "other").unwrap();
        *RAW_ERRORS.lock().unwrap() = vec![
            ctx.authenticate_client_with_user(&other, callback("drop-panic"))
                .unwrap_err()
                .to_string(),
            ctx.authenticate_client_with_acl_user(b"default", callback("drop-panic"))
                .unwrap_err()
                .to_string(),
            ctx.deauthenticate_and_close_client(ClientId(client))
                .unwrap_err()
                .to_string(),
            ctx.remove_module_user(&other).unwrap_err().to_string(),
            ctx.set_module_user_acl_string(&other, "off")
                .unwrap_err()
                .to_string(),
        ];
    });
    redis_module::raw::REDISMODULE_ERR as i32
}
fn revocation_probe(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let id = ClientId(
        arg(&args, 1)?
            .parse()
            .map_err(|_| RedisError::Str("ERR invalid ID"))?,
    );
    let original = unsafe { redis_module::raw::RedisModule_DeauthenticateAndCloseClient };
    CHECKED_NATIVE_CALLS.store(0, Ordering::Relaxed);
    unsafe {
        redis_module::raw::RedisModule_DeauthenticateAndCloseClient = Some(refused_revoke);
    }
    let result = CURRENT.set(ctx, || ctx.deauthenticate_and_close_client(id));
    unsafe {
        redis_module::raw::RedisModule_DeauthenticateAndCloseClient = original;
    }
    Ok(vec![
        result.unwrap_err().to_string(),
        CHECKED_NATIVE_CALLS.load(Ordering::Relaxed).to_string(),
    ]
    .into())
}
fn reply_refusals(ctx: &Context, handle: ModuleUserHandle) -> RedisResult {
    let (attach, acl, full, revoke) = unsafe {
        (
            redis_module::raw::RedisModule_AuthenticateClientWithUser,
            redis_module::raw::RedisModule_AuthenticateClientWithACLUser,
            redis_module::raw::RedisModule_SetModuleUserACLString,
            redis_module::raw::RedisModule_DeauthenticateAndCloseClient,
        )
    };
    unsafe {
        redis_module::raw::RedisModule_AuthenticateClientWithUser = None;
        redis_module::raw::RedisModule_AuthenticateClientWithACLUser = None;
        redis_module::raw::RedisModule_SetModuleUserACLString = None;
        redis_module::raw::RedisModule_DeauthenticateAndCloseClient = None;
    }
    // The removed handle also makes an admission regression fail without native
    // user free. All other mutation entry points have missing inert APIs.
    let errors = vec![
        ctx.authenticate_client_with_user(&handle, callback("drop-panic"))
            .unwrap_err()
            .to_string(),
        ctx.authenticate_client_with_acl_user(b"default", callback("drop-panic"))
            .unwrap_err()
            .to_string(),
        ctx.remove_module_user(&handle).unwrap_err().to_string(),
        ctx.set_module_user_acl_string(&handle, "off")
            .unwrap_err()
            .to_string(),
        ctx.deauthenticate_and_close_client(ClientId(0))
            .unwrap_err()
            .to_string(),
    ];
    unsafe {
        redis_module::raw::RedisModule_AuthenticateClientWithUser = attach;
        redis_module::raw::RedisModule_AuthenticateClientWithACLUser = acl;
        redis_module::raw::RedisModule_SetModuleUserACLString = full;
        redis_module::raw::RedisModule_DeauthenticateAndCloseClient = revoke;
    }
    Ok(errors.into())
}
fn internal_refusals(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    reply_refusals(ctx, user(ctx, "reply-stale")?)
}
fn call_refusals(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.call("auth_users.internal_refusals", &[] as &[&str])
}
fn reply_timeout(ctx: &Context) -> RedisResult {
    let stale = user(ctx, "reply-stale")?;
    reply_refusals(ctx, stale)
}
fn reply_probe(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let stale = ctx.create_module_user("reply-stale")?;
    ctx.remove_module_user(&stale)?;
    USERS
        .lock(ctx)
        .insert("reply-stale".to_owned(), stale.clone());
    let timeout = arg(&args, 1)? == "timeout";
    let blocked = ctx.block_client_on_reply(
        timeout.then_some(std::time::Duration::from_millis(1)),
        reply_refusals,
        reply_timeout,
    )?;
    if timeout {
        *WORKER.lock().unwrap() = Some(std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            blocked.unblock(stale);
        }));
    } else {
        blocked.unblock(stale);
    }
    Ok(RedisValue::NoReply)
}
fn alias_source(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    inactive_probe(ctx, args)
}
fn inactive_probe(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.authenticate_client_with_acl_user(b"default", callback("drop-panic"))?;
    Ok("unexpected".into())
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
    CLIENTS.lock().unwrap().clear();
    RAW_PROBE.store(false, Ordering::Relaxed);
    RAW_TARGET.lock(ctx).take();
    RAW_ERRORS.lock().unwrap().clear();
    FOREIGN_ERRORS.lock().unwrap().clear();
    FOREIGN_CHANGED.store(0, Ordering::Relaxed);
    CHECKED_NATIVE_CALLS.store(0, Ordering::Relaxed);
    let alias_callback = redis_module::__redis_command_callback!(alias_source);
    if redis_module::commands::CommandBuilder::__new("auth_users.alias_source", alias_callback)
        .read_only()
        .register(ctx)
        .is_err()
    {
        return Status::Err;
    }
    // A lawful raw alias reuses a known SDK callback address but has no SDK
    // registration-name provenance. It must not inherit the safe source name.
    let supports_busy = unsafe {
        redis_module::raw::RedisModule_GetServerVersion.is_some_and(|version| version() >= 0x070000)
    };
    if supports_busy
        && redis_module::commands::CommandBuilder::__new(
            "auth_users.busy_probe",
            redis_module::__redis_command_callback!(inactive_probe),
        )
        .read_only()
        .flags(redis_module::CommandFlags::ALLOW_BUSY)
        .register(ctx)
        .is_err()
    {
        return Status::Err;
    }
    if unsafe {
        redis_module::raw::RedisModule_CreateCommand.unwrap()(
            ctx.get_raw(),
            c"auth_users.raw_busy_alias".as_ptr(),
            Some(alias_callback),
            if supports_busy { c"allow-busy" } else { c"" }.as_ptr(),
            0,
            0,
            0,
        )
    } != redis_module::raw::REDISMODULE_OK as i32
    {
        return Status::Err;
    }
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
    eprintln!("auth_users deinit");
    assert!(ctx.create_module_user("unload-denied").is_err());
    if EARLY_UNLOAD.swap(false, Ordering::Relaxed) {
        let before = DROPPED.load(Ordering::Relaxed);
        let error = ctx
            .call("auth_users.inactive_probe", &[] as &[&str])
            .unwrap_err()
            .to_string();
        assert!(error.contains("lifecycle is not active"), "{error}");
        assert_eq!(DROPPED.load(Ordering::Relaxed), before + 1);
        eprintln!("auth_users inactive callback drop contained");
    }
    if WORKER.lock().unwrap().is_some() || REFUSE.swap(false, Ordering::Relaxed) {
        Status::Err
    } else {
        // The SDK admitted unload only after explicit command-side drain.
        USERS.lock(ctx).clear();
        RAW_TARGET.lock(ctx).take();
        RAW_ERRORS.lock().unwrap().clear();
        FOREIGN_ERRORS.lock().unwrap().clear();
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
        redis_command!("auth_users.foreign", foreign_identity).write(),
        redis_command!("auth_users.full_refusal_probe", full_refusal_probe).read_only(),
        redis_command!("auth_users.pending_probe", pending_probe).write(),
        redis_command!("auth_users.checked_probe", checked_probe).write(),
        redis_command!("auth_users.foreign_results", foreign_results).read_only(),
        redis_command!("auth_users.early_unload", early_unload).read_only(),
        redis_command!("auth_users.finish", finish).read_only(),
        redis_command!("auth_users.drain", drain).write(),
        redis_command!("auth_users.revocation_probe", revocation_probe).write(),
        redis_command!("auth_users.reply_probe", reply_probe).write(),
        redis_command!("auth_users.internal_refusals", internal_refusals).read_only(),
        redis_command!("auth_users.call_refusals", call_refusals).read_only(),
        redis_command!("auth_users.inactive_probe", inactive_probe).read_only(),

    ],
}
