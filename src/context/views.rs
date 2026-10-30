//! Capabilities shared by client, event, and locked worker invocations.
use super::*;
use std::cell::OnceCell;

/// Operations that need a live Redis context and the Redis lock, but no client
/// reply target or module ownership: calls, keys, database selection, logging,
/// and scoped users for ACL-checked calls.
/// Helpers shared by commands, events, init hooks, and workers should take this
/// type. Each more specialized view dereferences to it.
///
/// A shared runtime view cannot be promoted to a client invocation:
///
/// ```compile_fail,E0308
/// fn needs_client(_: &redis_module::Context) {}
/// fn helper(ctx: &redis_module::RuntimeContext) { needs_client(ctx); }
/// ```
#[derive(Debug)]
pub struct RuntimeContext {
    pub(crate) ctx: *mut raw::RedisModuleCtx,
    pub(super) call_users: OnceCell<super::call_user::CallUserView>,
}

impl RuntimeContext {
    /// Wraps a locked native context without granting client or module capabilities.
    ///
    /// # Safety
    ///
    /// `ctx` must be a live Redis callback or thread-safe context. The Redis lock
    /// must be held on this thread throughout every use of this wrapper and any
    /// references derived from it. Neither may outlive the native invocation or
    /// lock scope, including when the wrapper is dropped. A raw pointer alone
    /// does not prove any of these conditions.
    pub const unsafe fn new(ctx: *mut raw::RedisModuleCtx) -> Self {
        Self {
            ctx,
            call_users: OnceCell::new(),
        }
    }

    /// A null context, only for native APIs that explicitly permit it (logging).
    ///
    /// # Safety
    ///
    /// The Redis lock must be held for every use. Callers must only invoke APIs
    /// that permit a null context, including through any derived references.
    pub const unsafe fn dummy() -> Self {
        Self {
            ctx: ptr::null_mut(),
            call_users: OnceCell::new(),
        }
    }
}

// SAFETY: construction requires the lock for the entire use of this view.
unsafe impl RedisLockIndicator for RuntimeContext {}

/// A module-owned event invocation. Provides shared runtime operations and APIs
/// that associate callbacks with the module, including timers and subscriptions.
/// Keyspace, server, timer, post-notification, and event-loop callbacks receive
/// this view. It has no client reply, client blocking, or load-time registration.
///
/// ```compile_fail,E0599
/// fn event(ctx: &redis_module::EventContext) { ctx.reply(Ok("OK".into())); }
/// ```
///
/// ```compile_fail,E0599
/// fn event(ctx: &redis_module::EventContext) { ctx.block_client(); }
/// ```
///
/// ```compile_fail,E0599
/// fn event(ctx: &redis_module::EventContext) { ctx.key_at_pos(1); }
/// ```
///
/// ```compile_fail,E0308
/// use redis_module::{Context, EventContext, RedisResult, RedisString, redis_command};
/// fn ping(_: &Context, _: Vec<RedisString>) -> RedisResult { Ok("PONG".into()) }
/// fn event(ctx: &EventContext) { redis_command!("x.ping", ping).register(ctx).unwrap(); }
/// ```
///
/// ```compile_fail,E0277
/// fn event(ctx: &redis_module::EventContext) {
///     std::thread::scope(|scope| { scope.spawn(|| ctx.log_notice("other thread")); });
/// }
/// ```
#[derive(Debug)]
pub struct EventContext {
    runtime: RuntimeContext,
}

impl EventContext {
    /// Runs a call-user scope while preserving this view's capabilities.
    /// See [`RuntimeContext::with_call_user`] for restoration and error behavior.
    pub fn with_call_user<'s, T>(
        &'s self,
        user_name: &RedisString,
        f: impl FnOnce(&'s EventContext) -> T,
    ) -> Result<T, RedisError> {
        self.runtime.with_call_user(user_name, |_| f(self))
    }

    /// Runs a database scope while preserving event capabilities.
    pub fn with_db<'s, T>(
        &'s self,
        db: i32,
        f: impl FnOnce(&'s EventContext) -> T,
    ) -> Result<T, RedisError> {
        self.runtime.with_db(db, |_| f(self))
    }

    /// Wraps a native callback belonging to this module, without a reply target.
    ///
    /// # Safety
    ///
    /// All requirements of [`RuntimeContext::new`] apply. In addition, `ctx`
    /// must have a live native association with this module. In particular, a
    /// context from `RedisModule_GetThreadSafeContext(NULL)` is not sufficient.
    pub const unsafe fn new(ctx: *mut raw::RedisModuleCtx) -> Self {
        Self {
            runtime: unsafe { RuntimeContext::new(ctx) },
        }
    }
}

impl Deref for EventContext {
    type Target = RuntimeContext;
    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

// SAFETY: the constructor requires the Redis lock throughout use.
unsafe impl RedisLockIndicator for EventContext {}

/// The view borrowed from a locked worker guard. Shared runtime operations are
/// available until that guard is dropped. It cannot reply to or block a client,
/// register commands, or retain module callbacks through timers/subscriptions.
/// A blocked client's [`ThreadSafeContext`](crate::ThreadSafeContext) retains its
/// separate reply API, which does not require locking.
///
/// ```compile_fail,E0599
/// fn worker(ctx: &redis_module::WorkerContext) { ctx.reply(Ok("OK".into())); }
/// ```
///
/// ```compile_fail,E0599
/// fn worker(ctx: &redis_module::WorkerContext) { ctx.block_client(); }
/// ```
///
/// ```compile_fail,E0599
/// fn worker(ctx: &redis_module::WorkerContext) {
///     ctx.subscribe_to_keyspace_events(redis_module::NotifyEvent::STRING,
///         redis_module::redis_event_handler!(|_: &redis_module::EventContext, _, _, _| {}));
/// }
/// ```
///
/// ```compile_fail,E0599
/// fn worker(ctx: &redis_module::WorkerContext) {
///     ctx.subscribe_to_server_event(redis_module::server_events::ServerEventHandler::Shutdown(|_| {}));
/// }
/// ```
///
/// ```compile_fail,E0599
/// fn worker(ctx: &redis_module::WorkerContext) { ctx.replicate_verbatim(); }
/// ```
///
/// ```compile_fail,E0277
/// fn worker(ctx: &redis_module::WorkerContext) {
///     std::thread::scope(|scope| { scope.spawn(|| ctx.log_notice("other thread")); });
/// }
/// ```
#[derive(Debug)]
pub struct WorkerContext {
    runtime: RuntimeContext,
    // Used by the blocking-call API on Redis 7.2 and later compatibility floors.
    #[allow(dead_code)]
    module_owned: bool,
}

impl WorkerContext {
    pub(super) fn release_call_users(&mut self) {
        self.runtime.release_call_users();
    }

    /// Runs a call-user scope while preserving this view's capabilities.
    /// See [`RuntimeContext::with_call_user`] for restoration and error behavior.
    pub fn with_call_user<'s, T>(
        &'s self,
        user_name: &RedisString,
        f: impl FnOnce(&'s WorkerContext) -> T,
    ) -> Result<T, RedisError> {
        self.runtime.with_call_user(user_name, |_| f(self))
    }

    /// Runs a database scope while preserving the worker view.
    pub fn with_db<'s, T>(
        &'s self,
        db: i32,
        f: impl FnOnce(&'s WorkerContext) -> T,
    ) -> Result<T, RedisError> {
        self.runtime.with_db(db, |_| f(self))
    }

    // Only guards construct these; safe code cannot extract an owned lock proof.
    pub(super) const unsafe fn new(ctx: *mut raw::RedisModuleCtx) -> Self {
        Self {
            runtime: unsafe { RuntimeContext::new(ctx) },
            module_owned: false,
        }
    }

    pub(super) const unsafe fn from_module_context(ctx: *mut raw::RedisModuleCtx) -> Self {
        Self {
            runtime: unsafe { RuntimeContext::new(ctx) },
            module_owned: true,
        }
    }

    /// Makes a blocking call from a module-owned detached worker. An ordinary
    /// thread-safe context has no module association and returns an error before
    /// calling Redis. The unblock handler receives an [`EventContext`].
    #[cfg(any(
        feature = "min-redis-compatibility-version-8-0",
        feature = "min-redis-compatibility-version-7-4",
        feature = "min-redis-compatibility-version-7-2"
    ))]
    pub fn call_blocking<
        'ctx,
        'a,
        T: Into<StrCallArgs<'a>>,
        R: From<PromiseCallReply<'ctx, 'ctx>>,
    >(
        &'ctx self,
        command: &str,
        options: &BlockingCallOptions,
        args: T,
    ) -> R {
        if !self.module_owned {
            return R::from(PromiseCallReply::Resolved(Err(ErrorReply::Message(
                "ERR blocking calls require a module-owned worker context".into(),
            ))));
        }
        if let Some(refusal) = super::call_reply::blocking_call_refusal() {
            return R::from(PromiseCallReply::Resolved(Err(ErrorReply::Message(
                refusal.into(),
            ))));
        }
        self.call_internal(command, options.options.as_ptr(), args)
    }
}

impl Deref for WorkerContext {
    type Target = RuntimeContext;
    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

// SAFETY: only live locked guards expose a reference to a worker view.
unsafe impl RedisLockIndicator for WorkerContext {}

#[cfg(all(
    test,
    any(
        feature = "min-redis-compatibility-version-8-0",
        feature = "min-redis-compatibility-version-7-4",
        feature = "min-redis-compatibility-version-7-2"
    )
))]
mod tests {
    use super::*;

    #[test]
    fn moduleless_worker_rejects_blocking_before_native_call() {
        // No native APIs are initialized. The rejection must not touch Redis.
        let worker = WorkerContext {
            runtime: RuntimeContext {
                ctx: ptr::null_mut(),
                call_users: OnceCell::new(),
            },
            module_owned: false,
        };
        let result: PromiseCallReply = worker.call_blocking(
            "BLPOP",
            &CallOptionsBuilder::new().build_blocking(),
            &["list", "1"],
        );
        match result {
            PromiseCallReply::Resolved(Err(ErrorReply::Message(message))) => {
                assert!(message.contains("module-owned worker context"));
            }
            _ => panic!("moduleless worker reached native blocking call"),
        }
    }
}
