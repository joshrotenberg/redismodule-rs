//! The wasmtime side: the engine, the loaded libraries, and the imports a guest
//! calls back into Redis through.

use std::collections::BTreeMap;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, TryLockError};

use anyhow::{Context as _, bail};
use redis_module::logging::RedisLogLevel;
use redis_module::{CallOptionResp, CallOptions, CallOptionsBuilder, CallResult, Context};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store};

use crate::busy::{self, Running, Ticker};
use crate::reply::push_call_result;

wasmtime::component::bindgen!({
    path: "wit",
    world: "library",
});

use redis_module_wasm::host::redis::LogLevel;

/// A store's data: the context of the command running a guest, set only for
/// the duration of a call.
pub(crate) struct CallState {
    ctx: Option<NonNull<Context>>,
    options: CallOptions,
}

// SAFETY: the context pointer is set, used and cleared on the main thread
// within one command (see `Instance::call`). Instances move between threads
// only as part of the host's state, and only between calls, when the pointer
// is None.
unsafe impl Send for CallState {}

impl CallState {
    fn new() -> Self {
        Self {
            ctx: None,
            // Like redis.call in a script: script mode (S), the caller's ACL
            // user (C), errors as replies (E), replicate the effects (!), and
            // RESP3 replies so the guest sees each reply's type.
            options: CallOptionsBuilder::new()
                .script_mode()
                .verify_acl()
                .errors_as_replies()
                .replicate()
                .resp(CallOptionResp::Resp3)
                .build(),
        }
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

impl redis_module_wasm::host::redis::Host for CallState {
    fn call(&mut self, command: String, args: Vec<Vec<u8>>) -> Vec<Node> {
        let args: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
        busy::note_call(self.ctx(), &command, &args);
        let result: CallResult<'static> =
            self.ctx()
                .call_ext(&command, &self.options, args.as_slice());
        let mut nodes = Vec::new();
        push_call_result(&mut nodes, &result);
        nodes
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
/// functions it provides, and the instance its calls reuse.
struct LoadedLibrary {
    pre: LibraryPre<CallState>,
    functions: Vec<String>,
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
}

impl Instance {
    fn new(engine: &Engine, pre: &LibraryPre<CallState>, ctx: &Context) -> anyhow::Result<Self> {
        let mut store = Store::new(engine, CallState::new());
        store.epoch_deadline_callback(|store| busy::on_epoch(store.data().ctx()));
        // Instantiating can run guest code, so it runs like a call.
        store.data_mut().ctx = Some(NonNull::from(ctx));
        store.set_epoch_deadline(1);
        let library = pre.instantiate(&mut store);
        store.data_mut().ctx = None;
        Ok(Self {
            store,
            library: library?,
        })
    }

    /// Runs `f` with `ctx` as the context of the guest's imports, yielding to
    /// Redis at each epoch.
    fn call<R>(
        &mut self,
        ctx: &Context,
        f: impl FnOnce(&Library, &mut Store<CallState>) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        self.store.data_mut().ctx = Some(NonNull::from(ctx));
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
    /// Which library provides each function.
    functions: BTreeMap<String, String>,
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

    /// Compiles `component` and adds it as library `name`, replacing a library
    /// of that name only if `replace` is set. Returns the library's functions.
    pub(crate) fn load(
        &mut self,
        ctx: &Context,
        name: &str,
        component: &[u8],
        replace: bool,
    ) -> anyhow::Result<Vec<String>> {
        if self.libraries.contains_key(name) && !replace {
            bail!("library '{name}' already exists");
        }
        let component = Component::new(&self.engine, component).context("invalid component")?;
        let pre = LibraryPre::new(self.linker.instantiate_pre(&component)?)?;

        // Listing the functions runs guest code too, so it can be killed.
        let running = Running::start();
        let mut instance = Instance::new(&self.engine, &pre, ctx)?;
        let functions = instance.call(ctx, |library, store| library.call_functions(store))?;
        drop(running);
        for function in &functions {
            match self.functions.get(function) {
                Some(owner) if owner != name => {
                    bail!("function '{function}' already exists in library '{owner}'")
                }
                _ => {}
            }
        }

        self.remove(name);
        for function in &functions {
            self.functions.insert(function.clone(), name.to_owned());
        }
        self.libraries.insert(
            name.to_owned(),
            LoadedLibrary {
                pre,
                functions: functions.clone(),
                // The instance that listed the functions serves the first call.
                kept: Arc::new(Mutex::new(Some(instance))),
            },
        );
        Ok(functions)
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
            self.functions.remove(&function);
        }
        true
    }

    /// The loaded libraries and their functions, by library name.
    pub(crate) fn list(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.libraries
            .iter()
            .map(|(name, library)| (name.as_str(), library.functions.as_slice()))
    }

    /// What a call of `function` needs, so the caller can release the host
    /// before running the guest.
    pub(crate) fn prepare(&self, function: &str) -> Option<PreparedCall> {
        let library = &self.libraries[self.functions.get(function)?];
        Some(PreparedCall {
            engine: self.engine.clone(),
            pre: library.pre.clone(),
            kept: Arc::clone(&library.kept),
        })
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
}

impl PreparedCall {
    /// Runs `function` with `keys` and `args` and returns its reply nodes.
    pub(crate) fn run(
        self,
        ctx: &Context,
        function: &str,
        keys: &[Vec<u8>],
        args: &[Vec<u8>],
    ) -> anyhow::Result<Vec<Node>> {
        let _running = Running::start();
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
        let result = instance.call(ctx, |library, store| {
            library.call_invoke(store, function, keys, args)
        });
        if let (Ok(_), Some(kept)) = (&result, kept.as_mut()) {
            **kept = Some(instance);
        }
        result
    }
}
