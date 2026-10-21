use crate::context::thread_safe::{RedisGILGuard, RedisLockIndicator};
use crate::{CallOptionResp, CallOptionsBuilder, CallResult, RedisValue, raw};
use crate::{Context, RedisError, RedisString, RegistrationContext};
use bitflags::bitflags;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::os::raw::{c_char, c_int, c_longlong, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

mod registration;
use registration::PendingRegistration;

bitflags! {
    /// Configuration options
    pub struct ConfigurationFlags : u32 {
        /// The default flags for a config. This creates a config that can be modified after startup.
        const DEFAULT = raw::REDISMODULE_CONFIG_DEFAULT;

        /// This config can only be provided loading time.
        const IMMUTABLE = raw::REDISMODULE_CONFIG_IMMUTABLE;

        /// The value stored in this config is redacted from all logging.
        const SENSITIVE = raw::REDISMODULE_CONFIG_SENSITIVE;

        /// The name is hidden from `CONFIG GET` with pattern matching.
        const HIDDEN = raw::REDISMODULE_CONFIG_HIDDEN;

        /// This config will be only be modifiable based off the value of enable-protected-configs.
        const PROTECTED = raw::REDISMODULE_CONFIG_PROTECTED;

        /// This config is not modifiable while the server is loading data.
        const DENY_LOADING = raw::REDISMODULE_CONFIG_DENY_LOADING;

        /// For numeric configs, this config will convert data unit notations into their byte equivalent.
        const MEMORY = raw::REDISMODULE_CONFIG_MEMORY;

        /// For enum configs, this config will allow multiple entries to be combined as bit flags.
        const BITFLAGS = raw::REDISMODULE_CONFIG_BITFLAGS;

        /// For configs, will avoid prefixing the config name with the module name.
        const UNPREFIXED = raw::REDISMODULE_CONFIG_UNPREFIXED;
    }
}

#[macro_export]
macro_rules! enum_configuration {
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$vmeta:meta])* $vname:ident = $val:expr,)*
    }) => {
        use $crate::configuration::EnumConfigurationValue;
        $(#[$meta])*
        $vis enum $name {
            $($(#[$vmeta])* $vname = $val,)*
        }

        impl std::convert::TryFrom<i32> for $name {
            type Error = $crate::RedisError;

            fn try_from(v: i32) -> Result<Self, Self::Error> {
                match v {
                    $(x if x == $name::$vname as i32 => Ok($name::$vname),)*
                    _ => Err($crate::RedisError::Str("Value is not supported")),
                }
            }
        }

        impl std::convert::From<$name> for i32 {
            fn from(val: $name) -> Self {
                val as i32
            }
        }

        impl EnumConfigurationValue for $name {
            fn get_options(&self) -> (Vec<String>, Vec<i32>) {
                (vec![$(stringify!($vname).to_string(),)*], vec![$($val,)*])
            }
        }

        impl Clone for $name {
            fn clone(&self) -> Self {
                match self {
                    $($name::$vname => $name::$vname,)*
                }
            }
        }
    }
}

/// [`ConfigurationContext`] is used as a special context that indicate that we are
/// running with the Redis GIL is held but we should not perform all the regular
/// operation we can perfrom on the regular Context.
pub struct ConfigurationContext {
    // Private, so users cannot construct one, and a raw pointer type, so it is
    // neither Send nor Sync: it proves the lock is held on this thread only.
    _not_send: std::marker::PhantomData<*mut ()>,
}

impl ConfigurationContext {
    fn new() -> ConfigurationContext {
        ConfigurationContext {
            _not_send: std::marker::PhantomData,
        }
    }
}

unsafe impl RedisLockIndicator for ConfigurationContext {}

/// A configuration value. Values are registered as `&'static` references and only
/// shared, so they need to be `Sync` but not `Send`.
pub trait ConfigurationValue<T>: Sync {
    fn get(&self, ctx: &ConfigurationContext) -> T;
    fn set(&self, ctx: &ConfigurationContext, val: T) -> Result<(), RedisError>;
}

pub trait EnumConfigurationValue: TryFrom<i32, Error = RedisError> + Into<i32> + Clone {
    fn get_options(&self) -> (Vec<String>, Vec<i32>);
}

impl<T: Clone> ConfigurationValue<T> for RedisGILGuard<T> {
    fn get(&self, ctx: &ConfigurationContext) -> T {
        let value = self.lock(ctx);
        value.clone()
    }
    fn set(&self, ctx: &ConfigurationContext, val: T) -> Result<(), RedisError> {
        let mut value = self.lock(ctx);
        *value = val;
        Ok(())
    }
}

impl<T: Clone + Send> ConfigurationValue<T> for Mutex<T> {
    fn get(&self, _ctx: &ConfigurationContext) -> T {
        let value = self.lock().unwrap();
        value.clone()
    }
    fn set(&self, _ctx: &ConfigurationContext, val: T) -> Result<(), RedisError> {
        let mut value = self.lock().unwrap();
        *value = val;
        Ok(())
    }
}

impl ConfigurationValue<i64> for AtomicI64 {
    fn get(&self, _ctx: &ConfigurationContext) -> i64 {
        self.load(Ordering::Relaxed)
    }
    fn set(&self, _ctx: &ConfigurationContext, val: i64) -> Result<(), RedisError> {
        self.store(val, Ordering::Relaxed);
        Ok(())
    }
}

impl ConfigurationValue<RedisString> for RedisGILGuard<String> {
    fn get(&self, ctx: &ConfigurationContext) -> RedisString {
        let value = self.lock(ctx);
        RedisString::create(None, value.as_str())
    }
    fn set(&self, ctx: &ConfigurationContext, val: RedisString) -> Result<(), RedisError> {
        let mut value = self.lock(ctx);
        *value = val.try_as_str()?.to_string();
        Ok(())
    }
}

impl ConfigurationValue<RedisString> for Mutex<String> {
    fn get(&self, _ctx: &ConfigurationContext) -> RedisString {
        let value = self.lock().unwrap();
        RedisString::create(None, value.as_str())
    }
    fn set(&self, _ctx: &ConfigurationContext, val: RedisString) -> Result<(), RedisError> {
        let mut value = self.lock().unwrap();
        *value = val.try_as_str()?.to_string();
        Ok(())
    }
}

impl ConfigurationValue<bool> for AtomicBool {
    fn get(&self, _ctx: &ConfigurationContext) -> bool {
        self.load(Ordering::Relaxed)
    }
    fn set(&self, _ctx: &ConfigurationContext, val: bool) -> Result<(), RedisError> {
        self.store(val, Ordering::Relaxed);
        Ok(())
    }
}

type OnUpdatedCallback<T> = Box<dyn Fn(&ConfigurationContext, &str, &'static T)>;

struct ConfigrationPrivateData<G, T: ConfigurationValue<G> + 'static> {
    variable: &'static T,
    on_changed: Option<OnUpdatedCallback<T>>,
    phantom: PhantomData<G>,
}

impl<G, T: ConfigurationValue<G> + 'static> ConfigrationPrivateData<G, T> {
    fn set_val(&self, name: *const c_char, val: G, err: *mut *mut raw::RedisModuleString) -> c_int {
        // we know the GIL is held so it is safe to use Context::dummy().
        let configuration_ctx = ConfigurationContext::new();
        if let Err(e) = self.variable.set(&configuration_ctx, val) {
            let error_msg = RedisString::create(None, e.to_string().as_str());
            unsafe { *err = error_msg.take() };
            return raw::REDISMODULE_ERR as i32;
        }
        let c_str_name = unsafe { CStr::from_ptr(name) };
        if let Some(v) = self.on_changed.as_ref() {
            v(
                &configuration_ctx,
                c_str_name.to_str().unwrap(),
                self.variable,
            )
        }
        raw::REDISMODULE_OK as i32
    }

    fn get_val(&self) -> G {
        self.variable.get(&ConfigurationContext::new())
    }
}

extern "C" fn i64_configuration_set<T: ConfigurationValue<i64> + 'static>(
    name: *const c_char,
    val: c_longlong,
    privdata: *mut c_void,
    err: *mut *mut raw::RedisModuleString,
) -> c_int {
    crate::panic::run_config_set(err, || {
        let private_data = unsafe { &*(privdata as *const ConfigrationPrivateData<i64, T>) };
        private_data.set_val(name, val, err)
    })
}

extern "C" fn i64_configuration_get<T: ConfigurationValue<i64> + 'static>(
    _name: *const c_char,
    privdata: *mut c_void,
) -> c_longlong {
    crate::panic::run_callback(
        "configuration getter",
        || {
            let private_data = unsafe { &*(privdata as *const ConfigrationPrivateData<i64, T>) };
            private_data.get_val()
        },
        || 0,
    )
}

/// Registers a numeric configuration. Call it during module OnLoad.
///
/// The default must be within the inclusive `min` and `max`, even when a
/// load-time configuration will override it. Returns an error if it is outside
/// those bounds, the Redis API is missing, or Redis rejects the registration
/// (for example a duplicate or invalid name); the private data and `on_changed`
/// callback of a rejected registration are dropped. Outside the `configurations`
/// block of `redis_module!`, call [`load_configurations`] after the last
/// registration.
#[expect(clippy::too_many_arguments)]
pub fn register_i64_configuration<T: ConfigurationValue<i64>>(
    ctx: &RegistrationContext<'_>,
    name: &str,
    variable: &'static T,
    default: i64,
    min: i64,
    max: i64,
    flags: ConfigurationFlags,
    on_changed: Option<OnUpdatedCallback<T>>,
) -> Result<(), RedisError> {
    if default < min || default > max {
        return Err(RedisError::String(format!(
            "Numeric configuration '{name}' default must be between {min} and {max}"
        )));
    }
    let name = CString::new(name).unwrap();
    let config_private_data = ConfigrationPrivateData {
        variable,
        on_changed,
        phantom: PhantomData::<i64>,
    };
    let (register, pending) = PendingRegistration::prepare(
        unsafe { raw::RedisModule_RegisterNumericConfig },
        config_private_data,
        "Numeric",
    )?;
    let status = unsafe {
        register(
            ctx.ctx,
            name.as_ptr(),
            default,
            flags.bits(),
            min,
            max,
            Some(i64_configuration_get::<T>),
            Some(i64_configuration_set::<T>),
            None,
            pending.as_ptr().cast(),
        )
    };
    pending.finish(status)
}

fn find_config_value<'a>(args: &'a [RedisString], name: &str) -> Option<&'a RedisString> {
    args.chunks_exact(2)
        .find(|pair| pair[0].as_slice() == name.as_bytes())
        .map(|pair| &pair[1])
}

pub fn get_i64_default_config_value(
    args: &[RedisString],
    name: &str,
    default: i64,
) -> Result<i64, RedisError> {
    find_config_value(args, name).map_or(Ok(default), |arg| {
        arg.try_as_str()?
            .parse::<i64>()
            .map_err(|e| RedisError::String(e.to_string()))
    })
}

/// The default of a configuration with [`ConfigurationFlags::MEMORY`] from
/// the module arguments, which may give it with a unit, as Redis's own memory
/// configurations take: a number and an optional `b`, `k` (1000), `kb` (1024),
/// `m`, `mb`, `g` or `gb`, in any case.
pub fn get_memory_default_config_value(
    args: &[RedisString],
    name: &str,
    default: i64,
) -> Result<i64, RedisError> {
    find_config_value(args, name).map_or(Ok(default), |arg| parse_memory(arg.try_as_str()?))
}

/// Parses a memory amount as Redis's `memtoull` does.
fn parse_memory(text: &str) -> Result<i64, RedisError> {
    const UNITS: [(&str, i64); 7] = [
        ("gb", 1 << 30),
        ("mb", 1 << 20),
        ("kb", 1 << 10),
        ("g", 1_000_000_000),
        ("m", 1_000_000),
        ("k", 1_000),
        ("b", 1),
    ];
    let invalid = || RedisError::String(format!("invalid memory amount '{text}'"));
    let lower = text.to_ascii_lowercase();
    let (digits, unit) = UNITS
        .iter()
        .find_map(|(suffix, unit)| lower.strip_suffix(suffix).map(|digits| (digits, *unit)))
        .unwrap_or((lower.as_str(), 1));
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    digits
        .parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(unit))
        .ok_or_else(invalid)
}

extern "C" fn string_configuration_set<T: ConfigurationValue<RedisString> + 'static>(
    name: *const c_char,
    val: *mut raw::RedisModuleString,
    privdata: *mut c_void,
    err: *mut *mut raw::RedisModuleString,
) -> c_int {
    crate::panic::run_config_set(err, || {
        let new_val = unsafe {
            RedisString::retain_from_ptr(
                std::ptr::NonNull::new(val).expect("non-null configuration input"),
            )
        };
        let private_data = unsafe { &*(privdata as *const StringConfigurationPrivateData<T>) };
        private_data.data.set_val(name, new_val, err)
    })
}

/// Private data of a string configuration.
///
/// Redis copies the string a getter returns and never frees it, so the last
/// returned string is kept here and dropped when the next get replaces it.
struct StringConfigurationPrivateData<T: ConfigurationValue<RedisString> + 'static> {
    data: ConfigrationPrivateData<RedisString, T>,
    last_reply: RefCell<Option<RedisString>>,
}

extern "C" fn string_configuration_get<T: ConfigurationValue<RedisString> + 'static>(
    _name: *const c_char,
    privdata: *mut c_void,
) -> *mut raw::RedisModuleString {
    // Redis reads a null string as empty.
    crate::panic::run_callback(
        "configuration getter",
        || {
            let private_data = unsafe { &*(privdata as *const StringConfigurationPrivateData<T>) };
            // we know the GIL is held so it is safe to use Context::dummy().
            let reply = private_data.data.get_val();
            let reply_ptr = reply.as_ptr();
            // Redis copied the previous reply when that get returned, so it can go now.
            *private_data.last_reply.borrow_mut() = Some(reply);
            reply_ptr
        },
        std::ptr::null_mut,
    )
}

/// Registers a string configuration. Call it during module OnLoad.
///
/// Returns an error if the Redis API is missing or Redis rejects the
/// registration (for example a duplicate or invalid name); the private data and
/// `on_changed` callback of a rejected registration are dropped. Outside the
/// `configurations` block of `redis_module!`, call [`load_configurations`]
/// after the last registration.
pub fn register_string_configuration<T: ConfigurationValue<RedisString>>(
    ctx: &RegistrationContext<'_>,
    name: &str,
    variable: &'static T,
    default: &str,
    flags: ConfigurationFlags,
    on_changed: Option<OnUpdatedCallback<T>>,
) -> Result<(), RedisError> {
    let name = CString::new(name).unwrap();
    let default = CString::new(default).unwrap();
    let config_private_data = StringConfigurationPrivateData {
        data: ConfigrationPrivateData {
            variable,
            on_changed,
            phantom: PhantomData::<RedisString>,
        },
        last_reply: RefCell::new(None),
    };
    let (register, pending) = PendingRegistration::prepare(
        unsafe { raw::RedisModule_RegisterStringConfig },
        config_private_data,
        "String",
    )?;
    let status = unsafe {
        register(
            ctx.ctx,
            name.as_ptr(),
            default.as_ptr(),
            flags.bits(),
            Some(string_configuration_get::<T>),
            Some(string_configuration_set::<T>),
            None,
            pending.as_ptr().cast(),
        )
    };
    pending.finish(status)
}

pub fn get_string_default_config_value<'a>(
    args: &'a [RedisString],
    name: &str,
    default: &'a str,
) -> Result<&'a str, RedisError> {
    find_config_value(args, name).map_or(Ok(default), |arg| arg.try_as_str())
}

extern "C" fn bool_configuration_set<T: ConfigurationValue<bool> + 'static>(
    name: *const c_char,
    val: i32,
    privdata: *mut c_void,
    err: *mut *mut raw::RedisModuleString,
) -> c_int {
    crate::panic::run_config_set(err, || {
        let private_data = unsafe { &*(privdata as *const ConfigrationPrivateData<bool, T>) };
        private_data.set_val(name, val != 0, err)
    })
}

extern "C" fn bool_configuration_get<T: ConfigurationValue<bool> + 'static>(
    _name: *const c_char,
    privdata: *mut c_void,
) -> c_int {
    crate::panic::run_callback(
        "configuration getter",
        || {
            let private_data = unsafe { &*(privdata as *const ConfigrationPrivateData<bool, T>) };
            private_data.get_val() as i32
        },
        || 0,
    )
}

/// Registers a boolean configuration. Call it during module OnLoad.
///
/// Returns an error if the Redis API is missing or Redis rejects the
/// registration (for example a duplicate or invalid name); the private data and
/// `on_changed` callback of a rejected registration are dropped. Outside the
/// `configurations` block of `redis_module!`, call [`load_configurations`]
/// after the last registration.
pub fn register_bool_configuration<T: ConfigurationValue<bool>>(
    ctx: &RegistrationContext<'_>,
    name: &str,
    variable: &'static T,
    default: bool,
    flags: ConfigurationFlags,
    on_changed: Option<OnUpdatedCallback<T>>,
) -> Result<(), RedisError> {
    let name = CString::new(name).unwrap();
    let config_private_data = ConfigrationPrivateData {
        variable,
        on_changed,
        phantom: PhantomData::<bool>,
    };
    let (register, pending) = PendingRegistration::prepare(
        unsafe { raw::RedisModule_RegisterBoolConfig },
        config_private_data,
        "Boolean",
    )?;
    let status = unsafe {
        register(
            ctx.ctx,
            name.as_ptr(),
            default as i32,
            flags.bits(),
            Some(bool_configuration_get::<T>),
            Some(bool_configuration_set::<T>),
            None,
            pending.as_ptr().cast(),
        )
    };
    pending.finish(status)
}

/// Reads a boolean module argument as `yes` or `no`, ignoring ASCII case.
/// Other values are errors; an absent name uses `default`.
pub fn get_bool_default_config_value(
    args: &[RedisString],
    name: &str,
    default: bool,
) -> Result<bool, RedisError> {
    find_config_value(args, name).map_or(Ok(default), |arg| {
        let value = arg.try_as_str()?;
        if value.eq_ignore_ascii_case("yes") {
            Ok(true)
        } else if value.eq_ignore_ascii_case("no") {
            Ok(false)
        } else {
            Err(RedisError::String(format!(
                "Boolean configuration '{name}' must be 'yes' or 'no'"
            )))
        }
    })
}

extern "C" fn enum_configuration_set<
    G: EnumConfigurationValue,
    T: ConfigurationValue<G> + 'static,
>(
    name: *const c_char,
    val: i32,
    privdata: *mut c_void,
    err: *mut *mut raw::RedisModuleString,
) -> c_int {
    crate::panic::run_config_set(err, || {
        let private_data = unsafe { &*(privdata as *const ConfigrationPrivateData<G, T>) };
        let val: Result<G, _> = val.try_into();
        match val {
            Ok(val) => private_data.set_val(name, val, err),
            Err(e) => {
                let error_msg = RedisString::create(None, e.to_string().as_str());
                unsafe { *err = error_msg.take() };
                raw::REDISMODULE_ERR as i32
            }
        }
    })
}

extern "C" fn enum_configuration_get<
    G: EnumConfigurationValue,
    T: ConfigurationValue<G> + 'static,
>(
    _name: *const c_char,
    privdata: *mut c_void,
) -> c_int {
    // Redis reports a value that matches no option as "unknown".
    crate::panic::run_callback(
        "configuration getter",
        || {
            let private_data = unsafe { &*(privdata as *const ConfigrationPrivateData<G, T>) };
            private_data.get_val().into()
        },
        || -1,
    )
}

/// Registers an enum configuration. Call it during module OnLoad.
///
/// Returns an error if the Redis API is missing or Redis rejects the
/// registration (for example a duplicate or invalid name); the private data and
/// `on_changed` callback of a rejected registration are dropped. Outside the
/// `configurations` block of `redis_module!`, call [`load_configurations`]
/// after the last registration.
pub fn register_enum_configuration<G: EnumConfigurationValue, T: ConfigurationValue<G>>(
    ctx: &RegistrationContext<'_>,
    name: &str,
    variable: &'static T,
    default: G,
    flags: ConfigurationFlags,
    on_changed: Option<OnUpdatedCallback<T>>,
) -> Result<(), RedisError> {
    let name = CString::new(name).unwrap();
    let (names, vals) = default.get_options();
    assert_eq!(names.len(), vals.len());
    let names: Vec<CString> = names
        .into_iter()
        .map(|v| CString::new(v).unwrap())
        .collect();
    let config_private_data = ConfigrationPrivateData {
        variable,
        on_changed,
        phantom: PhantomData::<G>,
    };
    let (register, pending) = PendingRegistration::prepare(
        unsafe { raw::RedisModule_RegisterEnumConfig },
        config_private_data,
        "Enum",
    )?;
    // Redis copies the option names during the call.
    let mut name_pointers: Vec<*const c_char> = names.iter().map(|v| v.as_ptr()).collect();
    let status = unsafe {
        register(
            ctx.ctx,
            name.as_ptr(),
            default.into(),
            flags.bits(),
            name_pointers.as_mut_ptr(),
            vals.as_ptr(),
            names.len() as i32,
            Some(enum_configuration_get::<G, T>),
            Some(enum_configuration_set::<G, T>),
            None,
            pending.as_ptr().cast(),
        )
    };
    pending.finish(status)
}

pub fn get_enum_default_config_value<G: EnumConfigurationValue>(
    args: &[RedisString],
    name: &str,
    default: G,
) -> Result<G, RedisError> {
    find_config_value(args, name).map_or(Ok(default.clone()), |arg| {
        let (names, vals) = default.get_options();
        let (index, _name) = names
            .into_iter()
            .enumerate()
            .find(|(_index, item)| item.as_bytes().eq(arg.as_slice()))
            .ok_or(RedisError::String(format!(
                "Enum '{}' not exists",
                arg.to_string_lossy()
            )))?;
        G::try_from(vals[index])
    })
}

/// Applies the values given for the module's configurations at load time, from
/// `MODULE LOADEX ... CONFIG` or the server's configuration file, and the defaults
/// of the configurations that were not given one.
///
/// Call it once, from the module's load, after the last registration. Redis
/// applies defaults only on the first call, so a configuration registered after
/// it keeps no default. The `configurations` block of `redis_module!` calls it
/// for its own registrations before `init`, so a module that uses that block
/// should not register more configurations in `init`.
///
/// # Errors
///
/// Returns an error if Redis rejects a value, for example one out of range or one
/// a setter refuses, or if the running Redis does not provide
/// `RedisModule_LoadConfigs`. Redis logs which value it rejected. The module load
/// should fail: Redis unloads a module whose values were not applied. Setters
/// that ran before the rejected value are not undone.
///
/// Requires a [`RegistrationContext`] from the module's active load invocation.
/// Safe runtime code cannot call this function.
pub fn load_configurations(ctx: &RegistrationContext<'_>) -> Result<(), RedisError> {
    let load_configs = unsafe { crate::__available_api!(RedisModule_LoadConfigs) };
    if unsafe { load_configs(ctx.ctx) } == raw::REDISMODULE_OK as c_int {
        Ok(())
    } else {
        Err(RedisError::Str(
            "Redis rejected a module configuration value; see the Redis log",
        ))
    }
}

pub fn module_config_get(
    ctx: &Context,
    args: Vec<RedisString>,
    name: &str,
) -> Result<RedisValue, RedisError> {
    let mut args: Vec<String> = args
        .into_iter()
        .skip(1)
        .map(|e| format!("{}.{}", name, e.to_string_lossy()))
        .collect();
    args.insert(0, "get".into());
    let res: CallResult = ctx.call_ext(
        "config",
        &CallOptionsBuilder::new()
            .errors_as_replies()
            .resp(CallOptionResp::Auto)
            .build(),
        args.iter()
            .map(|v| v.as_str())
            .collect::<Vec<&str>>()
            .as_slice(),
    );
    let res = res.map_err(|e| {
        RedisError::String(
            e.to_utf8_string()
                .unwrap_or("Failed converting error to utf8".into()),
        )
    })?;
    Ok((&res).into())
}

pub fn module_config_set(
    ctx: &Context,
    args: Vec<RedisString>,
    name: &str,
) -> Result<RedisValue, RedisError> {
    let mut args: Vec<String> = args
        .into_iter()
        .skip(1)
        .enumerate()
        .map(|(index, e)| {
            if index % 2 == 0 {
                format!("{}.{}", name, e.to_string_lossy())
            } else {
                e.to_string_lossy()
            }
        })
        .collect();
    args.insert(0, "set".into());
    let res: CallResult = ctx.call_ext(
        "config",
        &CallOptionsBuilder::new()
            .errors_as_replies()
            .resp(CallOptionResp::Auto)
            .build(),
        args.iter()
            .map(|v| v.as_str())
            .collect::<Vec<&str>>()
            .as_slice(),
    );
    let res = res.map_err(|e| {
        RedisError::String(
            e.to_utf8_string()
                .unwrap_or("Failed converting error to utf8".into()),
        )
    })?;
    Ok((&res).into())
}

#[cfg(test)]
mod memory_tests {
    use super::parse_memory;

    // As Redis's memtoull, except that an empty number, which memtoull reads
    // as 0, is an error.
    #[test]
    fn memory_amounts_parse_as_redis_parses_them_but_need_a_number() {
        assert_eq!(parse_memory("100").unwrap(), 100);
        assert_eq!(parse_memory("8mb").unwrap(), 8 << 20);
        assert_eq!(parse_memory("8MB").unwrap(), 8 << 20);
        assert_eq!(parse_memory("8m").unwrap(), 8_000_000);
        assert_eq!(parse_memory("1kb").unwrap(), 1024);
        assert_eq!(parse_memory("1k").unwrap(), 1000);
        assert_eq!(parse_memory("2gb").unwrap(), 2 << 30);
        assert_eq!(parse_memory("5b").unwrap(), 5);
        for bad in ["", "mb", "-1", "1.5mb", "8 mb", "8tb", "99999999999gb"] {
            assert!(parse_memory(bad).is_err(), "{bad}");
        }
    }
}
