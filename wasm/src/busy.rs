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

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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

/// How many functions are running: one, or more when another module's
/// callback calls a function from inside a running one. Functions run on the
/// main thread; another client's `WASM.CALL` while one yields gets `-BUSY`.
static RUNNING: AtomicUsize = AtomicUsize::new(0);
/// Set by `WASM.KILL`, acted on at the next epoch.
static KILL: AtomicBool = AtomicBool::new(false);
/// Set when the running function calls a write command; such a function is
/// not killed, so its effects are not left half done.
static WROTE: AtomicBool = AtomicBool::new(false);

/// Marks a function as running until dropped. A nested call keeps the outer
/// call's kill and write state.
pub(crate) struct Running(());

/// The outermost running function and when it started, for `WASM.STATS`.
/// Functions run on the main thread, where `WASM.STATS` runs too.
static CURRENT: Mutex<Option<(String, Instant)>> = Mutex::new(None);

impl Running {
    /// Marks guest code as running, such as a library listing its functions.
    pub(crate) fn start() -> Self {
        if RUNNING.fetch_add(1, Ordering::SeqCst) == 0 {
            KILL.store(false, Ordering::SeqCst);
            WROTE.store(false, Ordering::SeqCst);
        }
        Self(())
    }

    /// Marks function `name` as running.
    pub(crate) fn start_function(name: &str) -> Self {
        let running = Self::start();
        if RUNNING.load(Ordering::SeqCst) == 1 {
            *current() = Some((name.to_owned(), Instant::now()));
        }
        running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if RUNNING.fetch_sub(1, Ordering::SeqCst) == 1 {
            KILL.store(false, Ordering::SeqCst);
            *current() = None;
        }
    }
}

fn current() -> std::sync::MutexGuard<'static, Option<(String, Instant)>> {
    CURRENT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The running function, if any, and how long it has run.
pub(crate) fn running_function() -> Option<(String, Duration)> {
    current()
        .as_ref()
        .map(|(name, started)| (name.clone(), started.elapsed()))
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
    if RUNNING.load(Ordering::SeqCst) == 0 {
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

/// What a command a guest calls may do.
#[derive(Clone, Copy, Default)]
pub(crate) struct Effects {
    /// It is a write command, from `COMMAND INFO`.
    pub(crate) write: bool,
    /// It may replicate something without being a write command.
    pub(crate) may_replicate: bool,
}

/// The commands flagged `may_replicate` that script mode allows. `COMMAND
/// INFO` hides that flag and no module API returns a command's flags, so they
/// are named here, from Redis's command table; `EVAL`, `EVALSHA` and `FCALL`,
/// the others, cannot run from a script.
const MAY_REPLICATE: [&str; 3] = ["publish", "spublish", "pfcount"];

/// Looks up what `command` with `args` may do, and records that the running
/// function has written if it is a write command.
pub(crate) fn note_call(ctx: &Context, command: &str, args: &[&[u8]]) -> Effects {
    let effects = effects(ctx, command, args.first().copied());
    if effects.write {
        WROTE.store(true, Ordering::SeqCst);
    }
    effects
}

/// What `COMMAND INFO` says about a command or subcommand.
#[derive(Clone, Copy)]
struct CommandInfo {
    write: bool,
    has_subcommands: bool,
}

thread_local! {
    /// `COMMAND INFO` results by name as the guest spelled it, such as `SET`
    /// or `config|set`. A first argument is looked up only for commands with
    /// subcommands, so the cache grows with the spellings guests use. Guests
    /// run on the main thread, so a thread-local cache needs no lock; each
    /// call looks a command up here without allocating.
    static COMMANDS: RefCell<HashMap<String, Option<CommandInfo>>> = RefCell::new(HashMap::new());
}

fn effects(ctx: &Context, command: &str, first_arg: Option<&[u8]>) -> Effects {
    let may_replicate = MAY_REPLICATE
        .iter()
        .any(|name| command.eq_ignore_ascii_case(name));
    let Some(info) = command_info(ctx, command) else {
        return Effects::default();
    };
    if info.has_subcommands {
        if let Some(sub) = first_arg.and_then(|arg| std::str::from_utf8(arg).ok()) {
            if let Some(sub_info) = command_info(ctx, &format!("{command}|{sub}")) {
                return Effects {
                    write: sub_info.write,
                    may_replicate,
                };
            }
        }
    }
    Effects {
        write: info.write,
        may_replicate,
    }
}

fn command_info(ctx: &Context, name: &str) -> Option<CommandInfo> {
    if let Some(info) = COMMANDS.with_borrow(|commands| commands.get(name).copied()) {
        return info;
    }
    // COMMAND INFO ignores case.
    let info = parse_command_info(ctx.call("COMMAND", &["INFO", name]).ok()?);
    COMMANDS.with_borrow_mut(|commands| commands.insert(name.to_owned(), info));
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
