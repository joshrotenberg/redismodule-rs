//! Redis authentication, separate from ordinary command contexts.
#[cfg(feature = "experimental-auth-redis-8-12-rc3")]
pub(super) mod asynchronous;
use std::cell::Cell;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::{ClientChangeCallback, ClientId, ModuleUserHandle, NativeOperation, PendingCallback};
use crate::{RedisError, RegistrationContext, raw};

/// An auth decision function. The function itself runs synchronously; only an
/// explicit [`AuthDecision::NotHandled`]
/// permits another module or Redis password authentication to handle the attempt.
///
/// ```compile_fail,E0308
/// use redis_module::{AuthCallback, AuthContext, AuthDecision, AuthRequest};
/// fn invalid(_: &AuthContext<'_>, _: AuthRequest<'static>) -> AuthDecision {
///     AuthDecision::NotHandled
/// }
/// let _: AuthCallback = invalid;
/// ```
pub type AuthCallback = for<'auth> fn(&AuthContext<'auth>, AuthRequest<'auth>) -> AuthDecision;

/// A borrowed authentication invocation with no ordinary client capabilities.
///
/// Implements [`crate::RedisLockIndicator`] for module-state lookup through
/// [`crate::RedisGILGuard`]. It cannot reply, call Redis commands, prepare users,
/// mutate identities or block a client. Prepare and drain identities explicitly
/// from ordinary registered commands before using this callback and unloading.
///
/// ```compile_fail,E0599
/// fn invalid(ctx: &redis_module::AuthContext<'_>) { ctx.reply(Ok("OK".into())); }
/// ```
/// ```compile_fail,E0599
/// fn invalid(ctx: &redis_module::AuthContext<'_>) { ctx.block_client(); }
/// ```
/// ```compile_fail,E0308
/// fn runtime(_: &redis_module::RuntimeContext) {}
/// fn invalid(ctx: &redis_module::AuthContext<'_>) { runtime(ctx); }
/// ```
/// ```compile_fail,E0599
/// fn invalid(ctx: &redis_module::AuthContext<'_>) { ctx.call("PING", &[] as &[&str]); }
/// ```
/// ```compile_fail,E0599
/// fn invalid(ctx: &redis_module::AuthContext<'_>) { ctx.create_module_user("hidden"); }
/// ```
/// ```compile_fail,E0277
/// fn invalid(ctx: &redis_module::AuthContext<'_>) {
///     std::thread::scope(|scope| { scope.spawn(|| ctx.client_id()); });
/// }
/// ```
pub struct AuthContext<'auth> {
    client: ClientId,
    invocation: PhantomData<&'auth Rc<()>>,
}
impl AuthContext<'_> {
    /// The current connection's ID. This does not keep the client alive.
    pub fn client_id(&self) -> ClientId {
        self.client
    }
}
// SAFETY: only the private synchronous native boundary constructs this view.
// Its borrow cannot outlive the locked invocation or cross threads.
unsafe impl crate::RedisLockIndicator for AuthContext<'_> {}

/// Credentials borrowed directly from Redis for one callback invocation.
/// No native string handle or ownership is exposed.
pub struct AuthRequest<'auth> {
    username: &'auth [u8],
    password: Password<'auth>,
}
impl<'auth> AuthRequest<'auth> {
    /// The supplied binary username, without UTF-8 conversion.
    pub fn username(&self) -> &'auth [u8] {
        self.username
    }
    /// The supplied password view. Reading its bytes is explicit.
    pub fn password(&self) -> &Password<'auth> {
        &self.password
    }
}

/// A non-owning password view. Formatting and cloning are deliberately absent.
/// Explicit byte access cannot prevent consumer logging or copying. The SDK
/// neither retains the native string nor makes an implicit secret copy.
///
/// ```compile_fail,E0277
/// fn invalid(password: &redis_module::Password<'_>) { println!("{password:?}"); }
/// ```
/// ```compile_fail,E0521
/// fn invalid(password: &redis_module::Password<'_>) {
///     let bytes = password.as_bytes();
///     std::thread::spawn(move || bytes.len());
/// }
/// ```
/// ```compile_fail,E0277
/// fn cloneable<T: Clone>() {}
/// cloneable::<redis_module::Password<'_>>();
/// ```
pub struct Password<'auth> {
    bytes: &'auth [u8],
    invocation: PhantomData<&'auth Rc<()>>,
}
impl Password<'_> {
    /// Borrows binary credential bytes. Do not log them or include them in
    /// public errors, panic messages or retained cleanup state.
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes
    }
}

/// Authentication outcomes. Deferred completion requires the explicit feature.
pub enum AuthDecision {
    /// Request a trusted identity attachment. Native refusal denies the attempt.
    Accept(AuthTarget),
    /// Stop authentication with an optional explicit public message.
    Deny(PublicAuthError),
    /// Deliberately continue to another module hook or Redis password auth.
    NotHandled,
    /// Explicit opt-in deferred authentication on a prepared, verified RC3 runtime.
    #[cfg(feature = "experimental-auth-redis-8-12-rc3")]
    Deferred(asynchronous::DeferredAuth),
}

enum Identity {
    ModuleUser(ModuleUserHandle),
    AclUser(Vec<u8>),
}
/// An owned identity selection and optional one-shot cleanup notification.
/// Dropping an uninstalled target contains cleanup-state destructor panics.
pub struct AuthTarget {
    identity: Identity,
    callback: PendingCallback,
}
impl AuthTarget {
    /// Selects an already configured module-owned user. Create/configure it
    /// explicitly from a post-load command, then keep its handle in module state.
    pub fn module_user(user: ModuleUserHandle) -> Self {
        Self {
            identity: Identity::ModuleUser(user),
            callback: PendingCallback::new(None),
        }
    }
    /// Selects an existing ACL name for a connection with an already installed
    /// SDK identity installed by an ordinary command. Native-auth history alone
    /// cannot certify that Redis still owns the callback. Other connections
    /// are denied before native lookup.
    /// This never creates or attaches an implicit bootstrap identity.
    pub fn acl_user(name: Vec<u8>) -> Self {
        Self {
            identity: Identity::AclUser(name),
            callback: PendingCallback::new(None),
        }
    }
    /// Installs owned `Send` cleanup, with [`ClientChangeCallback`]'s rules.
    pub fn on_changed(mut self, callback: ClientChangeCallback) -> Self {
        let previous = std::mem::replace(&mut self.callback, PendingCallback::new(Some(callback)));
        drop(previous);
        self
    }
}

/// An explicit, static message safe to disclose to the authenticating client.
/// Default uses Redis's standard WRONGPASS. No provider/error conversion exists.
#[derive(Default)]
pub struct PublicAuthError(Option<&'static str>);
impl PublicAuthError {
    /// Declares a public message. Redis's error code remains WRONGPASS; NUL,
    /// CR and LF are replaced with spaces. Never include credential material.
    pub const fn new(message: &'static str) -> Self {
        Self(Some(message))
    }
}
const FAILED: PublicAuthError = PublicAuthError::new("Module authentication failed");
static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
pub(super) fn in_flight() -> bool {
    INVOCATIONS.load(Ordering::Relaxed) != 0
}
struct BoundaryLease;
impl BoundaryLease {
    fn begin() -> Self {
        INVOCATIONS.fetch_add(1, Ordering::Relaxed);
        Self
    }
}
impl Drop for BoundaryLease {
    fn drop(&mut self) {
        INVOCATIONS.fetch_sub(1, Ordering::Relaxed);
    }
}

// This proof is private to the actual registered native-auth boundary. It does
// not authorize any ordinary Context mutation or expose a raw pointer publicly.
struct Invocation {
    ctx: *mut raw::RedisModuleCtx,
    client: ClientId,
}
impl Invocation {
    unsafe fn begin(ctx: *mut raw::RedisModuleCtx) -> Result<Self, RedisError> {
        super::main_thread()?;
        super::registry().active()?;
        if ctx.is_null()
            || INVOCATIONS.load(Ordering::Relaxed) != 1
            || super::NATIVE_OPERATIONS.load(Ordering::Relaxed) != 0
            || super::registry().executing != 0
        {
            return Err(RedisError::Str(
                "ERR authentication invocation is not quiescent",
            ));
        }
        let flags = unsafe { crate::__available_api!(RedisModule_GetContextFlags)(ctx) };
        if flags
            & (raw::REDISMODULE_CTX_FLAGS_LOADING | raw::REDISMODULE_CTX_FLAGS_ASYNC_LOADING) as i32
            != 0
        {
            return Err(RedisError::Str(
                "ERR authentication is refused while Redis is loading",
            ));
        }
        let client = unsafe { connected_client(ctx)? };
        Ok(Self { ctx, client })
    }
    fn attach(&self, target: AuthTarget, committed: &Cell<bool>) -> Result<(), RedisError> {
        self.attach_before(target, committed, || true)
    }
    fn attach_before(
        &self,
        target: AuthTarget,
        committed: &Cell<bool>,
        permitted: impl FnOnce() -> bool,
    ) -> Result<(), RedisError> {
        let AuthTarget { identity, callback } = target;
        match identity {
            Identity::ModuleUser(handle) => {
                let attach =
                    unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithUser) };
                let lease = super::UserLease::begin(&handle)?;
                super::attach_identity(
                    self.client,
                    callback,
                    false,
                    super::AttachmentOrigin::NativeAuth,
                    |changed, private, id| {
                        let status = before_auth_attach(permitted, || unsafe {
                            attach(self.ctx, lease.ptr, changed, private, id)
                        });
                        if status == raw::REDISMODULE_OK as i32 {
                            committed.set(true);
                        }
                        status
                    },
                )?;
            }
            Identity::AclUser(name) => {
                let attach =
                    unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithACLUser) };
                super::attach_identity(
                    self.client,
                    callback,
                    true,
                    super::AttachmentOrigin::NativeAuth,
                    |changed, private, id| {
                        let status = before_auth_attach(permitted, || unsafe {
                            attach(
                                self.ctx,
                                name.as_ptr().cast(),
                                name.len(),
                                changed,
                                private,
                                id,
                            )
                        });
                        if status == raw::REDISMODULE_OK as i32 {
                            committed.set(true);
                        }
                        status
                    },
                )?;
            }
        }
        Ok(())
    }
}
// The predicate runs after identity admission, leases and pending-cookie
// preparation, immediately before either native AuthenticateClient entry.
fn before_auth_attach(permitted: impl FnOnce() -> bool, native: impl FnOnce() -> i32) -> i32 {
    if permitted() {
        native()
    } else {
        raw::REDISMODULE_ERR as i32
    }
}
unsafe fn connected_client(ctx: *mut raw::RedisModuleCtx) -> Result<ClientId, RedisError> {
    let get_id = unsafe { crate::__available_api!(RedisModule_GetClientId) };
    let info = unsafe { crate::__available_api!(RedisModule_GetClientInfoById) };
    let id = unsafe { get_id(ctx) };
    if id == 0 || unsafe { info(ptr::null_mut(), id) } != raw::REDISMODULE_OK as i32 {
        return Err(RedisError::Str(
            "ERR authentication requires a connected client",
        ));
    }
    Ok(ClientId(id))
}

fn catch_auth<T>(f: impl FnOnce() -> T) -> Result<T, ()> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        // Do not inspect or drop a potentially secret-bearing panic payload.
        std::mem::forget(payload);
    })
}
fn panic_notice() {
    // A fixed diagnostic only. Rust's process panic hook has already run; the
    // SDK does not replace it or promise to prevent consumer-hook disclosure.
    unsafe {
        if let Some(log) = raw::RedisModule_Log {
            log(
                ptr::null_mut(),
                c"warning".as_ptr(),
                c"%s".as_ptr(),
                c"module authentication callback panicked".as_ptr(),
            );
        }
    }
}

/// Macro plumbing. A declared hook requires every companion capability.
#[doc(hidden)]
pub fn auth_callback_available() -> Result<(), RedisError> {
    unsafe {
        let _ = crate::__available_api!(RedisModule_RegisterAuthCallback);
        let _ = crate::__available_api!(RedisModule_StringPtrLen);
        let _ = crate::__available_api!(RedisModule_CreateString);
        let _ = crate::__available_api!(RedisModule_FreeString);
        let _ = crate::__available_api!(RedisModule_ACLAddLogEntryByUserName);
        let _ = crate::__available_api!(RedisModule_AuthenticateClientWithUser);
        let _ = crate::__available_api!(RedisModule_AuthenticateClientWithACLUser);
        let _ = crate::__available_api!(RedisModule_GetContextFlags);
        let _ = crate::__available_api!(RedisModule_GetClientId);
        let _ = crate::__available_api!(RedisModule_GetClientInfoById);
        let _ = crate::__available_api!(RedisModule_DeauthenticateAndCloseClient);
    }
    Ok(())
}

/// # Safety
/// `callback` must be the generated auth trampoline, registered only in OnLoad.
#[doc(hidden)]
pub unsafe fn register_auth_callback(
    ctx: &RegistrationContext<'_>,
    callback: raw::RedisModuleAuthCallback,
) -> Result<(), RedisError> {
    auth_callback_available()?;
    unsafe { raw::RedisModule_RegisterAuthCallback.unwrap()(ctx.get_raw(), callback) };
    Ok(())
}

unsafe fn bytes(_: &Invocation, string: *mut raw::RedisModuleString) -> Result<&[u8], RedisError> {
    let get = unsafe { crate::__available_api!(RedisModule_StringPtrLen) };
    if string.is_null() {
        return Err(RedisError::Str("ERR missing auth argument"));
    }
    let mut len = 0;
    let data = unsafe { get(string, &mut len) };
    if data.is_null() {
        return Err(RedisError::Str("ERR missing auth bytes"));
    }
    Ok(unsafe { std::slice::from_raw_parts(data.cast(), len) })
}

fn public_error_bytes(message: &str) -> Vec<u8> {
    let mut text = b"-WRONGPASS ".to_vec();
    text.extend(message.bytes().map(|b| {
        if matches!(b, 0 | b'\r' | b'\n') {
            b' '
        } else {
            b
        }
    }));
    text
}

// RM_Call runs foreign command filters before lookup. A filter can attempt
// nested AUTH, whose refusal must not recursively issue another CONFIG read.
static AUDIT_READ: AtomicBool = AtomicBool::new(false);
struct AuditRead;
impl AuditRead {
    fn begin() -> Option<Self> {
        AUDIT_READ
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .ok()?;
        Some(Self)
    }
}
impl Drop for AuditRead {
    fn drop(&mut self) {
        AUDIT_READ.store(false, Ordering::Relaxed);
    }
}
struct AuditReply {
    ptr: *mut raw::RedisModuleCallReply,
    free: unsafe extern "C" fn(*mut raw::RedisModuleCallReply),
}
impl Drop for AuditReply {
    fn drop(&mut self) {
        unsafe { (self.free)(self.ptr) };
    }
}
fn positive_retention(key: &[u8], value: &[u8]) -> bool {
    key == b"acllog-max-len"
        && !value.is_empty()
        && value
            .iter()
            .try_fold(0u64, |n, digit| {
                digit.is_ascii_digit().then_some(())?;
                n.checked_mul(10)?.checked_add((digit - b'0') as u64)
            })
            .is_some_and(|n| n > 0)
}
// Field order keeps recursion refusal active while deferred consumers drain.
struct AuditPermit {
    _operation: NativeOperation,
    _read: AuditRead,
}
// Only fixed public configuration is read; no credential is copied into a call.
// Unknown/disabled retention silently skips audit, while authentication denies.
unsafe fn audit_retention_enabled(ctx: *mut raw::RedisModuleCtx) -> Option<AuditPermit> {
    let _read = AuditRead::begin()?;
    // Declared after the read lease: deferred consumers also run under it.
    let operation = NativeOperation::begin();
    let (Some(call), Some(free), Some(kind), Some(length), Some(element), Some(string)) = (unsafe {
        (
            raw::RedisModule_Call,
            raw::RedisModule_FreeCallReply,
            raw::RedisModule_CallReplyType,
            raw::RedisModule_CallReplyLength,
            raw::RedisModule_CallReplyArrayElement,
            raw::RedisModule_CallReplyStringPtr,
        )
    }) else {
        return None;
    };
    // Default RESP2, no writes, no propagation or blocking. Native filters still
    // run. Their error/changed/malformed reply fails the strict parser below.
    let reply = unsafe {
        call(
            ctx,
            c"CONFIG".as_ptr(),
            c"ccW".as_ptr(),
            c"GET".as_ptr(),
            c"acllog-max-len".as_ptr(),
        )
    };
    let enabled = unsafe {
        parse_retention_reply(
            reply,
            AuditReplyApis {
                free,
                kind,
                length,
                element,
                string,
            },
        )
    };
    enabled.then_some(AuditPermit {
        _operation: operation,
        _read,
    })
}
struct AuditReplyApis {
    free: unsafe extern "C" fn(*mut raw::RedisModuleCallReply),
    kind: unsafe extern "C" fn(*mut raw::RedisModuleCallReply) -> i32,
    length: unsafe extern "C" fn(*mut raw::RedisModuleCallReply) -> usize,
    element: unsafe extern "C" fn(
        *mut raw::RedisModuleCallReply,
        usize,
    ) -> *mut raw::RedisModuleCallReply,
    string:
        unsafe extern "C" fn(*mut raw::RedisModuleCallReply, *mut usize) -> *const std::ffi::c_char,
}
// Own the root through every child access and free it once on every exit.
unsafe fn parse_retention_reply(
    reply: *mut raw::RedisModuleCallReply,
    apis: AuditReplyApis,
) -> bool {
    if reply.is_null() {
        return false;
    }
    let reply = AuditReply {
        ptr: reply,
        free: apis.free,
    };
    if unsafe { (apis.kind)(reply.ptr) } != raw::REDISMODULE_REPLY_ARRAY as i32
        || unsafe { (apis.length)(reply.ptr) } != 2
    {
        return false;
    }
    let owned = |index| -> Option<Vec<u8>> {
        let child = unsafe { (apis.element)(reply.ptr, index) };
        if child.is_null() || unsafe { (apis.kind)(child) } != raw::REDISMODULE_REPLY_STRING as i32
        {
            return None;
        }
        let mut len = 0;
        let bytes = unsafe { (apis.string)(child, &mut len) };
        if bytes.is_null() || len > 32 {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), len) }.to_vec())
    };
    let (Some(key), Some(value)) = (owned(0), owned(1)) else {
        return false;
    };
    positive_retention(&key, &value)
}

// All callers have Redis's actual native-auth context and live input strings.
// Audit is best effort and never changes a handled denial into fallback.
unsafe fn deny(
    ctx: *mut raw::RedisModuleCtx,
    username: *mut raw::RedisModuleString,
    err: *mut *mut raw::RedisModuleString,
    public: PublicAuthError,
) -> i32 {
    let apis = DenialApis {
        create: unsafe { raw::RedisModule_CreateString },
        free: unsafe { raw::RedisModule_FreeString },
        audit: unsafe { raw::RedisModule_ACLAddLogEntryByUserName },
    };
    // Keep the native/read leases through audit, error transfer and reply
    // release. No deferred consumer can change retention after verification.
    let permit =
        if !ctx.is_null() && !username.is_null() && unsafe { connected_client(ctx) }.is_ok() {
            unsafe { audit_retention_enabled(ctx) }
        } else {
            None
        };
    unsafe { denial_with_apis(ctx, username, err, public, &apis, permit.is_some()) }
}

type CreateString = unsafe extern "C" fn(
    *mut raw::RedisModuleCtx,
    *const std::ffi::c_char,
    usize,
) -> *mut raw::RedisModuleString;
type FreeString = unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleString);
type Audit = unsafe extern "C" fn(
    *mut raw::RedisModuleCtx,
    *mut raw::RedisModuleString,
    *mut raw::RedisModuleString,
    raw::RedisModuleACLLogEntryReason,
);
struct DenialApis {
    create: Option<CreateString>,
    free: Option<FreeString>,
    audit: Option<Audit>,
}

// The native-reference transfer protocol is separate so inert tests can verify
// it without mutating process-global API slots alongside other unit tests.
unsafe fn denial_with_apis(
    ctx: *mut raw::RedisModuleCtx,
    username: *mut raw::RedisModuleString,
    err: *mut *mut raw::RedisModuleString,
    public: PublicAuthError,
    apis: &DenialApis,
    audit_allowed: bool,
) -> i32 {
    if let Some(create) = apis.create {
        if let (Some(free), Some(audit)) = (apis.free, apis.audit) {
            if audit_allowed {
                let object = unsafe { create(ptr::null_mut(), c"module-auth".as_ptr(), 11) };
                if !object.is_null() {
                    unsafe {
                        audit(
                            ctx,
                            username,
                            object,
                            raw::RedisModuleACLLogEntryReason_REDISMODULE_ACL_LOG_AUTH,
                        );
                        free(ptr::null_mut(), object);
                    }
                }
            }
        }
        if !err.is_null() {
            if let Some(message) = public.0 {
                let text = public_error_bytes(message);
                // NULL producer: one manual reference transfers to AUTH/HELLO.
                unsafe {
                    *err = create(ptr::null_mut(), text.as_ptr().cast(), text.len());
                }
            }
        }
    }
    raw::REDISMODULE_AUTH_HANDLED as i32
}

/// Native auth adapter used only by `redis_module!`.
///
/// # Safety
/// Redis must supply this module's live registered auth invocation, borrowed
/// username/password and error slot. The Redis lock must be held throughout.
#[doc(hidden)]
pub unsafe fn run_auth_callback(
    ctx: *mut raw::RedisModuleCtx,
    username: *mut raw::RedisModuleString,
    password: *mut raw::RedisModuleString,
    err: *mut *mut raw::RedisModuleString,
    handler: AuthCallback,
) -> i32 {
    // Own the entire boundary, including admission refusal and panic recovery
    // before the first subscription exists. Drop performs no native operation.
    let _boundary = BoundaryLease::begin();
    if !err.is_null() {
        unsafe {
            *err = ptr::null_mut();
        }
    }
    let committed = Cell::new(false);
    let outcome = catch_auth(|| -> Result<i32, RedisError> {
        let invocation = unsafe { Invocation::begin(ctx)? };
        let _operation = NativeOperation::begin();
        let context = AuthContext {
            client: invocation.client,
            invocation: PhantomData,
        };
        let request = AuthRequest {
            username: unsafe { bytes(&invocation, username)? },
            password: Password {
                bytes: unsafe { bytes(&invocation, password)? },
                invocation: PhantomData,
            },
        };
        let decision = match catch_auth(|| handler(&context, request)) {
            Ok(decision) => decision,
            Err(()) => {
                panic_notice();
                AuthDecision::Deny(FAILED)
            }
        };
        Ok(match decision {
            #[cfg(feature = "experimental-auth-redis-8-12-rc3")]
            AuthDecision::Deferred(deferred) => unsafe {
                asynchronous::begin(&invocation, deferred, username, err)
            },
            AuthDecision::NotHandled => raw::REDISMODULE_AUTH_NOT_HANDLED as i32,
            AuthDecision::Deny(public) => unsafe { deny(ctx, username, err, public) },
            AuthDecision::Accept(target) => match invocation.attach(target, &committed) {
                Ok(()) => raw::REDISMODULE_AUTH_HANDLED as i32,
                Err(_) => unsafe { deny(ctx, username, err, FAILED) },
            },
        })
    });
    match outcome {
        Ok(Ok(status)) => status,
        Err(()) => {
            panic_notice();
            // Native AUTH_HAS_RESULT already means success after a committed
            // attachment. Never mislabel that state as a failed denial.
            if committed.get() {
                raw::REDISMODULE_AUTH_HANDLED as i32
            } else {
                unsafe { deny(ctx, username, err, FAILED) }
            }
        }
        Ok(Err(_)) => unsafe { deny(ctx, username, err, FAILED) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn callback_credentials_borrow_binary_storage() {
        let password = [0, 255, b'p'];
        let request = AuthRequest {
            username: b"\0user\xff",
            password: Password {
                bytes: &password,
                invocation: PhantomData,
            },
        };
        assert_eq!(request.username(), b"\0user\xff");
        assert_eq!(request.password().as_bytes().as_ptr(), password.as_ptr());
        assert_eq!(
            public_error_bytes("public\r\nmessage\0tail"),
            b"-WRONGPASS public  message tail"
        );
    }

    #[test]
    fn auth_panic_payload_destructor_is_not_executed() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        struct Payload;
        impl Drop for Payload {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }
        assert!(catch_auth(|| std::panic::panic_any(Payload)).is_err());
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn invocation_owns_unload_before_first_subscription() {
        let _serial = super::super::tests::PROTOCOL_TEST.lock().unwrap();
        assert!(!super::super::registry().has_auth_ownership());
        // Exercise the real boundary lease and unload ownership predicate,
        // without constructing a native invocation or running Redis code.
        {
            let mut registry = super::super::registry();
            registry.phase = super::super::Phase::Active;
            registry.owner = Some(std::sync::Arc::new(()));
        }
        unsafe extern "C" fn lookup(_: *mut std::ffi::c_void, _: u64) -> i32 {
            panic!("reconciliation must not reach lookup while auth is in flight");
        }
        let invocation = BoundaryLease::begin();
        assert!(super::super::registry().has_auth_ownership());
        assert!(super::super::retire_missing_identity(ClientId(1), Some(lookup)).is_err());
        drop(invocation);
        assert!(!super::super::retire_missing_identity(ClientId(1), Some(lookup)).unwrap());
        let mut registry = super::super::registry();
        assert!(!registry.has_auth_ownership());
        registry.phase = super::super::Phase::Unloaded;
        registry.owner = None;
    }

    #[test]
    fn denial_audit_requires_positive_retention_and_bounds_read_reentry() {
        for value in [b"1".as_slice(), b"128", b"0002"] {
            assert!(positive_retention(b"acllog-max-len", value));
        }
        for value in [
            b"".as_slice(),
            b"0",
            b"-1",
            b"+1",
            b" 1",
            b"1x",
            b"18446744073709551616",
        ] {
            assert!(!positive_retention(b"acllog-max-len", value));
        }
        assert!(!positive_retention(b"unknown", b"1"));
        let read = AuditRead::begin().unwrap();
        assert!(AuditRead::begin().is_none());
        // A nested AUTH refusal under a foreign filter cannot call Redis or
        // dereference this inert context while the outer read is in flight.
        assert!(unsafe { audit_retention_enabled(ptr::null_mut()) }.is_none());
        drop(read);
        assert!(AuditRead::begin().is_some());
    }

    #[test]
    fn retention_reply_owns_root_on_success_malformed_and_read_error() {
        enum Reply {
            Array(Vec<Reply>),
            Bytes(Vec<u8>),
            Error,
        }
        static FREES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn free(reply: *mut raw::RedisModuleCallReply) {
            FREES.fetch_add(1, Ordering::Relaxed);
            unsafe {
                drop(Box::from_raw(reply.cast::<Reply>()));
            }
        }
        unsafe extern "C" fn kind(reply: *mut raw::RedisModuleCallReply) -> i32 {
            match unsafe { &*reply.cast::<Reply>() } {
                Reply::Array(_) => raw::REDISMODULE_REPLY_ARRAY as i32,
                Reply::Bytes(_) => raw::REDISMODULE_REPLY_STRING as i32,
                Reply::Error => raw::REDISMODULE_REPLY_ERROR as i32,
            }
        }
        unsafe extern "C" fn length(reply: *mut raw::RedisModuleCallReply) -> usize {
            match unsafe { &*reply.cast::<Reply>() } {
                Reply::Array(v) => v.len(),
                _ => 0,
            }
        }
        unsafe extern "C" fn element(
            reply: *mut raw::RedisModuleCallReply,
            index: usize,
        ) -> *mut raw::RedisModuleCallReply {
            match unsafe { &mut *reply.cast::<Reply>() } {
                Reply::Array(v) => v
                    .get_mut(index)
                    .map_or(ptr::null_mut(), |child| (child as *mut Reply).cast()),
                _ => ptr::null_mut(),
            }
        }
        unsafe extern "C" fn string(
            reply: *mut raw::RedisModuleCallReply,
            len: *mut usize,
        ) -> *const std::ffi::c_char {
            match unsafe { &*reply.cast::<Reply>() } {
                Reply::Bytes(v) => {
                    unsafe {
                        *len = v.len();
                    }
                    v.as_ptr().cast()
                }
                _ => ptr::null(),
            }
        }
        let apis = || AuditReplyApis {
            free,
            kind,
            length,
            element,
            string,
        };
        assert!(!unsafe { parse_retention_reply(ptr::null_mut(), apis()) });
        assert_eq!(FREES.load(Ordering::Relaxed), 0);
        let cases = [
            (Reply::Error, false),
            (Reply::Array(vec![]), false),
            (
                Reply::Array(vec![Reply::Bytes(b"acllog-max-len".to_vec()), Reply::Error]),
                false,
            ),
            (
                Reply::Array(vec![
                    Reply::Bytes(b"unknown".to_vec()),
                    Reply::Bytes(b"1".to_vec()),
                ]),
                false,
            ),
            (
                Reply::Array(vec![
                    Reply::Bytes(b"acllog-max-len".to_vec()),
                    Reply::Bytes(b"0".to_vec()),
                ]),
                false,
            ),
            (
                Reply::Array(vec![
                    Reply::Bytes(b"acllog-max-len".to_vec()),
                    Reply::Bytes(vec![b'1'; 33]),
                ]),
                false,
            ),
            (
                Reply::Array(vec![
                    Reply::Bytes(b"acllog-max-len".to_vec()),
                    Reply::Bytes(b"128".to_vec()),
                ]),
                true,
            ),
        ];
        for (index, (reply, expected)) in cases.into_iter().enumerate() {
            let reply = Box::into_raw(Box::new(reply)).cast();
            assert_eq!(unsafe { parse_retention_reply(reply, apis()) }, expected);
            assert_eq!(FREES.load(Ordering::Relaxed), index + 1);
        }
    }

    static CREATED: AtomicUsize = AtomicUsize::new(0);
    static FREED: AtomicUsize = AtomicUsize::new(0);
    static AUDITS: AtomicUsize = AtomicUsize::new(0);
    static VALID: AtomicBool = AtomicBool::new(true);
    unsafe extern "C" fn create(
        ctx: *mut raw::RedisModuleCtx,
        text: *const std::ffi::c_char,
        len: usize,
    ) -> *mut raw::RedisModuleString {
        if !ctx.is_null() {
            VALID.store(false, Ordering::Relaxed);
        }
        CREATED.fetch_add(1, Ordering::Relaxed);
        let value = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), len) }.to_vec();
        Box::into_raw(Box::new(value)).cast()
    }
    unsafe extern "C" fn create_failure(
        _: *mut raw::RedisModuleCtx,
        _: *const std::ffi::c_char,
        _: usize,
    ) -> *mut raw::RedisModuleString {
        ptr::null_mut()
    }
    unsafe extern "C" fn free(ctx: *mut raw::RedisModuleCtx, value: *mut raw::RedisModuleString) {
        if !ctx.is_null() {
            VALID.store(false, Ordering::Relaxed);
        }
        FREED.fetch_add(1, Ordering::Relaxed);
        unsafe {
            drop(Box::from_raw(value.cast::<Vec<u8>>()));
        }
    }
    unsafe extern "C" fn audit(
        _: *mut raw::RedisModuleCtx,
        _: *mut raw::RedisModuleString,
        object: *mut raw::RedisModuleString,
        reason: raw::RedisModuleACLLogEntryReason,
    ) {
        AUDITS.fetch_add(1, Ordering::Relaxed);
        if unsafe { &*object.cast::<Vec<u8>>() }.as_slice() != b"module-auth"
            || reason != raw::RedisModuleACLLogEntryReason_REDISMODULE_ACL_LOG_AUTH
        {
            VALID.store(false, Ordering::Relaxed);
        }
    }
    #[test]
    fn denial_transfers_one_manual_error_and_releases_only_audit_object() {
        CREATED.store(0, Ordering::Relaxed);
        FREED.store(0, Ordering::Relaxed);
        AUDITS.store(0, Ordering::Relaxed);
        VALID.store(true, Ordering::Relaxed);
        let mut apis = DenialApis {
            create: Some(create),
            free: Some(free),
            audit: Some(audit),
        };
        let ctx = ptr::NonNull::<u8>::dangling().as_ptr().cast();
        let username = ptr::NonNull::<u8>::dangling().as_ptr().cast();
        let mut error = ptr::null_mut();
        // Inert FFI table: no actual Redis context or native mutation is used.
        let status = unsafe {
            denial_with_apis(
                ctx,
                username,
                &mut error,
                PublicAuthError::new("public\r\nmessage\0tail"),
                &apis,
                true,
            )
        };
        assert_eq!(status, raw::REDISMODULE_AUTH_HANDLED as i32);
        assert_eq!(CREATED.load(Ordering::Relaxed), 2);
        assert_eq!(FREED.load(Ordering::Relaxed), 1);
        assert_eq!(AUDITS.load(Ordering::Relaxed), 1);
        assert!(VALID.load(Ordering::Relaxed));
        assert_eq!(
            unsafe { &*error.cast::<Vec<u8>>() }.as_slice(),
            b"-WRONGPASS public  message tail"
        );
        // Simulate Redis consuming its transferred reference after AUTH/HELLO.
        unsafe {
            free(ptr::null_mut(), error);
        }
        assert_eq!(FREED.load(Ordering::Relaxed), 2);
        error = ptr::null_mut();
        // Disabled, unknown or failed retention reads never allocate an audit
        // object. Public error transfer remains independent of audit eligibility.
        unsafe {
            denial_with_apis(ctx, username, &mut error, FAILED, &apis, false);
        }
        assert_eq!(CREATED.load(Ordering::Relaxed), 3);
        assert_eq!(AUDITS.load(Ordering::Relaxed), 1);
        unsafe {
            free(ptr::null_mut(), error);
        }
        apis.create = Some(create_failure);
        error = ptr::null_mut();
        assert_eq!(
            unsafe { denial_with_apis(ctx, username, &mut error, FAILED, &apis, true) },
            raw::REDISMODULE_AUTH_HANDLED as i32
        );
        assert!(error.is_null());
        assert_eq!(AUDITS.load(Ordering::Relaxed), 1);
        apis.audit = None;
        assert_eq!(
            unsafe { denial_with_apis(ctx, username, &mut error, FAILED, &apis, true) },
            raw::REDISMODULE_AUTH_HANDLED as i32
        );
    }
}
