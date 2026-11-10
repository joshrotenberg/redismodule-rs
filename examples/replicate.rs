//! Replication from code that keeps running while Redis pauses writes: a
//! read-only command, a timer, a server event and a worker thread.
//! `CLIENT PAUSE ... WRITE`, `FAILOVER` and `CLUSTER FAILOVER` pause
//! replication, and Redis aborts if a module replicates during the pause. The
//! SDK refuses instead: `replicate` and `replicate_verbatim` return
//! `Status::Err`, and a call with a replicate flag returns an error without
//! running the command. Check `avoid_replication_traffic` before changing data
//! you would replicate.
//!
//! The `replicate.command`, `replicate.verbatim` and `replicate.call` commands
//! are flagged `readonly` on purpose, so they run during a write pause; a
//! module's own commands that replicate should be flagged `write` or
//! `may-replicate`. Redis postpones those during a write pause, and in a
//! transaction that starts the pause itself the SDK lets them replicate, as
//! `replicate.write` shows.
//!
//! The other order, replicating and then pausing, aborts Redis too: it
//! propagates the queued replication after the command returns, during the
//! pause. `replicate.then_call` queues replication and then makes a call;
//! outside a transaction the SDK refuses a `CLIENT PAUSE` or `FAILOVER` call,
//! which would start a pause.
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

/// `replicate.call_to replicas|aof command [arg ...]`: like `replicate.call`,
/// replicating only to the replicas or only to the AOF.
fn replicate_call_to(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let target = args.next_string()?;
    let builder = match target.as_str() {
        "replicas" => CallOptionsBuilder::new().replicate_to_replicas_only(),
        "aof" => CallOptionsBuilder::new().replicate_to_aof_only(),
        _ => return Err(RedisError::Str("ERR expected replicas or aof")),
    };
    let command = args.next_string()?;
    let args: Vec<RedisString> = args.collect();
    let args: Vec<&RedisString> = args.iter().collect();
    let result: CallResult = ctx.call_ext(&command, &builder.build(), args.as_slice());
    ctx.reply_with_call_reply(&result)?;
    Ok(RedisValue::NoReply)
}

/// `replicate.write verbatim|command|call key value`: a `write` command that
/// sets the key and replicates the change with `replicate_verbatim`, with
/// `replicate` or through a replicated call. Replies 1 if Redis accepted the
/// replication or 0 if not.
fn replicate_write(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut parsed = args.iter().skip(1);
    let how = parsed.next().ok_or(RedisError::WrongArity)?.try_as_str()?;
    let key = parsed.next().ok_or(RedisError::WrongArity)?;
    let value = parsed.next().ok_or(RedisError::WrongArity)?;
    if parsed.next().is_some() {
        return Err(RedisError::WrongArity);
    }
    let set = [key, value];
    let status = match how {
        "verbatim" => {
            ctx.call("SET", set.as_slice())?;
            ctx.replicate_verbatim()
        }
        "command" => {
            ctx.call("SET", set.as_slice())?;
            ctx.replicate("SET", set.as_slice())
        }
        "call" => {
            let options = CallOptionsBuilder::new().replicate().build();
            match ctx.call_ext::<_, CallResult>("SET", &options, set.as_slice()) {
                Ok(_) => Status::Ok,
                Err(_) => Status::Err,
            }
        }
        _ => return Err(RedisError::Str("ERR expected verbatim, command or call")),
    };
    Ok(replicated(status).into())
}

/// `replicate.forward command [arg ...]`: a read-only command that calls
/// another command without the replicate flag and passes its reply on. Redis
/// drops what the called command replicates.
fn replicate_forward(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let (command, args) = args[1..].split_first().ok_or(RedisError::WrongArity)?;
    let args: Vec<&RedisString> = args.iter().collect();
    ctx.call(command.try_as_str()?, args.as_slice())
}

/// `replicate.then_call verbatim|call|open|none key command [arg ...]`: queues
/// replication, then runs the command through `call` and passes its reply on.
/// `verbatim` replicates this command with `replicate_verbatim`, `call` sets
/// the key through a replicated `call_ext`, `open` opens the key, which
/// deletes it and queues a `DEL` if it expired, and `none` queues nothing.
/// Redis propagates what was queued after the command returns.
fn replicate_then_call(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let how = args.next_string()?;
    let key = args.next_arg()?;
    let command = args.next_string()?;
    let args: Vec<RedisString> = args.collect();
    match how.as_str() {
        "verbatim" => {
            if ctx.replicate_verbatim() == Status::Err {
                return Err(RedisError::Str("ERR replicate_verbatim was refused"));
            }
        }
        "call" => {
            let options = CallOptionsBuilder::new().replicate().build();
            let value = ctx.create_string("value");
            let set: CallResult = ctx.call_ext("SET", &options, &[&key, &value]);
            set?;
        }
        "open" => drop(ctx.open_key(&key)),
        "none" => {}
        _ => {
            return Err(RedisError::Str("ERR expected verbatim, call, open or none"));
        }
    }
    let args: Vec<&RedisString> = args.iter().collect();
    ctx.call(&command, args.as_slice())
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
        ["replicate.call_to", replicate_call_to, "readonly", 0, 0, 0, ""],
        ["replicate.write", replicate_write, "write", 2, 2, 1, ""],
        ["replicate.forward", replicate_forward, "readonly", 0, 0, 0, ""],
        ["replicate.then_call", replicate_then_call, "readonly", 0, 0, 0, ""],
        ["replicate.timer", replicate_timer, "readonly", 0, 0, 0, ""],
        ["replicate.event", replicate_event, "readonly", 0, 0, 0, ""],
        ["replicate.worker", replicate_worker, "readonly", 0, 0, 0, ""],
        ["replicate.result", replicate_result, "readonly", 0, 0, 0, ""],
        ["replicate.paused", replicate_paused, "readonly", 0, 0, 0, ""],
    ],
}
