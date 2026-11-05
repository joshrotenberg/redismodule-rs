//! A Redis module that runs WebAssembly components as functions, the way
//! `FUNCTION LOAD` and `FCALL` run Lua.
//!
//! A library is a component that implements the `library` world in
//! `wit/redis.wit`: it lists its functions, and runs one by name with its keys
//! and arguments. It reaches Redis through the imports `call` and `log`.
//!
//! ```text
//! WASM.LOAD <library> <component> [REPLACE]
//! WASM.CALL <function> <numkeys> [key ...] [arg ...]
//! WASM.CALL_RO <function> <numkeys> [key ...] [arg ...]
//! WASM.LIST
//! WASM.DELETE <library>
//! WASM.KILL
//! WASM.DUMP
//! WASM.RESTORE <payload> [FLUSH|APPEND|REPLACE]
//! WASM.FLUSH
//! WASM.STATS
//! ```
//!
//! A function that runs past `busy-reply-threshold` yields to Redis, which
//! answers other clients with `-BUSY` and accepts `WASM.KILL`; see `busy.rs`.
//!
//! `WASM.LOAD` and `WASM.DELETE` replicate as they are, and the RDB keeps the
//! libraries; see `persist.rs`.

use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

use anyhow::Context as _;

use redis_module::configuration::ConfigurationFlags;
use redis_module::{
    ArgCursor, Context, ContextFlags, EventContext, ModuleOptions, RedisError, RedisResult,
    RedisString, RedisValue, RegistrationContext, Status, redis_module,
};
use redis_module_macros::command;

mod budget;
mod busy;
mod code;
mod compiler;
mod compiler_engine;
mod compiler_pipe;
mod compiler_process;
mod compiler_protocol;
mod compiler_source;
mod compiler_watch;
mod dump;
mod flags;
mod host;
mod image;
mod memory;
mod persist;
mod reply;

use code::WaitingCall;
use host::Host;
use persist::LIBRARIES;

/// Set when the module loads.
static HOST: Mutex<Option<Host>> = Mutex::new(None);

fn host() -> MutexGuard<'static, Option<Host>> {
    HOST.lock().unwrap_or_else(PoisonError::into_inner)
}

fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Result<R, RedisError> {
    host()
        .as_mut()
        .map(f)
        .ok_or(RedisError::Str("ERR the wasm host is not initialized"))
}

/// Like `with_host`, but `None` if the host is held.
fn try_with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    let mut host = match HOST.try_lock() {
        Ok(host) => host,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return None,
    };
    host.as_mut().map(f)
}

fn error(context: &str, error: &anyhow::Error) -> RedisError {
    RedisError::String(format!("ERR {context}: {error:#}"))
}

/// `WASM.LOAD <library> <component> [REPLACE]`: replies with the library's
/// functions.
///
/// Compiling a large component takes seconds, so the component compiles on a
/// thread while the client waits and the server keeps serving others; the
/// library is added on the main thread once it has compiled. Where the client
/// cannot block, inside `MULTI`, a script or an AOF being loaded, it compiles
/// here instead. On a replica, the link to the primary blocks like any client,
/// so the commands after the load in the replication stream wait for it.
fn wasm_load(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let name = args.next_str()?.to_owned();
    let component = args.next_bytes()?;
    let replace = match args.next() {
        None => false,
        Some(arg) if arg.as_slice().eq_ignore_ascii_case(b"REPLACE") => true,
        Some(_) => return Err(RedisError::Str("ERR the only option is REPLACE")),
    };
    args.done()?;
    let mut compiler = with_host(|host| {
        host.check_name(&name, replace)
            .and_then(|()| host.compiler())
    })?
    .map_err(|e| error("WASM.LOAD failed", &e))?;
    let component =
        compiler_source::Source::spool(component).map_err(|e| error("WASM.LOAD failed", &e))?;

    let Ok(blocked) = ctx.block_client_on_reply(None, finish_load, load_timed_out) else {
        let result = compiler.compile(component);
        drop(compiler);
        return finish_load(
            ctx,
            Loaded {
                name,
                replace,
                blocked: false,
                result,
            },
        );
    };
    // If the thread cannot start, the handle is dropped with it, which
    // unblocks the client with an error reply.
    let supervisor = compiler.supervisor();
    if let Err(error) = supervisor.spawn(move || {
        let result = compiler.compile(component);
        // Release proven compiler capacity before the main-thread callback can
        // reply. A fast client may submit its next load immediately afterward.
        drop(compiler);
        blocked.unblock(Loaded {
            name,
            replace,
            blocked: true,
            result,
        });
    }) {
        // The failed spawn dropped its captured blocked handle, which owns
        // the one eventual error reply. Returning another error would send two.
        ctx.log_warning(&format!("wasm compiler worker did not start: {error:#}"));
    }
    Ok(RedisValue::NoReply)
}

/// A `WASM.LOAD` whose component has compiled, or failed to.
struct Loaded {
    name: String,
    replace: bool,
    /// Whether the load finishes in a blocked client's reply callback.
    blocked: bool,
    result: anyhow::Result<host::Compiled>,
}

/// Refuses to finish a load or restore from a blocked client while Redis pauses
/// replication. A write command cannot start during a pause, but a pause can
/// start while its client is blocked, and the SDK would then refuse to
/// replicate the finished change, which would stay on this server. The inline
/// path (inside `MULTI`, a script or an AOF load) cannot meet a pause that
/// started mid-command. A replica applies what its primary sent regardless.
fn refuse_paused_finish(ctx: &Context, blocked: bool, command: &str) -> Result<(), RedisError> {
    if blocked && ctx.avoid_replication_traffic() && !on_replica(ctx) {
        return Err(RedisError::String(format!(
            "ERR {command} failed: Redis paused replication while the client was blocked; retry after the pause"
        )));
    }
    Ok(())
}

/// Whether this server is a replica, or the command came from its primary.
fn on_replica(ctx: &Context) -> bool {
    ctx.get_flags()
        .intersects(ContextFlags::SLAVE | ContextFlags::REPLICATED)
}

/// Replicates the running command verbatim. The SDK refuses while Redis pauses
/// replication, except for a write command in a transaction that started the
/// pause; a refused change stays on this server, and the log says so. A
/// replica passes its primary's stream on to its own replicas itself, so it
/// does not warn.
fn replicate(ctx: &Context, command: &str) {
    if ctx.replicate_verbatim() == Status::Err && !on_replica(ctx) {
        ctx.log_warning(&format!(
            "{command} was applied here but not replicated: Redis paused replication"
        ));
    }
}

/// Adds a compiled library and replies with its functions, on the main thread.
fn finish_load(ctx: &Context, loaded: Loaded) -> RedisResult {
    let compiled = loaded.result.map_err(|e| error("WASM.LOAD failed", &e))?;
    refuse_paused_finish(ctx, loaded.blocked, "WASM.LOAD")?;
    let functions = with_host(|host| host.add(ctx, &loaded.name, compiled, loaded.replace))?
        .map_err(|e| error("WASM.LOAD failed", &e))?;
    // A blocked client keeps its arguments until it is unblocked, after this.
    replicate(ctx, "WASM.LOAD");
    Ok(RedisValue::Array(
        functions
            .into_iter()
            .map(|function| RedisValue::BulkString(function.name))
            .collect(),
    ))
}

/// `WASM.LOAD` blocks with no timeout, so this does not run.
fn load_timed_out(_ctx: &Context) -> RedisResult {
    Err(RedisError::Str("ERR WASM.LOAD timed out"))
}

/// `WASM.CALL <function> <numkeys> [key ...] [arg ...]`
///
/// Like `FCALL`'s, its key spec reads the keys from `numkeys`, so `COMMAND
/// GETKEYS`, ACL key patterns and cluster routing see them. A function may do
/// anything with a key, so the spec assumes it reads and updates each one.
/// Like `FCALL`, it is not a write command: the function's flags decide where
/// it may run (see `flags.rs`).
#[command(
    {
        name: "wasm.call",
        flags: [MayReplicate, DenyScript, NoMandatoryKeys, AllowStale, GetkeysApi],
        acl_categories: [Scripting],
        arity: -3,
        key_spec: [
            {
                notes: "the function may read and update any of its keys",
                flags: [ReadWrite, Access, Update],
                begin_search: Index({ index : 2 }),
                find_keys: Keynum({ key_num_idx : 0, first_key : 1, key_step : 1 }),
            }
        ]
    }
)]
fn wasm_call(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if ctx.is_keys_position_request() {
        return declare_keys(ctx, &args);
    }
    run_function(ctx, &args, false)
}

/// `WASM.CALL_RO <function> <numkeys> [key ...] [arg ...]`: like `FCALL_RO`,
/// runs only a function with `no-writes`.
#[command(
    {
        name: "wasm.call_ro",
        flags: [ReadOnly, DenyScript, NoMandatoryKeys, AllowStale, GetkeysApi],
        acl_categories: [Scripting],
        arity: -3,
        key_spec: [
            {
                notes: "the function only reads its keys",
                flags: [ReadOnly, Access],
                begin_search: Index({ index : 2 }),
                find_keys: Keynum({ key_num_idx : 0, first_key : 1, key_step : 1 }),
            }
        ]
    }
)]
fn wasm_call_ro(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    if ctx.is_keys_position_request() {
        return declare_keys(ctx, &args);
    }
    run_function(ctx, &args, true)
}

/// Gives Redis the positions of a call's keys, for the `getkeys-api` flag:
/// before Redis 8.4, a cluster routes a module command by these or by its
/// first, last and step, which a numkeys key spec cannot set, and not by its
/// key spec.
fn declare_keys(ctx: &Context, args: &[RedisString]) -> RedisResult {
    let numkeys = args
        .get(2)
        .and_then(|n| n.to_string_lossy().parse::<usize>().ok())
        .unwrap_or(0);
    // A position past the arguments would crash Redis; the SDK also refuses
    // one.
    for pos in 3..args.len().min(3usize.saturating_add(numkeys)) {
        ctx.key_at_pos(i32::try_from(pos).unwrap_or(i32::MAX))?;
    }
    Ok(RedisValue::NoReply)
}

fn run_function(ctx: &Context, args: &[RedisString], read_only: bool) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let function = args.next_str()?.to_owned();
    let numkeys = usize::try_from(args.next_u64()?)
        .ok()
        .filter(|&n| n <= args.remaining().len())
        .ok_or(RedisError::Str(
            "ERR numkeys is larger than the number of arguments",
        ))?;
    let keys: Vec<Vec<u8>> = args.by_ref().take(numkeys).map(|k| k.to_vec()).collect();
    let rest: Vec<Vec<u8>> = args.map(|a| a.to_vec()).collect();
    call_function(
        ctx,
        WaitingCall {
            function,
            keys,
            args: rest,
            read_only,
        },
    )
}

/// Runs a function call, or holds it while the function's library compiles
/// after an RDB load.
fn call_function(ctx: &Context, call: WaitingCall) -> RedisResult {
    let function = call.function.as_str();
    // Release the host before running the guest, so a guest that calls a
    // WASM.* command fails on its command flags, not on a lock.
    let prepared = with_host(|host| host.prepare(function))?
        .ok_or_else(|| RedisError::String(format!("ERR function '{function}' not found")))?;
    let ready = match prepared {
        host::Prepared::Ready(ready) => ready,
        host::Prepared::Failed(message) => {
            return Err(RedisError::String(format!("ERR {message}")));
        }
        host::Prepared::Compiling(code) => {
            return match ctx.block_client_on_reply(None, finish_waiting, wait_timed_out) {
                Ok(blocked) => {
                    code.wait(blocked, call);
                    Ok(RedisValue::NoReply)
                }
                // Inside MULTI or a script, or from a reply callback.
                Err(_) => {
                    code.wait_here();
                    call_function(ctx, call)
                }
            };
        }
    };
    let mode = flags::check(ctx, ready.flags(), call.read_only, &call.keys)?;
    let nodes = ready
        .run(ctx, function, &call.keys, &call.args, mode)
        .map_err(|e| match e.downcast_ref::<host::CallError>() {
            // As Lua names the script after an error that redis.call raised.
            Some(host::CallError(message)) => {
                RedisError::String(format!("{message} function: {function}."))
            }
            None => error(&format!("function '{function}' failed"), &e),
        })?;
    reply::to_value(&nodes).map_err(|reason| {
        RedisError::String(format!(
            "ERR function '{function}' returned a malformed reply: {reason}"
        ))
    })
}

/// Runs a call that waited for its library to compile.
fn finish_waiting(ctx: &Context, call: WaitingCall) -> RedisResult {
    call_function(ctx, call)
}

/// A waiting call blocks with no timeout, so this does not run.
fn wait_timed_out(_ctx: &Context) -> RedisResult {
    Err(RedisError::Str(
        "ERR timed out waiting for a library to compile",
    ))
}

/// `WASM.LIST`: `[library, [[function, [flag ...]] ...]]` for each library.
fn wasm_list(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ArgCursor::new(&args[1..]).done()?;
    let function = |function: &host::Function| {
        RedisValue::Array(vec![
            RedisValue::BulkString(function.name.clone()),
            RedisValue::Array(
                flags::names(function.flags)
                    .map(RedisValue::SimpleStringStatic)
                    .collect(),
            ),
        ])
    };
    with_host(|host| {
        RedisValue::Array(
            host.list()
                .map(|(name, functions)| {
                    RedisValue::Array(vec![
                        RedisValue::BulkString(name.to_owned()),
                        RedisValue::Array(functions.iter().map(function).collect()),
                    ])
                })
                .collect(),
        )
    })
}

/// `WASM.DELETE <library>`
fn wasm_delete(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let name = args.next_str()?;
    args.done()?;
    if with_host(|host| host.remove(name))? {
        replicate(ctx, "WASM.DELETE");
        Ok(RedisValue::SimpleStringStatic("OK"))
    } else {
        Err(RedisError::String(format!(
            "ERR library '{name}' not found"
        )))
    }
}

/// `WASM.DUMP`: every library, as a payload for `WASM.RESTORE`, as
/// `FUNCTION DUMP` gives one for Lua libraries.
fn wasm_dump(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ArgCursor::new(&args[1..]).done()?;
    with_host(|host| {
        RedisValue::StringBuffer(dump::dump(
            host.saved().map(|(name, component, _)| (name, component)),
        ))
    })
}

/// `WASM.RESTORE <payload> [FLUSH|APPEND|REPLACE]`: loads the libraries of a
/// `WASM.DUMP` payload, all or none, as `FUNCTION RESTORE` does. `APPEND`, the
/// default, fails if a library exists; `REPLACE` replaces it; `FLUSH` deletes
/// every library first. The components compile on a thread, as for
/// `WASM.LOAD`.
fn wasm_restore(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let payload = args.next_bytes()?;
    let policy = match args.next() {
        None => host::Policy::Append,
        Some(arg) => match arg.as_slice().to_ascii_uppercase().as_slice() {
            b"APPEND" => host::Policy::Append,
            b"REPLACE" => host::Policy::Replace,
            b"FLUSH" => host::Policy::Flush,
            _ => {
                return Err(RedisError::Str(
                    "ERR the policy must be FLUSH, APPEND or REPLACE",
                ));
            }
        },
    };
    args.done()?;
    let mut compiler =
        with_host(|host| host.compiler())?.map_err(|e| error("WASM.RESTORE failed", &e))?;
    let libraries = dump::parse(payload).map_err(|e| error("WASM.RESTORE failed", &e))?;
    let libraries = libraries
        .into_iter()
        .map(|(name, component)| {
            compiler_source::Source::spool(component).map(|source| (name, source))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(|e| error("WASM.RESTORE failed", &e))?;
    let supervisor = compiler.supervisor();
    let compile = move || -> anyhow::Result<Vec<(String, host::Compiled)>> {
        let result = libraries
            .into_iter()
            .map(|(name, component)| {
                let compiled = compiler
                    .compile(component)
                    .with_context(|| format!("library '{name}'"))?;
                Ok((name, compiled))
            })
            .collect();
        drop(compiler);
        result
    };

    let Ok(blocked) = ctx.block_client_on_reply(None, finish_restore, restore_timed_out) else {
        return finish_restore(
            ctx,
            Restoring {
                policy,
                blocked: false,
                result: compile(),
            },
        );
    };
    if let Err(error) = supervisor.spawn(move || {
        blocked.unblock(Restoring {
            policy,
            blocked: true,
            result: compile(),
        });
    }) {
        ctx.log_warning(&format!("wasm compiler worker did not start: {error:#}"));
    }
    Ok(RedisValue::NoReply)
}

/// A `WASM.RESTORE` whose components have compiled, or failed to.
struct Restoring {
    policy: host::Policy,
    /// Whether the restore finishes in a blocked client's reply callback.
    blocked: bool,
    result: anyhow::Result<Vec<(String, host::Compiled)>>,
}

/// Lists each restored library's functions, then adds them all or none, on
/// the main thread.
fn finish_restore(ctx: &Context, restoring: Restoring) -> RedisResult {
    let compiled = restoring
        .result
        .map_err(|e| error("WASM.RESTORE failed", &e))?;
    refuse_paused_finish(ctx, restoring.blocked, "WASM.RESTORE")?;
    with_host(|host| -> anyhow::Result<()> {
        let staged = compiled
            .into_iter()
            .map(|(name, compiled)| {
                host.stage(ctx, name.clone(), compiled)
                    .with_context(|| format!("library '{name}'"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        host.restore_all(staged, restoring.policy)
    })?
    .map_err(|e| error("WASM.RESTORE failed", &e))?;
    replicate(ctx, "WASM.RESTORE");
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `WASM.RESTORE` blocks with no timeout, so this does not run.
fn restore_timed_out(_ctx: &Context) -> RedisResult {
    Err(RedisError::Str("ERR WASM.RESTORE timed out"))
}

/// `WASM.STATS`: the running function and the counts of libraries and
/// functions, as `FUNCTION STATS` gives for scripts. It is `allow-busy`, so it
/// answers while a function runs; the counts are nil while a library loads,
/// which holds the host.
fn wasm_stats(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ArgCursor::new(&args[1..]).done()?;
    let running = match busy::running_function() {
        Some((name, duration)) => RedisValue::Array(vec![
            RedisValue::SimpleStringStatic("name"),
            RedisValue::BulkString(name),
            RedisValue::SimpleStringStatic("duration_ms"),
            RedisValue::Integer(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)),
        ]),
        None => RedisValue::Null,
    };
    let count = |n: Option<usize>| {
        n.and_then(|n| i64::try_from(n).ok())
            .map_or(RedisValue::Null, RedisValue::Integer)
    };
    let sizes = try_with_host(|host| host.sizes());
    Ok(RedisValue::Array(vec![
        RedisValue::SimpleStringStatic("running_function"),
        running,
        RedisValue::SimpleStringStatic("libraries_count"),
        count(sizes.as_ref().map(|s| s.libraries)),
        RedisValue::SimpleStringStatic("functions_count"),
        count(sizes.as_ref().map(|s| s.functions)),
    ]))
}

/// `WASM.FLUSH`: deletes every library, as `FUNCTION FLUSH` does.
fn wasm_flush(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ArgCursor::new(&args[1..]).done()?;
    with_host(Host::clear)?;
    replicate(ctx, "WASM.FLUSH");
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `WASM.KILL`: stops the running function if it has not called a write
/// command, as `FUNCTION KILL` does for Lua.
fn wasm_kill(_ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ArgCursor::new(&args[1..]).done()?;
    match busy::kill() {
        busy::Kill::Killed => Ok(RedisValue::SimpleStringStatic("OK")),
        busy::Kill::NotBusy => Err(RedisError::Str("NOTBUSY No wasm function is running.")),
        busy::Kill::Unkillable => Err(RedisError::Str(
            "UNKILLABLE The function has already run write commands. \
             Wait for it to finish, or stop the server with SHUTDOWN NOSAVE.",
        )),
    }
}

fn init(ctx: &RegistrationContext<'_>, _args: &[RedisString]) -> Status {
    // Redis has applied the module's configuration by now. The data type the
    // RDB keeps libraries under is registered only with persistence on, since
    // Redis does not unload a module with a data type.
    if persist::enabled() {
        if let Err(e) = LIBRARIES.create_data_type(ctx) {
            ctx.log_warning(&format!("wasm host: {e}"));
            return Status::Err;
        }
        // aux_load checks for read errors, so a replica can load its
        // primary's RDB from the socket.
        ctx.set_module_options(ModuleOptions::HANDLE_IO_ERRORS);
    }
    match Host::new() {
        Ok(new_host) => {
            *host() = Some(new_host);
            Status::Ok
        }
        Err(e) => {
            ctx.log_warning(&format!("wasm host: {e:#}"));
            Status::Err
        }
    }
}

fn deinit(ctx: &EventContext) -> Status {
    let mut current = host();
    if let Some(host) = current.as_ref() {
        if let Err(error) = host.stop_compiler() {
            ctx.log_warning(&format!("wasm compiler prevents unload: {error:#}"));
            return Status::Err;
        }
    }
    if let Some(host) = current.take() {
        host.shutdown();
    }
    Status::Ok
}

redis_module! {
    name: "wasm",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    // LIBRARIES is registered in init, when wasm.persist is yes.
    data_types: [],
    init: init,
    deinit: deinit,
    commands: [
        // In @scripting, as FUNCTION and FCALL are.
        ["wasm.load", wasm_load, "write deny-oom deny-script", 0, 0, 0, AclCategory::Scripting],
        ["wasm.list", wasm_list, "readonly deny-script", 0, 0, 0, AclCategory::Scripting],
        ["wasm.delete", wasm_delete, "write deny-script", 0, 0, 0, AclCategory::Scripting],
        ["wasm.kill", wasm_kill, "allow-busy deny-script fast", 0, 0, 0, AclCategory::Scripting],
        ["wasm.dump", wasm_dump, "readonly deny-script", 0, 0, 0, AclCategory::Scripting],
        ["wasm.restore", wasm_restore, "write deny-oom deny-script", 0, 0, 0, AclCategory::Scripting],
        ["wasm.flush", wasm_flush, "write deny-script", 0, 0, 0, AclCategory::Scripting],
        ["wasm.stats", wasm_stats, "allow-busy deny-script", 0, 0, 0, AclCategory::Scripting],
    ],
    configurations: [
        i64: [
            ["guest-memory-budget", &budget::GUEST, budget::DEFAULT_BUDGET, 0, i64::MAX, ConfigurationFlags::MEMORY, None],
            ["code-budget", &budget::CODE, budget::DEFAULT_BUDGET, 0, i64::MAX, ConfigurationFlags::MEMORY, None],
            ["compiler-memory", &compiler::MEMORY, compiler::DEFAULT_MEMORY, 64 << 20, 16 << 30, ConfigurationFlags::MEMORY | ConfigurationFlags::IMMUTABLE, None],
            ["compiler-cpu-seconds", &compiler::CPU, compiler::DEFAULT_CPU, 1, 600, ConfigurationFlags::IMMUTABLE, None],
            ["compiler-wall-seconds", &compiler::WALL, compiler::DEFAULT_WALL, 1, 1200, ConfigurationFlags::IMMUTABLE, None],
            ["compiler-queue", &compiler::QUEUE, compiler::DEFAULT_QUEUE, 0, 64, ConfigurationFlags::IMMUTABLE, None],
            [
                "max-memory",
                &memory::MAX_MEMORY,
                memory::DEFAULT_MAX_MEMORY,
                1 << 20,
                i64::MAX,
                ConfigurationFlags::MEMORY,
                None
            ],
        ],
        string: [["compiler-path", &compiler::PATH, "", ConfigurationFlags::IMMUTABLE, None]],
        bool: [
            [
                "persist",
                &persist::PERSIST,
                true,
                ConfigurationFlags::IMMUTABLE,
                None
            ],
        ],
        module_args_as_configuration: true,
    ]
}
