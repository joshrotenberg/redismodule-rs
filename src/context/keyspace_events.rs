//! Keyspace notification subscriptions with masks chosen at runtime.
use crate::{EventContext, NotifyEvent, RedisError, raw};
use std::os::raw::{c_char, c_int};

type NotificationCallback = extern "C" fn(
    *mut raw::RedisModuleCtx,
    c_int,
    *const c_char,
    *mut raw::RedisModuleString,
) -> c_int;

/// A panic-safe keyspace callback, constructed by [`crate::redis_event_handler!`].
///
/// This holds a static callback, not a closure or captured state. Redis owns the
/// subscription until the module unloads; no Rust allocation is retained.
#[derive(Clone, Copy)]
pub struct KeyspaceEventHandler {
    callback: NotificationCallback,
}

impl KeyspaceEventHandler {
    /// Constructs an adapter for the public macro.
    ///
    /// # Safety
    ///
    /// The callback must contain panics and must only borrow Redis's event and
    /// key pointers for the duration of the invocation.
    #[doc(hidden)]
    pub const unsafe fn __new(callback: NotificationCallback) -> Self {
        Self { callback }
    }
}

impl EventContext {
    /// Subscribes to a keyspace event mask chosen at runtime.
    ///
    /// ```no_run
    /// use redis_module::{EventContext, NotifyEvent, RedisError, redis_event_handler};
    /// fn on_event(_: &EventContext, _: NotifyEvent, _: &str, _: &[u8]) {}
    /// fn subscribe(ctx: &EventContext, include_misses: bool) -> Result<(), RedisError> {
    ///     let mut mask = NotifyEvent::STRING;
    ///     if include_misses { mask |= NotifyEvent::MISSED; }
    ///     ctx.subscribe_to_keyspace_events(mask, redis_event_handler!(on_event))
    /// }
    /// ```
    ///
    /// Module load and command contexts dereference to this event view. Worker
    /// guards cannot subscribe. For custom native callbacks, see [`Self::new`].
    /// Each call adds a subscription; repeated calls do not replace an earlier
    /// mask. Redis removes subscriptions when the module unloads. Notifications
    /// do not depend on the server's `notify-keyspace-events` configuration.
    ///
    /// When Redis can report its supported flags, unsupported flags are logged
    /// and filtered, just as with `redis_module!` event handlers. Older Redis
    /// 6.0 servers receive the requested mask unchanged: "all" means all flags
    /// known to this SDK, and that server only emits the events it implements.
    /// An empty resulting mask succeeds without subscribing.
    /// Event names are decoded lossily; keys are borrowed only during the
    /// callback. A handler must copy any data it needs to keep. Panics are caught
    /// and logged. Use [`Self::add_post_notification_job`] for writes in response
    /// to an event, because notifications run inside Redis's keyspace operations.
    pub fn subscribe_to_keyspace_events(
        &self,
        mask: NotifyEvent,
        handler: KeyspaceEventHandler,
    ) -> Result<(), RedisError> {
        keyspace_subscription_available()?;
        let supported = unsafe { raw::RedisModule_GetKeyspaceNotificationFlagsAll }
            .map_or(mask, |flags_all| {
                NotifyEvent::from_bits_truncate(unsafe { flags_all() })
            });
        let available = mask.intersection(supported);
        if !supported.contains(mask) {
            let unsupported = mask.difference(supported);
            self.log_notice(&format!(
                "These event notification flags set aren't supported: {unsupported:?}. These flags will be used: {available:?}"
            ));
        }
        if available.is_empty() {
            return Ok(());
        }
        let subscribe = unsafe { raw::RedisModule_SubscribeToKeyspaceEvents }.ok_or(
            RedisError::Str("ERR keyspace subscription API is unavailable"),
        )?;
        let status = unsafe { subscribe(self.ctx, available.bits(), Some(handler.callback)) };
        if status == raw::Status::Ok as c_int {
            Ok(())
        } else {
            Err(RedisError::Str("ERR keyspace subscription failed"))
        }
    }
}

/// Checks for the APIs [`EventContext::subscribe_to_keyspace_events`] needs.
/// `redis_module!` subscribes its `event_handlers` after `init`, the last step
/// of the load, because Redis before 7.2.4 keeps a failed load's keyspace
/// subscriptions and has no API to remove them; this check lets it fail the
/// load before `init` instead.
#[doc(hidden)]
pub fn keyspace_subscription_available() -> Result<(), RedisError> {
    unsafe { raw::RedisModule_SubscribeToKeyspaceEvents }.ok_or(RedisError::Str(
        "ERR keyspace subscription API is unavailable",
    ))?;
    Ok(())
}
