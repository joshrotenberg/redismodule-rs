use crate::{
    context::call_reply::{CallResult, VerbatimStringFormat},
    CallReply, RedisError, RedisString,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    hash::Hash,
};

#[derive(Debug, PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]
pub enum RedisValueKey {
    Integer(i64),
    String(String),
    BulkRedisString(RedisString),
    BulkString(Vec<u8>),
    Bool(bool),
}

#[derive(Debug, PartialEq, Clone)]
pub enum RedisValue {
    SimpleStringStatic(&'static str),
    SimpleString(String),
    BulkString(String),
    BulkRedisString(RedisString),
    StringBuffer(Vec<u8>),
    Integer(i64),
    Bool(bool),
    Float(f64),
    BigNumber(String),
    VerbatimString((VerbatimStringFormat, Vec<u8>)),
    Array(Vec<RedisValue>),
    StaticError(&'static str),
    Map(HashMap<RedisValueKey, RedisValue>),
    Set(HashSet<RedisValueKey>),
    OrderedMap(BTreeMap<RedisValueKey, RedisValue>),
    OrderedSet(BTreeSet<RedisValueKey>),
    /// A map replied in the order of its entries: a RESP3 map, or for RESP2 a
    /// flat array of keys and values. `Map` replies in hash order and
    /// `OrderedMap` sorted by key.
    /// Duplicate keys are sent as they are.
    InsertionOrderedMap(Vec<(RedisValueKey, RedisValue)>),
    Null,
    NoReply, // No reply at all (as opposed to a Null reply)
}

impl TryFrom<RedisValue> for String {
    type Error = RedisError;
    fn try_from(val: RedisValue) -> Result<Self, RedisError> {
        match val {
            RedisValue::SimpleStringStatic(s) => Ok(s.to_string()),
            RedisValue::SimpleString(s) => Ok(s),
            RedisValue::BulkString(s) => Ok(s),
            RedisValue::BulkRedisString(s) => Ok(s.try_as_str()?.to_string()),
            RedisValue::StringBuffer(s) => Ok(std::str::from_utf8(&s)?.to_string()),
            _ => Err(RedisError::Str("Can not convert result to String")),
        }
    }
}

impl From<String> for RedisValueKey {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<i64> for RedisValueKey {
    fn from(i: i64) -> Self {
        Self::Integer(i)
    }
}

impl From<RedisString> for RedisValueKey {
    fn from(rs: RedisString) -> Self {
        Self::BulkRedisString(rs)
    }
}

impl From<Vec<u8>> for RedisValueKey {
    fn from(s: Vec<u8>) -> Self {
        Self::BulkString(s)
    }
}

impl From<&str> for RedisValueKey {
    fn from(s: &str) -> Self {
        s.to_owned().into()
    }
}

impl From<&String> for RedisValueKey {
    fn from(s: &String) -> Self {
        s.clone().into()
    }
}

impl From<bool> for RedisValueKey {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<()> for RedisValue {
    fn from(_: ()) -> Self {
        Self::Null
    }
}

impl From<i64> for RedisValue {
    fn from(i: i64) -> Self {
        Self::Integer(i)
    }
}

impl From<bool> for RedisValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<usize> for RedisValue {
    fn from(i: usize) -> Self {
        (i as i64).into()
    }
}

impl From<f64> for RedisValue {
    fn from(f: f64) -> Self {
        Self::Float(f)
    }
}

impl From<String> for RedisValue {
    fn from(s: String) -> Self {
        Self::BulkString(s)
    }
}

impl From<RedisString> for RedisValue {
    fn from(s: RedisString) -> Self {
        Self::BulkRedisString(s)
    }
}

impl From<Vec<u8>> for RedisValue {
    fn from(s: Vec<u8>) -> Self {
        Self::StringBuffer(s)
    }
}

impl From<&RedisString> for RedisValue {
    fn from(s: &RedisString) -> Self {
        s.clone().into()
    }
}

impl From<&str> for RedisValue {
    fn from(s: &str) -> Self {
        s.to_owned().into()
    }
}

impl From<&String> for RedisValue {
    fn from(s: &String) -> Self {
        s.clone().into()
    }
}

impl<T: Into<Self>> From<Option<T>> for RedisValue {
    fn from(s: Option<T>) -> Self {
        s.map_or(Self::Null, Into::into)
    }
}

impl<T: Into<Self>> From<Vec<T>> for RedisValue {
    fn from(items: Vec<T>) -> Self {
        Self::Array(items.into_iter().map(Into::into).collect())
    }
}

impl<K: Into<RedisValueKey>, V: Into<RedisValue>> From<HashMap<K, V>> for RedisValue {
    fn from(items: HashMap<K, V>) -> Self {
        Self::Map(
            items
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl<K: Into<RedisValueKey>, V: Into<RedisValue>> From<BTreeMap<K, V>> for RedisValue {
    fn from(items: BTreeMap<K, V>) -> Self {
        Self::OrderedMap(
            items
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl<K: Into<RedisValueKey>> From<HashSet<K>> for RedisValue {
    fn from(items: HashSet<K>) -> Self {
        Self::Set(items.into_iter().map(Into::into).collect())
    }
}

impl<K: Into<RedisValueKey>> From<BTreeSet<K>> for RedisValue {
    fn from(items: BTreeSet<K>) -> Self {
        Self::OrderedSet(items.into_iter().map(Into::into).collect())
    }
}

// Copy native reply bytes once before the reply owner goes away. UTF-8 failure
// returns that same buffer rather than discarding it and copying a second time.
fn call_string_value(bytes: Vec<u8>) -> RedisValue {
    match String::from_utf8(bytes) {
        Ok(text) => RedisValue::BulkString(text),
        Err(error) => RedisValue::StringBuffer(error.into_bytes()),
    }
}

fn call_string_key(bytes: Vec<u8>) -> RedisValueKey {
    match String::from_utf8(bytes) {
        Ok(text) => RedisValueKey::String(text),
        Err(error) => RedisValueKey::BulkString(error.into_bytes()),
    }
}

impl<'root> TryFrom<&CallReply<'root>> for RedisValueKey {
    type Error = RedisError;
    fn try_from(reply: &CallReply<'root>) -> Result<Self, Self::Error> {
        match reply {
            CallReply::I64(reply) => Ok(RedisValueKey::Integer(reply.to_i64())),
            CallReply::String(reply) => Ok(call_string_key(reply.as_bytes().to_vec())),
            CallReply::Bool(b) => Ok(RedisValueKey::Bool(b.to_bool())),
            _ => Err(RedisError::String(format!(
                "Given CallReply can not be used as a map key or a set element, {:?}",
                reply
            ))),
        }
    }
}

impl<'root> From<&CallReply<'root>> for RedisValue {
    fn from(reply: &CallReply<'root>) -> Self {
        match reply {
            CallReply::Unknown => RedisValue::StaticError("Error on method call"),
            CallReply::Array(reply) => {
                RedisValue::Array(reply.iter().map(|v| (&v).into()).collect())
            }
            CallReply::I64(reply) => RedisValue::Integer(reply.to_i64()),
            CallReply::String(reply) if reply.is_simple_string() => {
                match String::from_utf8(reply.as_bytes().to_vec()) {
                    Ok(text) => RedisValue::SimpleString(text),
                    Err(error) => RedisValue::StringBuffer(error.into_bytes()),
                }
            }
            CallReply::String(reply) => call_string_value(reply.as_bytes().to_vec()),
            CallReply::Null(_) => RedisValue::Null,
            CallReply::Map(reply) => RedisValue::Map(
                reply
                    .iter()
                    .map(|(key, val)| {
                        (
                            (&key).try_into().unwrap_or_else(|e| {
                                panic!("Got unhashable map key from Redis, {key:?}, {e}")
                            }),
                            (&val).into(),
                        )
                    })
                    .collect(),
            ),
            CallReply::Set(reply) => RedisValue::Set(
                reply
                    .iter()
                    .map(|v| {
                        (&v).try_into().unwrap_or_else(|e| {
                            panic!("Got unhashable set element from Redis, {v:?}, {e}")
                        })
                    })
                    .collect(),
            ),
            CallReply::Bool(reply) => RedisValue::Bool(reply.to_bool()),
            CallReply::Double(reply) => RedisValue::Float(reply.to_double()),
            CallReply::BigNumber(reply) => reply
                .to_string()
                .map(RedisValue::BigNumber)
                .unwrap_or_else(|| {
                    // BigNumber should always be valid UTF-8, but if not, treat as raw bytes
                    RedisValue::StaticError("Invalid BigNumber: not valid UTF-8")
                }),
            CallReply::VerbatimString(reply) => reply
                .to_parts()
                .map(RedisValue::VerbatimString)
                .unwrap_or_else(|| {
                    // VerbatimString format should always be valid UTF-8, but if not, treat as error
                    RedisValue::StaticError("Invalid VerbatimString: format not valid UTF-8")
                }),
        }
    }
}

impl<'root> From<&CallResult<'root>> for RedisValue {
    fn from(reply: &CallResult<'root>) -> Self {
        reply.as_ref().map_or_else(
            |e| {
                // [RedisValue] does not support error, we can change that but to avoid
                // drastic changes and try to keep backword compatability, currently
                // we will stansform the error into a String buffer.
                RedisValue::StringBuffer(e.as_bytes().to_vec())
            },
            |v| (v).into(),
        )
    }
}

impl<'root> TryFrom<&CallResult<'root>> for RedisValueKey {
    type Error = RedisError;
    fn try_from(reply: &CallResult<'root>) -> Result<Self, Self::Error> {
        reply.as_ref().map_or_else(
            |e| {
                Err(RedisError::String(
                    format!("Got an error reply which can not be translated into a map key or set element, {:?}", e),
                ))
            },
            |v| v.try_into(),
        )
    }
}

//////////////////////////////////////////////////////////

#[cfg(test)]
mod tests {
    use super::{call_string_key, call_string_value, RedisValue, RedisValueKey};
    use std::collections::{BTreeSet, HashSet};

    #[test]
    fn call_strings_preserve_bytes_and_strict_text_policy() {
        let inputs: &[&[u8]] = &[
            b"",
            b"ASCII",
            "héllo".as_bytes(),
            b"a\0b",
            b"\r",
            b"\n",
            b"\r\n",
            &[0xff, 0, 0xfe],
            &[b'a', 0xf0, 0x9f, 0x92],
        ];
        for bytes in inputs {
            let value = call_string_value(bytes.to_vec());
            let key = call_string_key(bytes.to_vec());
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    assert_eq!(value, RedisValue::BulkString(text.to_owned()));
                    assert_eq!(key, RedisValueKey::String(text.to_owned()));
                    assert_eq!(String::try_from(value).unwrap(), text);
                }
                Err(_) => {
                    assert_eq!(value, RedisValue::StringBuffer(bytes.to_vec()));
                    assert_eq!(key, RedisValueKey::BulkString(bytes.to_vec()));
                    assert!(String::try_from(value).is_err());
                }
            }
        }
    }

    #[test]
    fn call_string_classification_keeps_the_owned_allocation() {
        for valid in [true, false] {
            for key in [true, false] {
                let mut bytes = Vec::with_capacity(4113);
                bytes.resize(4096, b'x');
                if !valid {
                    bytes[2048] = 0xff;
                }
                let expected = bytes.clone();
                let pointer = bytes.as_ptr();
                let capacity = bytes.capacity();
                if key {
                    match call_string_key(bytes) {
                        RedisValueKey::String(text) => {
                            assert!(valid);
                            assert_eq!(text.as_ptr(), pointer);
                            assert_eq!(text.capacity(), capacity);
                            assert_eq!(text.len(), 4096);
                            assert_eq!(text.as_bytes(), expected);
                        }
                        RedisValueKey::BulkString(bytes) => {
                            assert!(!valid);
                            assert_eq!(bytes.as_ptr(), pointer);
                            assert_eq!(bytes.capacity(), capacity);
                            assert_eq!(bytes[2048], 0xff);
                            assert_eq!(bytes.as_slice(), expected);
                        }
                        _ => panic!("unexpected key variant"),
                    }
                } else {
                    match call_string_value(bytes) {
                        RedisValue::BulkString(text) => {
                            assert!(valid);
                            assert_eq!(text.as_ptr(), pointer);
                            assert_eq!(text.capacity(), capacity);
                            assert_eq!(text.len(), 4096);
                            assert_eq!(text.as_bytes(), expected);
                        }
                        RedisValue::StringBuffer(bytes) => {
                            assert!(!valid);
                            assert_eq!(bytes.as_ptr(), pointer);
                            assert_eq!(bytes.capacity(), capacity);
                            assert_eq!(bytes[2048], 0xff);
                            assert_eq!(bytes.as_slice(), expected);
                        }
                        _ => panic!("unexpected value variant"),
                    }
                }
            }
        }
    }

    #[test]
    fn call_string_keys_keep_text_identity_hashing_and_ordering() {
        let keys: Vec<_> = ["a", "z", "a\0b", "héllo"]
            .into_iter()
            .map(|s| call_string_key(s.as_bytes().to_vec()))
            .collect();
        let hashed: HashSet<_> = keys.iter().cloned().collect();
        let ordered: BTreeSet<_> = keys.iter().cloned().collect();
        for text in ["a", "z", "a\0b", "héllo"] {
            let key = RedisValueKey::String(text.to_owned());
            assert!(hashed.contains(&key));
            assert!(ordered.contains(&key));
            assert!(!hashed.contains(&RedisValueKey::BulkString(text.as_bytes().to_vec())));
        }
        let expected: BTreeSet<_> = ["a", "z", "a\0b", "héllo"]
            .into_iter()
            .map(|s| RedisValueKey::String(s.to_owned()))
            .collect();
        assert_eq!(ordered, expected);
    }

    #[test]
    fn from_vec_string() {
        assert_eq!(
            RedisValue::from(vec!["foo".to_string()]),
            RedisValue::Array(vec![RedisValue::BulkString("foo".to_owned())])
        );
    }

    #[test]
    fn from_vec_str() {
        assert_eq!(
            RedisValue::from(vec!["foo"]),
            RedisValue::Array(vec![RedisValue::BulkString("foo".to_owned())])
        );
    }

    #[test]
    fn from_vec_string_ref() {
        assert_eq!(
            RedisValue::from(vec![&"foo".to_string()]),
            RedisValue::Array(vec![RedisValue::BulkString("foo".to_owned())])
        );
    }

    #[test]
    fn from_option_str() {
        assert_eq!(
            RedisValue::from(Some("foo")),
            RedisValue::BulkString("foo".to_owned())
        );
    }

    #[test]
    fn from_vec() {
        let v: Vec<u8> = vec![0, 3, 5, 21, 255];
        assert_eq!(
            RedisValue::from(v),
            RedisValue::StringBuffer(vec![0, 3, 5, 21, 255])
        );
    }

    #[test]
    fn from_option_none() {
        assert_eq!(RedisValue::from(None::<()>), RedisValue::Null,);
    }
}
