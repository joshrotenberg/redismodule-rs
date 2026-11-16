use crate::cases::echo_cases;
use crate::shared::{CommandIo, HandlerError, HandlerResult, Reply, echo_handler};
use std::collections::VecDeque;

#[derive(Debug, PartialEq, Eq)]
enum HarnessFailure {
    EmptyArgv,
    UnexpectedCall,
    WrongCommand {
        expected: String,
        actual: String,
    },
    WrongArguments {
        expected: Vec<Vec<u8>>,
        actual: Vec<Vec<u8>>,
    },
    UnusedCalls(usize),
}

struct ExpectedCall {
    command: String,
    args: Vec<Vec<u8>>,
    result: HandlerResult,
}

#[derive(Default)]
struct ScriptedIo {
    calls: VecDeque<ExpectedCall>,
    failure: Option<HarnessFailure>,
}

impl CommandIo for ScriptedIo {
    fn call(&mut self, command: &str, args: &[&[u8]]) -> HandlerResult {
        let actual: Vec<_> = args.iter().map(|arg| arg.to_vec()).collect();
        let failure = match self.calls.pop_front() {
            None => HarnessFailure::UnexpectedCall,
            Some(call) if call.command != command => HarnessFailure::WrongCommand {
                expected: call.command,
                actual: command.to_owned(),
            },
            Some(call) if call.args != actual => HarnessFailure::WrongArguments {
                expected: call.args,
                actual,
            },
            Some(call) => return call.result,
        };
        // A handler can catch this ordinary error, but cannot clear failure.
        self.failure.get_or_insert(failure);
        Err(HandlerError::Message("ERR scripted call mismatch".into()))
    }
}

#[derive(Default)]
struct HandlerHarness {
    io: ScriptedIo,
}

impl HandlerHarness {
    fn expect_call(mut self, command: &str, args: &[&[u8]], result: HandlerResult) -> Self {
        self.io.calls.push_back(ExpectedCall {
            command: command.to_owned(),
            args: args.iter().map(|arg| arg.to_vec()).collect(),
            result,
        });
        self
    }

    // The outer Result is test validity. The inner Result is handler output.
    fn run(
        mut self,
        argv: &[&[u8]],
        handler: impl FnOnce(&mut ScriptedIo, &[&[u8]]) -> HandlerResult,
    ) -> Result<HandlerResult, HarnessFailure> {
        if argv.is_empty() {
            return Err(HarnessFailure::EmptyArgv);
        }
        let output = handler(&mut self.io, argv);
        if let Some(failure) = self.io.failure {
            return Err(failure);
        }
        if !self.io.calls.is_empty() {
            return Err(HarnessFailure::UnusedCalls(self.io.calls.len()));
        }
        Ok(output)
    }
}

#[test]
fn shared_echo_contract() {
    for case in echo_cases() {
        let argv: Vec<_> = case.argv.iter().map(Vec::as_slice).collect();
        let expected = case
            .expected
            .map(Reply::Bulk)
            .map_err(|error| HandlerError::Message(error.into()));
        let output = HandlerHarness::default()
            .expect_call("ECHO", &argv[1..], expected.clone())
            .run(&argv, echo_handler)
            .expect(case.name);
        assert_eq!(output, expected, "{}: {:?}", case.name, case.argv);
    }
}

#[test]
fn captures_owned_reply_kinds_without_normalizing_them() {
    let result = Ok(Reply::Array(vec![
        Reply::Simple("same".into()),
        Reply::Bulk(b"same".to_vec()),
        Reply::Bulk(vec![0, 255]),
        Reply::Integer(-4),
        Reply::Null,
        Reply::NullArray,
        Reply::Error("CUSTOM detail".into()),
    ]));
    let output = HandlerHarness::default()
        .expect_call("ECHO", &[b"ignored"], result.clone())
        .run(&[b"call_reply.echo", b"ignored"], echo_handler)
        .unwrap();
    assert_eq!(output, result);
}

#[test]
fn scripted_errors_are_handler_inputs() {
    for error in [
        HandlerError::WrongArity,
        HandlerError::Message("ERR denied".into()),
    ] {
        let output = HandlerHarness::default()
            .expect_call("ECHO", &[], Err(error))
            .run(&[b"handler"], |io, _| {
                assert!(io.call("ECHO", &[]).is_err());
                Ok(Reply::Simple("recovered".into()))
            })
            .unwrap();
        assert_eq!(output, Ok(Reply::Simple("recovered".into())));
    }
}

#[test]
fn calls_are_fifo() {
    let output = HandlerHarness::default()
        .expect_call("FIRST", &[b"a"], Ok(Reply::Integer(1)))
        .expect_call("SECOND", &[b"b"], Ok(Reply::Integer(2)))
        .run(&[b"handler"], |io, _| {
            let first = io.call("FIRST", &[b"a"])?;
            let second = io.call("SECOND", &[b"b"])?;
            Ok(Reply::Array(vec![first, second]))
        })
        .unwrap();
    assert_eq!(
        output,
        Ok(Reply::Array(vec![Reply::Integer(1), Reply::Integer(2)]))
    );
}

#[test]
fn rejects_empty_argv_before_entering_the_body() {
    assert_eq!(
        HandlerHarness::default().run(&[], |_, _| panic!("body entered")),
        Err(HarnessFailure::EmptyArgv)
    );
}

#[test]
fn unused_expectations_fail_even_when_handler_returns_an_error() {
    assert_eq!(
        HandlerHarness::default()
            .expect_call("ECHO", &[], Ok(Reply::Null))
            .run(&[b"handler"], |_, _| Err(HandlerError::WrongArity)),
        Err(HarnessFailure::UnusedCalls(1))
    );
}

#[test]
fn unexpected_calls_stay_failed_when_swallowed() {
    let output = HandlerHarness::default().run(&[b"handler"], |io, _| {
        let _ = io.call("UNSCRIPTED", &[]);
        Ok(Reply::Null)
    });
    assert_eq!(output, Err(HarnessFailure::UnexpectedCall));
}

#[test]
fn extra_call_after_a_valid_call_stays_failed() {
    let output = HandlerHarness::default()
        .expect_call("ECHO", &[], Ok(Reply::Null))
        .run(&[b"handler"], |io, _| {
            io.call("ECHO", &[])?;
            let _ = io.call("ECHO", &[]);
            Ok(Reply::Null)
        });
    assert_eq!(output, Err(HarnessFailure::UnexpectedCall));
}

#[test]
fn wrong_command_and_reordered_calls_stay_failed() {
    for actual in ["echo", "SECOND"] {
        let output = HandlerHarness::default()
            .expect_call("ECHO", &[], Ok(Reply::Null))
            .expect_call("SECOND", &[], Ok(Reply::Null))
            .run(&[b"handler"], |io, _| {
                let _ = io.call(actual, &[]);
                // A later correct call cannot reset the first mismatch.
                io.call("SECOND", &[])
            });
        assert_eq!(
            output,
            Err(HarnessFailure::WrongCommand {
                expected: "ECHO".into(),
                actual: actual.into(),
            })
        );
    }
}

#[test]
fn wrong_argument_count_order_and_bytes_stay_failed() {
    for actual in [
        vec![&b"a"[..]],
        vec![&b"b"[..], b"a"],
        vec![&b"a"[..], b"c"],
    ] {
        let output = HandlerHarness::default()
            .expect_call("ECHO", &[b"a", b"b"], Ok(Reply::Null))
            .run(&[b"handler"], |io, _| {
                let _ = io.call("ECHO", &actual);
                Ok(Reply::Null)
            });
        assert_eq!(
            output,
            Err(HarnessFailure::WrongArguments {
                expected: vec![b"a".to_vec(), b"b".to_vec()],
                actual: actual.iter().map(|arg| arg.to_vec()).collect(),
            })
        );
    }
}
