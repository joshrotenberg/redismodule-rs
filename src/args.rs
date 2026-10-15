use std::iter::FusedIterator;
use std::slice;

use crate::{RedisError, RedisString};

/// A cursor over borrowed command arguments.
///
/// Pass the slice you want to parse, excluding the command name when appropriate.
/// Advancing or dropping the cursor does not invalidate a returned reference:
/// references borrow the input slice, not the cursor. Text is strict UTF-8;
/// [`Self::next_bytes`] preserves arbitrary bytes. Neither conversion copies the
/// argument or retains another native string reference.
///
/// ```
/// use redis_module::{ArgCursor, RedisError, RedisString};
/// fn parse(argv: &[RedisString]) -> Result<(&RedisString, &str, u64), RedisError> {
///     let mut args = ArgCursor::new(argv);
///     let key = args.next_arg()?;
///     let expression = args.next_str()?;
///     let count = args.next_u64()?;
///     args.done()?;
///     Ok((key, expression, count))
/// }
/// ```
///
/// Borrowed text cannot outlive the input's owners:
///
/// ```compile_fail,E0515
/// use redis_module::{ArgCursor, RedisString};
/// fn escape(argv: Vec<RedisString>) -> &'static str {
///     ArgCursor::new(&argv).next_str().unwrap()
/// }
/// ```
///
/// Nor can the owner be dropped while returned bytes are still in use:
///
/// ```compile_fail,E0505
/// use redis_module::{ArgCursor, RedisString};
/// fn drop_owner(argv: Vec<RedisString>) {
///     let bytes = ArgCursor::new(&argv).next_bytes().unwrap();
///     drop(argv);
///     println!("{bytes:?}");
/// }
/// ```
///
/// Safe Rust mutation through the same owner is excluded during the borrow:
///
/// ```compile_fail,E0502
/// use redis_module::{ArgCursor, RedisString};
/// fn mutate_owner(argv: &mut [RedisString]) {
///     let bytes = ArgCursor::new(argv).next_bytes().unwrap();
///     argv[0].append("more");
///     println!("{bytes:?}");
/// }
/// ```
///
/// The cursor uses existing [`RedisString`] operations. It does not strengthen
/// the SDK's raw-pointer, native aliasing, callback or threading contracts.
#[derive(Clone, Debug)]
pub struct ArgCursor<'a> {
    args: slice::Iter<'a, RedisString>,
}

impl<'a> ArgCursor<'a> {
    #[must_use]
    pub fn new(args: &'a [RedisString]) -> Self {
        Self { args: args.iter() }
    }

    /// Return the unconsumed arguments without advancing the cursor.
    #[must_use]
    pub fn remaining(&self) -> &'a [RedisString] {
        self.args.as_slice()
    }

    pub fn next_arg(&mut self) -> Result<&'a RedisString, RedisError> {
        self.next().ok_or(RedisError::WrongArity)
    }

    pub fn next_bytes(&mut self) -> Result<&'a [u8], RedisError> {
        Ok(self.next_arg()?.as_slice())
    }

    /// Consume one argument, rejecting invalid UTF-8 without replacing bytes.
    pub fn next_str(&mut self) -> Result<&'a str, RedisError> {
        std::str::from_utf8(self.next_bytes()?)
            .map_err(|_| RedisError::Str("Couldn't parse as UTF-8 string"))
    }

    /// Parse with Redis's integer parser, as [`crate::NextArg::next_i64`] does.
    pub fn next_i64(&mut self) -> Result<i64, RedisError> {
        self.next_arg()?.parse_integer()
    }

    /// Preserve the existing SDK's unsigned range: zero through `i64::MAX`.
    /// Negative values and integers above `i64::MAX` are rejected.
    pub fn next_u64(&mut self) -> Result<u64, RedisError> {
        self.next_arg()?.parse_unsigned_integer()
    }

    /// Parse with Redis's float parser, including its existing special values.
    pub fn next_f64(&mut self) -> Result<f64, RedisError> {
        self.next_arg()?.parse_float()
    }

    /// Check exact arity without consuming an unexpected argument.
    pub fn done(&self) -> Result<(), RedisError> {
        if self.args.len() == 0 {
            Ok(())
        } else {
            Err(RedisError::WrongArity)
        }
    }
}

impl<'a> Iterator for ArgCursor<'a> {
    type Item = &'a RedisString;

    fn next(&mut self) -> Option<Self::Item> {
        self.args.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.args.size_hint()
    }
}

impl ExactSizeIterator for ArgCursor<'_> {}
impl FusedIterator for ArgCursor<'_> {}
