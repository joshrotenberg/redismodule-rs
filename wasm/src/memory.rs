//! Guest memory: a limit on each instance's linear memory, the totals the
//! `INFO wasm` section reports, and the check that refuses a call when Redis is
//! out of memory.
//!
//! Linear memory is allocated outside the Redis allocator, so `used_memory` and
//! `maxmemory` do not count it; `INFO wasm` is where it shows.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use anyhow::{Context as _, bail};
use redis_module::{InfoContext, RedisResult};
use redis_module_macros::info_command_handler;
use wasmtime::ResourceLimiter;

use crate::{
    budget::{self, Lease},
    host::Host,
};

/// The default for `wasm.max-memory`.
pub(crate) const DEFAULT_MAX_MEMORY: i64 = 64 << 20;

/// `wasm.max-memory`: the most linear memory, over all its memories, that one
/// instance may use. A guest that grows past it traps, and its call fails.
pub(crate) static MAX_MEMORY: AtomicI64 = AtomicI64::new(DEFAULT_MAX_MEMORY);

/// Live instances: kept ones, and ones running a call.
static INSTANCES: AtomicUsize = AtomicUsize::new(0);

/// A store's linear memory, held to `MAX_MEMORY`. wasmtime asks it before
/// creating or growing a memory.
pub(crate) struct Memory {
    lease: Lease,
    /// The last growth allowed, taken back if the growth then fails.
    last_growth: usize,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            lease: budget::GUEST.lease(),
            last_growth: 0,
        }
    }
}

impl Memory {
    /// Keep a second owner OUTSIDE Store data. Wasmtime drops Store data
    /// before deallocating its memories; the outer owner releases afterward.
    pub(crate) fn lease(&self) -> Lease {
        self.lease.clone()
    }
}

impl ResourceLimiter for Memory {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        let limit = usize::try_from(MAX_MEMORY.load(Ordering::Relaxed)).unwrap_or(0);
        self.last_growth = 0;
        let growth = desired
            .checked_sub(current)
            .context("guest memory cannot shrink")?;
        let after = self
            .lease
            .bytes()
            .checked_add(growth)
            .context("instance memory size overflow")?;
        if after > limit {
            bail!(
                "the instance would use {after} bytes of memory, more than wasm.max-memory ({limit})"
            );
        }
        self.lease.grow(growth)?;
        self.last_growth = growth;
        Ok(true)
    }

    fn memory_grow_failed(&mut self, _error: anyhow::Error) -> anyhow::Result<()> {
        self.lease.rollback(self.last_growth)?;
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
    /// Libraries whose component is still compiling after an RDB load.
    pub(crate) compiling: usize,
}

/// `INFO wasm`.
#[info_command_handler]
fn info(ctx: &InfoContext, _for_crash_report: bool) -> RedisResult<()> {
    // A crash report can ask for INFO while this thread holds the host, so
    // the library sizes are left out rather than waited for.
    let sizes = crate::try_with_host(Host::sizes).unwrap_or_default();
    let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    let compiler = crate::compiler::diagnostics();
    let (guest_limit, guest_used) = budget::GUEST.snapshot();
    let (code_limit, code_used) = budget::CODE.snapshot();
    ctx.builder()
        .add_section("")
        .field("libraries", count(sizes.libraries))?
        .field("functions", count(sizes.functions))?
        .field("component_bytes", count(sizes.component_bytes))?
        .field("code_bytes", count(sizes.code_bytes))?
        .field("compiling", count(sizes.compiling))?
        .field("instances", count(INSTANCES.load(Ordering::Relaxed)))?
        .field("instance_memory_bytes", guest_used)?
        .field("max_memory", MAX_MEMORY.load(Ordering::Relaxed))?
        .field("guest_memory_budget", guest_limit)?
        .field("guest_memory_used", guest_used)?
        .field("code_budget", code_limit)?
        .field("code_used", code_used)?
        .field("compiler_active", compiler[0])?
        .field("compiler_queued", compiler[1])?
        .field("compiler_completed", compiler[2])?
        .field("compiler_failed", compiler[3])?
        .field("compiler_baseline_vm", compiler[4])?
        .field("compiler_ceiling_vm", compiler[5])?
        .field("compiler_cpu_ms", compiler[6])?
        .field("compiler_uncertain", compiler[7])?
        .build_section()?
        .build_info()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{Budget, Retained};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use wasmtime::{Memory as NativeMemory, MemoryType, Store};

    const PAGE: usize = 65536;

    struct Data {
        memory: Memory,
        budget: &'static Budget,
        dropped: Arc<AtomicBool>,
    }

    impl Drop for Data {
        fn drop(&mut self) {
            // Wasmtime destroys data before StoreOpaque. The same Retained
            // used in production must keep admission until Store finishes.
            assert!(self.budget.snapshot().1 > 0);
            self.dropped.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn real_store_growth_rolls_back_and_outer_owner_outlives_early_data_drop() {
        let budget = Budget::for_test(8 * PAGE);
        let dropped = Arc::new(AtomicBool::new(false));
        let memory = Memory {
            lease: budget.lease(),
            last_growth: 0,
        };
        let owner = memory.lease();
        let data = Data {
            memory,
            budget,
            dropped: Arc::clone(&dropped),
        };
        let engine = crate::compiler_engine::engine().unwrap();
        let mut runtime = Retained {
            resource: Store::new(&engine, data),
            _owners: owner,
        };
        let store = &mut runtime.resource;
        store.limiter(|data| &mut data.memory);
        let first = NativeMemory::new(&mut *store, MemoryType::new(1, Some(3))).unwrap();
        let _second = NativeMemory::new(&mut *store, MemoryType::new(2, Some(2))).unwrap();
        assert_eq!(budget.snapshot().1, (3 * PAGE) as i64);
        first.grow(&mut *store, 2).unwrap();
        assert_eq!(budget.snapshot().1, (5 * PAGE) as i64);
        // Limiter accepts one page; the native maximum refuses it afterward.
        assert!(first.grow(&mut *store, 1).is_err());
        assert_eq!(budget.snapshot().1, (5 * PAGE) as i64);
        assert_eq!(store.data().memory.last_growth, 0);
        assert!(first.grow(&mut *store, u64::MAX).is_err());
        assert_eq!(budget.snapshot().1, (5 * PAGE) as i64);
        first.grow(&mut *store, 0).unwrap();
        assert_eq!(budget.snapshot().1, (5 * PAGE) as i64);
        budget.limit_for_test((5 * PAGE) as i64);
        assert!(NativeMemory::new(&mut *store, MemoryType::new(1, None)).is_err());
        assert_eq!(budget.snapshot().1, (5 * PAGE) as i64);
        drop(runtime);
        assert!(dropped.load(Ordering::Relaxed));
        assert_eq!(budget.snapshot().1, 0);
    }

    struct Creator {
        budget: &'static Budget,
        data_dropped: Arc<AtomicBool>,
        memory_drops: Arc<AtomicUsize>,
        expect_data_dropped: bool,
        calls: AtomicUsize,
        fail_on: usize,
    }

    struct ObservedMemory {
        address: usize,
        size: usize,
        capacity: usize,
        mapping: usize,
        budget: &'static Budget,
        data_dropped: Arc<AtomicBool>,
        memory_drops: Arc<AtomicUsize>,
        expect_data_dropped: bool,
    }

    // SAFETY: test-only anonymous page-aligned mapping, initially zero-filled,
    // stable through capacity, with an inaccessible guard. Only Wasmtime may
    // access/mutate it; no host aliases are exposed.
    unsafe impl wasmtime::LinearMemory for ObservedMemory {
        fn byte_size(&self) -> usize {
            self.size
        }
        fn byte_capacity(&self) -> usize {
            self.capacity
        }
        fn as_ptr(&self) -> *mut u8 {
            self.address as *mut u8
        }
        fn grow_to(&mut self, size: usize) -> anyhow::Result<()> {
            anyhow::ensure!(size <= self.capacity, "observer capacity exceeded");
            anyhow::ensure!(
                unsafe {
                    libc::mprotect(
                        self.as_ptr().cast(),
                        size,
                        libc::PROT_READ | libc::PROT_WRITE,
                    )
                } == 0,
                "observer growth failed"
            );
            self.size = size;
            Ok(())
        }
    }

    impl Drop for ObservedMemory {
        fn drop(&mut self) {
            // Actual native destruction precedes final lease release. A start
            // trap retains memory past data drop; failed second allocation
            // destroys its partial first memory earlier, with the provisional
            // charge conservatively kept until the Store also drops.
            assert_eq!(
                self.data_dropped.load(Ordering::Relaxed),
                self.expect_data_dropped
            );
            assert!(self.budget.snapshot().1 > 0);
            assert_eq!(
                unsafe { libc::munmap((self.address as *mut u8).cast(), self.mapping) },
                0
            );
            assert!(self.budget.snapshot().1 > 0);
            self.memory_drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: the returned memory satisfies the LinearMemory contract above,
    // honors all reserved/guard sizes and is owned solely by the test Store.
    unsafe impl wasmtime::MemoryCreator for Creator {
        fn new_memory(
            &self,
            _: MemoryType,
            minimum: usize,
            maximum: Option<usize>,
            reserved: Option<usize>,
            guard: usize,
        ) -> Result<Box<dyn wasmtime::LinearMemory>, String> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if call == self.fail_on {
                return Err("injected initial allocation refusal".into());
            }
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            let capacity = reserved
                .unwrap_or_else(|| maximum.unwrap_or(minimum))
                .max(minimum);
            let capacity =
                budget::rounded_image_bytes(capacity.max(page), page).map_err(|e| e.to_string())?;
            let mapping = capacity
                .checked_add(guard.max(page))
                .ok_or("observer mapping overflow")?;
            let pointer = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    mapping,
                    libc::PROT_NONE,
                    libc::MAP_ANON | libc::MAP_PRIVATE,
                    -1,
                    0,
                )
            };
            if pointer == libc::MAP_FAILED {
                return Err("observer mmap failed".into());
            }
            if minimum > 0
                && unsafe { libc::mprotect(pointer, minimum, libc::PROT_READ | libc::PROT_WRITE) }
                    != 0
            {
                unsafe { libc::munmap(pointer, mapping) };
                return Err("observer mprotect failed".into());
            }
            Ok(Box::new(ObservedMemory {
                address: pointer as usize,
                size: minimum,
                capacity,
                mapping,
                budget: self.budget,
                data_dropped: Arc::clone(&self.data_dropped),
                memory_drops: Arc::clone(&self.memory_drops),
                expect_data_dropped: self.expect_data_dropped,
            }))
        }
    }

    #[test]
    fn failed_initial_allocation_and_start_trap_keep_charge_through_actual_memory_destruction() {
        for start_trap in [false, true] {
            let budget = Budget::for_test(8 * PAGE);
            let dropped = Arc::new(AtomicBool::new(false));
            let memory_drops = Arc::new(AtomicUsize::new(0));
            let creator = Arc::new(Creator {
                budget,
                data_dropped: Arc::clone(&dropped),
                memory_drops: Arc::clone(&memory_drops),
                expect_data_dropped: start_trap,
                calls: AtomicUsize::new(0),
                fail_on: if start_trap { usize::MAX } else { 2 },
            });
            let mut config = crate::compiler_engine::config();
            config.with_host_memory(creator);
            // Observer only; production retains Wasmtime's normal mmap path.
            config.memory_init_cow(false);
            let engine = wasmtime::Engine::new(&config).unwrap();
            let memory = Memory {
                lease: budget.lease(),
                last_growth: 0,
            };
            let owner = memory.lease();
            let data = Data {
                memory,
                budget,
                dropped: Arc::clone(&dropped),
            };
            let mut runtime = Retained {
                resource: Store::new(&engine, data),
                _owners: owner,
            };
            let store = &mut runtime.resource;
            store.limiter(|data| &mut data.memory);
            store.set_epoch_deadline(1);
            let error = if start_trap {
                // Valid core wasm: (module (memory 1) (func unreachable) (start 0)).
                let bytes = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0\x03\x02\x01\0\x05\x03\x01\0\x01\x08\x01\0\x0a\x05\x01\x03\0\0\x0b";
                let module = wasmtime::Module::new(&engine, bytes).unwrap();
                let error = wasmtime::Instance::new(&mut *store, &module, &[])
                    .err()
                    .unwrap();
                assert!(error.downcast_ref::<wasmtime::WasmBacktrace>().is_none());
                assert!(format!("{error:#}").contains("unreachable"), "{error:#}");
                assert_eq!(budget.snapshot().1, PAGE as i64);
                error
            } else {
                // The engine's test creator applies to module allocations;
                // standalone Memory::new uses a different native allocator.
                // Valid core wasm: (module (memory 1) (memory 1)).
                let bytes = b"\0asm\x01\0\0\0\x05\x05\x02\0\x01\0\x01";
                let module = wasmtime::Module::new(&engine, bytes).unwrap();
                let error = wasmtime::Instance::new(&mut *store, &module, &[])
                    .err()
                    .unwrap();
                assert!(format!("{error:#}").contains("injected initial allocation refusal"));
                // Initial allocation has no failed-growth callback: its
                // provisional delta stays owned until the failed Store dies.
                assert_eq!(budget.snapshot().1, (2 * PAGE) as i64);
                error
            };
            drop(runtime);
            assert_eq!(memory_drops.load(Ordering::Relaxed), 1);
            assert_eq!(budget.snapshot().1, 0);
            // Keeping the trap error alive must not keep an unpaired image
            // backtrace/Module alive under the production engine policy.
            assert!(error.downcast_ref::<wasmtime::WasmBacktrace>().is_none());
            drop(error);
        }
    }
}
