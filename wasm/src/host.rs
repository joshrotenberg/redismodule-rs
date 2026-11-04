//! The wasmtime side: the engine, the loaded libraries, and the imports a guest
//! calls back into Redis through.
//!
//! # The context pointer
//!
//! A guest's imports and the epoch callback need the context of the command
//! that runs the guest, but a store's data must be `'static`, so `CallState`
//! holds that context as a raw pointer. It is sound because:
//!
//! - Every guest run, `Instance::call` and `Instance::new` (instantiating runs
//!   guest code), goes through [`invoke`], which sets the pointer from the
//!   command's `&Context` and holds an [`Invocation`] whose `Drop` clears it.
//!   The guest runs synchronously inside the invocation, so every use, from an
//!   import or the epoch callback, comes while the borrow it was made from is
//!   live, and the pointer is `None` after every exit: a return, an error, a
//!   guest trap, and a panic from an import, which wasmtime carries back
//!   through the guest's frames and resumes as a Rust panic. An import that
//!   runs with no invocation, which nothing does, panics in `CallState::ctx`
//!   rather than use a stale pointer.
//! - A call that re-enters the host, through a command the guest runs that
//!   runs `WASM.CALL` again, uses another store: a new instance when it calls
//!   the same library, whose kept instance is taken, or the other library's
//!   instance. The outer pointer stays valid below it on the stack. A guest
//!   cannot run `WASM.*` commands itself (they deny scripts). While a guest
//!   yields, Redis runs only `allow-busy` commands; `WASM.CALL` is not one,
//!   and one from another module that runs `WASM.CALL` is such a re-entry.
//! - An instance whose call failed or panicked is a local value of
//!   `PreparedCall::run` or `Host::stage` (which `WASM.LOAD` and
//!   `WASM.RESTORE` use) and is dropped, not kept; it is put back as the kept
//!   instance only after a call that returned `Ok`.
//! - Instances live in the host's state and are only touched on the main
//!   thread. The fork that saves an RDB reads only component bytes, and the
//!   compile threads hold clones of the engine and linker and the code slots
//!   they fill (`code.rs`), never an instance. `CallState` is
//!   `Send` so that the host can sit in a static `Mutex`, and the pointer is
//!   `None` whenever no call runs.
//!
//! The SDK gives callbacks a borrowed `Context` and no `'static` handle to one
//! (issue 54), so the invocation is the scope: a store holds the pointer only
//! while one runs (S3 in `TRACKING.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, TryLockError};

use anyhow::bail;
use redis_module::logging::RedisLogLevel;
use redis_module::{
    CallOptionResp, CallOptions, CallOptionsBuilder, CallResult, Context, RedisString,
};
use wasmtime::component::{HasSelf, Linker};
use wasmtime::{AsContextMut, Engine, Store, StoreContextMut};

use crate::busy::{self, Running, Ticker};
use crate::code::{Code, Now};
use crate::compiler::{Reservation, Supervisor};
use crate::compiler_source::Source;
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
    /// or the error the host refused it with. The reply borrows the call's
    /// context, so callers convert it before the guest call returns.
    fn run_command(&mut self, command: &str, args: &[Vec<u8>]) -> Result<CallResult<'_>, String> {
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
    /// epoch callback run only inside an invocation.
    fn ctx(&self) -> &Context {
        let ctx = self.ctx.expect("guest code runs only inside a call");
        // SAFETY: only an Invocation sets the pointer, from the command's
        // &Context, and its Drop clears it; the guest runs synchronously inside
        // it, so the Context outlives every use.
        unsafe { ctx.as_ref() }
    }
}

/// One guest run's hold on the command's context. [`invoke`] puts the context
/// in the store's data and creates this; its `Drop` takes the context out, on
/// every exit from the run, including a panic unwinding through it. See "The
/// context pointer".
struct Invocation<'s> {
    store: &'s mut Store<CallState>,
}

impl Drop for Invocation<'_> {
    fn drop(&mut self) {
        self.store.data_mut().ctx = None;
    }
}

/// Runs guest code on `store` with `ctx` as the context of its imports and
/// `mode` for its calls, yielding to Redis at each epoch. Every guest run, a
/// call or an instantiation, goes through here, and `run` gets the store only
/// as a `StoreContextMut`, so it cannot swap the store out of the invocation.
///
/// # Safety
///
/// `ctx` must come from the running command's `&Context`, borrowed for at
/// least this call: the guest's imports and the epoch callback dereference it
/// (`CallState::ctx`) until `invoke` returns or unwinds.
unsafe fn invoke<R>(
    store: &mut Store<CallState>,
    ctx: NonNull<Context>,
    mode: Mode,
    run: impl FnOnce(StoreContextMut<'_, CallState>) -> R,
) -> R {
    // An invocation holds the store mutably, so no other one runs on it; a
    // nested call runs on another instance's store.
    debug_assert!(store.data().ctx.is_none(), "a store has one invocation");
    let state = store.data_mut();
    state.ctx = Some(ctx);
    state.mode = mode;
    state.resp3 = true;
    let invocation = Invocation { store };
    invocation.store.set_epoch_deadline(1);
    run(invocation.store.as_context_mut())
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
    code: Arc<Code>,
    component: Arc<Vec<u8>>,
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
        // Instantiating can run guest code, so it runs like a call, in loading
        // mode.
        // SAFETY: ctx is the command's context, borrowed for this call.
        let library = unsafe {
            invoke(&mut store, NonNull::from(ctx), Mode::loading(), |store| {
                pre.instantiate(store)
            })
        };
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
        f: impl FnOnce(&Library, StoreContextMut<'_, CallState>) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        let library = &self.library;
        // SAFETY: ctx is the command's context, borrowed for this call.
        unsafe {
            invoke(&mut self.store, NonNull::from(ctx), mode, |store| {
                f(library, store)
            })
        }
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
    compiler: Arc<Supervisor>,
}

impl Host {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let engine = crate::compiler_engine::engine()?;
        let mut linker = Linker::new(&engine);
        Library::add_to_linker::<CallState, HasSelf<CallState>>(&mut linker, |state| state)?;
        let compiler = Supervisor::new()?;
        // Start the owned thread only after all other fallible setup succeeds.
        let ticker = Ticker::start(engine.clone())?;
        Ok(Self {
            engine,
            ticker,
            linker,
            libraries: BTreeMap::new(),
            functions: BTreeMap::new(),
            compiler,
        })
    }

    /// What compiling a component needs, to compile it on another thread.
    pub(crate) fn compiler(&self) -> anyhow::Result<Compiler> {
        Ok(Compiler {
            engine: self.engine.clone(),
            linker: self.linker.clone(),
            reservation: self.compiler.reserve()?,
        })
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
        self.insert_staged(staged)?;
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
        mut staged: Vec<Staged>,
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

        // Every guest has finished staging. Materialize lasting Redis-owned
        // source bytes before the atomic mutation, with no subsequent guest,
        // Redis command or yield until this callback returns and argv is freed.
        for library in &mut staged {
            library.compiled.component.materialize()?;
        }
        if policy == Policy::Flush {
            self.clear();
        }
        for library in &staged {
            self.remove(&library.name);
        }
        for library in staged {
            self.insert_staged(library)?;
        }
        Ok(())
    }

    /// Adds library `name` from an RDB, which also lists its functions,
    /// replacing a library of that name, before its component has compiled.
    /// Returns its code for the caller to compile: calls wait for it. Loading
    /// runs no guest code: the library's first call creates its instance.
    pub(crate) fn restore_compiling(
        &mut self,
        name: &str,
        component: Arc<Vec<u8>>,
        functions: Vec<Function>,
    ) -> anyhow::Result<Arc<Code>> {
        let code = Arc::new(Code::compiling());
        self.insert(name, Arc::clone(&code), component, functions, None)?;
        Ok(code)
    }

    fn insert_staged(&mut self, staged: Staged) -> anyhow::Result<()> {
        self.insert(
            &staged.name,
            Arc::new(Code::ready(staged.compiled.pre)),
            staged.compiled.component.into_saved()?,
            staged.functions,
            Some(staged.instance),
        )
    }

    fn insert(
        &mut self,
        name: &str,
        code: Arc<Code>,
        component: Arc<Vec<u8>>,
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
                code,
                component,
                functions,
                kept: Arc::new(Mutex::new(instance)),
            },
        );
        Ok(())
    }

    pub(crate) fn stop_compiler(&self) -> anyhow::Result<()> {
        self.compiler.shutdown()
    }

    /// Only after compiler cleanup succeeds; a refused unload retains Host/ticker.
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
            match library.code.now() {
                Now::Ready(pre) => {
                    let code = pre.instance_pre().component().image_range();
                    sizes.code_bytes += code.end as usize - code.start as usize;
                }
                Now::Compiling => sizes.compiling += 1,
                Now::Failed(_) => {}
            }
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
    pub(crate) fn prepare(&self, function: &str) -> Option<Prepared> {
        let (name, flags) = self.functions.get(function)?;
        let library = &self.libraries[name];
        Some(match library.code.now() {
            Now::Ready(pre) => Prepared::Ready(PreparedCall {
                engine: self.engine.clone(),
                pre,
                kept: Arc::clone(&library.kept),
                flags: *flags,
            }),
            Now::Compiling => Prepared::Compiling(Arc::clone(&library.code)),
            Now::Failed(message) => {
                Prepared::Failed(format!("library '{name}' did not compile: {message}"))
            }
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
    component: Source,
}

/// Compiles components. It holds clones of the engine and linker, so it can run
/// on another thread while the server keeps serving.
pub(crate) struct Compiler {
    engine: Engine,
    linker: Linker<CallState>,
    reservation: Reservation,
}

impl Compiler {
    pub(crate) fn supervisor(&self) -> Arc<Supervisor> {
        self.reservation.supervisor()
    }

    pub(crate) fn compile(&mut self, component: Source) -> anyhow::Result<Compiled> {
        let pre = self.compile_code(&component)?;
        Ok(Compiled { pre, component })
    }

    /// Compiles a component without keeping its bytes.
    pub(crate) fn compile_code(
        &mut self,
        component: &Source,
    ) -> anyhow::Result<LibraryPre<CallState>> {
        let compiled = self.reservation.compile(&self.engine, component)?;
        LibraryPre::new(self.linker.instantiate_pre(&compiled)?)
    }
}

/// What a call of a function finds.
pub(crate) enum Prepared {
    Ready(PreparedCall),
    /// The library's component is compiling after an RDB load.
    Compiling(Arc<Code>),
    /// The library's component did not compile; the reason.
    Failed(String),
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

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use redis_module_wasm::host::redis::Host as _;
    use wasmtime::{Caller, Linker, Module, Trap};

    use super::*;

    /// A core module, encoded by hand since the crate has no text-format
    /// parser: `boom` calls the host import `host.boom`, and `trap` runs
    /// `unreachable`.
    const GUEST: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version 1
        0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // types: () -> ()
        0x02, 0x0d, 0x01, 0x04, b'h', b'o', b's', b't', 0x04, b'b', b'o', b'o', b'm', 0x00,
        0x00, // import host.boom
        0x03, 0x03, 0x02, 0x00, 0x00, // two functions of type 0
        0x07, 0x0f, 0x02, 0x04, b'b', b'o', b'o', b'm', 0x00, 0x01, 0x04, b't', b'r', b'a', b'p',
        0x00, 0x02, // exports boom (1) and trap (2)
        0x0a, 0x0a, 0x02, 0x04, 0x00, 0x10, 0x00, 0x0b, 0x03, 0x00, 0x00,
        0x0b, // boom: call 0; trap: unreachable
    ];

    /// A store as `Instance::new` makes one, with `GUEST` instantiated in it
    /// and a `host.boom` import that panics. The context is never
    /// dereferenced, so a dangling pointer stands in for it.
    fn guest() -> (Store<CallState>, wasmtime::Instance) {
        let engine = crate::compiler_engine::engine().unwrap();
        let module = Module::new(&engine, GUEST).unwrap();
        let mut linker = Linker::new(&engine);
        linker
            .func_wrap("host", "boom", |caller: Caller<'_, CallState>| {
                // It runs inside an invocation, so it sees the context, and
                // panics there; without one it returns, failing the test.
                if caller.data().ctx.is_some() {
                    panic!("a host import panicked");
                }
            })
            .unwrap();
        let mut store = Store::new(&engine, CallState::new());
        let instance =
            invoke_in_test(&mut store, |store| linker.instantiate(store, &module)).unwrap();
        assert!(store.data().ctx.is_none(), "after instantiating");
        (store, instance)
    }

    /// `invoke` with a dangling context, which only the test guest's import
    /// and `CallState::ctx` look at, and only to see whether one is set.
    fn invoke_in_test<R>(
        store: &mut Store<CallState>,
        run: impl FnOnce(StoreContextMut<'_, CallState>) -> R,
    ) -> R {
        // SAFETY: nothing dereferences the context: the test guest's only
        // import checks whether it is set, the store has no epoch callback,
        // and no test calls an import that uses it inside an invocation.
        unsafe { invoke(store, NonNull::dangling(), Mode::loading(), run) }
    }

    fn run(
        store: &mut Store<CallState>,
        instance: &wasmtime::Instance,
        name: &str,
    ) -> anyhow::Result<()> {
        invoke_in_test(store, |mut store| {
            assert!(store.data().ctx.is_some());
            let function = instance.get_typed_func::<(), ()>(&mut store, name)?;
            function.call(&mut store, ())
        })
    }

    #[test]
    fn every_exit_from_an_invocation_clears_the_context() {
        let (mut store, instance) = guest();

        invoke_in_test(&mut store, |store| assert!(store.data().ctx.is_some()));
        assert!(store.data().ctx.is_none(), "after a return");

        let failed = invoke_in_test(&mut store, |_| -> anyhow::Result<()> {
            Err(CallError("ERR this function always fails".into()).into())
        });
        assert!(failed.is_err());
        assert!(store.data().ctx.is_none(), "after an error");

        let trapped = run(&mut store, &instance, "trap").unwrap_err();
        assert_eq!(
            trapped.downcast_ref::<Trap>(),
            Some(&Trap::UnreachableCodeReached)
        );
        assert!(store.data().ctx.is_none(), "after a guest trap");

        let panicked = catch_unwind(AssertUnwindSafe(|| run(&mut store, &instance, "boom")));
        let payload = panicked.expect_err("the import's panic unwound through the guest");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"a host import panicked")
        );
        assert!(store.data().ctx.is_none(), "after a panic in a host import");

        // The store takes a new invocation after each of them.
        let trapped = run(&mut store, &instance, "trap").unwrap_err();
        assert_eq!(
            trapped.downcast_ref::<Trap>(),
            Some(&Trap::UnreachableCodeReached)
        );
        assert!(store.data().ctx.is_none());
    }

    #[test]
    fn a_host_import_after_the_invocation_returned_cannot_reach_the_context() {
        let (mut store, _) = guest();
        invoke_in_test(&mut store, |store| assert!(store.data().ctx.is_some()));
        let state = store.data_mut();
        let reached = catch_unwind(AssertUnwindSafe(|| {
            state.log(LogLevel::Notice, "after the invocation".into());
        }));
        let payload = reached.expect_err("an import reached the context after its invocation");
        assert_eq!(
            payload.downcast_ref::<String>().map(String::as_str),
            Some("guest code runs only inside a call")
        );
    }
}
