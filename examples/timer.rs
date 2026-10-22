//! Typed timer handles are stored behind display IDs for this command example.
//! The IDs are local to this module; they are never cast into native timer IDs.
use redis_module::{
    Context, EventContext, NextArg, NotifyEvent, RedisError, RedisGILGuard, RedisResult,
    RedisString, RedisValue, RegistrationContext, Status, ThreadSafeContext, TimerHandle,
    redis_event_handler, redis_module,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static TIMERS: RedisGILGuard<BTreeMap<u64, TimerHandle<String>>> =
    RedisGILGuard::new(BTreeMap::new());
static PROBES: RedisGILGuard<BTreeMap<u64, TimerHandle<TrackedData>>> =
    RedisGILGuard::new(BTreeMap::new());
static FIRED: AtomicI64 = AtomicI64::new(0);
static DATA_DROPS: AtomicI64 = AtomicI64::new(0);
static CAPTURE_DROPS: AtomicI64 = AtomicI64::new(0);
static REENTRY_OK: AtomicI64 = AtomicI64::new(0);
static CROSSED_THREAD: AtomicBool = AtomicBool::new(false);
static DEINIT_MODE: AtomicI64 = AtomicI64::new(0);
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

fn callback(ctx: &EventContext, data: String) {
    ctx.call("SET", &["timer:fired", &data]).unwrap();
}

fn string_handle(ctx: &EventContext, id: u64) -> Result<TimerHandle<String>, RedisError> {
    TIMERS
        .lock(ctx)
        .get(&id)
        .cloned()
        .ok_or(RedisError::Str("ERR unknown display timer ID"))
}

fn timer_create(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let duration = args.next_u64()?;
    let data = args.next_string()?;
    let timer = ctx.create_timer(Duration::from_millis(duration), callback, data)?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    TIMERS.lock(ctx).insert(id, timer);
    Ok(id.to_string().into())
}

fn remaining_reply(remaining: Option<Duration>) -> RedisResult {
    match remaining {
        Some(duration) => i64::try_from(duration.as_millis())
            .map(Into::into)
            .map_err(|_| RedisError::Str("ERR timer duration does not fit reply")),
        None => Ok(RedisValue::Null),
    }
}

fn timer_info(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let id = args.into_iter().skip(1).next_u64()?;
    remaining_reply(ctx.timer_remaining(&string_handle(ctx, id)?)?)
}

fn timer_stop(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let id = args.into_iter().skip(1).next_u64()?;
    Ok(ctx.cancel_timer(&string_handle(ctx, id)?)?.into())
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

// Cell makes the payload Send but not Sync. A timer consumes it once.
struct TrackedData {
    value: Cell<i64>,
    drop_path: Option<String>,
}

impl Drop for TrackedData {
    fn drop(&mut self) {
        DATA_DROPS.fetch_add(1, Ordering::SeqCst);
        record_drop(&self.drop_path, b'D');
    }
}

// The nonzero, over-aligned captured value exposes incorrect cancellation
// layouts and proves that the retained callback is dropped as its real type.
#[repr(align(64))]
struct Capture {
    touched: Cell<i64>,
    drop_path: Option<String>,
    panic_on_drop: bool,
}

impl Capture {
    fn new(drop_path: Option<String>) -> Self {
        Self {
            touched: Cell::new(0),
            drop_path,
            panic_on_drop: false,
        }
    }
    fn touch(&self) {
        self.touched.set(self.touched.get() + 1);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        CAPTURE_DROPS.fetch_add(1, Ordering::SeqCst);
        record_drop(&self.drop_path, b'C');
        if self.panic_on_drop {
            panic!("timer capture drop panic");
        }
    }
}

fn probe_handle(ctx: &EventContext, id: u64) -> Result<TimerHandle<TrackedData>, RedisError> {
    PROBES
        .lock(ctx)
        .get(&id)
        .cloned()
        .ok_or(RedisError::Str("ERR unknown probe ID"))
}

fn probe(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let mode = args.next_string()?;
    let delay = args.next_u64()?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut capture = Capture::new(None);
    capture.panic_on_drop = mode == "cancel-panic";
    let data = TrackedData {
        value: Cell::new(7),
        drop_path: None,
    };
    let keep_handle = mode != "passive";
    let timer = ctx.create_timer(
        Duration::from_millis(delay),
        move |ctx, data: TrackedData| {
            capture.touch();
            data.value.set(data.value.get() + 1);
            FIRED.fetch_add(1, Ordering::SeqCst);
            if mode == "reenter" {
                let own = probe_handle(ctx, id).unwrap();
                let cancelled = ctx.cancel_timer(&own).unwrap().is_none();
                let remaining = ctx.timer_remaining(&own).unwrap().is_none();
                let unload_refused = ctx.call("MODULE", &["UNLOAD", "timer"]).is_err();
                if cancelled && remaining && unload_refused {
                    REENTRY_OK.fetch_add(1, Ordering::SeqCst);
                }
            }
            if mode == "panic" {
                panic!("timer probe callback panic");
            }
            drop(capture);
        },
        data,
    )?;
    if keep_handle {
        PROBES.lock(ctx).insert(id, timer);
    }
    Ok(id.to_string().into())
}

fn probe_stop(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let id = args.into_iter().skip(1).next_u64()?;
    Ok(ctx
        .cancel_timer(&probe_handle(ctx, id)?)?
        .map(|data| data.value.get())
        .into())
}

fn probe_info(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let id = args.into_iter().skip(1).next_u64()?;
    remaining_reply(ctx.timer_remaining(&probe_handle(ctx, id)?)?)
}

fn probe_counts(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        FIRED.load(Ordering::SeqCst),
        DATA_DROPS.load(Ordering::SeqCst),
        CAPTURE_DROPS.load(Ordering::SeqCst),
        REENTRY_OK.load(Ordering::SeqCst),
        i64::from(CROSSED_THREAD.load(Ordering::SeqCst)),
    ]
    .into())
}

fn invalid_delay(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let capture = Capture::new(None);
    let data = TrackedData {
        value: Cell::new(7),
        drop_path: None,
    };
    let _timer = ctx.create_timer(Duration::MAX, move |_, _: TrackedData| drop(capture), data)?;
    Err(RedisError::Str("ERR excessive delay was accepted"))
}

fn on_worker_event(ctx: &EventContext, _: NotifyEvent, _: &str, key: &[u8]) {
    if key != b"timer:worker-origin" {
        return;
    }
    let creator = thread::current().id();
    let capture = Capture::new(None);
    let data = TrackedData {
        value: Cell::new(7),
        drop_path: None,
    };
    ctx.create_timer(
        Duration::from_millis(1),
        move |_, _: TrackedData| {
            capture.touch();
            CROSSED_THREAD.store(thread::current().id() != creator, Ordering::SeqCst);
            FIRED.fetch_add(1, Ordering::SeqCst);
            drop(capture);
        },
        data,
    )
    .unwrap();
}

fn worker(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR join the previous worker"));
    }
    let blocked = ctx.block_client()?;
    *worker = Some(thread::spawn(move || {
        let owner = ThreadSafeContext::with_blocked_client(blocked);
        let result = {
            let guard = owner.lock();
            guard.call("SET", &["timer:worker-origin", "1"])
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
            .map_err(|_| RedisError::Str("ERR timer worker panicked"))?;
    }
    Ok(true)
}

fn finish(_: &Context, _: Vec<RedisString>) -> RedisResult {
    join_worker().map(|done| i64::from(done).into())
}

fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    // A platform may retain the library mapping across unload/reload. Reset the
    // fixture observations explicitly rather than relying on static reinitialization.
    NEXT_ID.store(1, Ordering::SeqCst);
    FIRED.store(0, Ordering::SeqCst);
    DATA_DROPS.store(0, Ordering::SeqCst);
    CAPTURE_DROPS.store(0, Ordering::SeqCst);
    REENTRY_OK.store(0, Ordering::SeqCst);
    CROSSED_THREAD.store(false, Ordering::SeqCst);
    let mode = args
        .first()
        .and_then(|arg| arg.try_as_str().ok())
        .unwrap_or("");
    DEINIT_MODE.store(
        match mode {
            "deinit-reject" => 1,
            "deinit-panic" => 2,
            "deinit-create" => 3,
            _ => 0,
        },
        Ordering::SeqCst,
    );
    if mode == "worker" {
        if let Err(error) = ctx.subscribe_to_keyspace_events(
            NotifyEvent::STRING,
            redis_event_handler!(on_worker_event),
        ) {
            ctx.log_warning(&error.to_string());
            return Status::Err;
        }
    }
    if matches!(mode, "init-denied" | "init-error" | "init-panic") {
        let Some(path) = args
            .get(1)
            .and_then(|arg| arg.try_as_str().ok())
            .map(str::to_owned)
        else {
            return Status::Err;
        };
        let rejected = Some(path.clone());
        let capture = Capture::new(Some(path.clone()));
        let data = TrackedData {
            value: Cell::new(7),
            drop_path: Some(path),
        };
        match ctx.create_timer(
            Duration::from_secs(60),
            move |_, _: TrackedData| {
                capture.touch();
                drop(capture);
            },
            data,
        ) {
            Err(_) => record_drop(&rejected, b'E'),
            Ok(timer) => {
                let _ = ctx.cancel_timer(&timer);
                return Status::Err;
            }
        }
        if mode == "init-panic" {
            panic!("timer init panic after rejected creation");
        }
        if mode == "init-error" {
            return Status::Err;
        }
    }
    Status::Ok
}

fn deinit(ctx: &EventContext) -> Status {
    match DEINIT_MODE.swap(0, Ordering::SeqCst) {
        1 => return Status::Err,
        2 => panic!("timer deinit panic"),
        3 => {
            match ctx.create_timer(Duration::from_secs(60), |_, ()| {}, ()) {
                Err(_) => {
                    let _ = ctx.call("SET", &["timer:deinit-create-denied", "1"]);
                }
                Ok(timer) => {
                    // Keep a broken implementation loaded rather than unloading
                    // a newly registered callback into unmapped code.
                    let _ = ctx.cancel_timer(&timer);
                    return Status::Err;
                }
            }
        }
        _ => {}
    }
    if !join_worker().unwrap_or(false) {
        return Status::Err;
    }
    TIMERS.lock(ctx).clear();
    PROBES.lock(ctx).clear();
    Status::Ok
}

redis_module! {
    name: "timer", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], init: init, deinit: deinit,
    commands: [
        ["timer.create", timer_create, "", 0, 0, 0, ""],
        ["timer.info", timer_info, "", 0, 0, 0, ""],
        ["timer.stop", timer_stop, "", 0, 0, 0, ""],
        ["timer.probe", probe, "", 0, 0, 0, ""],
        ["timer.probe_stop", probe_stop, "", 0, 0, 0, ""],
        ["timer.probe_info", probe_info, "readonly", 0, 0, 0, ""],
        ["timer.probe_counts", probe_counts, "readonly", 0, 0, 0, ""],
        ["timer.invalid_delay", invalid_delay, "", 0, 0, 0, ""],
        ["timer.worker", worker, "", 0, 0, 0, ""],
        ["timer.finish", finish, "", 0, 0, 0, ""],
    ],
}
