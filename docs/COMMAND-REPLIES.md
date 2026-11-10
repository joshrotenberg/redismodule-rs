# Command replies

SDK command callbacks count complete replies written through the safe
`Context::reply`, `reply_simple_string`, `reply_error_string`, `reply_with_key`
and `reply_with_call_reply` methods. Once one succeeds, another safe reply is
refused before it reaches Redis. The SDK logs a warning and drops a handler's
returned value or error if it would be another reply. A panic after a complete
safe reply is logged without appending its error to the client connection.

An ordinary handler can keep returning its value. A handler that replies
directly should return `RedisValue::NoReply`. A top-level `NoReply` writes
nothing and does not consume the invocation's reply. Inside an aggregate it
still becomes null, because the aggregate owes an element.

## Scope

This is the bounded whole-reply prototype for #224; it leaves #224 open.
Handler signatures are unchanged. There is no new command streaming writer.

- Raw `RedisModule_ReplyWith*` calls are not observed. A raw aggregate header
  followed by safe element writes is unsupported: each safe call is treated
  as a complete root. Construct a `RedisValue` aggregate or write the entire
  raw reply using the native API.
- A panic during serialization can leave part of an aggregate written. This
  prototype does not track or finish those elements. It retains the previous
  panic-error behavior for that case.
- A blocked client sent to a worker has separate reply ownership. Continue
  returning `NoReply` after successful blocking. The command frame does not
  coordinate with a later worker reply.
- Key-blocking callbacks retain their existing `KeyBlockingReply` writer and
  its partial-aggregate accounting.

If a handler drops a blocked handle without replying and then writes a
complete safe reply itself, returning `NoReply` keeps that reply instead of
adding the released-without-a-reply error.

## Accounting

Each SDK command links a stack frame to a thread-local pointer. The slot has
no destructor, so it does not prevent module unloading on glibc. The frame
stays linked through panic recovery. Nested commands link their own frames,
and a write matches its native context against the innermost matching frame.

Admission reserves one whole reply before entering its serializer. The
serializer's recursive calls use the private reply context and do not acquire
another root. No state borrow spans a native call. Successful writes mark the
root complete; a refused forwarding attempt leaves room for an error. The
message-error form of a forwarded call uses the ordinary error entry point
once. `Context::call` itself sends no client reply and is not counted.

The ordinary scalar path allocates no aggregate state. This does not establish
its cost: the performance measurements must compare the actual loaded module.

## Verification

The wire regression pipelines a command and `PING` on the same connection,
then sends another `PING`. It covers direct replies followed by a return,
error or panic, repeated direct and forwarded replies, nested commands,
released blocked handles, failed forwarding, `NoReply` and nested aggregates
under RESP2 and RESP3. Before the fix, a direct reply followed by a returned
value yields `first, second` where the pipeline expects `first, after`.

Performance measurements compare the same fixture module against the base SDK
and the candidate, with the same dependency lockfile and release profile.
Return-only scalar, direct scalar, forwarded reply and aggregate cases are
measured separately. A manual optimized bookkeeping measurement is available:

```sh
cargo test --release --lib measure_command_reply_bookkeeping -- --ignored --nocapture
```

That measurement excludes native serialization and network I/O and uses the
test binary's TLS model. It does not replace the loaded-module comparison.

For the loaded comparison, build the same `test_helper` fixture against the
base SDK and the candidate with `cargo build --release --example test_helper`:

```sh
python3 tests/bench_command_replies.py \
  --baseline /path/to/base/libtest_helper.so \
  --candidate /path/to/candidate/libtest_helper.so \
  --server redis-server --work-dir /path/to/owned/benchmark-output
```

The script starts two owned Redis children, verifies their process IDs, checks
the reply bytes, alternates variant order and kills/reaps both children on
exit. Its JSON includes every sample and each module's SHA-256 digest.

### Local comparison, 2026-11-10

Redis 8.10.2, macOS 26.7.3 arm64, Rust 1.94.0, release profile. The base SDK
was `e6d6740`; both builds used identical fixture source and Cargo.lock. Each
case had nine alternating samples of 32,768 commands, pipelined 128 at a time.
Owned compilation and test jobs were paused during the comparison.

| Case | Base median ns/command | Candidate median ns/command | Change |
| --- | ---: | ---: | ---: |
| Returned scalar | 611.1 | 608.4 | -0.4% |
| Direct scalar plus NoReply | 587.7 | 616.0 | +4.8% |
| Forwarded call reply plus NoReply | 971.8 | 982.8 | +1.1% |
| Returned nested aggregate | 569.1 | 568.4 | -0.1% |

These amortized wall times include client I/O and reply verification. Small
differences may be noise; this comparison does not set an acceptance threshold
or latency bound. The isolated optimized bookkeeping run measured a median
3.289 ns with frame/admission accounting against 0.275 ns for its empty
baseline. That test-binary result is not a loaded-module per-command cost.
