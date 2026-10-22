//! A string configuration whose getter returns a new string on every call.
//! The module counts those strings as they are returned and as they are freed,
//! through a wrapper around its own `RedisModule_FreeString` pointer, so tests
//! can check that CONFIG GET does not leak getter replies.
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use redis_module::{
    ConfigurationValue, Context, RedisError, RedisResult, RedisString, RedisValue, Status,
    configuration::{ConfigurationContext, ConfigurationFlags},
    raw, redis_module,
};

type FreeString = unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleString);

static FREE_STRING: OnceLock<FreeString> = OnceLock::new();
static TRACKING: AtomicBool = AtomicBool::new(false);
static RETURNED: AtomicI64 = AtomicI64::new(0);
static LIVE: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());
static FREE_MARKERS: Mutex<Option<PathBuf>> = Mutex::new(None);
static VALUE: Mutex<String> = Mutex::new(String::new());

unsafe extern "C" fn counting_free_string(
    ctx: *mut raw::RedisModuleCtx,
    string: *mut raw::RedisModuleString,
) {
    unsafe {
        let tracked = LIVE.lock().unwrap().remove(&(string as usize));
        if tracked {
            if let Some(path) = FREE_MARKERS.lock().unwrap().as_ref() {
                // Test-only external evidence survives dlclose. A failed write
                // becomes a missing marker assertion, never an FFI panic.
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path)
                {
                    let _ = file.write_all(b"F");
                }
            }
        }
        let free_string = FREE_STRING.get().expect("FreeString saved at load");
        free_string(ctx, string);
    }
}

struct Text;
static TEXT: Text = Text;

impl ConfigurationValue<RedisString> for Text {
    fn get(&self, _: &ConfigurationContext) -> RedisString {
        let value = RedisString::create(None, VALUE.lock().unwrap().as_str());
        if TRACKING.load(Ordering::Relaxed) {
            RETURNED.fetch_add(1, Ordering::Relaxed);
            LIVE.lock().unwrap().insert(value.as_ptr() as usize);
        }
        value
    }

    fn set(&self, _: &ConfigurationContext, value: RedisString) -> Result<(), RedisError> {
        *VALUE.lock().unwrap() = value.try_as_str()?.to_owned();
        Ok(())
    }
}

fn init(_: &redis_module::RegistrationContext<'_>, args: &[RedisString]) -> Status {
    *FREE_MARKERS.lock().unwrap() = args
        .first()
        .map(|path| PathBuf::from(path.try_as_str().expect("UTF-8 marker path")));
    // SAFETY: OnLoad runs on the main thread before any command, so nothing
    // reads the module's FreeString pointer while it is replaced.
    unsafe {
        let Some(free_string) = raw::RedisModule_FreeString else {
            return Status::Err;
        };
        FREE_STRING.get_or_init(|| free_string);
        raw::RedisModule_FreeString = Some(counting_free_string);
    }
    TRACKING.store(true, Ordering::Relaxed);
    Status::Ok
}

/// Replies with the number of getter strings returned since load and the
/// number of them that have not been freed.
fn strings(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let live = LIVE.lock().unwrap().len() as i64;
    Ok(RedisValue::Array(vec![
        RETURNED.load(Ordering::Relaxed).into(),
        live.into(),
    ]))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "configuration_getter",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        ["configuration_getter.strings", strings, "readonly", 0, 0, 0, ""],
    ],
    configurations: [
        i64: [],
        string: [
            ["text", &TEXT, "default", ConfigurationFlags::DEFAULT, None],
        ],
        bool: [],
        enum: [],
        module_args_as_configuration: false,
    ]
}
