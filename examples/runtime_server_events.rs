//! Static server handlers selected at load time or from a command. Load with
//! `on` for conditional cron subscription, or call `runtime_server.subscribe`.
//! Redis removes subscriptions at unload; the SDK clears its owned registry.
use redis_module::{
    Context, EventContext, RedisError, RedisResult, RedisString, RegistrationContext, Status, raw,
    redis_module,
    server_events::{FlushInfo, FlushSubevent, ServerEventHandler, SwapDbInfo},
};
use redis_module_macros::{cron_event_handler, flush_event_handler};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

static COUNTS: [AtomicI64; 6] = [const { AtomicI64::new(0) }; 6];
static ORDER: Mutex<Vec<i64>> = Mutex::new(Vec::new());
static PANIC: AtomicBool = AtomicBool::new(false);
static REJECT_UNLOAD: AtomicBool = AtomicBool::new(false);

// Retain the image across unload/reload so tests exercise registry cleanup even
// when the dynamic loader keeps Rust statics alive.
struct Retain;
impl Drop for Retain {
    fn drop(&mut self) {}
}
thread_local! { static RETAIN: Retain = const { Retain }; }

#[cron_event_handler]
fn attribute_cron(_: &EventContext, _: u64) {
    COUNTS[0].fetch_add(1, Ordering::Relaxed);
}
fn cron(_: &EventContext, _: u64) {
    COUNTS[1].fetch_add(1, Ordering::Relaxed);
}
#[flush_event_handler]
fn attribute_flush(_: &EventContext, phase: FlushSubevent) {
    if phase == FlushSubevent::Started {
        COUNTS[2].fetch_add(1, Ordering::Relaxed);
        ORDER.lock().unwrap().push(0);
    }
}
fn late_flush(_: &EventContext, phase: FlushSubevent) {
    if phase == FlushSubevent::Started {
        COUNTS[5].fetch_add(1, Ordering::Relaxed);
        ORDER.lock().unwrap().push(3);
    }
}
fn flush(ctx: &EventContext, phase: FlushSubevent) {
    if phase == FlushSubevent::Started {
        COUNTS[3].fetch_add(1, Ordering::Relaxed);
        ORDER.lock().unwrap().push(2);
        // Registration from a handler must neither deadlock nor mutate the
        // current delivery's snapshot. The same function is added only once.
        ctx.subscribe_to_server_event(ServerEventHandler::Flush(late_flush))
            .unwrap();
        assert!(!PANIC.load(Ordering::Relaxed), "runtime flush panic");
    }
}
fn flush_info(_: &EventContext, phase: FlushSubevent, _: FlushInfo) {
    if phase == FlushSubevent::Started {
        COUNTS[4].fetch_add(1, Ordering::Relaxed);
        ORDER.lock().unwrap().push(1);
    }
}

fn install(ctx: &EventContext) -> Result<(), RedisError> {
    // The reverse payload/legacy order verifies shared-event registration order.
    for handler in [
        ServerEventHandler::Cron(cron),
        ServerEventHandler::Cron(cron),
        ServerEventHandler::Cron(attribute_cron),
        ServerEventHandler::FlushInfo(flush_info),
        ServerEventHandler::Flush(flush),
        ServerEventHandler::Flush(flush),
        ServerEventHandler::Flush(attribute_flush),
    ] {
        ctx.subscribe_to_server_event(handler)?;
    }
    Ok(())
}
fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    RETAIN.with(|_| ());
    for count in &COUNTS {
        count.store(0, Ordering::Relaxed);
    }
    ORDER.lock().unwrap().clear();
    SWAPS.store(0, Ordering::Relaxed);
    PANIC.store(false, Ordering::Relaxed);
    REJECT_UNLOAD.store(false, Ordering::Relaxed);
    let mode = args
        .first()
        .and_then(|v| v.try_as_str().ok())
        .unwrap_or("off");
    if (mode == "on" || mode == "fail" || mode == "panic") && install(ctx).is_err() {
        return Status::Err;
    }
    if mode == "panic" {
        panic!("runtime server init panic");
    }
    if mode == "fail" {
        Status::Err
    } else {
        Status::Ok
    }
}
fn deinit(_: &EventContext) -> Status {
    if REJECT_UNLOAD.load(Ordering::Relaxed) {
        Status::Err
    } else {
        Status::Ok
    }
}
fn subscribe(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    install(ctx)?;
    Ok("OK".into())
}
static SWAPS: AtomicI64 = AtomicI64::new(0);
fn swapped(ctx: &EventContext, info: SwapDbInfo) {
    SWAPS.fetch_add(1, Ordering::Relaxed);
    ctx.log_notice(&format!("runtime swap: {} {}", info.first, info.second));
}
fn subscribe_swap(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.subscribe_to_server_event(ServerEventHandler::SwapDb(swapped))?;
    Ok("OK".into())
}
fn swaps(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(SWAPS.load(Ordering::Relaxed).into())
}
// Fault injection belongs only to this example, never the SDK's public API.
type SubscribeApi = unsafe extern "C" fn(
    *mut raw::RedisModuleCtx,
    raw::RedisModuleEvent,
    raw::RedisModuleEventCallback,
) -> std::os::raw::c_int;
struct RestoreApi(Option<SubscribeApi>);
impl Drop for RestoreApi {
    fn drop(&mut self) {
        // SAFETY: this command holds Redis's lock, including during unwinding.
        unsafe {
            raw::RedisModule_SubscribeToServerEvent = self.0;
        }
    }
}
unsafe extern "C" fn reject_subscription(
    _: *mut raw::RedisModuleCtx,
    _: raw::RedisModuleEvent,
    _: raw::RedisModuleEventCallback,
) -> std::os::raw::c_int {
    raw::REDISMODULE_ERR as _
}
fn refuse(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let [_, mode] = args.as_slice() else {
        return Err(RedisError::WrongArity);
    };
    let mode = mode.try_as_str()?;
    if mode != "missing" && mode != "rejected" {
        return Err(RedisError::Str("ERR unknown refusal mode"));
    }
    // SAFETY: Redis serializes module commands; restore the image-local API
    // pointer before returning, including if the Rust call panics.
    let _restore = RestoreApi(unsafe { raw::RedisModule_SubscribeToServerEvent });
    unsafe {
        raw::RedisModule_SubscribeToServerEvent = if mode == "missing" {
            None
        } else {
            Some(reject_subscription)
        };
    }
    ctx.subscribe_to_server_event(ServerEventHandler::SwapDb(swapped))?;
    Ok("OK".into())
}
fn counts(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(COUNTS
        .iter()
        .map(|n| n.load(Ordering::Relaxed))
        .collect::<Vec<_>>()
        .into())
}
fn order(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(std::mem::take(&mut *ORDER.lock().unwrap()).into())
}
fn flags(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let [_, panic, reject] = args.as_slice() else {
        return Err(RedisError::WrongArity);
    };
    PANIC.store(panic.try_as_str()? == "yes", Ordering::Relaxed);
    REJECT_UNLOAD.store(reject.try_as_str()? == "yes", Ordering::Relaxed);
    Ok("OK".into())
}
redis_module! {
    name: "runtime_server_events",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    deinit: deinit,
    commands: [
        ["runtime_server.subscribe", subscribe, "", 0, 0, 0, ""],
        ["runtime_server.subscribe_swap", subscribe_swap, "", 0, 0, 0, ""],
        ["runtime_server.swaps", swaps, "readonly", 0, 0, 0, ""],
        ["runtime_server.refuse", refuse, "", 0, 0, 0, ""],
        ["runtime_server.counts", counts, "readonly", 0, 0, 0, ""],
        ["runtime_server.order", order, "", 0, 0, 0, ""],
        ["runtime_server.flags", flags, "", 0, 0, 0, ""],
    ],
}
