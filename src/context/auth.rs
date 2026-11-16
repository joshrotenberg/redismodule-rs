//! Module-owned users and privileged client identity attachment.
use std::collections::BTreeMap;
use std::ffi::{CString, c_void};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{Context, RedisError, RedisString, RuntimeContext, raw};

/// A Redis connection ID. IDs do not keep clients alive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ClientId(pub u64);

/// One-shot cleanup for an attached client identity.
///
/// Runs with Redis locked, possibly on a worker holding a thread-safe context.
/// Captures must be owned and `Send`. No context is provided by Redis; do not
/// reacquire the Redis lock in this callback. It also runs on same-user reauth,
/// disconnect, explicit revocation and successful module unload. Panics are
/// contained without logging their payload (the process panic hook still runs).
///
/// ```compile_fail,E0277
/// use redis_module::ClientChangeCallback;
/// let local = std::rc::Rc::new(1);
/// let callback: ClientChangeCallback = Box::new(move |_| drop(local));
/// ```
pub type ClientChangeCallback = Box<dyn FnOnce(ClientId) + Send + 'static>;

/// An opaque identity for a user owned by this SDK module's current load.
///
/// Clones identify the same owner. Dropping a handle does not call Redis or
/// remove the user. Use [`RuntimeContext::remove_module_user`] with a real lock
/// capability, or let successful module unload remove remaining users. Removed,
/// foreign-module and previous-load handles are rejected before native access.
#[derive(Clone)]
pub struct ModuleUserHandle {
    owner: Arc<()>,
    generation: u64,
}
impl fmt::Debug for ModuleUserHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModuleUserHandle")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

struct NativeUser {
    ptr: NonNull<raw::RedisModuleUser>,
    free: unsafe extern "C" fn(*mut raw::RedisModuleUser),
    name: CString,
}
// SAFETY: native access requires a RuntimeContext lock capability. Moving the
// inert owner never calls Redis, and the registry never holds a lock across FFI.
unsafe impl Send for NativeUser {}

struct Cookie(u64);
struct Subscription {
    cookie: Box<Cookie>,
    client: Option<ClientId>,
    callback: Option<ClientChangeCallback>,
}
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Unloaded,
    Loading,
    Active,
    Unloading,
}
struct Registry {
    phase: Phase,
    owner: Option<Arc<()>>,
    next: u64,
    users: BTreeMap<u64, NativeUser>,
    subscriptions: BTreeMap<u64, Subscription>,
    executing: usize,
}
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    phase: Phase::Unloaded,
    owner: None,
    next: 0,
    users: BTreeMap::new(),
    subscriptions: BTreeMap::new(),
    executing: 0,
});
fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}
const INACTIVE: RedisError = RedisError::Str("ERR module user lifecycle is not active");
const INVALID: RedisError = RedisError::Str("ERR stale or foreign module user handle");
impl Registry {
    fn active(&self) -> Result<(), RedisError> {
        if self.phase == Phase::Active {
            Ok(())
        } else {
            Err(INACTIVE)
        }
    }
    fn next(&mut self) -> Result<u64, RedisError> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or(RedisError::Str("ERR module user identity exhausted"))?;
        Ok(self.next)
    }
    fn user(&self, handle: &ModuleUserHandle) -> Result<*mut raw::RedisModuleUser, RedisError> {
        self.active()?;
        if !self
            .owner
            .as_ref()
            .is_some_and(|o| Arc::ptr_eq(o, &handle.owner))
        {
            return Err(INVALID);
        }
        self.users
            .get(&handle.generation)
            .map(|u| u.ptr.as_ptr())
            .ok_or(INVALID)
    }
}

// Do not turn secret-bearing panic payloads into logs or error text. Leaking a
// caught payload also prevents its destructor from unwinding through C.
fn contain(f: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(f)) {
        std::mem::forget(payload);
    }
}
unsafe extern "C" fn changed(client: u64, private: *mut c_void) {
    contain(|| {
        // SAFETY: each installed native callback owns one live registry cookie.
        let key = unsafe { (*(private.cast::<Cookie>())).0 };
        let subscription = {
            let mut registry = registry();
            let subscription = registry.subscriptions.remove(&key);
            if subscription.is_some() {
                registry.executing += 1;
            }
            subscription
        };
        if let Some(mut subscription) = subscription {
            // Remove ownership before user code and run without a registry lock.
            let callback = subscription.callback.take();
            drop(subscription);
            if let Some(callback) = callback {
                contain(|| callback(ClientId(client)));
            }
            registry().executing -= 1;
        }
    });
}

/// Internal `redis_module!` lifecycle binding, not a custom-loader API.
#[doc(hidden)]
pub struct UserLoadGuard {
    committed: bool,
}
impl UserLoadGuard {
    /// # Safety
    /// Call once in this SDK module's OnLoad, before user code.
    pub unsafe fn begin() -> Result<Self, RedisError> {
        let mut registry = registry();
        if registry.phase != Phase::Unloaded {
            return Err(INACTIVE);
        }
        registry.phase = Phase::Loading;
        registry.owner = Some(Arc::new(()));
        Ok(Self { committed: false })
    }
    pub fn commit(mut self) {
        registry().phase = Phase::Active;
        self.committed = true;
    }
}
impl Drop for UserLoadGuard {
    fn drop(&mut self) {
        if !self.committed {
            let mut registry = registry();
            registry.phase = Phase::Unloaded;
            registry.owner = None;
        }
    }
}

/// Internal `redis_module!` lifecycle binding, not a custom-loader API.
#[doc(hidden)]
pub struct UserUnloadGuard<'a> {
    ctx: &'a RuntimeContext,
    committed: bool,
}
impl<'a> UserUnloadGuard<'a> {
    /// # Safety
    /// Call only in this SDK module's OnUnload, before user code.
    pub unsafe fn begin(ctx: &'a RuntimeContext) -> Result<Self, RedisError> {
        let mut registry = registry();
        registry.active()?;
        if registry.executing != 0 {
            return Err(RedisError::Str("ERR module user cleanup is executing"));
        }
        registry.phase = Phase::Unloading;
        Ok(Self {
            ctx,
            committed: false,
        })
    }
    pub fn commit(mut self) -> Result<(), RedisError> {
        // Authentication preflights this API before installing any subscription.
        let clients: Vec<_> = registry()
            .subscriptions
            .values()
            .filter_map(|s| s.client)
            .collect();
        for client in clients {
            // SAFETY: OnUnload holds the Redis lock. Revocation synchronously
            // consumes the callback before Redis schedules the connection close.
            unsafe {
                raw::RedisModule_DeauthenticateAndCloseClient.unwrap()(self.ctx.ctx, client.0);
            }
        }
        if !registry().subscriptions.is_empty() {
            return Err(RedisError::Str(
                "ERR module client cleanup is still pending",
            ));
        }
        // Invalidate every handle before freeing: native free can invoke cleanup.
        let users = {
            let mut registry = registry();
            registry.owner = None;
            std::mem::take(&mut registry.users)
        };
        for (_, user) in users {
            unsafe {
                (user.free)(user.ptr.as_ptr());
            }
        }
        // Native revocation has drained each successful installation. Redis
        // itself performs the same synchronous cleanup when unloading modules.
        registry().phase = Phase::Unloaded;
        self.committed = true;
        Ok(())
    }
}
impl Drop for UserUnloadGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            registry().phase = Phase::Active;
        }
    }
}

impl RuntimeContext {
    /// Creates an unlinked Redis ACL user (Redis 6.0+).
    ///
    /// Requires a successfully loaded `redis_module!` module. Creation during
    /// init or unload is refused, including loads Redis may later reject. Names
    /// must be unique among this module's live created users. Users are initially
    /// disabled; configure their permissions before attaching a client.
    pub fn create_module_user(&self, name: &str) -> Result<ModuleUserHandle, RedisError> {
        let create = unsafe { crate::__available_api!(RedisModule_CreateModuleUser) };
        let free = unsafe { crate::__available_api!(RedisModule_FreeModuleUser) };
        let name = CString::new(name).map_err(|_| RedisError::Str("ERR user name contains NUL"))?;
        let (owner, generation) = {
            let mut registry = registry();
            registry.active()?;
            if registry.users.values().any(|u| u.name == name) {
                return Err(RedisError::Str("ERR duplicate module user name"));
            }
            (registry.owner.as_ref().unwrap().clone(), registry.next()?)
        };
        let ptr = NonNull::new(unsafe { create(name.as_ptr()) })
            .ok_or(RedisError::Str("ERR Redis could not create module user"))?;
        registry()
            .users
            .insert(generation, NativeUser { ptr, free, name });
        Ok(ModuleUserHandle { owner, generation })
    }

    /// Removes a created user exactly once and revokes its clients (Redis 6.0+).
    /// Invalidates all clones before native free and any resulting callbacks.
    pub fn remove_module_user(&self, handle: &ModuleUserHandle) -> Result<(), RedisError> {
        let user = {
            let mut registry = registry();
            registry.user(handle)?;
            registry.users.remove(&handle.generation).unwrap()
        };
        unsafe {
            (user.free)(user.ptr.as_ptr());
        }
        Ok(())
    }

    /// Applies one ACL directive (Redis 6.0+). This is not an atomic batch.
    /// Errors omit the directive and native parser diagnostics, which may contain secrets.
    pub fn set_module_user_acl(
        &self,
        handle: &ModuleUserHandle,
        directive: &str,
    ) -> Result<(), RedisError> {
        let set = unsafe { crate::__available_api!(RedisModule_SetModuleUserACL) };
        let directive =
            CString::new(directive).map_err(|_| RedisError::Str("ERR invalid ACL directive"))?;
        let user = registry().user(handle)?;
        if unsafe { set(user, directive.as_ptr()) } == raw::REDISMODULE_OK as i32 {
            Ok(())
        } else {
            Err(RedisError::Str("ERR invalid ACL directive"))
        }
    }

    /// Atomically applies a complete ACL rule string using Redis's transactional
    /// setter (Redis 7.0+). On error existing rules remain unchanged. No fallback
    /// to repeated single-rule calls is used. Errors omit ACL text and native
    /// diagnostics, including password directives and hashes.
    pub fn set_module_user_acl_string(
        &self,
        handle: &ModuleUserHandle,
        rules: &str,
    ) -> Result<(), RedisError> {
        let set = unsafe { crate::__available_api!(RedisModule_SetModuleUserACLString) };
        let rules = CString::new(rules).map_err(|_| RedisError::Str("ERR invalid ACL rules"))?;
        let user = registry().user(handle)?;
        // Passing no error slot makes Redis free any secret-bearing diagnostic.
        if unsafe { set(self.ctx, user, rules.as_ptr(), std::ptr::null_mut()) }
            == raw::REDISMODULE_OK as i32
        {
            Ok(())
        } else {
            Err(RedisError::Str("ERR invalid ACL rules"))
        }
    }

    /// Revokes and asynchronously closes a connection (Redis 6.0+).
    /// Cleanup is synchronous and can run on the thread holding this lock.
    pub fn deauthenticate_and_close_client(&self, client: ClientId) -> Result<(), RedisError> {
        let revoke = unsafe { crate::__available_api!(RedisModule_DeauthenticateAndCloseClient) };
        if unsafe { revoke(self.ctx, client.0) } == raw::REDISMODULE_OK as i32 {
            Ok(())
        } else {
            Err(RedisError::Str("ERR client does not exist"))
        }
    }

    /// Copies a peer certificate as owned PEM bytes (Redis 6.0+). `None` can
    /// mean missing client, non-TLS connection or no client certificate.
    pub fn client_certificate(&self, client: ClientId) -> Result<Option<Vec<u8>>, RedisError> {
        let get = unsafe { crate::__available_api!(RedisModule_GetClientCertificate) };
        let string = unsafe { get(self.ctx, client.0) };
        // SAFETY: self is the actual producer context, including AutoMemory.
        let owned = unsafe { RedisString::from_owned_ptr_or_none(NonNull::new(self.ctx), string) };
        Ok(owned.map(|s| s.as_slice().to_vec()))
    }
}

impl Context {
    /// Attaches a configured module-created identity to this connection (Redis
    /// 6.0+). This privileged operation does not check a password. It returns the
    /// native attachment outcome, not the result of an AUTH/HELLO exchange.
    /// Installs SDK cleanup even when `on_changed` is `None`, so module unload
    /// revokes the connection. See [`ClientChangeCallback`] for execution rules.
    pub fn authenticate_client_with_user(
        &self,
        user: &ModuleUserHandle,
        on_changed: Option<ClientChangeCallback>,
    ) -> Result<ClientId, RedisError> {
        let attach = unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithUser) };
        let user = registry().user(user)?;
        self.attach_identity(on_changed, |callback, private, id| unsafe {
            attach(self.ctx, user, callback, private, id)
        })
    }

    /// Attaches an existing ACL account by name (Redis 6.0+). The name is resolved
    /// by Redis at this call, never cached as a borrowed native user handle.
    /// Missing and disabled accounts fail. This does not check a password.
    /// Installs cleanup even without a consumer callback; see [`ClientChangeCallback`].
    pub fn authenticate_client_with_acl_user(
        &self,
        name: &[u8],
        on_changed: Option<ClientChangeCallback>,
    ) -> Result<ClientId, RedisError> {
        let attach = unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithACLUser) };
        self.attach_identity(on_changed, |callback, private, id| unsafe {
            attach(
                self.ctx,
                name.as_ptr().cast(),
                name.len(),
                callback,
                private,
                id,
            )
        })
    }

    fn attach_identity(
        &self,
        callback: Option<ClientChangeCallback>,
        attach: impl FnOnce(raw::RedisModuleUserChangedFunc, *mut c_void, *mut u64) -> i32,
    ) -> Result<ClientId, RedisError> {
        // Every successful installation must be revocable during SDK unload.
        let _ = unsafe { crate::__available_api!(RedisModule_DeauthenticateAndCloseClient) };
        let (key, private) = {
            let mut registry = registry();
            registry.active()?;
            let key = registry.next()?;
            let mut subscription = Subscription {
                cookie: Box::new(Cookie(key)),
                client: None,
                callback,
            };
            let private = (&mut *subscription.cookie as *mut Cookie).cast();
            registry.subscriptions.insert(key, subscription);
            (key, private)
        };
        let mut id = 0;
        if attach(Some(changed), private, &mut id) != raw::REDISMODULE_OK as i32 {
            let failed = registry().subscriptions.remove(&key);
            contain(|| drop(failed));
            return Err(RedisError::Str(
                "ERR Redis refused client identity attachment",
            ));
        }
        registry().subscriptions.get_mut(&key).unwrap().client = Some(ClientId(id));
        Ok(ClientId(id))
    }

    /// Redacts a current command argument from MONITOR, SLOWLOG and server logs
    /// (Redis 6.2+). Position zero and out-of-range positions are rejected.
    pub fn redact_client_command_argument(&self, position: usize) -> Result<(), RedisError> {
        let redact = unsafe { crate::__available_api!(RedisModule_RedactClientCommandArgument) };
        let position = i32::try_from(position)
            .map_err(|_| RedisError::Str("ERR invalid command argument position"))?;
        if unsafe { redact(self.ctx, position) } == raw::REDISMODULE_OK as i32 {
            Ok(())
        } else {
            Err(RedisError::Str("ERR invalid command argument position"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_and_previous_load_handles_never_resolve() {
        let owner = Arc::new(());
        let mut registry = Registry {
            phase: Phase::Active,
            owner: Some(owner.clone()),
            next: 1,
            users: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            executing: 0,
        };
        unsafe extern "C" fn never_free(_: *mut raw::RedisModuleUser) {
            panic!("must never free");
        }
        registry.users.insert(
            1,
            NativeUser {
                ptr: NonNull::dangling(),
                free: never_free,
                name: c"test".into(),
            },
        );
        let valid = ModuleUserHandle {
            owner: owner.clone(),
            generation: 1,
        };
        assert!(registry.user(&valid).is_ok());
        let foreign = ModuleUserHandle {
            owner: Arc::new(()),
            generation: 1,
        };
        assert!(registry.user(&foreign).is_err());
        registry.owner = Some(Arc::new(()));
        assert!(registry.user(&valid).is_err());
        registry.owner = Some(owner);
        registry.users.remove(&1);
        assert!(registry.user(&valid).is_err());
        // Reusing a native name still needs a fresh monotonic generation.
        assert_eq!(registry.next().unwrap(), 2);
    }
}
