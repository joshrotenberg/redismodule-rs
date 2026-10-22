use std::any::Any;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::{EventContext, RedisError, RedisGILGuard, raw};

/// A typed capability for a timer created by this SDK module.
///
/// Clones refer to the same timer. Dropping a handle does not cancel it and
/// does not call Redis. Use [`EventContext::cancel_timer`] explicitly.
/// The payload type is invariant and cannot be changed or recovered from a raw ID.
///
/// ```compile_fail
/// use redis_module::{EventContext, TimerHandle};
/// fn wrong(ctx: &EventContext, handle: &TimerHandle<String>) {
///     let _: Option<u64> = ctx.cancel_timer(handle).unwrap();
/// }
/// ```
pub struct TimerHandle<T> {
    owner: Arc<()>,
    key: u64,
    payload: PhantomData<fn(T) -> T>,
}

impl<T> Clone for TimerHandle<T> {
    fn clone(&self) -> Self {
        Self {
            owner: self.owner.clone(),
            key: self.key,
            payload: PhantomData,
        }
    }
}

impl<T> fmt::Debug for TimerHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimerHandle")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

trait ErasedTimer: Send {
    fn fire(self: Box<Self>, ctx: &EventContext);
    fn cancel(self: Box<Self>) -> Box<dyn Any + Send>;
}

struct Payload<F, T> {
    callback: F,
    data: T,
}
impl<F, T> ErasedTimer for Payload<F, T>
where
    F: FnOnce(&EventContext, T) + Send + 'static,
    T: Send + 'static,
{
    fn fire(self: Box<Self>, ctx: &EventContext) {
        let Self { callback, data } = *self;
        callback(ctx, data);
    }
    fn cancel(self: Box<Self>) -> Box<dyn Any + Send> {
        let Self { callback, data } = *self;
        drop(callback);
        Box::new(data)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Unloaded,
    Loading,
    Active,
    Unloading,
}
#[derive(Clone, Copy, PartialEq)]
enum State {
    Pending,
    Firing,
    Cancelling,
}
struct Cookie {
    key: u64,
}
struct Entry {
    cookie: Box<Cookie>,
    native_id: raw::RedisModuleTimerID,
    state: State,
    payload: Option<Box<dyn ErasedTimer>>,
}
struct Registry {
    phase: Phase,
    owner: Option<Arc<()>>,
    next: u64,
    entries: BTreeMap<u64, Entry>,
}
static TIMERS: RedisGILGuard<Registry> = RedisGILGuard::new(Registry {
    phase: Phase::Unloaded,
    owner: None,
    next: 0,
    entries: BTreeMap::new(),
});

/// Internal binding for `redis_module!`; not a custom-loader lifecycle API.
#[doc(hidden)]
pub struct TimerLoadGuard<'a> {
    ctx: &'a EventContext,
    committed: bool,
}
impl<'a> TimerLoadGuard<'a> {
    /// # Safety
    /// Call only once inside this module's native OnLoad, before user code.
    pub unsafe fn begin(ctx: &'a EventContext) -> Result<Self, RedisError> {
        let mut registry = TIMERS.lock(ctx);
        if registry.phase != Phase::Unloaded || !registry.entries.is_empty() {
            return Err(RedisError::Str("ERR timer lifecycle is already bound"));
        }
        registry.phase = Phase::Loading;
        registry.owner = Some(Arc::new(()));
        Ok(Self {
            ctx,
            committed: false,
        })
    }
    pub fn commit(mut self) {
        TIMERS.lock(self.ctx).phase = Phase::Active;
        self.committed = true;
    }
}
impl Drop for TimerLoadGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let mut registry = TIMERS.lock(self.ctx);
            registry.phase = Phase::Unloaded;
            registry.owner = None;
        }
    }
}

/// Internal binding for `redis_module!`; not a custom-loader lifecycle API.
#[doc(hidden)]
pub struct TimerUnloadGuard<'a> {
    ctx: &'a EventContext,
    committed: bool,
}
impl<'a> TimerUnloadGuard<'a> {
    /// # Safety
    /// Call only inside this module's native OnUnload, before user code.
    pub unsafe fn begin(ctx: &'a EventContext) -> Result<Self, RedisError> {
        let mut registry = TIMERS.lock(ctx);
        if registry.phase != Phase::Active || !registry.entries.is_empty() {
            return Err(RedisError::Str(
                "ERR module has pending or executing SDK timers",
            ));
        }
        registry.phase = Phase::Unloading;
        Ok(Self {
            ctx,
            committed: false,
        })
    }
    pub fn commit(mut self) {
        let mut registry = TIMERS.lock(self.ctx);
        registry.phase = Phase::Unloaded;
        registry.owner = None;
        self.committed = true;
    }
}
impl Drop for TimerUnloadGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            TIMERS.lock(self.ctx).phase = Phase::Active;
        }
    }
}

// Retain the entry throughout user callbacks/destructors, including unwinding.
// Their reentrant unload attempts must still observe an executing timer.
struct Completion<'a> {
    ctx: &'a EventContext,
    key: u64,
}
impl Drop for Completion<'_> {
    fn drop(&mut self) {
        let entry = TIMERS.lock(self.ctx).entries.remove(&self.key);
        // No user payload remains in this entry. Never run user Drop under a borrow.
        drop(entry);
    }
}

fn period_millis(period: Duration) -> Result<i64, RedisError> {
    let millis = period.as_nanos().div_ceil(1_000_000);
    // Native Redis converts milliseconds to microseconds and adds wall time.
    // Leave half the signed range for the current timestamp and rounding.
    if millis > (i64::MAX / 2000) as u128 {
        return Err(RedisError::Str("ERR timer period is too large"));
    }
    Ok(millis as i64)
}

impl EventContext {
    /// Creates a one-shot timer and returns a typed cancellation handle.
    ///
    /// The callback and payload are owned until firing or explicit cancellation.
    /// They must be `Send + 'static`, since creation can occur in a notification
    /// on a worker while Redis invokes the timer on its main thread. `Sync` is
    /// not required. Zero runs on a later event-loop turn; positive fractional
    /// milliseconds round up. Excessive periods return an error.
    ///
    /// Creation requires a successfully loaded `redis_module!` module. It returns
    /// an error during initialization or unloading, or if a required API is absent.
    /// Redis can reject a load after OnLoad returns, so init timers are unsupported.
    ///
    /// ```compile_fail
    /// use redis_module::EventContext;
    /// use std::time::Duration;
    /// fn borrowed(ctx: &EventContext) {
    ///     let local = String::from("temporary");
    ///     ctx.create_timer(Duration::ZERO, |_, ()| println!("{local}"), ()).unwrap();
    /// }
    /// ```
    /// ```compile_fail
    /// use redis_module::EventContext;
    /// use std::{rc::Rc, time::Duration};
    /// fn non_send(ctx: &EventContext) {
    ///     ctx.create_timer(Duration::ZERO, |_, _| (), Rc::new(1)).unwrap();
    /// }
    /// ```
    pub fn create_timer<F, T>(
        &self,
        period: Duration,
        callback: F,
        data: T,
    ) -> Result<TimerHandle<T>, RedisError>
    where
        F: FnOnce(&Self, T) + Send + 'static,
        T: Send + 'static,
    {
        let period = period_millis(period)?;
        // SAFETY: the live context proves Redis initialized its immutable API table.
        let create = unsafe {
            let create = crate::__available_api!(RedisModule_CreateTimer);
            let _ = crate::__available_api!(RedisModule_StopTimer);
            let _ = crate::__available_api!(RedisModule_GetTimerInfo);
            create
        };
        let (owner, key) = {
            let mut registry = TIMERS.lock(self);
            if registry.phase != Phase::Active {
                return Err(RedisError::Str("ERR timers require an active SDK module"));
            }
            let key = registry
                .next
                .checked_add(1)
                .ok_or(RedisError::Str("ERR timer identity exhausted"))?;
            registry.next = key;
            (
                registry.owner.as_ref().expect("active timer owner").clone(),
                key,
            )
        };
        let mut cookie = Box::new(Cookie { key });
        let pointer = (&mut *cookie as *mut Cookie).cast::<c_void>();
        let entry = Entry {
            cookie,
            native_id: 0,
            state: State::Pending,
            payload: Some(Box::new(Payload { callback, data })),
        };
        TIMERS.lock(self).entries.insert(key, entry);
        // SAFETY: Redis retains only a pointer to the registry-owned stable cookie.
        // Native CreateTimer does not invoke the callback synchronously or fail.
        let native_id = unsafe { create(self.ctx, period, Some(raw_callback), pointer) };
        TIMERS
            .lock(self)
            .entries
            .get_mut(&key)
            .expect("new timer entry")
            .native_id = native_id;
        Ok(TimerHandle {
            owner,
            key,
            payload: PhantomData,
        })
    }

    /// Cancels a pending timer, returning its owned payload exactly once.
    ///
    /// `None` means fired, firing, already cancelled, or from an earlier load.
    /// API/native errors preserve the pending entry and handle for retry.
    /// Captured callback state is dropped with its original type and layout.
    pub fn cancel_timer<T: Send + 'static>(
        &self,
        timer: &TimerHandle<T>,
    ) -> Result<Option<T>, RedisError> {
        let Some((id, pointer)) = self.pending_timer(timer) else {
            return Ok(None);
        };
        // SAFETY: the live context proves the initialized API table is available.
        let stop = unsafe { crate::__available_api!(RedisModule_StopTimer) };
        let mut returned = std::ptr::null_mut();
        // SAFETY: identity is checked against this module's live registry entry.
        let status = unsafe { stop(self.ctx, id, &mut returned) };
        if status != raw::Status::Ok as i32 || pointer != returned {
            return Err(RedisError::Str(
                "ERR native timer cancellation failed ownership check",
            ));
        }
        let payload = {
            let mut registry = TIMERS.lock(self);
            let entry = registry.entries.get_mut(&timer.key).expect("pending timer");
            entry.state = State::Cancelling;
            entry.payload.take().expect("pending payload")
        };
        let _completion = Completion {
            ctx: self,
            key: timer.key,
        };
        let data = payload
            .cancel()
            .downcast::<T>()
            .map_err(|_| RedisError::Str("ERR timer payload type mismatch"))?;
        Ok(Some(*data))
    }

    /// Returns remaining duration for a pending timer, without exposing payloads.
    /// Returns `None` after cancellation or once callback execution starts.
    pub fn timer_remaining<T>(
        &self,
        timer: &TimerHandle<T>,
    ) -> Result<Option<Duration>, RedisError> {
        let Some((id, pointer)) = self.pending_timer(timer) else {
            return Ok(None);
        };
        // SAFETY: the live context proves the initialized API table is available.
        let info = unsafe { crate::__available_api!(RedisModule_GetTimerInfo) };
        let mut remaining = 0;
        let mut returned = std::ptr::null_mut();
        // SAFETY: the native ID is still owned by this module and is pending.
        let status = unsafe { info(self.ctx, id, &mut remaining, &mut returned) };
        if status != raw::Status::Ok as i32 || pointer != returned {
            return Err(RedisError::Str(
                "ERR native timer metadata failed ownership check",
            ));
        }
        Ok(Some(Duration::from_millis(remaining)))
    }

    fn pending_timer<T>(
        &self,
        timer: &TimerHandle<T>,
    ) -> Option<(raw::RedisModuleTimerID, *mut c_void)> {
        TIMERS.lock(self).pending(timer)
    }
}

impl Registry {
    fn pending<T>(&self, timer: &TimerHandle<T>) -> Option<(raw::RedisModuleTimerID, *mut c_void)> {
        if !self
            .owner
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, &timer.owner))
        {
            return None;
        }
        let entry = self.entries.get(&timer.key)?;
        if entry.state != State::Pending {
            return None;
        }
        Some((
            entry.native_id,
            (&*entry.cookie as *const Cookie).cast_mut().cast(),
        ))
    }
}

extern "C" fn raw_callback(ctx: *mut raw::RedisModuleCtx, data: *mut c_void) {
    crate::panic::run_callback(
        "timer callback",
        || {
            // SAFETY: Redis invokes this callback while holding its lock, with the
            // live module context and cookie retained by the pending registry entry.
            let ctx = unsafe { EventContext::new(ctx) };
            if data.is_null() {
                return;
            }
            let key = unsafe { (*data.cast::<Cookie>()).key };
            let payload = {
                let mut registry = TIMERS.lock(&ctx);
                let entry = registry.entries.get_mut(&key).expect("native timer entry");
                assert!(entry.state == State::Pending);
                entry.state = State::Firing;
                entry.payload.take().expect("pending timer payload")
            };
            let _completion = Completion { ctx: &ctx, key };
            payload.fire(&ctx);
        },
        || (),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delay_rounding_and_limits() {
        assert_eq!(period_millis(Duration::ZERO).unwrap(), 0);
        assert_eq!(period_millis(Duration::from_nanos(1)).unwrap(), 1);
        assert_eq!(period_millis(Duration::from_micros(1001)).unwrap(), 2);
        assert_eq!(period_millis(Duration::from_millis(42)).unwrap(), 42);
        assert!(period_millis(Duration::MAX).is_err());
    }
    #[test]
    fn stale_handles_cannot_alias_a_reused_native_id() {
        let old_owner = Arc::new(());
        let owner = Arc::new(());
        let handle = TimerHandle::<u64> {
            owner: old_owner,
            key: 1,
            payload: PhantomData,
        };
        let current = TimerHandle::<u64> {
            owner: owner.clone(),
            key: 1,
            payload: PhantomData,
        };
        let mut registry = Registry {
            phase: Phase::Active,
            owner: Some(owner),
            next: 2,
            entries: BTreeMap::new(),
        };
        registry.entries.insert(
            1,
            Entry {
                cookie: Box::new(Cookie { key: 1 }),
                native_id: 42,
                state: State::Pending,
                payload: None,
            },
        );
        assert!(registry.pending(&handle).is_none());
        assert_eq!(registry.pending(&current).unwrap().0, 42);
        registry.entries.get_mut(&1).unwrap().state = State::Firing;
        assert!(registry.pending(&current).is_none());
        registry.entries.get_mut(&1).unwrap().state = State::Cancelling;
        assert!(registry.pending(&current).is_none());
        registry.entries.remove(&1);
        registry.entries.insert(
            2,
            Entry {
                cookie: Box::new(Cookie { key: 2 }),
                native_id: 42,
                state: State::Pending,
                payload: None,
            },
        );
        assert!(registry.pending(&current).is_none());
    }
    #[test]
    fn handle_traits_do_not_require_payload_traits() {
        struct Neither;
        let handle = TimerHandle::<Neither> {
            owner: Arc::new(()),
            key: 1,
            payload: PhantomData,
        };
        assert_eq!(handle.clone().key, 1);
        assert!(format!("{handle:?}").contains("TimerHandle"));
        fn send_sync<T: Send + Sync>() {}
        send_sync::<TimerHandle<std::cell::Cell<u8>>>();
    }
}
