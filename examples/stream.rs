use redis_module::raw::{KeyType, RedisModuleStreamID};
use redis_module::{
    ArgCursor, Context, NextArg, RedisError, RedisResult, RedisString, RedisValue, redis_module,
};

fn stream_read_from(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);

    let stream_key = args.next_arg()?;

    // End the iterator and the read key before opening the key for writing.
    let (element, id_to_keep) = {
        let stream = ctx.open_key(&stream_key);
        let key_type = stream.key_type();

        if key_type != KeyType::Stream {
            return Err(RedisError::WrongType);
        }

        let mut iter = stream.get_stream_iterator(false)?;
        let element = iter.next();
        let id_to_keep = iter.next().as_ref().map_or_else(
            || RedisModuleStreamID {
                ms: u64::MAX,
                seq: u64::MAX,
            },
            |e| e.id,
        );
        (element, id_to_keep)
    };

    // An empty stream has nothing to trim.
    let Some(element) = element else {
        return Ok(RedisValue::Null);
    };
    let stream = ctx.open_key_writable(&stream_key);
    stream.trim_stream_by_id(id_to_keep, false)?;
    Ok(RedisValue::BulkString(format!(
        "{}-{}",
        element.id.ms, element.id.seq
    )))
}

/// Reads the first entry with one iterator, drops it, and reads the first
/// entry again with a second iterator on the same key.
fn stream_first_twice(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter().skip(1);
    let stream_key = args.next_arg()?;
    args.done()?;

    let stream = ctx.open_key(&stream_key);
    if stream.key_type() != KeyType::Stream {
        return Err(RedisError::WrongType);
    }
    let first_id = || -> RedisResult {
        let mut iter = stream.get_stream_iterator(false)?;
        Ok(iter.next().map_or(RedisValue::Null, |e| {
            RedisValue::BulkString(format!("{}-{}", e.id.ms, e.id.seq))
        }))
    };
    Ok(RedisValue::Array(vec![first_id()?, first_id()?]))
}

/// Trim entries older than an ID and report the count, including zero.
fn stream_trim_before(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let name = args.next_arg()?;
    let (ms, seq) = args.next_str()?.split_once('-').ok_or(RedisError::Str(
        "ERR expected a stream ID as milliseconds-sequence",
    ))?;
    let id = RedisModuleStreamID {
        ms: ms.parse()?,
        seq: seq.parse()?,
    };
    let approximate = match args.next_str()? {
        "exact" => false,
        "approx" => true,
        _ => return Err(RedisError::Str("ERR expected exact or approx")),
    };
    args.done()?;
    let key = ctx.open_key_writable(name);
    let removed = key.trim_stream_by_id(id, approximate)?;
    Ok(RedisValue::Integer(i64::try_from(removed)?))
}

/// Retain only a prefix of each entry, leaving the remainder unmaterialized.
fn stream_entries(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = ArgCursor::new(&args[1..]);
    let name = args.next_arg()?;
    let reverse = args.next_i64()? != 0;
    let from = args.next_arg()?;
    let from = if from.as_slice() == b"_" {
        None
    } else {
        Some(from.parse_stream_id()?)
    };
    let to = args.next_arg()?;
    let to = if to.as_slice() == b"_" {
        None
    } else {
        Some(to.parse_stream_id()?)
    };
    let exclusive = args.next_i64()? != 0;
    let take = usize::try_from(args.next_u64()?)?;
    let mode = args.next_str()?;
    let auto_memory = args.next_i64()? != 0;
    args.done()?;
    if !matches!(mode, "skip" | "drop" | "forget" | "alternate") {
        return Err(RedisError::Str(
            "ERR expected skip, drop, forget or alternate",
        ));
    }
    if auto_memory {
        // This command retains no untracked context-owned native results.
        unsafe {
            ctx.auto_memory();
        }
    }
    let key = ctx.open_key(name);
    if key.key_type() != KeyType::Stream {
        return Err(RedisError::WrongType);
    }
    let mut iterator = key.get_stream_range_iterator(from, to, exclusive, reverse)?;
    let mut rows = Vec::new();
    loop {
        if mode == "alternate" && rows.len() % 2 == 1 {
            let Some(record) = iterator.next() else {
                break;
            };
            rows.push(entry_row(record.id, record.fields, 0));
            continue;
        }
        let Some(mut entry) = iterator.next_entry() else {
            break;
        };
        let id = entry.id;
        let mut fields = Vec::new();
        for _ in 0..take {
            let Some(field) = entry.next_field() else {
                break;
            };
            fields.push(field);
        }
        let remaining = entry.remaining_fields();
        match mode {
            "skip" => {
                assert_eq!(entry.skip_fields(), remaining);
                assert_eq!(entry.skip_fields(), 0);
                assert!(entry.next_field().is_none());
            }
            "forget" | "alternate" => std::mem::forget(entry),
            _ => drop(entry),
        }
        rows.push(entry_row(id, fields, remaining));
    }
    // The rows own their strings independently of both cursor and key.
    drop(iterator);
    drop(key);
    Ok(rows.into())
}

fn entry_row(
    id: RedisModuleStreamID,
    fields: Vec<(RedisString, RedisString)>,
    skipped: usize,
) -> RedisValue {
    let fields: Vec<_> = fields
        .into_iter()
        .flat_map(|(field, value)| {
            [
                RedisValue::BulkRedisString(field),
                RedisValue::BulkRedisString(value),
            ]
        })
        .collect();
    RedisValue::Array(vec![
        format!("{}-{}", id.ms, id.seq).into(),
        fields.into(),
        (skipped as i64).into(),
    ])
}

fn stream_parse_id(_: &Context, args: Vec<RedisString>) -> RedisResult {
    use redis_module::raw;
    struct Restore(
        Option<
            unsafe extern "C" fn(
                *const raw::RedisModuleString,
                *mut raw::RedisModuleStreamID,
            ) -> std::os::raw::c_int,
        >,
    );
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                raw::RedisModule_StringToStreamID = self.0;
            }
        }
    }
    let input = args.get(1).ok_or(RedisError::WrongArity)?;
    let _restore = Restore(unsafe { raw::RedisModule_StringToStreamID });
    if args.get(2).is_some_and(|arg| arg.as_slice() == b"missing") {
        unsafe {
            raw::RedisModule_StringToStreamID = None;
        }
    }
    let id = input.parse_stream_id()?;
    Ok(format!("{}-{}", id.ms, id.seq).into())
}

fn stream_key_pointer(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    use redis_module::raw;
    let name = args.get(1).ok_or(RedisError::WrongArity)?;
    let writable = args.get(2).is_some_and(|arg| arg.as_slice() == b"write");
    let _probe = field_probe::close_probe()?;
    let kind = unsafe { raw::RedisModule_KeyType }
        .ok_or(RedisError::Str("ERR key type API unavailable"))?;
    let native_kind = if writable {
        let key = ctx.open_key_writable(name);
        unsafe { kind(key.as_ptr()) }
    } else {
        let key = ctx.open_key(name);
        unsafe { kind(key.as_ptr()) }
    };
    // Both accessors left one SDK owner, which has now closed. Reopen and
    // copy the unchanged value; the writable demonstration does not mutate it.
    let key = ctx.open_key(name);
    let after = key
        .read()?
        .ok_or(RedisError::Str("ERR expected existing string"))?;
    drop(key);
    Ok(vec![
        RedisValue::Integer(i64::from(native_kind)),
        after.into(),
        field_probe::closes().into(),
    ]
    .into())
}

//////////////////////////////////////////////////////

redis_module! {
    name: "stream",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["STREAM_ENTRIES", stream_entries, "readonly", 1, 1, 1, ""],
        ["STREAM_PARSE_ID", stream_parse_id, "readonly", 0, 0, 0, ""],
        ["STREAM_KEY_POINTER", stream_key_pointer, "readonly", 1, 1, 1, ""],
        ["STREAM_FIELD_COST", field_probe::cost, "readonly", 1, 1, 1, ""],
        ["STREAM_POP", stream_read_from, "write", 1, 1, 1, ""],
        ["STREAM_FIRST_TWICE", stream_first_twice, "readonly", 1, 1, 1, ""],
        ["STREAM_TRIM_BEFORE", stream_trim_before, "write", 1, 1, 1, ""],
    ],
}

/// Instrument native output requests, not allocator byte sizes. Redis creates
/// a string for each non-null NextField output; null outputs only advance.
mod field_probe {
    use super::*;
    use redis_module::{Status, raw};
    use std::os::raw::c_int;
    use std::sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicI64, Ordering},
    };

    type NextField = unsafe extern "C" fn(
        *mut raw::RedisModuleKey,
        *mut *mut raw::RedisModuleString,
        *mut *mut raw::RedisModuleString,
    ) -> c_int;
    static ORIGINAL: OnceLock<NextField> = OnceLock::new();
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    type CloseKey = unsafe extern "C" fn(*mut raw::RedisModuleKey);
    static ORIGINAL_CLOSE: OnceLock<CloseKey> = OnceLock::new();
    static CLOSES: AtomicI64 = AtomicI64::new(0);

    unsafe extern "C" fn counted_close(key: *mut raw::RedisModuleKey) {
        CLOSES.fetch_add(1, Ordering::Relaxed);
        unsafe {
            (ORIGINAL_CLOSE.get().expect("probe installed"))(key);
        }
    }

    pub(super) struct CloseProbe(CloseKey);
    impl Drop for CloseProbe {
        fn drop(&mut self) {
            unsafe {
                raw::RedisModule_CloseKey = Some(self.0);
            }
            ACTIVE.store(false, Ordering::Relaxed);
        }
    }
    pub(super) fn close_probe() -> Result<CloseProbe, RedisError> {
        let original = unsafe { raw::RedisModule_CloseKey }
            .ok_or(RedisError::Str("ERR native key close unavailable"))?;
        if ACTIVE.swap(true, Ordering::Relaxed) {
            return Err(RedisError::Str("ERR field probe is already active"));
        }
        let guard = CloseProbe(original);
        ORIGINAL_CLOSE.get_or_init(|| original);
        CLOSES.store(0, Ordering::Relaxed);
        unsafe {
            raw::RedisModule_CloseKey = Some(counted_close);
        }
        Ok(guard)
    }
    pub(super) fn closes() -> i64 {
        CLOSES.load(Ordering::Relaxed)
    }

    static FIELDS: AtomicI64 = AtomicI64::new(0);
    static STRINGS: AtomicI64 = AtomicI64::new(0);

    unsafe extern "C" fn counted(
        key: *mut raw::RedisModuleKey,
        field: *mut *mut raw::RedisModuleString,
        value: *mut *mut raw::RedisModuleString,
    ) -> c_int {
        let result = unsafe { (ORIGINAL.get().expect("probe installed"))(key, field, value) };
        if result == raw::REDISMODULE_OK as c_int {
            FIELDS.fetch_add(1, Ordering::Relaxed);
            STRINGS.fetch_add(
                i64::from(!field.is_null()) + i64::from(!value.is_null()),
                Ordering::Relaxed,
            );
        }
        result
    }

    struct Probe(NextField);
    impl Drop for Probe {
        fn drop(&mut self) {
            unsafe {
                raw::RedisModule_StreamIteratorNextField = Some(self.0);
            }
            ACTIVE.store(false, Ordering::Relaxed);
        }
    }

    pub(super) fn cost(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
        let mut args = ArgCursor::new(&args[1..]);
        let name = args.next_arg()?;
        let mode = args.next_str()?;
        let reverse = args.next_i64()? != 0;
        args.done()?;
        if !matches!(
            mode,
            "all" | "skip" | "entry" | "entry_zero" | "drop" | "forget" | "alternate"
        ) {
            return Err(RedisError::Str(
                "ERR expected all, skip, entry, entry_zero, drop, forget or alternate",
            ));
        }
        let original = unsafe { raw::RedisModule_StreamIteratorNextField }
            .ok_or(RedisError::Str("ERR native stream fields unavailable"))?;
        if ACTIVE.swap(true, Ordering::Relaxed) {
            return Err(RedisError::Str("ERR field probe is already active"));
        }
        let _probe = Probe(original);
        ORIGINAL.get_or_init(|| original);
        FIELDS.store(0, Ordering::Relaxed);
        STRINGS.store(0, Ordering::Relaxed);
        // Only this synchronous module command uses its binding, under the
        // Redis lock; the active flag refuses reentrant instrumentation.
        unsafe {
            raw::RedisModule_StreamIteratorNextField = Some(counted);
        }
        let key = ctx.open_key(name);
        if key.key_type() != KeyType::Stream {
            return Err(RedisError::WrongType);
        }
        // This borrowed pointer leaves the SDK key owner in place.
        let pointer = key.as_ptr();
        let mut iterator = key.get_stream_iterator(reverse)?;
        let mut entries = 0_i64;
        if mode == "all" {
            for record in iterator.by_ref() {
                entries += 1;
                drop(record);
            }
        } else if mode != "skip" {
            loop {
                if mode == "alternate" && entries % 2 == 1 {
                    let Some(record) = iterator.next() else {
                        break;
                    };
                    entries += 1;
                    drop(record);
                    continue;
                }
                let Some(mut entry) = iterator.next_entry() else {
                    break;
                };
                entries += 1;
                if mode != "entry_zero" {
                    drop(entry.next_field());
                }
                match mode {
                    "entry" | "entry_zero" => {
                        entry.skip_fields();
                        assert_eq!(entry.skip_fields(), 0);
                        assert!(entry.next_field().is_none());
                    }
                    "forget" | "alternate" => std::mem::forget(entry),
                    _ => drop(entry),
                }
            }
        } else {
            let next_id = unsafe { raw::RedisModule_StreamIteratorNextID }
                .ok_or(RedisError::Str("ERR native stream IDs unavailable"))?;
            let mut id = raw::RedisModuleStreamID { ms: 0, seq: 0 };
            while Status::Ok == unsafe { next_id(pointer, &mut id, std::ptr::null_mut()) }.into() {
                entries += 1;
                while Status::Ok
                    == unsafe { counted(pointer, std::ptr::null_mut(), std::ptr::null_mut()) }
                        .into()
                {}
            }
        }
        drop(iterator);
        Ok(vec![
            entries,
            FIELDS.load(Ordering::Relaxed),
            STRINGS.load(Ordering::Relaxed),
        ]
        .into())
    }
}
