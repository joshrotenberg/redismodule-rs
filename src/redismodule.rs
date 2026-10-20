use std::borrow::Borrow;
use std::convert::TryFrom;
use std::fmt::Display;
use std::ops::Deref;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr::NonNull;
use std::slice;
use std::str;
use std::str::Utf8Error;
use std::string::FromUtf8Error;
use std::{fmt, ptr};

use serde::de::{Error, SeqAccess};

use crate::Context;
pub use crate::raw;
pub use crate::rediserror::RedisError;
pub use crate::redisvalue::RedisValue;

/// A short-hand type that stores a [std::result::Result] with custom
/// type and [RedisError].
pub type RedisResult<T = RedisValue> = Result<T, RedisError>;
/// A [RedisResult] with [RedisValue].
pub type RedisValueResult = RedisResult<RedisValue>;

impl From<RedisValue> for RedisValueResult {
    fn from(v: RedisValue) -> Self {
        Ok(v)
    }
}

impl From<RedisError> for RedisValueResult {
    fn from(v: RedisError) -> Self {
        Err(v)
    }
}

pub const REDIS_OK: RedisValueResult = Ok(RedisValue::SimpleStringStatic("OK"));
pub const TYPE_METHOD_VERSION: u64 = raw::REDISMODULE_TYPE_METHOD_VERSION as u64;

/// Parse a consuming iterator of owned arguments.
///
/// Use [`crate::ArgCursor`] for borrowed text, or bind [`Self::next_arg`] to a
/// local owner before calling [`RedisString::try_as_str`]. `next_string` retains
/// its existing lossy conversion; it is not a strict-text replacement.
///
/// The consuming borrowed-text method has been removed:
///
/// ```compile_fail,E0599
/// use redis_module::{NextArg, RedisString};
/// fn text(argv: Vec<RedisString>) {
///     let _ = argv.into_iter().next_str();
/// }
/// ```
///
/// ```
/// use redis_module::{NextArg, RedisError, RedisString};
/// fn owned_text(argv: Vec<RedisString>) -> Result<String, RedisError> {
///     let owner = argv.into_iter().next_arg()?;
///     Ok(owner.try_as_str()?.to_owned())
/// }
/// ```
pub trait NextArg {
    fn next_arg(&mut self) -> Result<RedisString, RedisError>;
    fn next_string(&mut self) -> Result<String, RedisError>;
    fn next_i64(&mut self) -> Result<i64, RedisError>;
    fn next_u64(&mut self) -> Result<u64, RedisError>;
    fn next_f64(&mut self) -> Result<f64, RedisError>;
    fn done(&mut self) -> Result<(), RedisError>;
}

impl<T> NextArg for T
where
    T: Iterator<Item = RedisString>,
{
    #[inline]
    fn next_arg(&mut self) -> Result<RedisString, RedisError> {
        self.next().ok_or(RedisError::WrongArity)
    }

    #[inline]
    fn next_string(&mut self) -> Result<String, RedisError> {
        self.next()
            .map_or(Err(RedisError::WrongArity), |v| Ok(v.to_string_lossy()))
    }

    #[inline]
    fn next_i64(&mut self) -> Result<i64, RedisError> {
        self.next()
            .map_or(Err(RedisError::WrongArity), |v| v.parse_integer())
    }

    #[inline]
    fn next_u64(&mut self) -> Result<u64, RedisError> {
        self.next()
            .map_or(Err(RedisError::WrongArity), |v| v.parse_unsigned_integer())
    }

    #[inline]
    fn next_f64(&mut self) -> Result<f64, RedisError> {
        self.next()
            .map_or(Err(RedisError::WrongArity), |v| v.parse_float())
    }

    /// Return an error if there are any more arguments
    #[inline]
    fn done(&mut self) -> Result<(), RedisError> {
        self.next().map_or(Ok(()), |_| Err(RedisError::WrongArity))
    }
}

/// Decode borrowed callback arguments into independently retained owners.
///
/// # Safety
/// Redis APIs must be initialized and native access/retention legal. For a
/// positive `argc` and non-null `argv`, `argv` must point to that many valid,
/// non-null, retainable native strings for this call. A null `argv` or a count
/// of zero or less yields no arguments. The callback keeps its own references.
pub unsafe fn decode_args(
    _ctx: *mut raw::RedisModuleCtx,
    argv: *mut *mut raw::RedisModuleString,
    argc: c_int,
) -> Vec<RedisString> {
    if argc <= 0 || argv.is_null() {
        return Vec::new();
    }
    // SAFETY: the callback supplies a valid argv/count and retainable strings.
    unsafe { slice::from_raw_parts(argv, argc as usize) }
        .iter()
        .map(|&arg| unsafe {
            RedisString::retain_from_ptr(NonNull::new(arg).expect("non-null callback argument"))
        })
        .collect()
}

///////////////////////////////////////////////////

/// One independently managed native string reference, with no cached context.
///
/// Creation, access, cloning and Drop require initialized Redis APIs and a legal
/// native access phase. Native references are not atomic. This owner does not
/// establish GIL, thread, context or alias/buffer-stability guarantees.
#[derive(Debug)]
pub struct RedisString {
    inner: NonNull<raw::RedisModuleString>,
}

impl RedisString {
    /// Borrow the native object pointer. This neither transfers ownership nor
    /// authorizes native access/mutation. Keep this owner alive while using it.
    /// The object pointer is distinct from a possibly reallocated byte buffer.
    pub fn as_ptr(&self) -> *mut raw::RedisModuleString {
        self.inner.as_ptr()
    }

    /// The memory Redis uses for this string, its object and its buffer,
    /// through `RedisModule_MallocSizeString`.
    ///
    /// # Errors
    ///
    /// Returns an error if the running Redis does not provide the API.
    pub fn malloc_size(&self) -> Result<usize, RedisError> {
        let malloc_size = unsafe { crate::__available_api!(RedisModule_MallocSizeString) };
        Ok(unsafe { malloc_size(self.as_ptr()) })
    }

    /// Suppress Drop for exactly one reference. StrCallArgs releases it in its
    /// own Drop; Redis releases configuration error strings. Do not use this for
    /// a string configuration getter's reply: Redis copies that string without
    /// releasing it, so the configuration keeps the owner instead.
    pub(crate) fn take(self) -> *mut raw::RedisModuleString {
        std::mem::ManuallyDrop::new(self).as_ptr()
    }

    /// Adopt a transferred native reference and resolve its AutoMemory entry.
    /// With a producer context, acquire one manual reference, then release the
    /// producer's reference/registration immediately. Null-context ownership
    /// is adopted directly without an extra retain.
    ///
    /// # Safety
    /// APIs must be initialized, `owned` must be a live native string with one
    /// transferred release responsibility, and native access/retention legal.
    /// `producer_ctx` must be its actual, currently valid producer context if
    /// it can have an AutoMemory entry; non-null does not imply AM is enabled.
    /// No other owner may release the transferred reference. Static or otherwise
    /// non-retainable strings cannot use the context detachment recipe.
    pub unsafe fn from_owned_ptr(
        producer_ctx: Option<NonNull<raw::RedisModuleCtx>>,
        owned: NonNull<raw::RedisModuleString>,
    ) -> Self {
        if let Some(ctx) = producer_ctx {
            raw::string_retain_string(ptr::null_mut(), owned.as_ptr());
            unsafe { crate::__required_api!(RedisModule_FreeString)(ctx.as_ptr(), owned.as_ptr()) };
        }
        Self { inner: owned }
    }

    /// Acquire one manual reference without taking the native caller's reference.
    ///
    /// # Safety
    /// APIs must be initialized, the string live and retainable, and access legal
    /// under Redis's native thread/callback rules. Retention is not a byte copy
    /// or a guarantee against buffer invalidation through other native aliases.
    pub unsafe fn retain_from_ptr(borrowed: NonNull<raw::RedisModuleString>) -> Self {
        raw::string_retain_string(ptr::null_mut(), borrowed.as_ptr());
        Self { inner: borrowed }
    }

    pub(crate) unsafe fn from_owned_ptr_or_none(
        producer_ctx: Option<NonNull<raw::RedisModuleCtx>>,
        owned: *mut raw::RedisModuleString,
    ) -> Option<Self> {
        NonNull::new(owned).map(|p| unsafe { Self::from_owned_ptr(producer_ctx, p) })
    }

    /// Legacy detach-or-increment acquisition shim; prefer the explicit recipes.
    ///
    /// # Safety
    /// `inner` must be live, non-null and retainable, APIs initialized and native
    /// access legal. A supplied context must be valid and the actual AM context.
    /// RetainString detaches an existing entry or increments if none exists;
    /// this is not the owned-result adoption recipe.
    pub unsafe fn new(
        ctx: Option<NonNull<raw::RedisModuleCtx>>,
        inner: *mut raw::RedisModuleString,
    ) -> Self {
        let inner = NonNull::new(inner).expect("non-null native string");
        raw::string_retain_string(ctx.map_or(ptr::null_mut(), |v| v.as_ptr()), inner.as_ptr());
        Self { inner }
    }

    /// Retain a native alias, without a byte copy. Native access must be legal;
    /// the context argument is advisory, not an unforgeable GIL/phase token.
    /// Command argv may be buffer-trimmed at command return: retain the owner,
    /// then derive a fresh view in the later legal phase. Do not carry an old
    /// byte pointer/view across that boundary. No thread sharing is certified.
    pub fn safe_clone(&self, _ctx: &Context) -> Self {
        unsafe { Self::retain_from_ptr(self.inner) }
    }

    /// Compatibility constructor. The context no longer controls lifetime or
    /// AutoMemory; the independent string is created with a null context. The
    /// bytes are copied as they are, interior NUL bytes included, like
    /// [`Self::from_bytes`]. Requires initialized APIs and legal native access.
    pub fn create<T: Into<Vec<u8>>>(_ctx: Option<NonNull<raw::RedisModuleCtx>>, s: T) -> Self {
        Self::from_bytes(&s.into())
    }

    /// Copy a byte slice into an independent native string. No text conversion.
    /// Requires initialized APIs and legal native access, including at Drop.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        unsafe { Self::from_raw_parts(None, bytes.as_ptr().cast(), bytes.len()) }
    }

    /// Copy raw bytes into an independent native string. Context is retained only
    /// for source compatibility; creation uses a null context.
    ///
    /// # Safety
    /// Redis APIs must be initialized and native access legal. `s` must point to
    /// a readable region of at least `len` bytes for this call.
    pub unsafe fn from_raw_parts(
        _ctx: Option<NonNull<raw::RedisModuleCtx>>,
        s: *const c_char,
        len: libc::size_t,
    ) -> Self {
        let inner =
            unsafe { crate::__required_api!(RedisModule_CreateString)(ptr::null_mut(), s, len) };
        Self {
            inner: NonNull::new(inner).expect("CreateString returned null"),
        }
    }

    /// Compatibility binary constructor; context does not govern ownership.
    pub fn create_from_slice(_ctx: *mut raw::RedisModuleCtx, s: &[u8]) -> Self {
        Self::from_bytes(s)
    }

    /// Compatibility adoption shim; no longer const because AM detachment can
    /// require native operations. Prefer [`Self::from_owned_ptr`].
    ///
    /// # Safety
    /// The ownership, native access and actual producer-context obligations of
    /// [`Self::from_owned_ptr`] apply, and `inner` must be non-null.
    pub unsafe fn from_redis_module_string(
        ctx: *mut raw::RedisModuleCtx,
        inner: *mut raw::RedisModuleString,
    ) -> Self {
        unsafe {
            Self::from_owned_ptr(
                NonNull::new(ctx),
                NonNull::new(inner).expect("non-null native string"),
            )
        }
    }

    /// Replace a consumed owner's pointer after native defrag moved its object.
    /// # Safety
    /// `replacement` must carry exactly this owner's transferred reference; its
    /// old pointer must no longer be used. Requires legal native defrag access.
    pub(crate) unsafe fn replace_ptr(
        mut self,
        replacement: NonNull<raw::RedisModuleString>,
    ) -> Self {
        self.inner = replacement;
        self
    }

    /// Borrow strict UTF-8 text from an ownerless native pointer.
    /// Prefer [`Self::try_as_str`] when a Rust owner is available.
    ///
    /// # Safety
    /// Redis's API must be initialized and `ptr` must identify a valid non-null
    /// native string. Its buffer must remain alive and unchanged for all of `'a`:
    /// no freeing, trimming, appending or other invalidation through any alias.
    /// Access must obey Redis's server/thread/callback rules. The returned view
    /// is read-only; this function neither retains nor takes ownership.
    ///
    /// ```compile_fail,E0133
    /// use redis_module::RedisString;
    /// fn text(owner: &RedisString) {
    ///     let _ = RedisString::from_ptr(owner.as_ptr());
    /// }
    /// ```
    ///
    /// ```
    /// use redis_module::RedisString;
    /// fn text(owner: &RedisString) -> Result<&str, std::str::Utf8Error> {
    ///     // For a valid owner, borrow only during legal Redis access, with no
    ///     // buffer invalidation through a native alias during this view.
    ///     unsafe { RedisString::from_ptr(owner.as_ptr()) }
    /// }
    /// ```
    pub unsafe fn from_ptr<'a>(ptr: *const raw::RedisModuleString) -> Result<&'a str, Utf8Error> {
        // SAFETY: the caller supplies the native lifetime/access guarantees.
        str::from_utf8(unsafe { Self::string_as_slice(ptr) })
    }

    pub fn append(&mut self, s: &str) -> raw::Status {
        raw::string_append_buffer(ptr::null_mut(), self.as_ptr(), s)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let mut len: usize = 0;
        raw::string_ptr_len(self.as_ptr(), &mut len);
        len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        let mut len: usize = 0;
        raw::string_ptr_len(self.as_ptr(), &mut len);
        len == 0
    }

    #[must_use]
    pub fn as_cstr_ptr_and_len(&self) -> (*const c_char, usize) {
        let mut len: usize = 0;
        let ptr = raw::string_ptr_len(self.as_ptr(), &mut len);
        (ptr, len)
    }

    /// Borrow strict UTF-8 text for the lifetime of this Rust owner.
    ///
    /// This borrows through [`Self::as_slice`] without a copy or native retain.
    /// Rust excludes mutation/drop through this owner while the view is used.
    /// Native aliases (including `safe_clone`), raw constructors
    /// and context/thread validity remain subject to existing SDK contracts.
    ///
    /// ```
    /// use redis_module::{RedisError, RedisString};
    /// fn text(owner: &RedisString) -> Result<&str, RedisError> {
    ///     owner.try_as_str()
    /// }
    /// ```
    ///
    /// ```compile_fail,E0515
    /// use redis_module::RedisString;
    /// fn escape(owner: RedisString) -> &'static str {
    ///     owner.try_as_str().unwrap()
    /// }
    /// ```
    ///
    /// ```compile_fail,E0505
    /// use redis_module::RedisString;
    /// fn drop_owner(owner: RedisString) {
    ///     let text = owner.try_as_str().unwrap();
    ///     drop(owner);
    ///     println!("{text}");
    /// }
    /// ```
    ///
    /// ```compile_fail,E0502
    /// use redis_module::RedisString;
    /// fn mutate_owner(mut owner: RedisString) {
    ///     let text = owner.try_as_str().unwrap();
    ///     owner.append("more");
    ///     println!("{text}");
    /// }
    /// ```
    pub fn try_as_str(&self) -> Result<&str, RedisError> {
        str::from_utf8(self.as_slice())
            .map_err(|_| RedisError::Str("Couldn't parse as UTF-8 string"))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: this owner holds a live native reference; the output borrows
        // it. Legal native access and buffer stability through aliases remain
        // obligations; independent ownership does not establish them.
        unsafe { Self::string_as_slice(self.as_ptr()) }
    }

    /// Borrow raw bytes without a Rust owner or a native retain.
    /// Prefer [`Self::as_slice`] when a Rust owner is available.
    ///
    /// # Safety
    /// The initialized Redis API, valid non-null native string, read-only stable
    /// buffer for all of `'a`, and legal server/thread/callback access guarantees
    /// documented on [`Self::from_ptr`] apply equally to this byte view.
    ///
    /// ```compile_fail,E0133
    /// use redis_module::RedisString;
    /// fn bytes(owner: &RedisString) {
    ///     let _ = RedisString::string_as_slice(owner.as_ptr());
    /// }
    /// ```
    ///
    /// ```
    /// use redis_module::RedisString;
    /// fn bytes(owner: &RedisString) -> &[u8] {
    ///     // For a valid owner during legal Redis access; no alias invalidation.
    ///     unsafe { RedisString::string_as_slice(owner.as_ptr()) }
    /// }
    /// ```
    pub unsafe fn string_as_slice<'a>(ptr: *const raw::RedisModuleString) -> &'a [u8] {
        let mut len: libc::size_t = 0;
        let bytes = unsafe { crate::__required_api!(RedisModule_StringPtrLen)(ptr, &mut len) };

        unsafe { slice::from_raw_parts(bytes.cast::<u8>(), len) }
    }

    /// Performs lossy conversion of a `RedisString` into an owned `String. This conversion
    /// will replace any invalid UTF-8 sequences with U+FFFD REPLACEMENT CHARACTER, which
    /// looks like this: �.
    ///
    /// # Panics
    ///
    /// Will panic if `RedisModule_StringPtrLen` is missing in redismodule.h
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(self.as_slice()).into_owned()
    }

    pub fn parse_unsigned_integer(&self) -> Result<u64, RedisError> {
        let val = self.parse_integer()?;
        u64::try_from(val)
            .map_err(|_| RedisError::Str("Couldn't parse negative number as unsigned integer"))
    }

    pub fn parse_integer(&self) -> Result<i64, RedisError> {
        let mut val: i64 = 0;
        match raw::string_to_longlong(self.as_ptr(), &mut val) {
            raw::Status::Ok => Ok(val),
            raw::Status::Err => Err(RedisError::Str("Couldn't parse as integer")),
        }
    }

    pub fn parse_float(&self) -> Result<f64, RedisError> {
        let mut val: f64 = 0.0;
        match raw::string_to_double(self.as_ptr(), &mut val) {
            raw::Status::Ok => Ok(val),
            raw::Status::Err => Err(RedisError::Str("Couldn't parse as float")),
        }
    }

    // TODO: Redis allows storing and retrieving any arbitrary bytes.
    // However rust's String and str can only store valid UTF-8.
    // Implement these to allow non-utf8 bytes to be consumed:
    // pub fn into_bytes(self) -> Vec<u8> {}
    // pub fn as_bytes(&self) -> &[u8] {}
}

impl Drop for RedisString {
    fn drop(&mut self) {
        // The producer's AM entry was resolved on ingress; no stored context.
        unsafe { crate::__required_api!(RedisModule_FreeString)(ptr::null_mut(), self.as_ptr()) };
    }
}

impl PartialEq for RedisString {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for RedisString {}

impl PartialOrd for RedisString {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RedisString {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        raw::string_compare(self.as_ptr(), other.as_ptr())
    }
}

impl core::hash::Hash for RedisString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl Display for RedisString {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.to_string_lossy())
    }
}

impl Borrow<str> for RedisString {
    fn borrow(&self) -> &str {
        // RedisString might not be UTF-8 safe
        self.try_as_str().unwrap_or("<Invalid UTF-8 data>")
    }
}

impl Clone for RedisString {
    fn clone(&self) -> Self {
        let inner =
            // Redis allows us to create RedisModuleString with NULL context
            // so we use [std::ptr::null_mut()] instead of the curren RedisString context.
            // We do this because we can not promise the new RedisString will not outlive the current
            // context and we want them to be independent.
            unsafe { crate::__required_api!(RedisModule_CreateStringFromString)(ptr::null_mut(), self.as_ptr()) };
        unsafe {
            Self::from_owned_ptr(
                None,
                NonNull::new(inner).expect("CreateStringFromString returned null"),
            )
        }
    }
}

impl From<RedisString> for String {
    fn from(rs: RedisString) -> Self {
        rs.to_string_lossy()
    }
}

impl Deref for RedisString {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl From<RedisString> for Vec<u8> {
    fn from(rs: RedisString) -> Self {
        rs.as_slice().to_vec()
    }
}

impl serde::Serialize for RedisString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bytes(self.as_slice())
    }
}

struct RedisStringVisitor;

impl<'de> serde::de::Visitor<'de> for RedisStringVisitor {
    type Value = RedisString;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("A bytes buffer")
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(RedisString::create(None, v))
    }

    fn visit_seq<V>(self, mut visitor: V) -> Result<Self::Value, V::Error>
    where
        V: SeqAccess<'de>,
    {
        let mut v = if let Some(size_hint) = visitor.size_hint() {
            Vec::with_capacity(size_hint)
        } else {
            Vec::new()
        };
        while let Some(elem) = visitor.next_element()? {
            v.push(elem);
        }

        Ok(RedisString::create(None, v.as_slice()))
    }
}

impl<'de> serde::Deserialize<'de> for RedisString {
    fn deserialize<D>(deserializer: D) -> Result<RedisString, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_bytes(RedisStringVisitor)
    }
}

///////////////////////////////////////////////////

#[derive(Debug)]
pub struct RedisBuffer {
    buffer: *mut c_char,
    len: usize,
}

impl RedisBuffer {
    /// Takes ownership of a buffer Redis allocated; dropping it frees the
    /// buffer with `RedisModule_Free`.
    ///
    /// # Safety
    ///
    /// `buffer` must point to `len` bytes that Redis allocated, such as the
    /// result of `RedisModule_LoadStringBuffer`, owned by no one else.
    pub const unsafe fn new(buffer: *mut c_char, len: usize) -> Self {
        Self { buffer, len }
    }

    pub fn to_string(&self) -> Result<String, FromUtf8Error> {
        String::from_utf8(self.as_ref().to_vec())
    }
}

impl AsRef<[u8]> for RedisBuffer {
    fn as_ref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.buffer as *const u8, self.len) }
    }
}

impl Drop for RedisBuffer {
    fn drop(&mut self) {
        unsafe {
            crate::__required_api!(RedisModule_Free)(self.buffer.cast::<c_void>());
        }
    }
}

#[cfg(test)]
mod ownership_contracts {
    use super::*;

    #[test]
    fn nullable_owned_result_never_initializes_or_calls_redis() {
        // The absent branch creates no native owner and touches no API globals.
        assert!(unsafe { RedisString::from_owned_ptr_or_none(None, ptr::null_mut()) }.is_none());
    }

    #[test]
    fn independent_owner_has_only_one_nonnull_pointer() {
        assert_eq!(
            std::mem::size_of::<RedisString>(),
            std::mem::size_of::<NonNull<raw::RedisModuleString>>()
        );
        assert_eq!(
            std::mem::size_of::<Option<RedisString>>(),
            std::mem::size_of::<RedisString>()
        );
    }
}
