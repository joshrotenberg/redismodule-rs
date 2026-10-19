use crate::context::call_reply::{ErrorCallReply, ErrorReply};
pub use crate::raw;
use std::ffi::CStr;
use std::fmt;

#[derive(Debug)]
pub enum RedisError {
    WrongArity,
    Str(&'static str),
    String(String),
    WrongType,
}

impl<'root> From<ErrorCallReply<'root>> for RedisError {
    fn from(err: ErrorCallReply<'root>) -> Self {
        RedisError::String(
            err.to_utf8_string()
                .unwrap_or("can not convert error into String".into()),
        )
    }
}

impl<'root> From<ErrorReply<'root>> for RedisError {
    fn from(err: ErrorReply<'root>) -> Self {
        RedisError::String(
            err.to_utf8_string()
                .unwrap_or("can not convert error into String".into()),
        )
    }
}

impl RedisError {
    #[must_use]
    pub const fn nonexistent_key() -> Self {
        Self::Str("ERR could not perform this operation on a key that doesn't exist")
    }

    #[must_use]
    pub const fn short_read() -> Self {
        Self::Str("ERR short read or OOM loading DB")
    }
}

impl std::error::Error for RedisError {}

impl RedisError {
    /// An `ERR` error with the message of `error`, for error types without a
    /// `From` conversion: `result.map_err(RedisError::from_error)?`. It takes
    /// any `Display`, so `anyhow::Error` and `Box<dyn Error>` work too.
    pub fn from_error(error: impl fmt::Display) -> Self {
        Self::String(format!("ERR {error}"))
    }
}

/// `From` conversions for the standard errors that `?` meets in module code.
/// Each becomes `ERR <message>`.
macro_rules! from_std_errors {
    ($($error:ty),* $(,)?) => {
        $(
            impl From<$error> for RedisError {
                fn from(error: $error) -> Self {
                    Self::from_error(error)
                }
            }
        )*
    };
}

from_std_errors!(
    std::alloc::LayoutError,
    std::array::TryFromSliceError,
    std::cell::BorrowError,
    std::cell::BorrowMutError,
    std::char::CharTryFromError,
    std::char::ParseCharError,
    std::char::TryFromCharError,
    std::collections::TryReserveError,
    std::convert::Infallible,
    std::env::VarError,
    std::ffi::FromBytesUntilNulError,
    std::ffi::FromBytesWithNulError,
    std::ffi::FromVecWithNulError,
    std::ffi::IntoStringError,
    std::ffi::NulError,
    std::fmt::Error,
    std::io::Error,
    std::net::AddrParseError,
    std::num::ParseFloatError,
    std::num::ParseIntError,
    std::num::TryFromIntError,
    std::str::ParseBoolError,
    std::str::Utf8Error,
    std::string::FromUtf16Error,
    std::string::FromUtf8Error,
    std::sync::mpsc::RecvError,
    std::sync::mpsc::RecvTimeoutError,
    std::sync::mpsc::TryRecvError,
    std::time::SystemTimeError,
    crate::error::Error,
    crate::error::GenericError,
);

impl<T> From<std::sync::PoisonError<T>> for RedisError {
    fn from(error: std::sync::PoisonError<T>) -> Self {
        Self::from_error(error)
    }
}

impl<T> From<std::sync::mpsc::SendError<T>> for RedisError {
    fn from(error: std::sync::mpsc::SendError<T>) -> Self {
        Self::from_error(error)
    }
}

impl fmt::Display for RedisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = match self {
            Self::WrongArity => "Wrong Arity",
            // remove NUL from the end of raw::REDISMODULE_ERRORMSG_WRONGTYPE
            // before converting &[u8] to &str to ensure CString::new() doesn't
            // panic when this is passed to it.
            Self::WrongType => std::str::from_utf8(
                CStr::from_bytes_with_nul(raw::REDISMODULE_ERRORMSG_WRONGTYPE)
                    .unwrap()
                    .to_bytes(),
            )
            .unwrap(),
            Self::Str(s) => s,
            Self::String(s) => s.as_str(),
        };

        write!(f, "{d}")
    }
}

#[cfg(test)]
mod tests {
    use super::RedisError;

    #[test]
    fn redis_error_is_a_std_error() {
        fn assert_std_error<E: std::error::Error + Send + Sync + 'static>() {}
        assert_std_error::<RedisError>();
        let boxed: Box<dyn std::error::Error + Send + Sync> = RedisError::Str("ERR boom").into();
        assert_eq!(boxed.to_string(), "ERR boom");
    }

    #[test]
    fn std_errors_convert_with_an_err_prefix() {
        fn parse(text: &str) -> Result<i64, RedisError> {
            Ok(text.parse::<i64>()?)
        }
        assert_eq!(
            parse("x").unwrap_err().to_string(),
            "ERR invalid digit found in string"
        );
        let error = std::ffi::CString::new("a\0b")
            .map_err(RedisError::from_error)
            .unwrap_err();
        assert!(error.to_string().starts_with("ERR "), "{error}");

        let lock = std::sync::Mutex::new(0);
        let _ = std::panic::catch_unwind(|| {
            let _guard = lock.lock().unwrap();
            panic!("poison the lock");
        });
        fn locked(lock: &std::sync::Mutex<i32>) -> Result<i32, RedisError> {
            Ok(*lock.lock()?)
        }
        assert!(locked(&lock).unwrap_err().to_string().starts_with("ERR "));

        let boxed: Box<dyn std::error::Error + Send + Sync> = "boxed".into();
        assert_eq!(RedisError::from_error(boxed).to_string(), "ERR boxed");
    }
}
