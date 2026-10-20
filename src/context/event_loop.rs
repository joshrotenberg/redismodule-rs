//! The module event loop API: run code on the main thread, and watch file
//! descriptors with Redis's event loop.
//!
//! Callbacks run on the main thread with the Redis lock held, through the
//! panic guard, and receive a [`Context`] for the module's detached context.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::raw::c_int;
use std::os::unix::io::RawFd;
use std::sync::{Arc, Mutex, PoisonError};

use bitflags::bitflags;

use crate::{Context, MODULE_CONTEXT, RedisError, raw};

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

type FdCallback = Arc<Mutex<dyn FnMut(&Context, RawFd, EventLoopMask) + Send>>;
type OneShot = Box<dyn FnOnce(&Context) + Send>;

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

/// The context passed to event loop callbacks.
fn callback_context() -> Context {
    let ctx = MODULE_CONTEXT
        .ctx
        .load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: Redis runs event loop callbacks on the main thread, and the
    // module's detached context lives as long as the module.
    unsafe { Context::new(ctx) }
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
/// above `maxclients`), or if the running Redis does not provide
/// `RedisModule_EventLoopAdd` (before 7.0). Nothing stays registered after an
/// error.
pub unsafe fn add(
    fd: RawFd,
    mask: EventLoopMask,
    callback: impl FnMut(&Context, RawFd, EventLoopMask) + Send + 'static,
) -> Result<(), RedisError> {
    let event_loop_add = unsafe { crate::__available_api!(RedisModule_EventLoopAdd) };
    let event_loop_del = unsafe { crate::__available_api!(RedisModule_EventLoopDel) };
    if mask.is_empty() {
        return Err(RedisError::Str(
            "ERR an event loop mask needs READABLE or WRITABLE",
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
                forget_callbacks(fd, added);
            }
            return Err(error);
        }
        added |= direction;
        let mut registry = fd_callbacks();
        let entry = registry.entry(fd).or_default();
        if direction == EventLoopMask::READABLE {
            entry.readable = Some(callback.clone());
        } else {
            entry.writable = Some(callback.clone());
        }
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
    forget_callbacks(fd, mask);
    Ok(())
}

/// Drops the registry's callbacks for the directions in `mask`.
fn forget_callbacks(fd: RawFd, mask: EventLoopMask) {
    let mut registry = fd_callbacks();
    if let Some(entry) = registry.get_mut(&fd) {
        if mask.contains(EventLoopMask::READABLE) {
            entry.readable = None;
        }
        if mask.contains(EventLoopMask::WRITABLE) {
            entry.writable = None;
        }
        if entry.readable.is_none() && entry.writable.is_none() {
            registry.remove(&fd);
        }
    }
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
    // The registry is unlocked here, so the callback may add or remove
    // descriptors, its own included.
    if let Some(callback) = callback {
        crate::panic::run_callback(
            "event loop callback",
            || {
                // A callback that lets Redis process events, such as through
                // Context::yield_now, can be reached again while it runs;
                // that nested call is skipped.
                if let Ok(mut callback) = callback.try_lock() {
                    callback(&callback_context(), fd, direction);
                }
            },
            || (),
        );
    }
}

/// Runs `callback` once on the main thread, with the Redis lock held. It can be
/// called from any thread.
///
/// # Errors
///
/// Returns an error if the running Redis does not provide
/// `RedisModule_EventLoopAddOneShot` (before 7.0), or rejects the call.
pub fn add_one_shot(callback: impl FnOnce(&Context) + Send + 'static) -> Result<(), RedisError> {
    let add_one_shot = unsafe { crate::__available_api!(RedisModule_EventLoopAddOneShot) };
    let callback: Box<OneShot> = Box::new(Box::new(callback));
    let user_data = Box::into_raw(callback);
    let status = unsafe { add_one_shot(Some(one_shot_trampoline), user_data.cast()) };
    if status != raw::REDISMODULE_OK as c_int {
        // SAFETY: Redis did not take the callback.
        drop(unsafe { Box::from_raw(user_data) });
        return Err(call_error("RedisModule_EventLoopAddOneShot"));
    }
    Ok(())
}

extern "C" fn one_shot_trampoline(user_data: *mut c_void) {
    // SAFETY: user_data came from Box::into_raw in add_one_shot, and Redis
    // calls each one-shot once.
    let callback = unsafe { Box::from_raw(user_data.cast::<OneShot>()) };
    crate::panic::run_callback(
        "event loop one-shot",
        || callback(&callback_context()),
        || (),
    );
}
