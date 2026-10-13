# F02 — Logging correctness

October 13, 2026. Adapted from Josh Rotenberg's upstream
[PR 476](https://github.com/RedisLabsModules/redismodule-rs/pull/476), exact source
head `437bf22b933c31d04573943b692542032edd9538`. Inherited BSD notices and package
identities are retained. This fork change leaves upstream issue 473 open.

## Behavior and implementation

Every embedded NUL in a Rust log message is rendered as the two visible bytes
`\0`; other UTF-8 bytes, percentages and existing backslashes retain their text.
The context/global facade and IO logger share the same conversion helper. The
raw notice helper delegates to the facade instead of maintaining a separate
conversion and native call.

Both native logging calls receive a fixed C format and one message argument:

```rust
let level = level.into().as_c_str();
let message = sanitize_message(message);
unsafe {
    raw::RedisModule_Log.expect(NOT_INITIALISED_MESSAGE)(
        ctx, level.as_ptr(), c"%s".as_ptr(), message.as_ptr(),
    )
}
```

The enum's level strings are static `CStr` values, eliminating the per-call
owned level string. Ordinary messages avoid an intermediate `replace` String;
the escaped path reserves enough bytes for all substitutions and the terminator.
These are mechanical allocation/copy changes, not a measured throughput claim.
The helper adds no unsafe conversion, and the owned message remains alive through
the native call. Public signatures remain unchanged. Allocation failure,
initialization contracts, Redis's log-length limits and persistent logger/context
ownership are not made total or certified by this repair.

## Validation

- Five new pure conversion/level tests cover empty and Unicode text, percentages,
  literal backslash-zero, leading/trailing/repeated NUL, a long message, and the
  first 256 Unicode code points. No Redis API pointers or allocator are needed.
- The locked library/dependency tests pass (17), and the existing pure harness
  tests pass (9): 26 Rust tests. All 13 Python tooling tests pass.
- All 28 examples compile in debug and release at the selected Redis 7.2 API
  floor, runtime bindgen, Rust 1.94.0.
- Two corrected-code integration tests pass on owned Redis 7.2.8 and 8.10.2 hosts
  in debug and release: eight executions. They verify exact retained log-line
  suffixes and command replies, then PING. The well-formed `%%` case remains two
  percent characters, making literal-text preservation observable.
- The example covers context, context-free global, raw notice and standard-log
  routes. Its IO command uses `SaveDataTypeToString` to obtain a real synchronous
  save callback and IO handle; the temporary String stays alive through the call,
  and the returned Redis string is freed once.
- Formatting, Clippy across the locked workspace/all-target graph, and whitespace
  checks pass. CI modernization and full platform/support coverage remain deferred.

The first fixture attempt incorrectly retained the load callback's temporary
context for the persistent standard logger. It failed on ordinary text on one
host. The failed evidence is retained; the corrected fixture initializes that
logger without a context and uses command contexts only within each command.
This does not repair or certify all existing logger-context ownership behavior.
No inherited invalid logging path was intentionally executed for reproduction.

## Independent review

Due/Claude performed a read-only source review: exit 0, `approve`, 219,865 ms.
The result, answer, 144 trace records and offline summary validate independently;
trace complete, zero dropped/gap records and no unmatched activities. The seven
reviewed files were unchanged during review. Model/effort identities remain
unavailable; no override was requested. The reviewer ran no tests or builds.

Two nonblocking suggestions were handled: strengthen the literal-percent case
and remove a stale lint allowance from the raw helper. We used the well-formed,
argument-free `%%` conversion for the stronger assertion. The reviewer-approved
SDK logic is unchanged by those test/lint edits; the caller reran the affected
checks. No second model review of those small follow-ups is claimed.

Raw local evidence is under `target/f02/`: fresh validation directories, initial
failed fixture evidence, command logs and `review/20261013T161119Z`. The compact
committed record contains input/artifact/log digests and actual outcomes. It does
not establish Linux/jemalloc, older hosts, full application suites, persistence
format compatibility, module reload or global logger threading soundness.

## Contribution and remote state

Before implementation, overlap checks found the existing upstream contribution
and no fork PR. The dedicated branch received empty kickoff `26e15fb`, was
pushed, and fork draft PR 1 was opened. Its base `review/f02-baseline` points to
validated local checkpoint `3ddbc62`, keeping the F02 review diff isolated from
M01/F01. Remote `master` was not updated by the kickoff.

The deferred CI workflow nevertheless ran on the empty kickoff: four of eight
jobs failed in their Test step, before this repair was present. Failed job logs
could not be retrieved in the first inspection; source-specific failure causes
are not inferred. The remote draft remains open for triage and Josh's merge
decision. Local results are reported separately from remote checks; no GitHub
merge, release or CI redesign is part of F02.

## Reproduce the focused checks

Use the pinned compiler and the documented libclang path. Build the `log` example
and the integration executable with `--locked`, exactly one API floor and
`bindgen-runtime`; repeat with `--release`. Select only
`test_logging_message_text` and `test_logging_io_message_text` with `--exact`.
Set `REDIS_MODULE_TEST_REDIS_SERVER`, `REDIS_MODULE_TEST_EXPECT_REDIS_VERSION`,
`REDIS_MODULE_TEST_MODULE_DIR` and `REDIS_MODULE_TEST_LOG_DIR` to the selected
host, fresh module output and evidence directory. The helper verifies PID/version
and bounds startup/commands. The retained caller script `target/f02/validate.py`
records fresh source hashes before/after, executable/module hashes, server identity,
per-command outcomes and retained log digests.
