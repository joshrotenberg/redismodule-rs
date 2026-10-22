#![no_main]

use libfuzzer_sys::fuzz_target;
use module_testing_parser::parse_limit;

#[cfg(feature = "sdk-allocator")]
#[global_allocator]
static ALLOCATOR: redis_module::alloc::RedisAlloc = redis_module::alloc::RedisAlloc;

fuzz_target!(|input: &[u8]| {
    // The independent oracle uses the standard parser plus our stricter grammar.
    let expected = std::str::from_utf8(input)
        .ok()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u16>().ok());
    let actual = parse_limit(input);
    assert_eq!(actual, expected);
    if let Some(value) = actual {
        // Also allocate through the selected Rust global allocator and round-trip.
        let canonical = value.to_string();
        assert_eq!(parse_limit(canonical.as_bytes()), Some(value));
    }
});
