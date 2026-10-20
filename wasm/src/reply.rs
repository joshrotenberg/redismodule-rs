//! Conversions between Redis replies and the `node` lists and flat replies of
//! the WIT interface.
//!
//! WIT has no recursive types, so a reply crosses the boundary as a list of
//! nodes in pre-order: an aggregate node carries its element count and its
//! elements follow it.

use redis_module::redisvalue::RedisValueKey;
use redis_module::{CallReply, CallResult, RedisValue};

use crate::host::Node;
use crate::host::redis_module_wasm::host::redis::{FlatNode, FlatReply, Span};

/// Where a walk of a reply puts each node: typed nodes, or a flat reply.
trait Sink {
    fn scalar(&mut self, node: Scalar<'_>);
    /// An aggregate of `len` elements, which follow it: `Node::Array`,
    /// `Node::Set` or `Node::Map`, by `kind`.
    fn aggregate(&mut self, kind: Aggregate, len: u32);
}

enum Scalar<'a> {
    Nil,
    Integer(i64),
    Double(f64),
    Boolean(bool),
    Simple(&'a [u8]),
    Bulk(&'a [u8]),
    Error(&'a [u8]),
}

#[derive(Clone, Copy)]
enum Aggregate {
    Array,
    Set,
    Map,
}

impl Sink for Vec<Node> {
    fn scalar(&mut self, node: Scalar<'_>) {
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        self.push(match node {
            Scalar::Nil => Node::Nil,
            Scalar::Integer(value) => Node::Integer(value),
            Scalar::Double(value) => Node::Double(value),
            Scalar::Boolean(value) => Node::Boolean(value),
            Scalar::Simple(bytes) => Node::Simple(text(bytes)),
            Scalar::Bulk(bytes) => Node::Bulk(bytes.to_vec()),
            Scalar::Error(bytes) => Node::Error(text(bytes)),
        });
    }

    fn aggregate(&mut self, kind: Aggregate, len: u32) {
        self.push(match kind {
            Aggregate::Array => Node::Array(len),
            Aggregate::Set => Node::Set(len),
            Aggregate::Map => Node::Map(len),
        });
    }
}

impl FlatReply {
    fn span(&mut self, bytes: &[u8]) -> Span {
        let offset = count(self.data.len());
        self.data.extend_from_slice(bytes);
        Span {
            offset,
            len: count(bytes.len()),
        }
    }

    /// A reply that is only the error `message`.
    pub(crate) fn error(message: &str) -> Self {
        let mut reply = Self {
            nodes: Vec::with_capacity(1),
            data: Vec::new(),
        };
        reply.scalar(Scalar::Error(message.as_bytes()));
        reply
    }

    /// The message, if the reply is only an error.
    pub(crate) fn as_error(&self) -> Option<String> {
        match self.nodes.as_slice() {
            [FlatNode::Error(span)] => {
                let start = span.offset as usize;
                let bytes = self.data.get(start..start + span.len as usize)?;
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            _ => None,
        }
    }
}

impl Sink for FlatReply {
    fn scalar(&mut self, node: Scalar<'_>) {
        let node = match node {
            Scalar::Nil => FlatNode::Nil,
            Scalar::Integer(value) => FlatNode::Integer(value),
            Scalar::Double(value) => FlatNode::Double(value),
            Scalar::Boolean(value) => FlatNode::Boolean(value),
            Scalar::Simple(bytes) => FlatNode::Simple(self.span(bytes)),
            Scalar::Bulk(bytes) => FlatNode::Bulk(self.span(bytes)),
            Scalar::Error(bytes) => FlatNode::Error(self.span(bytes)),
        };
        self.nodes.push(node);
    }

    fn aggregate(&mut self, kind: Aggregate, len: u32) {
        self.nodes.push(match kind {
            Aggregate::Array => FlatNode::Array(len),
            Aggregate::Set => FlatNode::Set(len),
            Aggregate::Map => FlatNode::Map(len),
        });
    }
}

/// Appends `result` to `nodes` in pre-order.
pub(crate) fn push_call_result(nodes: &mut Vec<Node>, result: &CallResult<'_>) {
    walk_result(nodes, result);
}

/// `result` as a flat reply.
pub(crate) fn flat_call_result(result: &CallResult<'_>) -> FlatReply {
    let mut reply = FlatReply {
        nodes: Vec::new(),
        data: Vec::new(),
    };
    walk_result(&mut reply, result);
    reply
}

fn walk_result(sink: &mut impl Sink, result: &CallResult<'_>) {
    match result {
        Ok(reply) => walk_reply(sink, reply),
        Err(error) => sink.scalar(Scalar::Error(error.as_bytes())),
    }
}

fn walk_reply(sink: &mut impl Sink, reply: &CallReply<'_>) {
    match reply {
        CallReply::I64(value) => sink.scalar(Scalar::Integer(value.to_i64())),
        CallReply::String(value) if value.is_simple_string() => {
            sink.scalar(Scalar::Simple(value.as_bytes()));
        }
        CallReply::String(value) => sink.scalar(Scalar::Bulk(value.as_bytes())),
        CallReply::Null(_) => sink.scalar(Scalar::Nil),
        CallReply::Bool(value) => sink.scalar(Scalar::Boolean(value.to_bool())),
        CallReply::Double(value) => sink.scalar(Scalar::Double(value.to_double())),
        // The interface has no big number or verbatim node; both arrive as
        // their text.
        CallReply::BigNumber(value) => {
            let text = value.to_string().unwrap_or_default();
            sink.scalar(Scalar::Bulk(text.as_bytes()));
        }
        CallReply::VerbatimString(value) => {
            let text = value.as_parts().map(|(_, text)| text).unwrap_or_default();
            sink.scalar(Scalar::Bulk(text));
        }
        CallReply::Array(array) => {
            sink.aggregate(Aggregate::Array, count(array.len()));
            for element in array.iter() {
                walk_result(sink, &element);
            }
        }
        CallReply::Set(set) => {
            sink.aggregate(Aggregate::Set, count(set.len()));
            for element in set.iter() {
                walk_result(sink, &element);
            }
        }
        CallReply::Map(map) => {
            sink.aggregate(Aggregate::Map, count(map.len()));
            for (key, value) in map.iter() {
                walk_result(sink, &key);
                walk_result(sink, &value);
            }
        }
        CallReply::Unknown => sink.scalar(Scalar::Nil),
    }
}

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// The reply a guest returned, as a value to send to the client.
///
/// # Errors
///
/// Returns a message if the nodes are not exactly one value in pre-order, or
/// if a map key is not a string, integer or boolean.
pub(crate) fn to_value(nodes: &[Node]) -> Result<RedisValue, String> {
    let mut rest = nodes;
    let value = next_value(&mut rest)?;
    if rest.is_empty() {
        Ok(value)
    } else {
        Err(format!("{} nodes after the reply", rest.len()))
    }
}

fn next_value(rest: &mut &[Node]) -> Result<RedisValue, String> {
    let (node, tail) = rest
        .split_first()
        .ok_or("an aggregate has fewer elements than its count")?;
    *rest = tail;
    Ok(match node {
        Node::Nil => RedisValue::Null,
        Node::Integer(value) => RedisValue::Integer(*value),
        Node::Double(value) => RedisValue::Float(*value),
        Node::Boolean(value) => RedisValue::Bool(*value),
        Node::Simple(text) => RedisValue::SimpleString(text.clone()),
        Node::Bulk(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(text) => RedisValue::BulkString(text),
            Err(error) => RedisValue::StringBuffer(error.into_bytes()),
        },
        Node::Error(message) => RedisValue::Error(message.clone()),
        // A set is replied as an array: RedisValue's sets reorder their
        // elements and only hold keys.
        Node::Array(len) | Node::Set(len) => RedisValue::Array(
            (0..*len)
                .map(|_| next_value(rest))
                .collect::<Result<_, _>>()?,
        ),
        Node::Map(len) => RedisValue::InsertionOrderedMap(
            (0..*len)
                .map(|_| Ok((to_key(next_value(rest)?)?, next_value(rest)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}

/// A map key. `RedisValueKey` has no simple string, so a simple string key is
/// replied as a bulk string.
fn to_key(value: RedisValue) -> Result<RedisValueKey, String> {
    match value {
        RedisValue::SimpleString(text) | RedisValue::BulkString(text) => {
            Ok(RedisValueKey::String(text))
        }
        RedisValue::StringBuffer(bytes) => Ok(RedisValueKey::BulkString(bytes)),
        RedisValue::Integer(value) => Ok(RedisValueKey::Integer(value)),
        RedisValue::Bool(value) => Ok(RedisValueKey::Bool(value)),
        other => Err(format!(
            "a map key must be a string, integer or boolean, not {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_reply_keeps_strings_as_spans_of_one_buffer() {
        let mut reply = FlatReply {
            nodes: Vec::new(),
            data: Vec::new(),
        };
        reply.aggregate(Aggregate::Array, 2);
        reply.scalar(Scalar::Bulk(b"ab"));
        reply.scalar(Scalar::Simple(b"cde"));
        assert_eq!(reply.data, b"abcde");
        assert!(matches!(
            reply.nodes.as_slice(),
            [
                FlatNode::Array(2),
                FlatNode::Bulk(Span { offset: 0, len: 2 }),
                FlatNode::Simple(Span { offset: 2, len: 3 }),
            ]
        ));
        assert_eq!(reply.as_error(), None);
        assert_eq!(
            FlatReply::error("ERR no").as_error().as_deref(),
            Some("ERR no")
        );
    }

    #[test]
    fn nested_nodes_become_one_value() {
        let nodes = [
            Node::Array(3),
            Node::Integer(1),
            Node::Map(1),
            Node::Simple("k".into()),
            Node::Bulk(b"v".to_vec()),
            Node::Error("ERR inner".into()),
        ];
        assert_eq!(
            to_value(&nodes),
            Ok(RedisValue::Array(vec![
                RedisValue::Integer(1),
                RedisValue::InsertionOrderedMap(vec![(
                    RedisValueKey::String("k".into()),
                    RedisValue::BulkString("v".into()),
                )]),
                RedisValue::Error("ERR inner".into()),
            ]))
        );
    }

    #[test]
    fn malformed_node_lists_are_errors() {
        assert!(to_value(&[]).is_err());
        assert!(to_value(&[Node::Array(2), Node::Nil]).is_err());
        assert!(to_value(&[Node::Nil, Node::Nil]).is_err());
        assert!(to_value(&[Node::Map(1), Node::Array(0), Node::Nil]).is_err());
    }

    #[test]
    fn bulk_bytes_that_are_not_utf8_stay_bytes() {
        assert_eq!(
            to_value(&[Node::Bulk(vec![0xff, 0])]),
            Ok(RedisValue::StringBuffer(vec![0xff, 0]))
        );
    }
}
