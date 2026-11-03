//! Checks alignment through `RedisAlloc` and demonstrates recoverable reserve
//! failures on servers with TryAlloc. Older servers skip the failure probe.

use redis_module::{Context, RedisError, RedisResult, RedisString, RedisValue, raw, redis_module};
use std::alloc::Layout;
use std::hint::black_box;

#[repr(align(16))]
struct Align16 {
    _bytes: [u8; 16],
}

#[repr(align(32))]
struct Align32 {
    _bytes: [u8; 32],
}

#[repr(align(64))]
struct Align64 {
    _bytes: [u8; 64],
}

#[repr(align(128))]
struct Align128 {
    _bytes: [u8; 128],
}

#[repr(align(256))]
struct Align256x3 {
    _bytes: [u8; 768],
}

#[repr(align(1024))]
struct Align1024x3 {
    _bytes: [u8; 3072],
}

#[repr(align(4096))]
struct Align4096 {
    _bytes: [u8; 4096],
}

#[repr(align(32768))]
struct Align32768 {
    _bytes: [u8; 32768],
}

/// Allocates `count` boxes of `T`, keeping all of them alive until the end so
/// that each one gets a distinct address, and returns `[align, size, misaligned]`.
///
/// The boxes are only inspected as raw pointers: dereferencing a misaligned
/// box panics in debug builds, which would abort the server.
fn check<T>(make: fn() -> T, count: usize) -> RedisValue {
    let align = std::mem::align_of::<T>();
    let mut pointers: Vec<*mut T> = Vec::with_capacity(count);
    let mut misaligned = 0;
    for _ in 0..count {
        let pointer = Box::into_raw(Box::new(make()));
        // black_box stops the compiler from assuming the allocator honored the alignment.
        if !(black_box(pointer) as usize).is_multiple_of(align) {
            misaligned += 1;
        }
        pointers.push(pointer);
    }
    for pointer in pointers {
        // SAFETY: every pointer came from `Box::into_raw` above and is released once.
        drop(unsafe { Box::from_raw(pointer) });
    }
    RedisValue::Array(vec![
        RedisValue::Integer(align as i64),
        RedisValue::Integer(std::mem::size_of::<T>() as i64),
        RedisValue::Integer(misaligned),
    ])
}

fn alloc_alignment_check(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(vec![
        check(|| Align16 { _bytes: [0; 16] }, 1000),
        check(|| Align32 { _bytes: [0; 32] }, 1000),
        check(|| Align64 { _bytes: [0; 64] }, 1000),
        check(|| Align128 { _bytes: [0; 128] }, 1000),
        check(|| Align256x3 { _bytes: [0; 768] }, 1000),
        check(|| Align1024x3 { _bytes: [0; 3072] }, 1000),
        check(|| Align4096 { _bytes: [0; 4096] }, 500),
        check(|| Align32768 { _bytes: [0; 32768] }, 200),
    ]))
}

fn fallible_available(_: &Context, _: Vec<RedisString>) -> RedisResult {
    // SAFETY: the module has initialized its native API bindings.
    Ok(i64::from(unsafe { raw::RedisModule_TryAlloc }.is_some()).into())
}

fn fallible_reserve(_: &Context, _: Vec<RedisString>) -> RedisResult {
    // Never run an exhaustion probe through the older aborting allocator.
    if unsafe { raw::RedisModule_TryAlloc }.is_none() {
        return Err(RedisError::Str(
            "ERR recoverable allocation requires TryAlloc",
        ));
    }
    // Maximum valid Rust layouts reach the native allocator, which refuses
    // these oversized requests. Vec's capacity arithmetic must not reject
    // them before Redis has a chance to return null.
    let huge = isize::MAX as usize;
    let fresh_failed = Vec::<u8>::new().try_reserve_exact(huge).is_err();

    let mut bytes = vec![1_u8, 2, 3, 4];
    let capacity = bytes.capacity();
    let additional = huge - bytes.len();
    Layout::array::<u8>(bytes.len() + additional)?;
    let grow_failed = bytes.try_reserve_exact(additional).is_err();
    let bytes_preserved = bytes == [1, 2, 3, 4] && bytes.capacity() == capacity;
    bytes.try_reserve_exact(128)?;
    let grow_ok = bytes == [1, 2, 3, 4];

    // u128 exercises the header/padding path on 64-bit platforms too.
    let mut aligned = vec![1_u128, 2, 3, 4];
    let capacity = aligned.capacity();
    let additional = huge / std::mem::size_of::<u128>() - aligned.len();
    Layout::array::<u128>(aligned.len() + additional)?;
    let aligned_failed = aligned.try_reserve_exact(additional).is_err();
    let aligned_preserved = aligned == [1, 2, 3, 4] && aligned.capacity() == capacity;
    aligned.try_reserve_exact(128)?;
    let aligned_grow_ok = aligned == [1, 2, 3, 4]
        && (black_box(aligned.as_ptr()) as usize).is_multiple_of(std::mem::align_of::<u128>());

    let layout = Layout::from_size_align(1024, 16)?;
    // SAFETY: layout is nonzero; check allocation before reading and free it
    // once with the original layout.
    let zeroed = unsafe {
        let block = std::alloc::alloc_zeroed(layout);
        if block.is_null() {
            false
        } else {
            let zeroed = std::slice::from_raw_parts(block, layout.size())
                .iter()
                .all(|byte| *byte == 0);
            std::alloc::dealloc(block, layout);
            zeroed
        }
    };
    Ok(vec![
        i64::from(fresh_failed),
        i64::from(grow_failed),
        i64::from(bytes_preserved),
        i64::from(grow_ok),
        i64::from(aligned_failed),
        i64::from(aligned_preserved),
        i64::from(aligned_grow_ok),
        i64::from(zeroed),
    ]
    .into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "alloc_alignment",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["alloc_alignment.check", alloc_alignment_check, "readonly", 0, 0, 0, ""],
        ["alloc_alignment.available", fallible_available, "readonly", 0, 0, 0, ""],
        ["alloc_alignment.try_reserve", fallible_reserve, "readonly", 0, 0, 0, ""],
    ],
}
