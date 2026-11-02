//! `random.hex 32` fills a caller-owned generation-id buffer. `random.bytes 32`
//! returns binary output. Diagnostics exercise missing bindings and context views.
use redis_module::{
    Context, EventContext, NextArg, NotifyEvent, RedisError, RedisGILGuard, RedisResult,
    RedisString, RegistrationContext, RuntimeContext, Status, ThreadSafeContext, raw,
    redis_event_handler, redis_module,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

static GENERATION: RedisGILGuard<[u8; 32]> = RedisGILGuard::new([0; 32]);
static EVENT_OK: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<JoinHandle<Result<(), RedisError>>>> = Mutex::new(None);

fn filled(ctx: &RuntimeContext, len: usize, hex: bool) -> Result<Vec<u8>, RedisError> {
    if len > 4096 {
        return Err(RedisError::Str("ERR example output limit is 4096 bytes"));
    }
    let mut buffer = vec![0xa5; len + 2];
    if hex {
        ctx.fill_random_hex(&mut buffer[1..1 + len])?;
    } else {
        ctx.fill_random_bytes(&mut buffer[1..1 + len])?;
    }
    if buffer[0] != 0xa5 || buffer[len + 1] != 0xa5 {
        return Err(RedisError::Str("ERR random fill exceeded the slice"));
    }
    let output = &buffer[1..1 + len];
    if hex
        && !output
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(RedisError::Str("ERR random hex contained a non-hex byte"));
    }
    Ok(output.to_vec())
}

fn length(args: Vec<RedisString>) -> Result<usize, RedisError> {
    let mut args = args.into_iter().skip(1);
    let length = args.next_string()?.parse()?;
    args.done()?;
    Ok(length)
}

fn bytes(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    Ok(filled(ctx, length(args)?, false)?.into())
}

fn hex(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    Ok(filled(ctx, length(args)?, true)?.into())
}

fn generation(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(GENERATION.lock(ctx).to_vec().into())
}

fn on_event(ctx: &EventContext, _: NotifyEvent, _: &str, key: &[u8]) {
    if key == b"random:trigger" {
        EVENT_OK.store(
            filled(ctx, 17, true)
                .and_then(|_| filled(ctx, 17, false))
                .is_ok(),
            Ordering::Release,
        );
    }
}

fn event_ok(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(i64::from(EVENT_OK.load(Ordering::Acquire)).into())
}

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    let result = (|| {
        let mut id = [0; 32];
        ctx.fill_random_hex(&mut id)?;
        *GENERATION.lock(ctx) = id;
        filled(ctx, 17, false)?;
        EVENT_OK.store(false, Ordering::Release);
        STOP.store(false, Ordering::Release);
        ctx.subscribe_to_keyspace_events(NotifyEvent::STRING, redis_event_handler!(on_event))
    })();
    match result {
        Ok(()) => Status::Ok,
        Err(error) => {
            ctx.log_warning(&error.to_string());
            Status::Err
        }
    }
}

fn worker(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap();
    if worker.is_some() {
        return Err(RedisError::Str("ERR worker already started"));
    }
    let owner = ThreadSafeContext::new();
    STOP.store(false, Ordering::Release);
    *worker = Some(thread::spawn(move || {
        while !STOP.load(Ordering::Acquire) {
            if let Some(guard) = owner.try_lock() {
                filled(&guard, 17, false)?;
                filled(&guard, 33, true)?;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }));
    Ok("OK".into())
}

fn join(worker: JoinHandle<Result<(), RedisError>>) -> Result<(), RedisError> {
    worker
        .join()
        .map_err(|_| RedisError::Str("ERR random worker panicked"))?
}

fn finish(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let mut worker = WORKER.lock().unwrap();
    if !worker.as_ref().is_some_and(JoinHandle::is_finished) {
        return Ok(0_i64.into());
    }
    join(worker.take().unwrap())?;
    Ok(1_i64.into())
}

fn deinit(_: &EventContext) -> Status {
    STOP.store(true, Ordering::Release);
    // try_lock allows a waiting worker to observe STOP while unload holds
    // Redis's lock. Join before its context or executable code can outlive us.
    match WORKER.lock().unwrap().take().map(join).transpose() {
        Ok(_) => Status::Ok,
        Err(_) => Status::Err,
    }
}

// Each scoped instance points at this module's binding. Commands hold the
// Redis lock and never yield while the binding is hidden, including on errors.
struct Restore<T: Copy> {
    slot: *mut T,
    value: T,
}

impl<T: Copy> Drop for Restore<T> {
    fn drop(&mut self) {
        unsafe { self.slot.write(self.value) };
    }
}

fn missing(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let mode = args.next_string()?;
    args.done()?;
    let mut buffer = [0xa5; 33];
    let (result, empty) = match mode.as_str() {
        "bytes" => {
            let _restore = Restore {
                slot: std::ptr::addr_of_mut!(raw::RedisModule_GetRandomBytes),
                value: unsafe { raw::RedisModule_GetRandomBytes },
            };
            unsafe { raw::RedisModule_GetRandomBytes = None };
            (
                ctx.fill_random_bytes(&mut buffer),
                ctx.fill_random_bytes(&mut []),
            )
        }
        "hex" => {
            let _restore = Restore {
                slot: std::ptr::addr_of_mut!(raw::RedisModule_GetRandomHexChars),
                value: unsafe { raw::RedisModule_GetRandomHexChars },
            };
            unsafe { raw::RedisModule_GetRandomHexChars = None };
            (
                ctx.fill_random_hex(&mut buffer),
                ctx.fill_random_hex(&mut []),
            )
        }
        _ => return Err(RedisError::Str("ERR expected bytes or hex")),
    };
    let error = result
        .err()
        .ok_or(RedisError::Str("ERR missing API unexpectedly succeeded"))?;
    if empty.is_ok() || buffer != [0xa5; 33] {
        return Err(RedisError::Str(
            "ERR missing API changed the destination or accepted empty output",
        ));
    }
    Ok(error.to_string().into())
}

redis_module! {
    name: "random_fill", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init, deinit: deinit,
    commands: [
        ["random.bytes", bytes, "readonly", 0, 0, 0, ""],
        ["random.hex", hex, "readonly", 0, 0, 0, ""],
        ["random.generation", generation, "readonly", 0, 0, 0, ""],
        ["random.event_ok", event_ok, "readonly", 0, 0, 0, ""],
        ["random.worker", worker, "readonly", 0, 0, 0, ""],
        ["random.finish", finish, "readonly", 0, 0, 0, ""],
        ["random.missing", missing, "readonly", 0, 0, 0, ""],
    ],
}
