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
use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::c_int;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::ptr;

use crate::raw;

/// Runs `f`, and returns the panic message as the error if it panics.
///
/// The closure is treated as unwind safe: after a panic, state it shared with
/// the caller may be partly updated, and a `Mutex` it held is poisoned.
pub fn catch_panic<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| panic_message(payload.as_ref()))
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

/// Runs a command callback. If it panics, replies to the client with
/// `ERR command panicked: <message>` and returns `REDISMODULE_OK`.
///
/// When the panic happens after part of the reply was written, for example
/// inside an array, the error reply is added after that part.
///
/// # Safety
///
/// `ctx` must be the context Redis passed to the command callback.
#[doc(hidden)]
pub unsafe fn run_command(ctx: *mut raw::RedisModuleCtx, command: impl FnOnce() -> c_int) -> c_int {
    catch_panic(command).unwrap_or_else(|message| {
        let reply = to_c_string(&format!("ERR command panicked: {message}"));
        // SAFETY: ctx is the context Redis passed to this command.
        unsafe {
            match raw::RedisModule_ReplyWithError {
                Some(reply_with_error) => reply_with_error(ctx, reply.as_ptr()),
                None => raw::Status::Err as c_int,
            }
        }
    })
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
    /// from Rust, waiting for that call to return.
    static DEFERRED_PANIC: RefCell<Option<Box<dyn Any + Send>>> = const { RefCell::new(None) };
}

/// Runs a callback that Redis makes in the middle of a call from Rust, such as
/// a scan callback. A panic is kept until [`resume_deferred_panic`] runs after
/// the outer call returns, and the callbacks that follow it are skipped.
pub(crate) fn defer_panic(f: impl FnOnce()) {
    if DEFERRED_PANIC.with(|deferred| deferred.borrow().is_some()) {
        return;
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(f)) {
        DEFERRED_PANIC.with(|deferred| *deferred.borrow_mut() = Some(payload));
    }
}

/// Resumes a panic that [`defer_panic`] kept, in the Rust code that made the
/// call into Redis.
pub(crate) fn resume_deferred_panic() {
    if let Some(payload) = DEFERRED_PANIC.with(|deferred| deferred.borrow_mut().take()) {
        resume_unwind(payload);
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
