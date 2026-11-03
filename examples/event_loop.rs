//! Commands that use the module event loop API.
use std::os::unix::io::RawFd;
use std::sync::{Mutex, PoisonError};
use std::thread;

use redis_module::event_loop::{self, EventLoopMask};
use redis_module::{
    Context, MODULE_CONTEXT, NextArg, RedisError, RedisResult, RedisString, RedisValue,
    RegistrationContext, Status, redis_module,
};

/// `eventloop.set_later key value`: a thread schedules a one-shot, which sets
/// `key` on the main thread.
fn set_later(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_string()?;
    let value = args.next_string()?;
    args.done()?;
    thread::spawn(move || {
        let scheduled = event_loop::add_one_shot(move |ctx| {
            if let Err(error) = ctx.call("SET", &[key.as_str(), value.as_str()]) {
                ctx.log_warning(&format!("eventloop.set_later: {error}"));
            }
        });
        if let Err(error) = scheduled {
            MODULE_CONTEXT.log_warning(&format!("eventloop.set_later: {error}"));
        }
    });
    Ok(RedisValue::SimpleStringStatic("OK"))
}

static LAST_READ: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// `eventloop.pipe text`: writes `text` into a new pipe. The pipe's readable
/// callback reads it for `eventloop.last_read`, stops watching, and closes it.
fn pipe(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let text = args.next_arg()?;
    args.done()?;

    let mut fds: [RawFd; 2] = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(RedisError::Str("ERR pipe failed"));
    }
    let [read_fd, write_fd] = fds;
    // SAFETY: this is a command, on the main thread, and the module owns the
    // pipe; the callback removes the descriptor before closing it.
    let added = unsafe {
        event_loop::add(read_fd, EventLoopMask::READABLE, |_ctx, fd, _mask| {
            let mut buffer = [0u8; 1024];
            let read = libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len());
            if let Ok(read) = usize::try_from(read) {
                *LAST_READ.lock().unwrap_or_else(PoisonError::into_inner) = buffer[..read].to_vec();
            }
            let _ = event_loop::remove(fd, EventLoopMask::READABLE);
            libc::close(fd);
        })
    };
    if let Err(error) = added {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Err(error);
    }

    let bytes = text.as_slice();
    let written = unsafe { libc::write(write_fd, bytes.as_ptr().cast(), bytes.len()) };
    unsafe { libc::close(write_fd) };
    if usize::try_from(written) != Ok(bytes.len()) {
        return Err(RedisError::Str("ERR write to the pipe failed"));
    }
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `eventloop.remove fd`: stops watching `fd`, which fails for a descriptor
/// this module did not add.
fn remove(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let fd = RawFd::try_from(args.next_i64()?)?;
    args.done()?;
    // SAFETY: this is a command, on the main thread.
    unsafe { event_loop::remove(fd, EventLoopMask::READABLE) }?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// Creates a pipe, returning its read and write ends.
fn new_pipe() -> Result<(RawFd, RawFd), RedisError> {
    let mut fds: [RawFd; 2] = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(RedisError::Str("ERR pipe failed"));
    }
    Ok((fds[0], fds[1]))
}

/// `eventloop.watch_fifo path`: creates a FIFO at `path` and watches its read
/// end, which stays open and watched until the module unloads, when the SDK
/// stops watching it. Returns the read end.
fn watch_fifo(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let path = std::ffi::CString::new(args.next_arg()?.as_slice().to_vec())?;
    args.done()?;
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
        return Err(RedisError::Str("ERR mkfifo failed"));
    }
    let read_fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if read_fd < 0 {
        return Err(RedisError::Str("ERR opening the FIFO failed"));
    }
    // SAFETY: this is a command, on the main thread, and the module owns the FIFO.
    unsafe {
        event_loop::add(read_fd, EventLoopMask::READABLE, |_, fd, _| {
            let mut buffer = [0u8; 64];
            libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len());
        })
    }?;
    Ok(RedisValue::Integer(read_fd.into()))
}

/// `eventloop.queue_noop`: queues a one-shot that does nothing, which keeps the
/// module from unloading until it runs.
fn queue_noop(_: &Context, _: Vec<RedisString>) -> RedisResult {
    event_loop::add_one_shot(|_| {})?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `eventloop.panic_watch`: watches a pipe with a byte to read whose callback
/// panics without reading it. The SDK stops watching it. Returns the read end.
fn panic_watch(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let (read_fd, write_fd) = new_pipe()?;
    // SAFETY: this is a command, on the main thread, and the module owns the pipe.
    unsafe {
        event_loop::add(read_fd, EventLoopMask::READABLE, |_, _, _| {
            panic!("readable callback failed before reading");
        })
    }?;
    unsafe {
        libc::write(write_fd, b"x".as_ptr().cast(), 1);
        libc::close(write_fd);
    }
    Ok(RedisValue::Integer(read_fd.into()))
}

/// Stops watching a descriptor when dropped.
struct RemoveOnDrop(RawFd);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        // SAFETY: the callback holding this is dropped on the main thread.
        let _ = unsafe { event_loop::remove(self.0, EventLoopMask::READABLE) };
    }
}

/// `eventloop.replace_with_dropper`: watches pipe A with a callback that owns a
/// value whose destructor stops watching pipe B, then replaces that callback,
/// so the destructor runs inside `event_loop::add`. Returns 1 when B was
/// removed by it.
fn replace_with_dropper(_: &Context, _: Vec<RedisString>) -> RedisResult {
    let (a_read, a_write) = new_pipe()?;
    let (b_read, b_write) = new_pipe()?;
    let remover = RemoveOnDrop(b_read);
    // SAFETY: this is a command, on the main thread, and the module owns the pipes.
    unsafe {
        event_loop::add(b_read, EventLoopMask::READABLE, |_, _, _| {})?;
        event_loop::add(a_read, EventLoopMask::READABLE, move |_, _, _| {
            let _ = &remover;
        })?;
        event_loop::add(a_read, EventLoopMask::READABLE, |_, _, _| {})?;
    }
    let removed = unsafe { event_loop::remove(b_read, EventLoopMask::READABLE) }.is_err();
    unsafe {
        event_loop::remove(a_read, EventLoopMask::READABLE)?;
        for fd in [a_read, a_write, b_read, b_write] {
            libc::close(fd);
        }
    }
    Ok(RedisValue::Integer(removed.into()))
}

/// What `add_one_shot` returned when `init` called it.
static INIT_ONE_SHOT: Mutex<String> = Mutex::new(String::new());

fn init(_: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    let result = match event_loop::add_one_shot(|_| {}) {
        Ok(()) => "OK".to_owned(),
        Err(error) => error.to_string(),
    };
    *INIT_ONE_SHOT.lock().unwrap_or_else(PoisonError::into_inner) = result;
    Status::Ok
}

/// `eventloop.init_one_shot`: what `add_one_shot` returned during `init`.
fn init_one_shot(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::SimpleString(
        INIT_ONE_SHOT
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    ))
}

/// `eventloop.last_read`: what the last pipe callback read.
fn last_read(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::StringBuffer(
        LAST_READ
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    ))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "event_loop",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    commands: [
        ["eventloop.set_later", set_later, "", 0, 0, 0, ""],
        ["eventloop.pipe", pipe, "", 0, 0, 0, ""],
        ["eventloop.last_read", last_read, "readonly", 0, 0, 0, ""],
        ["eventloop.remove", remove, "", 0, 0, 0, ""],
        ["eventloop.watch_fifo", watch_fifo, "", 0, 0, 0, ""],
        ["eventloop.queue_noop", queue_noop, "", 0, 0, 0, ""],
        ["eventloop.panic_watch", panic_watch, "", 0, 0, 0, ""],
        ["eventloop.replace_with_dropper", replace_with_dropper, "", 0, 0, 0, ""],
        ["eventloop.init_one_shot", init_one_shot, "readonly", 0, 0, 0, ""],
    ],
}
