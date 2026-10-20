//! Commands that report memory through the memory accounting wrappers.
use std::alloc::Layout;

use redis_module::{
    Context, NextArg, RedisResult, RedisString, RedisValue, alloc::redis_alloc_size, redis_module,
};

/// `memory.ratio`: used memory relative to `maxmemory`, as text.
fn ratio(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(ctx.used_memory_ratio()?.to_string().into())
}

/// `memory.string_size text`: the memory Redis uses for the argument.
fn string_size(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let text = args.next_arg()?;
    args.done()?;
    Ok(RedisValue::Integer(text.malloc_size()? as i64))
}

#[repr(align(64))]
struct Aligned([u8; 64]);

/// `memory.buffer_sizes n`: the memory allocated for a `Vec<u8>` with
/// capacity `n`, and for a 64-byte-aligned `Box`.
fn buffer_sizes(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let capacity = args.next_u64()? as usize;
    if capacity == 0 {
        // A zero-capacity Vec has no allocation to measure.
        return Err(redis_module::RedisError::Str(
            "ERR the capacity must be positive",
        ));
    }
    args.done()?;

    let buffer: Vec<u8> = Vec::with_capacity(capacity);
    let buffer_layout = Layout::array::<u8>(buffer.capacity())?;
    // SAFETY: RedisAlloc, this module's global allocator, allocated the buffer
    // for this layout, and both values live until after the calls.
    let buffer_size = unsafe { redis_alloc_size(buffer.as_ptr(), buffer_layout) }?;

    let aligned = Box::new(Aligned([1; 64]));
    if !(aligned.0.as_ptr() as usize).is_multiple_of(64) {
        return Err(redis_module::RedisError::Str(
            "ERR the box is not 64-byte aligned",
        ));
    }
    let aligned_size = unsafe {
        redis_alloc_size(
            std::ptr::from_ref(aligned.as_ref()).cast(),
            Layout::new::<Aligned>(),
        )
    }?;

    Ok(RedisValue::Array(vec![
        RedisValue::Integer(buffer_size as i64),
        RedisValue::Integer(aligned_size as i64),
    ]))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "memory",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["memory.ratio", ratio, "readonly", 0, 0, 0, ""],
        ["memory.string_size", string_size, "readonly", 0, 0, 0, ""],
        ["memory.buffer_sizes", buffer_sizes, "readonly", 0, 0, 0, ""],
    ],
}
