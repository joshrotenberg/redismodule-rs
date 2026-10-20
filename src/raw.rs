extern crate libc;

use std::cmp::Ordering;
use std::ffi::{c_ulonglong, CString};
use std::os::raw::{c_char, c_double, c_int, c_long, c_longlong, c_void};
use std::ptr;
use std::slice;

use crate::RedisResult;
use bitflags::bitflags;
use libc::size_t;

use crate::error::Error;
pub use crate::redisraw::bindings::*;
use crate::{context::StrCallArgs, RedisString};
use crate::{RedisBuffer, RedisError};

/// Panics because the running Redis does not provide the module API `name`.
///
/// Bindgen declares every API in the vendored header, and Redis leaves the
/// ones an older server lacks as `None`.
#[doc(hidden)]
#[cold]
#[track_caller]
pub fn api_unavailable(name: &str) -> ! {
    panic!("{name} is not available in this Redis server")
}

/// The error for a module API that the running Redis does not provide.
#[doc(hidden)]
#[cold]
pub fn api_unavailable_error(name: &str) -> RedisError {
    RedisError::String(format!("ERR {name} is not available in this Redis server"))
}

const GENERIC_ERROR_MESSAGE: &str = "Generic error.";

bitflags! {
    pub struct KeyMode: c_int {
        const READ = REDISMODULE_READ as c_int;
        const WRITE = REDISMODULE_WRITE as c_int;
    }
}

bitflags! {
    pub struct ModuleOptions: c_int {
        const HANDLE_IO_ERRORS = REDISMODULE_OPTIONS_HANDLE_IO_ERRORS as c_int;
        const NO_IMPLICIT_SIGNAL_MODIFIED = REDISMODULE_OPTION_NO_IMPLICIT_SIGNAL_MODIFIED as c_int;
        const HANDLE_REPL_ASYNC_LOAD = REDISMODULE_OPTIONS_HANDLE_REPL_ASYNC_LOAD as c_int;
    }
}

/// Defines a C-valued enum and its `From<c_int>`, mapping a value the enum
/// does not list to the `$unknown` variant instead of panicking.
macro_rules! c_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$variant_meta:meta])* $variant:ident = $value:expr,)* }
        unknown $unknown:ident
    ) => {
        $(#[$meta])*
        #[repr(i32)]
        #[non_exhaustive]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$variant_meta])* $variant = $value as i32,)*
        }

        impl From<c_int> for $name {
            fn from(value: c_int) -> Self {
                $(
                    if value == $value as c_int {
                        return Self::$variant;
                    }
                )*
                Self::$unknown
            }
        }
    };
}

c_enum! {
    /// The type of a key, from `RedisModule_KeyType`.
    KeyType {
        Empty = REDISMODULE_KEYTYPE_EMPTY,
        String = REDISMODULE_KEYTYPE_STRING,
        List = REDISMODULE_KEYTYPE_LIST,
        Hash = REDISMODULE_KEYTYPE_HASH,
        Set = REDISMODULE_KEYTYPE_SET,
        ZSet = REDISMODULE_KEYTYPE_ZSET,
        Module = REDISMODULE_KEYTYPE_MODULE,
        Stream = REDISMODULE_KEYTYPE_STREAM,
        /// The GCRA rate limiter type of recent Redis versions.
        Gcra = REDISMODULE_KEYTYPE_GCRA,
        /// A key type this crate does not know yet. Not a value Redis uses.
        Unknown = -1,
    }
    unknown Unknown
}

/// The end of a list to push to or pop from.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Where {
    ListHead = REDISMODULE_LIST_HEAD as i32,
    ListTail = REDISMODULE_LIST_TAIL as i32,
}

c_enum! {
    /// The type of a call reply, from `RedisModule_CallReplyType`.
    ReplyType {
        Unknown = REDISMODULE_REPLY_UNKNOWN,
        String = REDISMODULE_REPLY_STRING,
        Error = REDISMODULE_REPLY_ERROR,
        Integer = REDISMODULE_REPLY_INTEGER,
        Array = REDISMODULE_REPLY_ARRAY,
        Null = REDISMODULE_REPLY_NULL,
        Map = REDISMODULE_REPLY_MAP,
        Set = REDISMODULE_REPLY_SET,
        Bool = REDISMODULE_REPLY_BOOL,
        Double = REDISMODULE_REPLY_DOUBLE,
        BigNumber = REDISMODULE_REPLY_BIG_NUMBER,
        VerbatimString = REDISMODULE_REPLY_VERBATIM_STRING,
        Attribute = REDISMODULE_REPLY_ATTRIBUTE,
        Promise = REDISMODULE_REPLY_PROMISE,
    }
    unknown Unknown
}

/// When an aux RDB callback runs, relative to the keyspace.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Aux {
    Before = REDISMODULE_AUX_BEFORE_RDB as i32,
    After = REDISMODULE_AUX_AFTER_RDB as i32,
}

/// The result of most Redis module API calls.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Ok = REDISMODULE_OK as i32,
    Err = REDISMODULE_ERR as i32,
}

impl From<Status> for RedisResult<()> {
    fn from(value: Status) -> Self {
        match value {
            Status::Ok => Ok(()),
            Status::Err => Err(RedisError::Str(GENERIC_ERROR_MESSAGE)),
        }
    }
}

impl From<c_int> for Status {
    /// `REDISMODULE_OK` is `Ok`; any other value is `Err`.
    fn from(v: c_int) -> Self {
        if v == REDISMODULE_OK as c_int {
            Self::Ok
        } else {
            Self::Err
        }
    }
}

impl From<Status> for Result<(), &str> {
    fn from(s: Status) -> Self {
        match s {
            Status::Ok => Ok(()),
            Status::Err => Err(GENERIC_ERROR_MESSAGE),
        }
    }
}

bitflags! {
    #[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
    pub struct NotifyEvent : c_int {
        const KEYSPACE = REDISMODULE_NOTIFY_KEYSPACE;
        const KEYEVENT = REDISMODULE_NOTIFY_KEYEVENT;
        const GENERIC = REDISMODULE_NOTIFY_GENERIC;
        const STRING = REDISMODULE_NOTIFY_STRING;
        const LIST = REDISMODULE_NOTIFY_LIST;
        const SET = REDISMODULE_NOTIFY_SET;
        const HASH = REDISMODULE_NOTIFY_HASH;
        const ZSET = REDISMODULE_NOTIFY_ZSET;
        const EXPIRED = REDISMODULE_NOTIFY_EXPIRED;
        const EVICTED = REDISMODULE_NOTIFY_EVICTED;
        const STREAM = REDISMODULE_NOTIFY_STREAM;
        /// Available only starting from Redis `7.0.1`.
        const NEW = REDISMODULE_NOTIFY_NEW;
        const MODULE = REDISMODULE_NOTIFY_MODULE;
        const LOADED = REDISMODULE_NOTIFY_LOADED;
        const MISSED = REDISMODULE_NOTIFY_KEY_MISS;
        /// Does not include the [`Self::MISSED`] and [`Self::NEW`].
        ///
        /// Includes [`Self::GENERIC`], [`Self::STRING`], [`Self::LIST`],
        /// [`Self::SET`], [`Self::HASH`], [`Self::ZSET`], [`Self::EXPIRED`],
        /// [`Self::EVICTED`], [`Self::STREAM`], [`Self::MODULE`].
        const ALL = REDISMODULE_NOTIFY_ALL;
        const TRIMMED = REDISMODULE_NOTIFY_TRIMMED;
    }
}

#[derive(Debug)]
pub enum CommandFlag {
    Write,
    Readonly,
    Denyoom,
    Admin,
    Pubsub,
    Noscript,
    Random,
    SortForScript,
    Loading,
    Stale,
    SkipMonitor,
    Asking,
    Fast,
    Movablekeys,
}

// This is the one static function we need to initialize a module.
// bindgen does not generate it for us (probably since it's defined as static in redismodule.h).
#[allow(improper_ctypes)]
#[link(name = "redismodule", kind = "static")]
extern "C" {
    pub fn Export_RedisModule_Init(
        ctx: *mut RedisModuleCtx,
        module_name: *const c_char,
        module_version: c_int,
        api_version: c_int,
    ) -> c_int;

    pub fn Export_RedisModule_InitAPI(ctx: *mut RedisModuleCtx) -> c_void;
}

///////////////////////////////////////////////////////////////

pub const FMT: *const c_char = c"v".as_ptr().cast::<c_char>();

// REDISMODULE_HASH_DELETE is defined explicitly here because bindgen cannot
// parse typecasts in C macro constants yet.
// See https://github.com/rust-lang/rust-bindgen/issues/316
pub const REDISMODULE_HASH_DELETE: *const RedisModuleString =
    std::ptr::dangling::<RedisModuleString>();

// Helper functions for the raw bindings.

pub(crate) fn call_reply_type(reply: *mut RedisModuleCallReply) -> ReplyType {
    unsafe {
        // TODO: Cache the unwrapped functions and use them instead of unwrapping every time?
        crate::__required_api!(RedisModule_CallReplyType)(reply).into()
    }
}

pub(crate) fn free_call_reply(reply: *mut RedisModuleCallReply) {
    unsafe { crate::__required_api!(RedisModule_FreeCallReply)(reply) }
}

pub(crate) fn call_reply_integer(reply: *mut RedisModuleCallReply) -> c_longlong {
    unsafe { crate::__required_api!(RedisModule_CallReplyInteger)(reply) }
}

/// # Panics
///
/// Panics if the Redis server doesn't support replying with bool (since RESP3).
pub(crate) fn call_reply_bool(reply: *mut RedisModuleCallReply) -> bool {
    (unsafe { crate::__required_api!(RedisModule_CallReplyBool)(reply) } != 0)
}

/// # Panics
///
/// Panics if the Redis server doesn't support replying with bool (since RESP3).
pub(crate) fn call_reply_double(reply: *mut RedisModuleCallReply) -> f64 {
    unsafe { crate::__required_api!(RedisModule_CallReplyDouble)(reply) }
}

/// # Panics
///
/// Panics if the Redis server doesn't support replying with bool (since RESP3).
pub(crate) fn call_reply_big_number(reply: *mut RedisModuleCallReply) -> Option<String> {
    unsafe {
        let mut len: size_t = 0;
        let reply_string: *mut u8 =
            crate::__required_api!(RedisModule_CallReplyBigNumber)(reply, &mut len) as *mut u8;
        if reply_string.is_null() {
            return None;
        }
        String::from_utf8(slice::from_raw_parts(reply_string, len).to_vec()).ok()
    }
}

/// # Panics
///
/// Panics if the Redis server doesn't support replying with bool (since RESP3).
///
/// # Safety
///
/// `reply` must be a live call reply that `RedisModule_Call` returned.
pub unsafe fn call_reply_verbatim_string(
    reply: *mut RedisModuleCallReply,
) -> Option<(String, Vec<u8>)> {
    unsafe {
        let mut len: size_t = 0;
        let mut format: *const c_char = ptr::null();
        let reply_string =
            crate::__required_api!(RedisModule_CallReplyVerbatim)(reply, &mut len, &mut format)
                .cast::<u8>();
        if reply_string.is_null() || format.is_null() {
            return None;
        }
        Some((
            String::from_utf8(slice::from_raw_parts(format.cast::<u8>(), 3).to_vec()).ok()?,
            slice::from_raw_parts(reply_string, len).to_vec(),
        ))
    }
}

pub(crate) fn call_reply_array_element(
    reply: *mut RedisModuleCallReply,
    idx: usize,
) -> *mut RedisModuleCallReply {
    unsafe { crate::__required_api!(RedisModule_CallReplyArrayElement)(reply, idx) }
}

/// # Panics
///
/// Panics if the Redis server doesn't support replying with bool (since RESP3).
pub(crate) fn call_reply_set_element(
    reply: *mut RedisModuleCallReply,
    idx: usize,
) -> *mut RedisModuleCallReply {
    unsafe { crate::__required_api!(RedisModule_CallReplySetElement)(reply, idx) }
}

/// # Panics
///
/// Panics if the Redis server doesn't support replying with bool (since RESP3).
pub(crate) fn call_reply_map_element(
    reply: *mut RedisModuleCallReply,
    idx: usize,
) -> (*mut RedisModuleCallReply, *mut RedisModuleCallReply) {
    let mut key: *mut RedisModuleCallReply = ptr::null_mut();
    let mut val: *mut RedisModuleCallReply = ptr::null_mut();
    unsafe {
        crate::__required_api!(RedisModule_CallReplyMapElement)(reply, idx, &mut key, &mut val)
    };
    (key, val)
}

pub(crate) fn call_reply_length(reply: *mut RedisModuleCallReply) -> usize {
    unsafe { crate::__required_api!(RedisModule_CallReplyLength)(reply) }
}

#[inline]
pub(crate) fn close_key(kp: *mut RedisModuleKey) {
    unsafe { crate::__required_api!(RedisModule_CloseKey)(kp) }
}

#[inline]
pub(crate) fn open_key(
    ctx: *mut RedisModuleCtx,
    keyname: *mut RedisModuleString,
    mode: KeyMode,
) -> *mut RedisModuleKey {
    unsafe {
        crate::__required_api!(RedisModule_OpenKey)(ctx, keyname, mode.bits())
            .cast::<RedisModuleKey>()
    }
}

#[inline]
pub(crate) fn open_key_with_flags(
    ctx: *mut RedisModuleCtx,
    keyname: *mut RedisModuleString,
    mode: KeyMode,
    flags: c_int,
) -> *mut RedisModuleKey {
    unsafe {
        crate::__required_api!(RedisModule_OpenKey)(ctx, keyname, mode.bits() | flags)
            .cast::<RedisModuleKey>()
    }
}

#[inline]
pub(crate) fn reply_with_array(ctx: *mut RedisModuleCtx, len: c_long) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithArray)(ctx, len).into() }
}

#[inline]
pub(crate) fn reply_with_map(ctx: *mut RedisModuleCtx, len: c_long) -> Status {
    unsafe {
        RedisModule_ReplyWithMap
            .map_or_else(
                || crate::__required_api!(RedisModule_ReplyWithArray)(ctx, len * 2),
                |f| f(ctx, len),
            )
            .into()
    }
}

#[inline]
pub(crate) fn reply_with_set(ctx: *mut RedisModuleCtx, len: c_long) -> Status {
    unsafe {
        RedisModule_ReplyWithSet
            .map_or_else(
                || crate::__required_api!(RedisModule_ReplyWithArray)(ctx, len * 2),
                |f| f(ctx, len),
            )
            .into()
    }
}

#[inline]
pub(crate) fn reply_with_null(ctx: *mut RedisModuleCtx) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithNull)(ctx).into() }
}

#[inline]
pub(crate) fn reply_with_bool(ctx: *mut RedisModuleCtx, b: c_int) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithBool)(ctx, b).into() }
}

#[inline]
pub(crate) fn reply_with_long_long(ctx: *mut RedisModuleCtx, ll: c_longlong) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithLongLong)(ctx, ll).into() }
}

#[inline]
pub(crate) fn reply_with_double(ctx: *mut RedisModuleCtx, f: c_double) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithDouble)(ctx, f).into() }
}

#[inline]
pub(crate) fn reply_with_string(ctx: *mut RedisModuleCtx, s: *mut RedisModuleString) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithString)(ctx, s).into() }
}

#[inline]
pub(crate) fn reply_with_simple_string(ctx: *mut RedisModuleCtx, s: *const c_char) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithSimpleString)(ctx, s).into() }
}

#[inline]
pub(crate) fn reply_with_string_buffer(
    ctx: *mut RedisModuleCtx,
    s: *const c_char,
    len: size_t,
) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithStringBuffer)(ctx, s, len).into() }
}

#[inline]
pub(crate) fn reply_with_big_number(
    ctx: *mut RedisModuleCtx,
    s: *const c_char,
    len: size_t,
) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplyWithBigNumber)(ctx, s, len).into() }
}

#[inline]
pub(crate) fn reply_with_verbatim_string(
    ctx: *mut RedisModuleCtx,
    s: *const c_char,
    len: size_t,
    format: *const c_char,
) -> Status {
    unsafe {
        crate::__required_api!(RedisModule_ReplyWithVerbatimStringType)(ctx, s, len, format).into()
    }
}

// Sets the expiry on a key.
//
// Expire is in milliseconds.
#[inline]
pub(crate) fn set_expire(key: *mut RedisModuleKey, expire: c_longlong) -> Status {
    unsafe { crate::__required_api!(RedisModule_SetExpire)(key, expire).into() }
}

#[inline]
pub(crate) fn string_dma(key: *mut RedisModuleKey, len: *mut size_t, mode: KeyMode) -> *mut c_char {
    unsafe { crate::__required_api!(RedisModule_StringDMA)(key, len, mode.bits()) }
}

#[inline]
pub(crate) fn string_truncate(key: *mut RedisModuleKey, new_len: size_t) -> Status {
    unsafe { crate::__required_api!(RedisModule_StringTruncate)(key, new_len).into() }
}

pub(crate) fn hash_get_multi<T>(
    key: *mut RedisModuleKey,
    fields: &[T],
    values: &mut [*mut RedisModuleString],
) -> Result<(), RedisError>
where
    T: Into<Vec<u8>> + Clone,
{
    assert_eq!(fields.len(), values.len());

    let fields = fields
        .iter()
        .map(|e| CString::new(e.clone()))
        .collect::<Result<Vec<CString>, _>>()?;

    let mut fi = fields.iter();
    let mut vi = values.iter_mut();

    macro_rules! rm {
        () => { unsafe {
            crate::__available_api!(RedisModule_HashGet)(key, REDISMODULE_HASH_CFIELDS as i32,
                                         ptr::null::<c_char>())
        }};
        ($($args:expr)*) => { unsafe {
            crate::__available_api!(RedisModule_HashGet)(
                key, REDISMODULE_HASH_CFIELDS as i32,
                $($args),*,
                ptr::null::<c_char>()
            )
        }};
    }
    macro_rules! f {
        () => {
            fi.next().unwrap().as_ptr()
        };
    }
    macro_rules! v {
        () => {
            vi.next().unwrap()
        };
    }

    // This convoluted code is necessary since Redis only exposes a varargs API for HashGet
    // to modules. Unfortunately there's no straightforward or portable way of calling a
    // a varargs function with a variable number of arguments that is determined at runtime.
    // See also the following Redis ticket: https://github.com/redis/redis/issues/7860
    let res = Status::from(match fields.len() {
        0 => rm! {},
        1 => rm! {f!() v!()},
        2 => rm! {f!() v!() f!() v!()},
        3 => rm! {f!() v!() f!() v!() f!() v!()},
        4 => rm! {f!() v!() f!() v!() f!() v!() f!() v!()},
        5 => rm! {f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()},
        6 => rm! {f!() v!() f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()},
        7 => rm! {
            f!() v!() f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()
            f!() v!()
        },
        8 => rm! {
            f!() v!() f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()
            f!() v!() f!() v!()
        },
        9 => rm! {
            f!() v!() f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()
            f!() v!() f!() v!() f!() v!()
        },
        10 => rm! {
            f!() v!() f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()
            f!() v!() f!() v!() f!() v!() f!() v!()
        },
        11 => rm! {
            f!() v!() f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()
            f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()
        },
        12 => rm! {
            f!() v!() f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()
            f!() v!() f!() v!() f!() v!() f!() v!() f!() v!() f!() v!()
        },
        _ => panic!("Unsupported length"),
    });

    match res {
        Status::Ok => Ok(()),
        Status::Err => Err(RedisError::WrongType),
    }
}

/// `HGET` with the field given as an existing `RedisModuleString`.
///
/// [`hash_get_multi`] passes fields as C strings (`REDISMODULE_HASH_CFIELDS`), which costs
/// a `CString` here and a transient string object per field inside Redis. With a
/// `RedisModuleString` Redis reads the field in place, so a caller that fetches the same
/// field repeatedly can build the string once and reuse it.
///
/// Returns a null pointer when the hash has no such field. A non-null result is an owned
/// string the caller must release with `RedisModule_FreeString`.
///
/// Returns [`RedisError::WrongType`] when the key holds a value that is not a hash.
#[inline]
pub(crate) fn hash_get_by_string(
    key: *mut RedisModuleKey,
    field: *mut RedisModuleString,
) -> Result<*mut RedisModuleString, RedisError> {
    let mut value: *mut RedisModuleString = ptr::null_mut();
    let res = Status::from(unsafe {
        crate::__available_api!(RedisModule_HashGet)(
            key,
            REDISMODULE_HASH_NONE as i32,
            field,
            &mut value,
            ptr::null::<c_char>(),
        )
    });
    match res {
        Status::Ok => Ok(value),
        Status::Err => Err(RedisError::WrongType),
    }
}

/// Checks that `key` is empty or holds a hash, before `RedisModule_HashSet`.
fn verify_hash_key(key: *mut RedisModuleKey) -> Result<(), RedisError> {
    let key_type = unsafe { crate::__available_api!(RedisModule_KeyType)(key) };
    if key_type == REDISMODULE_KEYTYPE_EMPTY as c_int
        || key_type == REDISMODULE_KEYTYPE_HASH as c_int
    {
        Ok(())
    } else {
        Err(RedisError::WrongType)
    }
}

fn hash_field(field: &str) -> Result<CString, RedisError> {
    CString::new(field).map_err(|_| {
        RedisError::Str("ERR the hash field contains a NUL byte; pass it as a RedisString")
    })
}

/// The count `RedisModule_HashSet` returns for one field: the existing fields
/// it replaced or deleted.
fn hash_count(count: c_int) -> Result<usize, RedisError> {
    usize::try_from(count)
        .map_err(|_| RedisError::String(format!("ERR unexpected HashSet result {count}")))
}

/// Sets `field` to `value` in the hash at `key`, which must be open for
/// writing. Returns the number of existing fields replaced: 0 for a new field,
/// 1 for a replaced one.
///
/// # Errors
///
/// Returns [`RedisError::WrongType`] if the key holds another type, and an error
/// if `field` contains a NUL byte or the running Redis lacks the API.
pub(crate) fn hash_set(
    key: *mut RedisModuleKey,
    field: &str,
    value: *mut RedisModuleString,
) -> Result<usize, RedisError> {
    verify_hash_key(key)?;
    let field = hash_field(field)?;
    let hash_set = unsafe { crate::__available_api!(RedisModule_HashSet) };
    hash_count(unsafe {
        hash_set(
            key,
            REDISMODULE_HASH_CFIELDS as c_int,
            field.as_ptr(),
            value,
            ptr::null::<c_char>(),
        )
    })
}

/// Like [`hash_set`], with the field as a Redis string, so it can hold any bytes.
///
/// # Errors
///
/// Returns [`RedisError::WrongType`] if the key holds another type, and an error
/// if the running Redis lacks the API.
pub(crate) fn hash_set_by_string(
    key: *mut RedisModuleKey,
    field: *mut RedisModuleString,
    value: *mut RedisModuleString,
) -> Result<usize, RedisError> {
    verify_hash_key(key)?;
    let hash_set = unsafe { crate::__available_api!(RedisModule_HashSet) };
    hash_count(unsafe {
        hash_set(
            key,
            REDISMODULE_HASH_NONE as c_int,
            field,
            value,
            ptr::null::<RedisModuleString>(),
        )
    })
}

/// Deletes `field` from the hash at `key`, which must be open for writing.
/// Returns the number of fields deleted: 0 or 1.
///
/// # Errors
///
/// Returns [`RedisError::WrongType`] if the key holds another type, and an error
/// if `field` contains a NUL byte or the running Redis lacks the API.
pub(crate) fn hash_del(key: *mut RedisModuleKey, field: &str) -> Result<usize, RedisError> {
    verify_hash_key(key)?;
    let field = hash_field(field)?;
    let hash_set = unsafe { crate::__available_api!(RedisModule_HashSet) };
    hash_count(unsafe {
        hash_set(
            key,
            REDISMODULE_HASH_CFIELDS as c_int,
            field.as_ptr(),
            REDISMODULE_HASH_DELETE,
            ptr::null::<c_char>(),
        )
    })
}

/// Like [`hash_del`], with the field as a Redis string, so it can hold any bytes.
///
/// # Errors
///
/// Returns [`RedisError::WrongType`] if the key holds another type, and an error
/// if the running Redis lacks the API.
pub(crate) fn hash_del_by_string(
    key: *mut RedisModuleKey,
    field: *mut RedisModuleString,
) -> Result<usize, RedisError> {
    verify_hash_key(key)?;
    let hash_set = unsafe { crate::__available_api!(RedisModule_HashSet) };
    hash_count(unsafe {
        hash_set(
            key,
            REDISMODULE_HASH_NONE as c_int,
            field,
            REDISMODULE_HASH_DELETE,
            ptr::null::<RedisModuleString>(),
        )
    })
}

#[inline]
pub(crate) fn list_push(
    key: *mut RedisModuleKey,
    list_where: Where,
    element: *mut RedisModuleString,
) -> Status {
    unsafe { crate::__required_api!(RedisModule_ListPush)(key, list_where as i32, element).into() }
}

#[inline]
pub(crate) fn list_pop(key: *mut RedisModuleKey, list_where: Where) -> *mut RedisModuleString {
    unsafe { crate::__required_api!(RedisModule_ListPop)(key, list_where as i32) }
}

// Returns pointer to the C string, and sets len to its length
#[inline]
pub(crate) fn string_ptr_len(s: *const RedisModuleString, len: *mut size_t) -> *const c_char {
    unsafe { crate::__required_api!(RedisModule_StringPtrLen)(s, len) }
}

#[inline]
pub(crate) fn string_retain_string(ctx: *mut RedisModuleCtx, s: *mut RedisModuleString) {
    unsafe { crate::__required_api!(RedisModule_RetainString)(ctx, s) }
}

#[inline]
pub(crate) fn string_to_longlong(s: *const RedisModuleString, len: *mut i64) -> Status {
    unsafe { crate::__required_api!(RedisModule_StringToLongLong)(s, len).into() }
}

#[inline]
pub(crate) fn string_to_double(s: *const RedisModuleString, len: *mut f64) -> Status {
    unsafe { crate::__required_api!(RedisModule_StringToDouble)(s, len).into() }
}

#[inline]
pub(crate) fn string_set(key: *mut RedisModuleKey, s: *mut RedisModuleString) -> Status {
    unsafe { crate::__required_api!(RedisModule_StringSet)(key, s).into() }
}

#[inline]
pub(crate) fn replicate_verbatim(ctx: *mut RedisModuleCtx) -> Status {
    unsafe { crate::__required_api!(RedisModule_ReplicateVerbatim)(ctx).into() }
}

/// # Safety
///
/// `rdb` must be the IO of the running RDB callback, as for [`is_io_error`].
unsafe fn load<F, T>(rdb: *mut RedisModuleIO, f: F) -> Result<T, Error>
where
    F: FnOnce(*mut RedisModuleIO) -> T,
{
    let res = f(rdb);
    if unsafe { is_io_error(rdb) } {
        Err(RedisError::short_read().into())
    } else {
        Ok(res)
    }
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_load` callback (or an `aux_load` callback), used only during that
/// callback.
pub unsafe fn load_unsigned(rdb: *mut RedisModuleIO) -> Result<u64, Error> {
    unsafe {
        let api = crate::__available_api!(RedisModule_LoadUnsigned);
        load(rdb, |rdb| api(rdb))
    }
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_load` callback (or an `aux_load` callback), used only during that
/// callback.
pub unsafe fn load_signed(rdb: *mut RedisModuleIO) -> Result<i64, Error> {
    unsafe {
        let api = crate::__available_api!(RedisModule_LoadSigned);
        load(rdb, |rdb| api(rdb))
    }
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_load` callback (or an `aux_load` callback), used only during that
/// callback.
pub unsafe fn load_string(rdb: *mut RedisModuleIO) -> Result<RedisString, Error> {
    let p = unsafe {
        let api = crate::__available_api!(RedisModule_LoadString);
        load(rdb, |rdb| api(rdb))?
    };
    // SAFETY: LoadString returns one manually released native owner.
    Ok(unsafe { RedisString::from_redis_module_string(ptr::null_mut(), p) })
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_load` callback (or an `aux_load` callback), used only during that
/// callback.
pub unsafe fn load_string_buffer(rdb: *mut RedisModuleIO) -> Result<RedisBuffer, Error> {
    unsafe {
        let mut len = 0;
        let api = crate::__available_api!(RedisModule_LoadStringBuffer);
        let buffer = load(rdb, |rdb| api(rdb, &mut len))?;
        Ok(RedisBuffer::new(buffer, len))
    }
}

pub(crate) fn replicate<'a, T: Into<StrCallArgs<'a>>>(
    ctx: *mut RedisModuleCtx,
    command: &str,
    args: T,
) -> Status {
    let mut call_args: StrCallArgs = args.into();
    let final_args = call_args.args_mut();

    let cmd = CString::new(command).unwrap();

    unsafe {
        crate::__required_api!(RedisModule_Replicate)(
            ctx,
            cmd.as_ptr(),
            FMT,
            final_args.as_ptr(),
            final_args.len(),
        )
        .into()
    }
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_load` callback (or an `aux_load` callback), used only during that
/// callback.
pub unsafe fn load_double(rdb: *mut RedisModuleIO) -> Result<f64, Error> {
    unsafe {
        let api = crate::__available_api!(RedisModule_LoadDouble);
        load(rdb, |rdb| api(rdb))
    }
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_load` callback (or an `aux_load` callback), used only during that
/// callback.
pub unsafe fn load_float(rdb: *mut RedisModuleIO) -> Result<f32, Error> {
    unsafe {
        let api = crate::__available_api!(RedisModule_LoadFloat);
        load(rdb, |rdb| api(rdb))
    }
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_save` callback (or an `aux_save` callback), used only during that
/// callback.
pub unsafe fn save_string(rdb: *mut RedisModuleIO, buf: &str) {
    unsafe {
        crate::__required_api!(RedisModule_SaveStringBuffer)(
            rdb,
            buf.as_ptr().cast::<c_char>(),
            buf.len(),
        )
    };
}

/// Save the `RedisString` into the RDB
///
/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_save` callback (or an `aux_save` callback), used only during that
/// callback.
pub unsafe fn save_redis_string(rdb: *mut RedisModuleIO, s: &RedisString) {
    unsafe { crate::__required_api!(RedisModule_SaveString)(rdb, s.as_ptr()) };
}

/// Save the `&[u8]` into the RDB
///
/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_save` callback (or an `aux_save` callback), used only during that
/// callback.
pub unsafe fn save_slice(rdb: *mut RedisModuleIO, buf: &[u8]) {
    unsafe {
        crate::__required_api!(RedisModule_SaveStringBuffer)(
            rdb,
            buf.as_ptr().cast::<c_char>(),
            buf.len(),
        )
    };
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_save` callback (or an `aux_save` callback), used only during that
/// callback.
pub unsafe fn save_double(rdb: *mut RedisModuleIO, val: f64) {
    unsafe { crate::__required_api!(RedisModule_SaveDouble)(rdb, val) };
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_save` callback (or an `aux_save` callback), used only during that
/// callback.
pub unsafe fn save_signed(rdb: *mut RedisModuleIO, val: i64) {
    unsafe { crate::__required_api!(RedisModule_SaveSigned)(rdb, val) };
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_save` callback (or an `aux_save` callback), used only during that
/// callback.
pub unsafe fn save_float(rdb: *mut RedisModuleIO, val: f32) {
    unsafe { crate::__required_api!(RedisModule_SaveFloat)(rdb, val) };
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_save` callback (or an `aux_save` callback), used only during that
/// callback.
pub unsafe fn save_unsigned(rdb: *mut RedisModuleIO, val: u64) {
    unsafe { crate::__required_api!(RedisModule_SaveUnsigned)(rdb, val) };
}

pub(crate) fn string_compare(a: *mut RedisModuleString, b: *mut RedisModuleString) -> Ordering {
    unsafe { crate::__required_api!(RedisModule_StringCompare)(a, b).cmp(&0) }
}

pub(crate) fn string_append_buffer(
    ctx: *mut RedisModuleCtx,
    s: *mut RedisModuleString,
    buff: &str,
) -> Status {
    unsafe {
        crate::__required_api!(RedisModule_StringAppendBuffer)(
            ctx,
            s,
            buff.as_ptr().cast::<c_char>(),
            buff.len(),
        )
        .into()
    }
}

/// # Safety
///
/// `ctx` must be the context Redis passed to the module's load.
#[doc(hidden)]
pub unsafe fn register_info_function(
    ctx: *mut RedisModuleCtx,
    callback: RedisModuleInfoFunc,
) -> Status {
    unsafe { crate::__required_api!(RedisModule_RegisterInfoFunc)(ctx, callback).into() }
}

pub(crate) fn add_info_section(ctx: *mut RedisModuleInfoCtx, name: Option<&str>) -> Status {
    name.map(|n| CString::new(n).unwrap()).map_or_else(
        || unsafe {
            crate::__required_api!(RedisModule_InfoAddSection)(ctx, ptr::null_mut()).into()
        },
        |n| unsafe { crate::__required_api!(RedisModule_InfoAddSection)(ctx, n.as_ptr()).into() },
    )
}

pub(crate) fn add_info_field_str(
    ctx: *mut RedisModuleInfoCtx,
    name: &str,
    content: &str,
) -> Status {
    let name = CString::new(name).unwrap();
    let content = RedisString::create(None, content);
    unsafe {
        crate::__required_api!(RedisModule_InfoAddFieldString)(ctx, name.as_ptr(), content.as_ptr())
            .into()
    }
}

pub(crate) fn add_info_field_long_long(
    ctx: *mut RedisModuleInfoCtx,
    name: &str,
    value: c_longlong,
) -> Status {
    let name = CString::new(name).unwrap();
    unsafe {
        crate::__required_api!(RedisModule_InfoAddFieldLongLong)(ctx, name.as_ptr(), value).into()
    }
}

pub(crate) fn add_info_field_unsigned_long_long(
    ctx: *mut RedisModuleInfoCtx,
    name: &str,
    value: c_ulonglong,
) -> Status {
    let name = CString::new(name).unwrap();
    unsafe {
        crate::__required_api!(RedisModule_InfoAddFieldULongLong)(ctx, name.as_ptr(), value).into()
    }
}

pub(crate) fn add_info_field_double(
    ctx: *mut RedisModuleInfoCtx,
    name: &str,
    value: c_double,
) -> Status {
    let name = CString::new(name).unwrap();
    unsafe {
        crate::__required_api!(RedisModule_InfoAddFieldDouble)(ctx, name.as_ptr(), value).into()
    }
}

pub(crate) fn add_info_begin_dict_field(ctx: *mut RedisModuleInfoCtx, name: &str) -> Status {
    let name = CString::new(name).unwrap();
    unsafe { crate::__required_api!(RedisModule_InfoBeginDictField)(ctx, name.as_ptr()).into() }
}

pub(crate) fn add_info_end_dict_field(ctx: *mut RedisModuleInfoCtx) -> Status {
    unsafe { crate::__required_api!(RedisModule_InfoEndDictField)(ctx).into() }
}

/// Exports `func` under `name` for other modules to look up with
/// `RedisModule_GetSharedAPI`. Returns [`Status::Err`] when the name is taken.
///
/// # Safety
///
/// - `ctx` must be a context Redis passed to the module, used on a thread
///   that holds the Redis lock.
/// - `name` must be a NUL-terminated string that stays valid for as long as
///   the server runs: Redis keeps the pointer, not a copy.
/// - `func` must stay valid for as long as other modules may call it.
///
/// # Panics
///
/// Panics when the [RedisModule_ExportSharedAPI] is unavailable.
pub unsafe fn export_shared_api(
    ctx: *mut RedisModuleCtx,
    func: *const ::std::os::raw::c_void,
    name: *const ::std::os::raw::c_char,
) -> Status {
    unsafe {
        crate::__required_api!(RedisModule_ExportSharedAPI)(
            ctx,
            name,
            func as *mut ::std::os::raw::c_void,
        )
        .into()
    }
}

/// Fires a keyspace notification for `keyname`.
///
/// # Safety
///
/// `ctx` must be a context Redis passed to the module, used on a thread that
/// holds the Redis lock.
///
/// # Panics
///
/// Panics when the [RedisModule_NotifyKeyspaceEvent] is unavailable.
pub unsafe fn notify_keyspace_event(
    ctx: *mut RedisModuleCtx,
    event_type: NotifyEvent,
    event: &str,
    keyname: &RedisString,
) -> Status {
    let event = CString::new(event).unwrap();
    crate::__required_api!(RedisModule_NotifyKeyspaceEvent)(
        ctx,
        event_type.bits(),
        event.as_ptr(),
        keyname.as_ptr(),
    )
    .into()
}

/// # Panics
///
/// Panics when the [RedisModule_GetNotifyKeyspaceEvents] is unavailable.
#[must_use]
pub fn get_keyspace_events() -> NotifyEvent {
    unsafe {
        let events = crate::__required_api!(RedisModule_GetNotifyKeyspaceEvents)();
        NotifyEvent::from_bits_truncate(events)
    }
}

/// Returns all the available notification flags for key-space
/// notifications.
///
/// # Panics
///
/// Panics when the [RedisModule_GetKeyspaceNotificationFlagsAll] is
/// unavailable.
pub fn get_keyspace_notification_flags_all() -> NotifyEvent {
    unsafe {
        NotifyEvent::from_bits_truncate(crate::__required_api!(
            RedisModule_GetKeyspaceNotificationFlagsAll
        )())
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: i32,
    pub minor: i32,
    pub patch: i32,
}

impl From<c_int> for Version {
    fn from(ver: c_int) -> Self {
        // Expected format: 0x00MMmmpp for Major, minor, patch
        Self {
            major: (ver & 0x00FF_0000) >> 16,
            minor: (ver & 0x0000_FF00) >> 8,
            patch: (ver & 0x0000_00FF),
        }
    }
}

/// # Safety
///
/// `rdb` must be the `RedisModuleIO` pointer Redis passed to the running
/// `rdb_save` or `rdb_load` callback (or the IO of an aux callback), used only
/// during that callback.
pub unsafe fn is_io_error(rdb: *mut RedisModuleIO) -> bool {
    unsafe { crate::__required_api!(RedisModule_IsIOError)(rdb) != 0 }
}

/// Logs `msg` at notice level.
/// Embedded NUL bytes are written as `\0`, which reads the same as a literal
/// backslash and zero; other text is passed literally.
///
/// # Safety
///
/// `ctx` must be null or a context Redis passed to the module, valid for this call.
pub unsafe fn redis_log(ctx: *mut RedisModuleCtx, msg: &str) {
    crate::logging::log_internal(ctx, crate::logging::RedisLogLevel::Notice, msg);
}

#[cfg(test)]
mod api_availability_tests {
    use super::*;

    // Outside a Redis server every module API pointer is None.

    #[test]
    fn a_missing_api_in_a_fallible_wrapper_returns_an_error() {
        let error = hash_get_by_string(ptr::null_mut(), ptr::null_mut()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "ERR RedisModule_HashGet is not available in this Redis server"
        );

        // crate::error::Error displays with its own prefix.
        // SAFETY: the API is missing outside Redis, so the pointer is not used.
        let error = unsafe { load_unsigned(ptr::null_mut()) }.unwrap_err();
        assert!(
            error
                .to_string()
                .ends_with("ERR RedisModule_LoadUnsigned is not available in this Redis server"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_api_elsewhere_panics_with_its_name() {
        let payload = std::panic::catch_unwind(|| RedisString::create(None, "text")).unwrap_err();
        assert_eq!(
            payload.downcast_ref::<String>().map(String::as_str),
            Some("RedisModule_CreateString is not available in this Redis server")
        );
    }
}

#[cfg(test)]
mod hash_tests {
    use super::{hash_count, hash_field};

    #[test]
    fn a_nul_in_a_text_field_is_an_error() {
        let error = hash_field("a\0b").unwrap_err();
        assert!(error.to_string().contains("NUL"), "{error}");
        assert_eq!(hash_field("field").unwrap().to_bytes(), b"field");
    }

    #[test]
    fn hash_counts_are_not_negative() {
        assert_eq!(hash_count(0).unwrap(), 0);
        assert_eq!(hash_count(1).unwrap(), 1);
        assert!(hash_count(-1).is_err());
    }
}

#[cfg(test)]
mod c_enum_tests {
    use super::*;

    #[test]
    fn unknown_values_map_to_unknown_instead_of_panicking() {
        assert_eq!(
            KeyType::from(REDISMODULE_KEYTYPE_HASH as c_int),
            KeyType::Hash
        );
        assert_eq!(
            KeyType::from(REDISMODULE_KEYTYPE_GCRA as c_int),
            KeyType::Gcra
        );
        assert_eq!(KeyType::from(99), KeyType::Unknown);
        assert_eq!(
            ReplyType::from(REDISMODULE_REPLY_ATTRIBUTE as c_int),
            ReplyType::Attribute
        );
        assert_eq!(ReplyType::from(99), ReplyType::Unknown);
        assert_eq!(Status::from(REDISMODULE_OK as c_int), Status::Ok);
        assert_eq!(Status::from(7), Status::Err);
    }
}
