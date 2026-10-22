//! Exercises RedisString ownership across producers (hash, list, stream, scan,
//! ServerInfo, RDB), command-thread retention, and AutoMemory on and off.
use redis_module::{
    Context, EventContext, KeysCursor, NextArg, RedisError, RedisResult, RedisString, RedisValue,
    ScanKeyCursor, Status, raw, redis_module,
};
use std::{
    cell::RefCell,
    mem::ManuallyDrop,
    os::raw::c_char,
    ptr::{self, NonNull},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

type StringFn = unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleString);
type CreateFn = unsafe extern "C" fn(
    *mut raw::RedisModuleCtx,
    *const c_char,
    usize,
) -> *mut raw::RedisModuleString;
type CopyFn = unsafe extern "C" fn(
    *mut raw::RedisModuleCtx,
    *const raw::RedisModuleString,
) -> *mut raw::RedisModuleString;
static RETAIN: OnceLock<StringFn> = OnceLock::new();
static FREE: OnceLock<StringFn> = OnceLock::new();
static CREATE: OnceLock<CreateFn> = OnceLock::new();
static COPY: OnceLock<CopyFn> = OnceLock::new();
static COUNTERS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
// Counts API calls, not native refcounts or process memory. These wrappers always
// forward valid calls to the real host APIs and are installed only at module load.
unsafe extern "C" fn retain(c: *mut raw::RedisModuleCtx, s: *mut raw::RedisModuleString) {
    COUNTERS[2].fetch_add(1, Ordering::Relaxed);
    unsafe { RETAIN.get().unwrap()(c, s) };
}
unsafe extern "C" fn free(c: *mut raw::RedisModuleCtx, s: *mut raw::RedisModuleString) {
    COUNTERS[if c.is_null() { 3 } else { 4 }].fetch_add(1, Ordering::Relaxed);
    unsafe { FREE.get().unwrap()(c, s) };
}
unsafe extern "C" fn create(
    c: *mut raw::RedisModuleCtx,
    b: *const c_char,
    n: usize,
) -> *mut raw::RedisModuleString {
    COUNTERS[0].fetch_add(1, Ordering::Relaxed);
    unsafe { CREATE.get().unwrap()(c, b, n) }
}
unsafe extern "C" fn copy(
    c: *mut raw::RedisModuleCtx,
    s: *const raw::RedisModuleString,
) -> *mut raw::RedisModuleString {
    COUNTERS[1].fetch_add(1, Ordering::Relaxed);
    unsafe { COPY.get().unwrap()(c, s) }
}
fn init(_: &redis_module::RegistrationContext<'_>, _: &[RedisString]) -> Status {
    unsafe {
        RETAIN.set(raw::RedisModule_RetainString.unwrap()).unwrap();
        FREE.set(raw::RedisModule_FreeString.unwrap()).unwrap();
        CREATE.set(raw::RedisModule_CreateString.unwrap()).unwrap();
        COPY.set(raw::RedisModule_CreateStringFromString.unwrap())
            .unwrap();
        raw::RedisModule_RetainString = Some(retain);
        raw::RedisModule_FreeString = Some(free);
        raw::RedisModule_CreateString = Some(create);
        raw::RedisModule_CreateStringFromString = Some(copy);
    }
    Status::Ok
}
fn snapshot() -> [u64; 5] {
    std::array::from_fn(|i| COUNTERS[i].load(Ordering::Relaxed))
}
fn mode(ctx: &Context, value: i64) -> Result<(), RedisError> {
    match value {
        0 => (),
        1 => ctx.auto_memory(),
        _ => return Err(RedisError::Str("ERR AM mode must be 0 or 1")),
    }
    Ok(())
}
fn require(value: bool, message: &'static str) -> Result<(), RedisError> {
    if value {
        Ok(())
    } else {
        Err(RedisError::Str(message))
    }
}
fn lifecycle(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    mode(ctx, args.next_i64()?)?;
    args.done()?;
    let before = snapshot();
    {
        let original = RedisString::from_bytes(b"a\0\xff");
        let mut copied = original.clone();
        require(
            copied.append("x") == Status::Ok,
            "ERR deep-copy append failed",
        )?;
        require(
            original.as_slice() == b"a\0\xff",
            "ERR deep-copy changed original",
        )?;
        let mut alias = original.safe_clone(ctx);
        require(
            alias.append("x") == Status::Err,
            "ERR retained alias append policy",
        )?;
        require(alias.as_slice() == b"a\0\xff", "ERR alias bytes changed")?;
        let transferred = unsafe {
            raw::RedisModule_CreateString.unwrap()(ctx.get_raw(), b"owned".as_ptr().cast(), 5)
        };
        let mut adopted = unsafe {
            RedisString::from_owned_ptr(
                NonNull::new(ctx.get_raw()),
                NonNull::new(transferred)
                    .ok_or(RedisError::Str("ERR native creation returned null"))?,
            )
        };
        require(
            adopted.append("!") == Status::Ok,
            "ERR adopted append failed",
        )?;
        require(adopted.as_slice() == b"owned!", "ERR adopted bytes")?;
        let manual = unsafe {
            raw::RedisModule_CreateString.unwrap()(ptr::null_mut(), b"manual".as_ptr().cast(), 6)
        };
        let _manual = unsafe {
            RedisString::from_owned_ptr(
                None,
                NonNull::new(manual).ok_or(RedisError::Str("ERR native creation returned null"))?,
            )
        };
    }
    let after = snapshot();
    let observed = std::array::from_fn::<_, 5, _>(|i| after[i] - before[i]);
    // The client asserts counts; returning them keeps failures observable.
    Ok(RedisValue::Array(
        observed
            .into_iter()
            .map(|v| RedisValue::Integer(v as i64))
            .collect(),
    ))
}
thread_local! {
    // Fixture-only retention on the invoking command thread. TLS teardown has
    // no Redis lock proof, so it must not run native string destructors. Owners
    // are instead released by replace/clear/deinit during a legal Redis phase;
    // process-exit leftovers are reclaimed with the process.
    static STORED: RefCell<ManuallyDrop<Vec<RedisString>>> =
        const { RefCell::new(ManuallyDrop::new(Vec::new())) };
}

fn replace_stored(owners: Vec<RedisString>) {
    let previous = STORED.with(|stored| std::mem::replace(&mut **stored.borrow_mut(), owners));
    // Release the RefCell borrow before native destruction.
    drop(previous);
}

fn deinit(_: &EventContext) -> Status {
    replace_stored(Vec::new());
    Status::Ok
}
fn store(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let origin = args.next_arg()?;
    mode(ctx, args.next_i64()?)?;
    let key = args.next_arg()?;
    args.done()?;
    let mut owners = Vec::new();
    match origin.try_as_str()? {
        "argv" => owners.push(key.safe_clone(ctx)),
        "create" => owners.push(ctx.create_string("context-created")),
        "owned" => {
            let p = unsafe {
                raw::RedisModule_CreateString.unwrap()(ctx.get_raw(), b"owned".as_ptr().cast(), 5)
            };
            owners.push(unsafe {
                RedisString::from_owned_ptr(
                    NonNull::new(ctx.get_raw()),
                    NonNull::new(p).ok_or(RedisError::Str("ERR native creation returned null"))?,
                )
            });
        }
        "info" => {
            let info = ctx.server_info("server");
            require(
                info.field("missing_owned_field").is_none(),
                "ERR missing INFO field present",
            )?;
            owners.push(
                info.field("redis_version")
                    .ok_or(RedisError::Str("ERR INFO version missing"))?,
            );
        }
        "hash" => {
            let handle = ctx.open_key(&key);
            owners.push(
                handle
                    .hash_get("f")?
                    .ok_or(RedisError::Str("ERR hash field missing"))?,
            );
            owners.push(
                handle
                    .hash_get_by_string(&RedisString::from_bytes(b"f"))?
                    .ok_or(RedisError::Str("ERR hash field missing"))?,
            );
            for (_, value) in handle
                .hash_get_multi::<_, RedisString>(&["f", "missing"])?
                .ok_or(RedisError::Str("ERR hash missing"))?
            {
                owners.push(value);
            }
        }
        "list" => {
            let handle = ctx.open_key_writable(&key);
            owners.push(
                handle
                    .list_pop_head()
                    .ok_or(RedisError::Str("ERR list head missing"))?,
            );
            owners.push(
                handle
                    .list_pop_tail()
                    .ok_or(RedisError::Str("ERR list tail missing"))?,
            );
            require(
                handle.list_pop_head().is_none(),
                "ERR unexpected extra list item",
            )?;
        }
        "stream" => {
            let handle = ctx.open_key(&key);
            for item in handle.get_stream_iterator(false)? {
                for (field, value) in item.fields {
                    owners.push(field);
                    owners.push(value);
                }
            }
        }
        "user" => owners.push(
            ctx.get_current_user()
                .ok_or(RedisError::Str("ERR ordinary command user missing"))?,
        ),
        _ => return Err(RedisError::Str("ERR unknown producer")),
    }
    let count = owners.len();
    replace_stored(owners);
    Ok(RedisValue::Integer(count as i64))
}
fn read(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    if argv.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    // Fresh views in this later command, after any command-return argv trimming.
    Ok(RedisValue::Array(STORED.with(|stored| {
        stored
            .borrow()
            .iter()
            .map(|s| RedisValue::StringBuffer(s.as_slice().to_vec()))
            .collect()
    })))
}
fn clear(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    if argv.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    replace_stored(Vec::new());
    Ok(RedisValue::SimpleStringStatic("OK"))
}
fn scan(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    mode(ctx, args.next_i64()?)?;
    let key = args.next_arg()?;
    let stop = args.next_i64()?;
    args.done()?;
    let cursor = ScanKeyCursor::new(ctx.open_key(&key));
    let mut pairs = Vec::new();
    let mut callbacks = 0;
    let mut scan_error = None;
    loop {
        let more = cursor.scan(|callback_key, field, value| {
            callbacks += 1;
            if callback_key.key_type() == raw::KeyType::Hash {
                // Exercises the actual producer context propagated in callback metadata.
                match callback_key.hash_get(field.as_slice()) {
                    Ok(Some(from_key))
                        if value.is_some_and(|v| from_key.as_slice() == v.as_slice()) => {}
                    Ok(_) => scan_error = Some(RedisError::Str("ERR scan getter/value mismatch")),
                    Err(error) => scan_error = Some(error),
                }
            }
            pairs.push(RedisValue::Array(vec![
                RedisValue::BulkRedisString(field.clone()),
                value.map_or(RedisValue::Null, |v| RedisValue::BulkRedisString(v.clone())),
            ]));
        });
        if let Some(error) = scan_error.take() {
            return Err(error);
        }
        if !more || stop != 0 {
            break;
        }
    }
    Ok(RedisValue::Array(vec![
        RedisValue::Integer(callbacks),
        RedisValue::Array(pairs),
    ]))
}
fn keys(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    if argv.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    let cursor = KeysCursor::new();
    let mut result = Vec::new();
    let mut count = 0;
    let mut callback = |ctx: &redis_module::RuntimeContext,
                        name: &RedisString,
                        _: Option<&redis_module::key::RedisKey>| {
        count += 1;
        result.push(RedisValue::BulkRedisString(name.safe_clone(ctx)));
    };
    while cursor.scan(ctx, &mut callback) {}
    Ok(RedisValue::Array(vec![
        RedisValue::Integer(count),
        RedisValue::Array(result),
    ]))
}
// Runs one producer many times and drops every string it returns. The reply is a
// count only, so a leaked string per call shows up in INFO used_memory.
fn churn(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let producer = args.next_arg()?;
    mode(ctx, args.next_i64()?)?;
    let iterations = args.next_u64()?;
    args.done()?;
    let mut produced = 0;
    match producer.try_as_str()? {
        "keys" => {
            let mut callback =
                |_: &redis_module::RuntimeContext,
                 _: &RedisString,
                 _: Option<&redis_module::key::RedisKey>| {
                    produced += 1;
                };
            for _ in 0..iterations {
                let cursor = KeysCursor::new();
                while cursor.scan(ctx, &mut callback) {}
            }
        }
        "list" => {
            let handle = ctx.open_key_writable(&RedisString::from_bytes(b"owner:churn"));
            for i in 0..iterations {
                // Longer than an embedded string, so each popped string is two allocations.
                handle.list_push_tail(RedisString::from_bytes(&[b'v'; 64]));
                let popped = if i % 2 == 0 {
                    handle.list_pop_head()
                } else {
                    handle.list_pop_tail()
                };
                require(popped.is_some(), "ERR churn list empty")?;
                produced += 1;
            }
        }
        "info" => {
            let info = ctx.server_info("server");
            for _ in 0..iterations {
                require(
                    info.field("redis_version").is_some(),
                    "ERR INFO version missing",
                )?;
                produced += 1;
            }
        }
        _ => return Err(RedisError::Str("ERR unknown producer")),
    }
    Ok(RedisValue::Integer(produced))
}
// Ordinary save/load of one string reference, exercised through DUMP/RESTORE.
unsafe extern "C" fn rdb_save(io: *mut raw::RedisModuleIO, value: *mut std::ffi::c_void) {
    // SAFETY: Redis passes this type's own IO and value.
    unsafe { raw::save_redis_string(io, &*value.cast::<RedisString>()) };
}
unsafe extern "C" fn rdb_load(io: *mut raw::RedisModuleIO, version: i32) -> *mut std::ffi::c_void {
    unsafe {
        if version != 0 {
            return ptr::null_mut();
        }
        match raw::load_string(io) {
            Ok(s) => Box::into_raw(Box::new(s)).cast(),
            Err(_) => ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn rdb_free(value: *mut std::ffi::c_void) {
    drop(unsafe { Box::from_raw(value.cast::<RedisString>()) });
}
static OWNER_TYPE: redis_module::native_types::RedisType =
    redis_module::native_types::RedisType::new(
        "owner0001",
        0,
        raw::RedisModuleTypeMethods {
            version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
            rdb_save: Some(rdb_save),
            rdb_load: Some(rdb_load),
            free: Some(rdb_free),
            aof_rewrite: None,
            mem_usage: None,
            digest: None,
            aux_load: None,
            aux_save: None,
            aux_save2: None,
            aux_save_triggers: 0,
            free_effort: None,
            unlink: None,
            copy: None,
            defrag: None,
            copy2: None,
            free_effort2: None,
            mem_usage2: None,
            unlink2: None,
        },
    );
fn rdb_set(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let key = args.next_arg()?;
    let value = args.next_arg()?;
    args.done()?;
    // Deep copy gives the datatype one manual reference of its own.
    ctx.open_key_writable(&key)
        .set_value(&OWNER_TYPE, value.clone())?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}
fn rdb_get(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let key = args.next_arg()?;
    args.done()?;
    let handle = ctx.open_key(&key);
    Ok(handle
        .get_value::<RedisString>(&OWNER_TYPE)?
        .map_or(RedisValue::Null, |s| {
            RedisValue::StringBuffer(s.as_slice().to_vec())
        }))
}
fn transfer(ctx: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = argv.into_iter().skip(1);
    let value = args.next_arg()?;
    args.done()?;
    // Owned StrCallArgs transfers a copied byte string, then releases it after Call.
    ctx.call("SET", &[b"owner:transfer".as_slice(), value.as_slice()])?;
    ctx.call("STRLEN", &[b"owner:transfer".as_slice()])
}
redis_module! {
    name: "string_ownership", version: 1,
    allocator: (redis_module::alloc::RedisAlloc,redis_module::alloc::RedisAlloc),
    data_types: [OWNER_TYPE], init: init, deinit: deinit,
    commands: [
        ["owner.lifecycle",lifecycle,"",0,0,0,""],
        ["owner.store",store,"",0,0,0,""],
        ["owner.read",read,"readonly",0,0,0,""],
        ["owner.clear",clear,"",0,0,0,""],
        ["owner.scan",scan,"readonly",0,0,0,""],
        ["owner.keys",keys,"readonly",0,0,0,""],
        ["owner.churn",churn,"",0,0,0,""],
        ["owner.rdbset",rdb_set,"write",1,1,1,""],
        ["owner.rdbget",rdb_get,"readonly",1,1,1,""],
        ["owner.transfer",transfer,"readonly",0,0,0,""],
    ],
}
