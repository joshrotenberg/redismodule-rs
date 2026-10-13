# F04 — Meaningful proc-command runtime fixtures

F04 fixes the keyless reply example's metadata and replaces its length-only
reply assertion with field/value contracts. The command takes no keys, so it
declares `key_spec: []`; the dummy index-zero, zero-step spec is removed before
runtime testing. The example's module name now matches `proc_macro_commands`.
No historical invalid fixture was executed. SDK production source, public API
signatures and dependency constraints are unchanged.

## Covered behavior

The example tests verify command names, arities and readonly flags; empty key
metadata for the keyless command; index/range, keyword/range and counted-key
discovery (including zero counted keys). The derived reply test checks all 11
fields, typed integer/string values, vectors, nested maps/sets, flattening,
the float value and the enum's string branch. A PING confirms the server still
responds after each test.

The external archived proc fixture verifies its name, exact arity, flags, empty
key specs and summary through COMMAND INFO/DOCS. Its reply must contain an
integer count and a string message, including signed i64 endpoints. Ordinary
missing/extra arguments and a non-integer return expected errors; a valid typed
command succeeds after each error. This exercises the public attribute, NextArg
and RedisValue derive through actual candidate archives without direct linkme.

The archive harness needed RESP2 array/integer decoding for metadata and typed
replies. Its small test-only decoder also handles simple/bulk strings, null and
error replies, closes each reader/socket, and rejects truncated frames. Pure
tests check nested binary/null decoding and rejection of an incorrectly typed
reply. This does not add a production Redis client or RESP3 support.

## Evidence

- Eight example-test executions: two tests x debug/release x Redis 7.2.8/8.10.2.
- Eight accepted archive-proc flows: Rust 1.94.0/1.98.1 x debug/release x those
  two Redis hosts. Metadata, integer/arity errors and recovery run in every flow.
- Four actual SDK archives, six consumers and 12 fresh archive builds; four
  linked-registration test runs. Existing manual echo checks also pass on 7.2.8.
- 19 Python tests, targeted debug/release example/integration builds,
  workspace/all-target Clippy, formatting and whitespace checks pass.
- Exact archive/cache/extracted-file and compiled module/executable hashes are
  retained with owned process IDs, version/allocator identity and server logs.

Detailed outputs: `target/f04/runtime/report.json`,
`target/f04/packages-validated/report.json` and
`target/f04/archive-8.10.2-final/report.json`. The two formerly excluded example tests
join the selected runtime inventory. Other unchanged baseline cases were not
rerun here; historical F01 evidence is preserved. The final compiled test
inventory has 27 selected cases, with no missing/unclassified cases, but the
full runner was not executed. The focused example producer reuses owned
`target/f02/build` compilation outputs; executable/module/input hashes identify
the F04 bytes. The final 8.10.2 archive producer and its hash are retained in
`target/f04/run-archive-final.py` and its report.

The initial package attempt stopped because the old string-only reader could
not decode a COMMAND INFO array. This was a harness limitation, not a failed
module assertion. Its archives/builds/logs remain in `target/f04/packages`.
After decoder correction and pure checks, a fresh package run passed. A separate
preliminary runtime report is retained in `target/f04/archive-runtime`; accepted
counts above use the final fresh artifacts.

## Process and limits

Issue-independent local branch from eea0ce6; no push, draft PR or publication.
Due/Claude approved the fixture/source changes; request/result/answer/trace/summary
contracts and the full receipt independently validate. The caller addressed its
evidence suggestions with the final 19-test Python log, Clippy with warnings
denied, inventory check and final archive producer. Its review of unchanged SDK
source was limited to supporting greps, not a new ownership audit. Post-review
changes clarify documentation/evidence; no second model review is claimed.

Archive queries open a fresh connection each time. A valid command after an
error proves continued server service; it does not prove same-client session
recovery. The example tests use the same connection for their final PING.

Evidence is native macOS arm64/libc, API floor 7.2, RESP2, ordinary standalone
module flows. It is not a full inherited-suite, RESP3, Cluster routing,
persistence, replication, CI or general FFI soundness certificate. No MSRV,
package/release identity or server/platform support commitment was chosen.

Next is one focused Rust API/ergonomics slice, informed by the four reference
modules without migrating their original working directories.
