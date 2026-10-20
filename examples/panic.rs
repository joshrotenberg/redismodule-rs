//! Commands and callbacks that panic on request, so tests can check that a
//! panic in module code is reported instead of aborting the server.
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use redis_module::{
    ConfigurationValue, Context, KeysCursor, NotifyEvent, RedisError, RedisResult, RedisString,
    configuration::{ConfigurationContext, ConfigurationFlags},
    key::RedisKey,
    redis_module,
};
use redis_module_macros::command;

fn panic_command(_: &Context, _: Vec<RedisString>) -> RedisResult {
    panic!("panic.command was called");
}

#[command({ flags: [ReadOnly], arity: 1, key_spec: [] })]
fn panic_proc_macro(_: &Context, _: Vec<RedisString>) -> RedisResult {
    panic!("panic_proc_macro was called");
}

/// Scans the keyspace with a callback that panics on the first key. The panic
/// comes back out of `scan` and fails this command.
fn panic_scan(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let cursor = KeysCursor::new();
    let mut callback = |_: &Context, _: &RedisString, _: Option<&RedisKey>| {
        panic!("scan callback panicked");
    };
    while cursor.scan(ctx, &mut callback) {}
    Ok("scan finished".into())
}

fn panic_timer(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ctx.create_timer(
        Duration::from_millis(1),
        |_: &Context, _: ()| panic!("timer callback panicked"),
        (),
    );
    Ok("timer created".into())
}

/// Panics on keyspace events for the key `panic:notify`.
fn on_string_event(_: &Context, _: NotifyEvent, _: &str, key: &[u8]) {
    if key == b"panic:notify" {
        panic!("keyspace event handler panicked");
    }
}

/// A numeric configuration whose setter panics on 7 and whose getter panics
/// while the value is 13.
struct Number;
static NUMBER: Number = Number;
static NUMBER_VALUE: AtomicI64 = AtomicI64::new(0);

impl ConfigurationValue<i64> for Number {
    fn get(&self, _: &ConfigurationContext) -> i64 {
        let value = NUMBER_VALUE.load(Ordering::Relaxed);
        if value == 13 {
            panic!("configuration getter panicked on 13");
        }
        value
    }

    fn set(&self, _: &ConfigurationContext, value: i64) -> Result<(), RedisError> {
        if value == 7 {
            panic!("configuration setter panicked on 7");
        }
        NUMBER_VALUE.store(value, Ordering::Relaxed);
        Ok(())
    }
}

//////////////////////////////////////////////////////

redis_module! {
    name: "panic",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["panic.command", panic_command, "readonly", 0, 0, 0, ""],
        ["panic.scan", panic_scan, "readonly", 0, 0, 0, ""],
        ["panic.timer", panic_timer, "readonly", 0, 0, 0, ""],
    ],
    event_handlers: [
        [@STRING: on_string_event],
    ],
    configurations: [
        i64: [
            ["number", &NUMBER, 0, 0, 100, ConfigurationFlags::DEFAULT, None],
        ],
        string: [],
        bool: [],
        enum: [],
        module_args_as_configuration: false,
    ]
}
