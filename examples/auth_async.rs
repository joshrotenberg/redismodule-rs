//! Experimental auth fixture. Administrative commands require application ACLs.
//! Fixed credentials and std threads are test behavior, not a provider adapter.
use redis_module::{
    AuthCompletion, AuthContext, AuthDecision, AuthRequest, AuthTarget, Context, ModuleUserHandle,
    PublicAuthError, RedisError, RedisGILGuard, RedisResult, RedisString, redis_command,
    redis_module,
};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

static USERS: RedisGILGuard<BTreeMap<&'static str, ModuleUserHandle>> =
    RedisGILGuard::new(BTreeMap::new());
static HELD: Mutex<Vec<(AuthCompletion, AuthTarget)>> = Mutex::new(Vec::new());
static WORKERS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());
static STARTED: AtomicUsize = AtomicUsize::new(0);
static CHANGED: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);
static BLOCK_CALLS: AtomicUsize = AtomicUsize::new(0);
type BlockFn = unsafe extern "C" fn(
    *mut redis_module::raw::RedisModuleCtx,
    redis_module::raw::RedisModuleAuthCallback,
    Option<unsafe extern "C" fn(*mut redis_module::raw::RedisModuleCtx, *mut std::ffi::c_void)>,
) -> *mut redis_module::raw::RedisModuleBlockedClient;
static BLOCK_API: Mutex<Option<Option<BlockFn>>> = Mutex::new(None);
unsafe extern "C" fn refused_block(
    _: *mut redis_module::raw::RedisModuleCtx,
    _: redis_module::raw::RedisModuleAuthCallback,
    _: Option<unsafe extern "C" fn(*mut redis_module::raw::RedisModuleCtx, *mut std::ffi::c_void)>,
) -> *mut redis_module::raw::RedisModuleBlockedClient {
    BLOCK_CALLS.fetch_add(1, Ordering::Relaxed);
    std::ptr::null_mut()
}
fn probe(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mode = args.get(1).ok_or(RedisError::WrongArity)?.try_as_str()?;
    if mode == "count" {
        return Ok((BLOCK_CALLS.load(Ordering::Relaxed) as i64).into());
    }
    if mode == "reset" {
        if let Some(original) = BLOCK_API.lock().unwrap().take() {
            unsafe {
                redis_module::raw::RedisModule_BlockClientOnAuth = original;
            }
        }
        return Ok("OK".into());
    }
    if mode != "missing" && mode != "counted" {
        return Err(RedisError::Str("ERR unknown probe"));
    }
    let mut saved = BLOCK_API.lock().unwrap();
    if saved.is_none() {
        *saved = Some(unsafe { redis_module::raw::RedisModule_BlockClientOnAuth });
    }
    BLOCK_CALLS.store(0, Ordering::Relaxed);
    unsafe {
        redis_module::raw::RedisModule_BlockClientOnAuth = if mode == "missing" {
            None
        } else {
            Some(refused_block)
        };
    }
    Ok("OK".into())
}
struct Track;
impl Drop for Track {
    fn drop(&mut self) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

fn authenticate(ctx: &AuthContext<'_>, request: AuthRequest<'_>) -> AuthDecision {
    let mode = match request.username() {
        b"async" => 0,
        b"deny" => 1,
        b"fallback" => 2,
        b"hold" => 3,
        b"panic" => 4,
        b"drop" => 5,
        b"stale" => 6,
        b"disabled" => 7,
        _ => return AuthDecision::NotHandled,
    };
    let key = match mode {
        6 => "stale",
        7 => "disabled",
        _ => "active",
    };
    let Some(user) = USERS.lock(ctx).get(key).cloned() else {
        return AuthDecision::Deny(PublicAuthError::default());
    };
    let secret = request.copy_for_async();
    let state = Track;
    ctx.defer(Duration::from_secs(1), move |completion| {
        STARTED.fetch_add(1, Ordering::Relaxed);
        let target = AuthTarget::module_user(user).on_changed(Box::new(move |_| {
            CHANGED.fetch_add(1, Ordering::Relaxed);
            drop(state);
        }));
        match mode {
            3 => HELD.lock().unwrap().push((completion, target)),
            4 => panic!("fixed test starter panic"),
            5 => drop(completion),
            _ => {
                let worker = std::thread::spawn(move || {
                    if mode == 1 || secret.password() != b"test-password" {
                        completion.deny(PublicAuthError::default());
                    } else if mode == 2 {
                        completion.not_handled();
                    } else {
                        completion.accept(target);
                    }
                });
                WORKERS.lock().unwrap().push(worker);
            }
        }
    })
}
fn users(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    if !USERS.lock(ctx).is_empty() {
        return Err(RedisError::Str("ERR drain users first"));
    }
    for key in ["active", "disabled", "stale"] {
        let user = ctx.create_module_user(&format!("async-{key}"))?;
        USERS.lock(ctx).insert(key, user.clone());
        ctx.set_module_user_acl(&user, "+@all")?;
        ctx.set_module_user_acl(&user, "~*")?;
        if key != "disabled" {
            ctx.set_module_user_acl(&user, "on")?;
        }
        if key == "stale" {
            ctx.remove_module_user(&user)?;
        }
    }
    Ok("OK".into())
}
fn prepare(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok((ctx.prepare_async_auth_runtime()?.generation as i64).into())
}
fn reap(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok((ctx.reap_async_auth(1024)? as i64).into())
}
fn release(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let held = std::mem::take(&mut *HELD.lock().unwrap());
    for (completion, target) in held {
        completion.accept(target);
    }
    Ok("OK".into())
}
fn stats(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        STARTED.load(Ordering::Relaxed) as i64,
        CHANGED.load(Ordering::Relaxed) as i64,
        DROPPED.load(Ordering::Relaxed) as i64,
        HELD.lock().unwrap().len() as i64,
    ]
    .into())
}
fn drain(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.cancel_async_auth()?;
    let held = std::mem::take(&mut *HELD.lock().unwrap());
    for (completion, _) in held {
        completion.deny(PublicAuthError::default());
    }
    let workers = std::mem::take(&mut *WORKERS.lock().unwrap());
    let mut pending = Vec::new();
    for worker in workers {
        if worker.is_finished() {
            let _ = worker.join();
        } else {
            pending.push(worker);
        }
    }
    WORKERS.lock().unwrap().extend(pending);
    ctx.reap_async_auth(1024)?;
    ctx.drain_client_identities()?;
    ctx.reap_async_auth(1024)?;
    let handles: Vec<_> = USERS
        .lock(ctx)
        .iter()
        .map(|(key, user)| (*key, user.clone()))
        .collect();
    for (key, user) in handles {
        match ctx.remove_module_user(&user) {
            Ok(()) => (),
            Err(error) if error.to_string().contains("stale or foreign") => (),
            Err(error) => return Err(error),
        }
        USERS.lock(ctx).remove(key);
    }
    Ok("OK".into())
}
redis_module! {
    name: "auth_async", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], auth: authenticate,
    commands: [
        redis_command!("auth_async.users", users).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.prepare", prepare).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.reap", reap).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.release", release).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.drain", drain).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.stats", stats).read_only(),
        redis_command!("auth_async.probe", probe).flags(redis_module::CommandFlags::ADMIN),
    ],
}
