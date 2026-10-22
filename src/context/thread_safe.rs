use std::borrow::Borrow;
use std::cell::{Cell, UnsafeCell};
use std::ops::{Deref, DerefMut};
use std::ptr;

use super::ReplyContext;
use crate::context::blocked::BlockedClient;
use crate::{RedisResult, RuntimeContext, WorkerContext, raw};
use std::marker::PhantomData;

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
/// struct (such as [crate::Context], [`ConfigurationContext`](crate::configuration::ConfigurationContext), [ContextGuard]).
/// User might also decide to implement this trait but he should
/// carefully consider that because it is easy to make mistakes,
/// this is why the trait is marked as unsafe.
/// A reference must prove that the lock remains held on the current thread
/// for the entire borrow. Implementations must prevent transferring that proof
/// to a thread that does not hold the lock.
pub unsafe trait RedisLockIndicator {}

/// Data accessible only while the Redis lock is held.
///
/// Sharing or moving this guard between threads requires `T: Send`. The lock
/// serializes accesses through this guard, but cannot protect aliases held
/// outside it. `T: Sync` is not required, so owned `Cell` values are supported:
///
/// ```
/// use redis_module::RedisGILGuard;
/// use std::cell::Cell;
/// fn shareable<T: Send + Sync>(_: T) {}
/// shareable(RedisGILGuard::new(Cell::new(0)));
/// ```
///
/// An `Rc` can have aliases outside the guard and cannot be shared this way:
///
/// ```compile_fail,E0277
/// use redis_module::RedisGILGuard;
/// use std::{cell::Cell, rc::Rc};
/// fn shareable<T: Sync>(_: &T) {}
/// let outside = Rc::new(Cell::new(0));
/// let guarded = RedisGILGuard::new(Rc::clone(&outside));
/// shareable(&guarded);
/// ```
///
/// Native string owners also cannot be shared in a guard. Use `String` for
/// UTF-8 configuration storage or `Vec<u8>` for arbitrary bytes:
///
/// ```compile_fail,E0277
/// use redis_module::{RedisGILGuard, RedisString};
/// fn shareable<T: Sync>() {}
/// shareable::<RedisGILGuard<RedisString>>();
/// ```
///
/// Non-`Send` values remain usable in a guard confined to one thread. See
/// `examples/threads.rs` for shared module state.
pub struct RedisGILGuard<T> {
    obj: UnsafeCell<T>,
    /// Whether a scope from [`Self::lock`] is live. Only read and written by
    /// the thread holding the Redis lock.
    locked: Cell<bool>,
}

impl<T> RedisGILGuard<T> {
    pub const fn new(obj: T) -> RedisGILGuard<T> {
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
// Redis lock, as a RedisLockIndicator proves. Access may move between threads,
// so T must be Send even though accesses are serialized. Otherwise, e.g. Rc
// aliases outside this guard could race with a locked worker's accesses.
unsafe impl<T: Send> Sync for RedisGILGuard<T> {}
// Moving the guard moves the value to another thread, so T must be Send.
unsafe impl<T: Send> Send for RedisGILGuard<T> {}

/// A worker view borrowed from its [`ThreadSafeContext`] while Redis is locked.
/// The owner cannot be freed before the guard, and the view cannot escape it.
///
/// ```compile_fail,E0515
/// fn escape() -> redis_module::ContextGuard<'static> {
///     let owner = redis_module::ThreadSafeContext::new();
///     owner.lock()
/// }
/// ```
///
/// ```compile_fail,E0505
/// fn free_owner(owner: redis_module::ThreadSafeContext<redis_module::DetachedFromClient>) {
///     let guard = owner.lock();
///     drop(owner);
///     guard.log_notice("owner is gone");
/// }
/// ```
///
/// ```compile_fail,E0599
/// fn reply(guard: &redis_module::ContextGuard<'_>) { guard.reply(Ok("OK".into())); }
/// ```
pub struct ContextGuard<'context> {
    ctx: WorkerContext,
    unlock_ctx: *mut raw::RedisModuleCtx,
    _context: PhantomData<&'context ()>,
}

unsafe impl RedisLockIndicator for ContextGuard<'_> {}

impl Drop for ContextGuard<'_> {
    fn drop(&mut self) {
        // Clear even forgotten call-user scopes before freeing the view or unlocking.
        self.ctx.release_call_users();
        unsafe {
            crate::__required_api!(RedisModule_FreeThreadSafeContext)(self.ctx.ctx);
            crate::__required_api!(RedisModule_ThreadSafeContextUnlock)(self.unlock_ctx);
        };
    }
}

impl Deref for ContextGuard<'_> {
    type Target = WorkerContext;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

impl Borrow<RuntimeContext> for ContextGuard<'_> {
    fn borrow(&self) -> &RuntimeContext {
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
            crate::__required_api!(RedisModule_GetThreadSafeContext)(blocked_client.inner.as_ptr())
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
        // ReplyContext grants no RedisLockIndicator or general runtime access.
        ReplyContext { ctx: self.ctx }.reply(r)
    }
}

impl<B: Send> ThreadSafeContext<B> {
    /// All other APIs require locking the context, so we wrap it in a way
    /// similar to `std::sync::Mutex`.
    pub fn lock(&self) -> ContextGuard<'_> {
        unsafe { crate::__required_api!(RedisModule_ThreadSafeContextLock)(self.ctx) };
        let ctx =
            unsafe { crate::__required_api!(RedisModule_GetThreadSafeContext)(ptr::null_mut()) };
        let ctx = unsafe { WorkerContext::new(ctx) };
        ContextGuard {
            ctx,
            unlock_ctx: self.ctx,
            _context: PhantomData,
        }
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
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _second = guard.lock(&Locked);
                }))
                .is_err()
            );
        }
        assert_eq!(*guard.lock(&Locked), 2);
    }
}
