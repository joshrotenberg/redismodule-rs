//! Shared call-boundary fixtures for the Rust harness and live module tests.

pub(crate) struct EchoCase {
    pub(crate) name: &'static str,
    pub(crate) argv: Vec<Vec<u8>>,
    // ECHO's bulk return or the error from Context::call, not direct ECHO.
    pub(crate) expected: Result<Vec<u8>, &'static str>,
}

pub(crate) fn binary_inputs() -> Vec<Vec<u8>> {
    let mut large = vec![b'x'; 4096];
    large.push(0xff);
    vec![
        b"".to_vec(),
        b"ASCII".to_vec(),
        "h\u{e9}llo \u{65e5}\u{672c}".as_bytes().to_vec(),
        b"a\0b".to_vec(),
        b"\r".to_vec(),
        b"\n".to_vec(),
        b"\r\n".to_vec(),
        vec![0xff, 0, 0xfe],
        large,
    ]
}

pub(crate) fn echo_cases() -> Vec<EchoCase> {
    let mut cases: Vec<_> = binary_inputs()
        .into_iter()
        .map(|input| EchoCase {
            name: "bulk bytes",
            argv: vec![b"call_reply.echo".to_vec(), input.clone()],
            expected: Ok(input),
        })
        .collect();
    for args in [vec![], vec![b"first".to_vec(), b"second".to_vec()]] {
        let mut argv = vec![b"call_reply.echo".to_vec()];
        argv.extend(args);
        cases.push(EchoCase {
            name: "call arity error",
            argv,
            expected: Err("ERR RedisModule_Call failed: wrong number of arguments"),
        });
    }
    cases
}
