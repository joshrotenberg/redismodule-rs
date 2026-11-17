//! Auth-only completion. Native reachability and producer ownership are separate.
use std::any::Any;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::{AuthRequest, AuthTarget, Invocation, PublicAuthError};
use crate::context::auth as users;
use crate::{Context, RedisError, raw};

mod runtime;
#[cfg(test)]
mod tests;

const FAILED: RedisError = RedisError::Str("ERR asynchronous authentication is unavailable");
static OWNED: AtomicUsize = AtomicUsize::new(0);
static SUBMISSIONS: AtomicUsize = AtomicUsize::new(0);
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static REAPERS: AtomicUsize = AtomicUsize::new(0);

/// An explicit credential copy for owned worker input. Formatting is unavailable.
/// Dropping it overwrites its password storage before releasing that allocation.
/// The SDK cannot erase copies made by application code or the allocator.
///
/// ```compile_fail,E0277
/// fn invalid(secret: &redis_module::OwnedAuthCredentials) { println!("{secret:?}"); }
/// ```
/// ```compile_fail,E0277
/// fn cloneable<T: Clone>() {}
/// cloneable::<redis_module::OwnedAuthCredentials>();
/// ```
pub struct OwnedAuthCredentials {
    username: Vec<u8>,
    password: Vec<u8>,
}
impl OwnedAuthCredentials {
    pub fn username(&self) -> &[u8] {
        &self.username
    }
    pub fn password(&self) -> &[u8] {
        &self.password
    }
}
impl Drop for OwnedAuthCredentials {
    fn drop(&mut self) {
        for byte in &mut self.password {
            // SAFETY: these bytes are exclusively owned until allocation release.
            unsafe { ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}
impl AuthRequest<'_> {
    /// Explicitly copies both binary fields for a deferred owned computation.
    pub fn copy_for_async(&self) -> OwnedAuthCredentials {
        OwnedAuthCredentials {
            username: self.username.to_vec(),
            password: self.password.bytes.to_vec(),
        }
    }
}

impl super::AuthContext<'_> {
    /// Creates an explicitly deferred decision for this live auth callback.
    /// The owned decision remains unload-tracked if retained instead of returned.
    pub fn defer(
        &self,
        timeout: Duration,
        starter: impl FnOnce(AuthCompletion) + Send + 'static,
    ) -> super::AuthDecision {
        super::AuthDecision::Deferred(DeferredAuth::new(timeout, starter))
    }
}

type Starter = Box<dyn FnOnce(AuthCompletion) + Send + 'static>;

/// Explicit opt-in deferred decision. Requires the experimental feature and
/// ordinary-command preparation of the exact approved Linux amd64 executable.
///
/// The timeout begins when the initial callback returns this decision. Earlier
/// handler/preparation work and event-loop delays have no elapsed-time bound.
/// A starter runs once after confirmed blocking and receives no Redis context.
/// It must return promptly and arrange its own owned computation. Provider or
/// executor setup is outside this API. An abandoned starter is retained until
/// ordinary-command cleanup; intentional starter invocation runs application code.
pub struct DeferredAuth {
    timeout: Duration,
    starter: Option<Starter>,
}
impl DeferredAuth {
    fn new(timeout: Duration, starter: impl FnOnce(AuthCompletion) + Send + 'static) -> Self {
        OWNED.fetch_add(1, Ordering::Relaxed);
        Self {
            timeout,
            starter: Some(Box::new(starter)),
        }
    }
}
impl Drop for DeferredAuth {
    fn drop(&mut self) {
        if let Some(starter) = self.starter.take() {
            retire(starter);
        }
        OWNED.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A one-use, owned completion token. It may cross threads; it cannot reply,
/// call commands, reblock, or expose a native handle. Drop selects denial if no
/// other event won. Late results retain cleanup until explicit ordinary reaping.
///
/// ```compile_fail,E0599
/// fn invalid(completion: redis_module::AuthCompletion) { completion.block_client(); }
/// ```
/// ```compile_fail,E0277
/// fn cloneable<T: Clone>() {}
/// cloneable::<redis_module::AuthCompletion>();
/// ```
pub struct AuthCompletion {
    job: Option<Arc<Job>>,
}
impl AuthCompletion {
    pub fn accept(mut self, mut target: AuthTarget) {
        target.callback.ordinary_async = true;
        self.finish(Outcome::Accept(target));
    }
    pub fn deny(mut self, public: PublicAuthError) {
        self.finish(Outcome::Deny(public));
    }
    pub fn not_handled(mut self) {
        self.finish(Outcome::NotHandled);
    }
    fn finish(&mut self, outcome: Outcome) {
        if let Some(job) = self.job.take() {
            choose(&job, outcome);
            // Keep the registry's strong owner until this producer's final Arc
            // is released. Otherwise a concurrent ordinary reaper could leave
            // the worker as last owner of arbitrary retained payloads.
            let state = registry();
            job.inner().producers -= 1;
            drop(job);
            drop(state);
        } else {
            retire(outcome);
        }
    }
}
impl Drop for AuthCompletion {
    fn drop(&mut self) {
        if self.job.is_some() {
            self.finish(Outcome::Deny(PublicAuthError::default()));
        }
    }
}

/// Informational result of preparation. It is not a caller-supplied capability.
#[derive(Clone, Copy, Debug)]
pub struct PreparedAuthRuntime {
    pub generation: u64,
}

enum Outcome {
    Accept(AuthTarget),
    Deny(PublicAuthError),
    NotHandled,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NativeState {
    Preparing,
    Blocked,
    Queued,
    Resuming,
    Freed,
    Unknown,
}
fn record_block_result(job: &Job, handle: usize, proof: bool) -> NativeState {
    let mut inner = job.inner();
    inner.handle = handle;
    if handle == 0 && proof {
        inner.state = NativeState::Preparing;
        inner.native_live = false;
    } else {
        inner.state = if handle != 0 && proof {
            NativeState::Blocked
        } else {
            NativeState::Unknown
        };
        inner.native_live = true;
    }
    inner.state
}
struct Cookie {
    key: u64,
}
struct Inner {
    state: NativeState,
    handle: usize,
    winner: bool,
    outcome: Option<Outcome>,
    starter: Option<Starter>,
    native_live: bool,
    submitting: usize,
    producers: usize,
    timer: Option<u64>,
    timer_unknown: bool,
}
struct Job {
    client: users::ClientId,
    owner: Arc<()>,
    receipt_generation: u64,
    deadline: u64,
    cookie: Box<Cookie>,
    api: Api,
    inner: Mutex<Inner>,
}
impl Job {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn private(&self) -> *mut c_void {
        (&*self.cookie as *const Cookie).cast_mut().cast()
    }
}

type Predicate = unsafe extern "C" fn(*mut raw::RedisModuleCtx) -> i32;
type Unblock = unsafe extern "C" fn(*mut raw::RedisModuleBlockedClient, *mut c_void) -> i32;
#[derive(Clone, Copy)]
struct Api {
    clock: unsafe extern "C" fn() -> u64,
    block: unsafe extern "C" fn(
        *mut raw::RedisModuleCtx,
        raw::RedisModuleAuthCallback,
        Option<unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut c_void)>,
    ) -> *mut raw::RedisModuleBlockedClient,
    set_private: unsafe extern "C" fn(*mut raw::RedisModuleBlockedClient, *mut c_void),
    disconnect:
        unsafe extern "C" fn(*mut raw::RedisModuleBlockedClient, raw::RedisModuleDisconnectFunc),
    unblock: Unblock,
    create_timer: unsafe extern "C" fn(
        *mut raw::RedisModuleCtx,
        i64,
        raw::RedisModuleTimerProc,
        *mut c_void,
    ) -> u64,
    stop_timer: unsafe extern "C" fn(*mut raw::RedisModuleCtx, u64, *mut *mut c_void) -> i32,
    reply: Predicate,
    timeout: Predicate,
    disconnected: Predicate,
    get_handle:
        unsafe extern "C" fn(*mut raw::RedisModuleCtx) -> *mut raw::RedisModuleBlockedClient,
    get_private: unsafe extern "C" fn(*mut raw::RedisModuleCtx) -> *mut c_void,
    info: runtime::ClientInfo,
    flags: unsafe extern "C" fn(*mut raw::RedisModuleCtx) -> i32,
}
impl Api {
    unsafe fn resolve() -> Result<Self, RedisError> {
        Ok(Self {
            clock: unsafe { raw::RedisModule_MonotonicMicroseconds }.ok_or(FAILED)?,
            block: unsafe { raw::RedisModule_BlockClientOnAuth }.ok_or(FAILED)?,
            set_private: unsafe { raw::RedisModule_BlockClientSetPrivateData }.ok_or(FAILED)?,
            disconnect: unsafe { raw::RedisModule_SetDisconnectCallback }.ok_or(FAILED)?,
            unblock: unsafe { raw::RedisModule_UnblockClient }.ok_or(FAILED)?,
            create_timer: unsafe { raw::RedisModule_CreateTimer }.ok_or(FAILED)?,
            stop_timer: unsafe { raw::RedisModule_StopTimer }.ok_or(FAILED)?,
            reply: unsafe { raw::RedisModule_IsBlockedReplyRequest }.ok_or(FAILED)?,
            timeout: unsafe { raw::RedisModule_IsBlockedTimeoutRequest }.ok_or(FAILED)?,
            disconnected: unsafe { raw::RedisModule_BlockedClientDisconnected }.ok_or(FAILED)?,
            get_handle: unsafe { raw::RedisModule_GetBlockedClientHandle }.ok_or(FAILED)?,
            get_private: unsafe { raw::RedisModule_GetBlockedClientPrivateData }.ok_or(FAILED)?,
            info: unsafe { raw::RedisModule_GetClientInfoById }.ok_or(FAILED)?,
            flags: unsafe { raw::RedisModule_GetContextFlags }.ok_or(FAILED)?,
        })
    }
}

enum Cleanup {
    Payload(Box<dyn Any + Send>),
    Subscription(users::ClientId, users::Subscription, bool),
    Job(Arc<Job>),
}
struct Registry {
    next: u64,
    jobs: BTreeMap<u64, Arc<Job>>,
    cleanup: VecDeque<Cleanup>,
    receipt: Option<Arc<runtime::Receipt>>,
    executing: bool,
}
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    next: 0,
    jobs: BTreeMap::new(),
    cleanup: VecDeque::new(),
    receipt: None,
    executing: false,
});
fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}
pub(in crate::context::auth) fn has_ownership() -> bool {
    OWNED.load(Ordering::Acquire) != 0
        || CALLBACKS.load(Ordering::Acquire) != 0
        || SUBMISSIONS.load(Ordering::Acquire) != 0
        || REAPERS.load(Ordering::Acquire) != 0
}
pub(in crate::context::auth) fn retire<T: Any + Send>(value: T) {
    OWNED.fetch_add(1, Ordering::Relaxed);
    registry()
        .cleanup
        .push_back(Cleanup::Payload(Box::new(value)));
}
pub(in crate::context::auth) fn retire_subscription(
    client: users::ClientId,
    subscription: users::Subscription,
    notify: bool,
) {
    OWNED.fetch_add(1, Ordering::Relaxed);
    registry()
        .cleanup
        .push_back(Cleanup::Subscription(client, subscription, notify));
}
pub(in crate::context::auth) fn unloaded() {
    registry().receipt = None;
}

fn choose(job: &Arc<Job>, outcome: Outcome) {
    let mut outcome = Some(outcome);
    let submit = {
        let mut inner = job.inner();
        if !inner.winner && inner.state == NativeState::Blocked {
            inner.winner = true;
            inner.outcome = outcome.take();
            inner.state = NativeState::Queued;
            inner.submitting += 1;
            SUBMISSIONS.fetch_add(1, Ordering::Relaxed);
            Some(inner.handle)
        } else {
            None
        }
    };
    if let Some(outcome) = outcome {
        retire(outcome);
    }
    if let Some(handle) = submit {
        // Transfer once. A native finalizer may run before this call returns.
        // No access to handle/cookie after the FFI. The job map and submit lease
        // retain Rust state independently of native-cookie ownership.
        let status = unsafe { (job.api.unblock)(handle as *mut _, job.private()) };
        let mut inner = job.inner();
        if status != raw::REDISMODULE_OK as i32 {
            inner.state = NativeState::Unknown;
        }
        inner.submitting -= 1;
        SUBMISSIONS.fetch_sub(1, Ordering::Relaxed);
    }
}

struct CallbackLease;
impl CallbackLease {
    fn begin() -> Self {
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
        Self
    }
}
impl Drop for CallbackLease {
    fn drop(&mut self) {
        CALLBACKS.fetch_sub(1, Ordering::Relaxed);
    }
}
unsafe fn from_cookie(private: *mut c_void) -> Option<Arc<Job>> {
    if private.is_null() {
        return None;
    }
    // The registry retains the cookie while any native/timer reachability exists.
    let key = unsafe { (*private.cast::<Cookie>()).key };
    registry().jobs.get(&key).cloned()
}
unsafe extern "C" fn freed(_: *mut raw::RedisModuleCtx, private: *mut c_void) {
    users::contain(|| {
        let _lease = CallbackLease::begin();
        if let Some(job) = unsafe { from_cookie(private) } {
            let mut inner = job.inner();
            inner.native_live = false;
            if inner.state != NativeState::Unknown {
                inner.state = NativeState::Freed;
            }
        }
    });
}
unsafe extern "C" fn disconnected(
    _: *mut raw::RedisModuleCtx,
    handle: *mut raw::RedisModuleBlockedClient,
) {
    users::contain(|| {
        let _lease = CallbackLease::begin();
        // Locate by retained opaque value, without re-reading native privdata.
        let job = {
            registry()
                .jobs
                .values()
                .find(|j| {
                    let inner = j.inner();
                    inner.handle == handle as usize
                        && inner.native_live
                        && inner.state == NativeState::Blocked
                })
                .cloned()
        };
        if let Some(job) = job {
            choose(&job, Outcome::Deny(PublicAuthError::default()));
        }
    });
}
unsafe extern "C" fn deadline(_: *mut raw::RedisModuleCtx, private: *mut c_void) {
    users::contain(|| {
        let _lease = CallbackLease::begin();
        if let Some(job) = unsafe { from_cookie(private) } {
            {
                let mut inner = job.inner();
                inner.timer = None;
                inner.timer_unknown = false;
            }
            choose(&job, Outcome::Deny(PublicAuthError::default()));
        }
        // Native owns removal/free of the currently firing timer after return.
    });
}

unsafe extern "C" fn resumed(
    ctx: *mut raw::RedisModuleCtx,
    username: *mut raw::RedisModuleString,
    _: *mut raw::RedisModuleString,
    err: *mut *mut raw::RedisModuleString,
) -> i32 {
    let _lease = CallbackLease::begin();
    if !err.is_null() {
        unsafe {
            *err = ptr::null_mut();
        }
    }
    let committed = std::cell::Cell::new(false);
    let result = super::catch_auth(|| -> Result<i32, RedisError> {
        users::main_thread()?;
        let api = unsafe { Api::resolve()? };
        if ctx.is_null()
            || unsafe { (api.reply)(ctx) } == 0
            || unsafe { (api.timeout)(ctx) } != 0
            || unsafe { (api.disconnected)(ctx) } != 0
        {
            return Err(FAILED);
        }
        let private = unsafe { (api.get_private)(ctx) };
        let job = unsafe { from_cookie(private) }.ok_or(FAILED)?;
        users::registry().active()?;
        if unsafe { super::connected_client(ctx)? } != job.client {
            return Err(FAILED);
        }
        let owner = users::registry().owner.clone().ok_or(FAILED)?;
        let handle = unsafe { (api.get_handle)(ctx) };
        let outcome = {
            let mut inner = job.inner();
            if inner.state != NativeState::Queued
                || inner.handle != handle as usize
                || !inner.native_live
                || !Arc::ptr_eq(&owner, &job.owner)
            {
                return Err(FAILED);
            }
            inner.state = NativeState::Resuming;
            inner.outcome.take().ok_or(FAILED)?
        };
        let _operation = users::NativeOperation::begin();
        let active_receipt = registry().receipt.as_ref().map(|r| r.generation);
        let expired = unsafe { (api.clock)() } >= job.deadline
            || active_receipt != Some(job.receipt_generation);
        if expired {
            retire(outcome);
            return Ok(unsafe { super::deny(ctx, username, err, PublicAuthError::default()) });
        }
        Ok(match outcome {
            Outcome::NotHandled => raw::REDISMODULE_AUTH_NOT_HANDLED as i32,
            Outcome::Deny(public) => unsafe { super::deny(ctx, username, err, public) },
            Outcome::Accept(mut target) => {
                target.callback.ordinary_async = true;
                let invocation = Invocation {
                    ctx,
                    client: job.client,
                };
                match invocation.attach(target, &committed) {
                    Ok(()) => raw::REDISMODULE_AUTH_HANDLED as i32,
                    Err(_) => unsafe {
                        super::deny(ctx, username, err, PublicAuthError::default())
                    },
                }
            }
        })
    });
    match result {
        Ok(Ok(status)) => status,
        _ if committed.get() => raw::REDISMODULE_AUTH_HANDLED as i32,
        _ => unsafe { super::deny(ctx, username, err, PublicAuthError::default()) },
    }
}

fn deadline_at(now: u64, duration: Duration) -> Result<(u64, i64), RedisError> {
    let millis = u64::try_from(duration.as_millis()).map_err(|_| FAILED)?;
    if millis == 0 || millis > 86_400_000 {
        return Err(FAILED);
    }
    let micros = millis.checked_mul(1000).ok_or(FAILED)?;
    Ok((now.checked_add(micros).ok_or(FAILED)?, millis as i64))
}

unsafe fn initial_role(invocation: &Invocation, api: Api) -> Result<(), RedisError> {
    let ctx = invocation.ctx;
    if ctx.is_null() {
        return Err(FAILED);
    }
    let forbidden = raw::REDISMODULE_CTX_FLAGS_LUA
        | raw::REDISMODULE_CTX_FLAGS_MULTI
        | raw::REDISMODULE_CTX_FLAGS_DENY_BLOCKING
        | raw::REDISMODULE_CTX_FLAGS_LOADING
        | raw::REDISMODULE_CTX_FLAGS_ASYNC_LOADING;
    if unsafe { (api.reply)(ctx) } != 0
        || unsafe { (api.timeout)(ctx) } != 0
        || unsafe { (api.disconnected)(ctx) } != 0
        || !unsafe { (api.get_handle)(ctx) }.is_null()
        || unsafe { (api.flags)(ctx) } & forbidden as i32 != 0
        || users::NATIVE_OPERATIONS.load(Ordering::Relaxed) != 1
        || super::INVOCATIONS.load(Ordering::Relaxed) != 1
        || SUBMISSIONS.load(Ordering::Relaxed) != 0
        || CALLBACKS.load(Ordering::Relaxed) != 0
    {
        return Err(FAILED);
    }
    unsafe {
        runtime::blocked(api.info, ctx, invocation.client.0, false)?;
    }
    Ok(())
}

pub(super) unsafe fn begin(
    invocation: &Invocation,
    mut deferred: DeferredAuth,
    username: *mut raw::RedisModuleString,
    err: *mut *mut raw::RedisModuleString,
) -> i32 {
    let committed_job = std::cell::RefCell::new(None::<Arc<Job>>);
    let result = super::catch_auth(|| -> Result<i32, RedisError> {
        let clock = unsafe { raw::RedisModule_MonotonicMicroseconds }.ok_or(FAILED)?;
        let now = unsafe { clock() };
        let (expiry, _) = deadline_at(now, deferred.timeout)?;
        let api = unsafe { Api::resolve()? };
        let owner = users::registry().owner.clone().ok_or(FAILED)?;
        let receipt = registry().receipt.clone().ok_or(FAILED)?;
        if unsafe { receipt.consistent(&owner) }.is_err() {
            let old = { registry().receipt.take() };
            if let Some(old) = old {
                retire(old);
            }
            return Err(FAILED);
        }
        let ctx = invocation.ctx;
        unsafe {
            initial_role(invocation, api)?;
        }
        if unsafe { (api.clock)() } >= expiry {
            return Err(FAILED);
        }
        let job = {
            let mut state = registry();
            state.next = state.next.checked_add(1).ok_or(FAILED)?;
            let key = state.next;
            let job = Arc::new(Job {
                client: invocation.client,
                owner,
                receipt_generation: receipt.generation,
                deadline: expiry,
                cookie: Box::new(Cookie { key }),
                api,
                inner: Mutex::new(Inner {
                    state: NativeState::Preparing,
                    handle: 0,
                    winner: false,
                    outcome: None,
                    starter: deferred.starter.take(),
                    native_live: false,
                    submitting: 0,
                    producers: 0,
                    timer: None,
                    timer_unknown: false,
                }),
            });
            OWNED.fetch_add(1, Ordering::Relaxed);
            state.jobs.insert(key, job.clone());
            job
        };
        if unsafe { (api.clock)() } >= expiry {
            return Err(FAILED);
        }
        let handle = unsafe { (api.block)(ctx, Some(resumed), Some(freed)) };
        let proof =
            unsafe { runtime::blocked(api.info, ctx, invocation.client.0, !handle.is_null()) };
        match record_block_result(&job, handle as usize, proof.is_ok()) {
            NativeState::Preparing => return Err(FAILED),
            NativeState::Unknown => return Ok(raw::REDISMODULE_AUTH_HANDLED as i32),
            _ => {}
        }
        unsafe {
            (api.set_private)(handle, job.private());
            (api.disconnect)(handle, Some(disconnected));
        }
        *committed_job.borrow_mut() = Some(job.clone());
        let timer_now = unsafe { (api.clock)() };
        if timer_now >= expiry {
            choose(&job, Outcome::Deny(PublicAuthError::default()));
        } else {
            let period = (expiry - timer_now).div_ceil(1000);
            // Leave a checked margin for the native timer's monotonic read.
            if timer_now.checked_add(period * 1000).is_none() {
                choose(&job, Outcome::Deny(PublicAuthError::default()));
                return Ok(raw::REDISMODULE_AUTH_HANDLED as i32);
            }
            // Timer IDs are opaque keys; zero is not an error sentinel.
            let timer =
                unsafe { (api.create_timer)(ctx, period as i64, Some(deadline), job.private()) };
            job.inner().timer = Some(timer);
            if unsafe { (api.clock)() } >= expiry {
                choose(&job, Outcome::Deny(PublicAuthError::default()));
                return Ok(raw::REDISMODULE_AUTH_HANDLED as i32);
            }
            let starter = {
                let mut inner = job.inner();
                inner.producers += 1;
                inner.starter.take()
            };
            let completion = AuthCompletion {
                job: Some(job.clone()),
            };
            if let Some(starter) = starter {
                if let Err(payload) = catch_unwind(AssertUnwindSafe(|| starter(completion))) {
                    std::mem::forget(payload);
                    choose(&job, Outcome::Deny(PublicAuthError::default()));
                }
            } else {
                drop(completion);
            }
        }
        Ok(raw::REDISMODULE_AUTH_HANDLED as i32)
    });
    match result {
        Ok(Ok(status)) => status,
        _ => {
            if let Some(job) = committed_job.into_inner() {
                choose(&job, Outcome::Deny(PublicAuthError::default()));
                raw::REDISMODULE_AUTH_HANDLED as i32
            } else {
                unsafe { super::deny(invocation.ctx, username, err, PublicAuthError::default()) }
            }
        }
    }
}

fn ordinary(ctx: &Context) -> Result<(), RedisError> {
    users::command_client(ctx)?;
    if super::in_flight()
        || SUBMISSIONS.load(Ordering::Relaxed) != 0
        || CALLBACKS.load(Ordering::Relaxed) != 0
        || registry().executing
    {
        return Err(FAILED);
    }
    Ok(())
}
struct Reaping;
impl Reaping {
    fn begin() -> Self {
        registry().executing = true;
        REAPERS.fetch_add(1, Ordering::Release);
        Self
    }
}
impl Drop for Reaping {
    fn drop(&mut self) {
        registry().executing = false;
        REAPERS.fetch_sub(1, Ordering::Release);
    }
}
impl Context {
    /// Hashes the actual running executable during an explicit ordinary command.
    /// This synchronous I/O has no wall-time guarantee and never runs on AUTH.
    /// Requires a reviewed exact Linux amd64 manifest; an empty manifest refuses.
    /// After preparation the trusted host must keep running image bytes immutable.
    ///
    /// ```compile_fail,E0599
    /// fn invalid(ctx: &redis_module::EventContext) { ctx.prepare_async_auth_runtime(); }
    /// ```
    pub fn prepare_async_auth_runtime(&self) -> Result<PreparedAuthRuntime, RedisError> {
        ordinary(self)?;
        if has_ownership()
            || users::registry()
                .subscriptions
                .values()
                .any(|s| s.ordinary_async)
        {
            return Err(FAILED);
        }
        let owner = users::registry().owner.clone().ok_or(FAILED)?;
        let generation = {
            let mut state = registry();
            state.receipt = None;
            state.next = state.next.checked_add(1).ok_or(FAILED)?;
            state.next
        };
        let _operation = users::NativeOperation::begin();
        let receipt = unsafe { runtime::prepare(owner, generation)? };
        registry().receipt = Some(Arc::new(receipt));
        Ok(PreparedAuthRuntime { generation })
    }
    /// Closes new deferred admission. Existing jobs must still finalize/drain.
    pub fn invalidate_async_auth_runtime(&self) -> Result<(), RedisError> {
        ordinary(self)?;
        registry().receipt = None;
        Ok(())
    }
    /// Stops new admission and selects one handled denial for each outstanding
    /// confirmed block. Producers remain owned until their tokens are released;
    /// this does not join workers, free native handles or run retained cleanup.
    pub fn cancel_async_auth(&self) -> Result<(), RedisError> {
        ordinary(self)?;
        registry().receipt = None;
        let jobs: Vec<_> = registry().jobs.values().cloned().collect();
        for job in jobs {
            if job.inner().state == NativeState::Blocked {
                choose(&job, Outcome::Deny(PublicAuthError::default()));
            }
        }
        Ok(())
    }
    /// Runs at most `limit` retained async cleanups from a quiescent ordinary
    /// registered command. Native/free/timer/worker callbacks never drain them.
    /// Captures may reenter; the execution lease keeps unload/nested drain refused.
    /// Returns the number processed. Explicit identity drain and user removal
    /// remain necessary before unload. Outstanding producers/unknown native
    /// ownership refuse unload even if the client ID disappeared.
    pub fn reap_async_auth(&self, limit: usize) -> Result<usize, RedisError> {
        ordinary(self)?;
        let _reaping = Reaping::begin();
        collect_ready(self.ctx);
        Ok(drain_cleanup(limit))
    }
}

fn collect_ready(ctx: *mut raw::RedisModuleCtx) {
    if SUBMISSIONS.load(Ordering::Relaxed) != 0 || CALLBACKS.load(Ordering::Relaxed) != 0 {
        return;
    }
    let jobs: Vec<_> = registry().jobs.values().cloned().collect();
    for job in jobs {
        let stop = {
            let mut inner = job.inner();
            if inner.winner || !inner.native_live {
                inner.timer.take()
            } else {
                None
            }
        };
        if let Some(timer) = stop {
            let mut data = ptr::null_mut();
            if unsafe { (job.api.stop_timer)(ctx, timer, &mut data) } != raw::REDISMODULE_OK as i32
                || data != job.private()
            {
                job.inner().timer_unknown = true;
            }
        }
        let eligible = {
            let inner = job.inner();
            !inner.native_live
                && inner.state != NativeState::Unknown
                && inner.producers == 0
                && inner.submitting == 0
                && inner.timer.is_none()
                && !inner.timer_unknown
        };
        if eligible {
            let mut state = registry();
            if let Some(job) = state.jobs.remove(&job.cookie.key) {
                state.cleanup.push_back(Cleanup::Job(job));
            }
        }
    }
}

fn drain_cleanup(limit: usize) -> usize {
    let mut processed = 0;
    while processed < limit {
        let next = registry().cleanup.pop_front();
        let Some(next) = next else {
            break;
        };
        users::contain(|| match next {
            Cleanup::Payload(value) => drop(value),
            Cleanup::Job(job) => drop(job),
            Cleanup::Subscription(client, subscription, true) => {
                users::notify(client, subscription)
            }
            Cleanup::Subscription(_, subscription, false) => {
                users::LIVE_SUBSCRIPTIONS.fetch_sub(1, Ordering::Relaxed);
                // OWNED and the reaper lease still exclude unload if a failed
                // installation's capture destructor unwinds here.
                drop(subscription);
            }
        });
        OWNED.fetch_sub(1, Ordering::Relaxed);
        processed += 1;
    }
    processed
}
