use crate::key::RedisKey;
use crate::raw;
use crate::RedisError;
use crate::RedisString;
use crate::Status;
use std::os::raw::{c_int, c_long};
use std::ptr;

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

#[derive(Debug)]
pub struct StreamRecord {
    pub id: raw::RedisModuleStreamID,
    pub fields: Vec<(RedisString, RedisString)>,
}

/// Iterates the entries of a stream key. Dropping it stops the iterator, after
/// which the key can start another one.
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
        // Look up every API before starting, so a started iterator can always
        // be stopped.
        let start = unsafe { crate::__available_api!(RedisModule_StreamIteratorStart) };
        let stop = unsafe { crate::__available_api!(RedisModule_StreamIteratorStop) };
        let next_id = unsafe { crate::__available_api!(RedisModule_StreamIteratorNextID) };
        let next_field = unsafe { crate::__available_api!(RedisModule_StreamIteratorNextField) };

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
            == unsafe { (self.next_field)(self.key.key_inner, &mut field_name, &mut field_val) }
                .into()
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
        // Stop, not Delete: StreamIteratorDelete removes the current entry.
        unsafe { (self.stop)(self.key.key_inner) };
    }
}
