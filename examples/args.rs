//! Borrowed parsing and a deliberately small owned-parser comparison.
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};

use redis_module::{
    alloc::RedisAlloc, redis_module, ArgCursor, Context, NextArg, RedisError, RedisResult,
    RedisString, RedisValue,
};

// Count Rust allocations only. Command decoding and replies happen outside the
// measured window; native Redis allocations are not routed through this hook.
struct CountingAlloc;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        RedisAlloc.alloc(layout)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        RedisAlloc.dealloc(pointer, layout);
    }
}

fn snapshot() -> (usize, usize) {
    (
        ALLOCATIONS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

fn reply(before: (usize, usize), parsed: (usize, u64)) -> RedisResult {
    let after = snapshot();
    Ok(vec![
        parsed.0 as i64,
        parsed.1 as i64,
        (after.0 - before.0) as i64,
        (after.1 - before.1) as i64,
    ]
    .into())
}

fn borrowed(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    let before = snapshot();
    let mut args = ArgCursor::new(&argv[1..]);
    let expression = args.next_str()?;
    if !args.next_bytes()?.eq_ignore_ascii_case(b"COUNT") {
        return Err(RedisError::Str("ERR expected COUNT"));
    }
    let count = args.next_u64()?;
    args.done()?;
    reply(before, (expression.len(), count))
}

fn owned(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    let before = snapshot();
    let mut args = argv.into_iter().skip(1);
    let expression = args.next_string()?;
    let option = args.next_string()?.to_ascii_uppercase();
    if option != "COUNT" {
        return Err(RedisError::Str("ERR expected COUNT"));
    }
    let count = args.next_u64()?;
    args.done()?;
    reply(before, (expression.len(), count))
}

fn inspect(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    let (key, text, bytes, initial, exhausted) = {
        let mut args = ArgCursor::new(&argv[1..]);
        let initial = args.len();
        let key = args.next_arg()?;
        let text = args.next_str()?;
        let bytes = args.next_bytes()?;
        args.done()?;
        let exhausted = args.next().is_none() && args.next().is_none();
        (key, text, bytes, initial, exhausted)
    };
    // Returned references remain usable after the cursor is gone.
    Ok(RedisValue::Array(vec![
        (initial as i64).into(),
        i64::from(std::ptr::eq(key, &argv[1])).into(),
        i64::from(text.as_ptr() == argv[2].as_slice().as_ptr()).into(),
        i64::from(bytes.as_ptr() == argv[3].as_slice().as_ptr()).into(),
        i64::from(exhausted).into(),
        RedisValue::StringBuffer(bytes.to_vec()),
    ]))
}

fn numbers(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&argv[1..]);
    let signed = args.next_i64()?;
    let unsigned = args.next_u64()?;
    let float = args.next_f64()?;
    args.done()?;
    Ok(RedisValue::Array(vec![
        signed.into(),
        (unsigned as i64).into(),
        float.into(),
    ]))
}

fn tail(_: &Context, argv: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&argv[1..]);
    let first = args.next_arg()?;
    let had_extra = matches!(args.done(), Err(RedisError::WrongArity));
    let remaining = args.remaining();
    Ok(vec![
        first.len() as i64,
        i64::from(had_extra),
        remaining.len() as i64,
    ]
    .into())
}

redis_module! {
    name: "args",
    version: 1,
    allocator: (CountingAlloc, CountingAlloc),
    data_types: [],
    commands: [
        ["args.borrowed", borrowed, "readonly", 0, 0, 0, ""],
        ["args.owned", owned, "readonly", 0, 0, 0, ""],
        ["args.inspect", inspect, "readonly", 0, 0, 0, ""],
        ["args.numbers", numbers, "readonly", 0, 0, 0, ""],
        ["args.tail", tail, "readonly", 0, 0, 0, ""],
    ],
}
