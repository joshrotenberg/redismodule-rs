//! Synchronous Redis authentication, separate from ordinary command contexts.
use std::cell::Cell;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{ClientChangeCallback, ClientId, ModuleUserHandle, NativeOperation, PendingCallback};
use crate::{RedisError, RegistrationContext, raw};

/// A synchronous auth function. Only an explicit [`AuthDecision::NotHandled`]
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

/// The only outcomes of a synchronous authentication callback.
pub enum AuthDecision {
    /// Request a trusted identity attachment. Native refusal denies the attempt.
    Accept(AuthTarget),
    /// Stop authentication with an optional explicit public message.
    Deny(PublicAuthError),
    /// Deliberately continue to another module hook or Redis password auth.
    NotHandled,
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
            callback: PendingCallback(None),
        }
    }
    /// Selects an existing ACL name for a connection with an already installed
    /// SDK identity. A fresh/untracked connection is denied before native lookup.
    /// This never creates or attaches an implicit bootstrap identity.
    pub fn acl_user(name: Vec<u8>) -> Self {
        Self {
            identity: Identity::AclUser(name),
            callback: PendingCallback(None),
        }
    }
    /// Installs owned `Send` cleanup, with [`ClientChangeCallback`]'s rules.
    pub fn on_changed(mut self, callback: ClientChangeCallback) -> Self {
        let previous = std::mem::replace(&mut self.callback, PendingCallback(Some(callback)));
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
        let AuthTarget { identity, callback } = target;
        match identity {
            Identity::ModuleUser(handle) => {
                let attach =
                    unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithUser) };
                let lease = super::UserLease::begin(&handle)?;
                super::attach_identity(self.client, callback, false, |changed, private, id| {
                    let status = unsafe { attach(self.ctx, lease.ptr, changed, private, id) };
                    if status == raw::REDISMODULE_OK as i32 {
                        committed.set(true);
                    }
                    status
                })?;
            }
            Identity::AclUser(name) => {
                let attach =
                    unsafe { crate::__available_api!(RedisModule_AuthenticateClientWithACLUser) };
                super::attach_identity(self.client, callback, true, |changed, private, id| {
                    let status = unsafe {
                        attach(
                            self.ctx,
                            name.as_ptr().cast(),
                            name.len(),
                            changed,
                            private,
                            id,
                        )
                    };
                    if status == raw::REDISMODULE_OK as i32 {
                        committed.set(true);
                    }
                    status
                })?;
            }
        }
        Ok(())
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
    let connected =
        !ctx.is_null() && !username.is_null() && unsafe { connected_client(ctx) }.is_ok();
    unsafe { denial_with_apis(ctx, username, err, public, &apis, connected) }
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
    connected: bool,
) -> i32 {
    if let Some(create) = apis.create {
        if let (Some(free), Some(audit)) = (apis.free, apis.audit) {
            if connected {
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
        let invocation = BoundaryLease::begin();
        assert!(super::super::registry().has_auth_ownership());
        drop(invocation);
        assert!(!super::super::registry().has_auth_ownership());
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
