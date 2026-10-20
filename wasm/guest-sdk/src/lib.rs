//! Helpers for writing a library for `redis-module-wasm` in Rust.
//!
//! The host's interface passes a reply as a list of nodes in pre-order (WIT
//! has no recursive types). This crate turns those lists into [`Value`]s and
//! back, and wraps the host's imports to take and return them:
//!
//! ```ignore
//! use redis_wasm_guest::{Value, pcall};
//!
//! let first = pcall("INCR", &[key]);
//! let second = pcall("INCR", &[key]);
//! Value::Array(vec![first, second]).into_nodes()
//! ```
//!
//! A library's own bindings use this crate's types through wit-bindgen's
//! `with` option:
//!
//! ```ignore
//! wit_bindgen::generate!({
//!     path: "wit",
//!     world: "library",
//!     with: { "redis-module-wasm:host/redis@0.1.0": redis_wasm_guest::redis },
//! });
//! ```

wit_bindgen::generate!({
    path: "../wit",
    world: "guest-imports",
});

pub use redis::{FlatNode, FlatReply, LogLevel, Node, Span};
/// The host's interface, as generated bindings.
pub use redis_module_wasm::host::redis;

/// A reply, or a value to reply with.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Nil,
    Integer(i64),
    Double(f64),
    Boolean(bool),
    /// A simple string, such as `OK`.
    Simple(String),
    /// A bulk string.
    Bulk(Vec<u8>),
    /// An error, such as `ERR something failed`.
    Error(String),
    Array(Vec<Value>),
    Set(Vec<Value>),
    Map(Vec<(Value, Value)>),
}

impl Value {
    /// The value as nodes in pre-order, to return from `invoke`.
    pub fn into_nodes(self) -> Vec<Node> {
        let mut nodes = Vec::new();
        self.push_nodes(&mut nodes);
        nodes
    }

    /// Appends the value's nodes, in pre-order, to `nodes`.
    pub fn push_nodes(self, nodes: &mut Vec<Node>) {
        match self {
            Value::Nil => nodes.push(Node::Nil),
            Value::Integer(n) => nodes.push(Node::Integer(n)),
            Value::Double(d) => nodes.push(Node::Double(d)),
            Value::Boolean(b) => nodes.push(Node::Boolean(b)),
            Value::Simple(text) => nodes.push(Node::Simple(text)),
            Value::Bulk(bytes) => nodes.push(Node::Bulk(bytes)),
            Value::Error(text) => nodes.push(Node::Error(text)),
            Value::Array(items) => {
                nodes.push(Node::Array(count(items.len())));
                items.into_iter().for_each(|item| item.push_nodes(nodes));
            }
            Value::Set(items) => {
                nodes.push(Node::Set(count(items.len())));
                items.into_iter().for_each(|item| item.push_nodes(nodes));
            }
            Value::Map(entries) => {
                nodes.push(Node::Map(count(entries.len())));
                for (key, value) in entries {
                    key.push_nodes(nodes);
                    value.push_nodes(nodes);
                }
            }
        }
    }

    /// The value that `nodes` holds, or `None` if they are not exactly one
    /// value in pre-order.
    pub fn from_nodes(nodes: &[Node]) -> Option<Value> {
        let mut rest = nodes.iter();
        let value = Self::take(&mut rest, &|node: &Node| node.clone().into())?;
        rest.next().is_none().then_some(value)
    }

    /// The value that a flat reply holds, or `None` if it is malformed.
    pub fn from_flat(reply: &FlatReply) -> Option<Value> {
        let bytes = |span: &Span| {
            let start = usize::try_from(span.offset).ok()?;
            let len = usize::try_from(span.len).ok()?;
            reply.data.get(start..start.checked_add(len)?)
        };
        let text = |span: &Span| bytes(span).map(|b| String::from_utf8_lossy(b).into_owned());
        let mut nodes = Vec::with_capacity(reply.nodes.len());
        for node in &reply.nodes {
            nodes.push(match node {
                FlatNode::Nil => Node::Nil,
                FlatNode::Integer(n) => Node::Integer(*n),
                FlatNode::Double(d) => Node::Double(*d),
                FlatNode::Boolean(b) => Node::Boolean(*b),
                FlatNode::Simple(span) => Node::Simple(text(span)?),
                FlatNode::Bulk(span) => Node::Bulk(bytes(span)?.to_vec()),
                FlatNode::Error(span) => Node::Error(text(span)?),
                FlatNode::Array(n) => Node::Array(*n),
                FlatNode::Set(n) => Node::Set(*n),
                FlatNode::Map(n) => Node::Map(*n),
            });
        }
        Self::from_nodes(&nodes)
    }

    /// Takes one value from the front of `nodes`.
    fn take<'a>(
        nodes: &mut impl Iterator<Item = &'a Node>,
        scalar: &dyn Fn(&Node) -> Value,
    ) -> Option<Value> {
        let node = nodes.next()?;
        let mut items =
            |n: u32| -> Option<Vec<Value>> { (0..n).map(|_| Self::take(nodes, scalar)).collect() };
        Some(match node {
            Node::Array(n) => Value::Array(items(*n)?),
            Node::Set(n) => Value::Set(items(*n)?),
            Node::Map(n) => {
                let mut entries = Vec::new();
                for _ in 0..*n {
                    let key = Self::take(nodes, scalar)?;
                    let value = Self::take(nodes, scalar)?;
                    entries.push((key, value));
                }
                Value::Map(entries)
            }
            scalar_node => scalar(scalar_node),
        })
    }

    /// Whether the value is an error.
    pub fn is_error(&self) -> bool {
        matches!(self, Value::Error(_))
    }

    /// The value as an integer, if it is one.
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// The value's bytes, if it is a bulk or simple string.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bulk(bytes) => Some(bytes),
            Value::Simple(text) => Some(text.as_bytes()),
            _ => None,
        }
    }
}

/// A scalar node as a value; an aggregate node, which `take` handles, as nil.
impl From<Node> for Value {
    fn from(node: Node) -> Self {
        match node {
            Node::Nil | Node::Array(_) | Node::Set(_) | Node::Map(_) => Value::Nil,
            Node::Integer(n) => Value::Integer(n),
            Node::Double(d) => Value::Double(d),
            Node::Boolean(b) => Value::Boolean(b),
            Node::Simple(text) => Value::Simple(text),
            Node::Bulk(bytes) => Value::Bulk(bytes),
            Node::Error(text) => Value::Error(text),
        }
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Integer(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Boolean(b)
    }
}

impl From<f64> for Value {
    fn from(d: f64) -> Self {
        Value::Double(d)
    }
}

/// A string as a bulk string, as Redis replies with stored values.
impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Value::Bulk(text.as_bytes().to_vec())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Value::Bulk(text.into_bytes())
    }
}

impl From<Vec<u8>> for Value {
    fn from(bytes: Vec<u8>) -> Self {
        Value::Bulk(bytes)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(value: Option<T>) -> Self {
        value.map_or(Value::Nil, Into::into)
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(items: Vec<T>) -> Self {
        Value::Array(items.into_iter().map(Into::into).collect())
    }
}

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn owned_args(args: &[impl AsRef<[u8]>]) -> Vec<Vec<u8>> {
    args.iter().map(|arg| arg.as_ref().to_vec()).collect()
}

/// Runs a Redis command, like `redis.call`: an error reply fails the
/// function, which replies with that error.
pub fn call(command: &str, args: &[impl AsRef<[u8]>]) -> Value {
    let nodes = redis::call(command, &owned_args(args));
    Value::from_nodes(&nodes).unwrap_or(Value::Nil)
}

/// Runs a Redis command, like `redis.pcall`: an error reply comes back as a
/// [`Value::Error`].
pub fn pcall(command: &str, args: &[impl AsRef<[u8]>]) -> Value {
    let nodes = redis::pcall(command, &owned_args(args));
    Value::from_nodes(&nodes).unwrap_or(Value::Nil)
}

/// Writes `message` to the server log.
pub fn log(level: LogLevel, message: &str) {
    redis::log(level, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        Value::Array(vec![
            Value::Map(vec![(Value::Simple("answer".into()), Value::Integer(42))]),
            Value::Nil,
            Value::Set(vec![Value::from("a"), Value::Boolean(true)]),
            Value::Error("ERR inside".into()),
        ])
    }

    #[test]
    fn values_round_trip_through_nodes() {
        let nodes = sample().into_nodes();
        assert!(matches!(nodes[0], Node::Array(4)));
        assert!(matches!(nodes[1], Node::Map(1)));
        assert_eq!(Value::from_nodes(&nodes), Some(sample()));
    }

    #[test]
    fn malformed_node_lists_are_none() {
        assert_eq!(Value::from_nodes(&[]), None);
        assert_eq!(Value::from_nodes(&[Node::Array(2), Node::Nil]), None);
        assert_eq!(Value::from_nodes(&[Node::Nil, Node::Nil]), None);
    }

    #[test]
    fn a_flat_reply_reads_as_the_same_value() {
        let reply = FlatReply {
            nodes: vec![
                FlatNode::Array(2),
                FlatNode::Bulk(Span { offset: 0, len: 2 }),
                FlatNode::Error(Span { offset: 2, len: 3 }),
            ],
            data: b"abERR".to_vec(),
        };
        assert_eq!(
            Value::from_flat(&reply),
            Some(Value::Array(vec![
                Value::from("ab"),
                Value::Error("ERR".into())
            ]))
        );
        let bad = FlatReply {
            nodes: vec![FlatNode::Bulk(Span { offset: 4, len: 9 })],
            data: b"ab".to_vec(),
        };
        assert_eq!(Value::from_flat(&bad), None);
    }

    #[test]
    fn conversions_pick_the_reply_types_redis_uses() {
        assert_eq!(Value::from(Some(3_i64)), Value::Integer(3));
        assert_eq!(Value::from(None::<i64>), Value::Nil);
        assert_eq!(
            Value::from(vec!["x"]),
            Value::Array(vec![Value::Bulk(b"x".to_vec())])
        );
    }
}
