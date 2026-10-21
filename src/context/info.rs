use std::ffi::CString;
use std::ptr::NonNull;

use crate::RuntimeContext;
use crate::{RedisString, raw};

pub struct ServerInfo {
    ctx: *mut raw::RedisModuleCtx,
    /// Null for a section name that cannot exist: such a `ServerInfo` has no
    /// fields.
    pub(crate) inner: *mut raw::RedisModuleServerInfoData,
}

impl Drop for ServerInfo {
    fn drop(&mut self) {
        if !self.inner.is_null() {
            unsafe { crate::__required_api!(RedisModule_FreeServerInfo)(self.ctx, self.inner) };
        }
    }
}

impl ServerInfo {
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
    #[must_use]
    pub fn server_info(&self, section: &str) -> ServerInfo {
        let Ok(section) = CString::new(section) else {
            return ServerInfo {
                ctx: self.ctx,
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
            inner: std::ptr::null_mut(),
        };
        assert!(info.field("redis_version").is_none());
        assert!(info.field("redis\0version").is_none());
        drop(info);
    }
}
