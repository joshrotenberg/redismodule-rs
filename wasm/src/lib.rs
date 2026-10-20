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
//! ```
//!
//! A function that runs past `busy-reply-threshold` yields to Redis, which
//! answers other clients with `-BUSY` and accepts `WASM.KILL`; see `busy.rs`.
//!
//! `WASM.LOAD` and `WASM.DELETE` replicate as they are, and the RDB keeps the
//! libraries; see `persist.rs`.

use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

use redis_module::configuration::ConfigurationFlags;
use redis_module::{
    ArgCursor, Context, ModuleOptions, RedisError, RedisResult, RedisString, RedisValue, Status,
    redis_module,
};
use redis_module_macros::command;

mod busy;
mod flags;
mod host;
mod memory;
mod persist;
mod reply;

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
    let component = args.next_bytes()?.to_vec();
    let replace = match args.next() {
        None => false,
        Some(arg) if arg.as_slice().eq_ignore_ascii_case(b"REPLACE") => true,
        Some(_) => return Err(RedisError::Str("ERR the only option is REPLACE")),
    };
    args.done()?;
    let compiler = with_host(|host| host.check_name(&name, replace).map(|()| host.compiler()))?
        .map_err(|e| error("WASM.LOAD failed", &e))?;

    let Ok(blocked) = ctx.block_client_on_reply(None, finish_load, load_timed_out) else {
        let result = compiler.compile(component);
        return finish_load(
            ctx,
            Loaded {
                name,
                replace,
                result,
            },
        );
    };
    // If the thread cannot start, the handle is dropped with it, which
    // unblocks the client with an error reply.
    let _ = std::thread::Builder::new()
        .name("wasm-compile".into())
        .spawn(move || {
            let result = compiler.compile(component);
            blocked.unblock(Loaded {
                name,
                replace,
                result,
            });
        });
    Ok(RedisValue::NoReply)
}

/// A `WASM.LOAD` whose component has compiled, or failed to.
struct Loaded {
    name: String,
    replace: bool,
    result: anyhow::Result<host::Compiled>,
}

/// Adds a compiled library and replies with its functions, on the main thread.
fn finish_load(ctx: &Context, loaded: Loaded) -> RedisResult {
    let compiled = loaded.result.map_err(|e| error("WASM.LOAD failed", &e))?;
    let functions = with_host(|host| host.add(ctx, &loaded.name, compiled, loaded.replace))?
        .map_err(|e| error("WASM.LOAD failed", &e))?;
    // A blocked client keeps its arguments until it is unblocked, after this.
    ctx.replicate_verbatim();
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
        flags: [MayReplicate, DenyScript, NoMandatoryKeys, AllowStale],
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
    run_function(ctx, &args, false)
}

/// `WASM.CALL_RO <function> <numkeys> [key ...] [arg ...]`: like `FCALL_RO`,
/// runs only a function with `no-writes`.
#[command(
    {
        name: "wasm.call_ro",
        flags: [ReadOnly, DenyScript, NoMandatoryKeys, AllowStale],
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
    run_function(ctx, &args, true)
}

fn run_function(ctx: &Context, args: &[RedisString], read_only: bool) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let function = args.next_str()?;
    let numkeys = usize::try_from(args.next_u64()?)
        .ok()
        .filter(|&n| n <= args.remaining().len())
        .ok_or(RedisError::Str(
            "ERR numkeys is larger than the number of arguments",
        ))?;
    let keys: Vec<Vec<u8>> = args.by_ref().take(numkeys).map(|k| k.to_vec()).collect();
    let rest: Vec<Vec<u8>> = args.map(|a| a.to_vec()).collect();

    // Release the host before running the guest, so a guest that calls a
    // WASM.* command fails on its command flags, not on a lock.
    let call = with_host(|host| host.prepare(function))?
        .ok_or_else(|| RedisError::String(format!("ERR function '{function}' not found")))?;
    let mode = flags::check(ctx, call.flags(), read_only, &keys)?;
    let nodes = call.run(ctx, function, &keys, &rest, mode).map_err(|e| {
        match e.downcast_ref::<host::CallError>() {
            // As Lua names the script after an error that redis.call raised.
            Some(host::CallError(message)) => {
                RedisError::String(format!("{message} function: {function}."))
            }
            None => error(&format!("function '{function}' failed"), &e),
        }
    })?;
    reply::to_value(&nodes).map_err(|reason| {
        RedisError::String(format!(
            "ERR function '{function}' returned a malformed reply: {reason}"
        ))
    })
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
        ctx.replicate_verbatim();
        Ok(RedisValue::SimpleStringStatic("OK"))
    } else {
        Err(RedisError::String(format!(
            "ERR library '{name}' not found"
        )))
    }
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

fn init(ctx: &Context, _args: &[RedisString]) -> Status {
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

fn deinit(_ctx: &Context) -> Status {
    if let Some(host) = host().take() {
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
        ["wasm.load", wasm_load, "write deny-oom deny-script", 0, 0, 0, ""],
        ["wasm.list", wasm_list, "readonly deny-script", 0, 0, 0, ""],
        ["wasm.delete", wasm_delete, "write deny-script", 0, 0, 0, ""],
        ["wasm.kill", wasm_kill, "allow-busy deny-script fast", 0, 0, 0, ""],
    ],
    configurations: [
        i64: [
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
        // The macro needs every group listed. Module arguments would be parsed
        // without units, so `max-memory 8mb` would fail the load: set it with
        // `wasm.max-memory` in the configuration or `CONFIG SET` instead.
        string: [],
        bool: [
            [
                "persist",
                &persist::PERSIST,
                true,
                ConfigurationFlags::IMMUTABLE,
                None
            ],
        ],
        enum: [],
        module_args_as_configuration: false,
    ]
}
