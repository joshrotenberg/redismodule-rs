//! Module initialization capabilities, borrowed for one load invocation.
use std::ops::Deref;

use crate::{EventContext, ModuleOptions};

/// A module's load-time registration context.
///
/// `redis_module!` provides this to `init` functions. Commands, data types, and
/// configurations require it for registration. Ordinary command handlers keep
/// using [`Context`](crate::Context), which cannot be promoted to registration by safe code.
/// The wrapper borrows the load invocation and cannot be cloned or retained.
/// Shared operations are forwarded through [`EventContext`] to
/// [`RuntimeContext`](crate::RuntimeContext). This view cannot reply to or block a client.
///
/// A command context cannot register a command:
///
/// ```compile_fail,E0308
/// use redis_module::{Context, RedisResult, RedisString, redis_command};
/// fn ping(_: &Context, _: Vec<RedisString>) -> RedisResult { Ok("PONG".into()) }
/// fn register(ctx: &Context) {
///     redis_command!("example.ping", ping).register(ctx).unwrap();
/// }
/// ```
///
/// Loading configurations also requires the load-time capability:
///
/// ```compile_fail,E0308
/// fn load(ctx: &redis_module::Context) {
///     redis_module::configuration::load_configurations(ctx).unwrap();
/// }
/// ```
///
/// Data-type registration has the same requirement:
///
/// ```compile_fail,E0308
/// use redis_module::{Context, native_types::RedisType};
/// fn register(ty: &RedisType, ctx: &Context) { ty.create_data_type(ctx).unwrap(); }
/// ```
///
/// Module options are not exposed on runtime contexts:
///
/// ```compile_fail,E0599
/// fn options(ctx: &redis_module::Context) {
///     ctx.set_module_options(redis_module::ModuleOptions::HANDLE_IO_ERRORS);
/// }
/// ```
///
/// Runtime code cannot construct the capability without an unsafe call:
///
/// ```compile_fail,E0133
/// fn promote(ctx: &redis_module::EventContext) {
///     let _ = redis_module::RegistrationContext::from_context(ctx);
/// }
/// ```
///
/// The Redis lock proof cannot cross into another thread:
///
/// ```compile_fail,E0277
/// fn cross_thread(ctx: &redis_module::RegistrationContext<'_>) {
///     std::thread::scope(|scope| {
///         scope.spawn(|| ctx.log_notice("not on the Redis thread"));
///     });
/// }
/// ```
///
/// The load context cannot escape its backing context:
///
/// ```compile_fail,E0515
/// use redis_module::{EventContext, RegistrationContext};
/// fn escape(raw: *mut redis_module::raw::RedisModuleCtx) -> RegistrationContext<'static> {
///     let context = unsafe { EventContext::new(raw) };
///     unsafe { RegistrationContext::from_context(&context) }
/// }
/// ```
///
/// Initialization has no client reply target:
///
/// ```compile_fail,E0599
/// fn init(ctx: &redis_module::RegistrationContext<'_>) { ctx.reply(Ok("OK".into())); }
/// ```
#[derive(Debug)]
pub struct RegistrationContext<'load> {
    context: &'load EventContext,
}

impl<'load> RegistrationContext<'load> {
    /// Runs a call-user scope while preserving registration capabilities.
    /// See [`RuntimeContext::with_call_user`](crate::RuntimeContext::with_call_user).
    pub fn with_call_user<'s, T>(
        &'s self,
        user_name: &crate::RedisString,
        f: impl FnOnce(&'s RegistrationContext<'load>) -> T,
    ) -> Result<T, crate::RedisError> {
        self.context.with_call_user(user_name, |_| f(self))
    }

    /// Wraps a module-owned context for its active load invocation.
    ///
    /// The SDK calls this after Redis module initialization succeeds. Custom
    /// native loaders may use it under the same contract.
    ///
    /// # Safety
    ///
    /// `context` must belong to this module's current `RedisModule_OnLoad`
    /// invocation, after successful module initialization. Its native module
    /// association must be live, and the Redis lock must be held. The wrapper
    /// and any references derived from it must not outlive that invocation.
    pub const unsafe fn from_context(context: &'load EventContext) -> Self {
        Self { context }
    }

    /// Sets module behavior options during initialization.
    ///
    /// For example, `HANDLE_IO_ERRORS` enables handling persistence read errors
    /// instead of terminating the server. Redis replaces the option mask.
    pub fn set_module_options(&self, options: ModuleOptions) {
        unsafe {
            crate::__required_api!(RedisModule_SetModuleOptions)(
                self.context.get_raw(),
                options.bits(),
            )
        };
    }
}

impl Deref for RegistrationContext<'_> {
    type Target = EventContext;

    fn deref(&self) -> &Self::Target {
        self.context
    }
}

// SAFETY: the constructor requires the Redis lock for the entire borrowed load
// invocation; the wrapper cannot outlive that borrow or cross threads.
unsafe impl crate::RedisLockIndicator for RegistrationContext<'_> {}
