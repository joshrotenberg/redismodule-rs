use std::alloc::{GlobalAlloc, Layout};
use std::mem::size_of;
use std::ptr;

use crate::raw;

/// Panics with a message without using an allocator.
/// Useful when using the allocator should be avoided or it is
/// inaccessible. The default [std::panic] performs allocations and so
/// will cause a double panic without a meaningful message if the
/// allocator can't be used. This function makes sure we can panic with
/// a reasonable message even without the allocator working.
fn allocation_free_panic(message: &'static str) -> ! {
    // SAFETY: writes the message's bytes to the standard error descriptor.
    let _ = unsafe { libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len()) };

    std::process::abort();
}

const REDIS_ALLOCATOR_NOT_AVAILABLE_MESSAGE: &str = "Critical error: the Redis Allocator isn't available. Outside a Redis server, \
     such as in cargo test, enable the redis-module feature system-allocator-fallback.\n";

/// Picks the allocator once per process for the `system-allocator-fallback`
/// feature.
#[cfg(feature = "system-allocator-fallback")]
mod fallback {
    use std::sync::atomic::{AtomicU8, Ordering};

    use crate::raw;

    const UNDECIDED: u8 = 0;
    const REDIS: u8 = 1;
    const SYSTEM: u8 = 2;

    static SOURCE: AtomicU8 = AtomicU8::new(UNDECIDED);

    /// Whether this process allocates from the system allocator. The first call
    /// decides: the system allocator if `RedisModule_Alloc` is not set then,
    /// Redis's if it is. Later calls return the same answer, so every block is
    /// freed by the allocator that allocated it.
    pub(super) fn use_system() -> bool {
        match SOURCE.load(Ordering::Acquire) {
            UNDECIDED => {
                // SAFETY: a read of the API pointer, which Redis sets once in
                // RedisModule_Init.
                let source = if unsafe { raw::RedisModule_Alloc }.is_some() {
                    REDIS
                } else {
                    SYSTEM
                };
                match SOURCE.compare_exchange(
                    UNDECIDED,
                    source,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => source == SYSTEM,
                    Err(decided) => decided == SYSTEM,
                }
            }
            decided => decided == SYSTEM,
        }
    }
}

/// The alignment `RedisModule_Alloc` guarantees, like `malloc`: 16 bytes on
/// 64-bit targets and 8 bytes on 32-bit targets.
///
/// Redis returns the underlying allocator's pointer unchanged when it is built
/// with jemalloc, tcmalloc, or a libc that provides `malloc_size` or
/// `malloc_usable_size` (macOS, glibc, FreeBSD). Rounding the requested size
/// up does not change where that pointer lands.
const MALLOC_ALIGN: usize = 2 * size_of::<usize>();

/// Space reserved below an over-aligned block for the pointer Redis returned.
const HEADER: usize = size_of::<*mut u8>();

/// Whether `layout` needs more alignment than `RedisModule_Alloc` guarantees.
fn is_over_aligned(layout: Layout) -> bool {
    layout.align() > MALLOC_ALIGN
}

/// The size to request for a layout that `RedisModule_Alloc` aligns itself.
///
/// Rounding the size up to the alignment keeps small requests out of size
/// classes that are aligned to less than `layout.align()`, such as jemalloc's
/// 8-byte class.
fn direct_size(layout: Layout) -> usize {
    layout.size().next_multiple_of(layout.align())
}

/// The size to request for an over-aligned layout: the header, the block, and
/// up to `align - 1` bytes of padding to move the block to an aligned address.
fn over_aligned_size(layout: Layout) -> Option<usize> {
    layout.size().checked_add(HEADER + layout.align() - 1)
}

/// The offset from `base` to the first address at or above `base + HEADER`
/// that is a multiple of `align`. It is at least `HEADER` and less than
/// `HEADER + align`, whatever the alignment of `base`.
fn over_aligned_offset(base: usize, align: usize) -> usize {
    (base + HEADER).next_multiple_of(align) - base
}

/// Allocates memory for `layout` from `alloc`, which has `malloc` semantics.
///
/// # Safety
///
/// `layout` must have a non-zero size, and `alloc` must return null or a
/// pointer to at least the requested number of bytes, aligned to
/// [`MALLOC_ALIGN`] for requests of at least that size.
unsafe fn alloc_with(layout: Layout, alloc: impl FnOnce(usize) -> *mut u8) -> *mut u8 {
    if !is_over_aligned(layout) {
        return alloc(direct_size(layout));
    }

    let Some(size) = over_aligned_size(layout) else {
        return ptr::null_mut();
    };
    let base = alloc(size);
    if base.is_null() {
        return base;
    }

    // SAFETY: the block starts at least HEADER bytes into the allocation and
    // ends within it, and it is aligned to more than HEADER, so the header slot
    // below it is in bounds and aligned for a pointer.
    unsafe {
        let block = base.add(over_aligned_offset(base as usize, layout.align()));
        block.sub(HEADER).cast::<*mut u8>().write(base);
        block
    }
}

/// Releases a block returned by [`alloc_with`] for the same `layout`, passing
/// the pointer that `alloc` returned to `free`.
///
/// # Safety
///
/// `block` must come from [`alloc_with`] with the same `layout` and must not
/// have been released already.
unsafe fn dealloc_with(block: *mut u8, layout: Layout, free: impl FnOnce(*mut u8)) {
    if !is_over_aligned(layout) {
        free(block);
    } else {
        // SAFETY: alloc_with wrote the allocation's base pointer in the header
        // slot below an over-aligned block.
        free(unsafe { block.sub(HEADER).cast::<*mut u8>().read() });
    }
}

/// Whether [`RedisAlloc`] allocates from the system allocator in this process,
/// which only the `system-allocator-fallback` feature allows. `redis_module!`
/// fails the module load when it does, since Redis would not count that memory.
#[doc(hidden)]
pub fn __uses_system_allocator() -> bool {
    #[cfg(feature = "system-allocator-fallback")]
    {
        fallback::use_system()
    }
    #[cfg(not(feature = "system-allocator-fallback"))]
    {
        false
    }
}

/// Defines the Redis allocator. This allocator delegates the allocation
/// and deallocation tasks to the Redis server when available, otherwise
/// it aborts the process.
///
/// `RedisModule_Alloc` aligns memory like `malloc`. Layouts that need more
/// alignment are over-allocated and aligned within the allocation, so every
/// alignment that [`Layout`] accepts is honored.
///
/// # Outside a Redis server
///
/// `redis_module!` installs this allocator as the global allocator, so a
/// module's `cargo test` aborts at its first allocation: no server has set
/// `RedisModule_Alloc`. The `system-allocator-fallback` feature makes the first
/// allocation pick the allocator for the rest of the process: the system
/// allocator if `RedisModule_Alloc` is not set at that point, Redis's if it is.
///
/// Enable it for tests only, through `[dev-dependencies]`. With Cargo's feature
/// resolver 2, `cargo build` then builds the module without it; `cargo test` and
/// `cargo build --examples` build it with the feature, which the next paragraph
/// covers:
///
/// ```toml
/// [dependencies]
/// redis-module = "2"
///
/// [dev-dependencies]
/// redis-module = { version = "2", features = ["system-allocator-fallback"] }
/// ```
///
/// A module built with the feature still uses Redis's allocator when Redis
/// loads it. If something in the module allocates before `RedisModule_OnLoad`,
/// such as a constructor, the system allocator is picked first, and
/// `redis_module!` fails the module load.
#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RedisAlloc;

unsafe impl GlobalAlloc for RedisAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        #[cfg(feature = "system-allocator-fallback")]
        if fallback::use_system() {
            // SAFETY: the caller's GlobalAlloc contract carries over.
            return unsafe { std::alloc::System.alloc(layout) };
        }
        // SAFETY: Redis sets the API pointers once, before the module runs.
        match unsafe { raw::RedisModule_Alloc } {
            // SAFETY: the caller's GlobalAlloc contract gives a non-zero
            // size; RedisModule_Alloc is callable once set and has malloc
            // semantics and alignment, as alloc_with requires.
            Some(alloc) => unsafe { alloc_with(layout, |size| alloc(size).cast()) },
            None => allocation_free_panic(REDIS_ALLOCATOR_NOT_AVAILABLE_MESSAGE),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        #[cfg(feature = "system-allocator-fallback")]
        if fallback::use_system() {
            // SAFETY: the caller's GlobalAlloc contract carries over.
            return unsafe { std::alloc::System.dealloc(ptr, layout) };
        }
        // SAFETY: Redis sets the API pointers once, before the module runs.
        match unsafe { raw::RedisModule_Free } {
            // SAFETY: GlobalAlloc passes a block this allocator returned for
            // `layout`, and its base came from RedisModule_Alloc.
            Some(free) => unsafe { dealloc_with(ptr, layout, |base| free(base.cast())) },
            None => allocation_free_panic(REDIS_ALLOCATOR_NOT_AVAILABLE_MESSAGE),
        };
    }
}

/// The memory Redis allocated for `ptr`, which may be more than was asked
/// for, through `RedisModule_MallocSize`.
///
/// # Errors
///
/// Returns an error if the running Redis does not provide the API.
///
/// # Safety
///
/// `ptr` must be a live allocation that `RedisModule_Alloc`, `RedisModule_Calloc`
/// or `RedisModule_Realloc` returned. For a block from [`RedisAlloc`], use
/// [`redis_alloc_size`].
pub unsafe fn malloc_size(ptr: *mut std::ffi::c_void) -> Result<usize, crate::RedisError> {
    // SAFETY: Redis sets the API pointers once, before the module runs.
    let malloc_size = unsafe { crate::__available_api!(RedisModule_MallocSize) };
    // SAFETY: the caller passes a live Redis allocation.
    Ok(unsafe { malloc_size(ptr) })
}

/// The usable size of `ptr`, through `RedisModule_MallocUsableSize`.
///
/// # Errors
///
/// Returns an error if the running Redis does not provide the API.
///
/// # Safety
///
/// The requirements of [`malloc_size`] apply.
pub unsafe fn malloc_usable_size(ptr: *mut std::ffi::c_void) -> Result<usize, crate::RedisError> {
    // SAFETY: Redis sets the API pointers once, before the module runs.
    let malloc_usable_size = unsafe { crate::__available_api!(RedisModule_MallocUsableSize) };
    // SAFETY: the caller passes a live Redis allocation.
    Ok(unsafe { malloc_usable_size(ptr) })
}

/// The memory Redis allocated for a block that [`RedisAlloc`] returned for
/// `layout`, for example the buffer of a `Vec` or a `Box` in a module that uses
/// `RedisAlloc` as its global allocator. A block with more alignment than
/// `RedisModule_Alloc` provides starts inside a larger allocation; this measures
/// that allocation.
///
/// # Errors
///
/// Returns an error if the running Redis does not provide `RedisModule_MallocSize`,
/// or if the module allocates from the system allocator under the
/// `system-allocator-fallback` feature.
///
/// # Safety
///
/// `block` must be a live block that [`RedisAlloc`] returned for `layout`, with
/// a non-zero size.
pub unsafe fn redis_alloc_size(
    block: *const u8,
    layout: Layout,
) -> Result<usize, crate::RedisError> {
    // SAFETY: Redis sets the API pointers once, before the module runs.
    let malloc_size = unsafe { crate::__available_api!(RedisModule_MallocSize) };
    // A block from the system allocator, under the system-allocator-fallback
    // feature, has no header and is not Redis's to measure.
    if __uses_system_allocator() {
        return Err(crate::RedisError::Str(
            "ERR the module allocates from the system allocator, not from Redis",
        ));
    }
    let base = if is_over_aligned(layout) {
        // SAFETY: RedisAlloc wrote the allocation's base in the header slot
        // below an over-aligned block.
        unsafe { block.sub(HEADER).cast::<*mut u8>().read() }
    } else {
        block.cast_mut()
    };
    // SAFETY: base is the live allocation RedisModule_Alloc returned.
    Ok(unsafe { malloc_size(base.cast()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    /// Alignments above [`MALLOC_ALIGN`] up to 64 KiB.
    fn over_alignments() -> impl Iterator<Item = usize> {
        (MALLOC_ALIGN.trailing_zeros() + 1..=16).map(|shift| 1 << shift)
    }

    #[test]
    fn layouts_up_to_malloc_align_are_not_over_aligned() {
        for align in [1, 2, 4, 8, MALLOC_ALIGN] {
            assert!(!is_over_aligned(Layout::from_size_align(1, align).unwrap()));
        }
        assert!(is_over_aligned(
            Layout::from_size_align(1, MALLOC_ALIGN * 2).unwrap()
        ));
    }

    #[test]
    fn offset_is_aligned_and_leaves_room_for_the_header() {
        for align in over_alignments() {
            // Every base in two alignment periods, including unaligned ones.
            for base in 0x10000..0x10000 + 2 * align {
                let offset = over_aligned_offset(base, align);
                assert!(offset >= HEADER, "base {base:#x} align {align}");
                assert!(offset < HEADER + align, "base {base:#x} align {align}");
                assert_eq!((base + offset) % align, 0, "base {base:#x} align {align}");
            }
        }
    }

    #[test]
    fn over_aligned_size_covers_the_largest_offset() {
        for align in over_alignments() {
            for size in [1, align - 1, align, 3 * align] {
                let layout = Layout::from_size_align(size, align).unwrap();
                let largest_offset = HEADER + align - 1;
                assert_eq!(over_aligned_size(layout), Some(largest_offset + size));
            }
        }
    }

    #[test]
    fn over_aligned_size_does_not_overflow_for_the_largest_layout() {
        let align = 1 << 16;
        let layout = Layout::from_size_align(isize::MAX as usize - (align - 1), align).unwrap();
        assert!(over_aligned_size(layout).is_some());
    }

    #[test]
    fn direct_path_rounds_the_size_and_returns_the_pointer_unchanged() {
        let backing = Layout::from_size_align(64, MALLOC_ALIGN).unwrap();
        for (size, align) in [(1, 1), (3, 2), (5, 4), (4, 8), (8, 8), (1, MALLOC_ALIGN)] {
            let layout = Layout::from_size_align(size, align).unwrap();
            unsafe {
                let base = System.alloc(backing);
                let mut requested = 0;
                let block = alloc_with(layout, |n| {
                    requested = n;
                    base
                });
                assert_eq!(block, base);
                assert_eq!(requested, size.next_multiple_of(align));

                let mut freed = ptr::null_mut();
                dealloc_with(block, layout, |p| freed = p);
                assert_eq!(freed, base);
                System.dealloc(base, backing);
            }
        }
    }

    /// Allocates `layout` through [`alloc_with`] from a backend whose base
    /// pointer sits `base_offset` bytes past a 64 KiB boundary, checks the
    /// block, and releases it through [`dealloc_with`].
    fn round_trip(layout: Layout, base_offset: usize) {
        const BACKING_ALIGN: usize = 1 << 16;
        unsafe {
            let mut backing = None;
            let mut requested = 0;
            let block = alloc_with(layout, |n| {
                let outer = Layout::from_size_align(base_offset + n, BACKING_ALIGN).unwrap();
                let start = System.alloc(outer);
                assert!(!start.is_null());
                backing = Some((start, outer));
                requested = n;
                start.add(base_offset)
            });
            let (start, outer) = backing.unwrap();
            let base = start.add(base_offset);

            let at = format!(
                "align {} size {} base offset {base_offset}",
                layout.align(),
                layout.size()
            );
            assert_eq!(block as usize % layout.align(), 0, "{at}");
            assert!(block as usize >= base as usize + HEADER, "{at}");
            assert!(
                block as usize + layout.size() <= base as usize + requested,
                "{at}"
            );
            block.write_bytes(0xa5, layout.size());

            let mut freed = ptr::null_mut();
            dealloc_with(block, layout, |p| freed = p);
            assert_eq!(freed, base, "{at}");
            System.dealloc(start, outer);
        }
    }

    #[test]
    fn over_aligned_blocks_round_trip_from_misaligned_bases() {
        for align in over_alignments() {
            for size in [1, align, 3 * align] {
                let layout = Layout::from_size_align(size, align).unwrap();
                for base_offset in [1, HEADER, MALLOC_ALIGN, align / 2, align - MALLOC_ALIGN] {
                    round_trip(layout, base_offset);
                }
            }
        }
    }

    #[test]
    fn over_aligned_alloc_returns_null_when_the_backend_does() {
        let layout = Layout::from_size_align(64, 64).unwrap();
        let block = unsafe { alloc_with(layout, |_| ptr::null_mut()) };
        assert!(block.is_null());
    }
}
