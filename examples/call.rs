use redis_module::{
    BlockedClient, CallOptionResp, CallOptionsBuilder, CallReply, CallResult, Context,
    EventContext, FutureCallReply, FutureHandler, PromiseCallReply, RedisError, RedisResult,
    RedisString, RedisValue, Status, ThreadSafeContext, raw, redis_module,
};

use std::cell::RefCell;
use std::sync::atomic::{AtomicI64, Ordering};
use std::thread;

fn call_test(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let res: String = ctx.call("ECHO", &["TEST"])?.try_into()?;
    if "TEST" != &res {
        return Err(RedisError::Str("Failed calling 'ECHO TEST'"));
    }

    let res: String = ctx.call("ECHO", vec!["TEST"].as_slice())?.try_into()?;
    if "TEST" != &res {
        return Err(RedisError::Str(
            "Failed calling 'ECHO TEST' dynamic str vec",
        ));
    }

    let res: String = ctx.call("ECHO", &[b"TEST"])?.try_into()?;
    if "TEST" != &res {
        return Err(RedisError::Str(
            "Failed calling 'ECHO TEST' with static [u8]",
        ));
    }

    let res: String = ctx.call("ECHO", vec![b"TEST"].as_slice())?.try_into()?;
    if "TEST" != &res {
        return Err(RedisError::Str(
            "Failed calling 'ECHO TEST' dynamic &[u8] vec",
        ));
    }

    let res: String = ctx.call("ECHO", &[&"TEST".to_string()])?.try_into()?;
    if "TEST" != &res {
        return Err(RedisError::Str("Failed calling 'ECHO TEST' with String"));
    }

    let res: String = ctx
        .call("ECHO", vec![&"TEST".to_string()].as_slice())?
        .try_into()?;
    if "TEST" != &res {
        return Err(RedisError::Str(
            "Failed calling 'ECHO TEST' dynamic &[u8] vec",
        ));
    }

    let res: String = ctx
        .call("ECHO", &[&ctx.create_string("TEST")])?
        .try_into()?;
    if "TEST" != &res {
        return Err(RedisError::Str(
            "Failed calling 'ECHO TEST' with RedisString",
        ));
    }

    let res: String = ctx
        .call("ECHO", vec![&ctx.create_string("TEST")].as_slice())?
        .try_into()?;
    if "TEST" != &res {
        return Err(RedisError::Str(
            "Failed calling 'ECHO TEST' with dynamic array of RedisString",
        ));
    }

    let call_options = CallOptionsBuilder::new().script_mode().errors_as_replies();
    let res: CallResult = ctx.call_ext::<&[&str; 0], _>("SHUTDOWN", &call_options.build(), &[]);
    if let Err(err) = res {
        let error_msg = err.to_utf8_string().unwrap();
        if !error_msg.contains("not allow") {
            return Err(RedisError::String(format!(
                "Failed to verify error messages, expected error message to contain 'not allow', error message: '{error_msg}'",
            )));
        }
    } else {
        return Err(RedisError::Str("Failed to set script mode on call_ext"));
    }

    // test resp3 on call_ext
    let call_options = CallOptionsBuilder::new()
        .script_mode()
        .resp(CallOptionResp::Resp3)
        .errors_as_replies()
        .build();
    ctx.call_ext::<_, CallResult>("HSET", &call_options, &["x", "foo", "bar"])
        .map_err(|e| -> RedisError { e.into() })?;
    let res: CallReply = ctx
        .call_ext::<_, CallResult>("HGETALL", &call_options, &["x"])
        .map_err(|e| -> RedisError { e.into() })?;
    if let CallReply::Map(map) = res {
        let res = map.iter().fold(Vec::new(), |mut vec, (key, val)| {
            if let CallReply::String(key) = key.unwrap() {
                vec.push(key.to_string().unwrap());
            }
            if let CallReply::String(val) = val.unwrap() {
                vec.push(val.to_string().unwrap());
            }
            vec
        });
        if res != vec!["foo".to_string(), "bar".to_string()] {
            return Err(RedisError::String(
                "Reply of hgetall does not match expected value".into(),
            ));
        }
    } else {
        return Err(RedisError::String(
            "Did not get a set type on hgetall".into(),
        ));
    }

    Ok("pass".into())
}

fn call_blocking_internal(ctx: &redis_module::EventContext) -> PromiseCallReply<'_, '_> {
    let call_options = CallOptionsBuilder::new().build_blocking();
    ctx.call_blocking("blpop", &call_options, &["list", "1"])
}

fn call_blocking_handle_future(
    ctx: &redis_module::RuntimeContext,
    f: FutureCallReply,
    blocked_client: BlockedClient,
) {
    let future_handler = f.set_unblock_handler(move |_ctx, reply| {
        let thread_ctx = ThreadSafeContext::with_blocked_client(blocked_client);
        thread_ctx.reply(reply.map_or_else(|e| Err(e.into()), |v| Ok((&v).into())));
    });
    future_handler.dispose(ctx);
}

fn call_blocking(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let res = call_blocking_internal(ctx);
    match res {
        PromiseCallReply::Resolved(r) => r.map_or_else(|e| Err(e.into()), |v| Ok((&v).into())),
        PromiseCallReply::Future(f) => {
            let blocked_client = ctx.block_client()?;
            call_blocking_handle_future(ctx, f, blocked_client);
            Ok(RedisValue::NoReply)
        }
    }
}

fn call_blocking_from_detach_ctx(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let blocked_client = ctx.block_client()?;
    thread::spawn(move || {
        let ctx_guard = redis_module::MODULE_CONTEXT.lock();
        let res = ctx_guard.call_blocking(
            "blpop",
            &CallOptionsBuilder::new().build_blocking(),
            &["list", "1"],
        );
        match res {
            PromiseCallReply::Resolved(r) => {
                let thread_ctx = ThreadSafeContext::with_blocked_client(blocked_client);
                thread_ctx.reply(r.map_or_else(|e| Err(e.into()), |v| Ok((&v).into())));
            }
            PromiseCallReply::Future(f) => {
                call_blocking_handle_future(&ctx_guard, f, blocked_client);
            }
        }
    });
    Ok(RedisValue::NoReply)
}

// Unblock handlers that ran, and captures that were dropped, after an abort.
static ABORTED_HANDLER_CALLS: AtomicI64 = AtomicI64::new(0);
static ABORTED_CAPTURE_DROPS: AtomicI64 = AtomicI64::new(0);

struct DropCounter(String);

impl Drop for DropCounter {
    fn drop(&mut self) {
        ABORTED_CAPTURE_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

/// `call.blocking_abort <key> empty|capture` starts `BLPOP <key> 0`, installs an
/// unblock handler and aborts the pending call. An `empty` handler captures
/// nothing, so it is zero-sized; a `capture` handler owns a [DropCounter].
fn call_blocking_abort(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let [_, key, kind] = args.as_slice() else {
        return Err(RedisError::WrongArity);
    };
    let options = CallOptionsBuilder::new().build_blocking();
    let future = match ctx.call_blocking("blpop", &options, &[key.try_as_str()?, "0"]) {
        PromiseCallReply::Future(future) => future,
        PromiseCallReply::Resolved(_) => return Err(RedisError::Str("ERR the call did not block")),
    };
    let status = match kind.try_as_str()? {
        "empty" => future
            .set_unblock_handler(|_ctx, _reply| {
                ABORTED_HANDLER_CALLS.fetch_add(1, Ordering::SeqCst);
            })
            .abort_and_dispose(ctx),
        "capture" => {
            let capture = DropCounter(key.to_string());
            future
                .set_unblock_handler(move |_ctx, _reply| {
                    let _owned = &capture.0;
                    ABORTED_HANDLER_CALLS.fetch_add(1, Ordering::SeqCst);
                })
                .abort_and_dispose(ctx)
        }
        _ => return Err(RedisError::Str("ERR expected empty or capture")),
    };
    Ok(match status {
        Status::Ok => RedisValue::SimpleStringStatic("aborted"),
        Status::Err => RedisValue::SimpleStringStatic("not aborted"),
    })
}

// A function pointer handler, so a kept handle has a nameable type.
type KeptHandler = fn(&EventContext, CallResult<'static>);

static KEPT_HANDLER_CALLS: AtomicI64 = AtomicI64::new(0);

thread_local! {
    // Commands run on the main thread, where the handle must stay.
    static KEPT_HANDLE: RefCell<Option<FutureHandler<KeptHandler>>> = const { RefCell::new(None) };
}

fn count_kept_call(_ctx: &EventContext, _reply: CallResult<'static>) {
    KEPT_HANDLER_CALLS.fetch_add(1, Ordering::SeqCst);
}

/// `call.blocking_keep <key>` starts `BLPOP <key> 0` and keeps its handle
/// after the command returns, for `call.blocking_abort_kept`.
fn call_blocking_keep(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let [_, key] = args.as_slice() else {
        return Err(RedisError::WrongArity);
    };
    let options = CallOptionsBuilder::new().build_blocking();
    let future = match ctx.call_blocking("blpop", &options, &[key.try_as_str()?, "0"]) {
        PromiseCallReply::Future(future) => future,
        PromiseCallReply::Resolved(_) => return Err(RedisError::Str("ERR the call did not block")),
    };
    let handle = future.set_unblock_handler(count_kept_call as KeptHandler);
    if let Some(previous) = KEPT_HANDLE.replace(Some(handle)) {
        previous.dispose(ctx);
    }
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// Aborts the handle `call.blocking_keep` kept, which fails once the call
/// has unblocked and its handler has run.
fn call_blocking_abort_kept(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let handle = KEPT_HANDLE
        .take()
        .ok_or(RedisError::Str("ERR no kept handle"))?;
    Ok(match handle.abort_and_dispose(ctx) {
        Status::Ok => RedisValue::SimpleStringStatic("aborted"),
        Status::Err => RedisValue::SimpleStringStatic("not aborted"),
    })
}

/// Replies with the aborted handler calls, capture drops and kept handler
/// calls since the module loaded.
fn call_blocking_abort_stats(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(vec![
        ABORTED_HANDLER_CALLS.load(Ordering::SeqCst).into(),
        ABORTED_CAPTURE_DROPS.load(Ordering::SeqCst).into(),
        KEPT_HANDLER_CALLS.load(Ordering::SeqCst).into(),
    ]))
}

fn call_dump_test(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    // Serialized bytes need not share the input's UTF-8 classification.
    for input in [b"test_value".as_slice(), &[0xff, 0xfe, 0x80, 0x81]] {
        ctx.call("SET", &[b"test_dump_key".as_slice(), input])?;
        let converted = match ctx.call("DUMP", &["test_dump_key"])? {
            RedisValue::BulkString(text) => text.into_bytes(),
            RedisValue::StringBuffer(bytes) => bytes,
            _ => return Err(RedisError::Str("DUMP returned unexpected type")),
        };
        let native: CallResult = ctx.call_ext(
            "DUMP",
            &CallOptionsBuilder::new().build(),
            &["test_dump_key"],
        );
        match native.map_err(RedisError::from)? {
            CallReply::String(reply) if reply.as_bytes() == converted => {}
            _ => return Err(RedisError::Str("DUMP conversion changed serialized bytes")),
        }
        ctx.call("DEL", &["test_dump_key"])?;
    }
    Ok("pass".into())
}

fn call_binary_data_test(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    // Test GET with invalid UTF-8 value
    let invalid_utf8: &[u8] = &[0xff, 0xfe, 0x80, 0x81];
    ctx.call("SET", &[&b"test_binary_key"[..], invalid_utf8])?;
    let result = ctx.call("GET", &["test_binary_key"])?;

    match result {
        RedisValue::StringBuffer(data) => {
            if data != invalid_utf8 {
                return Err(RedisError::Str("GET returned wrong binary data"));
            }
        }
        _ => {
            return Err(RedisError::Str(
                "GET with invalid UTF-8 should return StringBuffer",
            ));
        }
    }
    ctx.call("DEL", &["test_binary_key"])?;

    Ok("pass".into())
}

/// `call.verbatim`: reads the RESP3 verbatim reply of `INFO server` through
/// `VerbatimStringCallReply::as_parts` and `raw::call_reply_verbatim_string`.
fn call_verbatim(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    let call_options = CallOptionsBuilder::new()
        .resp(CallOptionResp::Resp3)
        .errors_as_replies()
        .build();
    let res: CallReply = ctx
        .call_ext::<_, CallResult>("INFO", &call_options, &["server"])
        .map_err(|e| -> RedisError { e.into() })?;
    let CallReply::VerbatimString(reply) = res else {
        return Err(RedisError::Str(
            "ERR INFO did not reply with a verbatim string",
        ));
    };
    let (format, data) = reply
        .as_parts()
        .ok_or(RedisError::Str("ERR as_parts returned None"))?;
    // SAFETY: the reply is alive for this call.
    let (raw_format, raw_data) = unsafe { raw::call_reply_verbatim_string(reply.get_raw()) }
        .ok_or(RedisError::Str(
            "ERR call_reply_verbatim_string returned None",
        ))?;
    Ok(RedisValue::Array(vec![
        RedisValue::BulkString(format.to_owned()),
        RedisValue::BulkString(raw_format),
        RedisValue::Integer(i64::from(raw_data == data)),
        RedisValue::Integer(i64::from(data.starts_with(b"# Server"))),
    ]))
}

/// `call.forward command [arg ...]`: runs the command and passes its reply
/// on unchanged.
fn call_forward(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let (command, args) = args[1..].split_first().ok_or(RedisError::WrongArity)?;
    let args: Vec<&RedisString> = args.iter().collect();
    let options = CallOptionsBuilder::new().errors_as_replies().build();
    let result: CallResult = ctx.call_ext(command.try_as_str()?, &options, args.as_slice());
    ctx.reply_with_call_reply(&result)?;
    Ok(RedisValue::NoReply)
}

/// `call.forward3 command [arg ...]`: like `call.forward`, with RESP3 replies.
fn call_forward3(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let (command, args) = args[1..].split_first().ok_or(RedisError::WrongArity)?;
    let args: Vec<&RedisString> = args.iter().collect();
    let options = CallOptionsBuilder::new()
        .resp(CallOptionResp::Resp3)
        .errors_as_replies()
        .build();
    let result: CallResult = ctx.call_ext(command.try_as_str()?, &options, args.as_slice());
    ctx.reply_with_call_reply(&result)?;
    Ok(RedisValue::NoReply)
}

/// `call.convert command [arg ...]`: runs the command with `Context::call` and
/// replies with the converted value.
fn call_convert(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let (command, args) = args[1..].split_first().ok_or(RedisError::WrongArity)?;
    let args: Vec<&RedisString> = args.iter().collect();
    ctx.call(command.try_as_str()?, args.as_slice())
}

/// `call.dry_run command [arg ...]`: checks the command without running it.
fn call_dry_run(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let (command, args) = args[1..].split_first().ok_or(RedisError::WrongArity)?;
    let args: Vec<&RedisString> = args.iter().collect();
    let options = CallOptionsBuilder::new()
        .dry_run()
        .errors_as_replies()
        .build();
    match ctx.call_ext::<_, CallResult>(command.try_as_str()?, &options, args.as_slice()) {
        Ok(CallReply::Unknown) => Ok(RedisValue::SimpleStringStatic("would run")),
        Ok(reply) => Err(RedisError::String(format!(
            "ERR dry run returned a reply: {reply}"
        ))),
        Err(error) => Err(error.into()),
    }
}

//////////////////////////////////////////////////////

redis_module! {
    name: "call",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["call.test", call_test, "", 0, 0, 0, ""],
        ["call.blocking", call_blocking, "", 0, 0, 0, ""],
        ["call.blocking_from_detached_ctx", call_blocking_from_detach_ctx, "", 0, 0, 0, ""],
        ["call.blocking_abort", call_blocking_abort, "", 0, 0, 0, ""],
        ["call.blocking_keep", call_blocking_keep, "", 0, 0, 0, ""],
        ["call.blocking_abort_kept", call_blocking_abort_kept, "", 0, 0, 0, ""],
        ["call.blocking_abort_stats", call_blocking_abort_stats, "", 0, 0, 0, ""],
        ["call.dump_test", call_dump_test, "", 0, 0, 0, ""],
        ["call.binary_data_test", call_binary_data_test, "", 0, 0, 0, ""],
        ["call.verbatim", call_verbatim, "", 0, 0, 0, ""],
        ["call.forward", call_forward, "", 0, 0, 0, ""],
        ["call.forward3", call_forward3, "", 0, 0, 0, ""],
        ["call.convert", call_convert, "", 0, 0, 0, ""],
        ["call.dry_run", call_dry_run, "", 0, 0, 0, ""],
    ],
}
