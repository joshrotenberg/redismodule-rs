//! The wasmtime side: the engine, the loaded libraries, and the imports a guest
//! calls back into Redis through.
//!
//! # The context pointer
//!
//! A guest's imports and the epoch callback need the context of the command
//! that runs the guest, but a store's data must be `'static`, so `CallState`
//! holds that context as a raw pointer. It is sound because:
//!
//! - `Instance::call` sets the pointer from the command's `&Context`, runs the
//!   guest, and clears it. The guest runs synchronously inside that call, so
//!   every use, from an import or the epoch callback, comes while the borrow
//!   it was made from is live. `Instance::new` does the same around
//!   instantiation.
//! - A call that re-enters the host, through a command the guest runs that
//!   runs `WASM.CALL` again, finds the kept instance locked and uses a new
//!   instance with its own pointer; the outer pointer stays valid below it on
//!   the stack. While a guest yields, Redis runs only `allow-busy` commands,
//!   and `WASM.CALL` is not one.
//! - If an import panics, the pointer is not cleared, but the instance is a
//!   local value of `PreparedCall::run` or `Host::add` and is put back as the
//!   kept instance only after a call that returned `Ok`, so the unwinding drops
//!   it with the stale pointer.
//! - Instances live in the host's state and are only touched on the main
//!   thread. The fork that saves an RDB reads only component bytes, and the
//!   compile thread has only clones of the engine and linker. `CallState` is
//!   `Send` so that the host can sit in a static `Mutex`, and the pointer is
//!   `None` whenever no call runs.
//!
//! The SDK has no lifetime-safe way to give a store the context (S3 in
//! `TRACKING.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, TryLockError};

use anyhow::{Context as _, bail};
use redis_module::logging::RedisLogLevel;
use redis_module::{
    CallOptionResp, CallOptions, CallOptionsBuilder, CallResult, Context, RedisString,
};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store};

use crate::busy::{self, Running, Ticker};
use crate::flags::{self, Mode};
use crate::memory::{LibrarySizes, Live, Memory};
use crate::reply::{flat_call_result, push_call_result};

wasmtime::component::bindgen!({
    path: "wit",
    world: "library",
    // These fail the function by trapping the guest; see CallError.
    imports: {
        "redis-module-wasm:host/redis/call": trappable,
        "redis-module-wasm:host/redis/call-flat": trappable,
        "redis-module-wasm:host/redis/set-resp": trappable,
    },
});

use redis_module_wasm::host::redis::{FlatReply, LogLevel};

/// An error reply that fails the function, from `call` or `set-resp`, as an
/// error raised by `redis.call` fails a Lua function. The function replies
/// with it.
#[derive(Debug)]
pub(crate) struct CallError(pub(crate) String);

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CallError {}

/// A store's data: the context of the command running a guest and how its
/// calls into Redis run, both set for each call.
pub(crate) struct CallState {
    ctx: Option<NonNull<Context>>,
    mode: Mode,
    /// Whether replies to the guest's calls are RESP3; `set-resp` changes it.
    resp3: bool,
    /// The call options for each combination of read_only, allow_oom and
    /// resp3, by `options`.
    options: [CallOptions; 8],
    memory: Memory,
}

// SAFETY: the context pointer is set, used and cleared on the main thread
// within one command; see "The context pointer" in the module docs.
unsafe impl Send for CallState {}

impl CallState {
    fn new() -> Self {
        Self {
            ctx: None,
            mode: Mode::loading(),
            resp3: true,
            options: std::array::from_fn(|i| call_options(i & 4 != 0, i & 2 != 0, i & 1 != 0)),
            memory: Memory::default(),
        }
    }

    fn options(&self) -> &CallOptions {
        &self.options[usize::from(self.mode.read_only) << 2
            | usize::from(self.mode.allow_oom) << 1
            | usize::from(self.resp3)]
    }

    /// Runs a command for `call`, `pcall` and their flat forms: its result,
    /// or the error the host refused it with.
    fn run_command(
        &mut self,
        command: &str,
        args: &[Vec<u8>],
    ) -> Result<CallResult<'static>, String> {
        if self.mode.loading {
            return Err("ERR a library may not call Redis while it loads".into());
        }
        let args: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
        if self.mode.one_slot {
            self.check_slot(command, &args)?;
        }
        let effects = busy::note_call(self.ctx(), command, &args);
        // As for a read-only Lua script: may-replicate commands count as
        // writes. RedisModule_Call's W flag refuses only write commands.
        if self.mode.read_only && (effects.write || effects.may_replicate) {
            return Err("ERR Write commands are not allowed from read-only functions.".into());
        }
        Ok(self
            .ctx()
            .call_ext(command, self.options(), args.as_slice()))
    }

    /// The reply to a command as nodes, an error as an error node.
    fn nodes(&mut self, command: &str, args: &[Vec<u8>]) -> Vec<Node> {
        match self.run_command(command, args) {
            Ok(result) => {
                let mut nodes = Vec::new();
                push_call_result(&mut nodes, &result);
                nodes
            }
            Err(message) => vec![Node::Error(message)],
        }
    }

    /// The reply to a command, flat, an error as an error node.
    fn flat(&mut self, command: &str, args: &[Vec<u8>]) -> FlatReply {
        match self.run_command(command, args) {
            Ok(result) => flat_call_result(&result),
            Err(message) => FlatReply::error(&message),
        }
    }

    /// Fails if a command using `args` would leave the call's slot, as Lua
    /// does on a cluster node for a script without `allow-cross-slot-keys`.
    fn check_slot(&mut self, command: &str, args: &[&[u8]]) -> Result<(), String> {
        let Some(slot) = flags::command_slot(self.ctx(), command, args) else {
            return Ok(());
        };
        match self.mode.slot {
            None => self.mode.slot = Some(slot),
            Some(first) if first != slot => {
                return Err(
                    "ERR Function attempted to access keys that do not hash to the same slot"
                        .into(),
                );
            }
            Some(_) => {}
        }
        Ok(())
    }

    /// The context of the command running the guest. Host imports and the
    /// epoch callback run only inside a call.
    fn ctx(&self) -> &Context {
        let ctx = self.ctx.expect("guest code runs only inside a call");
        // SAFETY: Instance::call sets the pointer from the command's &Context
        // and clears it before returning, and the guest runs synchronously in
        // that call, so the Context outlives every use.
        unsafe { ctx.as_ref() }
    }
}

/// The options for a guest's calls, like those of redis.call in a script:
/// script mode (S), the caller's ACL user (C), errors as replies (E), and the
/// effects replicated (!). A read-only call refuses write commands (W); one
/// without allow-oom refuses deny-oom commands over maxmemory (M). Replies are
/// RESP3 unless the guest asked for RESP2.
fn call_options(read_only: bool, allow_oom: bool, resp3: bool) -> CallOptions {
    let resp = if resp3 {
        CallOptionResp::Resp3
    } else {
        CallOptionResp::Resp2
    };
    let mut builder = CallOptionsBuilder::new()
        .script_mode()
        .verify_acl()
        .errors_as_replies()
        .replicate()
        .resp(resp);
    if read_only {
        builder = builder.no_writes();
    }
    if !allow_oom {
        builder = builder.verify_oom();
    }
    builder.build()
}

impl redis_module_wasm::host::redis::Host for CallState {
    fn call(&mut self, command: String, args: Vec<Vec<u8>>) -> wasmtime::Result<Vec<Node>> {
        let nodes = self.nodes(&command, &args);
        // Only an error reply fails the function; an error inside an array,
        // such as one in EXEC's reply, does not.
        match nodes.as_slice() {
            [Node::Error(message)] => Err(CallError(message.clone()).into()),
            _ => Ok(nodes),
        }
    }

    fn pcall(&mut self, command: String, args: Vec<Vec<u8>>) -> Vec<Node> {
        self.nodes(&command, &args)
    }

    fn call_flat(&mut self, command: String, args: Vec<Vec<u8>>) -> wasmtime::Result<FlatReply> {
        let reply = self.flat(&command, &args);
        match reply.as_error() {
            Some(message) => Err(CallError(message).into()),
            None => Ok(reply),
        }
    }

    fn pcall_flat(&mut self, command: String, args: Vec<Vec<u8>>) -> FlatReply {
        self.flat(&command, &args)
    }

    fn acl_check_command(&mut self, command: String, args: Vec<Vec<u8>>) -> Result<bool, String> {
        let ctx = self.ctx();
        // A context with no user, such as the AOF's, may run anything.
        let Some(user) = ctx.get_current_user() else {
            return Ok(true);
        };
        let argv: Vec<RedisString> = std::iter::once(command.into_bytes())
            .chain(args)
            .map(|arg| ctx.create_string(arg))
            .collect();
        match ctx.acl_check_command_permission(&user, &argv) {
            Ok(()) => Ok(true),
            Err(error) => {
                let message = error.to_string();
                if message.starts_with("NOPERM") {
                    Ok(false)
                } else {
                    Err(message)
                }
            }
        }
    }

    fn set_resp(&mut self, version: u8) -> wasmtime::Result<()> {
        self.resp3 = match version {
            2 => false,
            3 => true,
            _ => return Err(CallError("ERR RESP version must be 2 or 3.".into()).into()),
        };
        Ok(())
    }

    fn log(&mut self, level: LogLevel, message: String) {
        let level = match level {
            LogLevel::Debug => RedisLogLevel::Debug,
            LogLevel::Verbose => RedisLogLevel::Verbose,
            LogLevel::Notice => RedisLogLevel::Notice,
            LogLevel::Warning => RedisLogLevel::Warning,
        };
        self.ctx().log(level, &message);
    }
}

/// A loaded library: its component, linked and ready to instantiate, the
/// component's bytes, which the RDB keeps, the functions it provides, and the
/// instance its calls reuse.
struct LoadedLibrary {
    pre: LibraryPre<CallState>,
    component: Vec<u8>,
    functions: Vec<Function>,
    kept: KeptInstance,
}

/// The instance a library's calls reuse. Reusing it, rather than creating one
/// per call, is what makes a call cheap: instantiating and dropping an
/// instance took about 16 of a call's 22 µs.
type KeptInstance = Arc<Mutex<Option<Instance>>>;

/// An instance of a library and its store.
struct Instance {
    store: Store<CallState>,
    library: Library,
    _live: Live,
}

impl Instance {
    fn new(engine: &Engine, pre: &LibraryPre<CallState>, ctx: &Context) -> anyhow::Result<Self> {
        let mut store = Store::new(engine, CallState::new());
        store.limiter(|state| &mut state.memory);
        store.epoch_deadline_callback(|store| busy::on_epoch(store.data().ctx()));
        // Instantiating can run guest code, so it runs like a call, in the
        // loading mode CallState starts in.
        store.data_mut().ctx = Some(NonNull::from(ctx));
        store.set_epoch_deadline(1);
        let library = pre.instantiate(&mut store);
        store.data_mut().ctx = None;
        Ok(Self {
            store,
            library: library?,
            _live: Live::new(),
        })
    }

    /// Runs `f` with `ctx` as the context of the guest's imports and `mode`
    /// for its calls, yielding to Redis at each epoch.
    fn call<R>(
        &mut self,
        ctx: &Context,
        mode: Mode,
        f: impl FnOnce(&Library, &mut Store<CallState>) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        let state = self.store.data_mut();
        state.ctx = Some(NonNull::from(ctx));
        state.mode = mode;
        state.resp3 = true;
        self.store.set_epoch_deadline(1);
        let result = f(&self.library, &mut self.store);
        self.store.data_mut().ctx = None;
        result
    }
}

/// The engine and the libraries loaded with `WASM.LOAD`.
pub(crate) struct Host {
    engine: Engine,
    ticker: Ticker,
    linker: Linker<CallState>,
    libraries: BTreeMap<String, LoadedLibrary>,
    /// Which library provides each function, and the function's flags.
    functions: BTreeMap<String, (String, FunctionFlags)>,
}

impl Host {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        // Redis installs its own SIGSEGV handler to write crash reports, so
        // keep wasmtime from installing signal handlers: guests get explicit
        // bounds checks instead.
        config.signals_based_traps(false);
        // Long-running guests yield at each epoch; see busy.rs.
        config.epoch_interruption(true);
        // Keep trap errors to one line: they become error replies.
        config.wasm_backtrace(false);
        let engine = Engine::new(&config)?;
        let ticker = Ticker::start(engine.clone())?;
        let mut linker = Linker::new(&engine);
        Library::add_to_linker::<CallState, HasSelf<CallState>>(&mut linker, |state| state)?;
        Ok(Self {
            engine,
            ticker,
            linker,
            libraries: BTreeMap::new(),
            functions: BTreeMap::new(),
        })
    }

    /// What compiling a component needs, to compile it on another thread.
    pub(crate) fn compiler(&self) -> Compiler {
        Compiler {
            engine: self.engine.clone(),
            linker: self.linker.clone(),
        }
    }

    /// Fails if library `name` exists and `replace` is not set.
    pub(crate) fn check_name(&self, name: &str, replace: bool) -> anyhow::Result<()> {
        if self.libraries.contains_key(name) && !replace {
            bail!("library '{name}' already exists");
        }
        Ok(())
    }

    /// Adds a compiled component as library `name`, replacing a library of
    /// that name only if `replace` is set. Returns the library's functions.
    pub(crate) fn add(
        &mut self,
        ctx: &Context,
        name: &str,
        compiled: Compiled,
        replace: bool,
    ) -> anyhow::Result<Vec<Function>> {
        // Checked again here: another load may have added the name while this
        // component compiled.
        self.check_name(name, replace)?;
        let staged = self.stage(ctx, name.to_owned(), compiled)?;
        let functions = staged.functions.clone();
        self.insert(
            &staged.name,
            staged.compiled,
            staged.functions,
            Some(staged.instance),
        )?;
        Ok(functions)
    }

    /// Instantiates a compiled library and lists its functions, without
    /// adding it.
    pub(crate) fn stage(
        &self,
        ctx: &Context,
        name: String,
        compiled: Compiled,
    ) -> anyhow::Result<Staged> {
        // Listing the functions runs guest code too, so it can be killed.
        let running = Running::start();
        let mut instance = Instance::new(&self.engine, &compiled.pre, ctx)?;
        let functions = instance.call(ctx, Mode::loading(), |library, store| {
            library.call_functions(store)
        })?;
        drop(running);
        Ok(Staged {
            name,
            compiled,
            functions,
            instance,
        })
    }

    /// Adds `staged` libraries, all or none, as `FUNCTION RESTORE` does with
    /// `policy`: `Append` fails if a library exists, `Replace` replaces it,
    /// and `Flush` deletes every library first. A function name may not be in
    /// two of the libraries, nor in a library that stays.
    pub(crate) fn restore_all(
        &mut self,
        staged: Vec<Staged>,
        policy: Policy,
    ) -> anyhow::Result<()> {
        let names: BTreeSet<&str> = staged.iter().map(|s| s.name.as_str()).collect();
        if names.len() != staged.len() {
            bail!("the payload has a library twice");
        }
        // Whether an existing library is still there after the restore.
        let stays = |library: &str| match policy {
            Policy::Flush => false,
            Policy::Append => true,
            Policy::Replace => !names.contains(library),
        };
        let mut restored_functions = BTreeMap::new();
        for library in &staged {
            if policy == Policy::Append && self.libraries.contains_key(&library.name) {
                bail!("library '{}' already exists", library.name);
            }
            for function in &library.functions {
                if let Some(other) = restored_functions.insert(&function.name, &library.name) {
                    bail!(
                        "function '{}' is in libraries '{other}' and '{}'",
                        function.name,
                        library.name
                    );
                }
                if let Some((owner, _)) = self.functions.get(&function.name) {
                    if stays(owner) {
                        bail!(
                            "function '{}' already exists in library '{owner}'",
                            function.name
                        );
                    }
                }
            }
        }

        if policy == Policy::Flush {
            self.clear();
        }
        for library in &staged {
            self.remove(&library.name);
        }
        for library in staged {
            self.insert(
                &library.name,
                library.compiled,
                library.functions,
                Some(library.instance),
            )?;
        }
        Ok(())
    }

    /// Adds library `name` from an RDB, which also lists its functions,
    /// replacing a library of that name. Loading runs no guest code: the
    /// library's first call creates its instance.
    pub(crate) fn restore(
        &mut self,
        name: &str,
        compiled: Compiled,
        functions: Vec<Function>,
    ) -> anyhow::Result<()> {
        self.insert(name, compiled, functions, None)
    }

    fn insert(
        &mut self,
        name: &str,
        compiled: Compiled,
        functions: Vec<Function>,
        instance: Option<Instance>,
    ) -> anyhow::Result<()> {
        for function in &functions {
            match self.functions.get(&function.name) {
                Some((owner, _)) if owner != name => {
                    bail!(
                        "function '{}' already exists in library '{owner}'",
                        function.name
                    )
                }
                _ => {}
            }
        }

        self.remove(name);
        for function in &functions {
            self.functions
                .insert(function.name.clone(), (name.to_owned(), function.flags));
        }
        self.libraries.insert(
            name.to_owned(),
            LoadedLibrary {
                pre: compiled.pre,
                component: compiled.component,
                functions,
                kept: Arc::new(Mutex::new(instance)),
            },
        );
        Ok(())
    }

    /// Stops the epoch thread, so the module can be unloaded.
    pub(crate) fn shutdown(self) {
        self.ticker.stop();
    }

    /// Removes library `name`; returns whether it existed.
    pub(crate) fn remove(&mut self, name: &str) -> bool {
        let Some(library) = self.libraries.remove(name) else {
            return false;
        };
        for function in library.functions {
            self.functions.remove(&function.name);
        }
        true
    }

    /// Removes every library.
    pub(crate) fn clear(&mut self) {
        self.libraries.clear();
        self.functions.clear();
    }

    /// The loaded libraries and their functions, by library name.
    pub(crate) fn list(&self) -> impl Iterator<Item = (&str, &[Function])> {
        self.libraries
            .iter()
            .map(|(name, library)| (name.as_str(), library.functions.as_slice()))
    }

    /// The libraries' counts and sizes, for `INFO wasm`.
    pub(crate) fn sizes(&mut self) -> LibrarySizes {
        let mut sizes = LibrarySizes {
            libraries: self.libraries.len(),
            functions: self.functions.len(),
            ..LibrarySizes::default()
        };
        for library in self.libraries.values() {
            sizes.component_bytes += library.component.len();
            let code = library.pre.instance_pre().component().image_range();
            sizes.code_bytes += code.end as usize - code.start as usize;
        }
        sizes
    }

    /// What the RDB keeps of each library: its name, its component's bytes and
    /// its functions.
    pub(crate) fn saved(&self) -> impl Iterator<Item = (&str, &[u8], &[Function])> {
        self.libraries.iter().map(|(name, library)| {
            (
                name.as_str(),
                library.component.as_slice(),
                library.functions.as_slice(),
            )
        })
    }

    /// What a call of `function` needs, so the caller can release the host
    /// before running the guest.
    pub(crate) fn prepare(&self, function: &str) -> Option<PreparedCall> {
        let (library, flags) = self.functions.get(function)?;
        let library = &self.libraries[library];
        Some(PreparedCall {
            engine: self.engine.clone(),
            pre: library.pre.clone(),
            kept: Arc::clone(&library.kept),
            flags: *flags,
        })
    }
}

/// A compiled library whose functions have been listed, ready to add.
pub(crate) struct Staged {
    name: String,
    compiled: Compiled,
    functions: Vec<Function>,
    instance: Instance,
}

/// How `WASM.RESTORE` treats the libraries already loaded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Policy {
    Append,
    Replace,
    Flush,
}

/// A compiled component, linked and ready to instantiate, and its bytes.
pub(crate) struct Compiled {
    pre: LibraryPre<CallState>,
    component: Vec<u8>,
}

/// Compiles components. It holds clones of the engine and linker, so it can run
/// on another thread while the server keeps serving.
pub(crate) struct Compiler {
    engine: Engine,
    linker: Linker<CallState>,
}

impl Compiler {
    pub(crate) fn compile(&self, component: Vec<u8>) -> anyhow::Result<Compiled> {
        let compiled = Component::new(&self.engine, &component).context("invalid component")?;
        let pre = LibraryPre::new(self.linker.instantiate_pre(&compiled)?)?;
        Ok(Compiled { pre, component })
    }
}

/// A function call ready to run.
///
/// It runs in the library's kept instance, so a guest's memory and globals
/// persist from one call to the next. A call that fails, for example one that
/// traps or is killed, may leave the instance in any state, so its instance is
/// dropped and the next call starts a new one. Replacing or deleting the
/// library drops it too.
pub(crate) struct PreparedCall {
    engine: Engine,
    pre: LibraryPre<CallState>,
    kept: KeptInstance,
    flags: FunctionFlags,
}

impl PreparedCall {
    /// The function's flags.
    pub(crate) fn flags(&self) -> FunctionFlags {
        self.flags
    }

    /// Runs `function` with `keys` and `args`, its calls in `mode`, and
    /// returns its reply nodes.
    pub(crate) fn run(
        self,
        ctx: &Context,
        function: &str,
        keys: &[Vec<u8>],
        args: &[Vec<u8>],
        mode: Mode,
    ) -> anyhow::Result<Vec<Node>> {
        let _running = Running::start_function(function);
        // Another module's callback can call this library again while one of
        // its calls runs (from inside a Redis command the guest called). That
        // nested call finds the kept instance in use and gets a new one.
        let mut kept = match self.kept.try_lock() {
            Ok(kept) => Some(kept),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        let mut instance = match kept.as_mut().and_then(|kept| kept.take()) {
            Some(instance) => instance,
            None => Instance::new(&self.engine, &self.pre, ctx)?,
        };
        let result = instance.call(ctx, mode, |library, store| {
            library.call_invoke(store, function, keys, args)
        });
        if let (Ok(_), Some(kept)) = (&result, kept.as_mut()) {
            **kept = Some(instance);
        }
        result
    }
}
