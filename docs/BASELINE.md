# F01 — Local runtime, package and consumer baseline

Completed 2026-10-08 against local base `febf8cb` (upstream source base
`a808377`). The SDK implementation and dependency graph are unchanged from M01.
F01 adds bounded test infrastructure, external package consumers, isolated
reference-module checks and durable evidence. No push, publication or GitHub CI
run was performed. This is a local plan chunk, not implementation of a tracked
upstream issue.

## Results

| Check | Result | Scope |
|---|---|---|
| Rust pure tests | 21 pass | Six facade tests, six dependency contracts, nine new harness tests |
| Python runner/provenance tests | 13 pass | Ten package/provenance/server-identity tests and three runtime-evidence tests |
| Formatting and Clippy | Pass | Pinned Rust 1.94; workspace/all-targets Clippy with `-D warnings`, Redis 7.2 API floor |
| Example builds | 27 debug + 27 release | Rust 1.94, Redis 7.2 API floor, runtime libclang |
| Inherited ordinary integration selection | 92 pass | 23 cases × debug/release × actual Redis 7.2.8/8.10.2 |
| Actual package archives | Four verified | Fresh alternate registry, exact archive/cache/extracted-file checksums |
| External package consumers | Eight builds pass | Manual and public-macro/derived consumers × debug/release × Rust 1.94/1.98.1 |
| Archived manual module | Three verified host runs pass | Both release compiler outputs on Redis 8.10.2; 1.94 output also on 7.2.8; four byte-preservation assertions each |
| Reference consumers | Four release builds and loads pass | JMESPath, event stream, stream filter and Carolina; no module Rust-source migration |
| Reference acceptance selection | Six tests + two JMESPath assertions pass | Formatting/filtering, blocking completion, RDB and AOF restart; details below |
| Candidate compiler floor | Rust 1.88 check passes | Locked workspace/all-targets check only; no MSRV selected |

All runtime evidence is **macOS arm64 with libc**. These results do not establish
Linux, jemalloc, older Redis, static libclang, complete application acceptance,
performance improvements or complete FFI soundness. M01's six API-floor compile
checks remain compile evidence; F01's runtime selection uses the 7.2 floor on
both actual hosts. No existing support promise is silently retired or expanded.

The [runtime evidence](evidence/f01-runtime.json) records source-file and lock
digests, exact Rust/C compiler identities, libclang/server/module hashes, command
logs, all 92 outcomes and 104 owned-server observations. Extra server starts
come from the INFO test's four modules. Redis 7.2.8 was built from the official
source archive with recorded checksum, `MALLOC=libc` and TLS disabled. Redis
8.10.2 is the resolved Homebrew binary, whose observed identity is recorded.
The 7.2.8 build's printed Git SHA comes from the enclosing fork checkout; the
recorded Redis source archive checksum is its source identity.

## What improved in the test infrastructure

The inherited helper retried startup indefinitely, guessed artifact locations,
discarded server output and reused data directories. The replacement:

- Bounds startup to 10 seconds and each command exchange to 5 seconds, including
  socket writes. The outer runtime runner bounds every test to 60 seconds.
- Uses a reserved loopback port, verifies `INFO server`'s PID and expected version
  before test commands, and retains server/memory INFO plus stdout/stderr.
- Infers example artifacts from the actual Cargo test executable, including
  custom target/profile paths. Every test receives a new retained data directory.
- Terminates/reaps its owned server; the runner also cleans up its process group
  on completion, failure or timeout.

The existing test call sites still look like:

```rust
let mut con = TestConnection::new("hello");
let result: Vec<i32> = redis::cmd("hello.mul")
    .arg(&[3, 4])
    .query(&mut con)?;
assert_eq!(result, vec![3, 4, 12]);
```

The helper now supplies a small test-only `ConnectionLike` implementation using
redis-rs's public RESP2 parser. redis-rs 0.23 sends a setup exchange before its
connection API exposes read/write timeout configuration, so merely setting a
TCP connection timeout was insufficient. This helper is not a new SDK/client
API; authentication, database switching and subscription support are outside it.

Pure tests cover silent sockets, blocked writes, expired deadlines, command and
pipeline error alignment, server identity, early exit, cleanup, artifact paths
and directory isolation. The Python evidence checks also reject zero-test
successes and duplicate server-version cells that could overwrite logs.

## Deliberate coverage boundaries and findings

The selected integration cases cover parsing, strings, key scans, streams,
ordinary calls and blocking calls, INFO, context/user APIs, ordinary access
checks, notifications, threading, server events, configuration, replies and
expiration. `tools/runtime_baseline.py` contains the exact positive selection
and a reason for every excluded inherited test. New unclassified tests stop the
runner for review.

| Deferred area or finding | Disposition |
|---|---|
| Shared proc-command example metadata and derived replies | Correct registration/reply contracts before exercising those runtime paths. Public macro and derive compilation is proven by archive consumers. |
| Specialized binary CallReply and event-input cases | Validate the corrected contracts in focused follow-up work; no full inherited-suite claim here. |
| `test_test_helper_err` uses a nonexistent helper command on the hello module | Replace with a meaningful test in the relevant repair. Its current error result would not establish the intended behavior. |
| Active defragmentation | Needs a server/allocator cell that supports it. A libc skip is not a pass. |
| 7.4/8.0-only ACL category tests | Not compiled in the selected 7.2 API-floor matrix; later cells must select the relevant floor. |
| F01 found public registration macros required the consumer's own `linkme` dependency | [F03](F03_MACROS.md) removes this requirement for all 11 registration attributes, with fresh archive consumers and linked-list tests. Historical F01 evidence retains its original manifests. |
| Linux, jemalloc, Redis 6.x/7.0, static libclang | Explicitly untested here. Container/CI execution is a later workstream. |
| Optional Search/JSON/TimeSeries and replication | Require separate reference acceptance cells; ordinary module flows do not certify them. |

The real reference-module flows do exercise public command macros. The deferred
macro runtime cells above refer specifically to the shared inherited example
and the archived proc/derive fixture, not every use of the attribute API.

No selected SDK runtime case failed. Two new-runner setup mistakes were found
and corrected before acceptance: the integration inventory initially omitted the
pure helper namespace, and the stream-filter snapshot omitted an embedded Lua
asset. Their logs and passing follow-up evidence are retained. Review also
strengthened artifact discovery and package-host identity verification; the
package evidence explicitly supersedes its initial PING-only runtime checks.

Package inputs and runtime inputs have their own hashes: archives were created
during F01 before the last harness/documentation edits. Their implementation and
dependency contents are the candidate graph tested, not a claim that later
documentation is present in those exact archives. Nothing was published.

## Reference adoption

All four reference consumers select the facade and public macros from the same
fork graph and explicitly enable `bindgen-runtime`. None required a module Rust
source change. Snapshot workspace isolation and test-helper adjustments are
recorded separately from adoption changes; original working directories remain
unchanged.

- **JMESPath:** exact filter/projection and Redis-String-backed query results.
- **Event stream:** existing byte-identical default entry-format acceptance test.
- **Stream filter:** both existing client-versus-module equivalence tests,
  including pending entries and the complement represented by ELSE.
- **Carolina:** existing native RDB restart, wake-on-append blocking completion,
  and AOF rewrite/restart tests.

See [reference details and reproduction](evidence/f01-reference-results.md) and
[package details and reproduction](evidence/f01-package-results.md). Exact
application revisions, selected source hashes, lockfile hashes, SDK graphs and
copied test-helper patches accompany those reports; full locks remain in the
retained snapshots under `target/f01/references`. This is current fork adoption
evidence; it is not a fresh before/after speed or allocation comparison.

## Reproduce the runtime baseline

With two installed/built native Redis servers, from the fork root:

```sh
export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
python3 tools/runtime_baseline.py \
  --output target/f01/runtime-new \
  --server 7.2.8=/absolute/path/to/redis-7.2.8/src/redis-server \
  --server 8.10.2=/absolute/path/to/redis-8.10.2/bin/redis-server
python3 -m unittest discover -s tools -p 'test_*.py'
cargo test --locked --workspace --lib --test dependency_contracts --test utils \
  --no-default-features \
  --features min-redis-compatibility-version-7-2,bindgen-runtime
```

The libclang path above is host-specific. Python 3.11+ is required. Use a new
output directory for each runtime/package run; raw logs and binaries are retained
under `target/f01`, while compact evidence is committed under `docs/evidence`.
Each runtime invocation requires distinct server versions; compare two builds
of one version in separate output directories. Every build is locked, and the
runtime runner obtains the test executable from Cargo's artifact messages.

The two final runtime arguments match actual `INFO server` values; labels and
API compatibility features alone cannot establish the host version. For a
standalone inherited test, the helper accepts
`REDIS_MODULE_TEST_REDIS_SERVER`, `REDIS_MODULE_TEST_MODULE_DIR`,
`REDIS_MODULE_TEST_LOG_DIR` and `REDIS_MODULE_TEST_EXPECT_REDIS_VERSION`.
Use the selected runner for this baseline instead of a blanket integration run.

## Next chunk and decisions

**F02 starts with a narrow logging-correctness repair**, adapting the existing
contribution with attribution, corrected-code tests and ordinary host behavior.
The evidence also queues macro dependency hygiene and meaningful proc-command
runtime fixtures; it does not justify a broad API rewrite yet.

Before implementation of tracked issues, check overlapping open PRs and follow
the user's issue-visibility kickoff/draft-PR process. F01 itself remained local
and issue-independent. CI modernization remains deferred. Package names,
release versions, MSRV, allocator and server/platform support commitments still
need explicit decisions backed by the appropriate cells; none blocks continuing
focused correctness work.
