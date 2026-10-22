use core::slice;
use std::os::raw::c_char;
use std::{
    fmt,
    fmt::{Debug, Display, Formatter},
    marker::PhantomData,
    ptr::NonNull,
};

use libc::c_void;

use crate::{EventContext, RedisError, RedisLockIndicator, RuntimeContext, raw::*};

pub struct StringCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> StringCallReply<'root> {
    /// Convert StringCallReply to String.
    /// Return None data is not a valid utf8.
    pub fn to_string(&self) -> Option<String> {
        String::from_utf8(self.as_bytes().to_vec()).ok()
    }

    /// Whether Redis sent this reply as a simple string, such as `+OK`, rather
    /// than a bulk string. Read from the reply's protocol through
    /// `RedisModule_CallReplyProto`.
    pub fn is_simple_string(&self) -> bool {
        let mut len: usize = 0;
        let proto = unsafe {
            crate::__required_api!(RedisModule_CallReplyProto)(self.reply.as_ptr(), &mut len)
        };
        // SAFETY: Redis returns `len` readable protocol bytes for the reply.
        !proto.is_null() && len > 0 && unsafe { *proto } as u8 == b'+'
    }

    /// Return the StringCallReply data as &[u8]
    pub fn as_bytes(&self) -> &[u8] {
        let mut len: usize = 0;
        let reply_string: *mut u8 = unsafe {
            crate::__required_api!(RedisModule_CallReplyStringPtr)(self.reply.as_ptr(), &mut len)
                as *mut u8
        };
        unsafe { slice::from_raw_parts(reply_string, len) }
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

impl<'root> Drop for StringCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for StringCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("StringCallReply");
        let debug_struct = debug_struct.field("reply", &self.reply);
        match self.to_string() {
            Some(s) => debug_struct.field("value", &s),
            None => debug_struct.field("value", &self.as_bytes()),
        }
        .finish()
    }
}

impl<'root> Display for StringCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&String::from_utf8_lossy(self.as_bytes()), f)
    }
}

pub struct ErrorCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> ErrorCallReply<'root> {
    /// Borrows the error prefix, such as `WRONGTYPE`, without decoding UTF-8.
    ///
    /// This is the payload before the first ASCII space, or the entire payload
    /// if there is no space. An empty payload has an empty code. The prefix is
    /// a Redis convention and is not validated or normalized here.
    #[must_use]
    pub fn code(&self) -> &[u8] {
        error_code(self.as_bytes())
    }

    /// Convert ErrorCallReply to String.
    /// Return None data is not a valid utf8.
    pub fn to_utf8_string(&self) -> Option<String> {
        String::from_utf8(self.as_bytes().to_vec()).ok()
    }

    /// Return the ErrorCallReply data as &[u8]
    pub fn as_bytes(&self) -> &[u8] {
        let mut len: usize = 0;
        let reply_string: *mut u8 = unsafe {
            crate::__required_api!(RedisModule_CallReplyStringPtr)(self.reply.as_ptr(), &mut len)
                as *mut u8
        };
        unsafe { slice::from_raw_parts(reply_string, len) }
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

impl<'root> Drop for ErrorCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for ErrorCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("ErrorCallReply");
        let debug_struct = debug_struct.field("reply", &self.reply);
        match self.to_utf8_string() {
            Some(s) => debug_struct.field("value", &s),
            None => debug_struct.field("value", &self.as_bytes()),
        }
        .finish()
    }
}

impl<'root> Display for ErrorCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&String::from_utf8_lossy(self.as_bytes()), f)
    }
}

#[derive(Debug)]
pub enum ErrorReply<'root> {
    Message(String),
    RedisError(ErrorCallReply<'root>),
}

/// Send implementation to [ErrorCallReply].
/// We need to implements this trait because [ErrorCallReply] hold
/// raw pointers to C data which does not auto implement the [Send] trait.
/// By implementing [Send] on [ErrorCallReply] we basically tells the compiler
/// that it is safe to send the underline C data between threads.
unsafe impl<'root> Send for ErrorCallReply<'root> {}

impl<'root> ErrorReply<'root> {
    /// Borrows the error prefix from either a native error reply or a message.
    ///
    /// Returns the bytes before the first ASCII space, or the entire message
    /// if no space exists. The message need not be valid UTF-8.
    ///
    /// ```
    /// use redis_module::ErrorReply;
    /// let error = ErrorReply::Message("BUSYGROUP group already exists".into());
    /// assert_eq!(error.code(), b"BUSYGROUP");
    /// ```
    #[must_use]
    pub fn code(&self) -> &[u8] {
        match self {
            ErrorReply::Message(message) => error_code(message.as_bytes()),
            ErrorReply::RedisError(reply) => reply.code(),
        }
    }

    /// Convert this reply to a [`String`], or return [`None`] for invalid UTF-8.
    pub fn to_utf8_string(&self) -> Option<String> {
        match self {
            ErrorReply::Message(s) => Some(s.clone()),
            ErrorReply::RedisError(r) => r.to_utf8_string(),
        }
    }

    /// Return the ErrorCallReply data as &[u8]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            ErrorReply::Message(s) => s.as_bytes(),
            ErrorReply::RedisError(r) => r.as_bytes(),
        }
    }
}

impl<'root> Display for ErrorReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&String::from_utf8_lossy(self.as_bytes()), f)
    }
}

fn error_code(message: &[u8]) -> &[u8] {
    let end = message
        .iter()
        .position(|byte| *byte == b' ')
        .unwrap_or(message.len());
    &message[..end]
}

pub struct I64CallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> I64CallReply<'root> {
    /// Return the i64 value of the [I64CallReply]
    pub fn to_i64(&self) -> i64 {
        call_reply_integer(self.reply.as_ptr())
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

impl<'root> Drop for I64CallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for I64CallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("I64CallReply")
            .field("reply", &self.reply)
            .field("value", &self.to_i64())
            .finish()
    }
}

impl<'root> Display for I64CallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.to_i64(), f)
    }
}

pub struct ArrayCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> Drop for ArrayCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> ArrayCallReply<'root> {
    /// Return an Iterator that allows to iterate over the elements
    /// in the [ArrayCallReply].
    pub fn iter(&self) -> ArrayCallReplyIterator<'root, '_> {
        ArrayCallReplyIterator {
            reply: self,
            index: 0,
        }
    }

    /// Return the array element on the given index.
    pub fn get(&self, idx: usize) -> Option<CallResult<'_>> {
        let res = NonNull::new(call_reply_array_element(self.reply.as_ptr(), idx))?;
        Some(create_call_reply(res))
    }

    /// Return the number of elements in the [ArrayCallReply].
    pub fn len(&self) -> usize {
        call_reply_length(self.reply.as_ptr())
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

pub struct ArrayCallReplyIterator<'root, 'curr> {
    reply: &'curr ArrayCallReply<'root>,
    index: usize,
}

impl<'root, 'curr> Iterator for ArrayCallReplyIterator<'root, 'curr> {
    type Item = CallResult<'curr>;

    fn next(&mut self) -> Option<Self::Item> {
        let res = self.reply.get(self.index);
        if res.is_some() {
            self.index += 1;
        }
        res
    }
}

impl<'root> Debug for ArrayCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArrayCallReply")
            .field("reply", &self.reply)
            .field("elements", &self.iter().collect::<Vec<CallResult>>())
            .finish()
    }
}

impl<'root> Display for ArrayCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;

        self.iter()
            .enumerate()
            .try_for_each(|(index, v)| -> fmt::Result {
                if index > 1 {
                    f.write_str(", ")?;
                }
                fmt_call_result(v, f)
            })?;

        f.write_str("]")
    }
}

pub struct NullCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> NullCallReply<'root> {
    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

impl<'root> Drop for NullCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for NullCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("NullCallReply")
            .field("reply", &self.reply)
            .finish()
    }
}

impl<'root> Display for NullCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("Null")
    }
}

pub struct MapCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> MapCallReply<'root> {
    /// Return an iterator over the elements in the [MapCallReply].
    /// The iterator return each element as a tuple representing the
    /// key and the value.
    pub fn iter(&self) -> MapCallReplyIterator<'root, '_> {
        MapCallReplyIterator {
            reply: self,
            index: 0,
        }
    }

    /// Return the map element on the given index.
    pub fn get(&self, idx: usize) -> Option<(CallResult<'_>, CallResult<'_>)> {
        let (key, val) = call_reply_map_element(self.reply.as_ptr(), idx);
        Some((
            create_call_reply(NonNull::new(key)?),
            create_call_reply(NonNull::new(val)?),
        ))
    }

    /// Return the number of elements in the [MapCallReply].
    pub fn len(&self) -> usize {
        call_reply_length(self.reply.as_ptr())
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

pub struct MapCallReplyIterator<'root, 'curr> {
    reply: &'curr MapCallReply<'root>,
    index: usize,
}

impl<'root, 'curr> Iterator for MapCallReplyIterator<'root, 'curr> {
    type Item = (CallResult<'curr>, CallResult<'curr>);

    fn next(&mut self) -> Option<Self::Item> {
        let res = self.reply.get(self.index);
        if res.is_some() {
            self.index += 1;
        }
        res
    }
}

impl<'root> Drop for MapCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for MapCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("MapCallReply")
            .field("reply", &self.reply)
            .field(
                "elements",
                &self.iter().collect::<Vec<(CallResult, CallResult)>>(),
            )
            .finish()
    }
}

impl<'root> Display for MapCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;

        self.iter()
            .enumerate()
            .try_for_each(|(index, (key, val))| -> fmt::Result {
                if index > 1 {
                    f.write_str(", ")?;
                }
                f.write_str("")?;
                fmt_call_result(key, f)?;
                f.write_str(": ")?;
                fmt_call_result(val, f)
            })?;

        f.write_str("}")
    }
}

pub struct SetCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> SetCallReply<'root> {
    /// Return an iterator over the elements in the [SetCallReply].
    pub fn iter(&self) -> SetCallReplyIterator<'root, '_> {
        SetCallReplyIterator {
            reply: self,
            index: 0,
        }
    }

    /// Return the set element on the given index.
    pub fn get(&self, idx: usize) -> Option<CallResult<'_>> {
        let res = NonNull::new(call_reply_set_element(self.reply.as_ptr(), idx))?;
        Some(create_call_reply(res))
    }

    /// Return the number of elements in the [SetCallReply].
    pub fn len(&self) -> usize {
        call_reply_length(self.reply.as_ptr())
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

pub struct SetCallReplyIterator<'root, 'curr> {
    reply: &'curr SetCallReply<'root>,
    index: usize,
}

impl<'root, 'curr> Iterator for SetCallReplyIterator<'root, 'curr> {
    type Item = CallResult<'curr>;

    fn next(&mut self) -> Option<Self::Item> {
        let res = self.reply.get(self.index);
        if res.is_some() {
            self.index += 1;
        }
        res
    }
}

impl<'root> Drop for SetCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for SetCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetCallReply")
            .field("reply", &self.reply)
            .field("elements", &self.iter().collect::<Vec<CallResult>>())
            .finish()
    }
}

impl<'root> Display for SetCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;

        self.iter()
            .enumerate()
            .try_for_each(|(index, v)| -> fmt::Result {
                if index > 1 {
                    f.write_str(", ")?;
                }
                fmt_call_result(v, f)
            })?;

        f.write_str("}")
    }
}

pub struct BoolCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> BoolCallReply<'root> {
    /// Return the boolean value of the [BoolCallReply].
    pub fn to_bool(&self) -> bool {
        call_reply_bool(self.reply.as_ptr())
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

impl<'root> Drop for BoolCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for BoolCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoolCallReply")
            .field("reply", &self.reply)
            .field("value", &self.to_bool())
            .finish()
    }
}

impl<'root> Display for BoolCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.to_bool(), f)
    }
}

pub struct DoubleCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> DoubleCallReply<'root> {
    /// Return the double value of the [BoolCallReply] as f64.
    pub fn to_double(&self) -> f64 {
        call_reply_double(self.reply.as_ptr())
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

impl<'root> Drop for DoubleCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for DoubleCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("DoubleCallReply")
            .field("reply", &self.reply)
            .field("value", &self.to_double())
            .finish()
    }
}

impl<'root> Display for DoubleCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.to_double(), f)
    }
}

pub struct BigNumberCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> BigNumberCallReply<'root> {
    /// Return the big number value of the [BigNumberCallReply] as String.
    /// Return None if the data is not a valid utf8
    pub fn to_string(&self) -> Option<String> {
        call_reply_big_number(self.reply.as_ptr())
    }

    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

impl<'root> Drop for BigNumberCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for BigNumberCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("BigNumberCallReply")
            .field("reply", &self.reply)
            .field("value", &self.to_string())
            .finish()
    }
}

impl<'root> Display for BigNumberCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.to_string().as_deref().unwrap_or("None"), f)
    }
}

pub struct VerbatimStringCallReply<'root> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<&'root ()>,
}

impl<'root> VerbatimStringCallReply<'root> {
    /// Return the raw pointer to the underlying [RedisModuleCallReply].
    pub fn get_raw(&self) -> *mut RedisModuleCallReply {
        self.reply.as_ptr()
    }
}

/// RESP3 state that the verbatim string format must be of length 3.
const VERBATIM_FORMAT_LENGTH: usize = 3;
/// The string format of a verbatim string ([VerbatimStringCallReply]).
#[repr(transparent)]
#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub struct VerbatimStringFormat(pub [c_char; VERBATIM_FORMAT_LENGTH]);

impl TryFrom<&str> for VerbatimStringFormat {
    type Error = RedisError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.len() != 3 {
            return Err(RedisError::String(format!(
                "Verbatim format length must be {VERBATIM_FORMAT_LENGTH}."
            )));
        }
        let mut res = VerbatimStringFormat::default();
        value.chars().take(3).enumerate().try_for_each(|(i, c)| {
            if c as u32 >= 127 {
                return Err(RedisError::String(
                    "Verbatim format must contains only ASCI values.".to_owned(),
                ));
            }
            res.0[i] = c as c_char;
            Ok(())
        })?;
        Ok(res)
    }
}

impl<'root> VerbatimStringCallReply<'root> {
    /// Return the verbatim string value of the [VerbatimStringCallReply] as a tuple.
    /// The first entry represents the format, the second entry represent the data.
    /// Return None if the format is not a valid utf8
    pub fn to_parts(&self) -> Option<(VerbatimStringFormat, Vec<u8>)> {
        let (format, data) = self.as_parts()?;
        Some((format.try_into().ok()?, data.to_vec()))
    }

    /// Borrow the verbatim string value of the [VerbatimStringCallReply] as a tuple.
    /// The first entry represents the format as &str, the second entry represent the data as &[u8].
    /// Return None if the format is not a valid utf8.
    pub fn as_parts(&self) -> Option<(&str, &[u8])> {
        // RESP3 state that veribatim string format must be of size 3.
        const FORMAT_LEN: usize = 3;
        let mut len: usize = 0;
        let mut format: *const c_char = std::ptr::null();
        let reply_string: *mut u8 = unsafe {
            crate::__required_api!(RedisModule_CallReplyVerbatim)(
                self.reply.as_ptr(),
                &mut len,
                &mut format,
            ) as *mut u8
        };
        if reply_string.is_null() || format.is_null() {
            return None;
        }
        Some((
            std::str::from_utf8(unsafe { slice::from_raw_parts(format as *const u8, FORMAT_LEN) })
                .ok()?,
            unsafe { slice::from_raw_parts(reply_string, len) },
        ))
    }
}

impl<'root> Drop for VerbatimStringCallReply<'root> {
    fn drop(&mut self) {
        free_call_reply(self.reply.as_ptr());
    }
}

impl<'root> Debug for VerbatimStringCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerbatimStringCallReply")
            .field("reply", &self.reply)
            .field("value", &self.as_parts())
            .finish()
    }
}

impl<'root> Display for VerbatimStringCallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.as_parts() {
            Some((format, data)) => write!(f, "({}, {})", format, String::from_utf8_lossy(data)),
            None => f.write_str("(None)"),
        }
    }
}

#[derive(Debug)]
pub enum CallReply<'root> {
    Unknown,
    I64(I64CallReply<'root>),
    String(StringCallReply<'root>),
    Array(ArrayCallReply<'root>),
    Null(NullCallReply<'root>),
    Map(MapCallReply<'root>),
    Set(SetCallReply<'root>),
    Bool(BoolCallReply<'root>),
    Double(DoubleCallReply<'root>),
    BigNumber(BigNumberCallReply<'root>),
    VerbatimString(VerbatimStringCallReply<'root>),
}

impl<'root> CallReply<'root> {
    /// Return the raw pointer to the underlying [RedisModuleCallReply], or `None` if this is the `Unknown` variant.
    pub fn get_raw(&self) -> Option<*mut RedisModuleCallReply> {
        match self {
            CallReply::Unknown => None,
            CallReply::I64(inner) => Some(inner.get_raw()),
            CallReply::String(inner) => Some(inner.get_raw()),
            CallReply::Array(inner) => Some(inner.get_raw()),
            CallReply::Null(inner) => Some(inner.get_raw()),
            CallReply::Map(inner) => Some(inner.get_raw()),
            CallReply::Set(inner) => Some(inner.get_raw()),
            CallReply::Bool(inner) => Some(inner.get_raw()),
            CallReply::Double(inner) => Some(inner.get_raw()),
            CallReply::BigNumber(inner) => Some(inner.get_raw()),
            CallReply::VerbatimString(inner) => Some(inner.get_raw()),
        }
    }
}

/// Send implementation to [CallReply].
/// We need to implements this trait because [CallReply] hold
/// raw pointers to C data which does not auto implement the [Send] trait.
/// By implementing [Send] on [CallReply] we basically tells the compiler
/// that it is safe to send the underline C data between threads.
unsafe impl<'root> Send for CallReply<'root> {}

impl<'root> Display for CallReply<'root> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            CallReply::Unknown => f.write_str("Unknown"),
            CallReply::I64(inner) => fmt::Display::fmt(&inner, f),
            CallReply::String(inner) => fmt::Display::fmt(&inner, f),
            CallReply::Array(inner) => fmt::Display::fmt(&inner, f),
            CallReply::Null(inner) => fmt::Display::fmt(&inner, f),
            CallReply::Map(inner) => fmt::Display::fmt(&inner, f),
            CallReply::Set(inner) => fmt::Display::fmt(&inner, f),
            CallReply::Bool(inner) => fmt::Display::fmt(&inner, f),
            CallReply::Double(inner) => fmt::Display::fmt(&inner, f),
            CallReply::BigNumber(inner) => fmt::Display::fmt(&inner, f),
            CallReply::VerbatimString(inner) => fmt::Display::fmt(&inner, f),
        }
    }
}

fn create_call_reply<'root>(reply: NonNull<RedisModuleCallReply>) -> CallResult<'root> {
    let ty = call_reply_type(reply.as_ptr());
    match ty {
        // A reply of a type the crate does not wrap: an attribute reply, a
        // promise (handled before this point), or a type Redis adds later.
        ReplyType::Unknown | ReplyType::Attribute | ReplyType::Promise => {
            free_call_reply(reply.as_ptr());
            Ok(CallReply::Unknown)
        }
        ReplyType::Integer => Ok(CallReply::I64(I64CallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::String => Ok(CallReply::String(StringCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::Error => Err(ErrorReply::RedisError(ErrorCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::Array => Ok(CallReply::Array(ArrayCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::Null => Ok(CallReply::Null(NullCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::Map => Ok(CallReply::Map(MapCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::Set => Ok(CallReply::Set(SetCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::Bool => Ok(CallReply::Bool(BoolCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::Double => Ok(CallReply::Double(DoubleCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::BigNumber => Ok(CallReply::BigNumber(BigNumberCallReply {
            reply,
            _dummy: PhantomData,
        })),
        ReplyType::VerbatimString => Ok(CallReply::VerbatimString(VerbatimStringCallReply {
            reply,
            _dummy: PhantomData,
        })),
    }
}

fn fmt_call_result(res: CallResult<'_>, f: &mut Formatter<'_>) -> fmt::Result {
    match res {
        Ok(r) => fmt::Display::fmt(&r, f),
        Err(e) => fmt::Display::fmt(&e, f),
    }
}

pub type CallResult<'root> = Result<CallReply<'root>, ErrorReply<'root>>;

/// The handle [FutureCallReply::set_unblock_handler] returns. Release it with
/// [Self::dispose] or cancel the command with [Self::abort_and_dispose] while
/// the Redis lock is held. It stays on the thread that installed the handler.
pub struct FutureHandler<C: FnOnce(&EventContext, CallResult<'static>)> {
    reply: NonNull<RedisModuleCallReply>,
    _dummy: PhantomData<C>,
    reply_freed: bool,
}

impl<C> FutureHandler<C>
where
    C: FnOnce(&EventContext, CallResult<'static>),
{
    /// Dispose the future, handler. This function must be called in order to
    /// release the [FutureHandler]. The reason we must have a dispose function
    /// and we can not use the Drop is that [FutureHandler] must be released
    /// when the Redis GIL is held. This is also why this function also gets a
    /// lock indicator.
    ///
    /// Disposing releases this handle only: the unblock handler still runs
    /// when the command unblocks. Use [Self::abort_and_dispose] to cancel it.
    pub fn dispose<LockIndicator: RedisLockIndicator>(mut self, _lock_indicator: &LockIndicator) {
        free_call_reply(self.reply.as_ptr());
        self.reply_freed = true;
    }

    /// Aborts the invocation of the blocking commands. Return [Status::Ok] on
    /// success and [Status::Err] on failure. In case of success it is promised
    /// that the unblock handler will not be called, and it is dropped here.
    /// [Status::Err] means the command is no longer blocked: the handler has
    /// run, or Redis has unblocked the command and will run it shortly.
    /// The function also dispose the [FutureHandler].
    pub fn abort_and_dispose<LockIndicator: RedisLockIndicator>(
        self,
        lock_indicator: &LockIndicator,
    ) -> Status {
        let mut callback: *mut C = std::ptr::null_mut();
        let res = unsafe {
            RedisModule_CallReplyPromiseAbort
                .expect("RedisModule_CallReplyPromiseAbort is expected to be available if we got a promise call reply")
                (self.reply.as_ptr(), &mut callback as *mut *mut C as *mut *mut c_void)
        }.into();

        // Release the reply first, so a panicking capture cannot leak it.
        self.dispose(lock_indicator);

        if !callback.is_null() {
            // SAFETY: a successful abort returns the pointer set_unblock_handler
            // made with Box::into_raw, and Redis will not invoke it. Box::from_raw
            // also releases zero-sized handlers, whose pointer is dangling.
            drop(unsafe { Box::from_raw(callback) });
        }

        res
    }
}

impl<C: FnOnce(&EventContext, CallResult<'static>)> Drop for FutureHandler<C> {
    fn drop(&mut self) {
        if !self.reply_freed {
            log::warn!("Memory leak detected!!! FutureHandler was freed without disposed.")
        }
    }
}

/// A reply from `EventContext::call_blocking`, available at the Redis 7.2 and
/// later compatibility floors.
///
/// Install an unblock handler before releasing the Redis lock. This reply
/// cannot outlive its borrowed context.
pub struct FutureCallReply<'ctx> {
    _ctx: &'ctx RuntimeContext,
    reply: Option<NonNull<RedisModuleCallReply>>,
}

extern "C" fn on_unblock<C: FnOnce(&EventContext, CallResult<'static>)>(
    ctx: *mut RedisModuleCtx,
    reply: *mut RedisModuleCallReply,
    private_data: *mut ::std::os::raw::c_void,
) {
    let on_unblock = unsafe { Box::from_raw(private_data as *mut C) };
    let ctx = unsafe { EventContext::new(ctx) };
    let reply = NonNull::new(reply).map_or(Ok(CallReply::Unknown), create_call_reply);
    crate::panic::run_callback(
        "blocking call unblock handler",
        || on_unblock(&ctx, reply),
        || (),
    );
}

impl<'ctx> FutureCallReply<'ctx> {
    /// Install a handler called when the command unblocks. Returns a handle
    /// that can be used to abort the command.
    ///
    /// Redis keeps the handler until the command unblocks or is aborted, after
    /// the current invocation returns and whether or not the returned handle is
    /// disposed, dropped or forgotten. It runs on the main thread, also when a
    /// module-owned worker installed it. The handler therefore owns its
    /// captures and must be `Send`:
    ///
    /// ```no_run
    /// use redis_module::{FutureCallReply, RuntimeContext};
    ///
    /// fn install(ctx: &RuntimeContext, future: FutureCallReply<'_>) {
    ///     let owned = String::from("moved into the handler");
    ///     future.set_unblock_handler(move |_, _| drop(owned)).dispose(ctx);
    /// }
    ///
    /// fn install_and_abort(ctx: &RuntimeContext, future: FutureCallReply<'_>) {
    ///     let owned = String::from("dropped by a successful abort");
    ///     future.set_unblock_handler(move |_, _| drop(owned)).abort_and_dispose(ctx);
    /// }
    ///
    /// // Only illustrates the bound: forgetting leaks the reply's reference.
    /// fn install_and_forget(future: FutureCallReply<'_>) {
    ///     let owned = String::from("kept by Redis");
    ///     std::mem::forget(future.set_unblock_handler(move |_, _| drop(owned)));
    /// }
    /// ```
    ///
    /// The handler runs with the Redis lock held, so it must not lock a
    /// thread-safe context or the module context itself.
    ///
    /// A borrowed capture is rejected, whether the handle is disposed, aborted
    /// or forgotten:
    ///
    /// ```compile_fail,E0597
    /// fn install(ctx: &redis_module::RuntimeContext, future: redis_module::FutureCallReply<'_>) {
    ///     let local = String::from("scoped");
    ///     let borrowed = &local;
    ///     future.set_unblock_handler(move |_, _| drop(borrowed.len())).dispose(ctx);
    /// }
    /// ```
    ///
    /// ```compile_fail,E0597
    /// fn install(ctx: &redis_module::RuntimeContext, future: redis_module::FutureCallReply<'_>) {
    ///     let local = String::from("scoped");
    ///     let borrowed = &local;
    ///     future.set_unblock_handler(move |_, _| drop(borrowed.len())).abort_and_dispose(ctx);
    /// }
    /// ```
    ///
    /// ```compile_fail,E0597
    /// fn install(future: redis_module::FutureCallReply<'_>) {
    ///     let local = String::from("scoped");
    ///     let borrowed = &local;
    ///     std::mem::forget(future.set_unblock_handler(move |_, _| drop(borrowed.len())));
    /// }
    /// ```
    ///
    /// So is a capture that cannot move to the main thread:
    ///
    /// ```compile_fail,E0277
    /// fn install(ctx: &redis_module::RuntimeContext, future: redis_module::FutureCallReply<'_>) {
    ///     let shared = std::rc::Rc::new(String::from("thread-bound"));
    ///     future.set_unblock_handler(move |_, _| drop(shared)).dispose(ctx);
    /// }
    /// ```
    pub fn set_unblock_handler<C: FnOnce(&EventContext, CallResult<'static>) + Send + 'static>(
        mut self,
        unblock_handler: C,
    ) -> FutureHandler<C> {
        let reply = self.reply.take().expect("Got a NULL future reply");
        unsafe {
            RedisModule_CallReplyPromiseSetUnblockHandler
                .expect("RedisModule_CallReplyPromiseSetUnblockHandler is expected to be available if we got a promise call reply")
                (reply.as_ptr(), Some(on_unblock::<C>), Box::into_raw(Box::new(unblock_handler)) as *mut c_void)
        }
        FutureHandler {
            reply,
            _dummy: PhantomData,
            reply_freed: false,
        }
    }
}

impl<'ctx> Drop for FutureCallReply<'ctx> {
    fn drop(&mut self) {
        if let Some(v) = self.reply {
            free_call_reply(v.as_ptr());
        }
    }
}

pub enum PromiseCallReply<'root, 'ctx> {
    Resolved(CallResult<'root>),
    Future(FutureCallReply<'ctx>),
}

pub(crate) fn create_promise_call_reply(
    ctx: &RuntimeContext,
    reply: Option<NonNull<RedisModuleCallReply>>,
) -> PromiseCallReply<'static, '_> {
    reply.map_or(PromiseCallReply::Resolved(Ok(CallReply::Unknown)), |val| {
        let ty = unsafe { crate::__required_api!(RedisModule_CallReplyType)(val.as_ptr()) };
        if ty == REDISMODULE_REPLY_PROMISE as i32 {
            return PromiseCallReply::Future(FutureCallReply {
                _ctx: ctx,
                reply: Some(val),
            });
        }
        PromiseCallReply::Resolved(create_call_reply(val))
    })
}

impl<'ctx> From<PromiseCallReply<'static, 'ctx>> for CallResult<'static> {
    fn from(value: PromiseCallReply<'static, 'ctx>) -> Self {
        match value {
            PromiseCallReply::Resolved(c) => c,
            PromiseCallReply::Future(_) => panic!("Got unexpected future call reply"),
        }
    }
}

#[cfg(test)]
mod error_code_tests {
    use super::{ErrorReply, error_code};

    #[test]
    fn message_codes_handle_bare_empty_and_nonstandard_payloads() {
        for (message, expected) in [
            ("OOM command not allowed", &b"OOM"[..]),
            ("BUSYGROUP group exists", &b"BUSYGROUP"[..]),
            ("ERR", &b"ERR"[..]),
            ("", &b""[..]),
            (" ERR message", &b""[..]),
            ("CUSTOM  multiple spaces", &b"CUSTOM"[..]),
            ("lowercase message", &b"lowercase"[..]),
            ("CUSTOM\tdetails", &b"CUSTOM\tdetails"[..]),
        ] {
            let reply = ErrorReply::Message(message.into());
            assert_eq!(reply.code(), expected, "{message:?}");
        }
    }

    #[test]
    fn codes_preserve_bytes_without_decoding_the_message() {
        assert_eq!(error_code(b"CUSTOM non-UTF-8 \xff"), b"CUSTOM");
        assert_eq!(error_code(b"\xffCODE details"), b"\xffCODE");
        assert_eq!(error_code(b"\xff"), b"\xff");
    }
}
