use std::ffi::CString;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::raw;

/// A native data type that a module registers with Redis.
///
/// The name must be 9 characters from `A-Z`, `a-z`, `0-9`, `-` and `_`, and
/// the encoding version must be between 0 and 1023. [`RedisType::new`] checks
/// both, so a `static` with a bad name or version fails to compile:
///
/// ```
/// use redis_module::{native_types::RedisType, raw};
///
/// const METHODS: raw::RedisModuleTypeMethods = raw::RedisModuleTypeMethods {
///     version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
///     ..unsafe { std::mem::zeroed() }
/// };
/// static MY_TYPE: RedisType = RedisType::new("mytype-01", 0, METHODS);
/// ```
///
/// ```compile_fail
/// use redis_module::{native_types::RedisType, raw};
///
/// const METHODS: raw::RedisModuleTypeMethods = raw::RedisModuleTypeMethods {
///     version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
///     ..unsafe { std::mem::zeroed() }
/// };
/// static TOO_LONG: RedisType = RedisType::new("mytype-0001", 0, METHODS);
/// ```
///
/// ```compile_fail
/// use redis_module::{native_types::RedisType, raw};
///
/// const METHODS: raw::RedisModuleTypeMethods = raw::RedisModuleTypeMethods {
///     version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
///     ..unsafe { std::mem::zeroed() }
/// };
/// static BAD_VERSION: RedisType = RedisType::new("mytype-01", 1024, METHODS);
/// ```
///
/// Redis also rejects `AAAAAAAAA` with version 0, whose type id is 0:
///
/// ```compile_fail
/// use redis_module::{native_types::RedisType, raw};
///
/// const METHODS: raw::RedisModuleTypeMethods = raw::RedisModuleTypeMethods {
///     version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
///     ..unsafe { std::mem::zeroed() }
/// };
/// static ZERO_ID: RedisType = RedisType::new("AAAAAAAAA", 0, METHODS);
/// ```
pub struct RedisType {
    name: &'static str,
    version: i32,
    type_methods: raw::RedisModuleTypeMethods,
    raw_type: AtomicPtr<raw::RedisModuleType>,
}

// SAFETY: the registered type is an atomic pointer, and the other fields are
// read only. A static RedisType needs Sync.
unsafe impl Sync for RedisType {}

/// Whether `byte` may appear in a native type name.
const fn is_type_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

impl RedisType {
    /// # Panics
    ///
    /// Panics, at compile time for a `static`, if `name` is not 9 characters
    /// from `A-Z`, `a-z`, `0-9`, `-` and `_`, or `version` is not between 0
    /// and 1023.
    #[must_use]
    pub const fn new(
        name: &'static str,
        version: i32,
        type_methods: raw::RedisModuleTypeMethods,
    ) -> Self {
        let bytes = name.as_bytes();
        assert!(
            bytes.len() == 9,
            "a native type name must be exactly 9 characters"
        );
        let mut i = 0;
        while i < bytes.len() {
            assert!(
                is_type_name_byte(bytes[i]),
                "a native type name may only use A-Z, a-z, 0-9, - and _"
            );
            i += 1;
        }
        assert!(
            version >= 0 && version <= 1023,
            "a native type encoding version must be between 0 and 1023"
        );
        // `A` is the first character of the type id alphabet, so this name and
        // version encode to id 0, which Redis rejects.
        let mut all_a = true;
        let mut j = 0;
        while j < bytes.len() {
            if bytes[j] != b'A' {
                all_a = false;
            }
            j += 1;
        }
        assert!(
            !(all_a && version == 0),
            "the native type name AAAAAAAAA with version 0 has type id 0, which Redis rejects"
        );
        Self {
            name,
            version,
            type_methods,
            raw_type: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// The type Redis registered for this `RedisType`, or null before the
    /// module registers it.
    #[must_use]
    pub fn raw_type(&self) -> *mut raw::RedisModuleType {
        self.raw_type.load(Ordering::Acquire)
    }

    /// Registers this type with Redis. `redis_module!` calls it for each entry
    /// in `data_types`; call it during the module's load.
    pub fn create_data_type(&self, ctx: &crate::RegistrationContext<'_>) -> Result<(), &str> {
        let type_name = CString::new(self.name).unwrap();

        let redis_type = unsafe {
            crate::__required_api!(RedisModule_CreateDataType)(
                ctx.ctx,
                type_name.as_ptr(),
                self.version, // Encoding version
                &mut self.type_methods.clone(),
            )
        };

        if redis_type.is_null() {
            ctx.log_notice("Error: created data type is null");
            return Err("Error: created data type is null");
        }

        self.raw_type.store(redis_type, Ordering::Release);

        ctx.log_notice(&format!("Created new data type '{}'", self.name));

        Ok(())
    }
}
