//! Native data types: Rust values that Redis stores in keys and persists in
//! RDB files.
//!
//! A type implements [`NativeType`], whose required methods read and write the
//! value in an RDB file, and is declared as a `static` [`RedisType`] listed in
//! `redis_module!`'s `data_types`. The SDK generates the native callbacks Redis
//! calls, so a module writes no `extern "C"` code. Each callback runs inside a
//! panic guard. The value is freed by its `Drop`.
//!
//! ```
//! use redis_module::error::Error;
//! use redis_module::io::{RdbReader, RdbWriter};
//! use redis_module::native_types::{NativeType, RedisType};
//!
//! struct Counter(u64);
//!
//! impl NativeType for Counter {
//!     fn rdb_load(io: &mut RdbReader, _encver: i32) -> Result<Self, Error> {
//!         Ok(Counter(io.read_unsigned()?))
//!     }
//!     fn rdb_save(&self, io: &mut RdbWriter) {
//!         io.write_unsigned(self.0);
//!     }
//! }
//!
//! static COUNTER: RedisType<Counter> = RedisType::new("counter01", 0);
//! ```
//!
//! The other callbacks are optional, and each is registered only when the
//! type asks for it, because Redis treats a missing callback differently from
//! one that does nothing: implement the trait and call the matching `with_`
//! method on the `static`, such as [`RedisType::with_mem_usage`] for
//! [`MemUsage`].
//!
//! Redis can free a value on one of its background threads (lazy free, such as
//! `UNLINK`, `FLUSHALL ASYNC`, and since Redis 7.4 a plain `FLUSHALL`), so a
//! native type must be `Send`.

use std::ffi::{CString, c_void};
use std::marker::PhantomData;
use std::os::raw::c_int;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use bitflags::bitflags;

use crate::defrag::DefragContext;
use crate::error::Error;
use crate::io::{RdbReader, RdbWriter};
use crate::logging::{RedisLogLevel, log_io_error};
use crate::{RedisError, RedisString, raw};

/// A Rust type that Redis stores in keys as a native data type.
///
/// Redis frees a value by dropping it, possibly on one of its background
/// threads, hence `Send`.
pub trait NativeType: Sized + Send + 'static {
    /// Reads a value that [`rdb_save`](Self::rdb_save) wrote with encoding
    /// version `encver`. An error, or a panic, makes Redis treat the data as
    /// corrupt: a `RESTORE` fails (Redis 6.0 exits instead), and Redis exits
    /// when an RDB file or a replica's full sync fails to load.
    ///
    /// A read that runs out of data reaches this callback as an error only
    /// when the module sets [`ModuleOptions::HANDLE_IO_ERRORS`]; without it,
    /// Redis aborts inside the read.
    ///
    /// [`ModuleOptions::HANDLE_IO_ERRORS`]: crate::ModuleOptions::HANDLE_IO_ERRORS
    ///
    /// # Errors
    ///
    /// Returns an error for data it cannot read.
    fn rdb_load(io: &mut RdbReader, encver: i32) -> Result<Self, Error>;

    /// Writes the value for [`rdb_load`](Self::rdb_load).
    ///
    /// It must not panic. Redis cannot be told that a save failed partway, so
    /// a panic is logged and ends the process rather than leave an incomplete
    /// value in the RDB file, AOF preamble or `DUMP` payload. A background
    /// save runs in a child process, which exits and fails the save while the
    /// server keeps running and the previous file stays; in the server itself,
    /// as for `SAVE` or `DUMP`, the panic aborts Redis.
    fn rdb_save(&self, io: &mut RdbWriter);
}

/// The memory a value uses, for `MEMORY USAGE`. Enable with
/// [`RedisType::with_mem_usage`]; without it Redis counts 0.
pub trait MemUsage: NativeType {
    /// The value's size in bytes. A panic reports 0.
    fn mem_usage(&self) -> usize;
}

/// Copying a value for `COPY`. Enable with [`RedisType::with_copy`]; without
/// it `COPY` replies that the key's type does not support it.
pub trait CopyValue: NativeType {
    /// A copy of the value stored under `from_key` for `to_key`, or `None`
    /// to fail the `COPY`. A panic fails it too.
    fn copy_value(&self, from_key: &[u8], to_key: &[u8]) -> Option<Self>;
}

/// How much work freeing a value is, which decides whether Redis frees it on
/// a background thread. Enable with [`RedisType::with_free_effort`].
pub trait FreeEffort: NativeType {
    /// The number of allocations freeing the value releases. Redis frees a
    /// value lazily when this is above its threshold (64), and always when it
    /// is 0. A panic reports 1, an immediate free.
    fn free_effort(&self, key: &[u8]) -> usize;
}

/// A callback for a value being removed from the keyspace, before it is
/// freed, possibly later on another thread. Enable with
/// [`RedisType::with_unlink`].
pub trait Unlink: NativeType {
    /// Called with the key the value is removed from. A panic is logged.
    fn unlink(&self, key: &[u8]);
}

/// The value's contribution to `DEBUG DIGEST` and `DEBUG DIGEST-VALUE`.
/// Enable with [`RedisType::with_digest`].
pub trait DigestValue: NativeType {
    /// Adds the value to `digest`. A panic leaves what was added.
    fn digest(&self, digest: &mut Digest);
}

/// Rewriting a value as commands, for an AOF rewrite without an RDB preamble
/// (`aof-use-rdb-preamble no`). Enable with [`RedisType::with_aof_rewrite`].
/// Without it, Redis 8.10 and later fail such a rewrite, and older releases
/// crash the rewrite's child process.
pub trait AofRewrite: NativeType {
    /// Emits the commands that recreate the value under `key`. A panic ends
    /// the rewrite's child process, which fails the rewrite.
    fn aof_rewrite(&self, aof: &mut AofRewriter, key: &[u8]);
}

/// Data saved once per RDB file for the type, rather than per value. Enable
/// with [`RedisType::with_aux`].
pub trait AuxData: NativeType {
    /// When Redis calls [`aux_save`](Self::aux_save).
    const AUX_SAVE_TRIGGERS: AuxTriggers;

    /// Writes the type's data. Writing nothing saves nothing, on servers with
    /// `aux_save2` (Redis 7.2 and later), so the file still loads without the
    /// module. Like [`NativeType::rdb_save`], a panic ends the process.
    ///
    /// On those servers, a callback that gets a context with
    /// `RedisModule_GetContextFromIO` through [`RdbWriter::as_ptr`] must write
    /// at least one value. When nothing was written, Redis returns without
    /// freeing that context, which also holds an execution unit open. After a
    /// save in the server process, such as `SAVE`, Redis then aborts on the
    /// next write while AOF is on or a replica is attached, or else on the
    /// next [`ThreadSafeContext::lock`](crate::ThreadSafeContext::lock). A
    /// `BGSAVE` runs the callback in a child process and is not affected. See
    /// `docs/NATIVE-TYPES.md` in the dependency source revision pinned by
    /// `Cargo.lock` for the versions and the reproduction.
    fn aux_save(io: &mut RdbWriter, when: AuxWhen);

    /// Reads what [`aux_save`](Self::aux_save) wrote. An error, or a panic,
    /// fails the load.
    ///
    /// # Errors
    ///
    /// Returns an error for data it cannot read.
    fn aux_load(io: &mut RdbReader, encver: i32, when: AuxWhen) -> Result<(), Error>;
}

/// The outcome of [`Defrag::defrag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefragStatus {
    /// The value is done.
    Done,
    /// More work remains. Store progress with [`DefragContext::set_cursor`];
    /// Redis calls again later.
    More,
}

/// Active defragmentation of a value's allocations. Enable with
/// [`RedisType::with_defrag`].
pub trait Defrag: NativeType {
    /// Moves the value's allocations with `ctx`. A panic reports
    /// [`DefragStatus::Done`].
    fn defrag(&mut self, ctx: &DefragContext, key: &[u8]) -> DefragStatus;
}

bitflags! {
    /// When Redis calls [`AuxData::aux_save`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AuxTriggers: c_int {
        /// Before the keyspace.
        const BEFORE_RDB = raw::REDISMODULE_AUX_BEFORE_RDB as c_int;
        /// After the keyspace.
        const AFTER_RDB = raw::REDISMODULE_AUX_AFTER_RDB as c_int;
    }
}

/// Which pass an aux callback runs for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxWhen {
    BeforeRdb,
    AfterRdb,
}

impl AuxWhen {
    fn from_raw(when: c_int) -> Self {
        if when == raw::REDISMODULE_AUX_AFTER_RDB as c_int {
            Self::AfterRdb
        } else {
            Self::BeforeRdb
        }
    }
}

/// A digest that [`DigestValue::digest`] adds to.
pub struct Digest {
    md: *mut raw::RedisModuleDigest,
}

impl Digest {
    /// Adds bytes to the current sequence.
    pub fn add_string_buffer(&mut self, bytes: &[u8]) {
        if let Some(add) = unsafe { raw::RedisModule_DigestAddStringBuffer } {
            // SAFETY: md is the digest Redis passed to this callback.
            unsafe { add(self.md, bytes.as_ptr().cast(), bytes.len()) };
        }
    }

    /// Adds an integer to the current sequence.
    pub fn add_long_long(&mut self, value: i64) {
        if let Some(add) = unsafe { raw::RedisModule_DigestAddLongLong } {
            // SAFETY: md is the digest Redis passed to this callback.
            unsafe { add(self.md, value) };
        }
    }

    /// Ends the current sequence. Redis mixes sequences in an order-insensitive
    /// way, and the elements within one in order.
    pub fn end_sequence(&mut self) {
        if let Some(end) = unsafe { raw::RedisModule_DigestEndSequence } {
            // SAFETY: md is the digest Redis passed to this callback.
            unsafe { end(self.md) };
        }
    }
}

/// Emits commands for [`AofRewrite::aof_rewrite`].
pub struct AofRewriter {
    io: *mut raw::RedisModuleIO,
}

impl AofRewriter {
    /// Emits `command` with binary-safe `args`.
    ///
    /// # Errors
    ///
    /// Returns an error if `command` contains a NUL byte, or the running Redis
    /// does not provide `RedisModule_EmitAOF`.
    pub fn emit(&mut self, command: &str, args: &[&[u8]]) -> Result<(), RedisError> {
        let emit = unsafe { raw::RedisModule_EmitAOF }
            .ok_or(RedisError::Str("ERR RedisModule_EmitAOF is unavailable"))?;
        let command = CString::new(command)
            .map_err(|_| RedisError::Str("ERR an AOF command name contains a NUL byte"))?;
        let args: Vec<RedisString> = args
            .iter()
            .map(|arg| RedisString::create_from_slice(ptr::null_mut(), arg))
            .collect();
        let mut argv: Vec<*mut raw::RedisModuleString> =
            args.iter().map(RedisString::as_ptr).collect();
        // SAFETY: io is the rewrite's IO; `v` takes an array of strings and its
        // length, which Redis copies before this returns.
        unsafe {
            emit(
                self.io,
                command.as_ptr(),
                c"v".as_ptr(),
                argv.as_mut_ptr(),
                argv.len(),
            )
        };
        Ok(())
    }
}

/// A native data type that a module registers with Redis, storing values of
/// type `T`.
///
/// Redis 6.0 ignores the `free_effort`, `unlink`, `copy` and `defrag`
/// callbacks, which Redis 6.2 added; the others work on every supported
/// server.
///
/// The name must be 9 characters from `A-Z`, `a-z`, `0-9`, `-` and `_`, and
/// the encoding version must be between 0 and 1023. [`RedisType::new`] checks
/// both, so a `static` with a bad name or version fails to compile:
///
/// ```compile_fail
/// # use redis_module::{error::Error, io::{RdbReader, RdbWriter}, native_types::{NativeType, RedisType}};
/// # struct V;
/// # impl NativeType for V {
/// #     fn rdb_load(_: &mut RdbReader, _: i32) -> Result<Self, Error> { Ok(V) }
/// #     fn rdb_save(&self, _: &mut RdbWriter) {}
/// # }
/// static TOO_LONG: RedisType<V> = RedisType::new("mytype-0001", 0);
/// ```
///
/// ```compile_fail
/// # use redis_module::{error::Error, io::{RdbReader, RdbWriter}, native_types::{NativeType, RedisType}};
/// # struct V;
/// # impl NativeType for V {
/// #     fn rdb_load(_: &mut RdbReader, _: i32) -> Result<Self, Error> { Ok(V) }
/// #     fn rdb_save(&self, _: &mut RdbWriter) {}
/// # }
/// static BAD_VERSION: RedisType<V> = RedisType::new("mytype-01", 1024);
/// ```
///
/// Redis also rejects `AAAAAAAAA` with version 0, whose type id is 0:
///
/// ```compile_fail
/// # use redis_module::{error::Error, io::{RdbReader, RdbWriter}, native_types::{NativeType, RedisType}};
/// # struct V;
/// # impl NativeType for V {
/// #     fn rdb_load(_: &mut RdbReader, _: i32) -> Result<Self, Error> { Ok(V) }
/// #     fn rdb_save(&self, _: &mut RdbWriter) {}
/// # }
/// static ZERO_ID: RedisType<V> = RedisType::new("AAAAAAAAA", 0);
/// ```
///
/// A value is read through the `RedisType` of its own type, so reading it as
/// another type does not compile:
///
/// ```compile_fail,E0308
/// # use redis_module::{error::Error, io::{RdbReader, RdbWriter}, key::RedisKey, native_types::{NativeType, RedisType}, RedisError};
/// # struct A(u64);
/// # struct B(String);
/// # impl NativeType for A {
/// #     fn rdb_load(_: &mut RdbReader, _: i32) -> Result<Self, Error> { Ok(A(0)) }
/// #     fn rdb_save(&self, _: &mut RdbWriter) {}
/// # }
/// # impl NativeType for B {
/// #     fn rdb_load(_: &mut RdbReader, _: i32) -> Result<Self, Error> { Ok(B(String::new())) }
/// #     fn rdb_save(&self, _: &mut RdbWriter) {}
/// # }
/// static A_TYPE: RedisType<A> = RedisType::new("a-type-01", 0);
/// fn read(key: &RedisKey) -> Result<(), RedisError> {
///     let _: Option<&B> = unsafe { key.get_value(&A_TYPE) }?;
///     Ok(())
/// }
/// ```
pub struct RedisType<T: Send + 'static> {
    name: &'static str,
    version: i32,
    type_methods: raw::RedisModuleTypeMethods,
    raw_type: AtomicPtr<raw::RedisModuleType>,
    value: PhantomData<fn() -> T>,
}

/// Whether `byte` may appear in a native type name.
const fn is_type_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// Checks a type name and encoding version, at compile time for a `static`.
const fn check_name_and_version(name: &'static str, version: i32) {
    let bytes = name.as_bytes();
    assert!(
        bytes.len() == 9,
        "a native type name must be exactly 9 characters"
    );
    let mut i = 0;
    while i < bytes.len() {
        assert!(
            is_type_name_byte(bytes[i]),
            "a native type name may only use A-Z, a-z, 0-9, - and _"
        );
        i += 1;
    }
    assert!(
        version >= 0 && version <= 1023,
        "a native type encoding version must be between 0 and 1023"
    );
    // `A` is the first character of the type id alphabet, so this name and
    // version encode to id 0, which Redis rejects.
    let mut all_a = true;
    let mut j = 0;
    while j < bytes.len() {
        if bytes[j] != b'A' {
            all_a = false;
        }
        j += 1;
    }
    assert!(
        !(all_a && version == 0),
        "the native type name AAAAAAAAA with version 0 has type id 0, which Redis rejects"
    );
}

/// Callbacks with nothing set, for the `with_` methods to fill in.
const NO_METHODS: raw::RedisModuleTypeMethods = raw::RedisModuleTypeMethods {
    version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
    rdb_load: None,
    rdb_save: None,
    aof_rewrite: None,
    mem_usage: None,
    digest: None,
    free: None,
    aux_load: None,
    aux_save: None,
    aux_save_triggers: 0,
    free_effort: None,
    unlink: None,
    copy: None,
    defrag: None,
    mem_usage2: None,
    free_effort2: None,
    unlink2: None,
    copy2: None,
    aux_save2: None,
};

impl<T: NativeType> RedisType<T> {
    /// A type named `name` with encoding version `version`, which
    /// [`NativeType::rdb_load`] receives for values this version saved.
    ///
    /// # Panics
    ///
    /// Panics, at compile time for a `static`, if `name` is not 9 characters
    /// from `A-Z`, `a-z`, `0-9`, `-` and `_`, or `version` is not between 0
    /// and 1023.
    #[must_use]
    pub const fn new(name: &'static str, version: i32) -> Self {
        check_name_and_version(name, version);
        let mut type_methods = NO_METHODS;
        type_methods.rdb_load = Some(rdb_load::<T>);
        type_methods.rdb_save = Some(rdb_save::<T>);
        type_methods.free = Some(free::<T>);
        Self {
            name,
            version,
            type_methods,
            raw_type: AtomicPtr::new(ptr::null_mut()),
            value: PhantomData,
        }
    }

    /// Registers [`MemUsage::mem_usage`].
    #[must_use]
    pub const fn with_mem_usage(mut self) -> Self
    where
        T: MemUsage,
    {
        self.type_methods.mem_usage = Some(mem_usage::<T>);
        self
    }

    /// Registers [`CopyValue::copy_value`].
    #[must_use]
    pub const fn with_copy(mut self) -> Self
    where
        T: CopyValue,
    {
        self.type_methods.copy = Some(copy::<T>);
        self
    }

    /// Registers [`FreeEffort::free_effort`].
    #[must_use]
    pub const fn with_free_effort(mut self) -> Self
    where
        T: FreeEffort,
    {
        self.type_methods.free_effort = Some(free_effort::<T>);
        self
    }

    /// Registers [`Unlink::unlink`].
    #[must_use]
    pub const fn with_unlink(mut self) -> Self
    where
        T: Unlink,
    {
        self.type_methods.unlink = Some(unlink::<T>);
        self
    }

    /// Registers [`DigestValue::digest`].
    #[must_use]
    pub const fn with_digest(mut self) -> Self
    where
        T: DigestValue,
    {
        self.type_methods.digest = Some(digest::<T>);
        self
    }

    /// Registers [`AofRewrite::aof_rewrite`].
    #[must_use]
    pub const fn with_aof_rewrite(mut self) -> Self
    where
        T: AofRewrite,
    {
        self.type_methods.aof_rewrite = Some(aof_rewrite::<T>);
        self
    }

    /// Registers the [`AuxData`] callbacks.
    #[must_use]
    pub const fn with_aux(mut self) -> Self
    where
        T: AuxData,
    {
        self.type_methods.aux_load = Some(aux_load::<T>);
        self.type_methods.aux_save = Some(aux_save::<T>);
        // Servers with aux_save2 prefer it: it saves nothing when the type
        // writes nothing.
        self.type_methods.aux_save2 = Some(aux_save::<T>);
        self.type_methods.aux_save_triggers = T::AUX_SAVE_TRIGGERS.bits();
        self
    }

    /// Registers [`Defrag::defrag`].
    #[must_use]
    pub const fn with_defrag(mut self) -> Self
    where
        T: Defrag,
    {
        self.type_methods.defrag = Some(defrag::<T>);
        self
    }
}

impl<T: Send + 'static> RedisType<T> {
    /// A type with native callbacks the module writes itself, for what the
    /// traits do not cover. `T` need not implement [`NativeType`].
    ///
    /// # Safety
    ///
    /// The callbacks must treat each value as a `Box<T>`: values that
    /// [`set_value`](crate::key::RedisKeyWritable::set_value) stores are
    /// `Box::into_raw(Box::new(value))`; `rdb_load`, `copy` and `copy2` must
    /// return such a pointer; `defrag` may replace the value only with another
    /// `Box<T>` from the global allocator; `free` must drop it as a `Box<T>`
    /// and may run on a Redis background thread; and none may unwind.
    ///
    /// # Panics
    ///
    /// Panics, at compile time for a `static`, on a bad name or version, as
    /// [`new`](Self::new) does, or if `rdb_load`, `rdb_save` or `free` is
    /// missing: Redis calls them without checking.
    #[must_use]
    pub const unsafe fn from_raw_methods(
        name: &'static str,
        version: i32,
        type_methods: raw::RedisModuleTypeMethods,
    ) -> Self {
        check_name_and_version(name, version);
        assert!(
            type_methods.rdb_load.is_some()
                && type_methods.rdb_save.is_some()
                && type_methods.free.is_some(),
            "a native type needs rdb_load, rdb_save and free callbacks"
        );
        Self {
            name,
            version,
            type_methods,
            raw_type: AtomicPtr::new(ptr::null_mut()),
            value: PhantomData,
        }
    }

    /// The type Redis registered for this `RedisType`, or null before the
    /// module registers it.
    #[must_use]
    pub fn raw_type(&self) -> *mut raw::RedisModuleType {
        self.raw_type.load(Ordering::Acquire)
    }

    /// Registers this type with Redis. `redis_module!` calls it for each entry
    /// in `data_types`; call it during the module's load.
    ///
    /// # Errors
    ///
    /// Returns an error if Redis does not create the type, for example when
    /// another module registered the same name.
    pub fn create_data_type(&self, ctx: &crate::RegistrationContext<'_>) -> Result<(), &str> {
        let type_name = CString::new(self.name).unwrap();
        LOAD_PROCESS.store(std::process::id(), Ordering::Relaxed);

        let redis_type = unsafe {
            crate::__required_api!(RedisModule_CreateDataType)(
                ctx.ctx,
                type_name.as_ptr(),
                self.version, // Encoding version
                &mut self.type_methods.clone(),
            )
        };

        if redis_type.is_null() {
            ctx.log_notice("Error: created data type is null");
            return Err("ERR created data type is null");
        }

        self.raw_type.store(redis_type, Ordering::Release);

        ctx.log_notice(&format!("Created new data type '{}'", self.name));

        Ok(())
    }
}

/// The process that registered the types, so a callback can tell it runs in a
/// fork child, such as a background save.
static LOAD_PROCESS: AtomicU32 = AtomicU32::new(0);

/// The key a callback receives, as bytes; empty for a null key.
///
/// # Safety
///
/// `key` must be null or a string Redis passed to the running callback.
unsafe fn key_bytes<'a>(key: *const raw::RedisModuleString) -> &'a [u8] {
    if key.is_null() {
        &[]
    } else {
        // SAFETY: the caller passes a live string borrowed for the callback.
        unsafe { RedisString::string_as_slice(key) }
    }
}

/// The message of a panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic payload is not a string".to_owned())
}

/// Runs a callback that writes persisted data. A panic leaves the data
/// incomplete, which Redis cannot be told, so it is logged and the process
/// ends. A fork child, such as a background save, kills itself with
/// `SIGKILL`: the server then removes the child's partial temporary file and
/// records the save as failed, which an exit status alone does not clean up,
/// and no crash handler runs, which an abort would (printing a bug report and
/// possibly removing the server's pid file). The server itself aborts.
fn run_writer(io: *mut raw::RedisModuleIO, callback: &str, f: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(f)) {
        let message = panic_text(payload.as_ref());
        // Dropping the payload could run its Drop outside this guard.
        std::mem::forget(payload);
        let in_child = LOAD_PROCESS.load(Ordering::Relaxed) != std::process::id();
        let ending = if in_child {
            "killing the child process"
        } else {
            "aborting the server"
        };
        // SAFETY: io is the IO Redis passed to the running callback.
        unsafe {
            log_io_error(
                io,
                RedisLogLevel::Warning,
                &format!(
                    "native type {callback} panicked: {message}; {ending} so no incomplete value is written"
                ),
            );
        }
        if in_child {
            // SAFETY: SIGKILL ends this fork child at once; it cannot be caught,
            // so neither Redis's crash handler nor any exit handler runs.
            unsafe { libc::raise(libc::SIGKILL) };
        }
        std::process::abort();
    }
}

unsafe extern "C" fn rdb_load<T: NativeType>(
    io: *mut raw::RedisModuleIO,
    encver: c_int,
) -> *mut c_void {
    crate::panic::run_callback(
        "native type rdb_load",
        || {
            // SAFETY: Redis passes the IO for this load callback.
            let mut rdb = unsafe { RdbReader::new(io) };
            match T::rdb_load(&mut rdb, encver) {
                Ok(value) => Box::into_raw(Box::new(value)).cast(),
                Err(error) => {
                    // SAFETY: io is the IO Redis passed to this callback.
                    unsafe {
                        log_io_error(
                            io,
                            RedisLogLevel::Warning,
                            &format!("native type rdb_load failed: {error}"),
                        );
                    }
                    ptr::null_mut()
                }
            }
        },
        ptr::null_mut,
    )
}

unsafe extern "C" fn rdb_save<T: NativeType>(io: *mut raw::RedisModuleIO, value: *mut c_void) {
    run_writer(io, "rdb_save", || {
        // SAFETY: Redis passes the IO for this save callback and this type's
        // own Box<T> value.
        let mut rdb = unsafe { RdbWriter::new(io) };
        unsafe { &*value.cast::<T>() }.rdb_save(&mut rdb);
    });
}

unsafe extern "C" fn free<T: NativeType>(value: *mut c_void) {
    // A panicking Drop is contained; the unwind still drops the value's fields
    // and frees the box. A second panic from one of those aborts.
    crate::panic::run_callback(
        "native type free",
        // SAFETY: Redis frees each value once, and every value is a Box<T>.
        || drop(unsafe { Box::from_raw(value.cast::<T>()) }),
        || (),
    );
}

unsafe extern "C" fn mem_usage<T: MemUsage>(value: *const c_void) -> usize {
    crate::panic::run_callback(
        "native type mem_usage",
        // SAFETY: Redis passes this type's own Box<T> value.
        || unsafe { &*value.cast::<T>() }.mem_usage(),
        || 0,
    )
}

unsafe extern "C" fn copy<T: CopyValue>(
    from_key: *mut raw::RedisModuleString,
    to_key: *mut raw::RedisModuleString,
    value: *const c_void,
) -> *mut c_void {
    crate::panic::run_callback(
        "native type copy",
        || {
            // SAFETY: Redis passes this type's own value and live key names.
            let value = unsafe { &*value.cast::<T>() };
            let (from_key, to_key) = unsafe { (key_bytes(from_key), key_bytes(to_key)) };
            value
                .copy_value(from_key, to_key)
                .map_or(ptr::null_mut(), |copy| Box::into_raw(Box::new(copy)).cast())
        },
        ptr::null_mut,
    )
}

unsafe extern "C" fn free_effort<T: FreeEffort>(
    key: *mut raw::RedisModuleString,
    value: *const c_void,
) -> usize {
    crate::panic::run_callback(
        "native type free_effort",
        // SAFETY: Redis passes this type's own value and a live key name.
        || unsafe { &*value.cast::<T>() }.free_effort(unsafe { key_bytes(key) }),
        || 1,
    )
}

unsafe extern "C" fn unlink<T: Unlink>(key: *mut raw::RedisModuleString, value: *const c_void) {
    crate::panic::run_callback(
        "native type unlink",
        // SAFETY: Redis passes this type's own value and a live key name.
        || unsafe { &*value.cast::<T>() }.unlink(unsafe { key_bytes(key) }),
        || (),
    );
}

unsafe extern "C" fn digest<T: DigestValue>(md: *mut raw::RedisModuleDigest, value: *mut c_void) {
    crate::panic::run_callback(
        "native type digest",
        || {
            let mut digest = Digest { md };
            // SAFETY: Redis passes this type's own Box<T> value.
            unsafe { &*value.cast::<T>() }.digest(&mut digest);
        },
        || (),
    );
}

unsafe extern "C" fn aof_rewrite<T: AofRewrite>(
    aof: *mut raw::RedisModuleIO,
    key: *mut raw::RedisModuleString,
    value: *mut c_void,
) {
    run_writer(aof, "aof_rewrite", || {
        let mut rewriter = AofRewriter { io: aof };
        // SAFETY: Redis passes this type's own value and a live key name.
        unsafe { &*value.cast::<T>() }.aof_rewrite(&mut rewriter, unsafe { key_bytes(key) });
    });
}

unsafe extern "C" fn aux_load<T: AuxData>(
    io: *mut raw::RedisModuleIO,
    encver: c_int,
    when: c_int,
) -> c_int {
    crate::panic::run_callback(
        "native type aux_load",
        || {
            // SAFETY: Redis passes the IO for this load callback.
            let mut rdb = unsafe { RdbReader::new(io) };
            match T::aux_load(&mut rdb, encver, AuxWhen::from_raw(when)) {
                Ok(()) => raw::REDISMODULE_OK as c_int,
                Err(error) => {
                    // SAFETY: io is the IO Redis passed to this callback.
                    unsafe {
                        log_io_error(
                            io,
                            RedisLogLevel::Warning,
                            &format!("native type aux_load failed: {error}"),
                        );
                    }
                    raw::REDISMODULE_ERR as c_int
                }
            }
        },
        || raw::REDISMODULE_ERR as c_int,
    )
}

unsafe extern "C" fn aux_save<T: AuxData>(io: *mut raw::RedisModuleIO, when: c_int) {
    run_writer(io, "aux_save", || {
        // SAFETY: Redis passes the IO for this save callback.
        let mut rdb = unsafe { RdbWriter::new(io) };
        T::aux_save(&mut rdb, AuxWhen::from_raw(when));
    });
}

unsafe extern "C" fn defrag<T: Defrag>(
    ctx: *mut raw::RedisModuleDefragCtx,
    key: *mut raw::RedisModuleString,
    value: *mut *mut c_void,
) -> c_int {
    crate::panic::run_callback(
        "native type defrag",
        || {
            // SAFETY: Redis passes a live defrag context, a key name and this
            // type's own value. The top-level box stays where it is.
            let defrag_ctx = unsafe { DefragContext::new(ctx) };
            let value = unsafe { &mut *(*value).cast::<T>() };
            match value.defrag(&defrag_ctx, unsafe { key_bytes(key) }) {
                DefragStatus::Done => 0,
                DefragStatus::More => 1,
            }
        },
        || 0,
    )
}
