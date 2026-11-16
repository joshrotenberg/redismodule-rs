# Testing module inputs and pure parsers

Use a live Redis process to test module callbacks and command registration. Keep
byte parsing in an ordinary Rust function so unit tests and a fuzzer can exercise
it without Redis. These patterns test different boundaries; parser fuzzing does
not establish that a callback's Redis API use or lifecycle is safe.

## Test an adapted handler without Redis

[examples/handler-testing](../examples/handler-testing) prototypes a private
byte/effect boundary for `call_reply.echo` (#215). Its registered
`fn echo(&Context, Vec<RedisString>) -> RedisResult` remains a native wrapper.
The wrapper borrows each argument's bytes, including the command name, while
keeping every `RedisString` owner alive. It calls this shared body:

```rust,ignore
fn echo_handler(io: &mut impl CommandIo, argv: &[&[u8]]) -> HandlerResult {
    io.call("ECHO", &argv[1..])
}
```

`CommandIo` exposes only synchronous `call(&str, &[&[u8]])`. The closed owned
reply type admits simple strings, bulk bytes, integers, arrays, errors, null
bulk strings and null arrays. Handler errors carry either wrong arity or an
owned message. `NativeIo` calls `Context::call`, converts the admitted values
and explicitly rejects other reply kinds, including native string handles.
The wrapper converts the owned result back for the existing SDK serializer.
There is no test dispatch inside `Context` or any SDK type.

The isolated harness runs the shared body with scripted effects:

```rust,ignore
let output = HandlerHarness::default()
    .expect_call("ECHO", &[b"a\0\xff"], Ok(Reply::Bulk(vec![b'a', 0, 255])))
    .run(&[b"call_reply.echo", b"a\0\xff"], echo_handler)
    .unwrap(); // Outer Result reports harness validity.
assert_eq!(output, Ok(Reply::Bulk(vec![b'a', 0, 255])));
```

Expectations form a first-in, first-out queue. Names match literally; the
harness does not implement Redis command lookup or case folding. Wrong command
names, wrong argument count/order/bytes, extra calls and unused expectations
fail the run. A mismatch remains failed even if the handler catches the error
or makes a later correct call. An expected call error is ordinary handler
input and can be recovered from. Empty synthetic argv is refused before the
body runs; a native callback always includes its command name. Missing or extra
ECHO operands reach the call unchanged and use the SDK call-boundary error,
`ERR RedisModule_Call failed: wrong number of arguments`.

Run the workspace separately; root workspace tests do not include it:

```bash
cargo +1.94.0 fmt --manifest-path examples/handler-testing/Cargo.toml -- --check
cargo +1.94.0 clippy --manifest-path examples/handler-testing/Cargo.toml --all-targets -- -D warnings
cargo +1.94.0 test --manifest-path examples/handler-testing/Cargo.toml
```

The package has no dependencies and installs no global allocator, so its tests
use Rust's system allocator. `HandlerHarness` and `ScriptedIo` exist only under
`cfg(test)` in that package. The native example imports only `shared.rs`, not
the harness or its workspace. There is no SDK testing feature whose additive
activation could pull mocks into a production module. CI formats, lints and
tests this package explicitly on the Linux and macOS lowest-floor jobs and
checks it on the minimum Rust version.

`cases.rs` supplies both test paths with empty, ASCII, UTF-8, NUL, CR, LF,
CRLF, invalid UTF-8 and 4 KiB-plus-one-byte arguments, plus missing and extra
operand cases. `test_adapted_echo_contract` loads the real registered command
and checks bulk reply kind/bytes or exact error code/message. Raw byte requests
check RESP2, RESP3 and a return to RESP2 with a following `PING` on the same
connection. These checks retain registration, native-adapter and wire coverage.
Build examples before running that integration test.

The no-server path tests the adapted body, not the unchanged native wrapper.
Its semantic reply capture does not test serialization, native object
lifetimes, callback safety, `ArgCursor`/`NextArg` numeric parsers or native
allocator behavior. The facade has no key, timer, user, registration, blocking,
replication, raw-pointer or typed call-reply methods. Such calls fail to compile
against it. There are no fake native objects or assigned Redis API pointers.
`call_ext`, `call_blocking`, direct reply writers, `NoReply`, partial aggregates
and blocked clients are outside this prototype. A direct-reply consumer would
need a separate scope decision or adaptation to return an owned reply.

An alternative backend inside `Context` could aim at unchanged-handler tests,
but it would also need Rust-owned `RedisString` storage, typed reply ownership,
inherited context capabilities, argument conversion, reply admission and native
pointer refusal. Allocator fallback alone supplies none of these. This private
facade confines the example to an explicit effect boundary; it establishes no
public SDK abstraction. Generalizing it needs concrete consumers and adapter
cost measurement.

## Send input from a companion module

A small companion module can call `notify_keyspace_event_bytes` to send unusual
input to the module under test. This recipe reuses two existing examples:

- [runtime_events](../examples/runtime_events.rs) is the recipient. Its `generic`
  load argument subscribes to generic notifications; `runtime.last` returns an
  owned copy of the last event, key bytes and selected database.
- [events](../examples/events.rs) is the companion. `events.send_bytes` sends its
  binary argument on the key `mykey`, without changing the module being tested.

From this checkout's root, with Rust 1.94.0, Python 3.13 and Redis server/CLI
8.10.2 on `PATH`, run:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
test_work=$(mktemp -d /tmp/module-testing-build.XXXXXX)
test_target=$(rustc +1.94.0 -vV | sed -n 's/^host: //p')
case "$test_target" in
  *-apple-darwin) test_ext=dylib ;;
  *-unknown-linux-gnu) test_ext=so ;;
  *) echo 'This recipe requires native macOS or GNU/Linux' >&2; exit 1 ;;
esac
cargo +1.94.0 build --example runtime_events --example events \
  --target "$test_target" --target-dir "$test_work/build" \
  --no-default-features --features min-redis-compatibility-version-6-0
python3 examples/module-testing/check_events.py \
  "$test_work/build/$test_target/debug/examples/libruntime_events.$test_ext" \
  "$test_work/build/$test_target/debug/examples/libevents.$test_ext"
```

[check_events.py](../examples/module-testing/check_events.py) loads both libraries
into a disposable native server with a private Unix socket, no TCP listener and
no persistence. It sends empty and invalid-UTF-8 event names through the
companion's stdin argument, checks the recipient and verifies a final `PING`.
Startup, CLI calls and shutdown are bounded; server logs remain in the printed
`/tmp/module-events-*` directory on success or failure. `TEST_REDIS_SERVER_BIN`
and `TEST_REDIS_CLI_BIN` can select explicit binaries.

The SDK's event callback receives `&str`, so invalid event-name UTF-8 is replaced
lossily: `b"ev\xffnt"` arrives as `"ev\u{FFFD}nt"`. Empty names remain empty.
Keys are borrowed bytes, and a callback must copy any data it retains. Event
names containing NUL are rejected by the sending API. See the existing
[SDK event regressions](../tests/integration.rs); this recipe demonstrates a
module-author pattern rather than adding another copy of those tests. For
writes triggered by notifications, use the [notification-write guide](KEYSPACE-NOTIFICATIONS.md).

## Extract and fuzz a pure parser

[examples/module-testing](../examples/module-testing) contains a small
`parse_limit(&[u8]) -> Option<u16>` parser. It accepts decimal ASCII digits and
leading zeroes, and rejects empty input, signs, whitespace, binary data and
overflow. It has no SDK dependency. A real command can call the same kind of
function on `ArgCursor::next_bytes()`; construct Redis handles only inside the
live server boundary.

The parser and its `fuzz/` directory declare independent Cargo workspaces. The
SDK's normal build does not enable their dependencies or fuzz-only features.
CI runs the parser's formatting, strict lint and unit checks separately on Linux
and macOS, and tests it on Rust 1.87. From this checkout:

```bash
cargo +1.94.0 test --manifest-path examples/module-testing/Cargo.toml
```

The fuzz recipe pins **nightly-2026-08-01**, **cargo-fuzz 0.13.2** and
**libfuzzer-sys 0.4.13**. Use rustup's tools throughout, including the subprocesses
started by cargo-fuzz. A C++ compiler is needed for libFuzzer; the validated macOS
setup used Apple Clang 21.0.0. See the [Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz/setup.html)
for platform prerequisites.

```bash
export PATH="$HOME/.cargo/bin:$PATH"
rustup toolchain install nightly-2026-08-01 --profile minimal --component rust-src
cargo +nightly-2026-08-01 install cargo-fuzz --version 0.13.2 --locked
cd examples/module-testing
mkdir -p fuzz/corpus/limit
cp seeds/* fuzz/corpus/limit/
cargo +nightly-2026-08-01 fuzz run limit --sanitizer address -- \
  -runs=10000 -max_len=128 -timeout=2 -max_total_time=30 -seed=1
```

The target compares the parser to an independent standard-library oracle, checks
a formatted-value round trip and treats a panic or mismatch as a failure. Keep
invalid inputs in the corpus too. A successful bounded run is a setup check, not
proof that a parser is correct. The seeded corpus includes valid, overflow and
binary cases; empty input also has a unit control.

Record the checkout revision (`git rev-parse HEAD`), tool versions, both generated
lockfiles and corpus/artifacts with a run. The library repository ignores
`Cargo.lock`, so retain your resolved files for replay. cargo-fuzz prints a failing
input's artifact path and a replay command; use those same features/toolchain
when reproducing it. Generated corpus, coverage and crash artifacts are ignored
by Git. The normal SDK workspace and its runtime dependencies are unchanged.

## Check the allocator used by the fuzz binary

The default parser target uses Rust's system allocator and links no SDK.
The target's optional `system-fallback` feature deliberately installs
`RedisAlloc` as the global allocator and enables the SDK's
`system-allocator-fallback` only in this separate fuzz package. No Redis process
or initialized API pointers exist in this run:

```bash
cargo +nightly-2026-08-01 fuzz run limit --sanitizer address \
  --features system-fallback -- \
  -runs=10000 -max_len=128 -timeout=2 -max_total_time=30 -seed=1
```

Verify the negative control too. `sdk-allocator` installs the same allocator but
keeps fallback disabled, so startup outside Redis must abort with the SDK's
allocator diagnostic. Check both the exit status and the reason; a compiler or
linker failure is not allocator evidence:

```bash
ulimit -c 0
if cargo +nightly-2026-08-01 fuzz run limit --features sdk-allocator -- \
    -runs=1 -timeout=2 > allocator-refusal.log 2>&1; then
  echo 'Expected the unavailable Redis allocator to refuse startup' >&2
  exit 1
fi
grep -F "Critical error: the Redis Allocator isn't available" allocator-refusal.log
```

Both AddressSanitizer configurations completed 10,000 runs on ARM64 macOS with
the pinned tools above; the disabled-fallback control refused startup as expected.
This verifies that selected fuzz/allocator setup. It does not initialize Redis
APIs, make `RedisString` usable outside Redis, or test Redis's own allocator.

Keep fallback out of production dependency features. The SDK chooses one
allocator for a process and does not switch live blocks between allocators;
`redis_module!` rejects loading a module if system allocation was selected before
Redis initialization. These guards remain unchanged. See
[RedisAlloc](../src/alloc.rs) and the [shipping guide](SHIPPING-MODULES.md).
