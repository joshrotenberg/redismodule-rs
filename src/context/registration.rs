//! Module initialization capabilities, borrowed for one load invocation.
use std::ffi::CString;
use std::ops::Deref;

use crate::{AclCategory, EventContext, ModuleOptions, RedisError, Status, raw};

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
    // This belongs to the load, including commands registered by init. It must
    // not survive a failed load or leak into another module's registrations.
    unregistered_acl_categories: Vec<String>,
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
        Self {
            context,
            unregistered_acl_categories: Vec::new(),
        }
    }

    /// Registration plumbing for `redis_module!`.
    #[doc(hidden)]
    pub fn __register_acl_categories(
        mut self,
        categories: impl IntoIterator<Item = AclCategory>,
    ) -> Result<Self, RedisError> {
        for category in categories {
            if category == AclCategory::None {
                continue;
            }
            let name = category.to_string();
            let Some(add) = (unsafe { raw::RedisModule_AddACLCategory }) else {
                if self.unregistered_acl_categories.is_empty() {
                    self.log_notice(
                        "Warning: Redis version does not support adding new ACL categories",
                    );
                }
                self.unregistered_acl_categories.push(name);
                continue;
            };
            let name_c = CString::new(name.as_str())?;
            if unsafe { add(self.context.get_raw(), name_c.as_ptr()) } == Status::Err as i32 {
                // Capture errno before formatting or logging can change it. Redis
                // keeps categories after unload; only EBUSY means the name exists.
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EBUSY) {
                    return Err(RedisError::String(format!(
                        "Error: failed to add ACL category `{name}`"
                    )));
                }
            }
        }
        Ok(self)
    }

    pub(crate) fn filter_optional_acl_categories(&self, categories: &str) -> String {
        let unregistered = &self.unregistered_acl_categories;
        // Redis separates category names with single spaces, treating empty
        // tokens as errors. Keep malformed input intact so it still warns.
        if unregistered.is_empty() || categories.split(' ').any(str::is_empty) {
            return categories.to_owned();
        }
        categories
            .split(' ')
            .filter(|name| {
                let declared = unregistered
                    .iter()
                    .any(|category| category.eq_ignore_ascii_case(name));
                // Built-in categories remain available even when a module
                // redundantly declares one on a server without AddACLCategory.
                let builtin = !matches!(
                    AclCategory::from(name.to_ascii_lowercase().as_str()),
                    AclCategory::Single(_) | AclCategory::Multi(_)
                );
                !declared || builtin
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Sets module behavior options during initialization.
    ///
    /// For example, `HANDLE_IO_ERRORS` enables handling persistence read errors
    /// instead of terminating the server. Each call replaces the stored mask.
    /// Redis stores bits it does not act on, such as
    /// `REQUIRE_LOADED_KEYS_IN_RAM` on Redis OSS, and neither rejects nor
    /// reports them; `RedisModule_GetModuleOptionsAll` does not list that bit,
    /// so it cannot detect support.
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
