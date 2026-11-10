//! Keeps panics in module code from unwinding into Redis.
//!
//! Redis calls a module through `extern "C"` functions. A Rust panic that
//! reaches one of them aborts the process, and the Redis server with it. Every
//! callback this crate registers runs the module's code through
//! [`catch_panic`]: a command replies with an error, and a callback with no
//! client to reply to logs the panic at warning level and returns a neutral
//! value.
//!
//! Panics are caught only when the module is built with the default
//! `panic = "unwind"`. Callbacks that a module registers itself, such as native
//! type methods, can use [`catch_panic`] the same way.

use std::any::Any;
use std::cell::Cell;
use std::ffi::CString;
use std::os::raw::c_int;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::raw;

/// Runs `f`, and returns the panic message as the error if it panics.
///
/// The closure is treated as unwind safe: after a panic, state it shared with
/// the caller may be partly updated, and a `Mutex` it held is poisoned.
/// The caught payload is leaked: its destructor can itself panic, so running
/// it here could unwind out of the callback's panic guard into Redis.
pub fn catch_panic<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let message = panic_message(payload.as_ref());
        std::mem::forget(payload);
        message
    })
}

/// The message of a panic payload, for the `&str` and `String` payloads that
/// `panic!` produces.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic payload is not a string".to_owned()
    }
}

/// Converts text for an error reply or a log line, replacing NUL, CR and LF,
/// which neither can carry, with spaces.
fn to_c_string(text: &str) -> CString {
    let text: String = text
        .chars()
        .map(|c| match c {
            '\0' | '\r' | '\n' => ' ',
            c => c,
        })
        .collect();
    CString::new(text).expect("NUL bytes were replaced")
}

/// Logs a caught panic at warning level, through a fixed format string.
fn log_panic(callback: &str, message: &str) {
    let text = to_c_string(&format!("{callback} panicked: {message}"));
    // SAFETY: Redis initialized the API before calling any callback, and a null
    // context logs without one. The message is a format argument, never the format.
    unsafe {
        if let Some(log) = raw::RedisModule_Log {
            log(
                ptr::null_mut(),
                c"warning".as_ptr(),
                c"%s".as_ptr(),
                text.as_ptr(),
            );
        }
    }
}

/// Ids for command invocations, unique across threads. 0 means none.
static NEXT_COMMAND: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The command invocation that [`run_command`] is running on this thread,
    /// or 0. A `Cell<u64>` has no destructor, which would keep glibc from
    /// unloading the module.
    static CURRENT_COMMAND: Cell<u64> = const { Cell::new(0) };
}

/// The command invocation running on this thread, or 0 outside one.
pub(crate) fn current_command() -> u64 {
    CURRENT_COMMAND.get()
}

/// Runs a command callback. If it panics, replies to the client with
/// `ERR command panicked: <message>` and returns `REDISMODULE_OK`.
///
/// After a complete safe command reply, logs the panic without appending an
/// error. Partial serialization and raw native reply writes are not counted;
/// a panic there still appends its error after the part already written.
///
/// # Safety
///
/// `ctx` must be the context Redis passed to the command callback.
#[doc(hidden)]
pub unsafe fn run_command(ctx: *mut raw::RedisModuleCtx, command: impl FnOnce() -> c_int) -> c_int {
    let previous = CURRENT_COMMAND.replace(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed));
    // A nested command records its own released blocked clients.
    let released = crate::context::blocked::replace_released_unreplied(0);
    // Keep the frame alive through panic recovery, after the command's own
    // stack has unwound. Linking borrows it so its address remains stable.
    let replies = crate::context::command_reply::Frame::new(ctx);
    let _replies = replies.link();
    let status = catch_panic(command).unwrap_or_else(|message| {
        if replies.complete() {
            log_panic("command", &message);
            return raw::REDISMODULE_OK as c_int;
        }
        let reply = to_c_string(&format!("ERR command panicked: {message}"));
        // SAFETY: ctx is the context Redis passed to this command.
        unsafe {
            match raw::RedisModule_ReplyWithError {
                Some(reply_with_error) => reply_with_error(ctx, reply.as_ptr()),
                None => raw::Status::Err as c_int,
            }
        }
    });
    crate::context::blocked::replace_released_unreplied(released);
    CURRENT_COMMAND.set(previous);
    status
}

/// Runs a callback that has no client to reply to. If it panics, logs the
/// panic at warning level, naming `callback`, and returns `fallback()`.
#[doc(hidden)]
pub fn run_callback<R>(callback: &str, f: impl FnOnce() -> R, fallback: impl FnOnce() -> R) -> R {
    catch_panic(f).unwrap_or_else(|message| {
        log_panic(callback, &message);
        fallback()
    })
}

/// Runs a configuration setter. If it panics, fails the CONFIG SET with the
/// panic message through `err`.
pub(crate) fn run_config_set(
    err: *mut *mut raw::RedisModuleString,
    set: impl FnOnce() -> c_int,
) -> c_int {
    catch_panic(set).unwrap_or_else(|message| {
        let text = to_c_string(&format!("configuration setter panicked: {message}"));
        // SAFETY: Redis passes a valid error slot and takes ownership of the
        // string stored in it.
        unsafe {
            if let Some(create_string) = raw::RedisModule_CreateString {
                *err = create_string(ptr::null_mut(), text.as_ptr(), text.as_bytes().len());
            }
        }
        raw::REDISMODULE_ERR as c_int
    })
}

thread_local! {
    /// A panic caught in a callback that Redis made in the middle of a call
    /// from Rust, waiting for that call to return. It holds a raw pointer
    /// rather than a `Box`: a thread local with a destructor would keep glibc
    /// from unloading the module on MODULE UNLOAD.
    static DEFERRED_PANIC: Cell<Option<NonNull<dyn Any + Send>>> = const { Cell::new(None) };
}

/// Runs a callback that Redis makes in the middle of a call from Rust, such as
/// a scan callback. A panic is kept until [`resume_deferred_panic`] runs after
/// the outer call returns, and the callbacks that follow it are skipped.
pub(crate) fn defer_panic(f: impl FnOnce()) {
    if DEFERRED_PANIC.get().is_some() {
        return;
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(f)) {
        DEFERRED_PANIC.set(Some(NonNull::from(Box::leak(payload))));
    }
}

/// Resumes a panic that [`defer_panic`] kept, in the Rust code that made the
/// call into Redis.
pub(crate) fn resume_deferred_panic() {
    if let Some(payload) = DEFERRED_PANIC.take() {
        // SAFETY: the pointer came from Box::leak in defer_panic, and taking it
        // out of the slot means it is turned back into a Box only once.
        resume_unwind(unsafe { Box::from_raw(payload.as_ptr()) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catch_panic_returns_the_value_or_the_message() {
        assert_eq!(catch_panic(|| 7), Ok(7));
        assert_eq!(
            catch_panic(|| -> i32 { panic!("static message") }),
            Err("static message".to_owned())
        );
        let value = 3;
        assert_eq!(
            catch_panic(|| -> i32 { panic!("formatted {value}") }),
            Err("formatted 3".to_owned())
        );
        assert_eq!(
            catch_panic(|| -> i32 { std::panic::panic_any(5_u8) }),
            Err("panic payload is not a string".to_owned())
        );
    }

    #[test]
    fn c_strings_replace_nul_and_line_breaks() {
        assert_eq!(to_c_string("a\0b\r\nc").as_bytes(), b"a b  c".as_slice());
        assert_eq!(to_c_string("caf\u{e9}").as_bytes(), "caf\u{e9}".as_bytes());
    }

    #[test]
    fn run_callback_returns_the_fallback_after_a_panic() {
        assert_eq!(run_callback("test", || 1, || 2), 1);
        // Outside Redis the log call is skipped, so only the fallback shows.
        assert_eq!(run_callback("test", || -> i32 { panic!("boom") }, || 2), 2);
    }

    #[test]
    fn caught_payload_destructors_cannot_escape_the_panic_guard() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("panic payload destructor ran");
            }
        }

        assert_eq!(
            catch_panic(|| std::panic::panic_any(PanicOnDrop)),
            Err("panic payload is not a string".to_owned())
        );
        assert_eq!(
            run_callback("payload test", || std::panic::panic_any(PanicOnDrop), || 42),
            42
        );
        assert_eq!(catch_panic(|| 7), Ok(7));
    }

    #[test]
    fn the_deferred_panic_slot_needs_no_destructor() {
        assert!(!std::mem::needs_drop::<Cell<Option<NonNull<dyn Any + Send>>>>());
        assert!(!std::mem::needs_drop::<Cell<u64>>());
    }

    #[test]
    fn run_command_sets_and_restores_the_current_invocation() {
        assert_eq!(current_command(), 0);
        let (outer, inner) = unsafe {
            let mut ids = (0, 0);
            run_command(ptr::null_mut(), || {
                ids.0 = current_command();
                run_command(ptr::null_mut(), || {
                    ids.1 = current_command();
                    0
                });
                assert_eq!(current_command(), ids.0);
                0
            });
            ids
        };
        assert_ne!(outer, 0);
        assert_ne!(inner, 0);
        assert_ne!(outer, inner);
        assert_eq!(current_command(), 0);
        // Outside Redis the panic reply is skipped, and the invocation is
        // still restored.
        unsafe { run_command(ptr::null_mut(), || panic!("command failed")) };
        assert_eq!(current_command(), 0);
    }

    #[test]
    fn deferred_panics_skip_later_callbacks_and_resume_once() {
        let mut calls = 0;
        defer_panic(|| calls += 1);
        defer_panic(|| panic!("first"));
        defer_panic(|| calls += 1);
        assert_eq!(calls, 1);
        let resumed = catch_panic(resume_deferred_panic);
        assert_eq!(resumed, Err("first".to_owned()));
        assert_eq!(catch_panic(resume_deferred_panic), Ok(()));
        defer_panic(|| calls += 1);
        assert_eq!(calls, 2);
    }
}
