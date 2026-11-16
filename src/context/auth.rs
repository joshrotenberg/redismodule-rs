//! Module-owned users and privileged client identity attachment.
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CString, c_void};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{Context, RedisError, RedisString, RuntimeContext, raw};

/// A Redis connection ID. IDs do not keep clients alive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ClientId(pub u64);

/// One-shot cleanup for an attached client identity.
///
/// Runs with Redis locked. During SDK native calls, consumer delivery is deferred
/// until the outer native call returns. Native notifications outside such calls
/// deliver immediately. Reattachment from cleanup is refused.
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

// Own incoming state from method entry, including every preflight refusal.
struct PendingCallback(Option<ClientChangeCallback>);
impl Drop for PendingCallback {
    fn drop(&mut self) {
        contain(|| drop(self.0.take()));
    }
}
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
    deferred: VecDeque<(ClientId, Subscription)>,
}
static NATIVE_OPERATIONS: AtomicUsize = AtomicUsize::new(0);
static LIVE_SUBSCRIPTIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static REGISTRY_ACCESSES: AtomicUsize = AtomicUsize::new(0);
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    phase: Phase::Unloaded,
    owner: None,
    next: 0,
    users: BTreeMap::new(),
    subscriptions: BTreeMap::new(),
    executing: 0,
    deferred: VecDeque::new(),
});
fn registry() -> MutexGuard<'static, Registry> {
    #[cfg(test)]
    REGISTRY_ACCESSES.fetch_add(1, Ordering::Relaxed);
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
// Native code may still be using an incoming user/client after it invokes the
// change trampoline. Defer consumer code until the outer SDK FFI call returns.
// This also covers AUTH/HELLO and ACL commands made through RuntimeContext::call.
pub(crate) struct NativeOperation;
impl NativeOperation {
    pub(crate) fn begin() -> Self {
        // Count even before the first installation: a yielding native call can
        // process a command that adds a subscription. Atomics also keep queued
        // consumers deferred if a different lawful worker runs during a yield.
        NATIVE_OPERATIONS.fetch_add(1, Ordering::Relaxed);
        Self
    }
}
impl Drop for NativeOperation {
    fn drop(&mut self) {
        let outer = NATIVE_OPERATIONS.fetch_sub(1, Ordering::Relaxed) == 1;
        // No live authentication: constant-time atomics, no registry locking.
        if outer && LIVE_SUBSCRIPTIONS.load(Ordering::Relaxed) != 0 {
            loop {
                let next = registry().deferred.pop_front();
                let Some((client, subscription)) = next else {
                    break;
                };
                notify(client, subscription);
            }
        }
    }
}
fn notify(client: ClientId, mut subscription: Subscription) {
    registry().executing += 1;
    let callback = subscription.callback.take();
    if let Some(callback) = callback {
        contain(|| callback(client));
    }
    // Redis clears callback/privdata AFTER the trampoline returns. Retain the
    // cookie through consumer code so native recursive delivery can read it and
    // observe an already-consumed registry entry without dereferencing freed data.
    drop(subscription);
    registry().executing -= 1;
    LIVE_SUBSCRIPTIONS.fetch_sub(1, Ordering::Relaxed);
}
unsafe extern "C" fn changed(client: u64, private: *mut c_void) {
    contain(|| {
        // SAFETY: the native installation or notify's stack retains this cookie.
        let key = unsafe { (*(private.cast::<Cookie>())).0 };
        let subscription = {
            let mut registry = registry();
            let subscription = registry.subscriptions.remove(&key);
            if NATIVE_OPERATIONS.load(Ordering::Relaxed) != 0 {
                if let Some(subscription) = subscription {
                    registry
                        .deferred
                        .push_back((ClientId(client), subscription));
                }
                return;
            }
            subscription
        };
        if let Some(subscription) = subscription {
            notify(ClientId(client), subscription);
        }
    });
}

fn main_thread() -> Result<(), RedisError> {
    if crate::configuration::lifecycle::is_main_thread() {
        Ok(())
    } else {
        Err(RedisError::Str(
            "ERR client identity operations require the Redis main thread",
        ))
    }
}

// Validate only sdssplitargs' quote/escape grammar. Keep the exact input for
// Redis's native transactional ACL parser. Native versions through 8.10 ignore
// a NULL split result and can report success for malformed quoting.
fn valid_acl_quoting(rules: &[u8]) -> bool {
    fn space(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 11 | 12)
    }
    let mut i = 0;
    while i < rules.len() {
        while i < rules.len() && space(rules[i]) {
            i += 1;
        }
        if i == rules.len() {
            break;
        }
        let mut quote = 0;
        loop {
            let Some(&b) = rules.get(i) else {
                if quote != 0 {
                    return false;
                }
                break;
            };
            if quote == b'"' {
                if b == b'\\' && rules.get(i + 1).is_some() {
                    if rules.get(i + 1) == Some(&b'x')
                        && rules.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
                        && rules.get(i + 3).is_some_and(u8::is_ascii_hexdigit)
                    {
                        i += 4;
                    } else {
                        i += 2;
                    }
                    continue;
                }
                if b == b'"' {
                    if rules.get(i + 1).is_some_and(|b| !space(*b)) {
                        return false;
                    }
                    i += 1;
                    break;
                }
            } else if quote == b'\'' {
                if b == b'\\' && rules.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                if b == b'\'' {
                    if rules.get(i + 1).is_some_and(|b| !space(*b)) {
                        return false;
                    }
                    i += 1;
                    break;
                }
            } else {
                if matches!(b, b' ' | b'\n' | b'\r' | b'\t') {
                    i += 1;
                    break;
                }
                if b == b'"' || b == b'\'' {
                    quote = b;
                }
            }
            i += 1;
        }
    }
    true
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
        if !registry.users.is_empty()
            || !registry.subscriptions.is_empty()
            || !registry.deferred.is_empty()
        {
            main_thread()?;
        }
        if registry.executing != 0 || NATIVE_OPERATIONS.load(Ordering::Relaxed) != 0 {
            return Err(RedisError::Str("ERR module user cleanup is executing"));
        }
        registry.phase = Phase::Unloading;
        Ok(Self {
            ctx,
            committed: false,
        })
    }
    pub fn commit(mut self) -> Result<(), RedisError> {
        let operation = NativeOperation::begin();
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
        drop(operation);
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
    /// Requires the actual Redis main thread; a worker lock is insufficient.
    pub fn remove_module_user(&self, handle: &ModuleUserHandle) -> Result<(), RedisError> {
        main_thread()?;
        let _operation = NativeOperation::begin();
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
        let _operation = NativeOperation::begin();
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
    /// diagnostics, including password directives and hashes. Malformed native
    /// quote/escape syntax is rejected before FFI. Requires the Redis main thread
    /// because native ACL updates can synchronously destroy Pub/Sub clients.
    pub fn set_module_user_acl_string(
        &self,
        handle: &ModuleUserHandle,
        rules: &str,
    ) -> Result<(), RedisError> {
        main_thread()?;
        if !valid_acl_quoting(rules.as_bytes()) {
            return Err(RedisError::Str("ERR invalid ACL rules"));
        }
        let set = unsafe { crate::__available_api!(RedisModule_SetModuleUserACLString) };
        let rules = CString::new(rules).map_err(|_| RedisError::Str("ERR invalid ACL rules"))?;
        let _operation = NativeOperation::begin();
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
    /// Requires the Redis main thread: a worker lock does not pause I/O threads.
    /// SDK consumer cleanup runs after native revocation returns.
    pub fn deauthenticate_and_close_client(&self, client: ClientId) -> Result<(), RedisError> {
        main_thread()?;
        let _operation = NativeOperation::begin();
        let revoke = unsafe { crate::__available_api!(RedisModule_DeauthenticateAndCloseClient) };
        if unsafe { revoke(self.ctx, client.0) } == raw::REDISMODULE_OK as i32 {
            Ok(())
        } else {
            Err(RedisError::Str("ERR client does not exist"))
        }
    }

    /// Copies a peer certificate as owned PEM bytes (Redis 6.0+). `None` can
    /// mean missing client, non-TLS connection or no client certificate.
    /// Requires Redis's main thread for native connection access.
    pub fn client_certificate(&self, client: ClientId) -> Result<Option<Vec<u8>>, RedisError> {
        main_thread()?;
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
        let pending = PendingCallback(on_changed);
        main_thread()?;
        let _operation = NativeOperation::begin();
        let attach = unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithUser) };
        let user = registry().user(user)?;
        self.attach_identity(pending, |callback, private, id| unsafe {
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
        let pending = PendingCallback(on_changed);
        main_thread()?;
        let _operation = NativeOperation::begin();
        let attach = unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithACLUser) };
        self.attach_identity(pending, |callback, private, id| unsafe {
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
        mut callback: PendingCallback,
        attach: impl FnOnce(raw::RedisModuleUserChangedFunc, *mut c_void, *mut u64) -> i32,
    ) -> Result<ClientId, RedisError> {
        registry().active()?;
        // Redis accepts CLIENT_LUA in the native attach API even though that
        // internal client is absent from the ID index used for revocation.
        // Only transfer subscriptions for a real, ID-addressable connection.
        let get_id = unsafe { crate::__available_api!(RedisModule_GetClientId) };
        let get_info = unsafe { crate::__available_api!(RedisModule_GetClientInfoById) };
        let id = unsafe { get_id(self.ctx) };
        if id == 0 || unsafe { get_info(std::ptr::null_mut(), id) } != raw::REDISMODULE_OK as i32 {
            return Err(RedisError::Str(
                "ERR identity attachment requires a connected client",
            ));
        }
        // Every successful installation must be revocable during SDK unload.
        let _ = unsafe { crate::__available_api!(RedisModule_DeauthenticateAndCloseClient) };
        let (key, private) = {
            let mut registry = registry();
            registry.active()?;
            if registry.executing != 0 {
                return Err(RedisError::Str(
                    "ERR identity attachment is refused during client cleanup",
                ));
            }
            let key = registry.next()?;
            let mut subscription = Subscription {
                cookie: Box::new(Cookie(key)),
                client: None,
                callback: callback.0.take(),
            };
            let private = (&mut *subscription.cookie as *mut Cookie).cast();
            registry.subscriptions.insert(key, subscription);
            LIVE_SUBSCRIPTIONS.fetch_add(1, Ordering::Relaxed);
            (key, private)
        };
        let mut id = 0;
        if attach(Some(changed), private, &mut id) != raw::REDISMODULE_OK as i32 {
            let failed = registry().subscriptions.remove(&key);
            contain(|| drop(failed));
            LIVE_SUBSCRIPTIONS.fetch_sub(1, Ordering::Relaxed);
            return Err(RedisError::Str(
                "ERR Redis refused client identity attachment",
            ));
        }
        registry().subscriptions.get_mut(&key).unwrap().client = Some(ClientId(id));
        Ok(ClientId(id))
    }

    /// Redacts a current command argument from MONITOR, SLOWLOG and server logs
    /// (Redis 7.0+). Position zero and out-of-range positions are rejected.
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
    fn native_call_without_auth_has_no_registry_access() {
        assert_eq!(LIVE_SUBSCRIPTIONS.load(Ordering::Relaxed), 0);
        let before = REGISTRY_ACCESSES.load(Ordering::Relaxed);
        for _ in 0..10_000 {
            let operation = NativeOperation::begin();
            drop(operation);
        }
        assert_eq!(REGISTRY_ACCESSES.load(Ordering::Relaxed), before);
    }
    #[test]
    fn acl_quote_validation_preserves_native_lexical_forms() {
        for valid in [
            "",
            "  ",
            "reset on +get",
            "'on' \"+get\"",
            "~prefix\"suffix\"",
            "\"~a\\x22b\"",
            "'~a\\'b'",
            "\"~a\\nb\"",
            "\"~a\\\\b\"",
        ] {
            assert!(valid_acl_quoting(valid.as_bytes()), "{valid:?}");
        }
        for invalid in [
            "'on",
            "\"on",
            "'on'suffix",
            "\"on\"suffix",
            "\"on\\\"",
            "\"on\\",
        ] {
            assert!(!valid_acl_quoting(invalid.as_bytes()), "{invalid:?}");
        }
    }
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
            deferred: VecDeque::new(),
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
