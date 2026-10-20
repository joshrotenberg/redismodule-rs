//! Conversions between Redis replies and the `node` lists of the WIT interface.
//!
//! WIT has no recursive types, so a reply crosses the boundary as a list of
//! nodes in pre-order: an aggregate node carries its element count and its
//! elements follow it.

use redis_module::redisvalue::RedisValueKey;
use redis_module::{CallReply, CallResult, RedisValue};

use crate::host::Node;

/// Appends `result` to `nodes` in pre-order.
pub(crate) fn push_call_result(nodes: &mut Vec<Node>, result: &CallResult<'_>) {
    match result {
        Ok(reply) => push_call_reply(nodes, reply),
        Err(error) => nodes.push(Node::Error(
            String::from_utf8_lossy(error.as_bytes()).into_owned(),
        )),
    }
}

fn push_call_reply(nodes: &mut Vec<Node>, reply: &CallReply<'_>) {
    match reply {
        CallReply::I64(value) => nodes.push(Node::Integer(value.to_i64())),
        CallReply::String(value) if value.is_simple_string() => nodes.push(Node::Simple(
            String::from_utf8_lossy(value.as_bytes()).into_owned(),
        )),
        CallReply::String(value) => nodes.push(Node::Bulk(value.as_bytes().to_vec())),
        CallReply::Null(_) => nodes.push(Node::Nil),
        CallReply::Bool(value) => nodes.push(Node::Boolean(value.to_bool())),
        CallReply::Double(value) => nodes.push(Node::Double(value.to_double())),
        // The interface has no big number or verbatim node; both arrive as
        // their text.
        CallReply::BigNumber(value) => nodes.push(Node::Bulk(
            value.to_string().unwrap_or_default().into_bytes(),
        )),
        CallReply::VerbatimString(value) => nodes.push(Node::Bulk(
            value
                .as_parts()
                .map(|(_, text)| text.to_vec())
                .unwrap_or_default(),
        )),
        CallReply::Array(array) => {
            nodes.push(Node::Array(count(array.len())));
            for element in array.iter() {
                push_call_result(nodes, &element);
            }
        }
        CallReply::Set(set) => {
            nodes.push(Node::Set(count(set.len())));
            for element in set.iter() {
                push_call_result(nodes, &element);
            }
        }
        CallReply::Map(map) => {
            nodes.push(Node::Map(count(map.len())));
            for (key, value) in map.iter() {
                push_call_result(nodes, &key);
                push_call_result(nodes, &value);
            }
        }
        CallReply::Unknown => nodes.push(Node::Nil),
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
