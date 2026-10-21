//! Libraries in the RDB, so a restart or a replica's full sync gets them back,
//! as Redis does for `FUNCTION` libraries.
//!
//! The libraries are aux data, saved before the keyspace. Aux data belongs to
//! a data type, so the module registers one that no key uses; Redis refuses
//! `MODULE UNLOAD` for a module with a data type. For each library the RDB
//! keeps its name, its component's bytes and its functions. Loading compiles
//! each component again and runs no guest code: a library's first call
//! creates its instance.
//!
//! Loading an RDB or AOF replaces the libraries: they are cleared when loading
//! starts, before the aux data is read.
//!
//! `wasm.persist no`, set when the module loads, leaves all of this off: the
//! data type is not registered, so Redis can unload the module, and libraries
//! live only in memory, untouched by loading.

use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use anyhow::{Context as _, bail};
use redis_module::native_types::RedisType;
use redis_module::server_events::LoadingSubevent;
use redis_module::{EventContext, RedisModuleIO, logging, raw};
use redis_module_macros::loading_event_handler;

use crate::flags;
use crate::host::{Function, FunctionFlags, Host};

/// `wasm.persist`: whether the RDB keeps the libraries. It cannot change once
/// the module has loaded.
pub(crate) static PERSIST: AtomicBool = AtomicBool::new(true);

pub(crate) fn enabled() -> bool {
    PERSIST.load(Ordering::Relaxed)
}

/// The version of the format below, which `aux_load` is passed. Version 0 had
/// no function flags.
const ENCODING_VERSION: i32 = 1;

/// The data type the libraries are saved under.
pub(crate) static LIBRARIES: RedisType = RedisType::new(
    "wasm-libs",
    ENCODING_VERSION,
    raw::RedisModuleTypeMethods {
        version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
        rdb_load: None,
        rdb_save: None,
        aof_rewrite: None,
        mem_usage: None,
        digest: None,
        free: None,
        aux_load: Some(aux_load),
        aux_save: Some(aux_save),
        aux_save_triggers: raw::Aux::Before as c_int,
        free_effort: None,
        unlink: None,
        copy: None,
        defrag: None,
        mem_usage2: None,
        free_effort2: None,
        unlink2: None,
        copy2: None,
        aux_save2: Some(aux_save2),
    },
);

/// Clears the libraries when Redis starts loading an RDB or AOF, which
/// replaces them. An RDB with no libraries has no aux data for this module
/// (see `aux_save2`), so this is what removes a replica's libraries when its
/// primary has none.
#[loading_event_handler]
fn on_loading(_ctx: &EventContext, event: LoadingSubevent) {
    if enabled()
        && matches!(
            event,
            LoadingSubevent::RdbStarted
                | LoadingSubevent::AofStarted
                | LoadingSubevent::ReplStarted
        )
    {
        let _ = crate::with_host(Host::clear);
    }
}

/// Saves the libraries: their count, then each library's name, component and
/// functions, each a name and its flags. Redis before 7.2 calls this one.
unsafe extern "C" fn aux_save(rdb: *mut raw::RedisModuleIO, _when: c_int) {
    // SAFETY: Redis passes the RDB it is saving, for this call only.
    save(&mut unsafe { RedisModuleIO::new(rdb) }, false);
}

/// Like `aux_save`, but saves nothing when there are no libraries, so that a
/// server without the module can load the RDB.
unsafe extern "C" fn aux_save2(rdb: *mut raw::RedisModuleIO, _when: c_int) {
    // SAFETY: as for aux_save.
    save(&mut unsafe { RedisModuleIO::new(rdb) }, true);
}

fn save(rdb: &mut RedisModuleIO, skip_if_empty: bool) {
    let host = crate::host();
    let libraries: Vec<_> = host.iter().flat_map(Host::saved).collect();
    if libraries.is_empty() && skip_if_empty {
        return;
    }
    rdb.write_unsigned(libraries.len() as u64);
    for (name, component, functions) in libraries {
        rdb.write_string(name);
        rdb.write_slice(component);
        rdb.write_unsigned(functions.len() as u64);
        for function in functions {
            rdb.write_string(&function.name);
            rdb.write_unsigned(flags::to_bits(function.flags));
        }
    }
}

unsafe extern "C" fn aux_load(rdb: *mut raw::RedisModuleIO, encver: c_int, _when: c_int) -> c_int {
    // SAFETY: Redis passes the RDB it is loading, for this call only.
    match load(&mut unsafe { RedisModuleIO::new(rdb) }, encver) {
        Ok(()) => raw::Status::Ok as c_int,
        Err(e) => {
            logging::log_warning(format!("wasm: loading libraries failed: {e:#}"));
            raw::Status::Err as c_int
        }
    }
}

/// Reads the libraries `save` wrote, then compiles and adds each one.
fn load(rdb: &mut RedisModuleIO, encver: c_int) -> anyhow::Result<()> {
    if !(0..=ENCODING_VERSION).contains(&encver) {
        bail!("unknown encoding version {encver}");
    }
    let started = Instant::now();
    let mut libraries = Vec::new();
    for _ in 0..rdb.read_unsigned()? {
        let name = rdb.read_string()?;
        let component = rdb.read_string_buffer()?.as_ref().to_vec();
        let functions = (0..rdb.read_unsigned()?)
            .map(|_| {
                let name = rdb.read_string()?;
                let flags = if encver >= 1 {
                    flags::from_bits(rdb.read_unsigned()?)
                } else {
                    FunctionFlags::empty()
                };
                Ok(Function { name, flags })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        libraries.push((name, component, functions));
    }

    let read = started.elapsed();
    let bytes: usize = libraries
        .iter()
        .map(|(_, component, _)| component.len())
        .sum();
    let count = libraries.len();

    // The libraries are added now, and compile on a thread: the server serves
    // meanwhile, and a call to a library still compiling waits for it.
    let compiler = crate::with_host(|host| host.compiler())?;
    let mut jobs = Vec::with_capacity(count);
    for (name, component, functions) in libraries {
        let code =
            crate::with_host(|host| host.restore_compiling(&name, component.clone(), functions))?
                .with_context(|| format!("library '{name}'"))?;
        jobs.push((code, component));
    }
    if count > 0 {
        let jobs = Arc::new(Mutex::new(jobs));
        let compile = {
            let jobs = Arc::clone(&jobs);
            move || {
                let jobs =
                    std::mem::take(&mut *jobs.lock().unwrap_or_else(PoisonError::into_inner));
                for (code, component) in jobs {
                    code.finish(compiler.compile_code(&component));
                }
            }
        };
        let spawned = std::thread::Builder::new()
            .name("wasm-compile".into())
            .spawn(compile.clone());
        if spawned.is_err() {
            // Without a thread, compile here; the load takes longer.
            compile();
        }
        logging::log_notice(format!(
            "wasm: loaded {count} libraries ({bytes} bytes) from the RDB in {} ms; they compile in the background",
            read.as_millis()
        ));
    }
    Ok(())
}
