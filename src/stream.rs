use crate::key::RedisKey;
use crate::raw;
use crate::RedisError;
use crate::RedisString;
use crate::Status;
use std::os::raw::{c_int, c_long};
use std::ptr;

type StartFn = unsafe extern "C" fn(
    *mut raw::RedisModuleKey,
    c_int,
    *mut raw::RedisModuleStreamID,
    *mut raw::RedisModuleStreamID,
) -> c_int;
type StopFn = unsafe extern "C" fn(*mut raw::RedisModuleKey) -> c_int;
type NextIdFn = unsafe extern "C" fn(
    *mut raw::RedisModuleKey,
    *mut raw::RedisModuleStreamID,
    *mut c_long,
) -> c_int;
type NextFieldFn = unsafe extern "C" fn(
    *mut raw::RedisModuleKey,
    *mut *mut raw::RedisModuleString,
    *mut *mut raw::RedisModuleString,
) -> c_int;

// Resolve every operation before Start can acquire an iterator. Generic values
// let pure tests exercise this exact selection without native APIs or handles.
fn resolve_apis<S, T, I, F>(
    start: Option<S>,
    stop: Option<T>,
    next_id: Option<I>,
    next_field: Option<F>,
) -> Result<(S, T, I, F), RedisError> {
    Ok((
        start.ok_or(RedisError::Str("StreamIteratorStart API is unavailable"))?,
        stop.ok_or(RedisError::Str("StreamIteratorStop API is unavailable"))?,
        next_id.ok_or(RedisError::Str("StreamIteratorNextID API is unavailable"))?,
        next_field.ok_or(RedisError::Str(
            "StreamIteratorNextField API is unavailable",
        ))?,
    ))
}

#[derive(Debug)]
pub struct StreamRecord {
    pub id: raw::RedisModuleStreamID,
    pub fields: Vec<(RedisString, RedisString)>,
}

#[derive(Debug)]
pub struct StreamIterator<'key> {
    key: &'key RedisKey,
    stop: StopFn,
    next_id: NextIdFn,
    next_field: NextFieldFn,
}

impl<'key> StreamIterator<'key> {
    pub(crate) fn new(
        key: &'key RedisKey,
        mut from: Option<raw::RedisModuleStreamID>,
        mut to: Option<raw::RedisModuleStreamID>,
        exclusive: bool,
        reverse: bool,
    ) -> Result<StreamIterator<'key>, RedisError> {
        let (start, stop, next_id, next_field): (StartFn, StopFn, NextIdFn, NextFieldFn) = unsafe {
            resolve_apis(
                raw::RedisModule_StreamIteratorStart,
                raw::RedisModule_StreamIteratorStop,
                raw::RedisModule_StreamIteratorNextID,
                raw::RedisModule_StreamIteratorNextField,
            )?
        };
        let mut flags = if exclusive {
            raw::REDISMODULE_STREAM_ITERATOR_EXCLUSIVE as i32
        } else {
            0
        };

        flags |= if reverse {
            raw::REDISMODULE_STREAM_ITERATOR_REVERSE as i32
        } else {
            0
        };

        let res = unsafe {
            start(
                key.key_inner,
                flags,
                from.as_mut().map_or(ptr::null_mut(), |v| v),
                to.as_mut().map_or(ptr::null_mut(), |v| v),
            )
        };
        if Status::Ok == res.into() {
            Ok(StreamIterator {
                key,
                stop,
                next_id,
                next_field,
            })
        } else {
            Err(RedisError::Str("Failed creating stream iterator"))
        }
    }
}

impl<'key> Iterator for StreamIterator<'key> {
    type Item = StreamRecord;

    fn next(&mut self) -> Option<Self::Item> {
        let mut id = raw::RedisModuleStreamID { ms: 0, seq: 0 };
        let mut num_fields: c_long = 0;
        let mut field_name: *mut raw::RedisModuleString = ptr::null_mut();
        let mut field_val: *mut raw::RedisModuleString = ptr::null_mut();
        if Status::Ok
            != unsafe { (self.next_id)(self.key.key_inner, &mut id, &mut num_fields) }.into()
        {
            return None;
        }
        let mut fields = Vec::new();
        while Status::Ok
            == unsafe {
                (self.next_field)(self.key.key_inner, &mut field_name, &mut field_val).into()
            }
        {
            fields.push((
                unsafe { RedisString::from_redis_module_string(self.key.ctx, field_name) },
                unsafe { RedisString::from_redis_module_string(self.key.ctx, field_val) },
            ));
        }
        Some(StreamRecord { id, fields })
    }
}

impl<'key> Drop for StreamIterator<'key> {
    fn drop(&mut self) {
        // Only a successful Start constructs this owner. The borrowed key must
        // remain a valid native stream handle until Stop; broad native aliases
        // are outside this lifetime guarantee. Drop cannot return Stop errors.
        unsafe { (self.stop)(self.key.key_inner) };
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_apis;

    #[test]
    fn all_operations_are_required_before_acquisition() {
        for missing in 0..4 {
            let mut slots = [Some(0), Some(1), Some(2), Some(3)];
            slots[missing] = None;
            let error = resolve_apis(slots[0], slots[1], slots[2], slots[3]).unwrap_err();
            let name = ["Start", "Stop", "NextID", "NextField"][missing];
            assert_eq!(
                error.to_string(),
                format!("StreamIterator{name} API is unavailable")
            );
        }
        assert_eq!(
            resolve_apis(Some(0), Some(1), Some(2), Some(3)).unwrap(),
            (0, 1, 2, 3)
        );
    }
}
