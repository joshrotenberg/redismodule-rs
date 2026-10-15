//! Isolated corrected stream fixture. Wrappers forward real APIs on legal handles.
use redis_module::{
    key::RedisKey, raw, redis_module, Context, NextArg, RedisError, RedisResult, RedisString,
    RedisValue, Status,
};
use std::{
    collections::BTreeSet,
    os::raw::c_int,
    sync::{Mutex, OnceLock},
};

type StartFn = unsafe extern "C" fn(
    *mut raw::RedisModuleKey,
    c_int,
    *mut raw::RedisModuleStreamID,
    *mut raw::RedisModuleStreamID,
) -> c_int;
type KeyFn = unsafe extern "C" fn(*mut raw::RedisModuleKey) -> c_int;
type CloseFn = unsafe extern "C" fn(*mut raw::RedisModuleKey);
static START: OnceLock<StartFn> = OnceLock::new();
static STOP: OnceLock<KeyFn> = OnceLock::new();
static DELETE: OnceLock<KeyFn> = OnceLock::new();
static CLOSE: OnceLock<CloseFn> = OnceLock::new();
#[derive(Default)]
struct Trace {
    events: Vec<&'static str>,
    active: BTreeSet<usize>,
    enabled: bool,
}
// This isolated diagnostic fixture treats unexpected mutex poisoning as fatal;
// no panic containment or arbitrary-callback safety is claimed.
static TRACE: Mutex<Trace> = Mutex::new(Trace {
    events: Vec::new(),
    active: BTreeSet::new(),
    enabled: false,
});
unsafe extern "C" fn start(
    k: *mut raw::RedisModuleKey,
    f: c_int,
    a: *mut raw::RedisModuleStreamID,
    b: *mut raw::RedisModuleStreamID,
) -> c_int {
    let result = unsafe { START.get().unwrap()(k, f, a, b) };
    let mut trace = TRACE.lock().unwrap();
    if trace.enabled {
        trace
            .events
            .push(if result == raw::REDISMODULE_OK as c_int {
                "start"
            } else {
                "rejected"
            });
        if result == raw::REDISMODULE_OK as c_int {
            trace.active.insert(k as usize);
        }
    }
    result
}
unsafe extern "C" fn stop(k: *mut raw::RedisModuleKey) -> c_int {
    let result = unsafe { STOP.get().unwrap()(k) };
    let mut trace = TRACE.lock().unwrap();
    if trace.enabled {
        let owned = trace.active.remove(&(k as usize));
        trace
            .events
            .push(if owned && result == raw::REDISMODULE_OK as c_int {
                "stop"
            } else {
                "bad-stop"
            });
    }
    result
}
unsafe extern "C" fn delete(k: *mut raw::RedisModuleKey) -> c_int {
    {
        let mut trace = TRACE.lock().unwrap();
        if trace.enabled {
            trace.events.push("delete");
        }
    }
    unsafe { DELETE.get().unwrap()(k) }
}
unsafe extern "C" fn close(k: *mut raw::RedisModuleKey) {
    {
        let mut trace = TRACE.lock().unwrap();
        if trace.enabled {
            // Keep a sticky sentinel after an active-close: do not hide a prior
            // teardown failure by allowing another traversal in this fixture.
            let active = trace.active.contains(&(k as usize));
            trace
                .events
                .push(if active { "active-close" } else { "close" });
        }
    }
    unsafe { CLOSE.get().unwrap()(k) };
}
fn init(_: &Context, _: &[RedisString]) -> Status {
    unsafe {
        START.get_or_init(|| raw::RedisModule_StreamIteratorStart.unwrap());
        STOP.get_or_init(|| raw::RedisModule_StreamIteratorStop.unwrap());
        DELETE.get_or_init(|| raw::RedisModule_StreamIteratorDelete.unwrap());
        CLOSE.get_or_init(|| raw::RedisModule_CloseKey.unwrap());
        raw::RedisModule_StreamIteratorStart = Some(start);
        raw::RedisModule_StreamIteratorStop = Some(stop);
        raw::RedisModule_StreamIteratorDelete = Some(delete);
        raw::RedisModule_CloseKey = Some(close);
    }
    Status::Ok
}
fn record(record: redis_module::stream::StreamRecord) -> RedisValue {
    RedisValue::Array(vec![
        RedisValue::BulkString(format!("{}-{}", record.id.ms, record.id.seq)),
        RedisValue::Array(
            record
                .fields
                .into_iter()
                .flat_map(|(f, v)| {
                    [
                        RedisValue::StringBuffer(f.as_slice().to_vec()),
                        RedisValue::StringBuffer(v.as_slice().to_vec()),
                    ]
                })
                .collect(),
        ),
    ])
}
fn early(key: &RedisKey) -> Result<(), RedisError> {
    let mut iter = key.get_stream_iterator(false)?;
    let _ = iter.next();
    Err(RedisError::Str("selected early return"))
}
fn exercise(
    key: &RedisKey,
    mode: &str,
    reverse: bool,
    exclusive: bool,
    bounded: bool,
) -> RedisResult {
    let from = bounded.then_some(raw::RedisModuleStreamID { ms: 1, seq: 0 });
    let to = bounded.then_some(raw::RedisModuleStreamID { ms: 3, seq: 0 });
    let selected = if mode == "early" {
        match early(key) {
            Err(RedisError::Str("selected early return")) => Vec::new(),
            other => {
                other?;
                return Err(RedisError::Str("early control missed"));
            }
        }
    } else {
        let mut iter = key.get_stream_range_iterator(from, to, exclusive, reverse)?;
        if mode == "second" && key.get_stream_iterator(false).is_ok() {
            return Err(RedisError::Str("second Start unexpectedly accepted"));
        }
        match mode {
            "zero" => Vec::new(),
            "one" | "second" => iter.next().map(record).into_iter().collect(),
            _ => iter.map(record).collect(),
        }
    };
    // This is the SAME still-open key, after the first iterator's Drop.
    let restarted = key.get_stream_iterator(false)?.map(record).collect();
    Ok(RedisValue::Array(vec![
        RedisValue::Array(selected),
        RedisValue::Array(restarted),
    ]))
}
fn check(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let name = args.next_arg()?;
    let mode = args.next_string()?;
    let reverse = args.next_i64()? != 0;
    let exclusive = args.next_i64()? != 0;
    let bounded = args.next_i64()? != 0;
    args.done()?;
    {
        let mut trace = TRACE.lock().unwrap();
        if !trace.active.is_empty() {
            return Err(RedisError::Str("previous traversal still active"));
        }
        trace.events.clear();
        trace.enabled = true;
    }
    let result = {
        let key = if mode == "writable" {
            // Actual context; one freshly opened handle and exactly one closing
            // owner. No AutoMemory registration is enabled by this fixture.
            // No mutation occurs until every iterator and this owner are gone.
            // Native CloseKey signals modification for WRITE mode even when
            // contents are unchanged; the fixture command is marked write.
            let native = raw::open_key(
                ctx.ctx,
                name.as_ptr(),
                raw::KeyMode::READ | raw::KeyMode::WRITE,
            );
            unsafe { RedisKey::from_raw_parts(ctx.ctx, native) }
        } else {
            ctx.open_key(&name)
        };
        exercise(&key, &mode, reverse, exclusive, bounded)
    };
    let events = {
        let mut trace = TRACE.lock().unwrap();
        trace.enabled = false;
        trace
            .events
            .iter()
            .map(|s| RedisValue::BulkString((*s).into()))
            .collect()
    };
    Ok(RedisValue::Array(vec![
        result.unwrap_or_else(|e| RedisValue::BulkString(e.to_string())),
        RedisValue::Array(events),
    ]))
}
redis_module! {
    name: "stream_cleanup", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], init: init,
    commands: [["streamcheck.read", check, "write", 1, 1, 1, ""]],
}
