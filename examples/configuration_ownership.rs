//! Native CONFIG frames outlive an individual SDK setter/observer callback.
//! Every probe refuses unload in deinit so the baseline cannot unmap live code.
use std::cell::Cell;
use std::ffi::{c_char, c_int, c_longlong, c_void};
use std::io::Write;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread::{self, JoinHandle};

use redis_module::configuration::{
    ConfigurationFlags, load_configurations, register_i64_configuration,
    register_string_configuration,
};
use redis_module::{
    Context, EventContext, NextArg, RedisError, RedisResult, RedisString, RedisValue,
    RegistrationContext, RuntimeContext, Status, ThreadSafeContext, raw, redis_module,
};

static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static TEXT: Mutex<String> = Mutex::new(String::new());
static VICTIM: AtomicI64 = AtomicI64::new(0);
static SECOND: AtomicI64 = AtomicI64::new(0);
static DEINIT_MODE: AtomicI64 = AtomicI64::new(0);
static COMPANION: AtomicI64 = AtomicI64::new(0);
static MODE: AtomicI64 = AtomicI64::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);
static OBSERVED: AtomicI64 = AtomicI64::new(0);
static APPLIED: AtomicI64 = AtomicI64::new(0);
static OBSERVER_RETURNED: AtomicBool = AtomicBool::new(false);
static RETURNED_BEFORE_ATTEMPT: AtomicBool = AtomicBool::new(false);
static ATTEMPTS: AtomicI64 = AtomicI64::new(0);
static DEINIT_ENTERED: AtomicI64 = AtomicI64::new(0);
static UNLOAD_REFUSED: AtomicBool = AtomicBool::new(false);

// Full-layout captures expose incorrect erased deallocation and independent
// cleanup catches. The marker survives even when the native loader unmaps us.
#[repr(align(64))]
struct DropProbe {
    path: Option<String>,
    tag: u8,
    panic_on_drop: bool,
    payload_panic_on_drop: bool,
    reenter_on_drop: bool,
}
impl DropProbe {
    fn touch(&self) {
        std::hint::black_box(self.tag);
    }
}
fn record_drop(path: &Option<String>, tag: u8) {
    let Some(path) = path else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(&[tag]);
    }
}

struct PanicOnDrop;
impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("cleanup panic payload destructor must not run");
    }
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        record_drop(&self.path, self.tag);
        if self.reenter_on_drop {
            teardown_reentry(&self.path);
        }
        if self.payload_panic_on_drop {
            std::panic::panic_any(PanicOnDrop);
        }
        if self.panic_on_drop {
            panic!("ownership capture drop panic {}", self.tag);
        }
    }
}

thread_local! {
    // Installed only around the synchronous CONFIG SET call made by probe().
    static COMMAND_CONTEXT: Cell<*mut raw::RedisModuleCtx> = const { Cell::new(ptr::null_mut()) };
    // Only drop-reenter uses this, during the same native OnUnload invocation
    // that installed it. No failed-load path enables destructor reentry.
    static UNLOAD_CONTEXT: Cell<*mut raw::RedisModuleCtx> = const { Cell::new(ptr::null_mut()) };
}

fn teardown_reentry(path: &Option<String>) {
    UNLOAD_CONTEXT.with(|slot| {
        let source = slot.get();
        assert!(
            !source.is_null(),
            "cleanup reentry must stay inside OnUnload"
        );
        // SAFETY: User deinit installed this live OnUnload context; metadata
        // cleanup runs before native OnUnload returns, holding the Redis lock.
        // A detached context grants the client required by RM_Call, unlike the
        // native unload context. This is private fixture ownership only.
        let get = unsafe { raw::RedisModule_GetDetachedThreadSafeContext }
            .expect("detached context API unavailable");
        let pointer = unsafe { get(source) };
        assert!(!pointer.is_null(), "detached cleanup context is null");
        struct Detached(*mut raw::RedisModuleCtx);
        impl Drop for Detached {
            fn drop(&mut self) {
                // SAFETY: This uniquely owns the context, while still locked.
                unsafe {
                    raw::RedisModule_FreeThreadSafeContext.unwrap()(self.0);
                }
            }
        }
        let owned = Detached(pointer);
        // SAFETY: The context is live, locked, client-bearing, and freed only
        // after this runtime view and all replies leave their scope.
        let ctx = unsafe { RuntimeContext::new(owned.0) };
        let read = |name| -> String {
            let RedisValue::Array(values) = ctx.call("CONFIG", &["GET", name]).unwrap() else {
                panic!("configuration cleanup getter returned a non-array");
            };
            String::try_from(values.last().expect("configuration value missing").clone()).unwrap()
        };
        assert_eq!(read("configuration_ownership.text"), "");
        assert!(
            ctx.call(
                "CONFIG",
                &["SET", "configuration_ownership.text", "changed"]
            )
            .is_err()
        );
        assert_eq!(read("configuration_ownership.victim"), "0");
        assert!(
            ctx.call("CONFIG", &["SET", "configuration_ownership.victim", "2"])
                .is_err()
        );
        record_drop(path, b'R');
    });
}

fn attempt_unload(ctx: &RuntimeContext) {
    RETURNED_BEFORE_ATTEMPT.store(OBSERVER_RETURNED.load(Ordering::SeqCst), Ordering::SeqCst);
    ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    UNLOAD_REFUSED.store(
        ctx.call("MODULE", &["UNLOAD", "configuration_ownership"])
            .is_err(),
        Ordering::SeqCst,
    );
}

fn attempt_unload_from_command() {
    COMMAND_CONTEXT.with(|slot| {
        let pointer = slot.get();
        assert!(
            !pointer.is_null(),
            "callback escaped its synchronous command"
        );
        // SAFETY: ProbeScope installs a live, client-bearing command context only
        // across CONFIG SET. Native callbacks here run synchronously on this
        // locked thread; Redis's apply context itself need not have a client.
        let ctx = unsafe { RuntimeContext::new(pointer) };
        attempt_unload(&ctx);
    });
}

extern "C" fn companion_get(_: *const c_char, _: *mut c_void) -> c_longlong {
    COMPANION.load(Ordering::SeqCst)
}

extern "C" fn companion_set(
    _: *const c_char,
    value: c_longlong,
    _: *mut c_void,
    _: *mut *mut raw::RedisModuleString,
) -> c_int {
    COMPANION.store(value, Ordering::SeqCst);
    raw::REDISMODULE_OK as c_int
}

extern "C" fn companion_apply(
    _: *mut raw::RedisModuleCtx,
    _: *mut c_void,
    _: *mut *mut raw::RedisModuleString,
) -> c_int {
    redis_module::panic::run_callback(
        "ownership companion apply",
        || {
            if MODE.load(Ordering::SeqCst) == 2 {
                APPLIED.fetch_add(1, Ordering::SeqCst);
                attempt_unload_from_command();
            }
            raw::REDISMODULE_OK as c_int
        },
        || raw::REDISMODULE_ERR as c_int,
    )
}

fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    MODE.store(0, Ordering::SeqCst);
    ARMED.store(false, Ordering::SeqCst);
    UNLOAD_CONTEXT.with(|slot| slot.set(ptr::null_mut()));
    DEINIT_ENTERED.store(0, Ordering::SeqCst);
    let mode = args
        .first()
        .and_then(|arg| arg.try_as_str().ok())
        .unwrap_or("");
    let path = args
        .get(1)
        .and_then(|arg| arg.try_as_str().ok())
        .map(str::to_owned);
    DEINIT_MODE.store(
        match mode {
            "deinit-refuse" => 1,
            "deinit-panic" => 2,
            "deinit-read" => 3,
            _ => 0,
        },
        Ordering::SeqCst,
    );
    let first = DropProbe {
        path: path.clone(),
        tag: b'A',
        panic_on_drop: matches!(mode, "drop-panic" | "init-panic-drop-panic"),
        payload_panic_on_drop: mode == "drop-payload-panic",
        reenter_on_drop: false,
    };
    let second = DropProbe {
        path,
        tag: b'B',
        panic_on_drop: matches!(mode, "drop-panic" | "init-panic-drop-panic"),
        payload_panic_on_drop: mode == "drop-payload-panic",
        reenter_on_drop: mode == "drop-reenter",
    };
    let result = (|| {
        register_string_configuration(
            ctx,
            "text",
            &TEXT,
            "live",
            ConfigurationFlags::DEFAULT,
            None,
        )?;
        register_i64_configuration(
            ctx,
            "victim",
            &VICTIM,
            0,
            0,
            100,
            ConfigurationFlags::DEFAULT,
            Some(Box::new(move |_, _, _| {
                first.touch();
                if MODE.load(Ordering::SeqCst) == 0 {
                    return;
                }
                OBSERVED.fetch_add(1, Ordering::SeqCst);
                if MODE.load(Ordering::SeqCst) == 1 {
                    attempt_unload_from_command();
                }
                OBSERVER_RETURNED.store(true, Ordering::SeqCst);
            })),
        )?;
        register_i64_configuration(
            ctx,
            "second",
            &SECOND,
            0,
            0,
            100,
            ConfigurationFlags::DEFAULT,
            Some(Box::new(move |_, _, _| second.touch())),
        )?;
        if mode == "registration-error" {
            register_i64_configuration(
                ctx,
                "victim",
                &VICTIM,
                0,
                0,
                100,
                ConfigurationFlags::DEFAULT,
                None,
            )?;
        }
        // The companion deliberately uses raw native registration. Its apply
        // callback runs after all setters return, outside SDK callback guards.
        let register = unsafe { raw::RedisModule_RegisterNumericConfig }
            .ok_or(RedisError::Str("ERR numeric configuration API unavailable"))?;
        // SAFETY: All callbacks and privdata (unused/null) are static. The
        // registration context and C string remain valid for this native call.
        let status = unsafe {
            register(
                ctx.get_raw(),
                c"companion".as_ptr(),
                0,
                0,
                0,
                100,
                Some(companion_get),
                Some(companion_set),
                Some(companion_apply),
                ptr::null_mut(),
            )
        };
        if status != raw::REDISMODULE_OK as c_int {
            return Err(RedisError::Str(
                "ERR companion configuration registration failed",
            ));
        }
        if mode == "skip-load" {
            Ok(())
        } else {
            load_configurations(ctx)
        }
    })();
    if let Err(error) = result {
        ctx.log_warning(&error.to_string());
        return Status::Err;
    }
    if mode == "init-error" {
        return Status::Err;
    }
    if matches!(mode, "init-panic" | "init-panic-drop-panic") {
        panic!("ownership init panic");
    }
    Status::Ok
}

type PostJobApi = Option<
    unsafe extern "C" fn(
        *mut raw::RedisModuleCtx,
        raw::RedisModulePostNotificationJobFunc,
        *mut c_void,
        Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int,
>;

// Test-only fault injection, scoped entirely within a held Redis lock. Restore
// the API before returning to Redis or releasing a worker's lock, even on panic.
struct PostJobScope {
    saved: PostJobApi,
    changed: bool,
}
impl PostJobScope {
    fn new(disable: bool) -> Self {
        // SAFETY: Every caller holds the Redis lock. No other module thread can
        // access this module's API table until this scope restores the slot.
        unsafe {
            let saved = raw::RedisModule_AddPostNotificationJob;
            if disable {
                raw::RedisModule_AddPostNotificationJob = None;
            }
            Self {
                saved,
                changed: disable,
            }
        }
    }
}
impl Drop for PostJobScope {
    fn drop(&mut self) {
        if self.changed {
            // SAFETY: The lock scope that constructed us still encloses Drop.
            unsafe {
                raw::RedisModule_AddPostNotificationJob = self.saved;
            }
        }
    }
}

struct ProbeScope;
impl Drop for ProbeScope {
    fn drop(&mut self) {
        COMMAND_CONTEXT.with(|slot| slot.set(ptr::null_mut()));
        MODE.store(0, Ordering::SeqCst);
        ARMED.store(false, Ordering::SeqCst);
    }
}

fn probe(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mode = args.into_iter().skip(1).next_string()?;
    let disable_job = mode.ends_with("-no-job");
    let mode = match mode.as_str() {
        "direct" | "direct-no-job" => 1,
        "later" | "later-no-job" => 2,
        _ => return Err(RedisError::Str("ERR expected direct or later")),
    };
    OBSERVED.store(0, Ordering::SeqCst);
    APPLIED.store(0, Ordering::SeqCst);
    OBSERVER_RETURNED.store(false, Ordering::SeqCst);
    RETURNED_BEFORE_ATTEMPT.store(false, Ordering::SeqCst);
    ATTEMPTS.store(0, Ordering::SeqCst);
    DEINIT_ENTERED.store(0, Ordering::SeqCst);
    UNLOAD_REFUSED.store(false, Ordering::SeqCst);
    MODE.store(mode, Ordering::SeqCst);
    ARMED.store(true, Ordering::SeqCst);
    COMMAND_CONTEXT.with(|slot| slot.set(ctx.get_raw()));
    let _scope = ProbeScope;
    let _api = PostJobScope::new(disable_job);
    ctx.call(
        "CONFIG",
        &[
            "SET",
            "configuration_ownership.victim",
            "1",
            "configuration_ownership.companion",
            "1",
        ],
    )?;
    Ok(vec![
        OBSERVED.load(Ordering::SeqCst),
        APPLIED.load(Ordering::SeqCst),
        i64::from(RETURNED_BEFORE_ATTEMPT.load(Ordering::SeqCst)),
        ATTEMPTS.load(Ordering::SeqCst),
        DEINIT_ENTERED.load(Ordering::SeqCst),
        i64::from(UNLOAD_REFUSED.load(Ordering::SeqCst)),
    ]
    .into())
}

fn worker(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let disable_job = args.get(1).is_some_and(|arg| arg.as_slice() == b"no-job");
    let later = args.get(1).is_some_and(|arg| arg.as_slice() == b"later");
    if later {
        OBSERVED.store(0, Ordering::SeqCst);
        APPLIED.store(0, Ordering::SeqCst);
        OBSERVER_RETURNED.store(false, Ordering::SeqCst);
        RETURNED_BEFORE_ATTEMPT.store(false, Ordering::SeqCst);
        ATTEMPTS.store(0, Ordering::SeqCst);
        UNLOAD_REFUSED.store(false, Ordering::SeqCst);
        DEINIT_ENTERED.store(0, Ordering::SeqCst);
    }
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR join the previous worker"));
    }
    if later {
        // Deliberately unassociated: a victim-owned blocked client would make
        // native MODULE UNLOAD refuse before the SDK barrier can be tested.
        *worker = Some(thread::spawn(move || {
            let owner = ThreadSafeContext::new();
            let guard = owner.lock();
            MODE.store(2, Ordering::SeqCst);
            ARMED.store(true, Ordering::SeqCst);
            COMMAND_CONTEXT.with(|slot| slot.set(guard.get_raw()));
            let _scope = ProbeScope;
            if let Err(error) = guard.call(
                "CONFIG",
                &[
                    "SET",
                    "configuration_ownership.victim",
                    "7",
                    "configuration_ownership.companion",
                    "7",
                ],
            ) {
                guard.log_warning(&format!("worker-later fixture CONFIG SET failed: {error}"));
            }
        }));
        return Ok("OK".into());
    }
    let blocked = ctx.block_client()?;
    *worker = Some(thread::spawn(move || {
        let owner = ThreadSafeContext::with_blocked_client(blocked);
        let result = {
            let guard = owner.lock();
            let _api = PostJobScope::new(disable_job);
            guard.call("CONFIG", &["SET", "configuration_ownership.victim", "7"])
        };
        owner.reply(result);
    }));
    Ok(RedisValue::NoReply)
}

fn join_worker() -> Result<bool, RedisError> {
    let mut worker = WORKER.lock().unwrap();
    if worker.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return Ok(false);
    }
    if let Some(handle) = worker.take() {
        handle
            .join()
            .map_err(|_| RedisError::Str("ERR configuration worker panicked"))?;
    }
    Ok(true)
}

fn finish(_: &Context, _: Vec<RedisString>) -> RedisResult {
    join_worker().map(|done| i64::from(done).into())
}

fn post_job_available(_: &Context, _: Vec<RedisString>) -> RedisResult {
    // SAFETY: A command holds Redis's lock; fixture API fault injection cannot
    // overlap this observation and is restored before any command returns.
    Ok(i64::from(unsafe { raw::RedisModule_AddPostNotificationJob }.is_some()).into())
}

fn worker_result(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        OBSERVED.load(Ordering::SeqCst),
        APPLIED.load(Ordering::SeqCst),
        i64::from(RETURNED_BEFORE_ATTEMPT.load(Ordering::SeqCst)),
        ATTEMPTS.load(Ordering::SeqCst),
        DEINIT_ENTERED.load(Ordering::SeqCst),
        i64::from(UNLOAD_REFUSED.load(Ordering::SeqCst)),
    ]
    .into())
}

fn deinit_read(source: *mut raw::RedisModuleCtx) -> Status {
    // SAFETY: Native OnUnload supplied this live context while holding Redis's
    // lock. The derived detached context has the client RM_Call requires.
    let get = unsafe { raw::RedisModule_GetDetachedThreadSafeContext }
        .expect("detached context API unavailable");
    let pointer = unsafe { get(source) };
    assert!(!pointer.is_null());
    let result = {
        let ctx = unsafe { RuntimeContext::new(pointer) };
        ctx.call("CONFIG", &["GET", "configuration_ownership.text"])
    };
    // SAFETY: The temporary runtime view/reply processing is complete; this is
    // the unique owner, and OnUnload still holds the Redis lock.
    unsafe {
        raw::RedisModule_FreeThreadSafeContext.unwrap()(pointer);
    }
    let RedisValue::Array(values) = result.expect("deinit CONFIG GET failed") else {
        panic!("deinit CONFIG GET returned a non-array");
    };
    assert_eq!(
        String::try_from(values.last().expect("deinit value missing").clone()).unwrap(),
        "live"
    );
    Status::Ok
}

fn deinit_count(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(DEINIT_ENTERED.load(Ordering::SeqCst).into())
}

fn deinit(ctx: &EventContext) -> Status {
    UNLOAD_CONTEXT.with(|slot| slot.set(ctx.get_raw()));
    DEINIT_ENTERED.fetch_add(1, Ordering::SeqCst);
    if !join_worker().unwrap_or(false) {
        return Status::Err;
    }
    if ARMED.load(Ordering::SeqCst) {
        return Status::Err;
    }
    match DEINIT_MODE.swap(0, Ordering::SeqCst) {
        1 => Status::Err,
        2 => panic!("ownership deinit panic"),
        3 => deinit_read(ctx.get_raw()),
        _ => Status::Ok,
    }
}

redis_module! {
    name: "configuration_ownership", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], init: init, deinit: deinit,
    commands: [
        ["config_ownership.probe", probe, "", 0, 0, 0, ""],
        ["config_ownership.worker", worker, "", 0, 0, 0, ""],
        ["config_ownership.finish", finish, "", 0, 0, 0, ""],
        ["config_ownership.worker_result", worker_result, "readonly", 0, 0, 0, ""],
        ["config_ownership.deinit_count", deinit_count, "readonly", 0, 0, 0, ""],
        ["config_ownership.post_job_available", post_job_available, "readonly", 0, 0, 0, ""],
    ],
}
