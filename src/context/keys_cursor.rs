use crate::context::Context;
use crate::key::RedisKey;
use crate::raw;
use crate::redismodule::RedisString;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

pub struct KeysCursor {
    inner_cursor: *mut raw::RedisModuleScanCursor,
}

extern "C" fn scan_callback<C: FnMut(&Context, &RedisString, Option<&RedisKey>)>(
    ctx: *mut raw::RedisModuleCtx,
    key_name: *mut raw::RedisModuleString,
    key: *mut raw::RedisModuleKey,
    private_data: *mut ::std::os::raw::c_void,
) {
    let context = Context::new(ctx);
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
        let inner_cursor = unsafe { raw::RedisModule_ScanCursorCreate.unwrap()() };
        Self { inner_cursor }
    }

    pub fn scan<F: FnMut(&Context, &RedisString, Option<&RedisKey>)>(
        &self,
        ctx: &Context,
        callback: &mut F,
    ) -> bool {
        let res = unsafe {
            raw::RedisModule_Scan.unwrap()(
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
        unsafe { raw::RedisModule_ScanCursorRestart.unwrap()(self.inner_cursor) };
    }
}

impl Default for KeysCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeysCursor {
    fn drop(&mut self) {
        unsafe { raw::RedisModule_ScanCursorDestroy.unwrap()(self.inner_cursor) };
    }
}
