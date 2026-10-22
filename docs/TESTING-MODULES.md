# Testing module inputs and pure parsers

Use a live Redis process to test module callbacks and command registration. Keep
byte parsing in an ordinary Rust function so unit tests and a fuzzer can exercise
it without Redis. These patterns test different boundaries; parser fuzzing does
not establish that a callback's Redis API use or lifecycle is safe.

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
