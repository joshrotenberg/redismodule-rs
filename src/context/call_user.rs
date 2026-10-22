//! Call-user scopes are shared by every Rust view of the same native context.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ptr;
use std::rc::{Rc, Weak};

use super::RuntimeContext;
use crate::{RedisError, RedisString, raw};

type SetUser = unsafe extern "C" fn(*mut raw::RedisModuleCtx, *const raw::RedisModuleUser);
type FreeUser = unsafe extern "C" fn(*mut raw::RedisModuleUser);

thread_local! {
    // Views of one active native context are on the same thread. They cannot
    // outlive their invocation/worker lock, so no Rc state crosses threads.
    // Weak entries own no native users; even TLS destruction cannot free one.
    // Never retain a registry borrow across native calls or user callbacks.
    static CALL_USERS: RefCell<BTreeMap<usize, Weak<CallUserState>>> =
        const { RefCell::new(BTreeMap::new()) };
}

#[derive(Debug)]
pub(super) struct CallUserView {
    state: Rc<CallUserState>,
    owner: Rc<()>,
}

#[derive(Debug)]
struct NativeUser {
    user: *mut raw::RedisModuleUser,
    free: FreeUser,
}

impl Drop for NativeUser {
    fn drop(&mut self) {
        unsafe { (self.free)(self.user) };
    }
}

#[derive(Debug)]
struct Frame {
    user: NativeUser,
    owner: Rc<()>,
}

#[derive(Debug)]
struct CallUserState {
    ctx: *mut raw::RedisModuleCtx,
    set_user: SetUser,
    baseline: Cell<*const raw::RedisModuleUser>,
    frames: RefCell<Vec<Option<Frame>>>,
}

impl CallUserState {
    fn push(&self, owner: &Rc<()>, user: NativeUser) -> usize {
        let mut frames = self.frames.borrow_mut();
        if frames.is_empty() {
            // This optional API appeared in Redis 8.8. Earlier servers cannot
            // expose a native override, but SDK scopes still nest correctly.
            self.baseline.set(unsafe {
                raw::RedisModule_GetContextUser.map_or(ptr::null(), |get| get(self.ctx))
            });
        }
        unsafe { (self.set_user)(self.ctx, user.user) };
        let index = frames.len();
        frames.push(Some(Frame {
            user,
            owner: Rc::clone(owner),
        }));
        index
    }

    fn remove(&self, index: usize) {
        let mut frames = self.frames.borrow_mut();
        let removed = frames[index].take();
        self.restore(&mut frames);
        // Restore first: the native context must never refer to a freed handle.
        drop(removed);
    }

    fn remove_owner(&self, owner: &Rc<()>) {
        let mut frames = self.frames.borrow_mut();
        let removed: Vec<_> = frames
            .iter_mut()
            .filter(|frame| frame.as_ref().is_some_and(|f| Rc::ptr_eq(&f.owner, owner)))
            .filter_map(Option::take)
            .collect();
        if !removed.is_empty() {
            self.restore(&mut frames);
        }
        drop(removed);
    }

    fn restore(&self, frames: &mut Vec<Option<Frame>>) {
        while frames.last().is_some_and(Option::is_none) {
            frames.pop();
        }
        let user = frames.last().map_or(self.baseline.get(), |frame| {
            frame.as_ref().unwrap().user.user
        });
        unsafe { (self.set_user)(self.ctx, user) };
    }
}

/// A scoped user for ACL-checked calls through [`RuntimeContext::scoped_call_user`].
/// Dropping it restores the latest still-active scope on this native context,
/// including during unwind and when guards are dropped out of order. After the
/// last scope ends, the native override is restored on Redis 8.8+ and cleared
/// on earlier Redis versions.
/// It borrows the runtime view, including the worker guard that provides it:
///
/// ```compile_fail,E0597
/// fn escape(owner: &redis_module::ThreadSafeContext<redis_module::DetachedFromClient>) {
///     let scope;
///     {
///         let guard = owner.lock();
///         let username = guard.create_string("default");
///         scope = guard.scoped_call_user(&username).unwrap();
///     }
///     drop(scope);
/// }
/// ```
#[derive(Debug)]
pub struct CallUserScope<'ctx> {
    ctx: &'ctx RuntimeContext,
    index: usize,
}

impl Drop for CallUserScope<'_> {
    fn drop(&mut self) {
        self.ctx.call_users.get().unwrap().state.remove(self.index);
    }
}

impl RuntimeContext {
    /// Selects the user used by calls with [`CallOptionsBuilder::verify_acl`](crate::CallOptionsBuilder::verify_acl) enabled.
    /// Keep the returned [`CallUserScope`] alive while making those calls.
    /// This changes the context's call user, including on events and workers;
    /// it does not authenticate or modify a client connection.
    ///
    /// Dropping a guard restores the latest still-active SDK scope on the same
    /// native context. After all scopes end, Redis 8.8+ restores any preexisting
    /// native override; earlier Redis versions clear it because they cannot
    /// query it. Avoid mixing these scopes with raw `SetContextUser` calls.
    ///
    /// Forgetting a guard keeps its scope active until this view is dropped.
    /// Worker guards clean up before unlocking, including on reused detached
    /// contexts. Do not delete scoped ACL users while their scopes are active:
    /// Redis's native user handles borrow those accounts.
    pub fn scoped_call_user(
        &self,
        user_name: &RedisString,
    ) -> Result<CallUserScope<'_>, RedisError> {
        // Resolve all required APIs before acquiring an owned native handle.
        let get_user = unsafe { crate::__available_api!(RedisModule_GetModuleUserFromUserName) };
        let set_user = unsafe { crate::__available_api!(RedisModule_SetContextUser) };
        let free = unsafe { crate::__available_api!(RedisModule_FreeModuleUser) };
        let user = unsafe { get_user(user_name.as_ptr()) };
        if user.is_null() {
            return Err(RedisError::Str(
                "ERR the user does not exist or is disabled",
            ));
        }
        let user = NativeUser { user, free };
        let view = self.call_users.get_or_init(|| {
            let state = CALL_USERS.with(|registry| {
                let mut registry = registry.borrow_mut();
                registry
                    .get(&(self.ctx as usize))
                    .and_then(Weak::upgrade)
                    .unwrap_or_else(|| {
                        let state = Rc::new(CallUserState {
                            ctx: self.ctx,
                            set_user,
                            baseline: Cell::new(ptr::null()),
                            frames: RefCell::new(Vec::new()),
                        });
                        registry.insert(self.ctx as usize, Rc::downgrade(&state));
                        state
                    })
            });
            CallUserView {
                state,
                owner: Rc::new(()),
            }
        });
        let index = view.state.push(&view.owner, user);
        Ok(CallUserScope { ctx: self, index })
    }

    /// Runs `f` with a scoped user for ACL-checked calls, restoring the enclosing
    /// user when this scope ends, including on panic. Calls must enable
    /// [`CallOptionsBuilder::verify_acl`](crate::CallOptionsBuilder::verify_acl) to enforce that user's permissions.
    ///
    /// A user lookup failure returns `Err` without running `f` or changing the
    /// active user. Otherwise returns `Ok(f(self))`: a closure returning a
    /// `Result` keeps that result nested, just like [`Self::with_db`].
    /// Explicit guards created inside `f` follow their own lifetimes.
    /// See [`Self::scoped_call_user`] for native override and account lifetimes.
    ///
    /// ```no_run
    /// use redis_module::{CallOptionsBuilder, CallResult, RedisError, RedisString, RuntimeContext};
    /// fn read<'c>(ctx: &'c RuntimeContext, user: &RedisString) -> Result<CallResult<'c>, RedisError> {
    ///     let options = CallOptionsBuilder::new().verify_acl().build();
    ///     ctx.with_call_user(user, |ctx| ctx.call_ext("GET", &options, &["allowed:item"]))
    /// }
    /// ```
    ///
    /// The closure receives only the capabilities of the view it was called on:
    ///
    /// ```compile_fail,E0599
    /// fn worker(ctx: &redis_module::WorkerContext, user: &redis_module::RedisString) {
    ///     ctx.with_call_user(user, |ctx| ctx.reply(Ok("OK".into()))).unwrap();
    /// }
    /// ```
    pub fn with_call_user<'s, T>(
        &'s self,
        user_name: &RedisString,
        f: impl FnOnce(&'s RuntimeContext) -> T,
    ) -> Result<T, RedisError> {
        let _scope = self.scoped_call_user(user_name)?;
        Ok(f(self))
    }

    pub(super) fn release_call_users(&mut self) {
        if let Some(view) = self.call_users.take() {
            view.state.remove_owner(&view.owner);
            if Rc::strong_count(&view.state) == 1 {
                CALL_USERS.with(|registry| {
                    registry.borrow_mut().remove(&(self.ctx as usize));
                });
            }
        }
    }
}

impl Drop for RuntimeContext {
    fn drop(&mut self) {
        self.release_call_users();
    }
}
