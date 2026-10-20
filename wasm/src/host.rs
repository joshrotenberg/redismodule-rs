//! The wasmtime side: the engine, the loaded libraries, and the imports a guest
//! calls back into Redis through.

use std::collections::BTreeMap;
use std::ptr::NonNull;

use anyhow::{Context as _, bail};
use redis_module::logging::RedisLogLevel;
use redis_module::{CallOptionResp, CallOptions, CallOptionsBuilder, CallResult, Context};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store};

use crate::reply::push_call_result;

wasmtime::component::bindgen!({
    path: "wit",
    world: "library",
});

use redis_module_wasm::host::redis::LogLevel;

/// The store data for one function call: the context of the command running
/// it.
pub(crate) struct CallState {
    ctx: NonNull<Context>,
    options: CallOptions,
}

impl CallState {
    fn new(ctx: &Context) -> Self {
        Self {
            ctx: NonNull::from(ctx),
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

    fn ctx(&self) -> &Context {
        // SAFETY: a CallState lives in a Store that the command handler
        // creates and drops before it returns, and the guest runs
        // synchronously in that handler, so the Context outlives every use.
        unsafe { self.ctx.as_ref() }
    }
}

impl redis_module_wasm::host::redis::Host for CallState {
    fn call(&mut self, command: String, args: Vec<Vec<u8>>) -> Vec<Node> {
        let args: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
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

/// A loaded library: its component, linked and ready to instantiate, and the
/// functions it provides.
struct LoadedLibrary {
    pre: LibraryPre<CallState>,
    functions: Vec<String>,
}

/// The engine and the libraries loaded with `WASM.LOAD`.
pub(crate) struct Host {
    engine: Engine,
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
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        Library::add_to_linker::<CallState, HasSelf<CallState>>(&mut linker, |state| state)?;
        Ok(Self {
            engine,
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

        let mut store = Store::new(&self.engine, CallState::new(ctx));
        let functions = pre.instantiate(&mut store)?.call_functions(&mut store)?;
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
            },
        );
        Ok(functions)
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
        let library = self.functions.get(function)?;
        Some(PreparedCall {
            engine: self.engine.clone(),
            pre: self.libraries[library].pre.clone(),
        })
    }
}

/// A function call ready to run: a fresh instance per call, so calls share no
/// guest state.
pub(crate) struct PreparedCall {
    engine: Engine,
    pre: LibraryPre<CallState>,
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
        let mut store = Store::new(&self.engine, CallState::new(ctx));
        let library = self.pre.instantiate(&mut store)?;
        library.call_invoke(&mut store, function, keys, args)
    }
}
