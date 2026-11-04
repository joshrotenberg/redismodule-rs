use std::cell::Cell;
use std::convert::TryFrom;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ops::DerefMut;
use std::os::raw::c_void;
use std::ptr;
use std::ptr::NonNull;
use std::time::Duration;

use libc::size_t;
use std::os::raw::c_int;

use raw::KeyType;

use crate::RedisError;
use crate::RedisResult;
use crate::RedisString;
use crate::native_types::RedisType;
use crate::raw;
use crate::redismodule::REDIS_OK;
pub use crate::redisraw::bindings::*;
use crate::stream::StreamIterator;
use bitflags::bitflags;

/// Access requested when opening a native Redis key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMode {
    Read,
    ReadWrite,
}

bitflags! {
    pub struct KeyFlags: c_int {
        /// Avoid touching the LRU/LFU of the key when opened.
        const NOTOUCH = REDISMODULE_OPEN_KEY_NOTOUCH as c_int;
        /// Don't trigger keyspace event on key misses.
        const NONOTIFY = REDISMODULE_OPEN_KEY_NONOTIFY as c_int;
        /// Don't update keyspace hits/misses counters.
        const NOSTATS = REDISMODULE_OPEN_KEY_NOSTATS as c_int;
        /// Avoid deleting lazy expired keys.
        const NOEXPIRE = REDISMODULE_OPEN_KEY_NOEXPIRE as c_int;
        /// Avoid any effects from fetching the key.
        const NOEFFECTS = REDISMODULE_OPEN_KEY_NOEFFECTS as c_int;
        /// Access lazy expire fields
        const ACCESS_EXPIRED = REDISMODULE_OPEN_KEY_ACCESS_EXPIRED as c_int;
        /// Allow access to a trimmed key that hasent't been deleted yet
        const ACCESS_TRIMMED = REDISMODULE_OPEN_KEY_ACCESS_TRIMMED as c_int;
    }
}

thread_local! {
    /// How many closures on this thread hold data borrowed from a key, through
    /// [`RedisKey::with_value`], [`RedisKeyWritable::with_value_mut`] or
    /// [`RedisKeyWritable::with_string_dma`]. A `Cell<u32>` has no destructor,
    /// which would keep glibc from unloading the module.
    static LENT: Cell<u32> = const { Cell::new(0) };
}

/// The error for key and command access while key data is lent.
pub(crate) const LENT_ERROR: &str =
    "ERR key data is lent to a closure; finish it before using keys or commands";

/// Marks key data as lent to a closure on this thread until dropped, even
/// when the closure panics.
struct Lent;

impl Lent {
    fn begin() -> Self {
        LENT.set(LENT.get() + 1);
        Lent
    }
}

impl Drop for Lent {
    fn drop(&mut self) {
        LENT.set(LENT.get() - 1);
    }
}

/// Whether a closure on this thread holds key data. The closures must be
/// `Send`, so they cannot capture the context or a key handle, but code that
/// reaches one another way, such as through a thread local, could change the
/// borrowed key; the SDK's key and command APIs refuse while this is set.
pub(crate) fn key_data_lent() -> bool {
    LENT.get() != 0
}

/// Refuses key access while key data is lent; see [`key_data_lent`].
fn refuse_while_lent() -> Result<(), RedisError> {
    if key_data_lent() {
        Err(RedisError::Str(LENT_ERROR))
    } else {
        Ok(())
    }
}

/// Panics, for APIs that cannot return an error, while key data is lent.
pub(crate) fn panic_while_lent() {
    assert!(!key_data_lent(), "{LENT_ERROR}");
}

/// Validate both the relative value and the signed absolute deadline.
fn checked_expiration(expire: Duration, now: i64) -> Result<(i64, i64), RedisError> {
    let relative = i64::try_from(expire.as_millis()).map_err(|_| {
        RedisError::Str("ERR invalid expire time: duration exceeds signed milliseconds")
    })?;
    let absolute = now
        .checked_add(relative)
        .filter(|_| now >= 0)
        .ok_or(RedisError::Str(
            "ERR invalid expire time: absolute deadline overflows",
        ))?;
    Ok((relative, absolute))
}

#[cfg(test)]
pub(crate) fn with_test_key_loan<R>(f: impl FnOnce() -> R) -> R {
    let _loan = Lent::begin();
    f()
}

/// A read-only open key, borrowing the runtime context that opened it.
///
/// Dropping the handle closes the native key. It must close before its opening
/// callback ends or its worker lock guard is released, even with AutoMemory off.
#[derive(Debug)]
pub struct RedisKey<'ctx> {
    pub(crate) ctx: *mut raw::RedisModuleCtx,
    pub(crate) key_inner: *mut raw::RedisModuleKey,
    _context: PhantomData<&'ctx ()>,
}

impl<'ctx> RedisKey<'ctx> {
    /// Borrows the native open-key pointer without transferring ownership.
    /// The pointer can be null for a read-only open of a missing key.
    ///
    /// Native access remains unsafe: keep this owner, its opening context and
    /// Redis lock alive; respect the open mode and all native aliasing rules.
    /// Do not close the pointer, build another owner, retain it past this key,
    /// or use raw APIs to bypass a scoped key-data loan. In particular, do not
    /// delete or replace a stream while its iterator is active. Use
    /// [`Self::to_raw_parts`] only when ownership transfer is intended.
    ///
    /// # Panics
    /// Panics while key data is lent to an SDK scoped callback.
    #[must_use]
    pub fn as_ptr(&self) -> *mut raw::RedisModuleKey {
        panic_while_lent();
        self.key_inner
    }

    /// Opens `key` for reading.
    ///
    /// # Safety
    ///
    /// `ctx` must be a context Redis passed to the module, and this thread must
    /// hold the Redis lock, from this call until the key is dropped (closing
    /// the key uses the context). The chosen `'ctx` lifetime must stay within
    /// that scope; raw pointers cannot establish this borrow. Prefer
    /// [`RuntimeContext::open_key`](crate::RuntimeContext::open_key) and its siblings.
    pub unsafe fn open(ctx: *mut raw::RedisModuleCtx, key: &RedisString) -> Self {
        panic_while_lent();
        let key_inner = raw::open_key(ctx, key.as_ptr(), to_raw_mode(KeyMode::Read));
        Self {
            ctx,
            key_inner,
            _context: PhantomData,
        }
    }

    /// Opens `key` for reading with `flags`.
    ///
    /// # Safety
    ///
    /// `ctx` must be a context Redis passed to the module, and this thread must
    /// hold the Redis lock, from this call until the key is dropped (closing
    /// the key uses the context). The chosen `'ctx` lifetime must stay within
    /// that scope; raw pointers cannot establish this borrow. Prefer
    /// [`RuntimeContext::open_key`](crate::RuntimeContext::open_key) and its siblings.
    pub unsafe fn open_with_flags(
        ctx: *mut raw::RedisModuleCtx,
        key: &RedisString,
        flags: KeyFlags,
    ) -> Self {
        panic_while_lent();
        let key_inner =
            raw::open_key_with_flags(ctx, key.as_ptr(), to_raw_mode(KeyMode::Read), flags.bits());
        Self {
            ctx,
            key_inner,
            _context: PhantomData,
        }
    }

    /// Construct a new `RedisKey` from a pointer to a redismodule context and a key.
    ///
    /// The returned `RedisKey` owns the key: dropping it calls `RedisModule_CloseKey`
    /// unless `key_inner` is null.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// 1. The `ctx` pointer remains valid for the lifetime of the `RedisKey`.
    /// 2. `key_inner` is null (a read-only open of a missing key returns null), or a
    ///    key that is open, remains valid for the lifetime of the `RedisKey`, and has
    ///    no other owner that will close it.
    /// 3. `ctx` is the actual native opening/producer context for this key; string
    ///    getters use it to resolve any AutoMemory registrations.
    /// 4. The Redis lock remains held until the key is dropped. The chosen
    ///    `'ctx` lifetime must stay within that context and lock scope; these raw
    ///    pointers cannot establish a borrow.
    pub const unsafe fn from_raw_parts(
        ctx: *mut raw::RedisModuleCtx,
        key_inner: *mut raw::RedisModuleKey,
    ) -> Self {
        Self {
            ctx,
            key_inner,
            _context: PhantomData,
        }
    }

    /// Decomposes a `RedisKey` into its raw components: `(redismodule context pointer, key pointer)`.
    ///
    /// The key stays open, and closing it becomes the caller's job: either rebuild one
    /// owner with [`from_raw_parts`][Self::from_raw_parts] and let it drop, or call
    /// `RedisModule_CloseKey` once. A null key pointer needs no close. Either way the
    /// key must be closed while the context that opened it is still valid. If the
    /// caller does neither, the key stays open until Redis frees it with the context's
    /// AutoMemory, or leaks if AutoMemory is off.
    #[must_use = "the key stays open; close it or rebuild a RedisKey with from_raw_parts"]
    pub fn to_raw_parts(self) -> (*mut raw::RedisModuleCtx, *mut raw::RedisModuleKey) {
        // Skip Drop, which would close the key being handed over.
        let key = ManuallyDrop::new(self);
        (key.ctx, key.key_inner)
    }

    /// Calls `f` with the key's value, read as the type `redis_type` stores,
    /// and returns what it returns; `None` for an empty key. `f` must be
    /// `Send`, so it cannot hold the context, a key handle or a `RedisString`,
    /// and while it runs the SDK refuses calls and key access on this thread
    /// (returning `ERR key data is lent to a closure`, or panicking where the
    /// API returns no `Result`, as opening a key and locking a context do), so
    /// it cannot change the key while it borrows the value. A closure that
    /// captures the context does not compile:
    ///
    /// ```compile_fail,E0277
    /// # use redis_module::{error::Error, io::{RdbReader, RdbWriter}, native_types::{NativeType, RedisType}, Context, RedisError, RedisString};
    /// # struct Count(u64);
    /// # impl NativeType for Count {
    /// #     fn rdb_load(_: &mut RdbReader, _: i32) -> Result<Self, Error> { Ok(Count(0)) }
    /// #     fn rdb_save(&self, _: &mut RdbWriter) {}
    /// # }
    /// # fn deleted(ctx: &Context, name: &RedisString, kind: &RedisType<Count>) -> Result<(), RedisError> {
    /// let key = ctx.open_key(name);
    /// key.with_value(kind, |count| {
    ///     // Capturing the context would let the closure run DEL here.
    ///     ctx.log_notice("reading the count");
    ///     count.0
    /// })?;
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type, and an
    /// error if `redis_type` is not registered or the running Redis does not
    /// provide `RedisModule_ModuleTypeGetValue`.
    pub fn with_value<T: Send + 'static, R>(
        &self,
        redis_type: &RedisType<T>,
        f: impl FnOnce(&T) -> R + Send,
    ) -> Result<Option<R>, RedisError> {
        // SAFETY: f is Send, so it holds no context or key handle, and the
        // SDK refuses key and command access while it runs.
        let value = unsafe { self.get_value(redis_type) }?;
        let _lent = Lent::begin();
        Ok(value.map(f))
    }

    /// The value of the key, read as the type `redis_type` stores; `None` for
    /// an empty key. [`with_value`](Self::with_value) is the safe form.
    ///
    /// # Safety
    ///
    /// While the borrow is live, nothing may delete, replace, resize or move
    /// the key's value: no [`RuntimeContext::call`](crate::RuntimeContext::call)
    /// or other command that writes the key, no other handle to the same key
    /// (string DMA, including a read through `read`, replaces an int- or
    /// embstr-encoded string, and in write mode a shared one), no
    /// `yield_now` (other clients' commands run during it), and no keyspace
    /// notification or other module callback that Redis runs synchronously,
    /// such as the `new` event a write to an empty key raises. The safe
    /// scoped forms refuse these while their closure runs.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type, and an
    /// error if `redis_type` is not registered or the running Redis does not
    /// provide `RedisModule_ModuleTypeGetValue`.
    pub unsafe fn get_value<T: Send + 'static>(
        &self,
        redis_type: &RedisType<T>,
    ) -> Result<Option<&T>, RedisError> {
        refuse_while_lent()?;
        verify_type(self.key_inner, redis_type)?;

        let value = unsafe {
            crate::__available_api!(RedisModule_ModuleTypeGetValue)(self.key_inner).cast::<T>()
        };

        if value.is_null() {
            return Ok(None);
        }

        let value = unsafe { &*value };

        Ok(Some(value))
    }

    /// # Panics
    ///
    /// Will panic if `RedisModule_KeyType` is missing in redismodule.h
    #[must_use]
    pub fn key_type(&self) -> raw::KeyType {
        unsafe { crate::__required_api!(RedisModule_KeyType)(self.key_inner) }.into()
    }

    /// Detects whether the key pointer given to us by Redis is null.
    #[must_use]
    pub fn is_null(&self) -> bool {
        let null_key: *mut raw::RedisModuleKey = ptr::null_mut();
        self.key_inner == null_key
    }

    /// Copies the key's string bytes into an owned buffer.
    ///
    /// Returns `None` when the key does not exist, and `Some(Vec::new())` for
    /// an existing empty string. The bytes need not be UTF-8. The returned
    /// buffer does not borrow the key or its opening context; the key handle
    /// itself must still close within its context and Redis lock scope.
    ///
    /// ```no_run
    /// use redis_module::{Context, RedisError, RedisString};
    /// fn copy_value(ctx: &Context, name: &RedisString) -> Result<Option<Vec<u8>>, RedisError> {
    ///     ctx.open_key(name).read()
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the key does not hold a string or key data is
    /// currently lent to a scoped callback.
    pub fn read(&self) -> Result<Option<Vec<u8>>, RedisError> {
        refuse_while_lent()?;
        // SAFETY: the bytes are copied before anything else can run.
        Ok(unsafe { self.read_borrowed() }?.map(<[u8]>::to_vec))
    }

    /// The key's string value, borrowed from Redis; `None` for an empty key.
    /// [`read`](Self::read) is the safe, copying form.
    ///
    /// # Safety
    ///
    /// While the borrow is live, nothing may delete, replace, resize or move
    /// the key's value: no [`RuntimeContext::call`](crate::RuntimeContext::call)
    /// or other command that writes the key, no other handle to the same key
    /// (string DMA, including a read through `read`, replaces an int- or
    /// embstr-encoded string, and in write mode a shared one), no
    /// `yield_now` (other clients' commands run during it), and no keyspace
    /// notification or other module callback that Redis runs synchronously,
    /// such as the `new` event a write to an empty key raises. The safe
    /// scoped forms refuse these while their closure runs.
    ///
    /// # Errors
    ///
    /// Returns an error if the key does not hold a string.
    pub unsafe fn read_borrowed(&self) -> Result<Option<&[u8]>, RedisError> {
        refuse_while_lent()?;
        if self.is_null() {
            Ok(None)
        } else {
            let mut length: size_t = 0;
            let dma = raw::string_dma(self.key_inner, &mut length, raw::KeyMode::READ);
            if dma.is_null() {
                Err(RedisError::Str("ERR Could not read key"))
            } else {
                Ok(Some(unsafe {
                    std::slice::from_raw_parts(dma.cast::<u8>(), length)
                }))
            }
        }
    }

    /// Reads one field from a hash, returning an owned Redis string.
    ///
    /// Returns `None` for a missing key or field. The value preserves arbitrary
    /// bytes; use [`RedisString::try_as_str`] only when text is expected.
    /// The result owns its reference but remains subject to `RedisString`'s
    /// thread and Redis-access rules.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] for a non-hash key, or an error when
    /// the field contains NUL or key data is lent to a scoped callback. Use
    /// [`Self::hash_get_by_string`] for a field containing NUL bytes.
    pub fn hash_get(&self, field: impl AsRef<[u8]>) -> Result<Option<RedisString>, RedisError> {
        refuse_while_lent()?;
        let val = if self.is_null() {
            None
        } else {
            hash_mget_key(self.ctx, self.key_inner, &[field.as_ref()])?
                .pop()
                .expect("hash_mget_key should return vector of same length as input")
        };
        Ok(val)
    }

    /// Like [`Self::hash_get`], with the field given as an existing [`RedisString`].
    ///
    /// Avoids the per-call `CString` and the transient field object Redis creates for
    /// C-string fields; prefer it when the same field is fetched for many keys.
    /// `None` when the key does not exist or has no such field.
    pub fn hash_get_by_string(
        &self,
        field: &RedisString,
    ) -> Result<Option<RedisString>, RedisError> {
        refuse_while_lent()?;
        if self.is_null() {
            return Ok(None);
        }
        let value = raw::hash_get_by_string(self.key_inner, field.as_ptr())?;
        Ok((!value.is_null()).then(|| unsafe {
            RedisString::from_owned_ptr(
                NonNull::new(self.ctx),
                NonNull::new(value).expect("present hash result"),
            )
        }))
    }

    /// Returns the values associated with the specified fields in the hash stored at this key.
    /// The result will be `None` if the key does not exist.
    pub fn hash_get_multi<'a, A, B>(
        &self,
        fields: &'a [A],
    ) -> Result<Option<HMGetResult<'a, A, B>>, RedisError>
    where
        A: Into<Vec<u8>> + Clone,
        RedisString: Into<B>,
    {
        refuse_while_lent()?;
        let val = if self.is_null() {
            None
        } else {
            Some(HMGetResult {
                fields,
                values: hash_mget_key(self.ctx, self.key_inner, fields)?,
                phantom: std::marker::PhantomData,
            })
        };
        Ok(val)
    }

    pub fn get_stream_iterator(&self, reverse: bool) -> Result<StreamIterator<'_>, RedisError> {
        refuse_while_lent()?;
        StreamIterator::new(self, None, None, false, reverse)
    }

    /// Iterates a stream while borrowing this key and its opening context.
    ///
    /// ```compile_fail,E0505
    /// # use redis_module::{DetachedFromClient, RedisString, ThreadSafeContext};
    /// # fn bad(owner: &ThreadSafeContext<DetachedFromClient>, name: &RedisString) {
    /// let guard = owner.lock();
    /// let key = guard.open_key(name);
    /// let mut records = key.get_stream_range_iterator(None, None, false, false).unwrap();
    /// drop(guard);
    /// records.next();
    /// # }
    /// ```
    pub fn get_stream_range_iterator(
        &self,
        from: Option<raw::RedisModuleStreamID>,
        to: Option<raw::RedisModuleStreamID>,
        exclusive: bool,
        reverse: bool,
    ) -> Result<StreamIterator<'_>, RedisError> {
        refuse_while_lent()?;
        StreamIterator::new(self, from, to, exclusive, reverse)
    }
}

impl Drop for RedisKey<'_> {
    // Frees resources appropriately as a RedisKey goes out of scope.
    fn drop(&mut self) {
        if !self.key_inner.is_null() {
            raw::close_key(self.key_inner);
        }
    }
}

/// `RedisKeyWritable` is an abstraction over a Redis key that allows read and
/// write operations, borrowing the runtime context that opened it.
///
/// Dropping the handle closes the native key while that context and its lock
/// are still live. Copy results into Rust-owned data before leaving that scope.
pub struct RedisKeyWritable<'ctx> {
    ctx: *mut raw::RedisModuleCtx,
    key_inner: *mut raw::RedisModuleKey,
    _context: PhantomData<&'ctx ()>,
}

impl<'ctx> RedisKeyWritable<'ctx> {
    /// Borrows the native open-key pointer; this handle still closes it.
    /// No ownership or additional native-access permission is transferred.
    /// Follow [`RedisKey::as_ptr`]'s context, lock, aliasing and loan rules.
    ///
    /// # Panics
    /// Panics while key data is lent to an SDK scoped callback.
    #[must_use]
    pub fn as_ptr(&self) -> *mut raw::RedisModuleKey {
        panic_while_lent();
        self.key_inner
    }

    /// Opens `key` for reading and writing.
    ///
    /// # Safety
    ///
    /// `ctx` must be a context Redis passed to the module, and this thread must
    /// hold the Redis lock, from this call until the key is dropped (closing
    /// the key uses the context). The chosen `'ctx` lifetime must stay within
    /// that scope; raw pointers cannot establish this borrow. Prefer
    /// [`RuntimeContext::open_key`](crate::RuntimeContext::open_key) and its siblings.
    pub unsafe fn open(ctx: *mut raw::RedisModuleCtx, key: &RedisString) -> Self {
        panic_while_lent();
        let key_inner = raw::open_key(ctx, key.as_ptr(), to_raw_mode(KeyMode::ReadWrite));
        Self {
            ctx,
            key_inner,
            _context: PhantomData,
        }
    }

    /// Opens `key` for reading and writing with `flags`.
    ///
    /// # Safety
    ///
    /// `ctx` must be a context Redis passed to the module, and this thread must
    /// hold the Redis lock, from this call until the key is dropped (closing
    /// the key uses the context). The chosen `'ctx` lifetime must stay within
    /// that scope; raw pointers cannot establish this borrow. Prefer
    /// [`RuntimeContext::open_key`](crate::RuntimeContext::open_key) and its siblings.
    pub unsafe fn open_with_flags(
        ctx: *mut raw::RedisModuleCtx,
        key: &RedisString,
        flags: KeyFlags,
    ) -> Self {
        panic_while_lent();
        let key_inner = raw::open_key_with_flags(
            ctx,
            key.as_ptr(),
            to_raw_mode(KeyMode::ReadWrite),
            flags.bits(),
        );
        Self {
            ctx,
            key_inner,
            _context: PhantomData,
        }
    }

    /// Returns `true` if the key is of type [KeyType::Empty].
    ///
    /// # Note
    ///
    /// An empty key can be reliably detected by looking for a null
    /// as the key is opened [RedisKeyWritable::open] in read mode,
    /// but when asking for a write, Redis returns a non-null pointer
    /// to allow to write to even an empty key. In that case, the key's
    /// value should be checked manually instead:
    ///
    /// ```
    /// use redis_module::key::RedisKeyWritable;
    /// use redis_module::RedisError;
    ///
    /// fn is_empty_old(key: &mut RedisKeyWritable) -> Result<bool, RedisError> {
    ///     key.with_string_dma(|s| Ok(s.write(b"new value")?.is_empty()))?
    /// }
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.key_type() == KeyType::Empty
    }

    /// Calls `f` with direct access to the string value's buffer and returns
    /// what it returns. `f` must be `Send`, so it cannot hold the context, a
    /// key handle or a `RedisString`, and while it runs the SDK refuses calls
    /// and key access on this thread except through the buffer, so it cannot
    /// change the key while it holds the buffer.
    ///
    /// Writing to an empty key creates it, and Redis raises the `new`
    /// keyspace event synchronously: this module's subscribers run with the
    /// data still lent, so their calls fail and opening a key panics, which
    /// the SDK logs. Another module's subscribers are outside this guard;
    /// Redis forbids writes from notification callbacks.
    ///
    /// A closure that captures the context does not compile:
    ///
    /// ```compile_fail,E0277
    /// # use redis_module::{Context, RedisError, RedisString};
    /// # fn deleted(ctx: &Context, name: &RedisString) -> Result<(), RedisError> {
    /// let mut key = ctx.open_key_writable(name);
    /// key.with_string_dma(|data| {
    ///     // Capturing the context would let the closure run DEL here.
    ///     ctx.log_notice("writing the value");
    ///     data.write(b"value").map(|_| ())
    /// })??;
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the key does not hold a string or is not open for
    /// writing.
    pub fn with_string_dma<R>(
        &mut self,
        f: impl FnOnce(&mut StringDMA<'_>) -> R + Send,
    ) -> Result<R, RedisError> {
        // SAFETY: f is Send, so it holds no context or key handle, and the
        // SDK refuses key and command access while it runs, except through
        // the buffer it receives.
        let mut dma = unsafe { self.as_string_dma() }?;
        let _lent = Lent::begin();
        Ok(f(&mut dma))
    }

    /// Direct access to the string value's buffer. The view borrows the key
    /// mutably, so no other call on this handle can resize or delete the value
    /// while it is live. The borrow also keeps the opening context locked:
    ///
    /// ```compile_fail,E0505
    /// # use redis_module::{DetachedFromClient, RedisString, ThreadSafeContext};
    /// # fn bad(owner: &ThreadSafeContext<DetachedFromClient>, name: &RedisString) {
    /// let guard = owner.lock();
    /// let mut key = guard.open_key_writable(name);
    /// let mut data = unsafe { key.as_string_dma() }.unwrap();
    /// drop(guard);
    /// data.write(b"value").unwrap();
    /// # }
    /// ```
    ///
    /// [`with_string_dma`](Self::with_string_dma) is the safe form.
    ///
    /// # Safety
    ///
    /// While the borrow is live, nothing may delete, replace, resize or move
    /// the key's value: no [`RuntimeContext::call`](crate::RuntimeContext::call)
    /// or other command that writes the key, no other handle to the same key
    /// (string DMA, including a read through `read`, replaces an int- or
    /// embstr-encoded string, and in write mode a shared one), no
    /// `yield_now` (other clients' commands run during it), and no keyspace
    /// notification or other module callback that Redis runs synchronously,
    /// such as the `new` event a write to an empty key raises. The safe
    /// scoped forms refuse these while their closure runs.
    ///
    /// # Errors
    ///
    /// Returns an error if the key does not hold a string or is not open for
    /// writing.
    pub unsafe fn as_string_dma(&mut self) -> Result<StringDMA<'_>, RedisError> {
        refuse_while_lent()?;
        StringDMA::new(self)
    }

    /// Sets `field` to `value`. Returns the number of existing fields replaced:
    /// 0 for a new field, 1 for a replaced one.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type, and an
    /// error if `field` contains a NUL byte; use [`Self::hash_set_by_string`]
    /// for such fields.
    pub fn hash_set(&self, field: &str, value: RedisString) -> Result<usize, RedisError> {
        refuse_while_lent()?;
        raw::hash_set(self.key_inner, field, value.as_ptr())
    }

    /// Like [`Self::hash_set`], with the field and value borrowed as Redis
    /// strings, so the field can hold any bytes.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type.
    pub fn hash_set_by_string(
        &self,
        field: &RedisString,
        value: &RedisString,
    ) -> Result<usize, RedisError> {
        refuse_while_lent()?;
        raw::hash_set_by_string(self.key_inner, field.as_ptr(), value.as_ptr())
    }

    /// Deletes `field`. Returns the number of fields deleted: 0 or 1.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type, and an
    /// error if `field` contains a NUL byte; use [`Self::hash_del_by_string`]
    /// for such fields.
    pub fn hash_del(&self, field: &str) -> Result<usize, RedisError> {
        refuse_while_lent()?;
        raw::hash_del(self.key_inner, field)
    }

    /// Like [`Self::hash_del`], with the field as a Redis string, so it can hold
    /// any bytes.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type.
    pub fn hash_del_by_string(&self, field: &RedisString) -> Result<usize, RedisError> {
        refuse_while_lent()?;
        raw::hash_del_by_string(self.key_inner, field.as_ptr())
    }

    /// Reads one hash field through this writable handle.
    ///
    /// Returns an owned [`RedisString`], or `None` if the key or field is
    /// missing. Value bytes need not be UTF-8.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] for a non-hash key, or an error when
    /// the field contains NUL or key data is lent to a scoped callback. Use
    /// [`Self::hash_get_by_string`] for a field containing NUL bytes.
    pub fn hash_get(&self, field: &str) -> Result<Option<RedisString>, RedisError> {
        refuse_while_lent()?;
        Ok(hash_mget_key(self.ctx, self.key_inner, &[field])?
            .pop()
            .expect("hash_mget_key should return vector of same length as input"))
    }

    /// Like [`Self::hash_get`], with the field given as an existing [`RedisString`].
    ///
    /// Avoids the per-call `CString` and the transient field object Redis creates for
    /// C-string fields; prefer it when the same field is fetched for many keys.
    /// `None` when the hash has no such field.
    pub fn hash_get_by_string(
        &self,
        field: &RedisString,
    ) -> Result<Option<RedisString>, RedisError> {
        refuse_while_lent()?;
        let value = raw::hash_get_by_string(self.key_inner, field.as_ptr())?;
        Ok((!value.is_null()).then(|| unsafe {
            RedisString::from_owned_ptr(
                NonNull::new(self.ctx),
                NonNull::new(value).expect("present hash result"),
            )
        }))
    }

    /// Returns the values associated with the specified fields in the hash stored at this key.
    pub fn hash_get_multi<'a, A, B>(
        &self,
        fields: &'a [A],
    ) -> Result<HMGetResult<'a, A, B>, RedisError>
    where
        A: Into<Vec<u8>> + Clone,
        RedisString: Into<B>,
    {
        refuse_while_lent()?;
        Ok(HMGetResult {
            fields,
            values: hash_mget_key(self.ctx, self.key_inner, fields)?,
            phantom: std::marker::PhantomData,
        })
    }

    /// Inserts `element` at the head of the list, creating it if missing.
    ///
    /// Returns [`raw::Status::Err`] for a non-list key or while key data is
    /// lent to a scoped callback. This write is not propagated automatically;
    /// the command must arrange replication and AOF propagation.
    #[allow(clippy::must_use_candidate)]
    pub fn list_push_head(&self, element: RedisString) -> raw::Status {
        if key_data_lent() {
            return raw::Status::Err;
        }
        raw::list_push(self.key_inner, raw::Where::ListHead, element.as_ptr())
    }

    /// Inserts `element` at the tail of the list, creating it if missing.
    ///
    /// Returns [`raw::Status::Err`] for a non-list key or while key data is
    /// lent to a scoped callback. This write is not propagated automatically;
    /// the command must arrange replication and AOF propagation.
    #[allow(clippy::must_use_candidate)]
    pub fn list_push_tail(&self, element: RedisString) -> raw::Status {
        if key_data_lent() {
            return raw::Status::Err;
        }
        raw::list_push(self.key_inner, raw::Where::ListTail, element.as_ptr())
    }

    /// Removes and returns the first element as an owned [`RedisString`].
    ///
    /// Returns `None` for an empty/missing list or a non-list key. This write
    /// is not propagated automatically; the command must arrange replication
    /// and AOF propagation.
    ///
    /// # Panics
    ///
    /// Panics while key data is lent to a scoped callback.
    #[allow(clippy::must_use_candidate)]
    pub fn list_pop_head(&self) -> Option<RedisString> {
        panic_while_lent();
        let ptr = raw::list_pop(self.key_inner, raw::Where::ListHead);

        if ptr.is_null() {
            return None;
        }

        Some(unsafe {
            RedisString::from_owned_ptr(
                NonNull::new(self.ctx),
                NonNull::new(ptr).expect("present list result"),
            )
        })
    }

    /// Removes and returns the last element as an owned [`RedisString`].
    ///
    /// Returns `None` for an empty/missing list or a non-list key. This write
    /// is not propagated automatically; the command must arrange replication
    /// and AOF propagation.
    ///
    /// # Panics
    ///
    /// Panics while key data is lent to a scoped callback.
    #[must_use]
    pub fn list_pop_tail(&self) -> Option<RedisString> {
        panic_while_lent();
        let ptr = raw::list_pop(self.key_inner, raw::Where::ListTail);

        if ptr.is_null() {
            return None;
        }

        Some(unsafe {
            RedisString::from_owned_ptr(
                NonNull::new(self.ctx),
                NonNull::new(ptr).expect("present list result"),
            )
        })
    }

    /// Sets the key's expiry to `expire` after the current Redis time.
    ///
    /// Sub-millisecond precision is discarded. A zero duration makes the key
    /// eligible to expire at the current timestamp; deletion timing follows
    /// Redis's expiry processing. This operation does not propagate itself to
    /// replicas or the AOF; the command must arrange propagation.
    ///
    /// Rejects a timestamp outside signed Redis milliseconds before changing
    /// the key. Redis 6.2+ receives the checked
    /// absolute deadline, avoiding a second native addition. Redis 6.0 has only
    /// the relative setter: its later wall-clock reading can still differ from
    /// this precheck, for example after a clock jump at the extreme boundary.
    ///
    /// # Errors
    ///
    /// Returns an `ERR` error when the duration cannot fit signed
    /// milliseconds, adding it to the current time overflows, the key is
    /// missing or not writable, or key data is lent to a scoped callback.
    /// Rejected overflow leaves the previous value and expiry unchanged.
    ///
    /// ```no_run
    /// use redis_module::{Context, RedisResult, RedisString};
    /// use std::time::Duration;
    /// fn expire(ctx: &Context, name: &RedisString) -> RedisResult {
    ///     ctx.open_key_writable(name).set_expire(Duration::from_secs(60))
    /// }
    /// ```
    pub fn set_expire(&self, expire: Duration) -> RedisResult {
        refuse_while_lent()?;
        let (relative, absolute) = checked_expiration(expire, crate::milliseconds())?;
        let status = match unsafe { raw::RedisModule_SetAbsExpire } {
            Some(set) => unsafe { set(self.key_inner, absolute) }.into(),
            None => raw::set_expire(self.key_inner, relative),
        };
        match status {
            raw::Status::Ok => REDIS_OK,
            // The key can be empty or not open for writing.
            raw::Status::Err => Err(RedisError::Str("ERR Error while setting key expire")),
        }
    }

    /// Remove expiration from a key if it exists.
    pub fn remove_expire(&self) -> RedisResult {
        refuse_while_lent()?;
        match raw::set_expire(self.key_inner, REDISMODULE_NO_EXPIRE.into()) {
            raw::Status::Ok => REDIS_OK,

            // Error may occur if the key wasn't open for writing or is an
            // empty key.
            raw::Status::Err => Err(RedisError::Str("ERR Error while removing key expire")),
        }
    }

    pub fn write(&self, val: &str) -> RedisResult {
        refuse_while_lent()?;
        let val_str = RedisString::create(NonNull::new(self.ctx), val);
        match raw::string_set(self.key_inner, val_str.as_ptr()) {
            raw::Status::Ok => REDIS_OK,
            raw::Status::Err => Err(RedisError::Str("ERR Error while setting key")),
        }
    }

    /// Deletes the key. Returns whether it held a value.
    ///
    /// # Errors
    ///
    /// Returns an error if Redis does not delete the key, or the running Redis
    /// does not provide `RedisModule_DeleteKey`.
    pub fn delete(&self) -> Result<bool, RedisError> {
        refuse_while_lent()?;
        let delete = unsafe { crate::__available_api!(RedisModule_DeleteKey) };
        let held_value = self.key_type() != KeyType::Empty;
        if unsafe { delete(self.key_inner) } != raw::REDISMODULE_OK as c_int {
            return Err(RedisError::Str("ERR Redis did not delete the key"));
        }
        Ok(held_value)
    }

    /// Unlinks the key: deletes it and frees its value in the background.
    /// Returns whether it held a value.
    ///
    /// # Errors
    ///
    /// Returns an error if Redis does not unlink the key, or the running Redis
    /// does not provide `RedisModule_UnlinkKey`.
    pub fn unlink(&self) -> Result<bool, RedisError> {
        refuse_while_lent()?;
        let unlink = unsafe { crate::__available_api!(RedisModule_UnlinkKey) };
        let held_value = self.key_type() != KeyType::Empty;
        if unsafe { unlink(self.key_inner) } != raw::REDISMODULE_OK as c_int {
            return Err(RedisError::Str("ERR Redis did not unlink the key"));
        }
        Ok(held_value)
    }

    /// # Panics
    ///
    /// Will panic if `RedisModule_KeyType` is missing in redismodule.h
    #[must_use]
    pub fn key_type(&self) -> raw::KeyType {
        unsafe { crate::__required_api!(RedisModule_KeyType)(self.key_inner) }.into()
    }

    /// Opens the key named by a raw Redis string for reading and writing.
    ///
    /// # Safety
    ///
    /// `ctx` must be a context Redis passed to the module, and this thread must
    /// hold the Redis lock, from this call until the key is dropped (closing
    /// the key uses the context). The chosen `'ctx` lifetime must stay within
    /// that scope; raw pointers cannot establish this borrow. Prefer
    /// [`RuntimeContext::open_key`](crate::RuntimeContext::open_key) and its siblings.
    /// `key` must be a live Redis string.
    pub unsafe fn open_with_redis_string(
        ctx: *mut raw::RedisModuleCtx,
        key: *mut raw::RedisModuleString,
    ) -> Self {
        panic_while_lent();
        let key_inner = raw::open_key(ctx, key, to_raw_mode(KeyMode::ReadWrite));
        Self {
            ctx,
            key_inner,
            _context: PhantomData,
        }
    }

    /// Calls `f` with the key's value, read as the type `redis_type` stores,
    /// for changing it in place, and returns what it returns; `None` for an
    /// empty key. Like [`RedisKey::with_value`], `f` must be `Send`.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type, and an
    /// error if `redis_type` is not registered or the running Redis does not
    /// provide `RedisModule_ModuleTypeGetValue`.
    pub fn with_value_mut<T: Send + 'static, R>(
        &mut self,
        redis_type: &RedisType<T>,
        f: impl FnOnce(&mut T) -> R + Send,
    ) -> Result<Option<R>, RedisError> {
        // SAFETY: f is Send, so it holds no context or key handle, and the
        // SDK refuses key and command access while it runs.
        let value = unsafe { self.get_value(redis_type) }?;
        let _lent = Lent::begin();
        Ok(value.map(f))
    }

    /// The key's value for changing in place.
    /// [`with_value_mut`](Self::with_value_mut) is the safe form.
    ///
    /// The reference borrows the key mutably, so only one can be live at a time,
    /// and only while this handle is open. The borrow covers this handle only,
    /// which is why the function is unsafe:
    ///
    /// ```compile_fail,E0499
    /// # use redis_module::{error::Error, io::{RdbReader, RdbWriter}, native_types::{NativeType, RedisType}, Context, RedisError, RedisString};
    /// # struct Count(u64);
    /// # impl NativeType for Count {
    /// #     fn rdb_load(_: &mut RdbReader, _: i32) -> Result<Self, Error> { Ok(Count(0)) }
    /// #     fn rdb_save(&self, _: &mut RdbWriter) {}
    /// # }
    /// # fn two(ctx: &Context, name: &RedisString, kind: &RedisType<Count>) -> Result<(), RedisError> {
    /// let mut key = ctx.open_key_writable(name);
    /// let first = unsafe { key.get_value(kind) }?;
    /// let second = unsafe { key.get_value(kind) }?;
    /// first.map(|value| value.0 += 1);
    /// # Ok(()) }
    /// ```
    ///
    /// ```compile_fail,E0597
    /// # use redis_module::{error::Error, io::{RdbReader, RdbWriter}, native_types::{NativeType, RedisType}, Context, RedisError, RedisString};
    /// # struct Count(u64);
    /// # impl NativeType for Count {
    /// #     fn rdb_load(_: &mut RdbReader, _: i32) -> Result<Self, Error> { Ok(Count(0)) }
    /// #     fn rdb_save(&self, _: &mut RdbWriter) {}
    /// # }
    /// # fn closed(ctx: &Context, name: &RedisString, kind: &RedisType<Count>) -> Result<(), RedisError> {
    /// let value = {
    ///     let mut key = ctx.open_key_writable(name);
    ///     unsafe { key.get_value(kind) }?
    /// };
    /// value.map(|value| value.0 += 1);
    /// # Ok(()) }
    /// ```
    ///
    /// # Safety
    ///
    /// While the borrow is live, nothing may delete, replace, resize or move
    /// the key's value: no [`RuntimeContext::call`](crate::RuntimeContext::call)
    /// or other command that writes the key, no other handle to the same key
    /// (string DMA, including a read through `read`, replaces an int- or
    /// embstr-encoded string, and in write mode a shared one), no
    /// `yield_now` (other clients' commands run during it), and no keyspace
    /// notification or other module callback that Redis runs synchronously,
    /// such as the `new` event a write to an empty key raises. The safe
    /// scoped forms refuse these while their closure runs.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type, and an
    /// error if `redis_type` is not registered or the running Redis does not
    /// provide `RedisModule_ModuleTypeGetValue`.
    pub unsafe fn get_value<T: Send + 'static>(
        &mut self,
        redis_type: &RedisType<T>,
    ) -> Result<Option<&mut T>, RedisError> {
        refuse_while_lent()?;
        verify_type(self.key_inner, redis_type)?;
        let value = unsafe {
            crate::__available_api!(RedisModule_ModuleTypeGetValue)(self.key_inner).cast::<T>()
        };

        if value.is_null() {
            return Ok(None);
        }

        let value = unsafe { &mut *value };
        Ok(Some(value))
    }

    /// Stores `value` as the key's value, replacing any existing value of
    /// this type.
    ///
    /// # Errors
    ///
    /// Returns [`RedisError::WrongType`] if the key holds another type, and an
    /// error if `redis_type` is not registered, Redis refuses the value (the
    /// key is not open for writing, or an iterator is open on it), or the
    /// running Redis does not provide `RedisModule_ModuleTypeSetValue`. A
    /// refused value is dropped.
    pub fn set_value<T: Send + 'static>(
        &self,
        redis_type: &RedisType<T>,
        value: T,
    ) -> Result<(), RedisError> {
        refuse_while_lent()?;
        verify_type(self.key_inner, redis_type)?;
        // Look up the API before boxing, so a missing one cannot leak the value.
        let set_value = unsafe { crate::__available_api!(RedisModule_ModuleTypeSetValue) };
        let value = Box::into_raw(Box::new(value)).cast::<c_void>();
        let status: raw::Status =
            unsafe { set_value(self.key_inner, redis_type.raw_type(), value) }.into();
        if status == raw::Status::Err {
            // SAFETY: Redis did not take the value, so this is still its only owner.
            drop(unsafe { Box::from_raw(value.cast::<T>()) });
            return Err(RedisError::Str("ERR Redis did not store the value"));
        }
        Ok(())
    }

    /// Removes stream entries with IDs less than `id`, returning the count.
    ///
    /// With `approx`, Redis may remove fewer entries to preserve whole internal
    /// blocks. A successful trim can remove zero entries, including on an empty
    /// stream or when no entries are older than the threshold.
    ///
    /// # Errors
    ///
    /// Returns an error if the server does not provide `RedisModule_StreamTrimByID`
    /// or the native trim fails, including a missing key or a non-stream value.
    pub fn trim_stream_by_id(
        &self,
        mut id: raw::RedisModuleStreamID,
        approx: bool,
    ) -> Result<usize, RedisError> {
        refuse_while_lent()?;
        let flags = if approx {
            raw::REDISMODULE_STREAM_TRIM_APPROX
        } else {
            0
        };
        let res = unsafe {
            crate::__available_api!(RedisModule_StreamTrimByID)(
                self.key_inner,
                flags as i32,
                &mut id,
            )
        };
        if res < 0 {
            Err(RedisError::Str("ERR Failed trimming the stream"))
        } else {
            Ok(res as usize)
        }
    }
}

/// Opaque type used to hold multi-get results. Use the provided methods to convert
/// the results into the desired type of Rust collection.
pub struct HMGetResult<'a, A, B>
where
    A: Into<Vec<u8>> + Clone,
    RedisString: Into<B>,
{
    fields: &'a [A],
    values: Vec<Option<RedisString>>,
    phantom: std::marker::PhantomData<B>,
}

pub struct HMGetIter<'a, A, B>
where
    A: Into<Vec<u8>>,
    RedisString: Into<B>,
{
    fields_iter: std::slice::Iter<'a, A>,
    values_iter: std::vec::IntoIter<Option<RedisString>>,
    phantom: std::marker::PhantomData<B>,
}

impl<'a, A, B> Iterator for HMGetIter<'a, A, B>
where
    A: Into<Vec<u8>> + Clone,
    RedisString: Into<B>,
{
    type Item = (A, B);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let a = self.fields_iter.next();
            let b = self.values_iter.next();
            match b {
                None => return None,
                Some(None) => continue,
                Some(Some(rs)) => {
                    return Some((
                        a.expect("field and value slices not of same length")
                            .clone(),
                        rs.into(),
                    ));
                }
            }
        }
    }
}

impl<'a, A, B> IntoIterator for HMGetResult<'a, A, B>
where
    A: Into<Vec<u8>> + Clone,
    RedisString: Into<B>,
{
    type Item = (A, B);
    type IntoIter = HMGetIter<'a, A, B>;

    /// Provides an iterator over the multi-get results in the form of (field-name, field-value)
    /// pairs. The type of field-name elements is the same as that passed to the original multi-
    /// get call, while the field-value elements may be of any type for which a `RedisString` `Into`
    /// conversion is implemented.
    ///
    /// # Examples
    ///
    /// Get a [`HashMap`] from the results:
    ///
    /// ```
    /// use redis_module::key::HMGetResult;
    /// use redis_module::{Context, RedisError, RedisResult, RedisString, RedisValue};
    ///
    /// fn call_hash(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ///     let key_name = RedisString::create(None, "config");
    ///     let fields = &["username", "password", "email"];
    ///     let hm: HMGetResult<'_, &str, RedisString> = ctx
    ///         .open_key(&key_name)
    ///         .hash_get_multi(fields)?
    ///         .ok_or(RedisError::Str("ERR key not found"))?;
    ///     let response: Vec<RedisValue> = hm.into_iter().map(|(_, v)| v.into()).collect();
    ///     Ok(RedisValue::Array(response))
    /// }
    /// ```
    ///
    /// Get a [`Vec`] of only the field values from the results:
    ///
    /// ```
    /// use redis_module::{Context, RedisError, RedisResult, RedisString, RedisValue};
    /// use redis_module::key::HMGetResult;
    ///
    /// fn call_hash(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    ///     let key_name = RedisString::create(None, "config");
    ///     let fields = &["username", "password", "email"];
    ///     let hm: HMGetResult<'_, &str, RedisString> = ctx
    ///          .open_key(&key_name)
    ///          .hash_get_multi(fields)?
    ///          .ok_or(RedisError::Str("ERR key not found"))?;
    ///     let response: Vec<RedisValue> = hm.into_iter().map(|(_, v)| RedisValue::BulkRedisString(v)).collect();
    ///     Ok(RedisValue::Array(response))
    /// }
    /// ```
    ///
    /// [`HashMap`]: std::collections::HashMap
    /// [`Vec`]: Vec
    fn into_iter(self) -> Self::IntoIter {
        Self::IntoIter {
            fields_iter: self.fields.iter(),
            values_iter: self.values.into_iter(),
            phantom: std::marker::PhantomData,
        }
    }
}

pub struct StringDMA<'a> {
    key: &'a RedisKeyWritable<'a>,
    buffer: &'a mut [u8],
}

impl<'a> Deref for StringDMA<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.buffer
    }
}

impl<'a> DerefMut for StringDMA<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buffer
    }
}

impl<'a> StringDMA<'a> {
    fn new(key: &'a RedisKeyWritable<'a>) -> Result<StringDMA<'a>, RedisError> {
        let mut length: size_t = 0;
        let dma = raw::string_dma(key.key_inner, &mut length, raw::KeyMode::WRITE);
        if dma.is_null() {
            Err(RedisError::Str("ERR Could not read key"))
        } else {
            let buffer = unsafe { std::slice::from_raw_parts_mut(dma.cast::<u8>(), length) };
            Ok(StringDMA { key, buffer })
        }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<&mut Self, RedisError> {
        if self.buffer.len() != data.len() {
            if raw::Status::Ok == raw::string_truncate(self.key.key_inner, data.len()) {
                let mut length: size_t = 0;
                let dma = raw::string_dma(self.key.key_inner, &mut length, raw::KeyMode::WRITE);
                self.buffer = unsafe { std::slice::from_raw_parts_mut(dma.cast::<u8>(), length) };
            } else {
                return Err(RedisError::Str("ERR Failed to truncate string"));
            }
        }
        self.buffer[..data.len()].copy_from_slice(data);
        Ok(self)
    }

    pub fn append(&mut self, data: &[u8]) -> Result<&mut Self, RedisError> {
        let current_len = self.buffer.len();
        let new_len = current_len + data.len();
        if raw::Status::Ok == raw::string_truncate(self.key.key_inner, new_len) {
            let mut length: size_t = 0;
            let dma = raw::string_dma(self.key.key_inner, &mut length, raw::KeyMode::WRITE);
            self.buffer = unsafe { std::slice::from_raw_parts_mut(dma.cast::<u8>(), length) };
        } else {
            return Err(RedisError::Str("ERR Failed to truncate string"));
        }
        self.buffer[current_len..new_len].copy_from_slice(data);
        Ok(self)
    }
}

impl Drop for RedisKeyWritable<'_> {
    // Frees resources appropriately as a RedisKey goes out of scope.
    fn drop(&mut self) {
        raw::close_key(self.key_inner);
    }
}

/// Get an arbitrary number of hash fields from a key by batching calls
/// to `raw::hash_get_multi`.
fn hash_mget_key<T>(
    ctx: *mut raw::RedisModuleCtx,
    key: *mut raw::RedisModuleKey,
    fields: &[T],
) -> Result<Vec<Option<RedisString>>, RedisError>
where
    T: Into<Vec<u8>> + Clone,
{
    const BATCH_SIZE: usize = 12;

    let mut values = Vec::with_capacity(fields.len());
    let mut values_raw = [std::ptr::null_mut(); BATCH_SIZE];

    for chunk_fields in fields.chunks(BATCH_SIZE) {
        let chunk_values = &mut values_raw[..chunk_fields.len()];
        raw::hash_get_multi(key, chunk_fields, chunk_values)?;
        values.extend(chunk_values.iter().map(|ptr| {
            if ptr.is_null() {
                None
            } else {
                Some(unsafe {
                    RedisString::from_owned_ptr(
                        NonNull::new(ctx),
                        NonNull::new(*ptr).expect("present hash result"),
                    )
                })
            }
        }));
    }

    Ok(values)
}

fn to_raw_mode(mode: KeyMode) -> raw::KeyMode {
    match mode {
        KeyMode::Read => raw::KeyMode::READ,
        KeyMode::ReadWrite => raw::KeyMode::READ | raw::KeyMode::WRITE,
    }
}

/// # Errors
///
/// Returns [`RedisError::WrongType`] if the key exists and holds a value of
/// another type, and an error if `redis_type` is not registered or the running
/// Redis does not provide `RedisModule_KeyType` or
/// `RedisModule_ModuleTypeGetType`.
pub(crate) fn verify_type<T: Send + 'static>(
    key_inner: *mut raw::RedisModuleKey,
    redis_type: &RedisType<T>,
) -> RedisResult {
    // An unregistered type has no native type: stored, its value would carry a
    // null type, and Redis would crash on the next DEL, TYPE or save.
    if redis_type.raw_type().is_null() {
        return Err(RedisError::Str(
            "ERR the native type is not registered; list it in data_types",
        ));
    }
    let key_type: KeyType =
        unsafe { crate::__available_api!(RedisModule_KeyType)(key_inner) }.into();

    if key_type != KeyType::Empty {
        // The key exists; check its type
        let raw_type = unsafe { crate::__available_api!(RedisModule_ModuleTypeGetType)(key_inner) };

        if raw_type != redis_type.raw_type() {
            return Err(RedisError::WrongType);
        }
    }

    REDIS_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    // Outside a Redis server every module API pointer is None, so a RedisKey that
    // closes its key panics with the name of RedisModule_CloseKey.

    fn open_key_pointer() -> *mut raw::RedisModuleKey {
        NonNull::dangling().as_ptr()
    }

    #[test]
    fn to_raw_parts_leaves_the_key_open() {
        let key = unsafe { RedisKey::from_raw_parts(ptr::null_mut(), open_key_pointer()) };
        let parts = std::panic::catch_unwind(|| key.to_raw_parts())
            .expect("to_raw_parts must not close the key");
        assert_eq!(parts, (ptr::null_mut(), open_key_pointer()));
    }

    #[test]
    fn the_rebuilt_owner_closes_the_key() {
        let key = unsafe { RedisKey::from_raw_parts(ptr::null_mut(), open_key_pointer()) };
        let (ctx, key_inner) = key.to_raw_parts();
        let payload = std::panic::catch_unwind(|| {
            drop(unsafe { RedisKey::from_raw_parts(ctx, key_inner) });
        })
        .unwrap_err();
        assert_eq!(
            payload.downcast_ref::<String>().map(String::as_str),
            Some("RedisModule_CloseKey is not available in this Redis server")
        );
    }
}

#[cfg(test)]
mod expiration_tests {
    use super::checked_expiration;
    use std::time::Duration;

    #[test]
    fn checked_expiration_keeps_the_signed_deadline_boundary() {
        assert_eq!(checked_expiration(Duration::ZERO, 0).unwrap(), (0, 0));
        assert_eq!(
            checked_expiration(Duration::from_millis(7), i64::MAX - 7).unwrap(),
            (7, i64::MAX)
        );
        for (duration, now) in [
            (Duration::from_millis(8), i64::MAX - 7),
            (Duration::from_millis(i64::MAX as u64 + 1), 0),
            (Duration::ZERO, -1),
        ] {
            assert!(
                checked_expiration(duration, now)
                    .unwrap_err()
                    .to_string()
                    .starts_with("ERR ")
            );
        }
    }
}

#[cfg(test)]
mod pointer_loan_tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn borrowed_pointers_obey_the_scoped_key_loan_guard() {
        // No native key is allocated or accessed: these tokens exercise only
        // pointer borrowing and the guard before it, without native Drop.
        let read = ManuallyDrop::new(RedisKey {
            ctx: ptr::null_mut(),
            key_inner: ptr::null_mut(),
            _context: PhantomData,
        });
        let write = ManuallyDrop::new(RedisKeyWritable {
            ctx: ptr::null_mut(),
            key_inner: ptr::null_mut(),
            _context: PhantomData,
        });
        assert!(read.as_ptr().is_null());
        assert!(write.as_ptr().is_null());
        let _loan = Lent::begin();
        assert!(catch_unwind(AssertUnwindSafe(|| read.as_ptr())).is_err());
        assert!(catch_unwind(AssertUnwindSafe(|| write.as_ptr())).is_err());
    }
}
