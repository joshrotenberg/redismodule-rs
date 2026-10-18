use std::{ffi::c_void, mem::ManuallyDrop, ptr::NonNull};

use crate::{key::RedisKey, raw, RedisString};

/// A cursor to scan hash/set/sorted-set entries. Set callbacks have no value;
/// hash and sorted-set callbacks provide Some(value).
///
/// It provides access via a closure given to [`ScanKeyCursor::for_each`] or if you need more control, you can use [`ScanKeyCursor::scan`]
/// and implement your own loop, e.g. to allow an early stop.
///
/// ## Example usage
///
/// Here we show how to extract values to communicate them back to the Redis client. We assume that the following hash key is setup before:
///
/// ```text
/// HSET user:123 name Alice age 29 location Austin
/// ```
///
/// The following example command implementation scans all fields and values in the hash key and returns them as an array of RedisString.
///
/// ```ignore
/// fn example_scan_key_for_each(ctx: &Context) -> RedisResult {
///    let key = ctx.open_key_with_flags("user:123", KeyFlags::NOEFFECTS | KeyFlags::NOEXPIRE | KeyFlags::ACCESS_EXPIRED );
///    let cursor  = ScanKeyCursor::new(key);
///
///    let res = RefCell::new(Vec::new());
///    cursor.for_each(|_key, field, value| {
///        let mut res = res.borrow_mut();
///        res.push(RedisValue::BulkRedisString(field.clone()));
///        res.push(value.map_or(RedisValue::Null, |v| RedisValue::BulkRedisString(v.clone())));
///    });
///
///    Ok(RedisValue::Array(res.take()))
/// }
/// ```
///
/// The method will produce the following output:
///
/// ```text
/// 1) "name"
/// 2) "Alice"
/// 3) "age"
/// 4) "29"
/// 5) "location"
/// 6) "Austin"
/// ```
pub struct ScanKeyCursor {
    key: RedisKey,
    inner_cursor: *mut raw::RedisModuleScanCursor,
}

impl ScanKeyCursor {
    /// Creates a new scan cursor for the given key.
    pub fn new(key: RedisKey) -> Self {
        let inner_cursor = unsafe { raw::RedisModule_ScanCursorCreate.unwrap()() };
        Self { key, inner_cursor }
    }

    /// Restarts the cursor from the beginning.
    pub fn restart(&self) {
        unsafe { raw::RedisModule_ScanCursorRestart.unwrap()(self.inner_cursor) };
    }

    /// Implements a call to `RedisModule_ScanKey` and calls the given closure for each callback invocation by ScanKey.
    /// Returns `true` if there are more fields to scan, `false` otherwise.
    ///
    /// The callback may be called multiple times per `RedisModule_ScanKey` invocation.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// while cursor.scan(|_key, field, value| {
    ///    // do something with field and value
    /// }) {
    ///   // do something between scans if needed, like an early stop
    /// }
    pub fn scan<F: FnMut(&RedisKey, &RedisString, Option<&RedisString>)>(&self, mut f: F) -> bool {
        struct CallbackData<F> {
            ctx: *mut raw::RedisModuleCtx,
            callback: F,
        }
        unsafe extern "C" fn scan_callback<
            F: FnMut(&RedisKey, &RedisString, Option<&RedisString>),
        >(
            key: *mut raw::RedisModuleKey,
            field: *mut raw::RedisModuleString,
            value: *mut raw::RedisModuleString,
            data: *mut c_void,
        ) {
            // SAFETY: scan passes an exclusive local CallbackData during this
            // synchronous invocation; ctx is the key's actual opening context.
            let data = unsafe { &mut *data.cast::<CallbackData<F>>() };
            let key = ManuallyDrop::new(unsafe { RedisKey::from_raw_parts(data.ctx, key) });
            let field =
                unsafe { RedisString::retain_from_ptr(NonNull::new(field).expect("scan field")) };
            let value = NonNull::new(value).map(|p| unsafe { RedisString::retain_from_ptr(p) });
            crate::panic::defer_panic(|| (data.callback)(&key, &field, value.as_ref()));
            // Release just our retained references. The borrowed key belongs to
            // Redis; set callbacks legally have no value string.
        }

        // Safety: The c-side initialized the function ptr and it is is never changed,
        // i.e. after module initialization the function pointers stay valid till the end of the program.
        let scan_key = unsafe { raw::RedisModule_ScanKey.unwrap() };

        let mut data = CallbackData {
            ctx: self.key.ctx,
            callback: &mut f,
        };
        let res = unsafe {
            scan_key(
                self.key.key_inner,
                self.inner_cursor,
                Some(scan_callback::<&mut F>),
                &mut data as *mut CallbackData<&mut F> as *mut c_void,
            )
        };
        crate::panic::resume_deferred_panic();

        res != 0
    }

    /// Implements a callback based for_each loop over all fields and values in the hash key.
    /// If you need more control, e.g. stopping after a scan invocation, then use [`ScanKeyCursor::scan`] directly.
    pub fn for_each<F: FnMut(&RedisKey, &RedisString, Option<&RedisString>)>(&self, mut f: F) {
        while self.scan(&mut f) {
            // do nothing, the callback does the work
        }
    }
}

impl Drop for ScanKeyCursor {
    fn drop(&mut self) {
        unsafe { raw::RedisModule_ScanCursorDestroy.unwrap()(self.inner_cursor) };
    }
}
