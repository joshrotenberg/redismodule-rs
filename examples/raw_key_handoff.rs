//! Corrected, isolated same-command raw-key transfer and borrowed-scan fixture.
use redis_module::{
    key::RedisKey, raw, redis_module, Context, KeysCursor, NextArg, RedisError, RedisResult,
    RedisString, RedisValue, ScanKeyCursor, Status,
};
use std::{
    os::raw::c_int,
    sync::{Mutex, OnceLock},
};
type OpenFn = unsafe extern "C" fn(
    *mut raw::RedisModuleCtx,
    *mut raw::RedisModuleString,
    c_int,
) -> *mut raw::RedisModuleKey;
type CloseFn = unsafe extern "C" fn(*mut raw::RedisModuleKey);
static OPEN: OnceLock<OpenFn> = OnceLock::new();
static CLOSE: OnceLock<CloseFn> = OnceLock::new();
#[derive(Clone, Default)]
struct Trace {
    enabled: bool,
    opens: usize,
    last: usize,
    closes: usize,
    null_closes: usize,
}
static TRACE: Mutex<Trace> = Mutex::new(Trace {
    enabled: false,
    opens: 0,
    last: 0,
    closes: 0,
    null_closes: 0,
});
// Diagnostic fixture only: unexpected mutex poisoning is fatal. No general panic
// or concurrency contract is claimed, and no lock is held while calling Redis.
unsafe extern "C" fn open(
    ctx: *mut raw::RedisModuleCtx,
    name: *mut raw::RedisModuleString,
    mode: c_int,
) -> *mut raw::RedisModuleKey {
    let key = unsafe { OPEN.get().unwrap()(ctx, name, mode) };
    let mut t = TRACE.lock().unwrap();
    if t.enabled {
        t.opens += 1;
        t.last = key as usize;
    }
    key
}
unsafe extern "C" fn close(key: *mut raw::RedisModuleKey) {
    {
        let mut t = TRACE.lock().unwrap();
        if t.enabled {
            if key.is_null() {
                t.null_closes += 1
            } else {
                t.closes += 1
            }
        }
    }
    unsafe { CLOSE.get().unwrap()(key) };
}
fn init(_: &Context, _: &[RedisString]) -> Status {
    // redis_module! has already run RedisModule_Init. Capture populated original
    // pointers before these fixture-only forwarding swaps.
    unsafe {
        OPEN.get_or_init(|| raw::RedisModule_OpenKey.unwrap());
        CLOSE.get_or_init(|| raw::RedisModule_CloseKey.unwrap());
        raw::RedisModule_OpenKey = Some(open);
        raw::RedisModule_CloseKey = Some(close);
    }
    Status::Ok
}
fn snapshot() -> Trace {
    TRACE.lock().unwrap().clone()
}
fn require(ok: bool, message: &'static str) -> Result<(), RedisError> {
    if ok {
        Ok(())
    } else {
        Err(RedisError::Str(message))
    }
}
fn acquire(
    ctx: &Context,
    name: &RedisString,
) -> Result<(*mut raw::RedisModuleCtx, *mut raw::RedisModuleKey), RedisError> {
    let before = snapshot();
    let key = ctx.open_key(name);
    let actual = snapshot();
    let parts = key.to_raw_parts();
    let after = snapshot();
    // Check close BEFORE any native pointer use. If a regression closes during
    // extraction, report it without dereferencing or closing that pointer again.
    require(
        after.closes == before.closes && after.null_closes == before.null_closes,
        "extraction issued CloseKey",
    )?;
    require(
        actual.opens == before.opens + 1 && parts.0 == ctx.ctx && parts.1 as usize == actual.last,
        "raw tuple differs from actual OpenKey",
    )?;
    Ok(parts)
}
fn early(ctx: *mut raw::RedisModuleCtx, key: *mut raw::RedisModuleKey) -> Result<(), RedisError> {
    // Sole manual close obligation from acquire, same legal command phase.
    let _owner = unsafe { RedisKey::from_raw_parts(ctx, key) };
    Err(RedisError::Str("selected early return"))
}
fn transfer(ctx: &Context, name: &RedisString, mode: &str) -> RedisResult {
    if mode == "ordinary" {
        let key = ctx.open_key(name);
        return Ok(key
            .read()?
            .map_or(RedisValue::Null, |v| RedisValue::StringBuffer(v.to_vec())));
    }
    let cycles = if mode == "cycles" { 16 } else { 1 };
    let mut output = RedisValue::Null;
    for _ in 0..cycles {
        let (opening, key) = acquire(ctx, name)?;
        // acquire already proved no close occurred. NULL is a genuine OpenKey
        // result; native KeyType(NULL) is explicitly permitted by these hosts.
        let kind = unsafe { raw::RedisModule_KeyType.unwrap()(key) };
        if mode == "manual" || mode == "null-manual" {
            unsafe { raw::RedisModule_CloseKey.unwrap()(key) };
            output = RedisValue::Integer(i64::from(kind));
        } else if mode == "early" {
            require(
                matches!(
                    early(opening, key),
                    Err(RedisError::Str("selected early return"))
                ),
                "early-return control failed",
            )?;
            output = RedisValue::BulkString("early".into());
        } else {
            let owner = unsafe { RedisKey::from_raw_parts(opening, key) };
            require(
                owner.is_null() == key.is_null(),
                "NULL classification changed",
            )?;
            output = owner
                .read()?
                .map_or(RedisValue::Null, |v| RedisValue::StringBuffer(v.to_vec()));
        }
    }
    Ok(output)
}
fn scans(ctx: &Context, name: &RedisString, mode: &str) -> RedisResult {
    let mut rows = Vec::new();
    let mut premature = false;
    if mode == "scan-db" {
        let cursor = KeysCursor::new();
        while cursor.scan(ctx, &mut |_ctx, name, key| {
            premature |= snapshot().closes != 0;
            // Callback-only view, never adopted, extracted, closed or escaped.
            if let Some(key) = key {
                let _ = key.key_type();
            }
            rows.push(RedisValue::StringBuffer(name.as_slice().to_vec()));
        }) {}
        require(
            snapshot().closes == 0 && !premature,
            "borrowed stack adapter closed a key",
        )?;
    } else {
        let cursor = ScanKeyCursor::new(ctx.open_key(name));
        while cursor.scan(|key, field, value| {
            premature |= snapshot().closes != 0;
            let _ = key.key_type();
            rows.push(RedisValue::Array(vec![
                RedisValue::StringBuffer(field.as_slice().to_vec()),
                value.map_or(RedisValue::Null, |v| {
                    RedisValue::StringBuffer(v.as_slice().to_vec())
                }),
            ]));
        }) {}
        require(
            snapshot().closes == 0 && !premature,
            "borrowed per-key adapter closed the outer key",
        )?;
        drop(cursor); // the real outer owner closes once, after the borrowed views.
    }
    Ok(RedisValue::Array(rows))
}
fn check(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let name = args.next_arg()?;
    let mode = args.next_string()?;
    let am = args.next_i64()?;
    args.done()?;
    require(
        matches!(
            mode.as_str(),
            "round"
                | "manual"
                | "null-manual"
                | "early"
                | "cycles"
                | "ordinary"
                | "scan-db"
                | "scan-key"
        ),
        "unknown fixture mode",
    )?;
    if am != 0 {
        ctx.auto_memory();
    }
    *TRACE.lock().unwrap() = Trace {
        enabled: true,
        ..Trace::default()
    };
    // Keep name and context valid until every owner/view has gone. No handle or
    // byte view survives command return. AM does not change these deadlines.
    let result = if mode.starts_with("scan-") {
        scans(ctx, &name, &mode)
    } else {
        transfer(ctx, &name, &mode)
    };
    let trace = {
        let mut t = TRACE.lock().unwrap();
        t.enabled = false;
        t.clone()
    };
    Ok(RedisValue::Array(vec![
        result?,
        RedisValue::Integer(trace.opens as i64),
        RedisValue::Integer(trace.closes as i64),
        RedisValue::Integer(trace.null_closes as i64),
    ]))
}
redis_module! {
    name:"raw_key_handoff",version:1,
    allocator:(redis_module::alloc::RedisAlloc,redis_module::alloc::RedisAlloc),
    data_types:[],init:init,
    commands:[["rawkey.check",check,"readonly",1,1,1,""]],
}
