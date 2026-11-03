use std::ffi::CStr;
use std::sync::Mutex;

use crate::{InfoContext, RedisResult, raw};
use crate::{RedisError, context::EventContext};
use linkme::distributed_slice;

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub enum ServerRole {
    Primary,
    Replica,
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub enum LoadingSubevent {
    RdbStarted,
    AofStarted,
    ReplStarted,
    Ended,
    Failed,
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub enum FlushSubevent {
    Started,
    Ended,
}

/// Details copied from a flush server event. Available to
/// `redis_module_macros::flush_info_event_handler` callbacks. For an
/// asynchronous flush, `Ended` means the old keyspace was detached, not that
/// background freeing has finished.
///
/// ```no_run
/// use redis_module::{EventContext, server_events::{FlushInfo, FlushSubevent}};
/// use redis_module_macros::flush_info_event_handler;
/// #[flush_info_event_handler]
/// fn on_flush(ctx: &EventContext, phase: FlushSubevent, info: FlushInfo) {
///     if phase == FlushSubevent::Started && info.db == Some(2) {
///         ctx.log_notice("database 2 is being flushed");
///     }
/// }
/// ```
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub struct FlushInfo {
    /// The affected database, or `None` for all databases (`FLUSHALL`).
    pub db: Option<i32>,
}

/// Database identifiers copied from a successful `SWAPDB` event.
/// Delivered after the swap, in the order supplied to the command. The event
/// context's selected database does not identify either side of the swap.
///
/// ```no_run
/// use redis_module::{EventContext, server_events::SwapDbInfo};
/// use redis_module_macros::swapdb_event_handler;
/// #[swapdb_event_handler]
/// fn on_swap(ctx: &EventContext, info: SwapDbInfo) {
///     ctx.log_notice(&format!("swapped {} and {}", info.first, info.second));
/// }
/// ```
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub struct SwapDbInfo {
    /// The first database supplied to `SWAPDB`.
    pub first: i32,
    /// The second database supplied to `SWAPDB`.
    pub second: i32,
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub enum ModuleChangeSubevent {
    Loaded,
    Unloaded,
}

/// A static function handler for a typed server event.
///
/// Register with [`EventContext::subscribe_to_server_event`]. Payloads and
/// contexts are borrowed only for the invocation; copy any data to retain it.
#[derive(Clone, Copy)]
pub enum ServerEventHandler {
    /// Replication role changes. The historical variant name is preserved.
    RuleChanged(fn(&EventContext, ServerRole)),
    Loading(fn(&EventContext, LoadingSubevent)),
    Flush(fn(&EventContext, FlushSubevent)),
    FlushInfo(fn(&EventContext, FlushSubevent, FlushInfo)),
    SwapDb(fn(&EventContext, SwapDbInfo)),
    ModuleChange(fn(&EventContext, ModuleChangeSubevent)),
    ConfigChanged(fn(&EventContext, &[&str])),
    Cron(fn(&EventContext, u64)),
    Shutdown(fn(&EventContext)),
}

// Each native module image has its own registry. No user code runs under this
// lock. EventContext proves the Redis lock, serializing native registration.
static RUNTIME_HANDLERS: Mutex<Vec<ServerEventHandler>> = Mutex::new(Vec::new());

impl ServerEventHandler {
    fn same_handler(self, other: Self) -> bool {
        match (self, other) {
            (Self::RuleChanged(a), Self::RuleChanged(b)) => std::ptr::fn_addr_eq(a, b),
            (Self::Loading(a), Self::Loading(b)) => std::ptr::fn_addr_eq(a, b),
            (Self::Flush(a), Self::Flush(b)) => std::ptr::fn_addr_eq(a, b),
            (Self::FlushInfo(a), Self::FlushInfo(b)) => std::ptr::fn_addr_eq(a, b),
            (Self::SwapDb(a), Self::SwapDb(b)) => std::ptr::fn_addr_eq(a, b),
            (Self::ModuleChange(a), Self::ModuleChange(b)) => std::ptr::fn_addr_eq(a, b),
            (Self::ConfigChanged(a), Self::ConfigChanged(b)) => std::ptr::fn_addr_eq(a, b),
            (Self::Cron(a), Self::Cron(b)) => std::ptr::fn_addr_eq(a, b),
            (Self::Shutdown(a), Self::Shutdown(b)) => std::ptr::fn_addr_eq(a, b),
            _ => false,
        }
    }

    fn is_attribute_handler(self) -> bool {
        match self {
            Self::RuleChanged(f) => ROLE_CHANGED_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
            Self::Loading(f) => LOADING_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
            Self::Flush(f) => FLUSH_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
            Self::FlushInfo(f) => FLUSH_INFO_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
            Self::SwapDb(f) => SWAPDB_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
            Self::ModuleChange(f) => MODULE_CHANGED_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
            Self::ConfigChanged(f) => CONFIG_CHANGED_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
            Self::Cron(f) => CRON_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
            Self::Shutdown(f) => SHUTDOWN_SERVER_EVENTS_LIST
                .iter()
                .any(|g| std::ptr::fn_addr_eq(f, *g)),
        }
    }

    fn native_subscription(self) -> (u64, raw::RedisModuleEventCallback) {
        match self {
            Self::RuleChanged(_) => (
                raw::REDISMODULE_EVENT_REPLICATION_ROLE_CHANGED,
                Some(role_changed_callback),
            ),
            Self::Loading(_) => (raw::REDISMODULE_EVENT_LOADING, Some(loading_event_callback)),
            Self::Flush(_) => (raw::REDISMODULE_EVENT_FLUSHDB, Some(flush_event_callback)),
            Self::FlushInfo(_) => (raw::REDISMODULE_EVENT_FLUSHDB, Some(flush_event_callback)),
            Self::SwapDb(_) => (raw::REDISMODULE_EVENT_SWAPDB, Some(swapdb_event_callback)),
            Self::ModuleChange(_) => (
                raw::REDISMODULE_EVENT_MODULE_CHANGE,
                Some(module_change_event_callback),
            ),
            Self::ConfigChanged(_) => (
                raw::REDISMODULE_EVENT_CONFIG,
                Some(config_change_event_callback),
            ),
            Self::Cron(_) => (raw::REDISMODULE_EVENT_CRON_LOOP, Some(cron_callback)),
            Self::Shutdown(_) => (
                raw::REDISMODULE_EVENT_SHUTDOWN,
                Some(shutdown_event_callback),
            ),
        }
    }
}

impl EventContext {
    /// Adds a static handler to the SDK's server-event subscription.
    ///
    /// ```no_run
    /// use redis_module::{EventContext, RedisError, server_events::ServerEventHandler};
    /// fn tick(ctx: &EventContext, hz: u64) { ctx.log_notice(&format!("tick at {hz} Hz")); }
    /// fn configure(ctx: &EventContext, enabled: bool) -> Result<(), RedisError> {
    ///     if enabled { ctx.subscribe_to_server_event(ServerEventHandler::Cron(tick))?; }
    ///     Ok(())
    /// }
    /// ```
    ///
    /// Attribute handlers remain active. Re-registering the same function for
    /// the same typed event is a no-op, including an attribute handler. Identity
    /// uses function addresses; distinct functions with identical code may be
    /// merged by the compiler and treated as one handler. Conversely, the same
    /// function compiled in different codegen units may have different addresses;
    /// pass the same function pointer when repeatable identity is needed.
    ///
    /// Attribute handlers run first, then runtime handlers in registration order.
    /// Each handler has its own panic boundary. Dispatch snapshots registrations
    /// before invoking handlers, so additions made by a handler start on the next
    /// delivery (including a nested delivery). A failed native subscription adds
    /// no handler. An unavailable API or unsupported event returns an error.
    ///
    /// Load, command and event callbacks can subscribe; locked workers cannot.
    /// Handlers last until successful module unload. A rejected unload preserves
    /// them; a failed load clears them. These lifecycle rules are wired by
    /// [`crate::redis_module!`]. There are no captured closures or unsubscribe
    /// tokens. Avoid modifying keys during events that prohibit it, such as flush
    /// callbacks, and follow Redis's restrictions for the chosen event.
    pub fn subscribe_to_server_event(&self, handler: ServerEventHandler) -> Result<(), RedisError> {
        if handler.is_attribute_handler()
            || RUNTIME_HANDLERS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|existing| handler.same_handler(*existing))
        {
            return Ok(());
        }
        let (event, callback) = handler.native_subscription();
        register_single_server_event_type(self, true, event, callback)?;
        RUNTIME_HANDLERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handler);
        Ok(())
    }
}

fn runtime_handlers() -> Vec<ServerEventHandler> {
    RUNTIME_HANDLERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Releases runtime registrations after a failed load or successful unload.
/// Used by the module entrypoint macro while the Redis allocator is available.
#[doc(hidden)]
pub fn __clear_runtime_handlers() {
    *RUNTIME_HANDLERS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
}

macro_rules! dispatch_runtime {
    ($handlers:expr, $variant:ident, $ctx:expr, $label:literal $(, $arg:expr)*) => {
        for handler in $handlers {
            if let ServerEventHandler::$variant(callback) = handler {
                crate::panic::run_callback($label, || callback($ctx $(, $arg)*), || ());
            }
        }
    };
}

#[distributed_slice()]
pub static ROLE_CHANGED_SERVER_EVENTS_LIST: [fn(&EventContext, ServerRole)] = [..];

#[distributed_slice()]
pub static LOADING_SERVER_EVENTS_LIST: [fn(&EventContext, LoadingSubevent)] = [..];

#[distributed_slice()]
pub static FLUSH_SERVER_EVENTS_LIST: [fn(&EventContext, FlushSubevent)] = [..];

#[distributed_slice()]
pub static FLUSH_INFO_SERVER_EVENTS_LIST: [fn(&EventContext, FlushSubevent, FlushInfo)] = [..];

#[distributed_slice()]
pub static SWAPDB_SERVER_EVENTS_LIST: [fn(&EventContext, SwapDbInfo)] = [..];

#[distributed_slice()]
pub static MODULE_CHANGED_SERVER_EVENTS_LIST: [fn(&EventContext, ModuleChangeSubevent)] = [..];

#[distributed_slice()]
pub static CONFIG_CHANGED_SERVER_EVENTS_LIST: [fn(&EventContext, &[&str])] = [..];

#[distributed_slice()]
pub static CRON_SERVER_EVENTS_LIST: [fn(&EventContext, u64)] = [..];

#[distributed_slice()]
pub static SHUTDOWN_SERVER_EVENTS_LIST: [fn(&EventContext)] = [..];

#[distributed_slice()]
pub static INFO_COMMAND_HANDLER_LIST: [fn(&InfoContext, bool) -> RedisResult<()>] = [..];

extern "C" fn cron_callback(
    ctx: *mut raw::RedisModuleCtx,
    _eid: raw::RedisModuleEvent,
    _subevent: u64,
    data: *mut ::std::os::raw::c_void,
) {
    // SAFETY: Redis passes a RedisModuleCronLoopV1 with the cron loop event.
    let data: &raw::RedisModuleCronLoopV1 = unsafe { &*(data as *mut raw::RedisModuleCronLoopV1) };
    let hz = data.hz as u64;
    let ctx = unsafe { EventContext::new(ctx) };
    let handlers = runtime_handlers();
    CRON_SERVER_EVENTS_LIST.iter().for_each(|callback| {
        crate::panic::run_callback("cron event handler", || callback(&ctx, hz), || ());
    });
    dispatch_runtime!(handlers, Cron, &ctx, "cron event handler", hz);
}

extern "C" fn role_changed_callback(
    ctx: *mut raw::RedisModuleCtx,
    _eid: raw::RedisModuleEvent,
    subevent: u64,
    _data: *mut ::std::os::raw::c_void,
) {
    let new_role = if subevent == raw::REDISMODULE_EVENT_REPLROLECHANGED_NOW_MASTER {
        ServerRole::Primary
    } else {
        ServerRole::Replica
    };
    let ctx = unsafe { EventContext::new(ctx) };
    let handlers = runtime_handlers();
    ROLE_CHANGED_SERVER_EVENTS_LIST.iter().for_each(|callback| {
        crate::panic::run_callback(
            "role changed event handler",
            || callback(&ctx, new_role),
            || (),
        );
    });
    dispatch_runtime!(
        handlers,
        RuleChanged,
        &ctx,
        "role changed event handler",
        new_role
    );
}

extern "C" fn loading_event_callback(
    ctx: *mut raw::RedisModuleCtx,
    _eid: raw::RedisModuleEvent,
    subevent: u64,
    _data: *mut ::std::os::raw::c_void,
) {
    let loading_sub_event = match subevent {
        raw::REDISMODULE_SUBEVENT_LOADING_RDB_START => LoadingSubevent::RdbStarted,
        raw::REDISMODULE_SUBEVENT_LOADING_REPL_START => LoadingSubevent::ReplStarted,
        raw::REDISMODULE_SUBEVENT_LOADING_AOF_START => LoadingSubevent::AofStarted,
        raw::REDISMODULE_SUBEVENT_LOADING_ENDED => LoadingSubevent::Ended,
        _ => LoadingSubevent::Failed,
    };
    let ctx = unsafe { EventContext::new(ctx) };
    let handlers = runtime_handlers();
    LOADING_SERVER_EVENTS_LIST.iter().for_each(|callback| {
        crate::panic::run_callback(
            "loading event handler",
            || callback(&ctx, loading_sub_event),
            || (),
        );
    });
    dispatch_runtime!(
        handlers,
        Loading,
        &ctx,
        "loading event handler",
        loading_sub_event
    );
}

extern "C" fn flush_event_callback(
    ctx: *mut raw::RedisModuleCtx,
    _eid: raw::RedisModuleEvent,
    subevent: u64,
    data: *mut ::std::os::raw::c_void,
) {
    let flush_sub_event = match subevent {
        raw::REDISMODULE_SUBEVENT_FLUSHDB_START => FlushSubevent::Started,
        raw::REDISMODULE_SUBEVENT_FLUSHDB_END => FlushSubevent::Ended,
        _ => return,
    };
    // SAFETY: the subscription requests dataver 1, whose flush events provide
    // a RedisModuleFlushInfoV1. Copy before invoking any reentrant user code.
    let data = unsafe { &*data.cast::<raw::RedisModuleFlushInfoV1>() };
    let info = FlushInfo {
        db: (data.dbnum != -1).then_some(data.dbnum),
    };
    let ctx = unsafe { EventContext::new(ctx) };
    let handlers = runtime_handlers();
    FLUSH_SERVER_EVENTS_LIST.iter().for_each(|callback| {
        crate::panic::run_callback(
            "flush event handler",
            || callback(&ctx, flush_sub_event),
            || (),
        );
    });
    FLUSH_INFO_SERVER_EVENTS_LIST.iter().for_each(|callback| {
        crate::panic::run_callback(
            "flush info event handler",
            || callback(&ctx, flush_sub_event, info),
            || (),
        );
    });
    for handler in handlers {
        match handler {
            ServerEventHandler::Flush(callback) => crate::panic::run_callback(
                "flush event handler",
                || callback(&ctx, flush_sub_event),
                || (),
            ),
            ServerEventHandler::FlushInfo(callback) => crate::panic::run_callback(
                "flush info event handler",
                || callback(&ctx, flush_sub_event, info),
                || (),
            ),
            _ => (),
        }
    }
}

extern "C" fn swapdb_event_callback(
    ctx: *mut raw::RedisModuleCtx,
    _eid: raw::RedisModuleEvent,
    _subevent: u64,
    data: *mut ::std::os::raw::c_void,
) {
    // SAFETY: Redis supplies a RedisModuleSwapDbInfoV1 for our dataver 1
    // subscription. No references into the native payload reach handlers.
    let data = unsafe { &*data.cast::<raw::RedisModuleSwapDbInfoV1>() };
    let info = SwapDbInfo {
        first: data.dbnum_first,
        second: data.dbnum_second,
    };
    let ctx = unsafe { EventContext::new(ctx) };
    let handlers = runtime_handlers();
    SWAPDB_SERVER_EVENTS_LIST.iter().for_each(|callback| {
        crate::panic::run_callback("swapdb event handler", || callback(&ctx, info), || ());
    });
    dispatch_runtime!(handlers, SwapDb, &ctx, "swapdb event handler", info);
}

extern "C" fn module_change_event_callback(
    ctx: *mut raw::RedisModuleCtx,
    _eid: raw::RedisModuleEvent,
    subevent: u64,
    _data: *mut ::std::os::raw::c_void,
) {
    let module_changed_sub_event = if subevent == raw::REDISMODULE_SUBEVENT_MODULE_LOADED {
        ModuleChangeSubevent::Loaded
    } else {
        ModuleChangeSubevent::Unloaded
    };
    let ctx = unsafe { EventContext::new(ctx) };
    let handlers = runtime_handlers();
    MODULE_CHANGED_SERVER_EVENTS_LIST
        .iter()
        .for_each(|callback| {
            crate::panic::run_callback(
                "module changed event handler",
                || callback(&ctx, module_changed_sub_event),
                || (),
            );
        });
    dispatch_runtime!(
        handlers,
        ModuleChange,
        &ctx,
        "module changed event handler",
        module_changed_sub_event
    );
}

extern "C" fn shutdown_event_callback(
    ctx: *mut raw::RedisModuleCtx,
    _eid: raw::RedisModuleEvent,
    _subevent: u64,
    _data: *mut ::std::os::raw::c_void,
) {
    let ctx = unsafe { EventContext::new(ctx) };
    let handlers = runtime_handlers();
    SHUTDOWN_SERVER_EVENTS_LIST.iter().for_each(|callback| {
        crate::panic::run_callback("shutdown event handler", || callback(&ctx), || ());
    });
    dispatch_runtime!(handlers, Shutdown, &ctx, "shutdown event handler");
}

extern "C" fn config_change_event_callback(
    ctx: *mut raw::RedisModuleCtx,
    _eid: raw::RedisModuleEvent,
    _subevent: u64,
    data: *mut ::std::os::raw::c_void,
) {
    let data: &raw::RedisModuleConfigChange =
        unsafe { &*(data as *mut raw::RedisModuleConfigChange) };
    let config_names: Vec<_> = (0..data.num_changes)
        .map(|i| unsafe {
            let name = *data.config_names.offset(i as isize);
            CStr::from_ptr(name)
        })
        .collect();
    let config_names: Vec<_> = config_names
        .iter()
        .map(|v| {
            v.to_str()
                .expect("Got a configuration name which is not a valid utf8")
        })
        .collect();
    let ctx = unsafe { EventContext::new(ctx) };
    let handlers = runtime_handlers();
    CONFIG_CHANGED_SERVER_EVENTS_LIST
        .iter()
        .for_each(|callback| {
            crate::panic::run_callback(
                "config changed event handler",
                || callback(&ctx, config_names.as_slice()),
                || (),
            );
        });
    dispatch_runtime!(
        handlers,
        ConfigChanged,
        &ctx,
        "config changed event handler",
        config_names.as_slice()
    );
}

fn register_single_server_event_type(
    ctx: &EventContext,
    has_handlers: bool,
    server_event: u64,
    inner_callback: raw::RedisModuleEventCallback,
) -> Result<(), RedisError> {
    if has_handlers {
        let subscribe = unsafe { raw::RedisModule_SubscribeToServerEvent }.ok_or(
            RedisError::Str("ERR server-event subscription API is unavailable"),
        )?;
        let res = unsafe {
            subscribe(
                ctx.ctx,
                raw::RedisModuleEvent {
                    id: server_event,
                    dataver: 1,
                },
                inner_callback,
            )
        };
        if res != raw::REDISMODULE_OK as i32 {
            return Err(RedisError::Str("ERR server-event subscription failed"));
        }
    }

    Ok(())
}

pub fn register_server_events(ctx: &EventContext) -> Result<(), RedisError> {
    register_single_server_event_type(
        ctx,
        !ROLE_CHANGED_SERVER_EVENTS_LIST.is_empty(),
        raw::REDISMODULE_EVENT_REPLICATION_ROLE_CHANGED,
        Some(role_changed_callback),
    )?;
    register_single_server_event_type(
        ctx,
        !LOADING_SERVER_EVENTS_LIST.is_empty(),
        raw::REDISMODULE_EVENT_LOADING,
        Some(loading_event_callback),
    )?;
    register_single_server_event_type(
        ctx,
        // Redis keeps one subscription per module/event. Fan out through one
        // adapter so registering either list never replaces the other.
        !FLUSH_SERVER_EVENTS_LIST.is_empty() || !FLUSH_INFO_SERVER_EVENTS_LIST.is_empty(),
        raw::REDISMODULE_EVENT_FLUSHDB,
        Some(flush_event_callback),
    )?;
    register_single_server_event_type(
        ctx,
        !SWAPDB_SERVER_EVENTS_LIST.is_empty(),
        raw::REDISMODULE_EVENT_SWAPDB,
        Some(swapdb_event_callback),
    )?;
    register_single_server_event_type(
        ctx,
        !MODULE_CHANGED_SERVER_EVENTS_LIST.is_empty(),
        raw::REDISMODULE_EVENT_MODULE_CHANGE,
        Some(module_change_event_callback),
    )?;
    register_single_server_event_type(
        ctx,
        !CONFIG_CHANGED_SERVER_EVENTS_LIST.is_empty(),
        raw::REDISMODULE_EVENT_CONFIG,
        Some(config_change_event_callback),
    )?;
    register_single_server_event_type(
        ctx,
        !CRON_SERVER_EVENTS_LIST.is_empty(),
        raw::REDISMODULE_EVENT_CRON_LOOP,
        Some(cron_callback),
    )?;
    register_single_server_event_type(
        ctx,
        !SHUTDOWN_SERVER_EVENTS_LIST.is_empty(),
        raw::REDISMODULE_EVENT_SHUTDOWN,
        Some(shutdown_event_callback),
    )?;
    Ok(())
}
