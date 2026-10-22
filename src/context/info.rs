use std::ffi::CString;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::RuntimeContext;
use crate::{RedisString, raw};

/// An INFO snapshot whose native allocation and cleanup belong to one live
/// runtime context. Drop it before leaving that context or unlocking its worker.
pub struct ServerInfo<'ctx> {
    ctx: *mut raw::RedisModuleCtx,
    _context: PhantomData<&'ctx RuntimeContext>,
    /// Null for a section name that cannot exist: such a `ServerInfo` has no
    /// fields.
    pub(crate) inner: *mut raw::RedisModuleServerInfoData,
}

impl Drop for ServerInfo<'_> {
    fn drop(&mut self) {
        if !self.inner.is_null() {
            unsafe { crate::__required_api!(RedisModule_FreeServerInfo)(self.ctx, self.inner) };
        }
    }
}

impl ServerInfo<'_> {
    /// The value of `field`, or `None` if the section has no such field. A
    /// name with a NUL byte names no field.
    pub fn field(&self, field: &str) -> Option<RedisString> {
        if self.inner.is_null() {
            return None;
        }
        let field = CString::new(field).ok()?;
        let value = unsafe {
            crate::__required_api!(RedisModule_ServerInfoGetField)(
                std::ptr::null_mut(),
                self.inner,
                field.as_ptr(),
            )
        };
        if value.is_null() {
            None
        } else {
            // Owned-result correction identified in upstream PR 487 (jonnyomerredis)
            // and refined for AM off/on in PR 488 (oshadmi).
            Some(unsafe { RedisString::from_owned_ptr(None, NonNull::new(value).unwrap()) })
        }
    }
}

impl RuntimeContext {
    /// The server's `INFO` fields in `section`. A section name with a NUL
    /// byte names no section, so its `ServerInfo` has no fields.
    ///
    /// The snapshot borrows this runtime view. Native cleanup uses its context,
    /// including when AutoMemory is disabled, so it must be dropped before the
    /// callback ends or a worker guard unlocks. Copy fields into owned Rust data
    /// if they are needed after that scope:
    ///
    /// ```no_run
    /// use redis_module::RuntimeContext;
    /// fn version(ctx: &RuntimeContext) -> Option<String> {
    ///     let info = ctx.server_info("server");
    ///     let value = info.field("redis_version")?;
    ///     value.try_as_str().ok().map(str::to_owned)
    /// }
    /// ```
    ///
    /// A snapshot cannot escape a worker's lock scope:
    ///
    /// ```compile_fail,E0597
    /// use redis_module::{DetachedFromClient, ThreadSafeContext};
    /// fn escape(owner: &ThreadSafeContext<DetachedFromClient>) {
    ///     let info = {
    ///         let guard = owner.lock();
    ///         guard.server_info("server")
    ///     };
    ///     drop(info);
    /// }
    /// ```
    ///
    /// Nor can its guard be explicitly unlocked before snapshot cleanup:
    ///
    /// ```compile_fail,E0505
    /// use redis_module::{DetachedFromClient, ThreadSafeContext};
    /// fn unlock(owner: &ThreadSafeContext<DetachedFromClient>) {
    ///     let guard = owner.lock();
    ///     let info = guard.server_info("server");
    ///     drop(guard);
    ///     drop(info);
    /// }
    /// ```
    #[must_use]
    pub fn server_info(&self, section: &str) -> ServerInfo<'_> {
        let Ok(section) = CString::new(section) else {
            return ServerInfo {
                ctx: self.ctx,
                _context: PhantomData,
                inner: std::ptr::null_mut(),
            };
        };
        let server_info = unsafe {
            crate::__required_api!(RedisModule_GetServerInfo)(
                self.ctx,         // ctx
                section.as_ptr(), // section
            )
        };

        ServerInfo {
            ctx: self.ctx,
            _context: PhantomData,
            inner: server_info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_with_a_nul_byte_has_no_fields() {
        // Neither path reaches the module API, so this runs without Redis.
        let info = ServerInfo {
            ctx: std::ptr::null_mut(),
            _context: PhantomData,
            inner: std::ptr::null_mut(),
        };
        assert!(info.field("redis_version").is_none());
        assert!(info.field("redis\0version").is_none());
        drop(info);
    }
}
