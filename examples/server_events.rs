use std::sync::atomic::{AtomicI64, Ordering};

use redis_module::{
    Context, RedisResult, RedisString, RedisValue, redis_module,
    server_events::{FlushInfo, FlushSubevent},
};
use redis_module_macros::{
    config_changed_event_handler, cron_event_handler, flush_event_handler,
    flush_info_event_handler, shutdown_event_handler,
};

static NUM_FLUSHES: AtomicI64 = AtomicI64::new(0);
static NUM_FLUSH_DETAILS: AtomicI64 = AtomicI64::new(0);
static NUM_CRONS: AtomicI64 = AtomicI64::new(0);
static LAST_CRON_HZ: AtomicI64 = AtomicI64::new(0);
static NUM_MAX_MEMORY_CONFIGURATION_CHANGES: AtomicI64 = AtomicI64::new(0);

#[flush_event_handler]
fn flushed_event_handler(_ctx: &redis_module::EventContext, flush_event: FlushSubevent) {
    if let FlushSubevent::Started = flush_event {
        NUM_FLUSHES.fetch_add(1, Ordering::SeqCst);
    }
}

#[flush_info_event_handler]
fn flush_details(_: &redis_module::EventContext, phase: FlushSubevent, _: FlushInfo) {
    if phase == FlushSubevent::Started {
        NUM_FLUSH_DETAILS.fetch_add(1, Ordering::SeqCst);
    }
}

fn num_flush_details(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(NUM_FLUSH_DETAILS.load(Ordering::SeqCst).into())
}

#[config_changed_event_handler]
fn config_changed_event_handler(_ctx: &redis_module::EventContext, changed_configs: &[&str]) {
    changed_configs
        .iter()
        .find(|v| **v == "maxmemory")
        .map(|_| NUM_MAX_MEMORY_CONFIGURATION_CHANGES.fetch_add(1, Ordering::SeqCst));
}

#[cron_event_handler]
fn cron_event_handler(_ctx: &redis_module::EventContext, hz: u64) {
    NUM_CRONS.fetch_add(1, Ordering::SeqCst);
    LAST_CRON_HZ.store(hz as i64, Ordering::SeqCst);
}

#[shutdown_event_handler]
fn shutdown_event_handler(ctx: &redis_module::EventContext) {
    ctx.log_notice("server_events: shutdown event received");
}

fn num_flushed(_ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Integer(NUM_FLUSHES.load(Ordering::SeqCst)))
}

fn num_crons(_ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Integer(NUM_CRONS.load(Ordering::SeqCst)))
}

fn last_cron_hz(_ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Integer(LAST_CRON_HZ.load(Ordering::SeqCst)))
}

fn num_maxmemory_changes(_ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Integer(
        NUM_MAX_MEMORY_CONFIGURATION_CHANGES.load(Ordering::SeqCst),
    ))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "server_events",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["num_flushed", num_flushed, "readonly", 0, 0, 0, ""],
        ["num_flush_details", num_flush_details, "readonly", 0, 0, 0, ""],
        ["num_max_memory_changes", num_maxmemory_changes, "readonly", 0, 0, 0, ""],
        ["num_crons", num_crons, "readonly", 0, 0, 0, ""],
        ["last_cron_hz", last_cron_hz, "readonly", 0, 0, 0, ""],
    ],
}
