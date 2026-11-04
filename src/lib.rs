//! Build native Redis modules in Rust.
//!
//! This is the 3.0 SDK fork of `RedisLabsModules/redismodule-rs`. It is consumed
//! from Git; the published docs.rs 2.x crate describes the upstream API. Generate
//! this reference with `cargo doc --open` from the same revision as your module's
//! dependency.
//!
//! # Module entry point and commands
//!
//! Build a `cdylib` and use [`redis_module!`] to register its name, version,
//! allocator, commands and optional lifecycle hooks. Command handlers receive a
//! [`Context`] and owned [`RedisString`] arguments, and return [`RedisResult`]:
//!
//! ```
//! use redis_module::{Context, RedisResult, RedisString};
//! fn ping(_: &Context, _: Vec<RedisString>) -> RedisResult {
//!     Ok("PONG".into())
//! }
//! ```
//!
//! The macro wraps command dispatch in the SDK's panic and reply handling.
//! [`commands::CommandBuilder`] adds flags, key specifications and ACL metadata
//! when a legacy command tuple is insufficient.
//!
//! # Context views
//!
//! [`RegistrationContext`] is for load-time registration. [`Context`] is a
//! command view, [`EventContext`] is for server/keyspace events, and
//! [`WorkerContext`] is obtained while a worker holds Redis's lock. Shared
//! operations live on [`RuntimeContext`]. [`ThreadSafeContext`] and
//! [`DetachedContext`] provide worker lock guards; retaining a native pointer
//! alone does not establish a valid context, thread or lock scope.
//!
//! # Keys, arguments and replies
//!
//! Open keys through [`RuntimeContext::open_key`] or
//! [`RuntimeContext::open_key_writable`]. Handles borrow the opening context.
//! Read the individual methods' access and aliasing requirements before keeping
//! native views across another Redis call. [`ArgCursor`] provides checked
//! argument parsing; [`RedisString::try_as_str`] validates text, while
//! [`RedisString::as_slice`] keeps arbitrary bytes intact.
//!
//! Return a [`RedisValue`] for a normal command reply. Arrays and maps may nest
//! other values. [`RedisValue::Null`] is a RESP2 null bulk string;
//! [`RedisValue::NullArray`] is a RESP2 null array; both are null in RESP3.
//! [`RedisValue::NoReply`] is for commands whose reply is supplied another way.
//! Root [`CallResult`]/[`CallReply`] values own the native result and are
//! scoped to the calling context. Nested replies and views borrow their parent.
//! Copy data before the relevant owner or legal Redis-access scope ends.
//!
//! # API floors and build requirements
//!
//! Rust 1.87 is the MSRV; this repository pins Rust 1.94.0. A C compiler builds
//! the native API stub. Select exactly one
//! `min-redis-compatibility-version-*` feature, disabling defaults for a floor
//! other than the default 6.0. The floor selects compile-time API assumptions,
//! not the Redis server process. Test your module on its actual oldest server.
//! Use [`RuntimeContext::api_available`] when an optional operation needs a
//! runtime availability check.
//!
//! # Module-author guides
//!
//! The matching source checkout includes `README.md` and the `docs/` guides:
//! `MIGRATING-3.0.md`, `COMPATIBILITY-FLOORS.md`, `CONFIGURATION.md`,
//! `KEYSPACE-NOTIFICATIONS.md`, `TESTING-MODULES.md`, `SHIPPING-MODULES.md` and
//! `UPGRADING-MODULES.md`. Open these from the dependency revision pinned by
//! `Cargo.lock`; the README links the guides relative to that source tree.
//! This avoids substituting current master documentation for a pinned SDK.
//!
//! For unwrapped APIs, [`raw`] contains the native bindings. A raw context/key
//! pointer does not transfer ownership or authorize calls outside Redis's native
//! callback, thread and lifetime rules.

pub use crate::context::InfoContext;

// Paths used by registration macros. Consumers do not need to declare the
// SDK's registration implementation dependencies themselves.
#[doc(hidden)]
pub mod __private {
    pub use crate::configuration::lifecycle::{ConfigurationLoad, ConfigurationUnload};
    pub use crate::context::blocked::__command_reply_after_release as command_reply_after_release;
    pub use crate::context::call_reply::{BlockingCallLoadGuard, BlockingCallUnloadGuard};
    pub use crate::context::event_loop::{EventLoopLoadGuard, EventLoopUnloadGuard};
    pub use crate::context::keyspace_events::keyspace_subscription_available;
    pub use crate::context::post_notification::{
        PostNotificationLoadGuard, PostNotificationUnloadGuard,
    };
    pub use crate::context::timer::{TimerLoadGuard, TimerUnloadGuard};
    pub use ::libc::EBUSY;
    pub use ::linkme;
    pub use ::redis_module_macros_internals::__register_command_list;
}

pub mod alloc;
pub mod apierror;
mod args;
pub mod cluster;
pub mod error;
pub mod io;
pub mod native_types;
pub mod panic;
pub mod raw;
pub mod rediserror;
mod redismodule;
pub mod redisraw;
pub mod redisvalue;
pub mod stream;

pub mod configuration;
mod context;
pub mod key;
pub mod logging;
mod macros;
mod module_version;

pub use crate::module_version::encode_module_version;

pub use crate::args::ArgCursor;
pub use crate::context::blocked::{
    BlockedClient, KeyBlockingResult, KeyBlockingTimeout, ReplyingBlockedClient,
};
pub use crate::context::thread_safe::{
    ContextGuard, DetachedFromClient, RedisGILGuard, RedisLockIndicator, ThreadSafeContext,
};
pub use crate::context::timer::TimerHandle;
pub use crate::io::RedisModuleIO;
// The raw types that the safe API uses; the rest of the raw bindings are
// under `raw`.
pub use crate::raw::{KeyType, ModuleOptions, NotifyEvent, Status, Version};

pub use crate::configuration::ConfigurationValue;
pub use crate::configuration::EnumConfigurationValue;
pub use crate::context::call_reply::{CallReply, CallResult, ErrorReply, PromiseCallReply};
pub use crate::context::call_reply::{FutureCallReply, FutureHandler};
pub use crate::context::commands;
pub use crate::context::commands::{CommandBuilder, CommandFlags, KeyRange};
pub use crate::context::defrag;
pub use crate::context::event_loop;
pub use crate::context::key_cursor::ScanKeyCursor;
pub use crate::context::keys_cursor::KeysCursor;
pub use crate::context::keyspace_events::KeyspaceEventHandler;
pub use crate::context::server_events;
pub use crate::context::{
    CallUserScope, EventContext, RegistrationContext, RuntimeContext, WorkerContext,
};
pub use common::AclCategory;

pub use crate::context::AclChannelPermissions;
pub use crate::context::AclLogReason;
pub use crate::context::AclPermissions;
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
pub use crate::context::BlockingCallOptions;
pub use crate::context::CallOptionResp;
pub use crate::context::CallOptions;
pub use crate::context::CallOptionsBuilder;
pub use crate::context::Context;
pub use crate::context::ContextFlags;
pub use crate::context::DetachedContext;
pub use crate::context::DetachedContextGuard;
pub use crate::context::YieldMode;
pub use crate::context::{
    InfoContextBuilderFieldBottomLevelValue, InfoContextBuilderFieldTopLevelValue,
    InfoContextFieldBottomLevelData, InfoContextFieldTopLevelData, OneInfoSectionData,
};

pub use crate::redismodule::*;
use context::server_events::INFO_COMMAND_HANDLER_LIST;

/// The detached Redis module context (the context of this module). It
/// is only set to a proper value after the module is initialised via the
/// provided [redis_module] macro.
/// See [DetachedContext].
pub static MODULE_CONTEXT: DetachedContext = DetachedContext::new();

#[deprecated(
    since = "2.1.0",
    note = "Please use the redis_module::logging::RedisLogLevel directly instead."
)]
pub type LogLevel = logging::RedisLogLevel;

fn add_trace_info(ctx: &InfoContext) -> RedisResult<()> {
    const SECTION_NAME: &str = "trace";
    const FIELD_NAME: &str = "backtrace";

    let current_backtrace = std::backtrace::Backtrace::force_capture();
    let trace = format!("{current_backtrace:?}");

    ctx.builder()
        .add_section(SECTION_NAME)
        .field(FIELD_NAME, trace)?
        .build_section()?
        .build_info()?;

    Ok(())
}

/// A custom INFO handler. Add fields through [`InfoContext::builder`] and
/// return success, or return an error for the dispatcher to log.
///
/// The boolean is `for_crash_report`. Preserve it when forwarding to other
/// handlers. A crash callback can interrupt state access or run on another
/// thread; ordinary INFO handlers should return early when it is true. See
/// [`InfoContext`] for the shared-state restrictions and an atomic snapshot.
pub type InfoHandlerFunctionType = fn(&InfoContext, bool) -> RedisResult<()>;

/// Default "INFO" command handler for the module.
///
/// This function can be invoked, for example, by sending `INFO modules`
/// through the RESP protocol.
pub fn basic_info_command_handler(ctx: &InfoContext, for_crash_report: bool) {
    if for_crash_report {
        if let Err(e) = add_trace_info(ctx) {
            log::error!("Couldn't send info for the module: {e}");
            return;
        }
    }

    INFO_COMMAND_HANDLER_LIST
        .iter()
        .filter_map(|callback| callback(ctx, for_crash_report).err())
        .for_each(|e| log::error!("Couldn't build info for the module's custom handler: {e}"));
}

/// Read the current Unix time in milliseconds through Redis's module API.
///
/// This is a fresh wall-clock reading, not a cached command timestamp or a
/// monotonic counter. System clock adjustments can move it backward. Use
/// [`std::time::Instant`] for measuring elapsed time.
///
/// No context or Redis lock is needed. It may be called from a module worker
/// after the module API has been initialized. The native API has been
/// available since Redis 4.0 and is supported at every SDK compatibility floor.
///
/// # Panics
///
/// Panics with the native API name if called before module API initialization
/// or if the running server does not provide `RedisModule_Milliseconds`.
///
/// ```no_run
/// let now_ms = redis_module::milliseconds();
/// ```
#[must_use]
pub fn milliseconds() -> i64 {
    // SAFETY: availability is checked before calling the no-argument native
    // clock, which reads fresh wall time without a context or server state.
    unsafe { crate::__required_api!(RedisModule_Milliseconds)() }
}

/// Initialize RedisModuleAPI without register as a module.
pub fn init_api(ctx: &RuntimeContext) {
    unsafe { crate::raw::Export_RedisModule_InitAPI(ctx.ctx) };
}

#[cfg(test)]
mod clock_tests {
    #[test]
    #[should_panic(expected = "RedisModule_Milliseconds is not available")]
    fn milliseconds_requires_api_initialization() {
        // No Redis server initializes module API pointers in this test binary.
        // Do not change the shared native pointer to simulate initialization.
        let _ = crate::milliseconds();
    }
}
