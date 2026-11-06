use crate::error::Error;
use crate::raw;
use crate::{RedisBuffer, RedisString};

fn decode_string(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(Error::from)
}

/// Wrapper around RedisModuleIO for type-safe serialization/deserialization.
///
/// This wrapper provides an ergonomic API for working with RedisModuleIO
/// RDB serialization operations while maintaining compatibility with the underlying C bindings.
pub struct RedisModuleIO {
    io: *mut raw::RedisModuleIO,
}

impl RedisModuleIO {
    /// Creates a new RedisModuleIO wrapper from a raw pointer.
    ///
    /// # Safety
    ///
    /// `io` must be the `RedisModuleIO` pointer Redis passed to the running
    /// `rdb_save`, `rdb_load` or aux callback, and the wrapper must not be used
    /// after that callback returns. A load callback must only read: while
    /// Redis loads a `RESTORE` payload, a write goes into the command's own
    /// argument and corrupts it.
    pub unsafe fn new(io: *mut raw::RedisModuleIO) -> Self {
        Self { io }
    }

    /// Returns the raw RedisModuleIO pointer.
    pub fn as_ptr(&self) -> *mut raw::RedisModuleIO {
        self.io
    }

    /// Reads a UTF-8 string from the IO stream.
    ///
    /// # Errors
    /// Returns an error if the operation fails, an IO error occurred, or the
    /// stored bytes are not valid UTF-8. Use [`Self::read_string_buffer`] for
    /// binary strings.
    pub fn read_string(&mut self) -> Result<String, Error> {
        // SAFETY: `new` requires a live callback IO.
        let string_ptr = unsafe { raw::load_string(self.io) }?;
        decode_string(string_ptr.as_slice().to_vec())
    }

    /// Reads a binary-safe string from the IO stream.
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_redis_string(&mut self) -> Result<RedisString, Error> {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::load_string(self.io) }
    }

    /// Writes a binary-safe string to the IO stream.
    pub fn write_redis_string(&mut self, s: &RedisString) {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::save_redis_string(self.io, s) };
    }

    /// Writes a string to the IO stream.
    pub fn write_string(&mut self, s: &str) {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::save_string(self.io, s) };
    }

    /// Reads an unsigned integer from the IO stream.
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_unsigned(&mut self) -> Result<u64, Error> {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::load_unsigned(self.io) }
    }

    /// Writes an unsigned integer to the IO stream.
    pub fn write_unsigned(&mut self, val: u64) {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::save_unsigned(self.io, val) };
    }

    /// Reads a signed integer from the IO stream.
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_signed(&mut self) -> Result<i64, Error> {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::load_signed(self.io) }
    }

    /// Writes a signed integer to the IO stream.
    pub fn write_signed(&mut self, val: i64) {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::save_signed(self.io, val) };
    }

    /// Reads a double from the IO stream.
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_double(&mut self) -> Result<f64, Error> {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::load_double(self.io) }
    }

    /// Writes a double to the IO stream.
    pub fn write_double(&mut self, val: f64) {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::save_double(self.io, val) };
    }

    /// Reads a float from the IO stream.
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_float(&mut self) -> Result<f32, Error> {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::load_float(self.io) }
    }

    /// Writes a float to the IO stream.
    pub fn write_float(&mut self, val: f32) {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::save_float(self.io, val) };
    }

    /// Reads a string buffer from the IO stream.
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_string_buffer(&mut self) -> Result<RedisBuffer, Error> {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::load_string_buffer(self.io) }
    }

    /// Writes a slice to the IO stream.
    pub fn write_slice(&mut self, buf: &[u8]) {
        // SAFETY: `new` requires a live callback IO.
        unsafe { raw::save_slice(self.io, buf) };
    }
}

/// The reads a native type's load callbacks get: [`NativeType::rdb_load`]
/// and [`AuxData::aux_load`]. It has no writes: during a `RESTORE` the IO
/// writes into the command's own argument, which would corrupt it.
///
/// A read that runs out of data returns an error only when the module sets
/// [`ModuleOptions::HANDLE_IO_ERRORS`]; without it, Redis aborts inside the
/// read.
///
/// [`ModuleOptions::HANDLE_IO_ERRORS`]: crate::ModuleOptions::HANDLE_IO_ERRORS
/// [`NativeType::rdb_load`]: crate::native_types::NativeType::rdb_load
/// [`AuxData::aux_load`]: crate::native_types::AuxData::aux_load
pub struct RdbReader {
    io: RedisModuleIO,
}

impl RdbReader {
    /// # Safety
    ///
    /// The requirements of [`RedisModuleIO::new`], for a load callback.
    pub(crate) unsafe fn new(io: *mut raw::RedisModuleIO) -> Self {
        // SAFETY: the caller passes a live load callback IO.
        Self {
            io: unsafe { RedisModuleIO::new(io) },
        }
    }

    /// The raw IO pointer, for raw APIs such as [`crate::logging::log_io_error`].
    ///
    /// With [`ModuleOptions::HANDLE_IO_ERRORS`] set, a raw read through this
    /// pointer that fails returns 0 or NULL; check
    /// [`is_io_error`](crate::raw::is_io_error) before using the result.
    ///
    /// [`ModuleOptions::HANDLE_IO_ERRORS`]: crate::ModuleOptions::HANDLE_IO_ERRORS
    pub fn as_ptr(&self) -> *mut raw::RedisModuleIO {
        self.io.as_ptr()
    }

    /// See [`RedisModuleIO::read_string`].
    ///
    /// # Errors
    /// Returns an error if the operation fails, an IO error occurred, or the
    /// stored bytes are not valid UTF-8.
    pub fn read_string(&mut self) -> Result<String, Error> {
        self.io.read_string()
    }

    /// See [`RedisModuleIO::read_redis_string`].
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_redis_string(&mut self) -> Result<RedisString, Error> {
        self.io.read_redis_string()
    }

    /// See [`RedisModuleIO::read_string_buffer`].
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_string_buffer(&mut self) -> Result<RedisBuffer, Error> {
        self.io.read_string_buffer()
    }

    /// See [`RedisModuleIO::read_unsigned`].
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_unsigned(&mut self) -> Result<u64, Error> {
        self.io.read_unsigned()
    }

    /// See [`RedisModuleIO::read_signed`].
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_signed(&mut self) -> Result<i64, Error> {
        self.io.read_signed()
    }

    /// See [`RedisModuleIO::read_double`].
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_double(&mut self) -> Result<f64, Error> {
        self.io.read_double()
    }

    /// See [`RedisModuleIO::read_float`].
    ///
    /// # Errors
    /// Returns an error if the operation fails or if an IO error occurred.
    pub fn read_float(&mut self) -> Result<f32, Error> {
        self.io.read_float()
    }
}

/// The writes a native type's save callbacks get: [`NativeType::rdb_save`]
/// and [`AuxData::aux_save`]. It has no reads, which Redis does not allow
/// while saving.
///
/// [`NativeType::rdb_save`]: crate::native_types::NativeType::rdb_save
/// [`AuxData::aux_save`]: crate::native_types::AuxData::aux_save
pub struct RdbWriter {
    io: RedisModuleIO,
}

impl RdbWriter {
    /// # Safety
    ///
    /// The requirements of [`RedisModuleIO::new`], for a save callback.
    pub(crate) unsafe fn new(io: *mut raw::RedisModuleIO) -> Self {
        // SAFETY: the caller passes a live save callback IO.
        Self {
            io: unsafe { RedisModuleIO::new(io) },
        }
    }

    /// The raw IO pointer, for raw APIs such as [`crate::logging::log_io_error`].
    ///
    /// In [`AuxData::aux_save`] on Redis 7.2 and later, a callback that passes
    /// it to `RedisModule_GetContextFromIO` must write at least one value:
    /// when nothing was written, Redis does not free that context. See
    /// [`AuxData::aux_save`] for the aborts that follow.
    ///
    /// [`AuxData::aux_save`]: crate::native_types::AuxData::aux_save
    pub fn as_ptr(&self) -> *mut raw::RedisModuleIO {
        self.io.as_ptr()
    }

    /// See [`RedisModuleIO::write_string`].
    pub fn write_string(&mut self, s: &str) {
        self.io.write_string(s);
    }

    /// See [`RedisModuleIO::write_redis_string`].
    pub fn write_redis_string(&mut self, s: &RedisString) {
        self.io.write_redis_string(s);
    }

    /// See [`RedisModuleIO::write_slice`].
    pub fn write_slice(&mut self, buf: &[u8]) {
        self.io.write_slice(buf);
    }

    /// See [`RedisModuleIO::write_unsigned`].
    pub fn write_unsigned(&mut self, val: u64) {
        self.io.write_unsigned(val);
    }

    /// See [`RedisModuleIO::write_signed`].
    pub fn write_signed(&mut self, val: i64) {
        self.io.write_signed(val);
    }

    /// See [`RedisModuleIO::write_double`].
    pub fn write_double(&mut self, val: f64) {
        self.io.write_double(val);
    }

    /// See [`RedisModuleIO::write_float`].
    pub fn write_float(&mut self, val: f32) {
        self.io.write_float(val);
    }
}

#[cfg(test)]
mod tests {
    use super::decode_string;
    use crate::error::Error;

    #[test]
    fn rdb_text_decoding_preserves_valid_bytes_and_refuses_invalid_utf8() {
        for bytes in [b"".as_slice(), b"a\0b", "héllo".as_bytes()] {
            assert_eq!(decode_string(bytes.to_vec()).unwrap().as_bytes(), bytes);
        }
        for bytes in [b"a\xffb".as_slice(), b"\xf0\x9f\x92"] {
            let Error::FromUtf8(error) = decode_string(bytes.to_vec()).unwrap_err() else {
                panic!("invalid RDB text did not return its UTF-8 error");
            };
            assert_eq!(error.into_bytes(), bytes);
        }
    }
}
