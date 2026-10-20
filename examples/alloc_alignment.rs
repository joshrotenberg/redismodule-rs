//! Boxes values with alignments above 16 bytes through `RedisAlloc` and
//! reports how many of them landed on a misaligned address.

use redis_module::{Context, RedisResult, RedisString, RedisValue, redis_module};
use std::hint::black_box;

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
        check(|| Align32 { _bytes: [0; 32] }, 1000),
        check(|| Align64 { _bytes: [0; 64] }, 1000),
        check(|| Align128 { _bytes: [0; 128] }, 1000),
        check(|| Align256x3 { _bytes: [0; 768] }, 1000),
        check(|| Align1024x3 { _bytes: [0; 3072] }, 1000),
        check(|| Align4096 { _bytes: [0; 4096] }, 500),
        check(|| Align32768 { _bytes: [0; 32768] }, 200),
    ]))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "alloc_alignment",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["alloc_alignment.check", alloc_alignment_check, "readonly", 0, 0, 0, ""],
    ],
}
