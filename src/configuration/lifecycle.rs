//! Private ownership for macro-managed configuration registrations.
//!
//! All access, including destruction and native API calls, requires the Redis
//! lock. Never hold a registry borrow across Redis calls or user destructors.
use super::ConfigurationContext;
use crate::{EventContext, RedisError, RedisGILGuard, raw};
use std::ffi::{c_int, c_void};

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Unbound,
    Loading,
    Active,
    Deinit,
    Closing,
    Unloaded,
}

pub(super) struct Owner {
    pointer: *mut c_void,
    destroy: unsafe fn(*mut c_void),
}
// SAFETY: Only configuration registration constructs these erased owners.
// Captures are Send and caller storage is borrowed as &'static T: Sync. The
// only non-Send owner is the private getter's native string cache, accessed and
// freed exclusively under the Redis lock while the module is alive. This is
// not a general native-handle container and grants no public Send capability.
unsafe impl Send for Owner {}
impl Owner {
    pub(super) unsafe fn new<T>(pointer: *mut T) -> Self {
        unsafe fn destroy<T>(pointer: *mut c_void) {
            // SAFETY: This is the unique owner of the original typed Box.
            unsafe { drop(Box::from_raw(pointer.cast::<T>())) };
        }
        Self {
            pointer: pointer.cast(),
            destroy: destroy::<T>,
        }
    }
}

struct NativeContext(*mut raw::RedisModuleCtx);
// SAFETY: This private detached context is accessed/freed only under the Redis
// lock. Its pointer never escapes as a transferable public context.
unsafe impl Send for NativeContext {}

// Redis runs its event loop on the process's original thread on these
// platforms. Do not identify it using OnLoad's caller: a locked worker can
// itself invoke MODULE LOAD. Rust thread identity would also install TLS with
// a destructor, which can prevent dlclose on Linux.
fn is_main_thread() -> bool {
    #[cfg(target_os = "linux")]
    // SAFETY: These OS identity queries require no pointers or capabilities.
    unsafe {
        libc::syscall(libc::SYS_gettid) == libc::getpid() as libc::c_long
    }
    #[cfg(target_os = "macos")]
    // SAFETY: pthread_main_np is a native thread-identity query.
    unsafe {
        libc::pthread_main_np() != 0
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

// Identify the synchronous load/deinit caller separately from the OS main
// thread. A worker can itself invoke MODULE LOAD or MODULE UNLOAD.
struct CallbackThread {
    #[cfg(unix)]
    id: libc::pthread_t,
}
// SAFETY: Native identity is compared only while the load/deinit caller is alive.
unsafe impl Send for CallbackThread {}
impl CallbackThread {
    fn capture() -> Self {
        Self {
            #[cfg(unix)]
            // SAFETY: Native identity query on this live thread.
            id: unsafe { libc::pthread_self() },
        }
    }
    fn is_current(&self) -> bool {
        #[cfg(unix)]
        // SAFETY: The native caller remains alive for this identity's lifetime.
        unsafe {
            libc::pthread_equal(self.id, libc::pthread_self()) != 0
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

struct Registry {
    phase: Phase,
    generation: usize,
    owners: Vec<Owner>,
    context: Option<NativeContext>,
    exclusive_thread: Option<CallbackThread>,
    active: usize,
    job: bool,
    timer: bool,
    unload_disabled: bool,
}
static REGISTRY: RedisGILGuard<Registry> = RedisGILGuard::new(Registry {
    phase: Phase::Unbound,
    generation: 0,
    owners: Vec::new(),
    context: None,
    exclusive_thread: None,
    active: 0,
    job: false,
    timer: false,
    unload_disabled: false,
});

// Configuration trampolines are entered by Redis with its lock held. This
// private proof is never returned to a user or transferred to another thread.
fn lock_proof() -> ConfigurationContext {
    ConfigurationContext::new()
}

pub(super) fn reserve() -> Result<bool, RedisError> {
    let proof = lock_proof();
    let mut registry = REGISTRY.lock(&proof);
    match registry.phase {
        Phase::Unbound => Ok(false), // Custom loader: preserve legacy retention.
        Phase::Loading => {
            registry
                .owners
                .try_reserve(1)
                .map_err(|_| RedisError::Str("ERR cannot reserve configuration ownership"))?;
            Ok(true)
        }
        _ => Err(RedisError::Str(
            "ERR configuration registration requires module load",
        )),
    }
}

pub(super) fn accept(owner: Owner) {
    let proof = lock_proof();
    // The native Register*Config call does not invoke callbacks. The slot was
    // reserved before that call, so acceptance cannot allocate or run user code.
    REGISTRY.lock(&proof).owners.push(owner);
}

/// Internal binding for redis_module!; not a custom-loader lifecycle API.
#[doc(hidden)]
pub struct ConfigurationLoad {
    generation: usize,
    _not_send: std::marker::PhantomData<*mut ()>,
}
impl ConfigurationLoad {
    /// # Safety
    /// Call once after Redis Init, inside this module's native OnLoad. Keep the
    /// Redis lock held through finish, before returning from that callback.
    pub unsafe fn begin(ctx: *mut raw::RedisModuleCtx) -> Result<Self, RedisError> {
        let proof = lock_proof();
        let generation = {
            let mut registry = REGISTRY.lock(&proof);
            if !matches!(registry.phase, Phase::Unbound | Phase::Unloaded) {
                return Err(RedisError::Str(
                    "ERR configuration lifecycle is already bound",
                ));
            }
            registry.generation = registry
                .generation
                .checked_add(1)
                .ok_or(RedisError::Str("ERR configuration generation exhausted"))?;
            registry.phase = Phase::Loading;
            registry.exclusive_thread = Some(CallbackThread::capture());
            registry.unload_disabled = false;
            registry.generation
        };
        // Native context holds the module association and a client (needed by
        // AddPostNotificationJob). Never reuse the SDK's global MODULE_CONTEXT.
        let context = unsafe {
            match (
                raw::RedisModule_GetDetachedThreadSafeContext,
                raw::RedisModule_FreeThreadSafeContext,
            ) {
                (Some(get), Some(_)) => Some(NativeContext(get(ctx))),
                _ => None,
            }
        };
        REGISTRY.lock(&proof).context = context.filter(|ctx| !ctx.0.is_null());
        Ok(Self {
            generation,
            _not_send: std::marker::PhantomData,
        })
    }

    /// Must run after the load callback has caught any init panic. No user Drop
    /// runs during unwinding of the init callback.
    pub fn finish(self, status: c_int) {
        let proof = lock_proof();
        if status == raw::REDISMODULE_OK as c_int {
            let mut registry = REGISTRY.lock(&proof);
            registry.phase = Phase::Active;
            registry.exclusive_thread = None;
        } else {
            close(self.generation);
        }
    }
}

/// Internal binding for redis_module!; not a custom-loader lifecycle API.
#[doc(hidden)]
pub struct ConfigurationUnload {
    generation: usize,
    _not_send: std::marker::PhantomData<*mut ()>,
    committed: bool,
}
impl ConfigurationUnload {
    /// # Safety
    /// Call inside the module's native OnUnload, before calling user deinit.
    /// Keep the Redis lock held for the guard's entire lifetime.
    pub unsafe fn begin(_ctx: &EventContext) -> Result<Self, RedisError> {
        let proof = lock_proof();
        let mut registry = REGISTRY.lock(&proof);
        if registry.phase != Phase::Active || registry.busy() {
            return Err(RedisError::Str(
                "ERR configuration operation prevents module unload",
            ));
        }
        registry.phase = Phase::Deinit;
        registry.exclusive_thread = Some(CallbackThread::capture());
        Ok(Self {
            generation: registry.generation,
            _not_send: std::marker::PhantomData,
            committed: false,
        })
    }
    pub fn commit(mut self) -> Result<(), RedisError> {
        let proof = lock_proof();
        if REGISTRY.lock(&proof).busy() {
            return Err(RedisError::Str(
                "ERR configuration operation started during deinit",
            ));
        }
        // From here onward cleanup is committed. Each destructor panic is
        // contained and cannot turn success into a failed unload with freed data.
        self.committed = true;
        close(self.generation);
        Ok(())
    }
}
impl Drop for ConfigurationUnload {
    fn drop(&mut self) {
        if !self.committed {
            let proof = lock_proof();
            let mut registry = REGISTRY.lock(&proof);
            registry.phase = Phase::Active;
            registry.exclusive_thread = None;
        }
    }
}
impl Registry {
    fn busy(&self) -> bool {
        self.active != 0 || self.job || self.timer || self.unload_disabled
    }
}

fn close(generation: usize) {
    let proof = lock_proof();
    let (owners, context) = {
        let mut registry = REGISTRY.lock(&proof);
        assert_eq!(registry.generation, generation);
        registry.phase = Phase::Closing;
        (
            std::mem::take(&mut registry.owners),
            registry.context.take(),
        )
    };
    for owner in owners {
        let destroyed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: Detached exact typed owners; no callback can dereference
            // their private data after Closing, and the Redis lock remains held.
            unsafe { (owner.destroy)(owner.pointer) };
        }));
        if let Err(payload) = destroyed {
            // A panic payload can itself have an arbitrary, panicking Drop.
            // Retain it rather than run another user destructor after teardown
            // has committed. Even cleanup-panic reporting must not reverse an
            // unload after some registration owners have already been freed.
            std::mem::forget(payload);
            // SAFETY: The API is initialized and null-context logging is legal.
            // Fixed C strings avoid formatting or inspecting the user payload.
            unsafe {
                if let Some(log) = raw::RedisModule_Log {
                    log(
                        std::ptr::null_mut(),
                        c"warning".as_ptr(),
                        c"%s".as_ptr(),
                        c"configuration cleanup panicked; continuing module teardown".as_ptr(),
                    );
                }
            }
        }
    }
    if let Some(context) = context {
        // SAFETY: No job/timer references this context at committed cleanup.
        // Native free may reenter; Closing still denies private-data access.
        unsafe { raw::RedisModule_FreeThreadSafeContext.unwrap()(context.0) };
    }
    let mut registry = REGISTRY.lock(&proof);
    registry.phase = Phase::Unloaded;
    registry.exclusive_thread = None;
}

pub(super) struct CallbackGuard {
    managed: bool,
}
impl CallbackGuard {
    pub(super) fn enter() -> Option<Self> {
        let proof = lock_proof();
        let (generation, context, main, needs_job) = {
            let mut registry = REGISTRY.lock(&proof);
            match registry.phase {
                Phase::Closing | Phase::Unloaded => return None,
                Phase::Unbound => return Some(Self { managed: false }),
                Phase::Loading
                    if !registry
                        .exclusive_thread
                        .as_ref()
                        .is_some_and(CallbackThread::is_current) =>
                {
                    return None;
                }
                _ => {}
            }
            registry.active += 1;
            let main = is_main_thread();
            let synchronous_deinit = registry.phase == Phase::Deinit
                && registry
                    .exclusive_thread
                    .as_ref()
                    .is_some_and(CallbackThread::is_current);
            let track = registry.phase == Phase::Active
                || (registry.phase == Phase::Deinit && !synchronous_deinit);
            (
                registry.generation,
                registry.context.as_ref().map(|ctx| ctx.0),
                main,
                track && !registry.job && !registry.unload_disabled,
            )
        };
        let guard = Self { managed: true };
        if needs_job {
            let job = unsafe {
                match (raw::RedisModule_AddPostNotificationJob, context) {
                    (Some(add), Some(ctx)) => {
                        add(ctx, Some(completed_job), generation as *mut c_void, None)
                            == raw::REDISMODULE_OK as c_int
                    }
                    _ => false,
                }
            };
            if job {
                REGISTRY.lock(&proof).job = true;
            } else if main {
                let needs_timer = !REGISTRY.lock(&proof).timer;
                if needs_timer {
                    let armed = unsafe {
                        match (raw::RedisModule_CreateTimer, context) {
                            (Some(create), Some(ctx)) => {
                                // Main-thread RM_Yield services file events,
                                // not timers. This runs only after its native
                                // configuration stack has returned to the loop.
                                create(ctx, 0, Some(completed_timer), generation as *mut c_void);
                                true
                            }
                            _ => false,
                        }
                    };
                    let mut registry = REGISTRY.lock(&proof);
                    registry.timer = armed;
                    registry.unload_disabled |= !armed;
                }
            } else {
                // A worker can release the GIL through RM_Yield while retaining
                // an outer CONFIG frame. Main-loop timer completion is not a
                // proof of its return. Fail closed for this load generation.
                REGISTRY.lock(&proof).unload_disabled = true;
            }
        }
        Some(guard)
    }
}
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        if self.managed {
            REGISTRY.lock(&lock_proof()).active -= 1;
        }
    }
}

unsafe extern "C" fn completed_job(_: *mut raw::RedisModuleCtx, data: *mut c_void) {
    let proof = lock_proof();
    let mut registry = REGISTRY.lock(&proof);
    if registry.generation == data as usize {
        registry.job = false;
    }
}
unsafe extern "C" fn completed_timer(_: *mut raw::RedisModuleCtx, data: *mut c_void) {
    let proof = lock_proof();
    let mut registry = REGISTRY.lock(&proof);
    if registry.generation == data as usize {
        registry.timer = false;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::CallbackThread;

    #[test]
    fn loading_affinity_accepts_only_the_live_load_caller() {
        let identity = CallbackThread::capture();
        assert!(identity.is_current());
        let other_is_caller = std::thread::spawn(move || identity.is_current())
            .join()
            .unwrap();
        assert!(!other_is_caller);
    }
}
