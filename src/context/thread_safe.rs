use std::borrow::Borrow;
use std::cell::{Cell, UnsafeCell};
use std::ops::{Deref, DerefMut};
use std::ptr;

use crate::context::blocked::BlockedClient;
use crate::{raw, Context, RedisResult};

pub struct RedisGILGuardScope<'ctx, 'mutex, T, G: RedisLockIndicator> {
    _context: &'ctx G,
    mutex: &'mutex RedisGILGuard<T>,
}

impl<'ctx, 'mutex, T, G: RedisLockIndicator> Deref for RedisGILGuardScope<'ctx, 'mutex, T, G> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.obj.get() }
    }
}

impl<'ctx, 'mutex, T, G: RedisLockIndicator> DerefMut for RedisGILGuardScope<'ctx, 'mutex, T, G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.obj.get() }
    }
}

impl<'ctx, 'mutex, T, G: RedisLockIndicator> Drop for RedisGILGuardScope<'ctx, 'mutex, T, G> {
    fn drop(&mut self) {
        self.mutex.locked.set(false);
    }
}

/// Whenever the user gets a reference to a struct that
/// implements this trait, it can assume that the Redis GIL
/// is held. Any struct that implements this trait can be
/// used to retrieve objects which are GIL protected (see
/// [RedisGILGuard] for more information)
///
/// Notice that this trait only gives indication that the
/// GIL is locked, unlike [RedisGILGuard] which protect data
/// access and make sure the protected data is only accesses
/// when the GIL is locked.
///
/// # Safety
///
/// In general this trait should not be implemented by the
/// user, the crate knows when the Redis GIL is held and will
/// make sure to implement this trait correctly on different
/// struct (such as [Context], [ConfigurationContext], [ContextGuard]).
/// User might also decide to implement this trait but he should
/// carefully consider that because it is easy to make mistakes,
/// this is why the trait is marked as unsafe.
pub unsafe trait RedisLockIndicator {}

/// This struct allows to guard some data and makes sure
/// the data is only access when the Redis GIL is locked.
/// From example, assuming you module want to save some
/// statistics inside some global variable, but without the
/// need to protect this variable with some mutex (because
/// we know this variable is protected by Redis lock).
/// For example, look at examples/threads.rs
pub struct RedisGILGuard<T> {
    obj: UnsafeCell<T>,
    /// Whether a scope from [`Self::lock`] is live. Only read and written by
    /// the thread holding the Redis lock.
    locked: Cell<bool>,
}

impl<T> RedisGILGuard<T> {
    pub fn new(obj: T) -> RedisGILGuard<T> {
        RedisGILGuard {
            obj: UnsafeCell::new(obj),
            locked: Cell::new(false),
        }
    }

    /// Gives access to the value while the Redis lock is held, as `context`
    /// proves.
    ///
    /// # Panics
    ///
    /// Panics if a scope from an earlier `lock` on this guard is still live,
    /// since two scopes would give two `&mut T` to the same value.
    pub fn lock<'mutex, 'ctx, G: RedisLockIndicator>(
        &'mutex self,
        context: &'ctx G,
    ) -> RedisGILGuardScope<'ctx, 'mutex, T, G> {
        assert!(
            !self.locked.replace(true),
            "RedisGILGuard is already locked; drop the earlier scope first"
        );
        RedisGILGuardScope {
            _context: context,
            mutex: self,
        }
    }
}

impl<T: Default> Default for RedisGILGuard<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// SAFETY: the value and the lock flag are only used by the thread holding the
// Redis lock, as a RedisLockIndicator proves. T is not required to be Send:
// RedisGILGuard<RedisString> holds string configurations, and RedisString is
// not Send. A value cloned out of a scope follows its own thread rules.
unsafe impl<T> Sync for RedisGILGuard<T> {}
// Moving the guard moves the value to another thread, so T must be Send.
unsafe impl<T: Send> Send for RedisGILGuard<T> {}

pub struct ContextGuard {
    ctx: Context,
}

unsafe impl RedisLockIndicator for ContextGuard {}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        unsafe {
            crate::__required_api!(RedisModule_ThreadSafeContextUnlock)(self.ctx.ctx);
            crate::__required_api!(RedisModule_FreeThreadSafeContext)(self.ctx.ctx);
        };
    }
}

impl Deref for ContextGuard {
    type Target = Context;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

impl Borrow<Context> for ContextGuard {
    fn borrow(&self) -> &Context {
        &self.ctx
    }
}

/// A ``ThreadSafeContext`` can either be bound to a blocked client, or detached from any client.
pub struct DetachedFromClient;

pub struct ThreadSafeContext<B: Send> {
    pub(crate) ctx: *mut raw::RedisModuleCtx,

    /// This field is only used implicitly by `Drop`, so avoid a compiler warning
    #[allow(dead_code)]
    blocked_client: B,
}

unsafe impl<B: Send> Send for ThreadSafeContext<B> {}
unsafe impl<B: Send> Sync for ThreadSafeContext<B> {}

impl ThreadSafeContext<DetachedFromClient> {
    #[must_use]
    pub fn new() -> Self {
        let ctx =
            unsafe { crate::__required_api!(RedisModule_GetThreadSafeContext)(ptr::null_mut()) };
        Self {
            ctx,
            blocked_client: DetachedFromClient,
        }
    }
}

impl Default for ThreadSafeContext<DetachedFromClient> {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadSafeContext<BlockedClient> {
    #[must_use]
    pub fn with_blocked_client(blocked_client: BlockedClient) -> Self {
        let ctx = unsafe {
            crate::__required_api!(RedisModule_GetThreadSafeContext)(blocked_client.inner)
        };
        Self {
            ctx,
            blocked_client,
        }
    }

    /// The Redis modules API does not require locking for `Reply` functions,
    /// so we pass through its functionality directly.
    #[allow(clippy::must_use_candidate)]
    pub fn reply(&self, r: RedisResult) -> raw::Status {
        // SAFETY: replies to a blocked client's thread-safe context are
        // collected without the lock and sent when Redis unblocks the client;
        // the Context is used only for this reply and is not kept.
        let ctx = unsafe { Context::new(self.ctx) };
        ctx.reply(r)
    }
}

impl<B: Send> ThreadSafeContext<B> {
    /// All other APIs require locking the context, so we wrap it in a way
    /// similar to `std::sync::Mutex`.
    pub fn lock(&self) -> ContextGuard {
        unsafe { crate::__required_api!(RedisModule_ThreadSafeContextLock)(self.ctx) };
        let ctx =
            unsafe { crate::__required_api!(RedisModule_GetThreadSafeContext)(ptr::null_mut()) };
        let ctx = unsafe { Context::new(ctx) };
        ContextGuard { ctx }
    }
}

impl<B: Send> Drop for ThreadSafeContext<B> {
    fn drop(&mut self) {
        unsafe { crate::__required_api!(RedisModule_FreeThreadSafeContext)(self.ctx) };
    }
}

#[cfg(test)]
mod tests {
    use super::{RedisGILGuard, RedisLockIndicator};

    struct Locked;
    // SAFETY: test only; no Redis data is touched.
    unsafe impl RedisLockIndicator for Locked {}

    #[test]
    fn a_guard_gives_one_scope_at_a_time() {
        let guard = RedisGILGuard::new(1);
        {
            let mut scope = guard.lock(&Locked);
            *scope += 1;
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _second = guard.lock(&Locked);
            }))
            .is_err());
        }
        assert_eq!(*guard.lock(&Locked), 2);
    }
}
