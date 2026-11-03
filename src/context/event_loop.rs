//! The module event loop API: run code on the main thread, and watch file
//! descriptors with Redis's event loop.
//!
//! Callbacks run on the main thread with the Redis lock held, through the
//! panic guard, and receive a [`EventContext`] for the module's detached context.
//!
//! Redis keeps pending one-shots and watched descriptors across `MODULE UNLOAD`
//! and calls them in the module image even after it is unmapped. So with
//! [`crate::redis_module!`], a successful unload stops watching every
//! descriptor still watched after `deinit`, and a failed load those it added.
//! Redis cannot cancel a one-shot, so [`add_one_shot`] is refused while the
//! module loads or unloads, and unload is refused while one is pending.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::raw::c_int;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use bitflags::bitflags;

use crate::{EventContext, MODULE_CONTEXT, RedisError, raw};

bitflags! {
    /// Which events on a file descriptor a callback from [`add`] handles.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EventLoopMask : c_int {
        /// The descriptor has data to read.
        const READABLE = raw::REDISMODULE_EVENTLOOP_READABLE as c_int;
        /// The descriptor accepts writes.
        const WRITABLE = raw::REDISMODULE_EVENTLOOP_WRITABLE as c_int;
    }
}

type FdCallback = Arc<Mutex<dyn FnMut(&EventContext, RawFd, EventLoopMask) + Send>>;
type OneShot = Box<dyn FnOnce(&EventContext) + Send>;

/// The callbacks registered for one file descriptor.
#[derive(Default)]
struct FdCallbacks {
    readable: Option<FdCallback>,
    writable: Option<FdCallback>,
}

/// Callbacks by file descriptor. Redis keeps one `user_data` per descriptor
/// for both directions, so the trampoline looks the callback up here. A static
/// rather than a thread local: a thread local with a destructor would keep
/// glibc from unloading the module.
static FD_CALLBACKS: Mutex<BTreeMap<RawFd, FdCallbacks>> = Mutex::new(BTreeMap::new());

fn fd_callbacks() -> std::sync::MutexGuard<'static, BTreeMap<RawFd, FdCallbacks>> {
    FD_CALLBACKS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where the module is in its `redis_module!` lifecycle, for the event loop.
const UNLOADED: u8 = 0;
const LOADING: u8 = 1;
const ACTIVE: u8 = 2;
const UNLOADING: u8 = 3;
static PHASE: AtomicU8 = AtomicU8::new(UNLOADED);

/// One-shots Redis holds that have not finished running.
static PENDING_ONE_SHOTS: AtomicUsize = AtomicUsize::new(0);

/// Internal binding for `redis_module!`: [`add_one_shot`] is refused until the
/// load commits, and a failed load stops watching what it added.
#[doc(hidden)]
pub struct EventLoopLoadGuard {
    committed: bool,
}

impl EventLoopLoadGuard {
    pub fn begin() -> Self {
        PHASE.store(LOADING, Ordering::SeqCst);
        Self { committed: false }
    }

    pub fn commit(mut self) {
        PHASE.store(ACTIVE, Ordering::SeqCst);
        self.committed = true;
    }
}

impl Drop for EventLoopLoadGuard {
    fn drop(&mut self) {
        if !self.committed {
            PHASE.store(UNLOADED, Ordering::SeqCst);
            remove_all_watches();
        }
    }
}

/// Internal binding for `redis_module!`: refuses unload while Redis holds a
/// pending one-shot, refuses new ones and new watches during it, and stops
/// watching every descriptor still watched when it commits.
#[doc(hidden)]
pub struct EventLoopUnloadGuard {
    committed: bool,
}

impl EventLoopUnloadGuard {
    pub fn begin() -> Result<Self, RedisError> {
        // A worker's add_one_shot counts itself before it reads the phase, so
        // either it sees this store and backs out, or the load below sees it.
        PHASE.store(UNLOADING, Ordering::SeqCst);
        if PENDING_ONE_SHOTS.load(Ordering::SeqCst) != 0 {
            PHASE.store(ACTIVE, Ordering::SeqCst);
            return Err(RedisError::Str(
                "ERR module has pending event loop one-shots",
            ));
        }
        Ok(Self { committed: false })
    }

    /// Call after user deinit, which may stop watching descriptors itself.
    pub fn commit(mut self) {
        PHASE.store(UNLOADED, Ordering::SeqCst);
        self.committed = true;
        remove_all_watches();
    }
}

impl Drop for EventLoopUnloadGuard {
    fn drop(&mut self) {
        if !self.committed {
            PHASE.store(ACTIVE, Ordering::SeqCst);
        }
    }
}

/// Stops watching every descriptor in the registry and drops their callbacks,
/// with the registry unlocked.
fn remove_all_watches() {
    let watched = std::mem::take(&mut *fd_callbacks());
    if let Some(event_loop_del) = unsafe { raw::RedisModule_EventLoopDel } {
        for (fd, entry) in &watched {
            let mut mask = EventLoopMask::empty();
            mask.set(EventLoopMask::READABLE, entry.readable.is_some());
            mask.set(EventLoopMask::WRITABLE, entry.writable.is_some());
            // SAFETY: OnLoad and OnUnload run on the main thread, and these are
            // the directions this module registered for the descriptor.
            unsafe { event_loop_del(*fd, mask.bits()) };
        }
    }
    drop(watched);
}

/// The context passed to event loop callbacks, or `None` while no module
/// context is stored.
fn callback_context() -> Option<EventContext> {
    let ctx = MODULE_CONTEXT
        .ctx
        .load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: Redis runs event loop callbacks on the main thread with its lock
    // held, and a stored context stays valid until an unload or a failed load,
    // which also hold the lock, releases it.
    (!ctx.is_null()).then(|| unsafe { EventContext::new(ctx) })
}

/// The error for a failed call, with the `errno` it left.
fn call_error(api: &str) -> RedisError {
    RedisError::String(format!(
        "ERR {api} failed: {}",
        std::io::Error::last_os_error()
    ))
}

/// Watches `fd` with Redis's event loop: on the main thread, Redis calls
/// `callback` with the direction, readable or writable, that is ready. A
/// callback registered for both directions is called once for each that is
/// ready. Adding a callback for a direction that already has one replaces it.
///
/// A callback that panics stops being called: the panic is logged and the
/// direction is no longer watched, since a callback that panicked before
/// handling its event would otherwise be called again on every event loop
/// iteration. With `redis_module!`, a successful unload stops watching every
/// descriptor still watched after `deinit`, and a failed load stops watching
/// those it added; the descriptors themselves stay open.
///
/// # Safety
///
/// Call it on the main thread, for example from a command: the event loop is
/// not thread safe. `fd` must be a descriptor the module owns, not one Redis
/// uses, such as a client connection: Redis keeps its own data for each
/// descriptor it watches. Stop watching `fd` with [`remove`] before closing it.
///
/// # Errors
///
/// Returns an error if `mask` is empty, if Redis rejects `fd` (for example one
/// above `maxclients`), if the module is unloading, or if the running Redis
/// does not provide `RedisModule_EventLoopAdd` (before 7.0). Nothing stays
/// registered after an error.
pub unsafe fn add(
    fd: RawFd,
    mask: EventLoopMask,
    callback: impl FnMut(&EventContext, RawFd, EventLoopMask) + Send + 'static,
) -> Result<(), RedisError> {
    let event_loop_add = unsafe { crate::__available_api!(RedisModule_EventLoopAdd) };
    let event_loop_del = unsafe { crate::__available_api!(RedisModule_EventLoopDel) };
    if mask.is_empty() {
        return Err(RedisError::Str(
            "ERR an event loop mask needs READABLE or WRITABLE",
        ));
    }
    if !matches!(PHASE.load(Ordering::SeqCst), LOADING | ACTIVE) {
        return Err(RedisError::Str(
            "ERR event loop descriptors cannot be watched while the module unloads",
        ));
    }
    let callback: FdCallback = Arc::new(Mutex::new(callback));
    let mut added = EventLoopMask::empty();
    // Each direction gets its own trampoline: Redis passes the whole fired
    // mask to both, so the mask alone does not say which callback it calls.
    for (direction, trampoline) in [
        (
            EventLoopMask::READABLE,
            readable_trampoline as RawEventLoopFunc,
        ),
        (
            EventLoopMask::WRITABLE,
            writable_trampoline as RawEventLoopFunc,
        ),
    ] {
        if !mask.contains(direction) {
            continue;
        }
        // SAFETY: the trampolines look the callback up by fd and ignore user_data.
        let status =
            unsafe { event_loop_add(fd, direction.bits(), Some(trampoline), std::ptr::null_mut()) };
        if status != raw::REDISMODULE_OK as c_int {
            let error = call_error("RedisModule_EventLoopAdd");
            if !added.is_empty() {
                // SAFETY: undoes the registration made above, on the same thread.
                unsafe { event_loop_del(fd, added.bits()) };
                drop(forget_callbacks(fd, added));
            }
            return Err(error);
        }
        added |= direction;
        let replaced = {
            let mut registry = fd_callbacks();
            let entry = registry.entry(fd).or_default();
            if direction == EventLoopMask::READABLE {
                entry.readable.replace(callback.clone())
            } else {
                entry.writable.replace(callback.clone())
            }
        };
        // Dropped with the registry unlocked: its destructor may add or remove
        // descriptors.
        drop(replaced);
    }
    Ok(())
}

type RawEventLoopFunc = unsafe extern "C" fn(c_int, *mut c_void, c_int);

/// Stops watching `fd` for the directions in `mask` that [`add`] registered,
/// and drops their callbacks. A callback may remove its own descriptor.
///
/// # Safety
///
/// Call it on the main thread, like [`add`].
///
/// # Errors
///
/// Returns an error if `fd` has no callback from [`add`] for a direction in
/// `mask`, or if the running Redis does not provide `RedisModule_EventLoopDel`
/// (before 7.0).
pub unsafe fn remove(fd: RawFd, mask: EventLoopMask) -> Result<(), RedisError> {
    let event_loop_del = unsafe { crate::__available_api!(RedisModule_EventLoopDel) };
    {
        // Only descriptors registered here: Redis frees its data for a
        // descriptor when its last event is removed, which for a descriptor
        // Redis owns would free that descriptor's connection.
        let registry = fd_callbacks();
        let registered = registry.get(&fd).is_some_and(|entry| {
            (!mask.contains(EventLoopMask::READABLE) || entry.readable.is_some())
                && (!mask.contains(EventLoopMask::WRITABLE) || entry.writable.is_some())
        });
        if mask.is_empty() || !registered {
            return Err(RedisError::Str(
                "ERR the descriptor has no event loop callback for that mask",
            ));
        }
    }
    let status = unsafe { event_loop_del(fd, mask.bits()) };
    if status != raw::REDISMODULE_OK as c_int {
        return Err(call_error("RedisModule_EventLoopDel"));
    }
    drop(forget_callbacks(fd, mask));
    Ok(())
}

/// Takes the registry's callbacks for the directions in `mask` out of it. The
/// caller drops them after the registry is unlocked, because a callback's
/// destructor may add or remove descriptors.
#[must_use]
fn forget_callbacks(fd: RawFd, mask: EventLoopMask) -> Vec<FdCallback> {
    let mut registry = fd_callbacks();
    let mut forgotten = Vec::new();
    if let Some(entry) = registry.get_mut(&fd) {
        if mask.contains(EventLoopMask::READABLE) {
            forgotten.extend(entry.readable.take());
        }
        if mask.contains(EventLoopMask::WRITABLE) {
            forgotten.extend(entry.writable.take());
        }
        if entry.readable.is_none() && entry.writable.is_none() {
            registry.remove(&fd);
        }
    }
    forgotten
}

extern "C" fn readable_trampoline(fd: c_int, _user_data: *mut c_void, _mask: c_int) {
    run_fd_callback(fd, EventLoopMask::READABLE);
}

extern "C" fn writable_trampoline(fd: c_int, _user_data: *mut c_void, _mask: c_int) {
    run_fd_callback(fd, EventLoopMask::WRITABLE);
}

fn run_fd_callback(fd: RawFd, direction: EventLoopMask) {
    let callback = {
        let registry = fd_callbacks();
        registry.get(&fd).and_then(|entry| {
            if direction == EventLoopMask::READABLE {
                entry.readable.clone()
            } else {
                entry.writable.clone()
            }
        })
    };
    let Some(callback) = callback else {
        return;
    };
    // The registry is unlocked here, so the callback may add or remove
    // descriptors, its own included.
    let completed = crate::panic::run_callback(
        "event loop callback",
        || match callback.try_lock() {
            Ok(mut callback) => {
                if let Some(ctx) = callback_context() {
                    callback(&ctx, fd, direction);
                }
                true
            }
            // A callback that lets Redis process events can be reached again
            // while it runs; that nested call is skipped.
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Poisoned(_)) => false,
        },
        || false,
    );
    if !completed {
        stop_after_panic(fd, direction, &callback);
    }
    // When the callback removed its own descriptor, this is the last reference,
    // and its destructor runs inside the guard.
    crate::panic::run_callback(
        "event loop callback destructor",
        move || drop(callback),
        || (),
    );
}

/// Stops watching `fd` in `direction` after its callback panicked, if that
/// callback is still the registered one.
fn stop_after_panic(fd: RawFd, direction: EventLoopMask, callback: &FdCallback) {
    let registered = fd_callbacks().get(&fd).is_some_and(|entry| {
        let current = if direction == EventLoopMask::READABLE {
            &entry.readable
        } else {
            &entry.writable
        };
        current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, callback))
    });
    if !registered {
        return;
    }
    // SAFETY: event loop callbacks run on the main thread, and the registry
    // holds a callback for this direction.
    let removed = unsafe { remove(fd, direction) };
    if let Some(ctx) = callback_context() {
        match removed {
            Ok(()) => ctx.log_warning(&format!(
                "event loop callback for descriptor {fd} panicked; it is no longer watched"
            )),
            Err(error) => ctx.log_warning(&format!(
                "event loop callback for descriptor {fd} panicked and could not be removed: {error}"
            )),
        }
    }
}

/// Runs `callback` once on the main thread, with the Redis lock held. It can be
/// called from any thread. Module unload is refused until it has run.
///
/// # Errors
///
/// Returns an error if the module is loading or unloading (schedule from a
/// command, event or worker instead of `init` or deinit), if the running Redis
/// does not provide `RedisModule_EventLoopAddOneShot` (before 7.0), or if it
/// rejects the call.
pub fn add_one_shot(
    callback: impl FnOnce(&EventContext) + Send + 'static,
) -> Result<(), RedisError> {
    let add_one_shot = unsafe { crate::__available_api!(RedisModule_EventLoopAddOneShot) };
    // Counted before reading the phase, so a concurrent unload either sees
    // this one-shot or this call sees the unload; see EventLoopUnloadGuard.
    PENDING_ONE_SHOTS.fetch_add(1, Ordering::SeqCst);
    if PHASE.load(Ordering::SeqCst) != ACTIVE {
        PENDING_ONE_SHOTS.fetch_sub(1, Ordering::SeqCst);
        return Err(RedisError::Str(
            "ERR event loop one-shots cannot be added while the module loads or unloads",
        ));
    }
    let callback: Box<OneShot> = Box::new(Box::new(callback));
    let user_data = Box::into_raw(callback);
    let status = unsafe { add_one_shot(Some(one_shot_trampoline), user_data.cast()) };
    if status != raw::REDISMODULE_OK as c_int {
        // SAFETY: Redis did not take the callback.
        drop(unsafe { Box::from_raw(user_data) });
        PENDING_ONE_SHOTS.fetch_sub(1, Ordering::SeqCst);
        return Err(call_error("RedisModule_EventLoopAddOneShot"));
    }
    Ok(())
}

extern "C" fn one_shot_trampoline(user_data: *mut c_void) {
    // SAFETY: user_data came from Box::into_raw in add_one_shot, and Redis
    // calls each one-shot once.
    let callback = unsafe { Box::from_raw(user_data.cast::<OneShot>()) };
    // Without a context the one-shot is dropped uncalled, inside the guard.
    crate::panic::run_callback(
        "event loop one-shot",
        || {
            if let Some(ctx) = callback_context() {
                callback(&ctx);
            }
        },
        || (),
    );
    PENDING_ONE_SHOTS.fetch_sub(1, Ordering::SeqCst);
}
