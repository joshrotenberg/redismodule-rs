use bitflags::bitflags;
use redis_module_macros_internals::api;
use std::collections::{BTreeMap, HashMap};
use std::ffi::CString;
use std::os::raw::c_void;
use std::os::raw::{c_char, c_int, c_long, c_longlong};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::key::{KeyFlags, RedisKey, RedisKeyWritable};
use crate::logging::RedisLogLevel;
use crate::raw::Version;
use crate::raw::{
    add_info_begin_dict_field, add_info_end_dict_field, add_info_field_double,
    add_info_field_long_long, add_info_field_str, add_info_field_unsigned_long_long,
    add_info_section,
};
use crate::redisvalue::RedisValueKey;
use crate::{RedisError, RedisString, RedisValue};
use crate::{RedisResult, Status, raw};
use std::ops::Deref;

use std::ffi::CStr;

use self::call_reply::{CallResult, ErrorReply, PromiseCallReply, create_promise_call_reply};
use self::thread_safe::RedisLockIndicator;

mod call_user;
pub use call_user::CallUserScope;
mod registration;
mod views;
pub use views::{EventContext, RuntimeContext, WorkerContext};
pub(crate) mod timer;
pub use registration::RegistrationContext;

pub mod blocked;
pub mod call_reply;
pub mod commands;
pub mod defrag;
pub mod event_loop;
pub mod info;
pub mod key_cursor;
pub mod keys_cursor;
pub mod keyspace_events;
pub mod server_events;
pub mod thread_safe;

pub struct CallOptionsBuilder {
    options: String,
}

impl Default for CallOptionsBuilder {
    fn default() -> Self {
        CallOptionsBuilder {
            options: "v".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct CallOptions {
    options: CString,
}

#[derive(Clone)]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
pub struct BlockingCallOptions {
    options: CString,
}

/// The reply protocol selected by [`CallOptionsBuilder::resp`].
#[derive(Copy, Clone)]
pub enum CallOptionResp {
    /// RESP2 replies, the default for module calls.
    Resp2,
    /// RESP3 replies, such as a map for `HGETALL` (Redis 7.0 and later).
    Resp3,
    /// Match the protocol of the client attached to the call's context
    /// (Redis 7.0 and later).
    Auto,
}

/// Sets `errno` to 0.
fn clear_errno() {
    // SAFETY: each returns a pointer to this thread's errno.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    unsafe {
        *libc::__error() = 0;
    }
}

/// The error for a `RedisModule_Call` that returned null with `errno` set, as
/// its documentation lists them; `None` when `errno` is 0, as after a dry run
/// that would succeed.
fn call_failure(errno: Option<i32>) -> Option<String> {
    let reason = match errno? {
        0 => return None,
        libc::EBADF => "wrong format specifiers",
        libc::EINVAL => "wrong number of arguments",
        libc::ENOENT => "unknown command",
        libc::EPERM => "a key is in a slot this node does not serve",
        libc::EROFS => "a write in a read-only cluster state",
        libc::ENETDOWN => "the cluster is down",
        libc::ENOTSUP => "no ACL user for this context",
        libc::EACCES => "the ACL rules deny the command",
        libc::ENOSPC => "the command is not allowed: out of memory or not enough replicas",
        libc::ESPIPE => "the command is not allowed in script mode",
        other => {
            return Some(format!(
                "ERR RedisModule_Call failed: {}",
                std::io::Error::from_raw_os_error(other)
            ));
        }
    };
    Some(format!("ERR RedisModule_Call failed: {reason}"))
}

/// What Redis may do while a command yields with [`RuntimeContext::yield_now`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum YieldMode {
    /// Process file events, such as replication traffic and client
    /// connections, but no client commands. Time events, such as module
    /// timers and the server cron, do not run.
    Events,
    /// Also process client commands. Redis rejects most of them with `-BUSY`
    /// and runs the ones flagged `allow-busy`.
    Clients,
}

/// Parses an `INFO` line `redis_version:<major>.<minor>.<patch>`, where each
/// part is ASCII digits. The patch number may be followed by text that does
/// not start with a letter, digit or `_`, such as `-rc1`.
fn parse_redis_version_line(line: &str) -> Option<Version> {
    let rest = line.trim().strip_prefix("redis_version:")?;
    let (major, rest) = rest.split_once('.')?;
    let (minor, rest) = rest.split_once('.')?;
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (patch, suffix) = rest.split_at(digits);
    if suffix.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let number = |part: &str| -> Option<c_int> {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    Some(Version {
        major: number(major)?,
        minor: number(minor)?,
        patch: number(patch)?,
    })
}

impl CallOptionsBuilder {
    pub fn new() -> CallOptionsBuilder {
        Self::default()
    }

    fn add_flag(&mut self, flag: &str) {
        self.options.push_str(flag);
    }

    /// Enable this option will not allow RM_Call to perform write commands
    pub fn no_writes(mut self) -> CallOptionsBuilder {
        self.add_flag("W");
        self
    }

    /// Enable this option will run RM_Call is script mode.
    /// This mean that Redis will enable the following protections:
    /// 1. Not allow running dangerous commands like 'shutdown'
    /// 2. Not allow running write commands on OOM or if there are not enough good replica's connected
    pub fn script_mode(mut self) -> CallOptionsBuilder {
        self.add_flag("S");
        self
    }

    /// Enable this option will perform ACL validation on the user attached to the context that
    /// is used to invoke the call.
    pub fn verify_acl(mut self) -> CallOptionsBuilder {
        self.add_flag("C");
        self
    }

    /// Enable this option will OOM validation before running the command
    pub fn verify_oom(mut self) -> CallOptionsBuilder {
        self.add_flag("M");
        self
    }

    /// Enable this option will return error as CallReply object instead of setting errno (it is
    /// usually recommend to enable it)
    pub fn errors_as_replies(mut self) -> CallOptionsBuilder {
        self.add_flag("E");
        self
    }

    /// Enable this option will cause the command to be replicaed to the replica and AOF
    pub fn replicate(mut self) -> CallOptionsBuilder {
        self.add_flag("!");
        self
    }

    /// Replicate the command to replicas but not to the AOF. Includes
    /// [`Self::replicate`], and replaces [`Self::replicate_to_aof_only`].
    pub fn replicate_to_replicas_only(mut self) -> CallOptionsBuilder {
        self.options = self.options.replace('R', "");
        self.add_flag("!A");
        self
    }

    /// Replicate the command to the AOF but not to replicas. Includes
    /// [`Self::replicate`], and replaces [`Self::replicate_to_replicas_only`].
    pub fn replicate_to_aof_only(mut self) -> CallOptionsBuilder {
        self.options = self.options.replace('A', "");
        self.add_flag("!R");
        self
    }

    /// Check the command without running it (Redis 7.2 and later). The other
    /// options, such as ACL and OOM checks, still apply. A command that would
    /// run returns [`CallReply::Unknown`](crate::CallReply::Unknown); one that
    /// would fail returns the error it would return. An older Redis rejects the
    /// flag, and the call returns an error.
    pub fn dry_run(mut self) -> CallOptionsBuilder {
        self.add_flag("D");
        self
    }

    /// Selects the protocol of the command's call reply. The default is RESP2.
    /// Each selection replaces the previous one, including when switching back
    /// to RESP2. Auto follows the client attached to the call's context.
    /// Redis before 7.0 rejects the RESP3 and Auto call flags; selecting RESP2
    /// removes those flags even if an earlier selection requested them.
    ///
    /// ```
    /// use redis_module::{CallOptionResp, CallOptionsBuilder};
    /// let options = CallOptionsBuilder::new()
    ///     .resp(CallOptionResp::Resp3)
    ///     .resp(CallOptionResp::Resp2)
    ///     .build(); // Uses RESP2, the last selection.
    /// ```
    pub fn resp(mut self, resp: CallOptionResp) -> CallOptionsBuilder {
        self.options.retain(|flag| flag != '0' && flag != '3');
        match resp {
            CallOptionResp::Auto => self.add_flag("0"),
            CallOptionResp::Resp2 => (),
            CallOptionResp::Resp3 => self.add_flag("3"),
        }
        self
    }

    /// Construct a CallOption object that can be used to run commands using call_ext
    pub fn build(self) -> CallOptions {
        CallOptions {
            options: CString::new(self.options).unwrap(), // the data will never contains internal \0 so it is safe to unwrap.
        }
    }

    /// Construct a CallOption object that can be used to run commands using call_blocking.
    /// The commands can be either blocking or none blocking. In case the command are blocking
    /// (like `blpop`) a [`FutureCallReply`](crate::FutureCallReply) will be returned.
    #[cfg(any(
        feature = "min-redis-compatibility-version-8-0",
        feature = "min-redis-compatibility-version-7-4",
        feature = "min-redis-compatibility-version-7-2"
    ))]
    pub fn build_blocking(mut self) -> BlockingCallOptions {
        self.add_flag("K");
        BlockingCallOptions {
            options: CString::new(self.options).unwrap(), // the data will never contains internal \0 so it is safe to unwrap.
        }
    }
}

/// This struct allows logging when the Redis GIL is not acquired.
/// It is implemented `Send` and `Sync` so it can safely be used
/// from within different threads.
pub struct DetachedContext {
    pub(crate) ctx: AtomicPtr<raw::RedisModuleCtx>,
}

impl DetachedContext {
    pub const fn new() -> Self {
        DetachedContext {
            ctx: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

impl Default for DetachedContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A worker view borrowed while [`DetachedContext`] holds the Redis lock.
/// Dropping the guard unlocks Redis. It dereferences to [`WorkerContext`], which
/// has shared runtime operations and no client replies or registration APIs.
pub struct DetachedContextGuard<'context> {
    pub(crate) ctx: WorkerContext,
    _context: std::marker::PhantomData<&'context DetachedContext>,
}

unsafe impl RedisLockIndicator for DetachedContextGuard<'_> {}

impl Drop for DetachedContextGuard<'_> {
    fn drop(&mut self) {
        self.ctx.release_call_users();
        unsafe {
            crate::__required_api!(RedisModule_ThreadSafeContextUnlock)(self.ctx.ctx);
        };
    }
}

impl Deref for DetachedContextGuard<'_> {
    type Target = WorkerContext;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

impl DetachedContext {
    /// Logs `message` at `level` with the module name.
    /// Embedded NUL bytes are written as `\0`, which reads the same as a literal
    /// backslash and zero; other text is passed literally.
    pub fn log(&self, level: RedisLogLevel, message: &str) {
        let c = self.ctx.load(Ordering::Relaxed);
        crate::logging::log_internal(c, level, message);
    }

    /// Logs `message` at debug level, like [`Self::log`].
    pub fn log_debug(&self, message: &str) {
        self.log(RedisLogLevel::Debug, message);
    }

    /// Logs `message` at notice level, like [`Self::log`].
    pub fn log_notice(&self, message: &str) {
        self.log(RedisLogLevel::Notice, message);
    }

    /// Logs `message` at verbose level, like [`Self::log`].
    pub fn log_verbose(&self, message: &str) {
        self.log(RedisLogLevel::Verbose, message);
    }

    /// Logs `message` at warning level, like [`Self::log`].
    pub fn log_warning(&self, message: &str) {
        self.log(RedisLogLevel::Warning, message);
    }

    pub fn set_context(&self, ctx: &EventContext) -> Result<(), RedisError> {
        let c = self.ctx.load(Ordering::Relaxed);
        if !c.is_null() {
            return Err(RedisError::Str("Detached context is already set"));
        }
        let ctx =
            unsafe { crate::__available_api!(RedisModule_GetDetachedThreadSafeContext)(ctx.ctx) };
        self.ctx.store(ctx, Ordering::Relaxed);
        Ok(())
    }

    /// Lock Redis for command invocation. Returns [DetachedContextGuard] which will unlock Redis when dispose.
    /// The guard dereferences to [`WorkerContext`], then [`RuntimeContext`].
    /// Locking Redis when Redis is already locked by the current thread is left unspecified.
    /// However, this function will not return on the second call (it might panic or deadlock, for example)..
    pub fn lock(&self) -> DetachedContextGuard<'_> {
        let c = self.ctx.load(Ordering::Relaxed);
        assert!(!c.is_null(), "Detached context has not been initialized");
        unsafe { crate::__required_api!(RedisModule_ThreadSafeContextLock)(c) };
        let ctx = unsafe { WorkerContext::from_module_context(c) };
        DetachedContextGuard {
            ctx,
            _context: std::marker::PhantomData,
        }
    }
}

unsafe impl Send for DetachedContext {}
unsafe impl Sync for DetachedContext {}

/// A client command invocation, with replies, key-position discovery, and
/// client blocking in addition to [`EventContext`] and [`RuntimeContext`].
/// SDK command handlers receive this type. Event callbacks and worker guards
/// cannot be converted to it by safe code.
///
/// ```compile_fail,E0451
/// fn forge(event: redis_module::EventContext) -> redis_module::Context {
///     redis_module::Context { event }
/// }
/// ```
#[derive(Debug)]
pub struct Context {
    event: EventContext,
}

impl Deref for Context {
    type Target = EventContext;
    fn deref(&self) -> &Self::Target {
        &self.event
    }
}

struct SelectedDbGuard<'ctx> {
    ctx: &'ctx RuntimeContext,
    previous: i32,
}

impl Drop for SelectedDbGuard<'_> {
    fn drop(&mut self) {
        // The previous database was valid before the scope, and Redis's
        // configured database count cannot change while the server runs.
        // Do not introduce a second panic while unwinding the callback.
        let _ = self.ctx.select_db(self.previous);
    }
}

pub struct StrCallArgs<'a> {
    is_owner: bool,
    args: Vec<*mut raw::RedisModuleString>,
    // Phantom is used to make sure the object will not live longer than actual arguments slice
    phantom: std::marker::PhantomData<&'a raw::RedisModuleString>,
}

impl<'a> Drop for StrCallArgs<'a> {
    fn drop(&mut self) {
        if self.is_owner {
            self.args.iter_mut().for_each(|v| unsafe {
                crate::__required_api!(RedisModule_FreeString)(std::ptr::null_mut(), *v)
            });
        }
    }
}

impl<'a, T: AsRef<[u8]> + ?Sized> From<&'a [&T]> for StrCallArgs<'a> {
    fn from(vals: &'a [&T]) -> Self {
        StrCallArgs {
            is_owner: true,
            args: vals
                .iter()
                .map(|v| RedisString::create_from_slice(std::ptr::null_mut(), v.as_ref()).take())
                .collect(),
            phantom: std::marker::PhantomData,
        }
    }
}

impl<'a> From<&'a [&RedisString]> for StrCallArgs<'a> {
    fn from(vals: &'a [&RedisString]) -> Self {
        StrCallArgs {
            is_owner: false,
            args: vals.iter().map(|v| v.as_ptr()).collect(),
            phantom: std::marker::PhantomData,
        }
    }
}

impl<'a, const SIZE: usize, T: ?Sized> From<&'a [&T; SIZE]> for StrCallArgs<'a>
where
    for<'b> &'a [&'b T]: Into<StrCallArgs<'a>>,
{
    fn from(vals: &'a [&T; SIZE]) -> Self {
        vals.as_ref().into()
    }
}

impl<'a> StrCallArgs<'a> {
    pub(crate) fn args_mut(&mut self) -> &mut [*mut raw::RedisModuleString] {
        &mut self.args
    }
}

impl Context {
    /// Runs a call-user scope while preserving client capabilities.
    /// See [`RuntimeContext::with_call_user`] for restoration and error behavior.
    pub fn with_call_user<T>(
        &self,
        user_name: &RedisString,
        f: impl FnOnce(&Context) -> T,
    ) -> Result<T, RedisError> {
        self.event.with_call_user(user_name, |_| f(self))
    }

    /// Wraps a module command or blocked-client reply callback.
    ///
    /// # Safety
    ///
    /// `ctx` must have a live association with this module and a client reply
    /// target (or a command key-position request). The Redis lock must be held
    /// on this thread. This wrapper and references derived from it must only
    /// be used while the native invocation is valid. Use [`EventContext::new`]
    /// for callbacks without a reply target and [`RuntimeContext::new`] for
    /// locked contexts without a module association.
    pub const unsafe fn new(ctx: *mut raw::RedisModuleCtx) -> Self {
        Self {
            event: unsafe { EventContext::new(ctx) },
        }
    }

    /// Alias for [`Self::new`].
    ///
    /// # Safety
    ///
    /// The pointer must satisfy every requirement of [`Self::new`].
    pub const unsafe fn from_module_context(ctx: *mut raw::RedisModuleCtx) -> Self {
        unsafe { Self::new(ctx) }
    }

    /// Parses the Redis version from an INFO response.
    pub fn version_from_info(info: RedisValue) -> Result<Version, RedisError> {
        RuntimeContext::version_from_info(info)
    }

    /// Runs a database scope while preserving client command capabilities.
    pub fn with_db<T>(&self, db: i32, f: impl FnOnce(&Context) -> T) -> Result<T, RedisError> {
        self.event.with_db(db, |_| f(self))
    }

    /// # Panics
    ///
    /// Will panic if `RedisModule_IsKeysPositionRequest` is missing in redismodule.h
    #[must_use]
    pub fn is_keys_position_request(&self) -> bool {
        // We want this to be available in tests where we don't have an actual Redis to call
        if cfg!(test) {
            return false;
        }

        (unsafe { crate::__required_api!(RedisModule_IsKeysPositionRequest)(self.ctx) }) != 0
    }

    /// # Panics
    ///
    /// Will panic if `RedisModule_KeyAtPos` is missing in redismodule.h
    pub fn key_at_pos(&self, pos: i32) {
        // TODO: This will crash redis if `pos` is out of range.
        // Think of a way to make this safe by checking the range.
        unsafe {
            crate::__required_api!(RedisModule_KeyAtPos)(self.ctx, pos as c_int);
        }
    }

    /// Converts text for a simple string or error reply, which cannot carry CR,
    /// LF or NUL, by replacing each of those bytes with a space. Other text,
    /// non-ASCII included, is kept: no UTF-8 multibyte sequence contains them.
    #[must_use]
    pub fn str_as_legal_resp_string(s: &str) -> CString {
        CString::new(
            s.bytes()
                .map(|b| match b {
                    b'\r' | b'\n' | b'\0' => b' ',
                    b => b,
                })
                .collect::<Vec<_>>(),
        )
        .expect("NUL bytes were replaced")
    }

    pub fn replicate_verbatim(&self) {
        raw::replicate_verbatim(self.ctx);
    }

    /// Replies to the client with `result` exactly as Redis sent it to the
    /// call: a simple string stays a simple string, and an error reply is
    /// passed on as an error. Uses `RedisModule_ReplyWithCallReply` for replies,
    /// and an error reply for an error message the crate built.
    ///
    /// # Errors
    ///
    /// Returns an error, without replying, if `result` holds no reply (such as
    /// the result of a dry run), if Redis cannot send it to this client (RESP3
    /// types to a RESP2 client), or if the running Redis lacks the API.
    pub fn reply_with_call_reply(&self, result: &CallResult) -> Result<(), RedisError> {
        let reply_with_call_reply =
            unsafe { crate::__available_api!(RedisModule_ReplyWithCallReply) };
        let reply = match result {
            Ok(reply) => reply.get_raw(),
            Err(ErrorReply::RedisError(error)) => Some(error.get_raw()),
            Err(ErrorReply::Message(message)) => {
                self.reply_error_string(message);
                return Ok(());
            }
        };
        let reply = reply.ok_or(RedisError::Str("ERR the call result holds no reply"))?;
        if unsafe { reply_with_call_reply(self.ctx, reply) } != raw::REDISMODULE_OK as c_int {
            return Err(RedisError::Str(
                "ERR Redis cannot send this reply to the client, for example RESP3 types to a RESP2 client",
            ));
        }
        Ok(())
    }

    pub fn current_command_name(&self) -> Result<String, RedisError> {
        unsafe {
            match raw::RedisModule_GetCurrentCommandName {
                Some(cmd) => {
                    let name = cmd(self.ctx);
                    if name.is_null() {
                        return Err(RedisError::Str("ERR no current command"));
                    }
                    Ok(CStr::from_ptr(name).to_string_lossy().into_owned())
                }
                None => Err(RedisError::Str(
                    "API RedisModule_GetCurrentCommandName is not available",
                )),
            }
        }
    }

    /// Return the current user name attached to a legal native user-query context.
    /// Redis 8.10 can return None when the native API reports no user. Older
    /// hosts require a client/user; Option does not make arbitrary constructed
    /// or missing-client contexts legal. A present string is independently owned.
    pub fn get_current_user(&self) -> Option<RedisString> {
        let user = unsafe { crate::__required_api!(RedisModule_GetCurrentUserName)(self.ctx) };
        // SAFETY: native result is owned; self.ctx is its actual producer.
        unsafe { RedisString::from_owned_ptr_or_none(NonNull::new(self.ctx), user) }
    }

    /// Adds an entry to the ACL log, which `ACL LOG` lists, for the user named
    /// `user_name` and the denied `object` (a command, key or channel name), as
    /// `RedisModule_ACLAddLogEntryByUserName` does. The entry records the
    /// client of this context, so call it from a command or another context
    /// with a client.
    ///
    /// # Errors
    ///
    /// Returns an error if the context has no client, or the running Redis does
    /// not provide the API (before 7.2).
    pub fn acl_add_log_entry(
        &self,
        user_name: &RedisString,
        object: &RedisString,
        reason: AclLogReason,
    ) -> Result<(), RedisError> {
        let add_log_entry =
            unsafe { crate::__available_api!(RedisModule_ACLAddLogEntryByUserName) };
        let client_id = unsafe { crate::__available_api!(RedisModule_GetClientId) };
        // Redis records the context's client in the entry, and crashes without one.
        if unsafe { client_id(self.ctx) } == 0 {
            return Err(RedisError::Str(
                "ERR an ACL log entry needs a context with a client",
            ));
        }
        // The vendored header declares no result. Redis fails only for an
        // unknown reason, and every AclLogReason is a known one.
        unsafe { add_log_entry(self.ctx, user_name.as_ptr(), object.as_ptr(), reason.into()) };
        Ok(())
    }

    /// Sends a reply to this invocation's client.
    pub fn reply_simple_string(&self, s: &str) -> raw::Status {
        ReplyContext { ctx: self.ctx }.reply_simple_string(s)
    }

    /// Sends a reply to this invocation's client.
    pub fn reply_error_string(&self, s: &str) -> raw::Status {
        ReplyContext { ctx: self.ctx }.reply_error_string(s)
    }

    /// Sends a reply to this invocation's client.
    pub fn reply_with_key(&self, result: RedisValueKey) -> raw::Status {
        ReplyContext { ctx: self.ctx }.reply_with_key(result)
    }

    /// Sends a reply to this invocation's client.
    pub fn reply(&self, result: RedisResult) -> raw::Status {
        ReplyContext { ctx: self.ctx }.reply(result)
    }
}

impl RuntimeContext {
    /// Logs `message` at `level` with the module name.
    /// Embedded NUL bytes are written as `\0`, which reads the same as a literal
    /// backslash and zero; other text is passed literally.
    pub fn log(&self, level: RedisLogLevel, message: &str) {
        crate::logging::log_internal(self.ctx, level, message);
    }

    /// Logs `message` at debug level, like [`Self::log`].
    pub fn log_debug(&self, message: &str) {
        self.log(RedisLogLevel::Debug, message);
    }

    /// Logs `message` at notice level, like [`Self::log`].
    pub fn log_notice(&self, message: &str) {
        self.log(RedisLogLevel::Notice, message);
    }

    /// Logs `message` at verbose level, like [`Self::log`].
    pub fn log_verbose(&self, message: &str) {
        self.log(RedisLogLevel::Verbose, message);
    }

    /// Logs `message` at warning level, like [`Self::log`].
    pub fn log_warning(&self, message: &str) {
        self.log(RedisLogLevel::Warning, message);
    }

    /// # Panics
    ///
    /// Will panic if `RedisModule_AutoMemory` is missing in redismodule.h
    pub fn auto_memory(&self) {
        unsafe {
            crate::__required_api!(RedisModule_AutoMemory)(self.ctx);
        }
    }

    fn call_internal<
        'ctx,
        'a,
        T: Into<StrCallArgs<'a>>,
        R: From<PromiseCallReply<'static, 'ctx>>,
    >(
        &'ctx self,
        command: &str,
        fmt: *const c_char,
        args: T,
    ) -> R {
        let mut call_args: StrCallArgs = args.into();
        let final_args = call_args.args_mut();

        let cmd = CString::new(command).unwrap();
        // RedisModule_Call returns null both for a dry run that would succeed
        // and, without the E flag, for a call that failed; errno tells them
        // apart, so clear it first.
        clear_errno();
        let reply: *mut raw::RedisModuleCallReply = unsafe {
            let p_call = crate::__required_api!(RedisModule_Call);
            p_call(
                self.ctx,
                cmd.as_ptr(),
                fmt,
                final_args.as_mut_ptr(),
                final_args.len(),
            )
        };
        if reply.is_null() {
            if let Some(error) = call_failure(std::io::Error::last_os_error().raw_os_error()) {
                return R::from(PromiseCallReply::Resolved(Err(ErrorReply::Message(error))));
            }
        }
        let promise = create_promise_call_reply(self, NonNull::new(reply));
        R::from(promise)
    }

    /// Call a command and copy its result into an owned value.
    ///
    /// A simple-string reply, such as `+OK`, is a `SimpleString`. A bulk-string
    /// reply is bulk text for valid UTF-8, or a byte buffer otherwise. Use
    /// `call_ext` for typed reply access and borrow bytes from that reply's
    /// owner, and [`Context::reply_with_call_reply`] to pass a reply on as it is.
    pub fn call<'a, T: Into<StrCallArgs<'a>>>(&self, command: &str, args: T) -> RedisResult {
        self.call_internal::<_, CallResult>(command, raw::FMT, args)
            .map_or_else(|e| Err(e.into()), |v| Ok((&v).into()))
    }

    /// Invoke a command on Redis and return the result
    /// Unlike 'call' this API also allow to pass a CallOption to control different aspects
    /// of the command invocation.
    pub fn call_ext<'a, T: Into<StrCallArgs<'a>>, R: From<CallResult<'static>>>(
        &self,
        command: &str,
        options: &CallOptions,
        args: T,
    ) -> R {
        let res: CallResult<'static> =
            self.call_internal(command, options.options.as_ptr() as *const c_char, args);
        R::from(res)
    }

    #[must_use]
    pub fn open_key(&self, key: &RedisString) -> RedisKey {
        // SAFETY: RuntimeContext construction requires a valid context and lock, so
        // self.ctx is valid and the lock is held now. The key's type does not
        // tie it to the Context or the lock; callers must not keep it past
        // them; borrowing native resources for their full lifetime is separate work.
        unsafe { RedisKey::open(self.ctx, key) }
    }

    #[must_use]
    pub fn open_key_with_flags(&self, key: &RedisString, flags: KeyFlags) -> RedisKey {
        // SAFETY: as for open_key.
        unsafe { RedisKey::open_with_flags(self.ctx, key, flags) }
    }

    #[must_use]
    pub fn open_key_writable(&self, key: &RedisString) -> RedisKeyWritable {
        // SAFETY: as for open_key.
        unsafe { RedisKeyWritable::open(self.ctx, key) }
    }

    #[must_use]
    pub fn open_key_writable_with_flags(
        &self,
        key: &RedisString,
        flags: KeyFlags,
    ) -> RedisKeyWritable {
        // SAFETY: as for open_key.
        unsafe { RedisKeyWritable::open_with_flags(self.ctx, key, flags) }
    }

    /// Replicate command to the replica and AOF.
    pub fn replicate<'a, T: Into<StrCallArgs<'a>>>(&self, command: &str, args: T) {
        raw::replicate(self.ctx, command, args);
    }

    /// How much memory Redis uses relative to its `maxmemory` setting, through
    /// `RedisModule_GetUsedMemoryRatio`: 0 when no limit is set, 1 at the
    /// limit, and more than 1 above it.
    ///
    /// # Errors
    ///
    /// Returns an error if the running Redis does not provide the API.
    pub fn used_memory_ratio(&self) -> Result<f32, RedisError> {
        let ratio = unsafe { crate::__available_api!(RedisModule_GetUsedMemoryRatio) };
        Ok(unsafe { ratio() })
    }

    #[must_use]
    pub fn create_string<T: Into<Vec<u8>>>(&self, s: T) -> RedisString {
        RedisString::create(NonNull::new(self.ctx), s)
    }

    #[must_use]
    pub const fn get_raw(&self) -> *mut raw::RedisModuleCtx {
        self.ctx
    }

    /// Lets Redis process events during a long-running command, through
    /// `RedisModule_Yield`. Call it periodically; Redis limits how often a call
    /// does any work, and does none until the command has run for
    /// `busy-reply-threshold`. It also works on a thread-safe context while its
    /// lock is held.
    ///
    /// Module callbacks can run inside this call, for example the module's own
    /// `allow-busy` commands or a server event handler. Do not hold a lock that
    /// such a callback takes across the call: the main thread would wait on
    /// itself.
    ///
    /// `busy_reply` replaces the text after `-BUSY` in the replies that other
    /// clients get under [`YieldMode::Clients`]. CR, LF and NUL in it are
    /// replaced with spaces.
    ///
    /// # Errors
    ///
    /// Returns an error if the running Redis does not provide
    /// `RedisModule_Yield` (before 7.0).
    pub fn yield_now(&self, mode: YieldMode, busy_reply: Option<&str>) -> Result<(), RedisError> {
        let yield_api = unsafe { crate::__available_api!(RedisModule_Yield) };
        let flags = match mode {
            YieldMode::Events => raw::REDISMODULE_YIELD_FLAG_NONE,
            YieldMode::Clients => raw::REDISMODULE_YIELD_FLAG_CLIENTS,
        };
        let busy_reply = busy_reply.map(Context::str_as_legal_resp_string);
        let busy_reply = busy_reply
            .as_ref()
            .map_or(ptr::null(), |text| text.as_ptr());
        // SAFETY: Redis reads busy_reply only during the call.
        unsafe { yield_api(self.ctx, flags as c_int, busy_reply) };
        Ok(())
    }

    /// Fires a keyspace notification for `keyname`.
    ///
    /// Returns [`Status::Err`] if the event contains a NUL byte or Redis rejects
    /// the notification. See [`Self::notify_keyspace_event_bytes`] for byte
    /// event names and detailed errors.
    #[allow(clippy::must_use_candidate)]
    pub fn notify_keyspace_event(
        &self,
        event_type: raw::NotifyEvent,
        event: &str,
        keyname: &RedisString,
    ) -> raw::Status {
        self.notify_keyspace_event_bytes(event_type, event.as_bytes(), keyname)
            .map_or(Status::Err, |()| Status::Ok)
    }

    /// Fires a keyspace notification with an event name that need not be UTF-8.
    ///
    /// Redis takes a C string, so interior NUL bytes are rejected before the
    /// notification is sent. Empty names are permitted. Existing event handlers
    /// still receive a lossily decoded string through `redis_event_handler!`.
    pub fn notify_keyspace_event_bytes(
        &self,
        event_type: raw::NotifyEvent,
        event: &[u8],
        keyname: &RedisString,
    ) -> Result<(), RedisError> {
        let event = CString::new(event)
            .map_err(|_| RedisError::Str("ERR keyspace event name contains a NUL byte"))?;
        let status: Status = unsafe {
            crate::__required_api!(RedisModule_NotifyKeyspaceEvent)(
                self.ctx,
                event_type.bits(),
                event.as_ptr(),
                keyname.as_ptr(),
            )
        }
        .into();
        match status {
            Status::Ok => Ok(()),
            Status::Err => Err(RedisError::Str("ERR keyspace notification failed")),
        }
    }

    /// Returns the redis version either by calling `RedisModule_GetServerVersion` API,
    /// Or if it is not available, by calling "info server" API and parsing the reply
    pub fn get_redis_version(&self) -> Result<Version, RedisError> {
        self.get_redis_version_internal(false)
    }

    /// Returns the redis version by calling "info server" API and parsing the reply
    pub fn get_redis_version_rm_call(&self) -> Result<Version, RedisError> {
        self.get_redis_version_internal(true)
    }

    pub fn version_from_info(info: RedisValue) -> Result<Version, RedisError> {
        if let RedisValue::SimpleString(info_str) | RedisValue::BulkString(info_str) = info {
            if let Some(version) = info_str.lines().find_map(parse_redis_version_line) {
                return Ok(version);
            }
        }
        Err(RedisError::Str("Error getting redis_version"))
    }

    fn get_redis_version_internal(&self, force_use_rm_call: bool) -> Result<Version, RedisError> {
        match unsafe { raw::RedisModule_GetServerVersion } {
            Some(api) if !force_use_rm_call => {
                // Call existing API
                Ok(Version::from(unsafe { api() }))
            }
            _ => {
                // Call "info server"
                if let Ok(info) = self.call("info", &["server"]) {
                    Self::version_from_info(info)
                } else {
                    Err(RedisError::Str("Error calling \"info server\""))
                }
            }
        }
    }

    /// Return ContextFlags object that allows to check properties related to the state of
    /// the current Redis instance such as:
    /// * Role (master/slave)
    /// * Loading RDB/AOF
    /// * Execution mode such as multi exec or Lua
    pub fn get_flags(&self) -> ContextFlags {
        ContextFlags::from_bits_truncate(unsafe {
            crate::__required_api!(RedisModule_GetContextFlags)(self.ctx)
        })
    }

    /// Returns the database selected on this context.
    #[must_use]
    pub fn get_selected_db(&self) -> i32 {
        unsafe { crate::__required_api!(RedisModule_GetSelectedDb)(self.ctx) }
    }

    /// Selects a database, returning an error if `db` is out of range.
    ///
    /// For a client context, the client retains this selection after the
    /// module command returns. Use [`Self::with_db`] for temporary access.
    pub fn select_db(&self, db: i32) -> Result<(), RedisError> {
        let status: Status =
            unsafe { crate::__required_api!(RedisModule_SelectDb)(self.ctx, db) }.into();
        match status {
            Status::Ok => Ok(()),
            Status::Err => Err(RedisError::Str("ERR DB index is out of range")),
        }
    }

    /// Runs `f` in `db`, restoring the original database when `f` returns or
    /// unwinds. Scopes may be nested. An invalid database returns an error
    /// without running `f` or changing the current selection.
    ///
    /// The closure's value is returned unchanged, including a `Result` value.
    /// Selection covers only the synchronous execution of `f`. Drop any
    /// database-bound resources opened inside the closure before it returns.
    ///
    /// ```no_run
    /// use redis_module::{Context, RedisResult};
    /// fn read_from_db(ctx: &Context, db: i32, key: &str) -> RedisResult {
    ///     ctx.with_db(db, |ctx| ctx.call("GET", &[key]))?
    /// }
    /// ```
    pub fn with_db<T>(
        &self,
        db: i32,
        f: impl FnOnce(&RuntimeContext) -> T,
    ) -> Result<T, RedisError> {
        let previous = self.get_selected_db();
        self.select_db(db)?;
        let _restore = SelectedDbGuard {
            ctx: self,
            previous,
        };
        Ok(f(self))
    }

    /// Returns the number of keys in the currently selected database.
    ///
    /// # Panics
    ///
    /// Panics if the running Redis does not provide `RedisModule_DbSize`.
    pub fn dbsize(&self) -> u64 {
        unsafe { crate::__required_api!(RedisModule_DbSize)(self.ctx) }
    }

    /// Runs `check` with the ACL user named `user_name`, and returns its result
    /// with the `errno` it left.
    ///
    /// Callers look up the API that `check` uses before calling this, so a
    /// missing API cannot leave the user allocated.
    fn with_acl_user(
        &self,
        user_name: &RedisString,
        check: impl FnOnce(*mut raw::RedisModuleUser) -> c_int,
    ) -> Result<(c_int, Option<i32>), RedisError> {
        let get_user = unsafe { crate::__available_api!(RedisModule_GetModuleUserFromUserName) };
        let free_user = unsafe { crate::__available_api!(RedisModule_FreeModuleUser) };
        let user = unsafe { get_user(user_name.as_ptr()) };
        if user.is_null() {
            return Err(RedisError::Str(
                "ERR the user does not exist or is disabled",
            ));
        }
        let status = check(user);
        let errno = std::io::Error::last_os_error().raw_os_error();
        unsafe { free_user(user) };
        Ok((status, errno))
    }

    /// Verify the the given user has the give ACL permission on the given key.
    /// Return Ok(()) if the user has the permissions or error (with relevant error message)
    /// if the validation failed.
    pub fn acl_check_key_permission(
        &self,
        user_name: &RedisString,
        key_name: &RedisString,
        permissions: &AclPermissions,
    ) -> Result<(), RedisError> {
        let check_key_permissions =
            unsafe { crate::__available_api!(RedisModule_ACLCheckKeyPermissions) };
        let (status, _) = self.with_acl_user(user_name, |user| unsafe {
            check_key_permissions(user, key_name.as_ptr(), permissions.bits())
        })?;
        if status == raw::REDISMODULE_OK as c_int {
            Ok(())
        } else {
            Err(RedisError::Str(
                "NOPERM User does not have permissions on the key",
            ))
        }
    }

    /// The positions in `args`, which start with the command name, of the keys
    /// the command would use, as Redis finds them for cluster routing and ACL
    /// checks (`RedisModule_GetCommandKeysWithFlags`, Redis 7.0). The key
    /// flags that API also gives are not returned.
    ///
    /// Redis checks whether a command has keys before its arity, so a command
    /// without keys gives an empty list even with the wrong number of
    /// arguments.
    ///
    /// # Errors
    ///
    /// Returns an error if `args` is empty, the command is unknown, `args` do
    /// not fit the arity of a command with keys, or the running Redis does not
    /// provide the API.
    pub fn get_command_keys(&self, args: &[RedisString]) -> Result<Vec<usize>, RedisError> {
        let get_keys = unsafe { crate::__available_api!(RedisModule_GetCommandKeysWithFlags) };
        // Redis reads the command name without checking the count.
        if args.is_empty() {
            return Err(RedisError::Str("ERR no command to look up"));
        }
        let argc = c_int::try_from(args.len())
            .map_err(|_| RedisError::Str("ERR too many command arguments"))?;
        let mut argv: Vec<*mut raw::RedisModuleString> =
            args.iter().map(RedisString::as_ptr).collect();
        let mut num_keys: c_int = 0;
        // SAFETY: argv holds live strings for the call, and out_flags may be
        // null. Redis allocates the returned positions, freed below.
        let positions = unsafe {
            get_keys(
                self.ctx,
                argv.as_mut_ptr(),
                argc,
                &mut num_keys,
                ptr::null_mut(),
            )
        };
        if positions.is_null() {
            // Null with errno 0 means a command without keys.
            return match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ENOENT) => Err(RedisError::Str("ERR unknown command")),
                Some(libc::EINVAL) => Err(RedisError::Str(
                    "ERR wrong number of arguments for the command",
                )),
                _ => Ok(Vec::new()),
            };
        }
        let len = usize::try_from(num_keys).unwrap_or(0);
        // SAFETY: Redis returned num_keys positions.
        let keys = unsafe { std::slice::from_raw_parts(positions, len) }
            .iter()
            .filter_map(|&pos| usize::try_from(pos).ok())
            .collect();
        unsafe { crate::__required_api!(RedisModule_Free)(positions.cast()) };
        Ok(keys)
    }

    /// Checks whether the user named `user_name` may run the command in `args`,
    /// its name followed by its arguments, as `RedisModule_ACLCheckCommandPermissions`
    /// does. The arguments matter for the key permissions of the command and for
    /// rules on subcommands.
    ///
    /// # Errors
    ///
    /// Returns an error starting with `NOPERM` if the user may not run the
    /// command, and an error if the user does not exist or is disabled, `args`
    /// is empty, the command does not exist, or the running Redis does not
    /// provide the API (before 7.0).
    pub fn acl_check_command_permission(
        &self,
        user_name: &RedisString,
        args: &[RedisString],
    ) -> Result<(), RedisError> {
        let check_command_permissions =
            unsafe { crate::__available_api!(RedisModule_ACLCheckCommandPermissions) };
        if args.is_empty() {
            return Err(RedisError::Str("ERR no command to check"));
        }
        let argc = c_int::try_from(args.len())
            .map_err(|_| RedisError::Str("ERR too many command arguments"))?;
        let mut argv: Vec<*mut raw::RedisModuleString> =
            args.iter().map(RedisString::as_ptr).collect();
        let (status, errno) = self.with_acl_user(user_name, |user| unsafe {
            check_command_permissions(user, argv.as_mut_ptr(), argc)
        })?;
        match (status == raw::REDISMODULE_OK as c_int, errno) {
            (true, _) => Ok(()),
            (false, Some(libc::ENOENT)) => Err(RedisError::Str("ERR unknown command")),
            (false, _) => Err(RedisError::Str(
                "NOPERM User does not have permissions to run the command",
            )),
        }
    }

    /// Checks whether the user named `user_name` may access the pub/sub
    /// `channel` as `permissions` describe, as
    /// `RedisModule_ACLCheckChannelPermissions` does. Unsubscribing is always
    /// allowed.
    ///
    /// # Errors
    ///
    /// Returns an error starting with `NOPERM` if the user may not access the
    /// channel, and an error if the user does not exist or is disabled, or the
    /// running Redis does not provide the API (before 7.0).
    pub fn acl_check_channel_permission(
        &self,
        user_name: &RedisString,
        channel: &RedisString,
        permissions: AclChannelPermissions,
    ) -> Result<(), RedisError> {
        let check_channel_permissions =
            unsafe { crate::__available_api!(RedisModule_ACLCheckChannelPermissions) };
        // Redis sets errno for invalid flags but not for a denial, so the
        // flags are checked here and errno is not read.
        if !AclChannelPermissions::all().contains(permissions) {
            return Err(RedisError::Str("ERR invalid channel permission flags"));
        }
        let (status, _) = self.with_acl_user(user_name, |user| unsafe {
            check_channel_permissions(user, channel.as_ptr(), permissions.bits())
        })?;
        if status == raw::REDISMODULE_OK as c_int {
            Ok(())
        } else {
            Err(RedisError::Str(
                "NOPERM User does not have permissions on the channel",
            ))
        }
    }

    api!(
        [RedisModule_AvoidReplicaTraffic],
        /// Returns true if a client sent the CLIENT PAUSE command to the server or
        /// if Redis Cluster does a manual failover, pausing the clients.
        /// This is needed when we have a master with replicas, and want to write,
        /// without adding further data to the replication channel, that the replicas
        /// replication offset, match the one of the master. When this happens, it is
        /// safe to failover the master without data loss.
        ///
        /// However modules may generate traffic by calling commands or directly send
        /// data to the replication stream.
        ///
        /// So modules may want to try to avoid very heavy background work that has
        /// the effect of creating data to the replication channel, when this function
        /// returns true. This is mostly useful for modules that have background
        /// garbage collection tasks, or that do writes and replicate such writes
        /// periodically in timer callbacks or other periodic callbacks.
        pub fn avoid_replication_traffic(&self) -> bool {
            unsafe { RedisModule_AvoidReplicaTraffic() == 1 }
        }
    );

    /// Return [Ok(true)] is the current Redis deployment is enterprise, otherwise [Ok(false)].
    /// Return error in case it was not possible to determind the deployment.
    fn is_enterprise_internal(&self) -> Result<bool, RedisError> {
        let info_res = self.call("info", &["server"])?;
        Self::enterprise_from_info(&info_res)
    }

    fn enterprise_from_info(info_res: &RedisValue) -> Result<bool, RedisError> {
        let info = match info_res {
            RedisValue::BulkRedisString(res) => res.try_as_str()?,
            RedisValue::SimpleString(res) | RedisValue::BulkString(res) => res.as_str(),
            _ => return Err(RedisError::Str("Mismatch call reply type")),
        };
        Ok(info.contains("rlec_version:"))
    }

    /// Return `true` is the current Redis deployment is enterprise, otherwise `false`.
    pub fn is_enterprise(&self) -> bool {
        self.is_enterprise_internal().unwrap_or_else(|e| {
            log::error!("Failed getting deployment type, assuming oss. Error: {e}.");
            false
        })
    }
}

impl EventContext {
    /// Like [`RuntimeContext::call_ext`], allowing blocking commands such as BLPOP.
    #[cfg(any(
        feature = "min-redis-compatibility-version-8-0",
        feature = "min-redis-compatibility-version-7-4",
        feature = "min-redis-compatibility-version-7-2"
    ))]
    pub fn call_blocking<
        'ctx,
        'a,
        T: Into<StrCallArgs<'a>>,
        R: From<PromiseCallReply<'static, 'ctx>>,
    >(
        &'ctx self,
        command: &str,
        options: &BlockingCallOptions,
        args: T,
    ) -> R {
        self.call_internal(command, options.options.as_ptr() as *const c_char, args)
    }

    /// # Safety
    ///
    /// See [raw::export_shared_api].
    pub unsafe fn export_shared_api(
        &self,
        func: *const ::std::os::raw::c_void,
        name: *const ::std::os::raw::c_char,
    ) -> raw::Status {
        unsafe { raw::export_shared_api(self.ctx, func, name) }
    }

    api!(
        [RedisModule_AddPostNotificationJob],
        /// Defers work from a keyspace notification until Redis permits writes.
        /// Requires Redis 7.2+. The job receives a fresh event view in the
        /// database selected when it was queued and runs as part of the same
        /// execution unit, rather than on a worker thread or timer.
        ///
        /// Capture owned data: callback key bytes and context references must
        /// not escape their invocation. Jobs can produce more notifications;
        /// exclude output keys before scheduling to prevent feedback loops.
        ///
        /// Redis rejects submission while loading or on a readonly replica.
        /// On rejection, the closure and its captures are dropped immediately.
        /// On acceptance, Redis owns the job until execution and cleanup.
        /// Callback panics are caught and logged by the SDK.
        ///
        /// With a 7.2+ compatibility floor the result is [`Status`]; older
        /// floors return `APIResult<Status>` and can report a missing API.
        /// Scheduling successfully does not promise the later write succeeds.
        /// See the [notification write guide](https://github.com/joshrotenberg/redismodule-rs/blob/master/docs/KEYSPACE-NOTIFICATIONS.md)
        /// for replication, role/loading gates, database scopes and teardown.
        pub fn add_post_notification_job<F: FnOnce(&EventContext) + 'static>(
            &self,
            callback: F,
        ) -> Status {
            let callback = Box::into_raw(Box::new(Some(callback)));
            let status = unsafe {
                RedisModule_AddPostNotificationJob(
                    self.ctx,
                    Some(post_notification_job::<F>),
                    callback as *mut c_void,
                    Some(post_notification_job_free_callback::<F>),
                )
            }
            .into();
            if status == Status::Err {
                // Redis rejected the job before taking ownership. It will not
                // invoke either callback, so release our closure and captures.
                unsafe { drop(Box::from_raw(callback)) };
            }
            status
        }
    );
}

impl ReplyContext {
    #[allow(clippy::must_use_candidate)]
    pub fn reply_simple_string(&self, s: &str) -> raw::Status {
        let msg = Context::str_as_legal_resp_string(s);
        raw::reply_with_simple_string(self.ctx, msg.as_ptr())
    }

    #[allow(clippy::must_use_candidate)]
    pub fn reply_error_string(&self, s: &str) -> raw::Status {
        let msg = Context::str_as_legal_resp_string(s);
        unsafe { crate::__required_api!(RedisModule_ReplyWithError)(self.ctx, msg.as_ptr()).into() }
    }

    pub fn reply_with_key(&self, result: RedisValueKey) -> raw::Status {
        match result {
            RedisValueKey::Integer(i) => raw::reply_with_long_long(self.ctx, i),
            RedisValueKey::String(s) => {
                raw::reply_with_string_buffer(self.ctx, s.as_ptr().cast::<c_char>(), s.len())
            }
            RedisValueKey::SimpleString(s) => self.reply_simple_string(&s),
            RedisValueKey::BulkString(b) => {
                raw::reply_with_string_buffer(self.ctx, b.as_ptr().cast::<c_char>(), b.len())
            }
            RedisValueKey::BulkRedisString(s) => raw::reply_with_string(self.ctx, s.as_ptr()),
            RedisValueKey::Bool(b) => raw::reply_with_bool(self.ctx, b.into()),
        }
    }

    /// # Panics
    ///
    /// Will panic if methods used are missing in redismodule.h
    #[allow(clippy::must_use_candidate)]
    pub fn reply(&self, result: RedisResult) -> raw::Status {
        match result {
            Ok(RedisValue::Bool(v)) => raw::reply_with_bool(self.ctx, v.into()),
            Ok(RedisValue::Integer(v)) => raw::reply_with_long_long(self.ctx, v),
            Ok(RedisValue::Float(v)) => raw::reply_with_double(self.ctx, v),
            Ok(RedisValue::SimpleStringStatic(s)) => self.reply_simple_string(s),

            Ok(RedisValue::SimpleString(s)) => self.reply_simple_string(&s),

            Ok(RedisValue::BulkString(s)) => {
                raw::reply_with_string_buffer(self.ctx, s.as_ptr().cast::<c_char>(), s.len())
            }

            Ok(RedisValue::BigNumber(s)) => {
                raw::reply_with_big_number(self.ctx, s.as_ptr().cast::<c_char>(), s.len())
            }

            Ok(RedisValue::VerbatimString((format, data))) => raw::reply_with_verbatim_string(
                self.ctx,
                data.as_ptr().cast(),
                data.len(),
                format.0.as_ptr().cast(),
            ),

            Ok(RedisValue::BulkRedisString(s)) => raw::reply_with_string(self.ctx, s.as_ptr()),

            Ok(RedisValue::StringBuffer(s)) => {
                raw::reply_with_string_buffer(self.ctx, s.as_ptr().cast::<c_char>(), s.len())
            }

            Ok(RedisValue::Array(array)) => {
                raw::reply_with_array(self.ctx, array.len() as c_long);

                for elem in array {
                    self.reply(Ok(elem));
                }

                raw::Status::Ok
            }

            Ok(RedisValue::Map(map)) => {
                raw::reply_with_map(self.ctx, map.len() as c_long);

                for (key, value) in map {
                    self.reply_with_key(key);
                    self.reply(Ok(value));
                }

                raw::Status::Ok
            }

            Ok(RedisValue::OrderedMap(map)) => {
                raw::reply_with_map(self.ctx, map.len() as c_long);

                for (key, value) in map {
                    self.reply_with_key(key);
                    self.reply(Ok(value));
                }

                raw::Status::Ok
            }

            Ok(RedisValue::InsertionOrderedMap(entries)) => {
                raw::reply_with_map(self.ctx, entries.len() as c_long);

                for (key, value) in entries {
                    self.reply_with_key(key);
                    self.reply(Ok(value));
                }

                raw::Status::Ok
            }

            Ok(RedisValue::Set(set)) => {
                raw::reply_with_set(self.ctx, set.len() as c_long);
                set.into_iter().for_each(|e| {
                    self.reply_with_key(e);
                });

                raw::Status::Ok
            }

            Ok(RedisValue::OrderedSet(set)) => {
                raw::reply_with_set(self.ctx, set.len() as c_long);
                set.into_iter().for_each(|e| {
                    self.reply_with_key(e);
                });

                raw::Status::Ok
            }

            Ok(RedisValue::Null) => raw::reply_with_null(self.ctx),

            Ok(RedisValue::NoReply) => raw::Status::Ok,

            Ok(RedisValue::StaticError(s)) => self.reply_error_string(s),

            Ok(RedisValue::Error(s)) => self.reply_error_string(&s),

            Err(RedisError::WrongArity) => unsafe {
                if !cfg!(test)
                    && crate::__required_api!(RedisModule_IsKeysPositionRequest)(self.ctx) != 0
                {
                    // We can't return a result since we don't have a client
                    raw::Status::Err
                } else {
                    crate::__required_api!(RedisModule_WrongArity)(self.ctx).into()
                }
            },

            Err(RedisError::WrongType) => {
                self.reply_error_string(RedisError::WrongType.to_string().as_str())
            }

            Err(RedisError::String(s)) => self.reply_error_string(s.as_str()),

            Err(RedisError::Str(s)) => self.reply_error_string(s),
        }
    }
}

extern "C" fn post_notification_job_free_callback<F: FnOnce(&EventContext)>(pd: *mut c_void) {
    crate::panic::run_callback(
        "post notification job free",
        || drop(unsafe { Box::from_raw(pd as *mut Option<F>) }),
        || (),
    );
}

extern "C" fn post_notification_job<F: FnOnce(&EventContext)>(
    ctx: *mut raw::RedisModuleCtx,
    pd: *mut c_void,
) {
    let callback = unsafe { &mut *(pd as *mut Option<F>) };
    let ctx = unsafe { EventContext::new(ctx) };
    callback.take().map_or_else(
        || {
            ctx.log(
                RedisLogLevel::Warning,
                "Got a None callback on post notification job.",
            )
        },
        |callback| {
            crate::panic::run_callback("post notification job", || callback(&ctx), || ());
        },
    );
}

unsafe impl RedisLockIndicator for Context {}

// Private reply-only adapter: unlike runtime views this is not a Redis lock proof.
// Blocked-client replies may use it without acquiring the Redis lock.
struct ReplyContext {
    ctx: *mut raw::RedisModuleCtx,
}

#[cfg(test)]
mod call_text_tests {
    use super::RuntimeContext;
    use crate::RedisValue;

    #[test]
    fn version_parser_keeps_explicit_and_bulk_text() {
        for value in [
            RedisValue::SimpleString("redis_version:7.2.8\r\n".into()),
            RedisValue::BulkString("redis_version:7.2.8\r\n".into()),
        ] {
            let version = RuntimeContext::version_from_info(value).unwrap();
            assert_eq!((version.major, version.minor, version.patch), (7, 2, 8));
        }
        assert!(
            RuntimeContext::version_from_info(RedisValue::BulkString("no version".into())).is_err()
        );
        let version = RuntimeContext::version_from_info(RedisValue::BulkString(
            "redis_version:8.10.2-rc1\r\n".into(),
        ))
        .unwrap();
        assert_eq!((version.major, version.minor, version.patch), (8, 10, 2));
        for text in [
            "redis_version:99999999999.0.0",
            "redis_version:7.2",
            "redis_version:x.y.z",
            "redis_version:7.2.4rc1",
            "redis_version:7.2.4_1",
            "redis_version:7x.2.4",
            "redis_version:7.2y.4",
            "redis_version:+7.2.4",
            "redis_version: 7.2.4",
            "redis_version:7..4",
        ] {
            assert!(
                RuntimeContext::version_from_info(RedisValue::BulkString(text.into())).is_err()
            );
        }
        assert!(RuntimeContext::version_from_info(RedisValue::Integer(7)).is_err());
    }

    #[test]
    fn enterprise_parser_keeps_explicit_and_bulk_text() {
        for enterprise in [true, false] {
            let text = if enterprise {
                "redis_version:7.2.8\r\nrlec_version:synthetic\r\n"
            } else {
                "redis_version:7.2.8\r\n"
            };
            for value in [
                RedisValue::SimpleString(text.into()),
                RedisValue::BulkString(text.into()),
            ] {
                assert_eq!(
                    RuntimeContext::enterprise_from_info(&value).unwrap(),
                    enterprise
                );
            }
        }
        assert!(RuntimeContext::enterprise_from_info(&RedisValue::Integer(0)).is_err());
    }
}

bitflags! {
    /// An object represent ACL permissions.
    /// Used to check ACL permission using `acl_check_key_permission`.
    #[derive(Debug)]
    pub struct AclPermissions : c_int {
        /// User can look at the content of the value, either return it or copy it.
        const ACCESS = raw::REDISMODULE_CMD_KEY_ACCESS as c_int;

        /// User can insert more data to the key, without deleting or modify existing data.
        const INSERT = raw::REDISMODULE_CMD_KEY_INSERT as c_int;

        /// User can delete content from the key.
        const DELETE = raw::REDISMODULE_CMD_KEY_DELETE as c_int;

        /// User can update existing data inside the key.
        const UPDATE = raw::REDISMODULE_CMD_KEY_UPDATE as c_int;
    }
}

bitflags! {
    /// How a user accesses a pub/sub channel, for
    /// [`Context::acl_check_channel_permission`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AclChannelPermissions : c_int {
        /// The channel name is a pattern, as for `PSUBSCRIBE`.
        const PATTERN = raw::REDISMODULE_CMD_CHANNEL_PATTERN as c_int;

        /// Publish to the channel.
        const PUBLISH = raw::REDISMODULE_CMD_CHANNEL_PUBLISH as c_int;

        /// Subscribe to the channel.
        const SUBSCRIBE = raw::REDISMODULE_CMD_CHANNEL_SUBSCRIBE as c_int;

        /// Unsubscribe from the channel, which is always allowed.
        const UNSUBSCRIBE = raw::REDISMODULE_CMD_CHANNEL_UNSUBSCRIBE as c_int;
    }
}

/// Why an ACL log entry from [`Context::acl_add_log_entry`] was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclLogReason {
    /// An authentication failure.
    Auth,
    /// A command the user may not run.
    Command,
    /// A key the user may not access.
    Key,
    /// A channel the user may not access.
    Channel,
}

impl From<AclLogReason> for raw::RedisModuleACLLogEntryReason {
    fn from(reason: AclLogReason) -> Self {
        match reason {
            AclLogReason::Auth => raw::RedisModuleACLLogEntryReason_REDISMODULE_ACL_LOG_AUTH,
            AclLogReason::Command => raw::RedisModuleACLLogEntryReason_REDISMODULE_ACL_LOG_CMD,
            AclLogReason::Key => raw::RedisModuleACLLogEntryReason_REDISMODULE_ACL_LOG_KEY,
            AclLogReason::Channel => raw::RedisModuleACLLogEntryReason_REDISMODULE_ACL_LOG_CHANNEL,
        }
    }
}

/// The values allowed in the "info" sections and dictionaries.
#[derive(Debug, Clone)]
pub enum InfoContextBuilderFieldBottomLevelValue {
    /// A simple string value.
    String(String),
    /// A numeric value ([`i64`]).
    I64(i64),
    /// A numeric value ([`u64`]).
    U64(u64),
    /// A numeric value ([`f64`]).
    F64(f64),
}

impl From<String> for InfoContextBuilderFieldBottomLevelValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for InfoContextBuilderFieldBottomLevelValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<i64> for InfoContextBuilderFieldBottomLevelValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<u64> for InfoContextBuilderFieldBottomLevelValue {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

#[derive(Debug, Clone)]
pub enum InfoContextBuilderFieldTopLevelValue {
    /// A simple bottom-level value.
    Value(InfoContextBuilderFieldBottomLevelValue),
    /// A dictionary value.
    ///
    /// An example of what it looks like:
    /// ```no_run,ignore,
    /// > redis-cli: INFO
    /// >
    /// > # <section name>
    /// <dictionary name>:<key 1>=<value 1>,<key 2>=<value 2>
    /// ```
    ///
    /// Let's suppose we added a section `"my_info"`. Then into this
    /// section we can add a dictionary. Let's add a dictionary named
    /// `"module"`, with with fields `"name"` which is equal to
    /// `"redisgears_2"` and `"ver"` with a value of `999999`. If our
    /// module is named "redisgears_2", we can call `INFO redisgears_2`
    /// to obtain this information:
    ///
    /// ```no_run,ignore,
    /// > redis-cli: INFO redisgears_2
    /// >
    /// > # redisgears_2_my_info
    /// module:name=redisgears_2,ver=999999
    /// ```
    Dictionary {
        name: String,
        fields: InfoContextFieldBottomLevelData,
    },
}

impl<T: Into<InfoContextBuilderFieldBottomLevelValue>> From<T>
    for InfoContextBuilderFieldTopLevelValue
{
    fn from(value: T) -> Self {
        Self::Value(value.into())
    }
}

/// Builds a dictionary within the [`InfoContext`], similar to
/// `INFO KEYSPACE`.
#[derive(Debug)]
pub struct InfoContextBuilderDictionaryBuilder<'a> {
    /// The info section builder this dictionary builder is for.
    info_section_builder: InfoContextBuilderSectionBuilder<'a>,
    /// The name of the section to build.
    name: String,
    /// The fields this section contains.
    fields: InfoContextFieldBottomLevelData,
}

impl<'a> InfoContextBuilderDictionaryBuilder<'a> {
    /// Adds a field within this section.
    pub fn field<F: Into<InfoContextBuilderFieldBottomLevelValue>>(
        mut self,
        name: &str,
        value: F,
    ) -> RedisResult<Self> {
        if self.fields.iter().any(|k| k.0.0 == name) {
            return Err(RedisError::String(format!(
                "Found duplicate key '{name}' in the info dictionary '{}'",
                self.name
            )));
        }

        self.fields.push((name.to_owned(), value.into()).into());
        Ok(self)
    }

    /// Builds the dictionary with the fields provided.
    pub fn build_dictionary(self) -> RedisResult<InfoContextBuilderSectionBuilder<'a>> {
        let name = self.name;
        let name_ref = name.clone();
        self.info_section_builder.field(
            &name_ref,
            InfoContextBuilderFieldTopLevelValue::Dictionary {
                name,
                fields: self.fields.to_owned(),
            },
        )
    }
}

/// Builds a section within the [`InfoContext`].
#[derive(Debug)]
pub struct InfoContextBuilderSectionBuilder<'a> {
    /// The info builder this section builder is for.
    info_builder: InfoContextBuilder<'a>,
    /// The name of the section to build.
    name: String,
    /// The fields this section contains.
    fields: InfoContextFieldTopLevelData,
}

impl<'a> InfoContextBuilderSectionBuilder<'a> {
    /// Adds a field within this section.
    pub fn field<F: Into<InfoContextBuilderFieldTopLevelValue>>(
        mut self,
        name: &str,
        value: F,
    ) -> RedisResult<Self> {
        if self.fields.iter().any(|(k, _)| k == name) {
            return Err(RedisError::String(format!(
                "Found duplicate key '{name}' in the info section '{}'",
                self.name
            )));
        }
        self.fields.push((name.to_owned(), value.into()));
        Ok(self)
    }

    /// Adds a new dictionary.
    pub fn add_dictionary(self, dictionary_name: &str) -> InfoContextBuilderDictionaryBuilder<'a> {
        InfoContextBuilderDictionaryBuilder {
            info_section_builder: self,
            name: dictionary_name.to_owned(),
            fields: InfoContextFieldBottomLevelData::default(),
        }
    }

    /// Builds the section with the fields provided.
    pub fn build_section(mut self) -> RedisResult<InfoContextBuilder<'a>> {
        if self
            .info_builder
            .sections
            .iter()
            .any(|(k, _)| k == &self.name)
        {
            return Err(RedisError::String(format!(
                "Found duplicate section in the Info reply: {}",
                self.name
            )));
        }

        self.info_builder
            .sections
            .push((self.name.clone(), self.fields));

        Ok(self.info_builder)
    }
}

/// A single info context's bottom level field data.
#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct InfoContextBottomLevelFieldData(pub (String, InfoContextBuilderFieldBottomLevelValue));
impl Deref for InfoContextBottomLevelFieldData {
    type Target = (String, InfoContextBuilderFieldBottomLevelValue);

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for InfoContextBottomLevelFieldData {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: Into<InfoContextBuilderFieldBottomLevelValue>> From<(String, T)>
    for InfoContextBottomLevelFieldData
{
    fn from(value: (String, T)) -> Self {
        Self((value.0, value.1.into()))
    }
}
/// A type for the `key => bottom-level-value` storage of an info
/// section.
#[derive(Debug, Default, Clone)]
#[repr(transparent)]
pub struct InfoContextFieldBottomLevelData(pub Vec<InfoContextBottomLevelFieldData>);
impl Deref for InfoContextFieldBottomLevelData {
    type Target = Vec<InfoContextBottomLevelFieldData>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for InfoContextFieldBottomLevelData {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// A type alias for the `key => top-level-value` storage of an info
/// section.
pub type InfoContextFieldTopLevelData = Vec<(String, InfoContextBuilderFieldTopLevelValue)>;
/// One section contents: name and children.
pub type OneInfoSectionData = (String, InfoContextFieldTopLevelData);
/// A type alias for the section data, associated with the info section.
pub type InfoContextTreeData = Vec<OneInfoSectionData>;

impl<T: Into<InfoContextBuilderFieldBottomLevelValue>> From<BTreeMap<String, T>>
    for InfoContextFieldBottomLevelData
{
    fn from(value: BTreeMap<String, T>) -> Self {
        Self(
            value
                .into_iter()
                .map(|e| (e.0, e.1.into()).into())
                .collect(),
        )
    }
}

impl<T: Into<InfoContextBuilderFieldBottomLevelValue>> From<HashMap<String, T>>
    for InfoContextFieldBottomLevelData
{
    fn from(value: HashMap<String, T>) -> Self {
        Self(
            value
                .into_iter()
                .map(|e| (e.0, e.1.into()).into())
                .collect(),
        )
    }
}

#[derive(Debug)]
pub struct InfoContextBuilder<'a> {
    context: &'a InfoContext,
    sections: InfoContextTreeData,
}
impl<'a> InfoContextBuilder<'a> {
    fn add_bottom_level_field(
        &self,
        key: &str,
        value: &InfoContextBuilderFieldBottomLevelValue,
    ) -> RedisResult<()> {
        use InfoContextBuilderFieldBottomLevelValue as BottomLevel;

        match value {
            BottomLevel::String(string) => add_info_field_str(self.context.ctx, key, string),
            BottomLevel::I64(number) => add_info_field_long_long(self.context.ctx, key, *number),
            BottomLevel::U64(number) => {
                add_info_field_unsigned_long_long(self.context.ctx, key, *number)
            }
            BottomLevel::F64(number) => add_info_field_double(self.context.ctx, key, *number),
        }
        .into()
    }
    /// Adds fields. Make sure that the corresponding section/dictionary
    /// have been added before calling this method.
    fn add_top_level_fields(&self, fields: &InfoContextFieldTopLevelData) -> RedisResult<()> {
        use InfoContextBuilderFieldTopLevelValue as TopLevel;

        fields.iter().try_for_each(|(key, value)| match value {
            TopLevel::Value(bottom_level) => self.add_bottom_level_field(key, bottom_level),
            TopLevel::Dictionary { name, fields } => {
                std::convert::Into::<RedisResult<()>>::into(add_info_begin_dict_field(
                    self.context.ctx,
                    name,
                ))?;
                fields
                    .iter()
                    .try_for_each(|f| self.add_bottom_level_field(&f.0.0, &f.0.1))?;
                add_info_end_dict_field(self.context.ctx).into()
            }
        })
    }

    fn finalise_data(&self) -> RedisResult<()> {
        self.sections
            .iter()
            .try_for_each(|(section_name, section_fields)| -> RedisResult<()> {
                if add_info_section(self.context.ctx, Some(section_name)) == Status::Ok {
                    self.add_top_level_fields(section_fields)
                } else {
                    // This section wasn't requested.
                    Ok(())
                }
            })
    }

    /// Sends the info accumulated so far to the [`InfoContext`].
    pub fn build_info(self) -> RedisResult<&'a InfoContext> {
        self.finalise_data().map(|_| self.context)
    }

    /// Returns a section builder.
    pub fn add_section(self, name: &'a str) -> InfoContextBuilderSectionBuilder<'a> {
        InfoContextBuilderSectionBuilder {
            info_builder: self,
            name: name.to_owned(),
            fields: InfoContextFieldTopLevelData::new(),
        }
    }

    /// Adds the section data without checks for the values already
    /// being present. In this case, the values will be overwritten.
    pub(crate) fn add_section_unchecked(mut self, section: OneInfoSectionData) -> Self {
        self.sections.push(section);
        self
    }
}

impl<'a> From<&'a InfoContext> for InfoContextBuilder<'a> {
    fn from(context: &'a InfoContext) -> Self {
        Self {
            context,
            sections: InfoContextTreeData::new(),
        }
    }
}

/// Output for one module INFO callback, including callbacks for crash reports.
///
/// This view only supports INFO fields. It does not implement
/// [`RedisLockIndicator`] or provide a [`RuntimeContext`]: the same callback
/// also runs during crash reporting, without a general Redis lock guarantee.
/// Keep the callback's `for_crash_report` argument and return early before
/// reading ordinary module state when it is true. This does not make the
/// entire SDK crash-report path async-signal-safe.
///
/// For ordinary INFO requests, independently synchronized Rust snapshots,
/// such as atomic counters, do not need a Redis lock capability:
///
/// ```no_run
/// use redis_module::{InfoContext, RedisResult};
/// use std::sync::atomic::{AtomicU64, Ordering};
/// static COMPLETED: AtomicU64 = AtomicU64::new(0);
/// fn info(ctx: &InfoContext, for_crash_report: bool) -> RedisResult<()> {
///     if for_crash_report { return Ok(()); }
///     ctx.builder().add_section("stats")
///         .field("completed", COMPLETED.load(Ordering::Relaxed))?
///         .build_section()?.build_info()?;
///     Ok(())
/// }
/// ```
///
/// Testing the crash-report flag does not grant access to Redis-protected data:
///
/// ```compile_fail,E0277
/// use redis_module::{InfoContext, RedisGILGuard};
/// static STATE: RedisGILGuard<u64> = RedisGILGuard::new(0);
/// fn info(ctx: &InfoContext, for_crash_report: bool) {
///     if for_crash_report { return; }
///     let _state = STATE.lock(ctx);
/// }
/// ```
///
/// Nor can an INFO view be passed to helpers that require a runtime context:
///
/// ```compile_fail,E0308
/// use redis_module::{InfoContext, RuntimeContext};
/// fn needs_runtime(_: &RuntimeContext) {}
/// fn info(ctx: &InfoContext) { needs_runtime(ctx); }
/// ```
#[derive(Debug)]
pub struct InfoContext {
    pub(crate) ctx: *mut raw::RedisModuleInfoCtx,
}

impl InfoContext {
    /// Wraps the info context Redis passed to the module's info callback.
    ///
    /// # Safety
    ///
    /// `ctx` must be the context of the running info callback, and the
    /// `InfoContext` must not outlive the callback. This is a native INFO
    /// output context, not a `RedisModuleCtx`; it cannot establish that the
    /// Redis lock is held or be cast to a runtime context.
    pub const unsafe fn new(ctx: *mut raw::RedisModuleInfoCtx) -> Self {
        Self { ctx }
    }

    /// The raw info context, for calls to the `RedisModule_Info*` functions
    /// in [`raw`].
    #[must_use]
    pub const fn get_raw(&self) -> *mut raw::RedisModuleInfoCtx {
        self.ctx
    }

    /// Returns a builder for the [`InfoContext`].
    pub fn builder(&self) -> InfoContextBuilder<'_> {
        InfoContextBuilder::from(self)
    }

    /// Returns a build result for the passed [`OneInfoSectionData`].
    pub fn build_one_section<T: Into<OneInfoSectionData>>(&self, data: T) -> RedisResult<()> {
        self.builder()
            .add_section_unchecked(data.into())
            .build_info()?;
        Ok(())
    }

    #[deprecated = "Please use [`InfoContext::builder`] instead."]
    /// The `name` of the sction will be prefixed with the module name
    /// and an underscore: `<module name>_<name>`.
    pub fn add_info_section(&self, name: Option<&str>) -> Status {
        add_info_section(self.ctx, name)
    }

    #[deprecated = "Please use [`InfoContext::builder`] instead."]
    /// The `name` will be prefixed with the module name and an
    /// underscore: `<module name>_<name>`. The `content` pass is left
    /// "as is".
    pub fn add_info_field_str(&self, name: &str, content: &str) -> Status {
        add_info_field_str(self.ctx, name, content)
    }

    #[deprecated = "Please use [`InfoContext::builder`] instead."]
    /// The `name` will be prefixed with the module name and an
    /// underscore: `<module name>_<name>`. The `value` pass is left
    /// "as is".
    pub fn add_info_field_long_long(&self, name: &str, value: c_longlong) -> Status {
        add_info_field_long_long(self.ctx, name, value)
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ContextFlags : c_int {
        /// The command is running in the context of a Lua script
        const LUA = raw::REDISMODULE_CTX_FLAGS_LUA as c_int;

        /// The command is running inside a Redis transaction
        const MULTI = raw::REDISMODULE_CTX_FLAGS_MULTI as c_int;

        /// The instance is a master
        const MASTER = raw::REDISMODULE_CTX_FLAGS_MASTER as c_int;

        /// The instance is a SLAVE
        const SLAVE = raw::REDISMODULE_CTX_FLAGS_SLAVE as c_int;

        /// The instance is read-only (usually meaning it's a slave as well)
        const READONLY = raw::REDISMODULE_CTX_FLAGS_READONLY as c_int;

        /// The instance is running in cluster mode
        const CLUSTER = raw::REDISMODULE_CTX_FLAGS_CLUSTER as c_int;

        /// The instance has AOF enabled
        const AOF = raw::REDISMODULE_CTX_FLAGS_AOF as c_int;

        /// The instance has RDB enabled
        const RDB = raw::REDISMODULE_CTX_FLAGS_RDB as c_int;

        /// The instance has Maxmemory set
        const MAXMEMORY = raw::REDISMODULE_CTX_FLAGS_MAXMEMORY as c_int;

        /// Maxmemory is set and has an eviction policy that may delete keys
        const EVICTED = raw::REDISMODULE_CTX_FLAGS_EVICT as c_int;

        /// Redis is out of memory according to the maxmemory flag.
        const OOM = raw::REDISMODULE_CTX_FLAGS_OOM as c_int;

        /// Less than 25% of memory available according to maxmemory.
        const OOM_WARNING = raw::REDISMODULE_CTX_FLAGS_OOM_WARNING as c_int;

        /// The command was sent over the replication link.
        const REPLICATED = raw::REDISMODULE_CTX_FLAGS_REPLICATED as c_int;

        /// Redis is currently loading either from AOF or RDB.
        const LOADING = raw::REDISMODULE_CTX_FLAGS_LOADING as c_int;

        /// The replica has no link with its master
        const REPLICA_IS_STALE = raw::REDISMODULE_CTX_FLAGS_REPLICA_IS_STALE as c_int;

        /// The replica is trying to connect with the master
        const REPLICA_IS_CONNECTING = raw::REDISMODULE_CTX_FLAGS_REPLICA_IS_CONNECTING as c_int;

        /// The replica is receiving an RDB file from its master.
        const REPLICA_IS_TRANSFERRING = raw::REDISMODULE_CTX_FLAGS_REPLICA_IS_TRANSFERRING as c_int;

        /// The replica is online, receiving updates from its master
        const REPLICA_IS_ONLINE = raw::REDISMODULE_CTX_FLAGS_REPLICA_IS_ONLINE as c_int;

        /// There is currently some background process active.
        const ACTIVE_CHILD = raw::REDISMODULE_CTX_FLAGS_ACTIVE_CHILD as c_int;

        /// Redis is currently running inside background child process.
        const IS_CHILD = raw::REDISMODULE_CTX_FLAGS_IS_CHILD as c_int;

        /// The next EXEC will fail due to dirty CAS (touched keys).
        const MULTI_DIRTY = raw::REDISMODULE_CTX_FLAGS_MULTI_DIRTY as c_int;

        /// The current client does not allow blocking, either called from
        /// within multi, lua, or from another module using RM_Call
        const DENY_BLOCKING = raw::REDISMODULE_CTX_FLAGS_DENY_BLOCKING as c_int;

        /// The current client uses RESP3 protocol
        const FLAGS_RESP3 = raw::REDISMODULE_CTX_FLAGS_RESP3 as c_int;

        /// Redis is currently async loading database for diskless replication.
        const ASYNC_LOADING = raw::REDISMODULE_CTX_FLAGS_ASYNC_LOADING as c_int;
    }
}

#[cfg(test)]
mod resp_text_tests {
    use super::Context;

    #[test]
    fn simple_reply_text_keeps_non_ascii_and_replaces_line_breaks_and_nul() {
        let text = Context::str_as_legal_resp_string("caf\u{e9} \u{65e5}\u{672c}\r\n+OK\0end");
        assert_eq!(text.to_str(), Ok("caf\u{e9} \u{65e5}\u{672c}  +OK end"));
    }
}

#[cfg(test)]
mod call_protocol_tests {
    use super::{CallOptionResp, CallOptionsBuilder};

    #[test]
    fn last_protocol_selection_wins() {
        let modes = [
            (CallOptionResp::Resp2, "v"),
            (CallOptionResp::Resp3, "v3"),
            (CallOptionResp::Auto, "v0"),
        ];
        assert_eq!(CallOptionsBuilder::new().build().options.to_str(), Ok("v"));
        for (first, _) in modes {
            for (second, _) in modes {
                for (last, expected) in modes {
                    let options = CallOptionsBuilder::new()
                        .resp(first)
                        .resp(second)
                        .resp(last)
                        .build();
                    assert_eq!(options.options.to_str(), Ok(expected));
                }
            }
        }
    }

    #[test]
    fn protocol_changes_preserve_other_options() {
        let options = CallOptionsBuilder::new()
            .no_writes()
            .resp(CallOptionResp::Resp3)
            .script_mode()
            .verify_acl()
            .verify_oom()
            .resp(CallOptionResp::Auto)
            .errors_as_replies()
            .replicate_to_replicas_only()
            .dry_run()
            .resp(CallOptionResp::Resp2)
            .build();
        assert_eq!(options.options.to_str(), Ok("vWSCME!AD"));
    }

    #[cfg(any(
        feature = "min-redis-compatibility-version-8-0",
        feature = "min-redis-compatibility-version-7-4",
        feature = "min-redis-compatibility-version-7-2"
    ))]
    #[test]
    fn blocking_options_keep_the_last_protocol_and_blocking_permission() {
        for (last, expected) in [
            (CallOptionResp::Resp2, "vEK"),
            (CallOptionResp::Resp3, "vE3K"),
            (CallOptionResp::Auto, "vE0K"),
        ] {
            let options = CallOptionsBuilder::new()
                .resp(CallOptionResp::Resp3)
                .resp(CallOptionResp::Auto)
                .errors_as_replies()
                .resp(last)
                .build_blocking();
            assert_eq!(options.options.to_str(), Ok(expected));
        }
    }
}

#[cfg(test)]
mod call_failure_tests {
    use super::call_failure;

    #[test]
    fn errno_maps_to_the_documented_reason() {
        assert_eq!(call_failure(None), None);
        assert_eq!(call_failure(Some(0)), None);
        assert_eq!(
            call_failure(Some(libc::ENOENT)).as_deref(),
            Some("ERR RedisModule_Call failed: unknown command")
        );
        assert!(
            call_failure(Some(libc::EIO))
                .unwrap()
                .starts_with("ERR RedisModule_Call failed: ")
        );
    }
}
