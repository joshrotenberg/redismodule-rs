//! Observe database events without retaining native payload pointers or keys.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use redis_module::{
    Context, EventContext, RedisResult, RedisString, RedisValue, redis_command, redis_module,
    server_events::{FlushInfo, FlushSubevent, SwapDbInfo},
};
use redis_module_macros::{flush_info_event_handler, swapdb_event_handler};

static FLUSHES: Mutex<Vec<Vec<i64>>> = Mutex::new(Vec::new());
static SWAPS: Mutex<Vec<Vec<i64>>> = Mutex::new(Vec::new());
static PANIC_FLUSH: AtomicBool = AtomicBool::new(false);
static PANIC_SWAP: AtomicBool = AtomicBool::new(false);
static PANICKING_FLUSH_CALLS: AtomicI64 = AtomicI64::new(0);
static PANICKING_SWAP_CALLS: AtomicI64 = AtomicI64::new(0);

// Each payload is an owned copy and can be kept after the callback ends.
#[flush_info_event_handler]
fn record_flush(ctx: &EventContext, phase: FlushSubevent, info: FlushInfo) {
    FLUSHES.lock().unwrap().push(vec![
        i64::from(phase == FlushSubevent::Ended),
        i64::from(info.db.unwrap_or(-1)),
        i64::from(ctx.get_selected_db()),
        ctx.dbsize() as i64,
    ]);
    if PANIC_FLUSH.load(Ordering::SeqCst) {
        panic!("deliberate recording flush panic");
    }
}

#[flush_info_event_handler]
fn maybe_panic_flush(_: &EventContext, _: FlushSubevent, _: FlushInfo) {
    if PANIC_FLUSH.load(Ordering::SeqCst) {
        PANICKING_FLUSH_CALLS.fetch_add(1, Ordering::SeqCst);
        panic!("deliberate flush payload panic");
    }
}

#[swapdb_event_handler]
fn record_swap(ctx: &EventContext, info: SwapDbInfo) {
    // Read both sides after SWAPDB through a database scope that restores the
    // event context. Do not retain database-bound key handles across events.
    let first_size = ctx.with_db(info.first, |ctx| ctx.dbsize()).unwrap();
    let second_size = ctx.with_db(info.second, |ctx| ctx.dbsize()).unwrap();
    SWAPS.lock().unwrap().push(vec![
        i64::from(info.first),
        i64::from(info.second),
        first_size as i64,
        second_size as i64,
    ]);
    if PANIC_SWAP.load(Ordering::SeqCst) {
        panic!("deliberate recording swap panic");
    }
}

#[swapdb_event_handler]
fn maybe_panic_swap(_: &EventContext, _: SwapDbInfo) {
    if PANIC_SWAP.load(Ordering::SeqCst) {
        PANICKING_SWAP_CALLS.fetch_add(1, Ordering::SeqCst);
        panic!("deliberate swapdb payload panic");
    }
}

fn drain(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let rows = match args.get(1).map(RedisString::try_as_str).transpose()? {
        Some("flush") => std::mem::take(&mut *FLUSHES.lock().unwrap()),
        Some("swap") => std::mem::take(&mut *SWAPS.lock().unwrap()),
        _ => return Err(redis_module::RedisError::WrongArity),
    };
    Ok(RedisValue::Array(
        rows.into_iter().map(Into::into).collect(),
    ))
}

fn panics(_: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() != 2 {
        return Err(redis_module::RedisError::WrongArity);
    }
    let enabled = args[1].try_as_str()? == "yes";
    PANIC_FLUSH.store(enabled, Ordering::SeqCst);
    PANIC_SWAP.store(enabled, Ordering::SeqCst);
    Ok("OK".into())
}

fn panic_counts(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        PANICKING_FLUSH_CALLS.load(Ordering::SeqCst),
        PANICKING_SWAP_CALLS.load(Ordering::SeqCst),
    ]
    .into())
}

redis_module! {
    name: "server_event_payloads",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        redis_command!("event_payloads.drain", drain),
        redis_command!("event_payloads.panics", panics),
        redis_command!("event_payloads.panic_counts", panic_counts),
    ],
}
