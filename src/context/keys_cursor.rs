use crate::context::RuntimeContext;
use crate::key::RedisKey;
use crate::raw;
use crate::redismodule::RedisString;
use std::ffi::c_void;
use std::ptr::NonNull;

pub struct KeysCursor {
    inner_cursor: *mut raw::RedisModuleScanCursor,
}

extern "C" fn scan_callback<C: FnMut(&RuntimeContext, &RedisString, Option<&RedisKey>)>(
    ctx: *mut raw::RedisModuleCtx,
    key_name: *mut raw::RedisModuleString,
    key: *mut raw::RedisModuleKey,
    private_data: *mut ::std::os::raw::c_void,
) {
    let context = unsafe { RuntimeContext::new(ctx) };
    // SAFETY: this synchronous native callback supplies a retainable borrowed name.
    let key_name =
        unsafe { RedisString::retain_from_ptr(NonNull::new(key_name).expect("scan key name")) };
    let redis_key = if key.is_null() {
        None
    } else {
        // Safety: The returned `RedisKey` does not outlive this callbacks and so by necessity
        // the pointers passed in as parameters are valid for its entire lifetime.
        Some(unsafe { RedisKey::borrowed(ctx, key) })
    };
    let callback = unsafe { &mut *(private_data.cast::<C>()) };
    crate::panic::defer_panic(|| callback(&context, &key_name, redis_key.as_deref()));

    // key_name releases our acquired reference; ManuallyDrop leaves the native
    // callback's borrowed key handle for its original owner to close.
}

impl KeysCursor {
    pub fn new() -> Self {
        let inner_cursor = unsafe { crate::__required_api!(RedisModule_ScanCursorCreate)() };
        Self { inner_cursor }
    }

    /// Scans the next batch of keys, calling `callback` with each one's name
    /// and the read-only key Redis lends it. Returns whether keys remain.
    ///
    /// The callback cannot change the keyspace: Redis closes the key it lent
    /// after the callback returns, reading the key's value, so calls,
    /// `yield_now`, notifications and key writes are refused there, and pops
    /// and nested keyspace scans panic. Collect what to change and change it
    /// after `scan` returns.
    pub fn scan<F: FnMut(&RuntimeContext, &RedisString, Option<&RedisKey>)>(
        &self,
        ctx: &RuntimeContext,
        callback: &mut F,
    ) -> bool {
        // A scan can expire keys, the lent one included.
        crate::key::panic_while_lent();
        // Expiring keys frees their values, so close the open ones first.
        crate::key::keyspace_will_change(None).unwrap_or_else(|error| panic!("{error}"));
        // Redis closes each key it lends the callback after the callback
        // returns, which reads the key's value, so a callback may not change
        // the keyspace.
        let frozen = crate::key::Frozen::begin();
        let res = unsafe {
            crate::__required_api!(RedisModule_Scan)(
                ctx.ctx,
                self.inner_cursor,
                Some(scan_callback::<F>),
                callback as *mut F as *mut c_void,
            )
        };
        drop(frozen);
        crate::panic::resume_deferred_panic();
        res != 0
    }

    pub fn restart(&self) {
        unsafe { crate::__required_api!(RedisModule_ScanCursorRestart)(self.inner_cursor) };
    }
}

impl Default for KeysCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeysCursor {
    fn drop(&mut self) {
        unsafe { crate::__required_api!(RedisModule_ScanCursorDestroy)(self.inner_cursor) };
    }
}
