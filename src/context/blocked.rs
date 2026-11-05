use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::os::raw::{c_int, c_long, c_longlong};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::Duration;

use crate::raw;
use crate::{Context, ContextFlags, RedisError, RedisResult, RedisString, RedisValue};

/// A successfully blocked client, unblocked when this handle is dropped.
/// Send it to a worker and use [`crate::ThreadSafeContext::with_blocked_client`]
/// to reply. A successful command must return [`crate::RedisValue::NoReply`].
///
/// The client gets one reply. If the handle is dropped without one, for
/// example when a worker returns early or panics, the client gets
/// `ERR the blocked client was released without a reply` instead of waiting
/// forever. A handle dropped by the command that blocked the client, on the
/// command's own thread and before it returns, leaves the reply to that
/// command: an error it returns with `?`, or the error for a panic, is the
/// only reply, and if it returns `NoReply` the SDK replies with the
/// released-without-a-reply error instead. Commands registered without the
/// SDK's command callbacks are not tracked this way, so a handle they drop
/// always sends the error, even when the command also replies. A command that
/// releases its handle, replies itself through [`Context::reply`] and then
/// returns `NoReply` sends two replies.
pub struct BlockedClient {
    pub(crate) inner: NonNull<raw::RedisModuleBlockedClient>,
    /// Whether a reply was sent through this client's thread-safe context.
    pub(crate) replied: Cell<bool>,
    /// The command invocation that blocked the client, or 0.
    command: u64,
}

// SAFETY: Redis permits UnblockClient and blocked-client thread-safe contexts
// on module workers. Ownership ensures one eventual UnblockClient call.
unsafe impl Send for BlockedClient {}

thread_local! {
    /// The command invocation on this thread that released a blocked client it
    /// created without a reply, or 0. A `Cell<u64>` has no destructor, which
    /// would keep glibc from unloading the module.
    static RELEASED_UNREPLIED: Cell<u64> = const { Cell::new(0) };
}

/// Sets the released-without-a-reply record, returning the previous one, so
/// that [`crate::panic::run_command`] can keep a nested command from
/// overwriting its caller's record.
pub(crate) fn replace_released_unreplied(invocation: u64) -> u64 {
    RELEASED_UNREPLIED.replace(invocation)
}

/// The released-without-a-reply error, for the command callbacks.
const RELEASED_WITHOUT_REPLY: &str = "ERR the blocked client was released without a reply";

/// Replaces a command's `NoReply` with the released-without-a-reply error when
/// the command released a blocked client it created without a reply, which
/// would otherwise leave the client with no reply at all. Used by the SDK's
/// command callbacks.
#[doc(hidden)]
pub fn __command_reply_after_release(response: RedisResult) -> RedisResult {
    let current = crate::panic::current_command();
    if current == 0 || RELEASED_UNREPLIED.get() != current {
        return response;
    }
    RELEASED_UNREPLIED.set(0);
    match response {
        Ok(crate::RedisValue::NoReply) => Err(RedisError::Str(RELEASED_WITHOUT_REPLY)),
        response => response,
    }
}

impl Drop for BlockedClient {
    fn drop(&mut self) {
        let in_blocking_command =
            self.command != 0 && crate::panic::current_command() == self.command;
        if !self.replied.get() {
            if in_blocking_command {
                RELEASED_UNREPLIED.set(self.command);
            } else {
                reply_released_without_reply(self.inner);
            }
        }
        unsafe {
            crate::__required_api!(RedisModule_UnblockClient)(self.inner.as_ptr(), ptr::null_mut())
        };
    }
}

/// Replies with an error to a blocked client that is about to be unblocked
/// with no reply. Redis would otherwise unblock it with nothing to send, and
/// the client would wait forever.
fn reply_released_without_reply(client: NonNull<raw::RedisModuleBlockedClient>) {
    // SAFETY: Redis permits a blocked client's thread-safe context, and its
    // reply functions without the Redis lock, on any thread until the client
    // is unblocked; the context is freed before UnblockClient.
    unsafe {
        let (Some(get_context), Some(reply_with_error), Some(free_context)) = (
            raw::RedisModule_GetThreadSafeContext,
            raw::RedisModule_ReplyWithError,
            raw::RedisModule_FreeThreadSafeContext,
        ) else {
            return;
        };
        let ctx = get_context(client.as_ptr());
        if ctx.is_null() {
            return;
        }
        reply_with_error(
            ctx,
            c"ERR the blocked client was released without a reply".as_ptr(),
        );
        free_context(ctx);
    }
}

impl Context {
    /// Blocks a client so a worker can reply through a thread-safe context.
    /// Dropping the returned handle unblocks the client, with an error reply
    /// if none was sent (see [`BlockedClient`]). Once this succeeds, return
    /// [`crate::RedisValue::NoReply`] and send the reply through that
    /// thread-safe context; returning a normal reply after handing the handle
    /// to a worker would send a second one.
    ///
    /// # Errors
    ///
    /// Rejects invocations with no client, scripts, transactions, module calls
    /// without blocking permission, already-blocked network clients, and
    /// blocked-client reply/timeout callbacks before calling Redis's blocking API. Also returns an error if a required
    /// API is unavailable or the native call unexpectedly returns a null handle.
    ///
    /// ```no_run
    /// use redis_module::{Context, RedisResult, RedisValue, ThreadSafeContext};
    /// fn command(ctx: &Context) -> RedisResult {
    ///     let blocked = ctx.block_client()?;
    ///     std::thread::spawn(move || {
    ///         let reply = ThreadSafeContext::with_blocked_client(blocked);
    ///         reply.reply(Ok("done".into()));
    ///     });
    ///     Ok(RedisValue::NoReply)
    /// }
    /// ```
    pub fn block_client(&self) -> Result<BlockedClient, RedisError> {
        let block = unsafe { crate::__available_api!(RedisModule_BlockClient) };
        unsafe { crate::__available_api!(RedisModule_UnblockClient) };
        self.validate_blocking_context()?;
        let inner = NonNull::new(unsafe { block(self.ctx, None, None, None, 0) })
            .ok_or(RedisError::Str("ERR Redis did not block the client"))?;
        Ok(BlockedClient {
            inner,
            replied: Cell::new(false),
            command: crate::panic::current_command(),
        })
    }

    fn validate_blocking_context(&self) -> Result<(), RedisError> {
        if self.blocking_context_allowed()? {
            if self.indexed_network_client_info()?.is_some_and(|info| {
                info.flags & u64::from(raw::REDISMODULE_CLIENTINFO_FLAG_BLOCKED) != 0
            }) {
                return Err(RedisError::Str("ERR this client is already blocked"));
            }
            Ok(())
        } else {
            Err(RedisError::Str(
                "ERR this command cannot block here (MULTI, a script, or a call without K)",
            ))
        }
    }

    fn blocking_context_allowed(&self) -> Result<bool, RedisError> {
        let client_id = unsafe { crate::__available_api!(RedisModule_GetClientId) };
        let is_reply = unsafe { crate::__available_api!(RedisModule_IsBlockedReplyRequest) };
        let is_timeout = unsafe { crate::__available_api!(RedisModule_IsBlockedTimeoutRequest) };
        let flags = unsafe { crate::__available_api!(RedisModule_GetContextFlags) };
        // A non-null native handle is not proof that the client was blocked:
        // Redis can emit an error and still return an aborted handle.
        let id = unsafe { client_id(self.ctx) };
        if id == 0 {
            return Err(RedisError::Str("ERR this context has no client to block"));
        }
        if unsafe { is_reply(self.ctx) != 0 || is_timeout(self.ctx) != 0 } {
            return Err(RedisError::Str(
                "ERR cannot block from a blocked client's reply or timeout callback",
            ));
        }
        // Older servers report script/transaction flags without DENY_BLOCKING.
        if ContextFlags::from_bits_truncate(unsafe { flags(self.ctx) })
            .intersects(ContextFlags::DENY_BLOCKING | ContextFlags::LUA | ContextFlags::MULTI)
        {
            return Ok(false);
        }
        // Redis 6.0 does not expose DENY_BLOCKING and its RM_Call clients
        // cannot block. Those temporary clients are not in the client index.
        let legacy_flags = unsafe { raw::RedisModule_GetServerVersion }
            .is_none_or(|version| unsafe { version() } < 0x0006_0200);
        if legacy_flags {
            let info = unsafe { crate::__available_api!(RedisModule_GetClientInfoById) };
            if unsafe { info(ptr::null_mut(), id) } != raw::REDISMODULE_OK as c_int {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // Get metadata only after an actual network client has been established.
    // None means an internal/module client or a client whose identity is not
    // available. The caller holds the Redis lock throughout the proof/read.
    fn indexed_network_client_info(
        &self,
    ) -> Result<Option<raw::RedisModuleClientInfoV1>, RedisError> {
        let client_id = unsafe { crate::__available_api!(RedisModule_GetClientId) };
        let client_info = unsafe { crate::__available_api!(RedisModule_GetClientInfoById) };
        let id = unsafe { client_id(self.ctx) };
        if id == 0 || unsafe { client_info(ptr::null_mut(), id) } != raw::REDISMODULE_OK as c_int {
            return Ok(None);
        }
        // Both lookups search the client index, so a module-call client fails
        // the check above. The username lookup also returns null for an
        // indexed client without an ACL user, such as a replica's link to its
        // master; that is not treated as a network client either. Ask for full
        // info only after these checks: fake clients have a null connection.
        // Redis 6.0 lacks the username API.
        if let Some(user_name) = unsafe { raw::RedisModule_GetClientUserNameById } {
            let free = unsafe { crate::__available_api!(RedisModule_FreeString) };
            // This API requires a live producer context for auto-memory.
            // Free through the same context to remove any auto-memory entry.
            let user = unsafe { user_name(self.ctx, id) };
            if user.is_null() {
                return Ok(None);
            }
            unsafe { free(self.ctx, user) };
        }
        let mut info = raw::RedisModuleClientInfoV1 {
            version: 1,
            flags: 0,
            id: 0,
            addr: [0; 46],
            port: 0,
            db: 0,
        };
        if unsafe { client_info((&mut info as *mut raw::RedisModuleClientInfoV1).cast(), id) }
            != raw::REDISMODULE_OK as c_int
        {
            return Err(RedisError::Str("ERR Redis did not return client metadata"));
        }
        Ok(Some(info))
    }

    /// Blocks the client until [`ReplyingBlockedClient::unblock`] hands over a
    /// value, usually from another thread, or until `timeout` passes.
    ///
    /// On the main thread, Redis then calls `on_reply` with the value, or
    /// `on_timeout` if the timeout passed first, and replies with what it
    /// returns. Once the client is blocked, the command must return
    /// [`RedisValue::NoReply`](crate::RedisValue::NoReply): an error returned
    /// after blocking, for example with `?`, is a second reply. Report errors
    /// through `on_reply` instead.
    ///
    /// # Errors
    ///
    /// Returns an error, without blocking, if the context cannot block: it has
    /// no client, it runs a command inside `MULTI`, a script, or a call without
    /// the `K` flag, the network client is already blocked, or it is the reply
    /// or timeout callback of another blocked client. Also returns an error if `timeout` is out of range, or the
    /// running Redis lacks an API this needs (`RedisModule_BlockClientSetPrivateData`
    /// arrived in Redis 7.2).
    pub fn block_client_on_reply<T: Send + 'static>(
        &self,
        timeout: Option<Duration>,
        on_reply: fn(&Context, T) -> RedisResult,
        on_timeout: fn(&Context) -> RedisResult,
    ) -> Result<ReplyingBlockedClient<T>, RedisError> {
        // Look up every API the callbacks and the handle use before blocking.
        let block = unsafe { crate::__available_api!(RedisModule_BlockClient) };
        let set_private_data =
            unsafe { crate::__available_api!(RedisModule_BlockClientSetPrivateData) };
        unsafe { crate::__available_api!(RedisModule_GetBlockedClientPrivateData) };
        unsafe { crate::__available_api!(RedisModule_UnblockClient) };
        self.validate_blocking_context()?;
        // 0 means no timeout to Redis, so a timeout under a millisecond is 1.
        // Redis rejects a timeout that overflows when added to the current
        // time; anything this large is rejected first.
        let timeout_ms = match timeout {
            None => 0,
            Some(timeout) => c_longlong::try_from(timeout.as_millis())
                .ok()
                .filter(|ms| *ms <= MAX_TIMEOUT_MS)
                .ok_or(RedisError::Str("ERR blocking timeout is out of range"))?
                .max(1),
        };
        let payload = Box::into_raw(Box::new(Payload {
            on_reply,
            on_timeout,
            value: Mutex::new(None),
        }));
        let inner = unsafe {
            block(
                self.ctx,
                Some(reply_callback::<T>),
                Some(timeout_callback::<T>),
                Some(free_payload::<T>),
                timeout_ms,
            )
        };
        if inner.is_null() {
            // RedisModule_BlockClient does not return null; this is a guard.
            // SAFETY: Redis did not take the payload.
            drop(unsafe { Box::from_raw(payload) });
            return Err(RedisError::Str("ERR Redis did not block the client"));
        }
        // SAFETY: the payload lives until Redis calls free_payload, after the
        // UnblockClient call that the handle always makes.
        unsafe { set_private_data(inner, payload.cast()) };
        Ok(ReplyingBlockedClient { inner, payload })
    }
}

/// The longest blocking timeout, in milliseconds: far above any real timeout,
/// and far below the point where Redis's `now + timeout` overflows.
const MAX_TIMEOUT_MS: c_longlong = c_longlong::MAX / 2;

/// The private data of a client blocked by [`Context::block_client_on_reply`].
struct Payload<T> {
    on_reply: fn(&Context, T) -> RedisResult,
    on_timeout: fn(&Context) -> RedisResult,
    value: Mutex<Option<T>>,
}

/// A client blocked by [`Context::block_client_on_reply`]. Send it to the
/// thread that produces the reply value and call [`Self::unblock`].
///
/// Dropping it without calling [`Self::unblock`] unblocks the client with an
/// error reply.
pub struct ReplyingBlockedClient<T: Send + 'static> {
    inner: *mut raw::RedisModuleBlockedClient,
    payload: *const Payload<T>,
}

// SAFETY: Redis allows UnblockClient from any thread, and the payload's value
// is behind a mutex.
unsafe impl<T: Send + 'static> Send for ReplyingBlockedClient<T> {}

impl<T: Send + 'static> ReplyingBlockedClient<T> {
    /// Unblocks the client. On the main thread, Redis calls the `on_reply`
    /// callback with `value`, unless the client already timed out or
    /// disconnected, in which case `value` is dropped there.
    pub fn unblock(self, value: T) {
        // SAFETY: the payload lives until Redis frees it after unblock.
        *unsafe { &*self.payload }
            .value
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(value);
        let this = ManuallyDrop::new(self);
        this.unblock_client();
    }

    fn unblock_client(&self) {
        // SAFETY: called once per handle, by unblock or by Drop.
        unsafe {
            crate::__required_api!(RedisModule_UnblockClient)(
                self.inner,
                self.payload.cast_mut().cast(),
            )
        };
    }
}

impl<T: Send + 'static> Drop for ReplyingBlockedClient<T> {
    fn drop(&mut self) {
        self.unblock_client();
    }
}

/// The payload of the blocked client whose callback Redis is running.
///
/// # Safety
///
/// `ctx` must be the context of a reply or timeout callback of a client blocked
/// by [`Context::block_client_on_reply`] with this `T`.
unsafe fn payload<'a, T>(ctx: *mut raw::RedisModuleCtx) -> Option<&'a Payload<T>> {
    unsafe {
        let payload = crate::__required_api!(RedisModule_GetBlockedClientPrivateData)(ctx);
        payload.cast::<Payload<T>>().as_ref()
    }
}

extern "C" fn reply_callback<T: Send + 'static>(
    ctx: *mut raw::RedisModuleCtx,
    _argv: *mut *mut raw::RedisModuleString,
    _argc: c_int,
) -> c_int {
    // SAFETY: Redis passes the reply context of a client blocked with this T.
    unsafe {
        crate::panic::run_command(ctx, || {
            let context = Context::new(ctx);
            let reply = payload::<T>(ctx)
                .and_then(|payload| {
                    let value = payload
                        .value
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take();
                    value.map(|value| (payload.on_reply)(&context, value))
                })
                .unwrap_or(Err(RedisError::Str(
                    "ERR the blocked client was released without a reply",
                )));
            context.reply(reply) as c_int
        })
    }
}

extern "C" fn timeout_callback<T: Send + 'static>(
    ctx: *mut raw::RedisModuleCtx,
    _argv: *mut *mut raw::RedisModuleString,
    _argc: c_int,
) -> c_int {
    // SAFETY: Redis passes the timeout context of a client blocked with this T.
    unsafe {
        crate::panic::run_command(ctx, || {
            let context = Context::new(ctx);
            let reply = match payload::<T>(ctx) {
                Some(payload) => (payload.on_timeout)(&context),
                None => Err(RedisError::Str("ERR the blocked client timed out")),
            };
            context.reply(reply) as c_int
        })
    }
}

extern "C" fn free_payload<T: Send + 'static>(
    _ctx: *mut raw::RedisModuleCtx,
    payload: *mut c_void,
) {
    crate::panic::run_callback(
        "blocked client free",
        // SAFETY: the payload came from Box::into_raw in block_client_on_reply,
        // and Redis frees it once.
        || drop(unsafe { Box::from_raw(payload.cast::<Payload<T>>()) }),
        || (),
    );
}

/// A validated timeout for [`Context::block_client_on_keys`].
///
/// Zero milliseconds means no deadline. Positive sub-millisecond durations
/// round up to one millisecond, as does `Duration::ZERO`; use [`Self::INFINITE`]
/// to disable the deadline. The maximum leaves more than 292,000 years of
/// clock headroom before Redis adds its fresh wall-clock reading, avoiding
/// the native overflow path that can leak a blocked-client handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyBlockingTimeout(c_longlong);

impl KeyBlockingTimeout {
    /// The largest accepted timeout in milliseconds.
    pub const MAX_MILLISECONDS: i64 = i64::MAX - i64::MAX / 1000;

    /// Wait until a key is ready, the client disconnects or CLIENT UNBLOCK
    /// requests the timeout callback.
    pub const INFINITE: Self = Self(0);

    /// The validated timeout passed to Redis; zero means no deadline.
    #[must_use]
    pub const fn as_milliseconds(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for KeyBlockingTimeout {
    type Error = RedisError;

    fn try_from(milliseconds: i64) -> Result<Self, Self::Error> {
        if !(0..=Self::MAX_MILLISECONDS).contains(&milliseconds) {
            return Err(RedisError::Str("ERR key blocking timeout is out of range"));
        }
        Ok(Self(milliseconds))
    }
}

impl TryFrom<Duration> for KeyBlockingTimeout {
    type Error = RedisError;

    fn try_from(duration: Duration) -> Result<Self, Self::Error> {
        let milliseconds = i64::try_from(duration.as_millis())
            .map_err(|_| RedisError::Str("ERR key blocking timeout is out of range"))?;
        Self::try_from(milliseconds.max(1))
    }
}

/// The result of [`Context::block_client_on_keys`].
#[derive(Debug)]
pub enum KeyBlockingResult<T> {
    /// Redis owns the waiter. Return [`RedisValue::NoReply`] from the command.
    Blocked {
        /// Whether Redis enabled waking this waiter when a key is deleted.
        /// Older servers fall back to ordinary readiness notifications.
        unblock_on_delete: bool,
    },
    /// This invocation cannot block (a script, transaction or module call,
    /// including a module call with `K`). No native block was attempted; use the returned
    /// data to compute the command's non-blocking fallback.
    Denied(T),
}

impl Context {
    /// Blocks the calling client on one or more keys, transferring `data` to
    /// Redis until the waiter is served, times out or disconnects.
    ///
    /// `on_ready` runs when a key is signaled ready. `Ok(None)` keeps waiting;
    /// `Ok(Some(value))` or an error replies and finishes the wait. The ready
    /// key is available through [`Self::blocked_client_ready_key`]. The timeout
    /// callback also runs for `CLIENT UNBLOCK ... TIMEOUT`. Both callbacks are
    /// guarded against panics, as is destruction of the private data. Returning
    /// `NoReply` from either callback becomes an error so the client cannot be
    /// unblocked without a reply. A callback either returns its reply, or
    /// writes it through [`Self::key_blocking_reply`], which streams a large
    /// reply without building a [`RedisValue`] for it first. A reply written
    /// any other way, such as through [`Self::reply`], is not counted, so
    /// returning `NoReply` after it sends a second reply.
    ///
    /// Only indexed network clients may wait. Module calls, including calls
    /// using the `K` flag, return [`KeyBlockingResult::Denied`] and must use the
    /// non-blocking fallback. Requires a module loaded with `redis_module!`;
    /// load/unload callbacks and unbound custom loaders cannot register waiters.
    ///
    /// Redis protects the module from unload while the waiter exists. Its
    /// free callback drops the data once even when the client disconnects
    /// without either reply callback running. Callback data is protected from
    /// overlapping/reentrant mutable access, and remains owned while a callback
    /// runs even if a nested command cancels the native waiter.
    ///
    /// When requested, Redis 7.2+ wakes on key deletion. Older servers use
    /// `BlockClientOnKeys` without that flag and report `unblock_on_delete:
    /// false` in the result. Readiness callbacks must recheck the actual key
    /// state, since another client may have consumed or deleted it.
    ///
    /// # Errors
    ///
    /// Requires an SDK command callback, a real client, nonempty keys and the
    /// native APIs used by the waiter. A client already blocked by any SDK
    /// handle cannot block again; reply/timeout callbacks cannot reblock.
    /// Denied blocking permission returns [`KeyBlockingResult::Denied`] before
    /// native blocking, preserving `data` for a normal reply. An unexpected
    /// null native handle is an error and releases the private data.
    pub fn block_client_on_keys<T: Send + 'static>(
        &self,
        keys: &[&RedisString],
        timeout: KeyBlockingTimeout,
        unblock_on_delete: bool,
        data: T,
        on_ready: fn(&Context, &mut T) -> Result<Option<RedisValue>, RedisError>,
        on_timeout: fn(&Context, &mut T) -> RedisResult,
    ) -> Result<KeyBlockingResult<T>, RedisError> {
        // Even validation/API errors can drop user-owned data. Keep that
        // destruction guarded until either Redis or the Denied caller owns it.
        let mut data = KeysInput(Some(data));
        let count = key_count(keys.len())?;
        if !self.blocking_context_allowed()? {
            return Ok(KeyBlockingResult::Denied(data.take()));
        }
        // A module-call client may carry K but it is not a real network client.
        // It is not in the client index, so the lookup below returns None and
        // the call is denied.
        let Some(info) = self.indexed_network_client_info()? else {
            return Ok(KeyBlockingResult::Denied(data.take()));
        };
        if info.flags & u64::from(raw::REDISMODULE_CLIENTINFO_FLAG_BLOCKED) != 0 {
            return Err(RedisError::Str("ERR this client is already blocked"));
        }
        // An auth/load context can be rejected by native blocking only after
        // it has allocated a handle. Only tracked command invocations are
        // eligible here; native reply/timeout phases were rejected above.
        if crate::panic::current_command() == 0 {
            return Err(RedisError::Str(
                "ERR key blocking requires an SDK command callback",
            ));
        }
        if !super::timer::sdk_module_is_active(self) {
            return Err(RedisError::Str(
                "ERR key blocking requires an active SDK module",
            ));
        }
        let lifecycle = super::post_notification::Job::new(()).ok_or(RedisError::Str(
            "ERR key blocking requires an active SDK module",
        ))?;
        let block = unsafe { crate::__available_api!(RedisModule_BlockClientOnKeys) };
        let with_flags = unsafe { raw::RedisModule_BlockClientOnKeysWithFlags };
        unsafe { crate::__available_api!(RedisModule_GetBlockedClientPrivateData) };
        unsafe { crate::__available_api!(RedisModule_GetBlockedClientReadyKey) };
        unsafe { crate::__available_api!(RedisModule_ReplyWithError) };
        let deletion = unblock_on_delete && with_flags.is_some();
        let mut keys: Vec<_> = keys.iter().map(|key| key.as_ptr()).collect();
        let payload = Box::new(KeysPayload {
            state: ManuallyDrop::new(Arc::new(KeysState {
                data: Mutex::new(data.take()),
                completed: AtomicBool::new(false),
                _lifecycle: lifecycle,
            })),
            on_ready,
            on_timeout,
        });
        // Redis retains the key names during this call. The pointer vector
        // itself is only borrowed for the call, never kept by Redis.
        transfer_keys_payload(payload, |payload| unsafe {
            if let Some(block) = with_flags {
                block(
                    self.ctx,
                    Some(keys_ready::<T>),
                    Some(keys_timeout::<T>),
                    Some(free_keys_payload::<T>),
                    timeout.0,
                    keys.as_mut_ptr(),
                    count,
                    payload,
                    if deletion {
                        raw::REDISMODULE_BLOCK_UNBLOCK_DELETED as c_int
                    } else {
                        0
                    },
                )
            } else {
                block(
                    self.ctx,
                    Some(keys_ready::<T>),
                    Some(keys_timeout::<T>),
                    Some(free_keys_payload::<T>),
                    timeout.0,
                    keys.as_mut_ptr(),
                    count,
                    payload,
                )
            }
        })?;
        Ok(KeyBlockingResult::Blocked {
            unblock_on_delete: deletion,
        })
    }

    /// Signals a key in the calling client's selected database as ready.
    /// Waiters recheck the key through their readiness callback. On older Redis
    /// versions signaling a nonexistent key has no effect.
    pub fn signal_key_as_ready(&self, key: &RedisString) -> Result<(), RedisError> {
        let client_id = unsafe { crate::__available_api!(RedisModule_GetClientId) };
        if unsafe { client_id(self.ctx) } == 0 {
            return Err(RedisError::Str(
                "ERR this context has no client database to signal",
            ));
        }
        let signal = unsafe { crate::__available_api!(RedisModule_SignalKeyAsReady) };
        unsafe { signal(self.ctx, key.as_ptr()) };
        Ok(())
    }

    /// Copies the key name from a key-blocking readiness callback into an
    /// independent native string. Outside a readiness callback, returns None.
    /// This takes no ownership of Redis's borrowed ready-key object.
    pub fn blocked_client_ready_key(&self) -> Result<Option<RedisString>, RedisError> {
        let is_reply = unsafe { crate::__available_api!(RedisModule_IsBlockedReplyRequest) };
        if unsafe { is_reply(self.ctx) } == 0 {
            return Ok(None);
        }
        let ready_key = unsafe { crate::__available_api!(RedisModule_GetBlockedClientReadyKey) };
        let copy = unsafe { crate::__available_api!(RedisModule_CreateStringFromString) };
        let key = unsafe { ready_key(self.ctx) };
        if key.is_null() {
            return Ok(None);
        }
        let owned = NonNull::new(unsafe { copy(ptr::null_mut(), key) })
            .ok_or(RedisError::Str("ERR Redis did not copy the ready key"))?;
        Ok(Some(unsafe { RedisString::from_owned_ptr(None, owned) }))
    }

    /// Returns a writer for the reply of the key-blocking callback this context
    /// belongs to: the `on_ready` or `on_timeout` callback of
    /// [`Self::block_client_on_keys`]. See [`KeyBlockingReply`].
    ///
    /// ```no_run
    /// use redis_module::{Context, RedisError, RedisValue};
    ///
    /// struct Waiter {
    ///     entries: Vec<Vec<u8>>,
    /// }
    ///
    /// fn on_ready(ctx: &Context, waiter: &mut Waiter) -> Result<Option<RedisValue>, RedisError> {
    ///     if waiter.entries.is_empty() {
    ///         return Ok(None);
    ///     }
    ///     let mut reply = ctx.key_blocking_reply()?;
    ///     reply.array(waiter.entries.len())?;
    ///     for entry in &waiter.entries {
    ///         reply.bulk(entry)?;
    ///     }
    ///     Ok(Some(RedisValue::NoReply))
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error outside those callbacks, including from a context of
    /// another client or a nested command.
    pub fn key_blocking_reply(&self) -> Result<KeyBlockingReply<'_>, RedisError> {
        with_frame(self.ctx, |_| ())?;
        Ok(KeyBlockingReply { ctx: self })
    }
}

/// A writer for the reply of a key-blocking callback, from
/// [`Context::key_blocking_reply`].
///
/// It writes straight to the client, so a callback can send a large reply
/// without building a [`RedisValue`] for it first. [`Self::array`],
/// [`Self::map`] and [`Self::set`] open an aggregate that the next elements
/// fill; every other method writes one element. The writer counts what the
/// reply still owes, so the client gets exactly one reply:
///
/// - The first write claims the reply. It is refused if the client already has
///   a reply, for example when a nested `CLIENT UNBLOCK ... TIMEOUT` of the same
///   client ran first. If a nested command unblocks the client after the first
///   write, the reply still finishes, and Redis is left to release the client.
/// - A write after the reply is complete is refused, and sends nothing.
/// - Once anything is written, the wait finishes. Return
///   `Ok(Some(RedisValue::NoReply))` from `on_ready`, or `Ok(RedisValue::NoReply)`
///   from `on_timeout`. Another value or error the callback returns would be a
///   second reply, so the SDK logs a warning and drops it. `Ok(None)` cannot
///   keep the client waiting after a reply, and is taken as `NoReply`.
/// - If the callback returns or panics while an aggregate still owes elements,
///   the SDK writes each owed element as an error reply: the callback's error
///   if it returned one, otherwise `ERR key blocking callback left its reply
///   incomplete`, or `ERR key blocking callback panicked`. It writes every
///   owed element, on the main thread. Take a length from the data in hand,
///   not from a client's `COUNT`, so a callback that stops early does not leave
///   the SDK millions of error replies to write.
///
/// Replies written another way, through [`Context::reply`] or the raw
/// `RedisModule_ReplyWith*` functions, are not counted. Neither is the error
/// Redis writes for `CLIENT UNBLOCK ... ERROR` of the client from inside its
/// own callback, which lands inside a reply already started.
pub struct KeyBlockingReply<'ctx> {
    ctx: &'ctx Context,
}

impl KeyBlockingReply<'_> {
    /// Opens an array of `len` elements, which the next writes fill.
    ///
    /// # Errors
    ///
    /// Returns an error, writing nothing, if the reply is already complete, the
    /// client already has its reply, or `len` is out of range.
    pub fn array(&mut self, len: usize) -> Result<(), RedisError> {
        let (len, owed) = aggregate_len(len, 1)?;
        let ctx = self.ctx.ctx;
        self.write(owed, || raw::reply_with_array(ctx, len))
    }

    /// Opens a map of `pairs` key and value pairs: the next `2 * pairs` writes
    /// fill it, alternating keys and values. A RESP2 client gets a flat array.
    ///
    /// # Errors
    ///
    /// As [`Self::array`].
    pub fn map(&mut self, pairs: usize) -> Result<(), RedisError> {
        let (pairs, owed) = aggregate_len(pairs, 2)?;
        let ctx = self.ctx.ctx;
        self.write(owed, || raw::reply_with_map(ctx, pairs))
    }

    /// Opens a set of `len` members, which the next writes fill. A RESP2
    /// client gets an array.
    ///
    /// # Errors
    ///
    /// As [`Self::array`].
    pub fn set(&mut self, len: usize) -> Result<(), RedisError> {
        let (len, owed) = aggregate_len(len, 1)?;
        let ctx = self.ctx.ctx;
        self.write(owed, || raw::reply_with_set(ctx, len))
    }

    /// Writes a bulk string.
    ///
    /// # Errors
    ///
    /// Returns an error, writing nothing, if the reply is already complete or
    /// the client already has its reply.
    pub fn bulk(&mut self, bytes: &[u8]) -> Result<(), RedisError> {
        let ctx = self.ctx.ctx;
        self.write(0, || {
            raw::reply_with_string_buffer(ctx, bytes.as_ptr().cast(), bytes.len())
        })
    }

    /// Writes a Redis string as a bulk string, without copying it into Rust.
    ///
    /// # Errors
    ///
    /// As [`Self::bulk`].
    pub fn string(&mut self, string: &RedisString) -> Result<(), RedisError> {
        let ctx = self.ctx.ctx;
        self.write(0, || raw::reply_with_string(ctx, string.as_ptr()))
    }

    /// Writes an integer.
    ///
    /// # Errors
    ///
    /// As [`Self::bulk`].
    pub fn integer(&mut self, value: i64) -> Result<(), RedisError> {
        let ctx = self.ctx.ctx;
        self.write(0, || raw::reply_with_long_long(ctx, value))
    }

    /// Writes `value` as one element, the way a callback's returned reply is
    /// written. An aggregate value is written whole, with its elements.
    ///
    /// # Errors
    ///
    /// As [`Self::bulk`], and for [`RedisValue::NoReply`], which writes nothing.
    pub fn value(&mut self, value: RedisValue) -> Result<(), RedisError> {
        if matches!(value, RedisValue::NoReply) {
            return Err(RedisError::Str(
                "ERR a key blocking reply cannot write NoReply",
            ));
        }
        let ctx = self.ctx.ctx;
        self.write(0, || super::ReplyContext { ctx }.reply(Ok(value)))
    }

    /// Whether the reply is complete: one whole value has been written.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        with_frame(self.ctx.ctx, |frame| {
            frame.written.get() == Written::Complete
        })
        .unwrap_or(true)
    }

    /// Checks that one more element can be written, writes it, and counts it.
    /// `opens` is the number of elements an aggregate header needs, or 0.
    fn write(&mut self, opens: u64, write: impl FnOnce() -> raw::Status) -> Result<(), RedisError> {
        with_frame(self.ctx.ctx, |frame| {
            match frame.written.get() {
                Written::Complete => {
                    return Err(RedisError::Str(
                        "ERR the key blocking reply is already complete",
                    ));
                }
                // SAFETY: a readiness frame's flag lives in the waiter state
                // that the running callback keeps alive.
                Written::Nothing
                    if unsafe { frame.claim.as_ref() }
                        .is_some_and(|claim| claim.swap(true, Ordering::AcqRel)) =>
                {
                    return Err(RedisError::Str(
                        "ERR the blocked client already has its reply",
                    ));
                }
                _ => {}
            }
            // The reply is claimed now. If the write panics, the panic path
            // still owes the client a reply and sends an error.
            if frame.written.get() == Written::Nothing {
                frame.written.set(Written::Partial);
            }
            write();
            frame.count(opens);
            Ok(())
        })?
    }
}

/// The native length of an aggregate of `len` items of `per` elements each,
/// and the number of elements it owes.
fn aggregate_len(len: usize, per: u64) -> Result<(c_long, u64), RedisError> {
    u64::try_from(len)
        .ok()
        .and_then(|len| len.checked_mul(per))
        .filter(|owed| c_long::try_from(*owed).is_ok())
        .and_then(|owed| Some((c_long::try_from(len).ok()?, owed)))
        .ok_or(RedisError::Str(
            "ERR key blocking reply length is out of range",
        ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Written {
    Nothing,
    Partial,
    Complete,
}

thread_local! {
    /// The innermost running key-blocking callback on this thread, or null.
    /// A raw pointer has no destructor, which would keep glibc from unloading
    /// the module.
    static REPLY_FRAMES: Cell<*const ReplyFrame> = const { Cell::new(ptr::null()) };
}

/// What the reply writer of one running key-blocking callback has written. It
/// lives on the callback's stack, linked from [`REPLY_FRAMES`] while it runs.
struct ReplyFrame {
    ctx: *mut raw::RedisModuleCtx,
    /// The waiter's completion flag, which the first write claims. Null for a
    /// timeout callback, which claims the reply before it runs.
    claim: *const AtomicBool,
    /// How many elements each open aggregate still needs, innermost last. An
    /// open aggregate counts as one of the elements its parent still needs.
    owed: RefCell<Vec<u64>>,
    written: Cell<Written>,
    outer: Cell<*const ReplyFrame>,
}

/// Unlinks its frame when the callback finishes, even by unwinding.
struct FrameLink<'a>(&'a ReplyFrame);

impl Drop for FrameLink<'_> {
    fn drop(&mut self) {
        REPLY_FRAMES.set(self.0.outer.get());
    }
}

/// Runs `f` on the running callback frame of `ctx`.
fn with_frame<R>(
    ctx: *mut raw::RedisModuleCtx,
    f: impl FnOnce(&ReplyFrame) -> R,
) -> Result<R, RedisError> {
    let mut frame = REPLY_FRAMES.get();
    // SAFETY: linked frames are alive: each unlinks itself before its
    // callback's stack frame ends.
    while let Some(current) = unsafe { frame.as_ref() } {
        if current.ctx == ctx {
            return Ok(f(current));
        }
        frame = current.outer.get();
    }
    Err(RedisError::Str(
        "ERR a key blocking reply can only be written in a key blocking callback",
    ))
}

impl ReplyFrame {
    fn new(ctx: *mut raw::RedisModuleCtx, claim: Option<&AtomicBool>) -> Self {
        Self {
            ctx,
            claim: claim.map_or(ptr::null(), ptr::from_ref),
            owed: RefCell::new(Vec::new()),
            written: Cell::new(Written::Nothing),
            outer: Cell::new(ptr::null()),
        }
    }

    /// Links the frame until the returned guard drops.
    fn link(&self) -> FrameLink<'_> {
        self.outer.set(REPLY_FRAMES.get());
        REPLY_FRAMES.set(self);
        FrameLink(self)
    }

    fn started(&self) -> bool {
        self.written.get() != Written::Nothing
    }

    /// Counts one written element, or an aggregate header that needs `opens`
    /// more elements.
    fn count(&self, opens: u64) {
        let mut owed = self.owed.borrow_mut();
        if opens > 0 {
            owed.push(opens);
            self.written.set(Written::Partial);
            return;
        }
        loop {
            let Some(left) = owed.last_mut() else {
                self.written.set(Written::Complete);
                return;
            };
            *left -= 1;
            if *left > 0 {
                self.written.set(Written::Partial);
                return;
            }
            // That element finished an aggregate, which is one element of its
            // parent.
            owed.pop();
        }
    }

    /// Finishes a reply the writer started. `response` is what the callback
    /// returned after writing.
    fn finish(&self, response: RedisResult) {
        let partial = self.written.get() == Written::Partial;
        if partial {
            let message = match &response {
                Err(error) => error.to_string(),
                Ok(_) => "ERR key blocking callback left its reply incomplete".to_owned(),
            };
            self.complete(&message);
        }
        let dropped = match response {
            Ok(RedisValue::NoReply) => false,
            Err(_) => !partial,
            Ok(_) => true,
        };
        if dropped {
            log_warning(
                c"key blocking callback wrote its reply and also returned one; the SDK dropped the returned one",
            );
        }
    }

    /// Writes every element an incomplete reply owes as the error `message`.
    fn complete(&self, message: &str) {
        let reply = super::ReplyContext { ctx: self.ctx };
        while self.written.get() == Written::Partial {
            reply.reply_error_string(message);
            self.count(0);
        }
    }

    /// Finishes a reply the writer started before its callback panicked.
    fn complete_after_panic(&self) {
        if self.written.get() == Written::Partial {
            self.complete("ERR key blocking callback panicked");
        }
    }
}

fn log_warning(message: &std::ffi::CStr) {
    // SAFETY: a null context logs as the module, with a NUL-terminated message.
    unsafe {
        if let Some(log) = raw::RedisModule_Log {
            log(
                ptr::null_mut(),
                c"warning".as_ptr(),
                c"%s".as_ptr(),
                message.as_ptr(),
            );
        }
    }
}

fn key_count(count: usize) -> Result<c_int, RedisError> {
    c_int::try_from(count)
        .ok()
        .filter(|count| *count > 0)
        .ok_or(RedisError::Str(
            "ERR key blocking requires between 1 and i32::MAX keys",
        ))
}

struct KeysState<T> {
    data: Mutex<T>,
    completed: AtomicBool,
    // Last so user data and callback snapshots retain the unload lease through
    // all captured destructors, including a reentrant MODULE UNLOAD attempt.
    _lifecycle: super::post_notification::Job<()>,
}

struct KeysInput<T>(Option<T>);

impl<T> KeysInput<T> {
    fn take(&mut self) -> T {
        self.0.take().expect("key blocking input transfers once")
    }
}

impl<T> Drop for KeysInput<T> {
    fn drop(&mut self) {
        if let Some(data) = self.0.take() {
            drop_keys_payload(data);
        }
    }
}

struct KeysPayload<T> {
    state: ManuallyDrop<Arc<KeysState<T>>>,
    on_ready: fn(&Context, &mut T) -> Result<Option<RedisValue>, RedisError>,
    on_timeout: fn(&Context, &mut T) -> RedisResult,
}

impl<T> Drop for KeysPayload<T> {
    fn drop(&mut self) {
        // A callback snapshot can be the last owner after nested Redis work
        // frees the native box. Keep T's destructor inside a panic guard even
        // when that final snapshot is dropped on the callback's return path.
        drop_keys_payload(unsafe { ManuallyDrop::take(&mut self.state) });
    }
}

// Keep an owned box until the native call accepts it, so a null return cannot
// lose the private data. Native blocking does not invoke its callbacks during
// successful registration.
fn transfer_keys_payload<T>(
    mut payload: Box<KeysPayload<T>>,
    block: impl FnOnce(*mut c_void) -> *mut raw::RedisModuleBlockedClient,
) -> Result<(), RedisError> {
    let inner = block((&mut *payload as *mut KeysPayload<T>).cast());
    if inner.is_null() {
        drop_keys_payload(payload);
        return Err(RedisError::Str(
            "ERR Redis did not block the client on keys",
        ));
    }
    std::mem::forget(payload);
    Ok(())
}

unsafe fn keys_payload<T>(ctx: *mut raw::RedisModuleCtx) -> Option<KeysPayload<T>> {
    let ptr = unsafe { crate::__required_api!(RedisModule_GetBlockedClientPrivateData)(ctx) };
    unsafe { ptr.cast::<KeysPayload<T>>().as_ref() }.map(|payload| KeysPayload {
        state: ManuallyDrop::new(Arc::clone(&payload.state)),
        on_ready: payload.on_ready,
        on_timeout: payload.on_timeout,
    })
}

fn keys_reply(ctx: &Context, response: RedisResult) -> c_int {
    let response = match response {
        Ok(RedisValue::NoReply) => Err(RedisError::Str(
            "ERR key blocking callback returned NoReply",
        )),
        response => response,
    };
    ctx.reply(response);
    raw::REDISMODULE_OK as c_int
}

fn keys_panic(ctx: *mut raw::RedisModuleCtx, completed: Option<&AtomicBool>) -> c_int {
    if completed.is_none_or(|done| !done.swap(true, Ordering::AcqRel)) {
        unsafe {
            if let Some(reply) = raw::RedisModule_ReplyWithError {
                reply(ctx, c"ERR key blocking callback panicked".as_ptr());
            }
        }
        raw::REDISMODULE_OK as c_int
    } else {
        raw::REDISMODULE_ERR as c_int
    }
}

/// The status of a readiness callback that served its client. A nested command
/// in the callback, such as `CLIENT UNBLOCK` of the same client, can make Redis
/// unblock it already; reporting it served again would free the native waiter
/// twice, so the callback reports it not served instead.
fn served(ctx: *mut raw::RedisModuleCtx) -> c_int {
    if still_blocked(ctx) {
        raw::REDISMODULE_OK as c_int
    } else {
        raw::REDISMODULE_ERR as c_int
    }
}

/// Whether Redis still holds the client of this callback context blocked. If
/// the client cannot be looked up, it is taken as still blocked, as before
/// this check existed.
fn still_blocked(ctx: *mut raw::RedisModuleCtx) -> bool {
    // SAFETY: plain calls on the live callback context, whose client is the
    // blocked client, and a version 1 info struct Redis fills in.
    unsafe {
        let (Some(client_id), Some(client_info)) = (
            raw::RedisModule_GetClientId,
            raw::RedisModule_GetClientInfoById,
        ) else {
            return true;
        };
        let id = client_id(ctx);
        let mut info = raw::RedisModuleClientInfoV1 {
            version: 1,
            flags: 0,
            id: 0,
            addr: [0; 46],
            port: 0,
            db: 0,
        };
        id == 0
            || client_info((&mut info as *mut raw::RedisModuleClientInfoV1).cast(), id)
                != raw::REDISMODULE_OK as c_int
            || info.flags & u64::from(raw::REDISMODULE_CLIENTINFO_FLAG_BLOCKED) != 0
    }
}

extern "C" fn keys_ready<T: Send + 'static>(
    ctx: *mut raw::RedisModuleCtx,
    _argv: *mut *mut raw::RedisModuleString,
    _argc: c_int,
) -> c_int {
    // Snapshot the callbacks and Arc before user code can reenter Redis and
    // release the native box. A try_lock refuses reentrant mutable access.
    let Some(payload) = (unsafe { keys_payload::<T>(ctx) }) else {
        return keys_panic(ctx, None);
    };
    let frame = ReplyFrame::new(ctx, Some(&payload.state.completed));
    let _link = frame.link();
    let status = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if payload.state.completed.load(Ordering::Acquire) {
            return raw::REDISMODULE_ERR as c_int;
        }
        let mut data = match payload.state.data.try_lock() {
            Ok(data) => data,
            Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(TryLockError::WouldBlock) => return raw::REDISMODULE_ERR as c_int,
        };
        let context = unsafe { Context::new(ctx) };
        let response = (payload.on_ready)(&context, &mut data);
        if frame.started() {
            // The writer claimed the reply, so the wait is over.
            frame.finish(response.map(|value| value.unwrap_or(RedisValue::NoReply)));
            return served(ctx);
        }
        match response {
            Ok(None) => raw::REDISMODULE_ERR as c_int,
            response => {
                if payload.state.completed.load(Ordering::Acquire) {
                    return raw::REDISMODULE_ERR as c_int;
                }
                if !still_blocked(ctx) {
                    // A nested command unblocked the client, and Redis sent
                    // its reply.
                    payload.state.completed.store(true, Ordering::Release);
                    return raw::REDISMODULE_ERR as c_int;
                }
                let status = keys_reply(&context, response.map(|value| value.expect("Some reply")));
                payload.state.completed.store(true, Ordering::Release);
                status
            }
        }
    }));
    match status {
        Ok(status) => status,
        Err(panic) => {
            // Panic payload destructors can panic too; never drop one at the
            // FFI boundary. This also handles panic_any payloads safely.
            std::mem::forget(panic);
            if frame.started() {
                frame.complete_after_panic();
                served(ctx)
            } else if still_blocked(ctx) {
                keys_panic(ctx, Some(&payload.state.completed))
            } else {
                payload.state.completed.store(true, Ordering::Release);
                raw::REDISMODULE_ERR as c_int
            }
        }
    }
}

extern "C" fn keys_timeout<T: Send + 'static>(
    ctx: *mut raw::RedisModuleCtx,
    _argv: *mut *mut raw::RedisModuleString,
    _argc: c_int,
) -> c_int {
    let Some(payload) = (unsafe { keys_payload::<T>(ctx) }) else {
        return keys_panic(ctx, None);
    };
    // The timeout claims the reply before its callback runs.
    let frame = ReplyFrame::new(ctx, None);
    let _link = frame.link();
    let status = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if payload.state.completed.swap(true, Ordering::AcqRel) {
            return raw::REDISMODULE_OK as c_int;
        }
        let context = unsafe { Context::new(ctx) };
        let response = match payload.state.data.try_lock() {
            Ok(mut data) => (payload.on_timeout)(&context, &mut data),
            Err(TryLockError::Poisoned(poison)) => {
                (payload.on_timeout)(&context, &mut poison.into_inner())
            }
            Err(TryLockError::WouldBlock) => Err(RedisError::Str(
                "ERR key blocking callback is already running",
            )),
        };
        if frame.started() {
            frame.finish(response);
            return raw::REDISMODULE_OK as c_int;
        }
        keys_reply(&context, response)
    }));
    match status {
        Ok(status) => status,
        Err(panic) => {
            std::mem::forget(panic);
            if frame.started() {
                frame.complete_after_panic();
                raw::REDISMODULE_OK as c_int
            } else {
                // The timeout already claimed completion, but still owes a reply.
                keys_panic(ctx, None)
            }
        }
    }
}

fn drop_keys_payload<T>(payload: T) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))) {
        std::mem::forget(panic);
        unsafe {
            if let Some(log) = raw::RedisModule_Log {
                log(
                    ptr::null_mut(),
                    c"warning".as_ptr(),
                    c"%s".as_ptr(),
                    c"key blocking data destructor panicked".as_ptr(),
                );
            }
        }
    }
}

extern "C" fn free_keys_payload<T: Send + 'static>(
    _ctx: *mut raw::RedisModuleCtx,
    payload: *mut c_void,
) {
    if !payload.is_null() {
        drop_keys_payload(unsafe { Box::from_raw(payload.cast::<KeysPayload<T>>()) });
    }
}

#[cfg(test)]
mod key_blocking_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn no_reply(_: &Context, _: &mut Tracked) -> Result<Option<RedisValue>, RedisError> {
        Ok(None)
    }

    fn timeout(_: &Context, _: &mut Tracked) -> RedisResult {
        Ok(RedisValue::NullArray)
    }

    struct Tracked {
        drops: Arc<AtomicUsize>,
        panic: bool,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            if self.panic {
                std::panic::panic_any(PanicOnDrop);
            }
        }
    }

    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("panic payload destructor must never run");
        }
    }

    fn payload(drops: &Arc<AtomicUsize>, panic: bool) -> Box<KeysPayload<Tracked>> {
        Box::new(KeysPayload {
            state: ManuallyDrop::new(Arc::new(KeysState {
                data: Mutex::new(Tracked {
                    drops: Arc::clone(drops),
                    panic,
                }),
                completed: AtomicBool::new(false),
                _lifecycle: crate::context::post_notification::Job::new(())
                    .expect("unit-test lifecycle"),
            })),
            on_ready: no_reply,
            on_timeout: timeout,
        })
    }

    fn wrote() -> raw::Status {
        raw::Status::Ok
    }

    #[test]
    fn reply_writer_counts_nested_aggregates_and_refuses_extra_writes() {
        let ctx = NonNull::<raw::RedisModuleCtx>::dangling().as_ptr();
        let context = unsafe { Context::new(ctx) };
        assert!(context.key_blocking_reply().is_err());
        let claim = AtomicBool::new(false);
        let frame = ReplyFrame::new(ctx, Some(&claim));
        {
            let _link = frame.link();
            let mut reply = context.key_blocking_reply().unwrap();
            // [[a, b], c]
            reply.write(2, wrote).unwrap();
            assert!(
                claim.load(Ordering::SeqCst),
                "the first write claims the reply"
            );
            reply.write(2, wrote).unwrap();
            assert_eq!(*frame.owed.borrow(), [2, 2]);
            reply.write(0, wrote).unwrap();
            reply.write(0, wrote).unwrap();
            assert_eq!(*frame.owed.borrow(), [1]);
            assert!(!reply.is_complete());
            reply.write(0, wrote).unwrap();
            assert!(reply.is_complete());
            assert!(frame.owed.borrow().is_empty());
            let extra = reply.write(0, || panic!("a complete reply must not be written"));
            assert!(extra.unwrap_err().to_string().contains("already complete"));
        }
        assert!(context.key_blocking_reply().is_err(), "the frame unlinks");
    }

    #[test]
    fn reply_writer_empty_aggregates_and_single_values_complete_at_once() {
        let ctx = NonNull::<raw::RedisModuleCtx>::dangling().as_ptr();
        let context = unsafe { Context::new(ctx) };
        // A single value, or an empty aggregate, is a whole reply.
        let frame = ReplyFrame::new(ctx, None);
        let link = frame.link();
        context
            .key_blocking_reply()
            .unwrap()
            .write(0, wrote)
            .unwrap();
        assert_eq!(frame.written.get(), Written::Complete);
        drop(link);
        // An empty aggregate inside another counts as one element.
        let frame = ReplyFrame::new(ctx, None);
        let _link = frame.link();
        let mut reply = context.key_blocking_reply().unwrap();
        reply.write(2, wrote).unwrap();
        reply.write(0, wrote).unwrap();
        assert_eq!(*frame.owed.borrow(), [1]);
        reply.write(0, wrote).unwrap();
        assert!(reply.is_complete());
    }

    #[test]
    fn reply_writer_refuses_a_reply_another_callback_claimed() {
        let ctx = NonNull::<raw::RedisModuleCtx>::dangling().as_ptr();
        let context = unsafe { Context::new(ctx) };
        let claim = AtomicBool::new(true);
        let frame = ReplyFrame::new(ctx, Some(&claim));
        let _link = frame.link();
        let refused = context
            .key_blocking_reply()
            .unwrap()
            .write(1, || panic!("a claimed reply must not be written"));
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("already has its reply")
        );
        assert!(!frame.started());
    }

    #[test]
    fn reply_frames_nest_by_context_and_unlink_when_a_callback_unwinds() {
        let outer_ctx = NonNull::<raw::RedisModuleCtx>::dangling().as_ptr();
        let inner_ctx = outer_ctx.wrapping_byte_add(8);
        let outer = ReplyFrame::new(outer_ctx, None);
        let _outer = outer.link();
        let caught = std::panic::catch_unwind(|| {
            let inner = ReplyFrame::new(inner_ctx, None);
            let _inner = inner.link();
            let context = unsafe { Context::new(outer_ctx) };
            context
                .key_blocking_reply()
                .unwrap()
                .write(0, wrote)
                .unwrap();
            assert!(!inner.started());
            panic!("unwind through the inner callback");
        });
        assert!(caught.is_err());
        assert_eq!(REPLY_FRAMES.get(), ptr::from_ref(&outer));
        assert!(outer.started());
        assert!(with_frame(inner_ctx, |_| ()).is_err());
    }

    #[test]
    fn reply_aggregate_lengths_fit_redis_and_count_map_pairs_twice() {
        assert_eq!(aggregate_len(3, 1).unwrap(), (3, 3));
        assert_eq!(aggregate_len(3, 2).unwrap(), (3, 6));
        let max = usize::try_from(c_long::MAX).unwrap();
        assert!(aggregate_len(max, 1).is_ok());
        assert!(aggregate_len(max / 2 + 1, 2).is_err());
        if usize::MAX as u128 > c_long::MAX as u128 {
            assert!(aggregate_len(usize::MAX, 1).is_err());
        }
    }

    #[test]
    fn key_timeout_bounds_and_rounding_do_not_call_redis() {
        assert!(KeyBlockingTimeout::try_from(-1).is_err());
        assert_eq!(
            KeyBlockingTimeout::try_from(0).unwrap(),
            KeyBlockingTimeout::INFINITE
        );
        assert_eq!(
            KeyBlockingTimeout::try_from(Duration::ZERO)
                .unwrap()
                .as_milliseconds(),
            1
        );
        assert_eq!(
            KeyBlockingTimeout::try_from(Duration::from_nanos(1))
                .unwrap()
                .as_milliseconds(),
            1
        );
        assert_eq!(
            KeyBlockingTimeout::try_from(Duration::from_millis(123))
                .unwrap()
                .as_milliseconds(),
            123
        );
        let maximum = KeyBlockingTimeout::MAX_MILLISECONDS;
        assert_eq!(
            KeyBlockingTimeout::try_from(maximum)
                .unwrap()
                .as_milliseconds(),
            maximum
        );
        assert!(KeyBlockingTimeout::try_from(maximum + 1).is_err());
        assert!(KeyBlockingTimeout::try_from(i64::MAX).is_err());
        assert!(KeyBlockingTimeout::try_from(Duration::MAX).is_err());
    }

    #[test]
    fn key_count_checks_native_integer_bounds_without_allocating() {
        assert!(key_count(0).is_err());
        assert_eq!(key_count(1).unwrap(), 1);
        assert_eq!(key_count(c_int::MAX as usize).unwrap(), c_int::MAX);
        assert!(key_count(c_int::MAX as usize + 1).is_err());
    }

    #[test]
    fn native_null_releases_the_waiter_once_even_if_its_destructor_panics() {
        for panic in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            assert!(transfer_keys_payload(payload(&drops, panic), |_| ptr::null_mut()).is_err());
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn rejected_input_guards_the_data_destructor_before_native_registration() {
        let drops = Arc::new(AtomicUsize::new(0));
        drop(KeysInput(Some(Tracked {
            drops: Arc::clone(&drops),
            panic: true,
        })));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn accepted_native_waiter_has_one_eventual_free_and_snapshots_keep_it_alive() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut native = ptr::null_mut();
        transfer_keys_payload(payload(&drops, false), |payload| {
            native = payload;
            NonNull::<raw::RedisModuleBlockedClient>::dangling().as_ptr()
        })
        .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let payload = unsafe { &*native.cast::<KeysPayload<Tracked>>() };
        let snapshot = KeysPayload {
            state: ManuallyDrop::new(Arc::clone(&payload.state)),
            on_ready: payload.on_ready,
            on_timeout: payload.on_timeout,
        };
        let lock = snapshot.state.data.lock().unwrap();
        assert!(matches!(
            snapshot.state.data.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        free_keys_payload::<Tracked>(ptr::null_mut(), native);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(lock);
        drop(snapshot);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_last_callback_snapshot_guards_the_value_and_panic_payload_destructors() {
        let drops = Arc::new(AtomicUsize::new(0));
        let native = payload(&drops, true);
        let snapshot = KeysPayload {
            state: ManuallyDrop::new(Arc::clone(&native.state)),
            on_ready: native.on_ready,
            on_timeout: native.on_timeout,
        };
        drop(native);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(snapshot);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
