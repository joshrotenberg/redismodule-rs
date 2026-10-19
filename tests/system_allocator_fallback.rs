//! Allocates with `RedisAlloc` as the global allocator and no Redis server,
//! which the `system-allocator-fallback` feature allows. Without the feature
//! the first allocation aborts the process.
use redis_module::alloc::RedisAlloc;

#[global_allocator]
static ALLOCATOR: RedisAlloc = RedisAlloc;

#[repr(align(64))]
struct OverAligned([u8; 64]);

#[test]
fn allocates_without_a_redis_server() {
    let text: String = (0..1000u32)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    assert_eq!(text.len(), 1000);

    let mut numbers: Vec<u64> = (0..10_000).collect();
    numbers.retain(|n| n % 3 == 0);
    assert_eq!(numbers.len(), 3334);

    let boxed = Box::new(OverAligned([7; 64]));
    assert_eq!(std::ptr::from_ref(boxed.as_ref()) as usize % 64, 0);
    assert_eq!(boxed.0[63], 7);
}
