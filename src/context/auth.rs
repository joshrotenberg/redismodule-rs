//! Module-owned users and privileged client identity attachment.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{CString, c_void};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{Context, RedisError, RedisString, RuntimeContext, raw};

mod callback;
pub use callback::{
    AuthCallback, AuthContext, AuthDecision, AuthRequest, AuthTarget, Password, PublicAuthError,
};
#[doc(hidden)]
pub use callback::{auth_callback_available, register_auth_callback, run_auth_callback};

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
/// disconnect and explicit revocation. Panics are
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
/// remove the user. Use [`Context::remove_module_user`] with a real lock
/// capability, before unloading the module. Removed,
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
    leased: bool,
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
#[cfg(test)]
impl Drop for Cookie {
    fn drop(&mut self) {
        tests::COOKIE_DROPS.fetch_add(1, Ordering::Relaxed);
    }
}
struct Subscription {
    cookie: Box<Cookie>,
    installed_client: Option<ClientId>,
    pending_client: Option<ClientId>,
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
    closing: BTreeSet<ClientId>,
    removing: BTreeSet<u64>,
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
    closing: BTreeSet::new(),
    removing: BTreeSet::new(),
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
    fn has_auth_ownership(&self) -> bool {
        !self.users.is_empty()
            || !self.subscriptions.is_empty()
            || !self.deferred.is_empty()
            || !self.closing.is_empty()
            || !self.removing.is_empty()
            || self.executing != 0
            || callback::in_flight()
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

// Redis can invoke lawful raw server-event hooks before its auth-change
// callback. Keep created users exclusive across every native pointer borrow,
// independently of SDK consumer deferral.
struct UserLease {
    generation: u64,
    ptr: *mut raw::RedisModuleUser,
}
impl UserLease {
    fn begin(handle: &ModuleUserHandle) -> Result<Self, RedisError> {
        let mut registry = registry();
        let ptr = registry.user(handle)?;
        let user = registry.users.get_mut(&handle.generation).unwrap();
        if user.leased {
            return Err(RedisError::Str("ERR module user is in a native operation"));
        }
        user.leased = true;
        Ok(Self {
            generation: handle.generation,
            ptr,
        })
    }
}
impl Drop for UserLease {
    fn drop(&mut self) {
        registry().users.get_mut(&self.generation).unwrap().leased = false;
    }
}

// Invalidate handles before native free, but keep the local native owner
// observable to unload until that free returns. The last removal can otherwise
// make the maps empty while a lawful raw callback still runs inside native free.
struct UserRemoval {
    generation: u64,
    user: NativeUser,
}
impl UserRemoval {
    fn begin(handle: &ModuleUserHandle) -> Result<Self, RedisError> {
        let mut registry = registry();
        registry.user(handle)?;
        if registry.users.get(&handle.generation).unwrap().leased {
            return Err(RedisError::Str("ERR module user is in a native operation"));
        }
        let user = registry.users.remove(&handle.generation).unwrap();
        registry.removing.insert(handle.generation);
        Ok(Self {
            generation: handle.generation,
            user,
        })
    }
}
impl Drop for UserRemoval {
    fn drop(&mut self) {
        registry().removing.remove(&self.generation);
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
pub struct UserUnloadGuard {
    committed: bool,
}
impl UserUnloadGuard {
    /// # Safety
    /// Call only in this SDK module's OnUnload, before user code.
    pub unsafe fn begin(_: &RuntimeContext) -> Result<Self, RedisError> {
        let mut registry = registry();
        registry.active()?;
        // OnUnload has a fresh temporary context, which cannot prove the phase
        // of an outer foreign native callback. Require explicit command-side
        // drain instead of performing any client-affecting cleanup here.
        if registry.has_auth_ownership() {
            return Err(RedisError::Str(
                "ERR drain module users and client identities before unload",
            ));
        }
        // An unrelated generic SDK call must preserve existing no-auth unload
        // behavior. An auth operation always owns a user or subscription above.
        registry.phase = Phase::Unloading;
        Ok(Self { committed: false })
    }
    pub fn commit(mut self) -> Result<(), RedisError> {
        let mut registry = registry();
        registry.owner = None;
        // Rust statics are not dropped by dlclose. Release empty queue capacity.
        registry.deferred = VecDeque::new();
        registry.phase = Phase::Unloaded;
        self.committed = true;
        Ok(())
    }
}
impl Drop for UserUnloadGuard {
    fn drop(&mut self) {
        if !self.committed {
            registry().phase = Phase::Active;
        }
    }
}

// A Context can also come from a blocked-client reply. Require the exact SDK
// command dispatch, an indexed connection, and a quiescent native-call boundary.
// Raw callbacks may borrow a scoped command Context; the depth check excludes
// that reuse while any guarded SDK call or Yield still owns native state.
fn command_client(ctx: &Context) -> Result<ClientId, RedisError> {
    registry().active()?;
    main_thread()?;
    if NATIVE_OPERATIONS.load(Ordering::Relaxed) != 0 || registry().executing != 0 {
        return Err(RedisError::Str(
            "ERR client identity mutation requires a quiescent command",
        ));
    }
    if !super::ordinary_running_command(ctx.ctx) {
        return Err(RedisError::Str(
            "ERR client identity mutation requires an ordinary SDK command",
        ));
    }
    let get_flags = unsafe { crate::__available_api!(RedisModule_GetContextFlags) };
    let flags = unsafe { get_flags(ctx.ctx) };
    if flags
        & (raw::REDISMODULE_CTX_FLAGS_LOADING | raw::REDISMODULE_CTX_FLAGS_ASYNC_LOADING) as i32
        != 0
    {
        return Err(RedisError::Str(
            "ERR client identity mutation is refused while Redis is loading",
        ));
    }
    let get_id = unsafe { crate::__available_api!(RedisModule_GetClientId) };
    let get_info = unsafe { crate::__available_api!(RedisModule_GetClientInfoById) };
    let id = unsafe { get_id(ctx.ctx) };
    if id == 0 || unsafe { get_info(std::ptr::null_mut(), id) } != raw::REDISMODULE_OK as i32 {
        return Err(RedisError::Str(
            "ERR client identity mutation requires a connected client",
        ));
    }
    Ok(ClientId(id))
}

struct Closing(ClientId);
impl Drop for Closing {
    fn drop(&mut self) {
        registry().closing.remove(&self.0);
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
        registry().users.insert(
            generation,
            NativeUser {
                ptr,
                free,
                name,
                leased: false,
            },
        );
        Ok(ModuleUserHandle { owner, generation })
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
        let lease = UserLease::begin(handle)?;
        let user = lease.ptr;
        if unsafe { set(user, directive.as_ptr()) } == raw::REDISMODULE_OK as i32 {
            Ok(())
        } else {
            Err(RedisError::Str("ERR invalid ACL directive"))
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
    /// Removes a created user exactly once and revokes its clients (Redis 6.0+).
    /// Invalidates all clones before native free and any resulting callbacks.
    /// Requires a quiescent ordinary SDK command on an indexed connection.
    /// Event, blocked-reply, internal-call, allow-busy and nested native-call
    /// contexts are refused before mutation. See [the guide](crate::Context::authenticate_client_with_user).
    pub fn remove_module_user(&self, handle: &ModuleUserHandle) -> Result<(), RedisError> {
        command_client(self)?;
        let _operation = NativeOperation::begin();
        let removal = UserRemoval::begin(handle)?;
        unsafe {
            (removal.user.free)(removal.user.ptr.as_ptr());
        }
        Ok(())
    }

    /// Atomically applies a complete ACL rule string using Redis's transactional
    /// setter (Redis 7.4+). On error existing rules remain unchanged. No fallback
    /// to repeated single-rule calls is used. Errors omit ACL text and native
    /// diagnostics, including password directives and hashes. Malformed native
    /// quote/escape syntax is rejected before FFI. Requires the Redis main thread
    /// for native client and event-loop access. Redis before 7.4 is refused
    /// before setter FFI: synchronous Pub/Sub teardown can invalidate a shared
    /// native client iterator through lawful raw callbacks. No batch fallback
    /// is used. The native version must be available and verifiable. Requires
    /// the same quiescent ordinary-command capability as identity attachment.
    pub fn set_module_user_acl_string(
        &self,
        handle: &ModuleUserHandle,
        rules: &str,
    ) -> Result<(), RedisError> {
        command_client(self)?;
        let set = unsafe { crate::__available_api!(RedisModule_SetModuleUserACLString) };
        let get_version = unsafe { crate::__available_api!(RedisModule_GetServerVersion) };
        let encoded = unsafe { get_version() };
        if encoded <= 0 || encoded & !0x00ff_ffff != 0 {
            return Err(RedisError::Str(
                "ERR cannot verify Redis runtime for transactional ACL updates",
            ));
        }
        let version = raw::Version::from(encoded);
        if (version.major, version.minor) < (7, 4) {
            return Err(RedisError::Str(
                "ERR transactional ACL updates require Redis 7.4 or later",
            ));
        }
        if !valid_acl_quoting(rules.as_bytes()) {
            return Err(RedisError::Str("ERR invalid ACL rules"));
        }
        let rules = CString::new(rules).map_err(|_| RedisError::Str("ERR invalid ACL rules"))?;
        let _operation = NativeOperation::begin();
        let lease = UserLease::begin(handle)?;
        let user = lease.ptr;
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
    /// Requires this target's actually installed current-load SDK subscription.
    /// Missing, stale, foreign, pending and closing ownership is refused without
    /// native revocation. Only quiescent ordinary SDK commands are admitted.
    /// SDK consumer cleanup runs after native revocation returns.
    pub fn deauthenticate_and_close_client(&self, client: ClientId) -> Result<(), RedisError> {
        command_client(self)?;
        let revoke = unsafe { crate::__available_api!(RedisModule_DeauthenticateAndCloseClient) };
        let _closing = {
            let mut registry = registry();
            if registry.closing.contains(&client)
                || registry
                    .subscriptions
                    .values()
                    .any(|s| s.pending_client == Some(client))
                || !registry
                    .subscriptions
                    .values()
                    .any(|s| s.installed_client == Some(client))
            {
                return Err(RedisError::Str(
                    "ERR revocation requires an installed SDK client identity",
                ));
            }
            registry.closing.insert(client);
            Closing(client)
        };
        let _operation = NativeOperation::begin();
        if unsafe { revoke(self.ctx, client.0) } == raw::REDISMODULE_OK as i32 {
            Ok(())
        } else {
            Err(RedisError::Str("ERR client does not exist"))
        }
    }

    /// Attaches a configured module-created identity to this connection (Redis
    /// 6.0+). This privileged operation does not check a password. It returns the
    /// native attachment outcome, not the result of an AUTH/HELLO exchange.
    /// Requires this exact SDK command invocation on an indexed connection, on
    /// Redis's main thread, outside any SDK native call or cleanup consumer.
    /// Allow-busy commands and unknown/raw command registrations are refused.
    /// Native LOADING and ASYNC_LOADING states are refused, because loading
    /// Yield can process events without the usual busy-command exclusion.
    /// A blocked-client reply Context does not establish command provenance.
    /// Raw FFI that bypasses SDK native-operation scopes does not establish this
    /// safety boundary. Legal raw callbacks inside guarded SDK calls are refused
    /// when they reenter through a scoped command Context.
    /// Installs SDK cleanup even when `on_changed` is `None`, so explicit command-side drain
    /// can revoke the connection. See [`ClientChangeCallback`] for execution rules.
    pub fn authenticate_client_with_user(
        &self,
        user: &ModuleUserHandle,
        on_changed: Option<ClientChangeCallback>,
    ) -> Result<ClientId, RedisError> {
        let pending = PendingCallback(on_changed);
        let client = command_client(self)?;
        let _operation = NativeOperation::begin();
        let attach = unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithUser) };
        let lease = UserLease::begin(user)?;
        let user = lease.ptr;
        attach_identity(client, pending, false, |callback, private, id| unsafe {
            attach(self.ctx, user, callback, private, id)
        })
    }

    /// Attaches an existing ACL account by name (Redis 6.0+). The name is resolved
    /// by Redis at this call, never cached as a borrowed native user handle.
    /// Requires this connection's current native callback to be an actually
    /// installed SDK subscription. First attach a caller-selected created user
    /// explicitly. Unknown, foreign, pending or detached ownership is refused
    /// before native account lookup or identity change; no hidden bootstrap is
    /// performed. This prevents a foreign prior callback from deleting the
    /// incoming borrowed ACL account while native attachment still uses it.
    /// Missing and disabled accounts fail. This does not check a password.
    /// Requires the same quiescent ordinary-command capability as created-user
    /// attachment. Installs cleanup even without a consumer callback; see [`ClientChangeCallback`].
    pub fn authenticate_client_with_acl_user(
        &self,
        name: &[u8],
        on_changed: Option<ClientChangeCallback>,
    ) -> Result<ClientId, RedisError> {
        let pending = PendingCallback(on_changed);
        let client = command_client(self)?;
        let _operation = NativeOperation::begin();
        let attach = unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithACLUser) };
        attach_identity(client, pending, true, |callback, private, id| unsafe {
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

fn attach_identity(
    client: ClientId,
    mut callback: PendingCallback,
    require_installed: bool,
    attach: impl FnOnce(raw::RedisModuleUserChangedFunc, *mut c_void, *mut u64) -> i32,
) -> Result<ClientId, RedisError> {
    registry().active()?;
    let id = client.0;
    let (key, private) = {
        let mut registry = registry();
        registry.active()?;
        if registry.executing != 0 || registry.closing.contains(&client) {
            return Err(RedisError::Str(
                "ERR identity attachment is refused during client cleanup",
            ));
        }
        // None is a preallocated, not-yet-installed registration. Only
        // native success below establishes ownership for this client ID.
        // Detached/deferred callbacks have already left this map.
        if require_installed
            && !registry
                .subscriptions
                .values()
                .any(|s| s.installed_client == Some(ClientId(id)))
        {
            return Err(RedisError::Str(
                "ERR ACL attachment requires an installed SDK client identity",
            ));
        }
        if registry
            .subscriptions
            .values()
            .any(|s| s.pending_client == Some(ClientId(id)))
        {
            return Err(RedisError::Str(
                "ERR client identity attachment is already in progress",
            ));
        }
        let key = registry.next()?;
        let mut subscription = Subscription {
            cookie: Box::new(Cookie(key)),
            installed_client: None,
            pending_client: Some(ClientId(id)),
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
    let mut registry = registry();
    let installed = registry.subscriptions.get_mut(&key).unwrap();
    installed.pending_client = None;
    installed.installed_client = Some(ClientId(id));
    Ok(ClientId(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) static PROTOCOL_TEST: Mutex<()> = Mutex::new(());
    pub(super) static COOKIE_DROPS: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn first_subscription_waits_for_outer_native_guard_across_threads() {
        let _serial = PROTOCOL_TEST.lock().unwrap();
        assert_eq!(LIVE_SUBSCRIPTIONS.load(Ordering::Relaxed), 0);
        assert_eq!(NATIVE_OPERATIONS.load(Ordering::Relaxed), 0);
        COOKIE_DROPS.store(0, Ordering::Relaxed);
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        struct OwnedState(Arc<AtomicUsize>);
        impl Drop for OwnedState {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Begin with no live auth. The first installation and notification
        // arrive inside the already-running native/Yield operation.
        let outer = NativeOperation::begin();
        let callback_calls = calls.clone();
        let state = OwnedState(drops.clone());
        let mut subscription = Subscription {
            cookie: Box::new(Cookie(42)),
            installed_client: Some(ClientId(7)),
            pending_client: None,
            callback: Some(Box::new(move |client| {
                assert_eq!(client, ClientId(7));
                callback_calls.fetch_add(1, Ordering::Relaxed);
                drop(state);
            })),
        };
        let private = (&mut *subscription.cookie as *mut Cookie).cast();
        registry().subscriptions.insert(42, subscription);
        LIVE_SUBSCRIPTIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: this is the actual Rust trampoline with its owned live cookie;
        // the synthetic consumer performs no Redis FFI and needs no context.
        unsafe {
            changed(7, private);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert_eq!(COOKIE_DROPS.load(Ordering::Relaxed), 0);
        assert_eq!(registry().deferred.len(), 1);
        let nested_calls = calls.clone();
        let nested_drops = drops.clone();
        let observation = std::thread::spawn(move || {
            let nested = NativeOperation::begin();
            drop(nested);
            (
                nested_calls.load(Ordering::Relaxed),
                nested_drops.load(Ordering::Relaxed),
                COOKIE_DROPS.load(Ordering::Relaxed),
            )
        })
        .join()
        .unwrap();
        assert_eq!(observation, (0, 0, 0));
        assert_eq!(NATIVE_OPERATIONS.load(Ordering::Relaxed), 1);
        drop(outer);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(COOKIE_DROPS.load(Ordering::Relaxed), 1);
        assert_eq!(LIVE_SUBSCRIPTIONS.load(Ordering::Relaxed), 0);
        assert_eq!(NATIVE_OPERATIONS.load(Ordering::Relaxed), 0);
        let registry = registry();
        assert!(registry.subscriptions.is_empty());
        assert!(registry.deferred.is_empty());
        assert_eq!(registry.executing, 0);
    }
    #[test]
    fn last_native_user_removal_remains_owned_until_return() {
        let _serial = PROTOCOL_TEST.lock().unwrap();
        static FREES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn synthetic_free(_: *mut raw::RedisModuleUser) {
            FREES.fetch_add(1, Ordering::Relaxed);
        }
        let owner = Arc::new(());
        let handle = ModuleUserHandle {
            owner: owner.clone(),
            generation: 99,
        };
        {
            let mut registry = registry();
            assert!(!registry.has_auth_ownership());
            assert!(registry.phase == Phase::Unloaded);
            registry.phase = Phase::Active;
            registry.owner = Some(owner);
            registry.users.insert(
                99,
                NativeUser {
                    ptr: NonNull::dangling(),
                    free: synthetic_free,
                    name: c"synthetic-removal".into(),
                    leased: false,
                },
            );
        }
        // Exercise actual removal admission and unload's actual ownership
        // predicate, without Redis FFI or a hazardous native reentry sequence.
        let removal = UserRemoval::begin(&handle).unwrap();
        {
            let registry = registry();
            assert!(registry.users.is_empty());
            assert!(registry.user(&handle).is_err());
            assert!(registry.has_auth_ownership());
            assert!(registry.removing.contains(&99));
        }
        drop(removal);
        assert_eq!(FREES.load(Ordering::Relaxed), 0); // Drop performs no FFI.
        let mut registry = registry();
        assert!(!registry.has_auth_ownership());
        registry.phase = Phase::Unloaded;
        registry.owner = None;
    }
    #[test]
    fn native_call_without_auth_has_no_registry_access() {
        let _serial = PROTOCOL_TEST.lock().unwrap();
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
            closing: BTreeSet::new(),
            removing: BTreeSet::new(),
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
                leased: false,
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
