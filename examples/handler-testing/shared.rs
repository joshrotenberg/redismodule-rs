//! Private prototype boundary shared with the native call_reply example.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    Simple(String),
    Bulk(Vec<u8>),
    Integer(i64),
    Array(Vec<Reply>),
    Null,
    NullArray,
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HandlerError {
    WrongArity,
    Message(String),
}

pub(crate) type HandlerResult = Result<Reply, HandlerError>;

pub(crate) trait CommandIo {
    fn call(&mut self, command: &str, args: &[&[u8]]) -> HandlerResult;
}

/// argv includes the command name, as in a Redis command callback.
pub(crate) fn echo_handler(io: &mut impl CommandIo, argv: &[&[u8]]) -> HandlerResult {
    io.call("ECHO", &argv[1..])
}
