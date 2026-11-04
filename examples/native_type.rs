//! A native data type, `note00001`, with every optional callback: memory use,
//! `COPY`, free effort, unlink, `DEBUG DIGEST`, AOF rewrite, aux data and
//! defrag. The SDK generates the native callbacks from the trait
//! implementations and runs each inside a panic guard: a note whose text is
//! `panic-load`, `panic-copy` or `panic-free` makes that callback panic, and
//! the server keeps running.
use redis_module::defrag::DefragContext;
use redis_module::error::Error;
use redis_module::io::RedisModuleIO;
use redis_module::native_types::{
    AofRewrite, AofRewriter, AuxData, AuxTriggers, AuxWhen, CopyValue, Defrag, DefragStatus,
    Digest, DigestValue, FreeEffort, MemUsage, NativeType, RedisType, Unlink,
};
use redis_module::{Context, NextArg, RedisResult, RedisString, RedisValue, redis_module};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

struct Note {
    text: String,
}

/// Notes dropped, unlink callbacks, and the aux value last loaded.
static FREES: AtomicI64 = AtomicI64::new(0);
static UNLINKS: AtomicI64 = AtomicI64::new(0);
static AUX_LOADED: AtomicI64 = AtomicI64::new(-1);
/// The value the next RDB saves as the type's aux data.
static AUX_TO_SAVE: AtomicU64 = AtomicU64::new(0);

impl NativeType for Note {
    fn rdb_load(io: &mut RedisModuleIO, _encver: i32) -> Result<Self, Error> {
        let text = io.read_string()?;
        assert!(text != "panic-load", "rdb_load refused the marker value");
        Ok(Note { text })
    }

    fn rdb_save(&self, io: &mut RedisModuleIO) {
        io.write_string(&self.text);
    }
}

impl Drop for Note {
    fn drop(&mut self) {
        FREES.fetch_add(1, Ordering::Relaxed);
        assert!(self.text != "panic-free", "free refused the marker value");
    }
}

impl MemUsage for Note {
    fn mem_usage(&self) -> usize {
        std::mem::size_of::<Self>() + self.text.capacity()
    }
}

impl CopyValue for Note {
    fn copy_value(&self, _from_key: &[u8], _to_key: &[u8]) -> Option<Self> {
        assert!(self.text != "panic-copy", "copy refused the marker value");
        (self.text != "no-copy").then(|| Note {
            text: self.text.clone(),
        })
    }
}

impl FreeEffort for Note {
    fn free_effort(&self, _key: &[u8]) -> usize {
        1
    }
}

impl Unlink for Note {
    fn unlink(&self, _key: &[u8]) {
        UNLINKS.fetch_add(1, Ordering::Relaxed);
    }
}

impl DigestValue for Note {
    fn digest(&self, digest: &mut Digest) {
        digest.add_string_buffer(self.text.as_bytes());
        digest.end_sequence();
    }
}

impl AofRewrite for Note {
    fn aof_rewrite(&self, aof: &mut AofRewriter, key: &[u8]) {
        aof.emit("note.set", &[key, self.text.as_bytes()])
            .expect("note.set is a valid command name");
    }
}

impl AuxData for Note {
    const AUX_SAVE_TRIGGERS: AuxTriggers = AuxTriggers::AFTER_RDB;

    fn aux_save(io: &mut RedisModuleIO, _when: AuxWhen) {
        io.write_unsigned(AUX_TO_SAVE.load(Ordering::Relaxed));
    }

    fn aux_load(io: &mut RedisModuleIO, _encver: i32, _when: AuxWhen) -> Result<(), Error> {
        let value = io.read_unsigned()?;
        AUX_LOADED.store(value as i64, Ordering::Relaxed);
        Ok(())
    }
}

impl Defrag for Note {
    fn defrag(&mut self, _ctx: &DefragContext, _key: &[u8]) -> DefragStatus {
        DefragStatus::Done
    }
}

static NOTE_TYPE: RedisType<Note> = RedisType::new("note00001", 0)
    .with_mem_usage()
    .with_copy()
    .with_free_effort()
    .with_unlink()
    .with_digest()
    .with_aof_rewrite()
    .with_aux()
    .with_defrag();

/// Never listed in `data_types`, so storing a value through it is refused.
static UNREGISTERED_TYPE: RedisType<Note> = RedisType::new("note00002", 0);

/// `note.set key text`: stores a note.
fn set(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    let text = args.next_string()?;
    args.done()?;
    ctx.open_key_writable(&key)
        .set_value(&NOTE_TYPE, Note { text })?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `note.get key`: the note's text, or nil.
fn get(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    args.done()?;
    Ok(ctx
        .open_key(&key)
        .get_value(&NOTE_TYPE)?
        .map_or(RedisValue::Null, |note| note.text.clone().into()))
}

/// `note.set_unregistered key text`: stores through a type that was never
/// registered, which fails.
fn set_unregistered(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let key = args.next_arg()?;
    let text = args.next_string()?;
    args.done()?;
    ctx.open_key_writable(&key)
        .set_value(&UNREGISTERED_TYPE, Note { text })?;
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `note.aux value`: the aux data the next RDB saves.
fn aux(_: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let value = args.next_u64()?;
    args.done()?;
    AUX_TO_SAVE.store(value, Ordering::Relaxed);
    Ok(RedisValue::SimpleStringStatic("OK"))
}

/// `note.counts`: notes dropped, unlink callbacks, and the aux value last
/// loaded (-1 before any).
fn counts(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(vec![
        FREES.load(Ordering::Relaxed),
        UNLINKS.load(Ordering::Relaxed),
        AUX_LOADED.load(Ordering::Relaxed),
    ]
    .into())
}

redis_module! {
    name: "native_type",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [NOTE_TYPE],
    commands: [
        ["note.set", set, "write deny-oom", 1, 1, 1, ""],
        ["note.get", get, "readonly", 1, 1, 1, ""],
        ["note.set_unregistered", set_unregistered, "write", 1, 1, 1, ""],
        ["note.aux", aux, "", 0, 0, 0, ""],
        ["note.counts", counts, "readonly", 0, 0, 0, ""],
    ],
}
