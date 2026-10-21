use crate::context::RuntimeContext;
use crate::key::RedisKey;
use crate::raw;
use crate::redismodule::RedisString;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
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
        Some(ManuallyDrop::new(unsafe {
            RedisKey::from_raw_parts(ctx, key)
        }))
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

    pub fn scan<F: FnMut(&RuntimeContext, &RedisString, Option<&RedisKey>)>(
        &self,
        ctx: &RuntimeContext,
        callback: &mut F,
    ) -> bool {
        let res = unsafe {
            crate::__required_api!(RedisModule_Scan)(
                ctx.ctx,
                self.inner_cursor,
                Some(scan_callback::<F>),
                callback as *mut F as *mut c_void,
            )
        };
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
