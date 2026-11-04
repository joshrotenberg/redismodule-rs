use crate::RedisError;
use crate::RedisString;
use crate::Status;
use crate::key::RedisKey;
use crate::raw;
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
/// which the key can start another one. The key borrow also keeps its opening
/// runtime context alive and locked.
///
/// A keyspace change while it iterates, such as a call in the loop body,
/// closes the key under it (see [`RedisKey`]), and its next step, or the next
/// field of an entry from [`next_entry`](Self::next_entry), panics: start a
/// new iterator after the change.
#[derive(Debug)]
pub struct StreamIterator<'key> {
    key: &'key RedisKey<'key>,
    /// The key's token when the iterator started.
    token: u64,
    stop: StopFn,
    next_id: NextIdFn,
    next_field: NextFieldFn,
    remaining: usize,
    finished: bool,
}

impl<'key> StreamIterator<'key> {
    pub(crate) fn new(
        key: &'key RedisKey<'key>,
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

        let native = key.native.current();
        let res = unsafe {
            start(
                native,
                flags,
                from.as_mut().map_or(ptr::null_mut(), |v| v),
                to.as_mut().map_or(ptr::null_mut(), |v| v),
            )
        };
        if Status::Ok == res.into() {
            Ok(StreamIterator {
                key,
                token: key.native.token(),
                stop,
                next_id,
                next_field,
                remaining: 0,
                finished: false,
            })
        } else {
            Err(RedisError::Str("ERR Failed creating stream iterator"))
        }
    }
}

impl<'key> StreamIterator<'key> {
    /// The native key the iterator started on.
    ///
    /// # Panics
    ///
    /// Panics if a keyspace change closed the key since: the native iterator
    /// ended with it.
    fn native(&self) -> *mut raw::RedisModuleKey {
        self.key.native.unchanged_since(self.token).expect(
            "the stream's key was closed by a keyspace change while iterating it; start a new iterator",
        )
    }

    /// Advances to an entry without allocating its field/value strings.
    ///
    /// The entry exclusively borrows this iterator until it is dropped. Its
    /// unread fields are skipped before another entry is read, even if the
    /// handle is forgotten with `mem::forget`. The existing `Iterator` may be
    /// used between entry handles and still returns complete `StreamRecord`s.
    /// Do not mutate the stream while either form of iteration is active.
    ///
    /// # Panics
    ///
    /// Panics while key data is lent to an SDK scoped callback.
    ///
    /// ```compile_fail,E0499
    /// # use redis_module::stream::StreamIterator;
    /// # fn overlap(iterator: &mut StreamIterator<'_>) {
    /// let mut entry = iterator.next_entry().unwrap();
    /// iterator.next();
    /// entry.next_field();
    /// # }
    /// ```
    pub fn next_entry(&mut self) -> Option<StreamEntry<'_, 'key>> {
        crate::key::panic_while_lent();
        // State belongs to the iterator, so forgetting an entry cannot leave
        // native NextID positioned in the middle of a field/value sequence.
        if self.finished {
            return None;
        }
        self.drain_fields();
        if self.finished {
            return None;
        }
        let mut id = raw::RedisModuleStreamID { ms: 0, seq: 0 };
        let mut fields: c_long = 0;
        let native = self.native();
        if Status::Ok != unsafe { (self.next_id)(native, &mut id, &mut fields) }.into() {
            self.finished = true;
            return None;
        }
        self.remaining = usize::try_from(fields).expect("native stream field count is nonnegative");
        Some(StreamEntry { iterator: self, id })
    }

    fn take_field(&mut self) -> Option<(RedisString, RedisString)> {
        if self.remaining == 0 {
            return None;
        }
        let mut field = ptr::null_mut();
        let mut value = ptr::null_mut();
        let native = self.native();
        if Status::Ok != unsafe { (self.next_field)(native, &mut field, &mut value) }.into() {
            self.remaining = 0;
            self.finished = true;
            return None;
        }
        self.remaining -= 1;
        // Preserve the same producer-aware owned adoption as the legacy
        // iterator, including detachment from the producer's AutoMemory.
        Some(unsafe {
            (
                RedisString::from_redis_module_string(self.key.ctx, field),
                RedisString::from_redis_module_string(self.key.ctx, value),
            )
        })
    }

    fn drain_fields(&mut self) -> usize {
        // A keyspace change ended the native iterator: nothing to skip, and
        // this runs from Drop, which must not panic.
        let Some(native) = self.key.native.unchanged_since(self.token) else {
            self.remaining = 0;
            self.finished = true;
            return 0;
        };
        let mut skipped = 0;
        while self.remaining != 0 {
            let status = unsafe { (self.next_field)(native, ptr::null_mut(), ptr::null_mut()) };
            if Status::Ok != status.into() {
                self.remaining = 0;
                self.finished = true;
                break;
            }
            self.remaining -= 1;
            skipped += 1;
        }
        skipped
    }
}

/// One stream entry, lending field-at-a-time access from its iterator.
///
/// Returned strings are independent owners with the same AutoMemory handling
/// as `StreamRecord` fields. Dropping the handle skips unread fields without
/// creating returned string objects. The iterator's borrow keeps the key and
/// opening context alive, but does not make mutation through another handle
/// or a native alias valid while iterating.
#[derive(Debug)]
pub struct StreamEntry<'iterator, 'key> {
    iterator: &'iterator mut StreamIterator<'key>,
    /// The entry's copied native ID; it does not borrow the cursor.
    pub id: raw::RedisModuleStreamID,
}

impl StreamEntry<'_, '_> {
    /// Number of field/value pairs not yet fetched or skipped.
    pub fn remaining_fields(&self) -> usize {
        self.iterator.remaining
    }

    /// Fetches the next field/value pair as owned native strings.
    /// Returns `None` once this entry is exhausted. Do not mutate the stream
    /// through another handle or call while this entry is active.
    ///
    /// # Panics
    /// Panics while key data is lent to an SDK scoped callback.
    pub fn next_field(&mut self) -> Option<(RedisString, RedisString)> {
        crate::key::panic_while_lent();
        self.iterator.take_field()
    }

    /// Skips all unread pairs and returns how many were skipped.
    /// Native `NextField(NULL, NULL)` advances without allocating field/value
    /// string objects. Repeating this operation returns zero.
    ///
    /// # Panics
    /// Panics while key data is lent to an SDK scoped callback.
    pub fn skip_fields(&mut self) -> usize {
        crate::key::panic_while_lent();
        self.iterator.drain_fields()
    }
}

impl Drop for StreamEntry<'_, '_> {
    fn drop(&mut self) {
        // Cleanup cannot refuse on the scoped-data guard. As with closing a
        // key/iterator, the native lifetime and lock borrow remains required.
        self.iterator.drain_fields();
    }
}

impl Iterator for StreamIterator<'_> {
    type Item = StreamRecord;

    fn next(&mut self) -> Option<Self::Item> {
        let mut entry = self.next_entry()?;
        let id = entry.id;
        let mut fields = Vec::new();
        while let Some(field) = entry.next_field() {
            fields.push(field);
        }
        Some(StreamRecord { id, fields })
    }
}

impl<'key> Drop for StreamIterator<'key> {
    fn drop(&mut self) {
        // Closing the key ended the native iterator with it.
        if let Some(native) = self.key.native.unchanged_since(self.token) {
            // Stop, not Delete: StreamIteratorDelete removes the current entry.
            unsafe { (self.stop)(native) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::mem::ManuallyDrop;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[derive(Default)]
    struct State {
        ids: Cell<usize>,
        fields: Cell<usize>,
        stops: Cell<usize>,
    }
    unsafe fn state<'a>(key: *mut raw::RedisModuleKey) -> &'a State {
        unsafe { &*key.cast::<State>() }
    }
    unsafe extern "C" fn next_id(
        key: *mut raw::RedisModuleKey,
        id: *mut raw::RedisModuleStreamID,
        fields: *mut c_long,
    ) -> c_int {
        let state = unsafe { state(key) };
        state.ids.set(state.ids.get() + 1);
        unsafe {
            *id = raw::RedisModuleStreamID { ms: 1, seq: 0 };
            *fields = 2;
        }
        raw::REDISMODULE_OK as c_int
    }
    unsafe extern "C" fn field_failure(
        key: *mut raw::RedisModuleKey,
        _: *mut *mut raw::RedisModuleString,
        _: *mut *mut raw::RedisModuleString,
    ) -> c_int {
        let state = unsafe { state(key) };
        state.fields.set(state.fields.get() + 1);
        raw::REDISMODULE_ERR as c_int
    }
    unsafe extern "C" fn stop(key: *mut raw::RedisModuleKey) -> c_int {
        let state = unsafe { state(key) };
        state.stops.set(state.stops.get() + 1);
        raw::REDISMODULE_OK as c_int
    }
    fn key(state: &State) -> ManuallyDrop<RedisKey<'_>> {
        // Private fake backend: the opaque token is a live State, consumed
        // only by the three callbacks above. No native key was allocated, so
        // the real CloseKey must never run for this test token.
        ManuallyDrop::new(unsafe {
            RedisKey::from_raw_parts(ptr::null_mut(), std::ptr::from_ref(state).cast_mut().cast())
        })
    }
    fn iterator<'a>(key: &'a RedisKey<'a>) -> StreamIterator<'a> {
        StreamIterator {
            key,
            token: key.native.token(),
            stop,
            next_id,
            next_field: field_failure,
            remaining: 0,
            finished: false,
        }
    }

    #[test]
    fn unexpected_field_failure_never_reenters_native_next_id() {
        for fetch in [false, true] {
            let state = State::default();
            let key = key(&state);
            let mut iterator = iterator(&key);
            let mut entry = iterator.next_entry().unwrap();
            if fetch {
                assert!(entry.next_field().is_none());
            }
            drop(entry); // Also exercise failure during cleanup's null skip.
            assert!(iterator.next_entry().is_none());
            assert!(iterator.next().is_none());
            assert_eq!(state.ids.get(), 1);
            assert_eq!(state.fields.get(), 1);
            drop(iterator);
            assert_eq!(state.stops.get(), 1);
        }
    }

    #[test]
    fn cached_handles_refuse_access_during_another_scoped_key_loan() {
        let state = State::default();
        let read = key(&state);
        let mut iterator = iterator(&read);
        // Model handles reached through TLS: ordinary closure captures are
        // already rejected by the Send bounds on public scoped-data APIs.
        crate::key::with_test_key_loan(|| {
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    iterator.next_entry();
                }))
                .is_err()
            );
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    iterator.next();
                }))
                .is_err()
            );
        });
        assert_eq!(state.ids.get(), 0);
        assert_eq!(state.fields.get(), 0);
        let mut entry = iterator.next_entry().unwrap();
        crate::key::with_test_key_loan(|| {
            assert!(catch_unwind(AssertUnwindSafe(|| entry.next_field())).is_err());
            assert!(catch_unwind(AssertUnwindSafe(|| entry.skip_fields())).is_err());
        });
        assert_eq!(entry.remaining_fields(), 2);
        assert_eq!(state.fields.get(), 0);
    }
}
