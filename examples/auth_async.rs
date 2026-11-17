//! Experimental auth fixture. Administrative commands require application ACLs.
//! Fixed credentials and std threads are test behavior, not a provider adapter.
use redis_module::{
    AuthCompletion, AuthContext, AuthDecision, AuthRequest, AuthTarget, Context, EventContext,
    ModuleUserHandle, PublicAuthError, RedisError, RedisGILGuard, RedisResult, RedisString, Status,
    redis_command, redis_module,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static USERS: RedisGILGuard<BTreeMap<&'static str, ModuleUserHandle>> =
    RedisGILGuard::new(BTreeMap::new());
static HELD: Mutex<Vec<(AuthCompletion, AuthTarget)>> = Mutex::new(Vec::new());
static WORKERS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());
static STARTED: AtomicUsize = AtomicUsize::new(0);
static CHANGED: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);
static BLOCK_CALLS: AtomicUsize = AtomicUsize::new(0);
type ClockFn = unsafe extern "C" fn() -> u64;
static ORIGINAL_CLOCK: OnceLock<ClockFn> = OnceLock::new();
static OBSERVING: AtomicBool = AtomicBool::new(false);
static OBSERVATION_ERROR: AtomicBool = AtomicBool::new(false);
static OBSERVATIONS: Mutex<Vec<i64>> = Mutex::new(Vec::new());
const OBSERVATION_LIMIT: usize = 256;
#[derive(Clone, Copy)]
enum ClockSpan {
    Idle,
    Armed,
    Running(Instant),
    Done,
}
thread_local! { static CLOCK_SPAN: Cell<ClockSpan> = const { Cell::new(ClockSpan::Idle) }; }

// Fixture-only observation of the real SDK begin_with clock0 -> clock1 span.
// It includes prepared admission/role checks, not just Receipt::consistent.
// Captured job clocks keep this forwarding function until verified empty unload.
unsafe extern "C" fn observed_clock() -> u64 {
    let Some(original) = ORIGINAL_CLOCK.get() else {
        return 0;
    };
    let value = unsafe { original() };
    let elapsed = CLOCK_SPAN
        .try_with(|span| match span.get() {
            ClockSpan::Armed => {
                span.set(ClockSpan::Running(Instant::now()));
                None
            }
            ClockSpan::Running(start) => {
                span.set(ClockSpan::Done);
                Some(start.elapsed())
            }
            _ => None,
        })
        .ok()
        .flatten();
    if let Some(elapsed) = elapsed {
        let mut samples = OBSERVATIONS.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(nanos) = i64::try_from(elapsed.as_nanos()) {
            if samples.len() < OBSERVATION_LIMIT {
                samples.push(nanos);
            } else {
                OBSERVATION_ERROR.store(true, Ordering::Relaxed);
            }
        } else {
            OBSERVATION_ERROR.store(true, Ordering::Relaxed);
        }
    }
    value
}
fn observe(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    // Require the same quiescent ordinary-command boundary as async cleanup.
    ctx.reap_async_auth(0)?;
    match args.get(1).ok_or(RedisError::WrongArity)?.try_as_str()? {
        "prepare" => {
            let start = Instant::now();
            let receipt = ctx.prepare_async_auth_runtime()?;
            let nanos = i64::try_from(start.elapsed().as_nanos())
                .map_err(|_| RedisError::Str("ERR observation overflow"))?;
            Ok(vec![receipt.generation as i64, nanos].into())
        }
        "start" => {
            let current = unsafe { redis_module::raw::RedisModule_MonotonicMicroseconds }
                .ok_or(RedisError::Str("ERR clock unavailable"))?;
            if let Some(original) = ORIGINAL_CLOCK.get() {
                if !std::ptr::fn_addr_eq(current, *original)
                    && !std::ptr::fn_addr_eq(current, observed_clock as ClockFn)
                {
                    return Err(RedisError::Str("ERR clock slot changed"));
                }
            } else {
                ORIGINAL_CLOCK
                    .set(current)
                    .map_err(|_| RedisError::Str("ERR clock already installed"))?;
            }
            *OBSERVATIONS.lock().unwrap() = Vec::with_capacity(OBSERVATION_LIMIT);
            OBSERVATION_ERROR.store(false, Ordering::Relaxed);
            CLOCK_SPAN.with(|span| span.set(ClockSpan::Idle));
            unsafe {
                redis_module::raw::RedisModule_MonotonicMicroseconds = Some(observed_clock);
            }
            OBSERVING.store(true, Ordering::Relaxed);
            Ok("OK".into())
        }
        "baseline" => {
            if !OBSERVING.load(Ordering::Relaxed) {
                return Err(RedisError::Str("ERR observer inactive"));
            }
            OBSERVATIONS.lock().unwrap().clear();
            for _ in 0..200 {
                CLOCK_SPAN.with(|span| span.set(ClockSpan::Armed));
                unsafe {
                    observed_clock();
                    observed_clock();
                }
            }
            CLOCK_SPAN.with(|span| span.set(ClockSpan::Idle));
            Ok(OBSERVATIONS.lock().unwrap().clone().into())
        }
        "reset" => {
            let mut samples = OBSERVATIONS.lock().unwrap();
            let count = samples.len();
            samples.clear();
            Ok((count as i64).into())
        }
        "samples" => {
            if OBSERVATION_ERROR.load(Ordering::Relaxed) {
                return Err(RedisError::Str("ERR observation overflow"));
            }
            Ok(OBSERVATIONS.lock().unwrap().clone().into())
        }
        "status" => Ok(vec![
            i64::from(OBSERVING.load(Ordering::Relaxed)),
            i64::from(
                unsafe { redis_module::raw::RedisModule_MonotonicMicroseconds }
                    .is_some_and(|clock| std::ptr::fn_addr_eq(clock, observed_clock as ClockFn)),
            ),
            OBSERVATIONS.lock().unwrap().len() as i64,
        ]
        .into()),
        "stop" => {
            OBSERVING.store(false, Ordering::Relaxed);
            CLOCK_SPAN.with(|span| span.set(ClockSpan::Idle));
            *OBSERVATIONS.lock().unwrap() = Vec::new();
            // Jobs may retain observed_clock. Restore only at empty deinit.
            Ok("OK".into())
        }
        _ => Err(RedisError::Str("ERR unknown observation")),
    }
}
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
        b"async" | b"observe" => 0,
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
    if request.username() == b"observe"
        && (!OBSERVING.load(Ordering::Relaxed)
            || CLOCK_SPAN
                .try_with(|span| span.set(ClockSpan::Armed))
                .is_err())
    {
        return AuthDecision::Deny(PublicAuthError::default());
    }
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
fn deinit(_: &EventContext) -> Status {
    // Application worker stacks are separate from SDK completion ownership.
    // Command-side drain must join them before native module unload.
    if !WORKERS.lock().unwrap().is_empty() || !HELD.lock().unwrap().is_empty() {
        Status::Err
    } else {
        if let Some(original) = ORIGINAL_CLOCK.get() {
            let Some(current) = (unsafe { redis_module::raw::RedisModule_MonotonicMicroseconds })
            else {
                return Status::Err;
            };
            if !std::ptr::fn_addr_eq(current, *original)
                && !std::ptr::fn_addr_eq(current, observed_clock as ClockFn)
            {
                return Status::Err;
            }
            unsafe {
                redis_module::raw::RedisModule_MonotonicMicroseconds = Some(*original);
            }
        }
        OBSERVING.store(false, Ordering::Relaxed);
        let _ = CLOCK_SPAN.try_with(|span| span.set(ClockSpan::Idle));
        *OBSERVATIONS.lock().unwrap() = Vec::new();
        Status::Ok
    }
}
redis_module! {
    name: "auth_async", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], deinit: deinit, auth: authenticate,
    commands: [
        redis_command!("auth_async.users", users).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.prepare", prepare).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.reap", reap).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.release", release).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.drain", drain).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.stats", stats).read_only(),
        redis_command!("auth_async.probe", probe).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("auth_async.observe", observe).flags(redis_module::CommandFlags::ADMIN),
    ],
}
