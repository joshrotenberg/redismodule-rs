use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::os::raw::{c_int, c_longlong};
use std::ptr::{self, NonNull};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::raw;
use crate::{Context, ContextFlags, RedisError, RedisResult};

/// A successfully blocked client, unblocked when this handle is dropped.
/// Send it to a worker and use [`crate::ThreadSafeContext::with_blocked_client`]
/// to reply. A successful command must return [`crate::RedisValue::NoReply`].
pub struct BlockedClient {
    pub(crate) inner: NonNull<raw::RedisModuleBlockedClient>,
}

// SAFETY: Redis permits UnblockClient and blocked-client thread-safe contexts
// on module workers. Ownership ensures one eventual UnblockClient call.
unsafe impl Send for BlockedClient {}

impl Drop for BlockedClient {
    fn drop(&mut self) {
        unsafe {
            crate::__required_api!(RedisModule_UnblockClient)(self.inner.as_ptr(), ptr::null_mut())
        };
    }
}

impl Context {
    /// Blocks a client so a worker can reply through a thread-safe context.
    /// Dropping the returned handle unblocks the client. Once this succeeds,
    /// return [`crate::RedisValue::NoReply`] and send the reply through that
    /// thread-safe context; returning a normal reply would send a second one.
    ///
    /// # Errors
    ///
    /// Rejects invocations with no client, scripts, transactions, module calls
    /// without blocking permission, and blocked-client reply/timeout callbacks,
    /// before calling Redis's blocking API. Also returns an error if a required
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
        Ok(BlockedClient { inner })
    }

    fn validate_blocking_context(&self) -> Result<(), RedisError> {
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
            return Err(RedisError::Str(
                "ERR this command cannot block here (MULTI, a script, or a call without K)",
            ));
        }
        // Redis 6.0 does not expose DENY_BLOCKING and its RM_Call clients
        // cannot block. Those temporary clients are not in the client index.
        let legacy_flags = unsafe { raw::RedisModule_GetServerVersion }
            .is_none_or(|version| unsafe { version() } < 0x0006_0200);
        if legacy_flags {
            let info = unsafe { crate::__available_api!(RedisModule_GetClientInfoById) };
            if unsafe { info(ptr::null_mut(), id) } != raw::REDISMODULE_OK as c_int {
                return Err(RedisError::Str(
                    "ERR this command cannot block here (MULTI, a script, or a call without K)",
                ));
            }
        }
        Ok(())
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
    /// the `K` flag, or it is the reply or timeout callback of another blocked
    /// client. Also returns an error if `timeout` is out of range, or the
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
