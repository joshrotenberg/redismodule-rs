//! Counts complete replies through the command context's safe entry points.
//!
//! Serialization remains one operation: recursive aggregate elements do not
//! acquire another root. Raw native replies and partial raw aggregates are
//! outside this accounting, as are replies sent later by a blocked worker.

use std::cell::Cell;
use std::ptr;

use crate::{RedisError, raw};

pub(crate) const ALREADY_REPLIED: &str = "ERR the command already has its reply";

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Empty,
    Writing,
    Complete,
}

thread_local! {
    // No destructor: a destructor-bearing TLS slot prevents module unload on
    // glibc. Frames live on the callback stack and unlink before it ends.
    static CURRENT: Cell<*const Frame> = const { Cell::new(ptr::null()) };
}

pub(crate) struct Frame {
    ctx: *mut raw::RedisModuleCtx,
    state: Cell<State>,
    outer: Cell<*const Frame>,
}

impl Frame {
    pub(crate) fn new(ctx: *mut raw::RedisModuleCtx) -> Self {
        Self {
            ctx,
            state: Cell::new(State::Empty),
            outer: Cell::new(ptr::null()),
        }
    }

    pub(crate) fn link(&self) -> Link<'_> {
        self.outer.set(CURRENT.replace(self));
        Link(self)
    }

    pub(crate) fn complete(&self) -> bool {
        self.state.get() == State::Complete
    }
}

pub(crate) struct Link<'a>(&'a Frame);

impl Drop for Link<'_> {
    fn drop(&mut self) {
        CURRENT.set(self.0.outer.get());
    }
}

/// Whether this invocation has already sent a complete safe reply. Used when
/// a dropped blocked handle would otherwise replace NoReply with an error.
pub(crate) fn current_complete() -> bool {
    // SAFETY: linked frames outlive their links, and unlink before return.
    unsafe { CURRENT.get().as_ref() }.is_some_and(Frame::complete)
}

/// Reserves one whole safe reply without borrowing state across native calls.
pub(crate) fn begin(ctx: *mut raw::RedisModuleCtx) -> Result<Write, RedisError> {
    let mut frame = CURRENT.get();
    // SAFETY: each linked frame lives on its callback's stack and its Link
    // keeps it borrowed until the frame unlinks, including during unwinding.
    while let Some(current) = unsafe { frame.as_ref() } {
        if current.ctx == ctx {
            if current.state.get() != State::Empty {
                log_duplicate();
                return Err(RedisError::Str(ALREADY_REPLIED));
            }
            current.state.set(State::Writing);
            return Ok(Write(frame));
        }
        frame = current.outer.get();
    }
    // Reply/timeout callbacks outside a command retain their existing writer.
    Ok(Write(ptr::null()))
}

pub(crate) struct Write(*const Frame);

impl Write {
    pub(crate) fn finish(mut self, status: raw::Status) -> raw::Status {
        // SAFETY: the synchronous reply operation ends before its callback's
        // frame unlinks. No mutable borrow is held across that operation.
        if let Some(frame) = unsafe { self.0.as_ref() } {
            frame.state.set(if status == raw::Status::Ok {
                State::Complete
            } else {
                State::Empty
            });
        }
        self.0 = ptr::null();
        status
    }
}

impl Drop for Write {
    fn drop(&mut self) {
        // A serialization panic leaves panic recovery responsible for its
        // error, as before. Partial serialization is not counted here.
        // SAFETY: Write never escapes the running synchronous operation.
        if let Some(frame) = unsafe { self.0.as_ref() } {
            frame.state.set(State::Empty);
        }
    }
}

pub(crate) fn write(
    ctx: *mut raw::RedisModuleCtx,
    send: impl FnOnce() -> raw::Status,
) -> raw::Status {
    let Ok(write) = begin(ctx) else {
        return raw::Status::Err;
    };
    write.finish(send())
}

fn log_duplicate() {
    // SAFETY: Redis initializes the API before command callbacks. A null
    // context logs for the module, and the message is a fixed format argument.
    unsafe {
        if let Some(log) = raw::RedisModule_Log {
            log(
                ptr::null_mut(),
                c"warning".as_ptr(),
                c"%s".as_ptr(),
                c"command wrote more than one safe reply; the SDK dropped another reply".as_ptr(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_and_panicking_writes_leave_room_for_an_error() {
        let ctx = ptr::null_mut();
        let frame = Frame::new(ctx);
        let _link = frame.link();
        assert_eq!(write(ctx, || raw::Status::Err), raw::Status::Err);
        let panic = std::panic::catch_unwind(|| {
            write(ctx, || panic!("serialization failed"));
        });
        assert!(panic.is_err());
        assert!(!frame.complete());
        assert_eq!(write(ctx, || raw::Status::Ok), raw::Status::Ok);
        assert!(frame.complete());
        assert!(begin(ctx).is_err());
    }

    #[test]
    fn nested_frames_match_context_and_restore_the_outer_frame() {
        let ctx = ptr::null_mut();
        let outer = Frame::new(ctx);
        let _outer = outer.link();
        {
            let inner = Frame::new(ptr::dangling_mut());
            let _inner = inner.link();
            assert_eq!(write(ctx, || raw::Status::Ok), raw::Status::Ok);
            assert!(outer.complete());
            assert!(!inner.complete());
            assert!(!current_complete());
        }
        assert!(current_complete());
        assert!(!std::mem::needs_drop::<Cell<*const Frame>>());
    }

    /// Measures only frame/admission bookkeeping in an optimized test binary.
    /// Wire measurements in docs/COMMAND-REPLIES.md cover the loaded module.
    #[test]
    #[ignore = "manual overhead measurement, not a timing assertion"]
    fn measure_command_reply_bookkeeping() {
        use std::hint::black_box;
        use std::time::Instant;

        const ITERATIONS: u32 = 1_000_000;
        for sample in 0..9 {
            let start = Instant::now();
            for _ in 0..ITERATIONS {
                black_box(raw::Status::Ok);
            }
            let baseline = start.elapsed().as_nanos() as f64 / f64::from(ITERATIONS);
            let start = Instant::now();
            for _ in 0..ITERATIONS {
                let ctx = black_box(ptr::null_mut());
                let frame = Frame::new(ctx);
                let _link = frame.link();
                black_box(write(ctx, || black_box(raw::Status::Ok)));
            }
            let guarded = start.elapsed().as_nanos() as f64 / f64::from(ITERATIONS);
            println!("sample={sample} baseline_ns={baseline:.3} guarded_ns={guarded:.3}");
        }
    }
}
