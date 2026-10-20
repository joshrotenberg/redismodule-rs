//! Commands that use the module event loop API.
use std::os::unix::io::RawFd;
use std::sync::{Mutex, PoisonError};
use std::thread;

use redis_module::event_loop::{self, EventLoopMask};
use redis_module::{
    Context, MODULE_CONTEXT, NextArg, RedisError, RedisResult, RedisString, RedisValue,
    redis_module,
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
    commands: [
        ["eventloop.set_later", set_later, "", 0, 0, 0, ""],
        ["eventloop.pipe", pipe, "", 0, 0, 0, ""],
        ["eventloop.last_read", last_read, "readonly", 0, 0, 0, ""],
        ["eventloop.remove", remove, "", 0, 0, 0, ""],
    ],
}
