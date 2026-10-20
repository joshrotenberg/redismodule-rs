//! Long-running functions: yielding to Redis while a guest runs, `-BUSY`
//! replies for other clients, and `WASM.KILL`, as Redis does for a long Lua
//! script and `FUNCTION KILL`.
//!
//! A ticker thread advances the engine's epoch. A guest's store has a deadline
//! one epoch ahead; when it passes, wasmtime calls [`on_epoch`], which lets Redis
//! process events through `RedisModule_Yield` and extends the deadline. Past
//! `busy-reply-threshold`, Redis answers other clients with `-BUSY` and runs
//! `allow-busy` commands such as `WASM.KILL`, which sets a flag the next epoch
//! acts on.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::bail;
use redis_module::{Context, RedisValue, YieldMode};
use wasmtime::{Engine, UpdateDeadline};

/// How often the ticker advances the epoch, so how often a running guest
/// yields.
const TICK: Duration = Duration::from_millis(10);

/// The text after `-BUSY` that other clients get while a function runs.
const BUSY_REPLY: &str =
    "a wasm function is running. You can only call WASM.KILL or SHUTDOWN NOSAVE.";

/// The error a killed function fails with.
const KILLED: &str = "killed by WASM.KILL";

/// The function running now. Functions run on the main thread, one at a time:
/// another `WASM.CALL` while one yields gets `-BUSY`.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// Set by `WASM.KILL`, acted on at the next epoch.
static KILL: AtomicBool = AtomicBool::new(false);
/// Set when the running function calls a write command; such a function is
/// not killed, so its effects are not left half done.
static WROTE: AtomicBool = AtomicBool::new(false);

/// Marks a function as running until dropped.
pub(crate) struct Running(());

impl Running {
    pub(crate) fn start() -> Self {
        KILL.store(false, Ordering::SeqCst);
        WROTE.store(false, Ordering::SeqCst);
        RUNNING.store(true, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
        KILL.store(false, Ordering::SeqCst);
    }
}

/// What `WASM.KILL` found.
pub(crate) enum Kill {
    /// The running function will stop at its next epoch.
    Killed,
    /// No function is running.
    NotBusy,
    /// The running function has called a write command.
    Unkillable,
}

pub(crate) fn kill() -> Kill {
    if !RUNNING.load(Ordering::SeqCst) {
        Kill::NotBusy
    } else if WROTE.load(Ordering::SeqCst) {
        Kill::Unkillable
    } else {
        KILL.store(true, Ordering::SeqCst);
        Kill::Killed
    }
}

/// Called by wasmtime when a running guest reaches its epoch deadline.
pub(crate) fn on_epoch(ctx: &Context) -> anyhow::Result<UpdateDeadline> {
    if !KILL.load(Ordering::SeqCst) {
        // A Redis without RedisModule_Yield (before 7.0) cannot process
        // events here; the guest runs on.
        let _ = ctx.yield_now(YieldMode::Clients, Some(BUSY_REPLY));
    }
    // WASM.KILL runs inside the yield, so check again after it.
    if KILL.load(Ordering::SeqCst) {
        bail!(KILLED);
    }
    Ok(UpdateDeadline::Continue(1))
}

/// Records that the running function is about to call `command` with `args`,
/// if it is a write command.
pub(crate) fn note_call(ctx: &Context, command: &str, args: &[&[u8]]) {
    if is_write(ctx, command, args.first().copied()) {
        WROTE.store(true, Ordering::SeqCst);
    }
}

/// What `COMMAND INFO` says about a command or subcommand.
#[derive(Clone, Copy)]
struct CommandInfo {
    write: bool,
    has_subcommands: bool,
}

/// `COMMAND INFO` results by lowercase name, such as `set` or `config|set`.
/// A first argument is looked up only for commands with subcommands, so the
/// cache grows with the command and subcommand names guests use.
static COMMANDS: Mutex<BTreeMap<String, Option<CommandInfo>>> = Mutex::new(BTreeMap::new());

fn is_write(ctx: &Context, command: &str, first_arg: Option<&[u8]>) -> bool {
    let name = command.to_ascii_lowercase();
    let Some(info) = command_info(ctx, &name) else {
        return false;
    };
    if info.has_subcommands {
        if let Some(sub) = first_arg.and_then(|arg| std::str::from_utf8(arg).ok()) {
            if let Some(sub_info) =
                command_info(ctx, &format!("{name}|{}", sub.to_ascii_lowercase()))
            {
                return sub_info.write;
            }
        }
    }
    info.write
}

fn command_info(ctx: &Context, name: &str) -> Option<CommandInfo> {
    let commands = || COMMANDS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(info) = commands().get(name) {
        return *info;
    }
    let info = parse_command_info(ctx.call("COMMAND", &["INFO", name]).ok()?);
    commands().insert(name.to_owned(), info);
    info
}

/// The first entry of a `COMMAND INFO` reply: `[name, arity, flags, first key,
/// last key, step, ACL categories, tips, key specs, subcommands]`, or nil for
/// an unknown command.
fn parse_command_info(reply: RedisValue) -> Option<CommandInfo> {
    let RedisValue::Array(entries) = reply else {
        return None;
    };
    let RedisValue::Array(fields) = entries.into_iter().next()? else {
        return None;
    };
    let write = match fields.get(2) {
        Some(RedisValue::Array(flags)) => flags.iter().any(|flag| match flag {
            RedisValue::SimpleString(text) | RedisValue::BulkString(text) => text == "write",
            _ => false,
        }),
        _ => false,
    };
    let has_subcommands =
        matches!(fields.get(9), Some(RedisValue::Array(subs)) if !subs.is_empty());
    Some(CommandInfo {
        write,
        has_subcommands,
    })
}

/// The thread that advances the engine's epoch.
pub(crate) struct Ticker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Ticker {
    pub(crate) fn start(engine: Engine) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("wasm-epoch".into())
            .spawn({
                let stop = Arc::clone(&stop);
                move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(TICK);
                        engine.increment_epoch();
                    }
                }
            })?;
        Ok(Self { stop, thread })
    }

    /// Stops the thread and waits for it, so the module can be unloaded.
    pub(crate) fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(flags: &[&str], subcommands: usize) -> RedisValue {
        RedisValue::Array(vec![RedisValue::Array(vec![
            RedisValue::BulkString("x".into()),
            RedisValue::Integer(-1),
            RedisValue::Array(
                flags
                    .iter()
                    .map(|f| RedisValue::SimpleString((*f).into()))
                    .collect(),
            ),
            RedisValue::Integer(0),
            RedisValue::Integer(0),
            RedisValue::Integer(0),
            RedisValue::Array(vec![]),
            RedisValue::Array(vec![]),
            RedisValue::Array(vec![]),
            RedisValue::Array(vec![RedisValue::Null; subcommands]),
        ])])
    }

    #[test]
    fn command_info_reads_the_write_flag_and_subcommands() {
        let set = parse_command_info(info(&["write", "denyoom"], 0)).unwrap();
        assert!(set.write && !set.has_subcommands);
        let get = parse_command_info(info(&["readonly", "fast"], 0)).unwrap();
        assert!(!get.write);
        let config = parse_command_info(info(&[], 4)).unwrap();
        assert!(config.has_subcommands);
        assert!(parse_command_info(RedisValue::Array(vec![RedisValue::Null])).is_none());
    }
}
