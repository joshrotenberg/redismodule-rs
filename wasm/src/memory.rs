//! Guest memory: a limit on each instance's linear memory, the totals the
//! `INFO wasm` section reports, and the check that refuses a call when Redis is
//! out of memory.
//!
//! Linear memory is allocated outside the Redis allocator, so `used_memory` and
//! `maxmemory` do not count it; `INFO wasm` is where it shows.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use anyhow::bail;
use redis_module::{InfoContext, RedisResult};
use redis_module_macros::info_command_handler;
use wasmtime::ResourceLimiter;

use crate::host::Host;

/// The default for `wasm.max-memory`.
pub(crate) const DEFAULT_MAX_MEMORY: i64 = 64 << 20;

/// `wasm.max-memory`: the most linear memory, over all its memories, that one
/// instance may use. A guest that grows past it traps, and its call fails.
pub(crate) static MAX_MEMORY: AtomicI64 = AtomicI64::new(DEFAULT_MAX_MEMORY);

/// Linear memory over all live instances, in bytes.
static INSTANCE_MEMORY: AtomicUsize = AtomicUsize::new(0);
/// Live instances: kept ones, and ones running a call.
static INSTANCES: AtomicUsize = AtomicUsize::new(0);

/// A store's linear memory, held to `MAX_MEMORY`. wasmtime asks it before
/// creating or growing a memory.
#[derive(Default)]
pub(crate) struct Memory {
    bytes: usize,
    /// The last growth allowed, taken back if the growth then fails.
    last_growth: usize,
}

impl ResourceLimiter for Memory {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        let limit = usize::try_from(MAX_MEMORY.load(Ordering::Relaxed)).unwrap_or(0);
        let growth = desired.saturating_sub(current);
        let after = self.bytes + growth;
        if after > limit {
            bail!(
                "the instance would use {after} bytes of memory, more than wasm.max-memory ({limit})"
            );
        }
        self.bytes = after;
        self.last_growth = growth;
        INSTANCE_MEMORY.fetch_add(growth, Ordering::Relaxed);
        Ok(true)
    }

    fn memory_grow_failed(&mut self, _error: anyhow::Error) -> anyhow::Result<()> {
        self.bytes -= self.last_growth;
        INSTANCE_MEMORY.fetch_sub(self.last_growth, Ordering::Relaxed);
        self.last_growth = 0;
        Ok(())
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        INSTANCE_MEMORY.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Counts an instance as live until dropped.
pub(crate) struct Live(());

impl Live {
    pub(crate) fn new() -> Self {
        INSTANCES.fetch_add(1, Ordering::Relaxed);
        Self(())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        INSTANCES.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What the libraries take: their components' bytes and their compiled code.
#[derive(Default)]
pub(crate) struct LibrarySizes {
    pub(crate) libraries: usize,
    pub(crate) functions: usize,
    pub(crate) component_bytes: usize,
    pub(crate) code_bytes: usize,
}

/// `INFO wasm`.
#[info_command_handler]
fn info(ctx: &InfoContext, _for_crash_report: bool) -> RedisResult<()> {
    // A crash report can ask for INFO while this thread holds the host, so
    // the library sizes are left out rather than waited for.
    let sizes = crate::try_with_host(Host::sizes).unwrap_or_default();
    let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    ctx.builder()
        .add_section("")
        .field("libraries", count(sizes.libraries))?
        .field("functions", count(sizes.functions))?
        .field("component_bytes", count(sizes.component_bytes))?
        .field("code_bytes", count(sizes.code_bytes))?
        .field("instances", count(INSTANCES.load(Ordering::Relaxed)))?
        .field(
            "instance_memory_bytes",
            count(INSTANCE_MEMORY.load(Ordering::Relaxed)),
        )?
        .field("max_memory", MAX_MEMORY.load(Ordering::Relaxed))?
        .build_section()?
        .build_info()?;
    Ok(())
}
