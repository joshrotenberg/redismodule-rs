//! Replication from code that keeps running while Redis pauses writes: a
//! read-only command, a timer, a server event and a worker thread.
//! `CLIENT PAUSE ... WRITE`,
//! `FAILOVER` and `CLUSTER FAILOVER` pause replication, and Redis aborts if a
//! module replicates during the pause. The SDK refuses instead: `replicate`
//! and `replicate_verbatim` return `Status::Err`, and a call with a replicate
//! flag returns an error without running the command. Check
//! `avoid_replication_traffic` before changing data you would replicate.
use redis_module::{
    CallOptionsBuilder, CallResult, Context, EventContext, NextArg, RedisError, RedisGILGuard,
    RedisResult, RedisString, RedisValue, Status, ThreadSafeContext, redis_module,
};
use redis_module_macros::cron_event_handler;
use std::sync::atomic::{AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

/// A timer, event or worker that has not replicated yet.
const PENDING: i64 = -1;

/// A command name and its arguments.
type OwnedCommand = (String, Vec<Vec<u8>>);

static TIMER_RESULT: AtomicI64 = AtomicI64::new(PENDING);
static EVENT_RESULT: AtomicI64 = AtomicI64::new(PENDING);
static WORKER_RESULT: AtomicI64 = AtomicI64::new(PENDING);
/// The command the next cron event replicates.
static EVENT_COMMAND: RedisGILGuard<Option<OwnedCommand>> = RedisGILGuard::new(None);

/// 1 if Redis accepted the replication, 0 if it was refused.
fn replicated(status: Status) -> i64 {
    i64::from(status == Status::Ok)
}

/// The command name and arguments to replicate, copied for a timer, event or
/// worker.
fn owned_command(args: &[RedisString]) -> Result<OwnedCommand, RedisError> {
    let (command, args) = args[1..].split_first().ok_or(RedisError::WrongArity)?;
    Ok((
        command.try_as_str()?.to_owned(),
        args.iter().map(|arg| arg.as_slice().to_vec()).collect(),
    ))
}

/// `replicate.command command [arg ...]`: replicates the command without
/// running it here, and replies 1 if Redis accepted it or 0 if not.
fn replicate_command(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let (command, args) = args[1..].split_first().ok_or(RedisError::WrongArity)?;
    let args: Vec<&RedisString> = args.iter().collect();
    Ok(replicated(ctx.replicate(command.try_as_str()?, args.as_slice())).into())
}

/// `replicate.verbatim [arg ...]`: replicates this command as the client sent
/// it, and replies 1 if Redis accepted it or 0 if not.
fn replicate_verbatim(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(replicated(ctx.replicate_verbatim()).into())
}

/// `replicate.call command [arg ...]`: runs the command and replicates it, and
/// passes its reply on. Redis 6.2 has no `errors_as_replies` flag.
fn replicate_call(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let (command, args) = args[1..].split_first().ok_or(RedisError::WrongArity)?;
    let args: Vec<&RedisString> = args.iter().collect();
    let options = CallOptionsBuilder::new().replicate().build();
    let result: CallResult = ctx.call_ext(command.try_as_str()?, &options, args.as_slice());
    ctx.reply_with_call_reply(&result)?;
    Ok(RedisValue::NoReply)
}

/// `replicate.timer command [arg ...]`: replicates the command from a timer
/// callback. `replicate.result timer` reports the outcome.
fn replicate_timer(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let command = owned_command(&args)?;
    TIMER_RESULT.store(PENDING, Ordering::SeqCst);
    ctx.create_timer(
        Duration::ZERO,
        |ctx: &EventContext, (command, args): OwnedCommand| {
            let args: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
            let status = ctx.replicate(&command, args.as_slice());
            TIMER_RESULT.store(replicated(status), Ordering::SeqCst);
        },
        command,
    )?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

#[cron_event_handler]
fn replicate_on_cron(ctx: &EventContext, _: u64) {
    let Some((command, args)) = EVENT_COMMAND.lock(ctx).take() else {
        return;
    };
    let args: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
    let status = ctx.replicate(&command, args.as_slice());
    EVENT_RESULT.store(replicated(status), Ordering::SeqCst);
}

/// `replicate.event command [arg ...]`: replicates the command from the next
/// cron server event. `replicate.result event` reports the outcome.
fn replicate_event(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let command = owned_command(&args)?;
    EVENT_RESULT.store(PENDING, Ordering::SeqCst);
    *EVENT_COMMAND.lock(ctx) = Some(command);
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `replicate.worker command [arg ...]`: replicates the command from a worker
/// thread holding the Redis lock. `replicate.result worker` reports the outcome.
fn replicate_worker(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let (command, args) = owned_command(&args)?;
    WORKER_RESULT.store(PENDING, Ordering::SeqCst);
    thread::spawn(move || {
        let owner = ThreadSafeContext::new();
        let ctx = owner.lock();
        let args: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
        let status = ctx.replicate(&command, args.as_slice());
        drop(ctx);
        WORKER_RESULT.store(replicated(status), Ordering::SeqCst);
    });
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `replicate.result timer|event|worker`: -1 while pending, then 1 if Redis
/// accepted the replication or 0 if not.
fn replicate_result(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let source = args.next_string()?;
    args.done()?;
    let result = match source.as_str() {
        "timer" => &TIMER_RESULT,
        "event" => &EVENT_RESULT,
        "worker" => &WORKER_RESULT,
        _ => return Err(RedisError::Str("ERR expected timer, event or worker")),
    };
    Ok(result.load(Ordering::SeqCst).into())
}

/// `replicate.paused`: 1 while Redis asks modules to avoid replication.
fn replicate_paused(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(i64::from(ctx.avoid_replication_traffic()).into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "replicate",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["replicate.command", replicate_command, "readonly", 0, 0, 0, ""],
        ["replicate.verbatim", replicate_verbatim, "readonly", 0, 0, 0, ""],
        ["replicate.call", replicate_call, "readonly", 0, 0, 0, ""],
        ["replicate.timer", replicate_timer, "readonly", 0, 0, 0, ""],
        ["replicate.event", replicate_event, "readonly", 0, 0, 0, ""],
        ["replicate.worker", replicate_worker, "readonly", 0, 0, 0, ""],
        ["replicate.result", replicate_result, "readonly", 0, 0, 0, ""],
        ["replicate.paused", replicate_paused, "readonly", 0, 0, 0, ""],
    ],
}
