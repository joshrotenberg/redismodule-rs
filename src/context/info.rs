use std::ffi::CString;
use std::ptr::NonNull;

use crate::Context;
use crate::{raw, RedisString};

pub struct ServerInfo {
    ctx: *mut raw::RedisModuleCtx,
    pub(crate) inner: *mut raw::RedisModuleServerInfoData,
}

impl Drop for ServerInfo {
    fn drop(&mut self) {
        unsafe { crate::__required_api!(RedisModule_FreeServerInfo)(self.ctx, self.inner) };
    }
}

impl ServerInfo {
    pub fn field(&self, field: &str) -> Option<RedisString> {
        let field = CString::new(field).unwrap();
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

impl Context {
    #[must_use]
    pub fn server_info(&self, section: &str) -> ServerInfo {
        let section = CString::new(section).unwrap();
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
