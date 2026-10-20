//! Direct database selection and temporary access with automatic restoration.
use redis_module::{
    ArgCursor, Context, RedisError, RedisResult, RedisString, RedisValue, redis_module,
};

fn parse_db(args: &mut ArgCursor<'_>) -> Result<i32, RedisError> {
    i32::try_from(args.next_i64()?).map_err(|_| RedisError::Str("ERR DB index is out of range"))
}

fn selected(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ArgCursor::new(&args[1..]).done()?;
    Ok(i64::from(ctx.get_selected_db()).into())
}

fn select(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let db = parse_db(&mut args)?;
    args.done()?;
    ctx.select_db(db)?;
    Ok(i64::from(ctx.get_selected_db()).into())
}

/// Reads the key in another database without changing the caller's selection.
fn get_in(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let db = parse_db(&mut args)?;
    let key = args.next_arg()?;
    args.done()?;
    ctx.with_db(db, |ctx| ctx.call("GET", &[key]))?
}

fn nested(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let outer = parse_db(&mut args)?;
    let inner = parse_db(&mut args)?;
    args.done()?;
    let before = ctx.get_selected_db();
    let (outer_before, inner, outer_after) = ctx.with_db(outer, |ctx| {
        let outer_before = ctx.get_selected_db();
        let inner = ctx.with_db(inner, Context::get_selected_db)?;
        Ok::<_, RedisError>((outer_before, inner, ctx.get_selected_db()))
    })??;
    Ok(RedisValue::Array(
        [
            before,
            outer_before,
            inner,
            outer_after,
            ctx.get_selected_db(),
        ]
        .into_iter()
        .map(|db| RedisValue::Integer(i64::from(db)))
        .collect(),
    ))
}

fn scoped_error(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let db = parse_db(&mut args)?;
    args.done()?;
    ctx.with_db(db, |_| Err(RedisError::Str("ERR scoped database error")))?
}

fn scoped_panic(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let db = parse_db(&mut args)?;
    args.done()?;
    ctx.with_db(db, |_| -> RedisResult {
        panic!("scoped database panic");
    })?
}

redis_module! {
    name: "database",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["database.selected", selected, "readonly", 0, 0, 0, ""],
        ["database.select", select, "deny-script", 0, 0, 0, ""],
        ["database.get_in", get_in, "readonly deny-script", 2, 2, 1, ""],
        ["database.nested", nested, "readonly deny-script", 0, 0, 0, ""],
        ["database.scoped_error", scoped_error, "readonly deny-script", 0, 0, 0, ""],
        ["database.scoped_panic", scoped_panic, "readonly deny-script", 0, 0, 0, ""],
    ],
}
