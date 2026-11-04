//! A native data type, `alloc`'s `mytype123`, storing a string per key. The
//! SDK generates its callbacks from the `NativeType` and `Defrag`
//! implementations: the value round-trips through RDB files, and active
//! defragmentation counts the values it visits.
use lazy_static::lazy_static;
use redis_module::defrag::DefragContext;
use redis_module::error::Error;
use redis_module::io::{RdbReader, RdbWriter};
use redis_module::native_types::{Defrag, DefragStatus, NativeType, RedisType};
use redis_module::redisvalue::RedisValueKey;
use redis_module::{
    Context, NextArg, RedisGILGuard, RedisResult, RedisString, RedisValue, redis_module,
};
use redis_module_macros::{defrag_end_function, defrag_function, defrag_start_function};

#[derive(Debug)]
struct MyType {
    data: String,
}

impl NativeType for MyType {
    fn rdb_load(io: &mut RdbReader, _encver: i32) -> Result<Self, Error> {
        Ok(MyType {
            data: io.read_string()?,
        })
    }

    fn rdb_save(&self, io: &mut RdbWriter) {
        io.write_string(&self.data);
    }
}

impl Defrag for MyType {
    fn defrag(&mut self, ctx: &DefragContext, _key: &[u8]) -> DefragStatus {
        *NUM_KEYS_DEFRAG.lock(ctx) += 1;
        DefragStatus::Done
    }
}

lazy_static! {
    static ref NUM_KEYS_DEFRAG: RedisGILGuard<usize> = RedisGILGuard::default();
    static ref NUM_DEFRAG_START: RedisGILGuard<usize> = RedisGILGuard::default();
    static ref NUM_DEFRAG_END: RedisGILGuard<usize> = RedisGILGuard::default();
    static ref NUM_DEFRAG_GLOBALS: RedisGILGuard<usize> = RedisGILGuard::default();
}

static MY_REDIS_TYPE: RedisType<MyType> = RedisType::new("mytype123", 0).with_defrag();

#[defrag_start_function]
fn defrag_start(defrag_ctx: &DefragContext) {
    let mut num_defrag_start = NUM_DEFRAG_START.lock(defrag_ctx);
    *num_defrag_start += 1;
}

#[defrag_end_function]
fn defrag_end(defrag_ctx: &DefragContext) {
    let mut num_defrag_end = NUM_DEFRAG_END.lock(defrag_ctx);
    *num_defrag_end += 1;
}

#[defrag_function]
fn defrag_globals(defrag_ctx: &DefragContext) {
    let mut num_defrag_globals = NUM_DEFRAG_GLOBALS.lock(defrag_ctx);
    *num_defrag_globals += 1;
}

fn alloc_set(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    let size = args.next_i64()?;

    ctx.log_debug(format!("key: {key}, size: {size}").as_str());

    let mut key = ctx.open_key_writable(&key);

    if let Some(value) = key.get_value(&MY_REDIS_TYPE)? {
        value.data = "B".repeat(size as usize);
    } else {
        let value = MyType {
            data: "A".repeat(size as usize),
        };

        key.set_value(&MY_REDIS_TYPE, value)?;
    }
    Ok(size.into())
}

fn alloc_get(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;

    let key = ctx.open_key(&key);

    let value = match key.get_value(&MY_REDIS_TYPE)? {
        Some(value) => value.data.as_str().into(),
        None => ().into(),
    };

    Ok(value)
}

fn alloc_defragstats(ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
    let num_keys_defrag = NUM_KEYS_DEFRAG.lock(ctx);
    let num_defrag_globals = NUM_DEFRAG_GLOBALS.lock(ctx);
    let num_defrag_start = NUM_DEFRAG_START.lock(ctx);
    let num_defrag_end = NUM_DEFRAG_END.lock(ctx);
    Ok(RedisValue::OrderedMap(
        [
            (
                RedisValueKey::String("num_keys_defrag".to_owned()),
                RedisValue::Integer(*num_keys_defrag as i64),
            ),
            (
                RedisValueKey::String("num_defrag_globals".to_owned()),
                RedisValue::Integer(*num_defrag_globals as i64),
            ),
            (
                RedisValueKey::String("num_defrag_start".to_owned()),
                RedisValue::Integer(*num_defrag_start as i64),
            ),
            (
                RedisValueKey::String("num_defrag_end".to_owned()),
                RedisValue::Integer(*num_defrag_end as i64),
            ),
        ]
        .into_iter()
        .collect(),
    ))
}

//////////////////////////////////////////////////////

redis_module! {
    name: "alloc",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [
        MY_REDIS_TYPE,
    ],
    commands: [
        ["alloc.set", alloc_set, "write", 1, 1, 1, ""],
        ["alloc.get", alloc_get, "readonly", 1, 1, 1, ""],
        ["alloc.defragstats", alloc_defragstats, "readonly", 0, 0, 0, ""],
    ],
}
