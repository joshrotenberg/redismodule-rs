# F01 reference consumers — 2026-10-08

All four isolated reference modules compile in release mode with Rust 1.94.0 and
load into owned loopback Redis 8.10.2 processes on macOS arm64. No module Rust
source migration was required. The consumer manifests select this fork's facade
and public macros together, preserving their Redis 7.2 API feature and adding
the now-explicit `bindgen-runtime` backend.

| Consumer | Exact source revision | Selected runtime evidence |
|---|---|---|
| JMESPath | `0e70ab55dd08823029552faa495b22fd43b2f977` | EVAL filter/projection and QUERY of a Redis String return exact expected JSON. |
| Event stream | `93022f60502a828a087c6ef8aff935443dcc1823` | Existing default entry-format test passes byte-identical output checks. |
| Stream filter | `97ebad3b34fdaba26c9708a771d5425d1aa2ee97` | Both existing equivalence tests pass: direct iterator and RM_Call results match client filtering, including pending entries; ELSE is the complement of sibling filters. |
| Carolina KLOG | `0191175bb18ecf371d49d8b264a514f2c662099c` | Existing RDB restart, wake-on-append blocking fetch and native AOF rewrite/restart tests pass. |

Each source repository was clean before the snapshot and remains unchanged.
The [machine-readable evidence](f01-reference-consumers.json) records source
revisions, selected-file hashes and aggregate digests, original/adapted lock
hashes, exact resolved SDK package paths/features, command results/log hashes,
server/module hashes and observed server version. The four SDK crates in each
graph resolve to this fork; same-version registry crates cannot substitute.
Owned load checks compare `INFO server`'s process ID with the child PID. Final
module artifact hashes still match those load checks after the acceptance tests.

## Reproduction

Run from the fork root with the four source repositories beside it:

```sh
python3 tools/reference_baseline.py --profile release
python3 tools/reference_baseline.py --reuse --runtime-only
python3 tools/reference_baseline.py --reuse --selected-tests --timeout 300
```

Snapshots, locks, complete Cargo metadata, logs and build products are under
`target/f01/references`; the writable Cargo cache is `target/cargo-home`.
Preparation refuses to overwrite existing snapshots. `--reuse` preserves the
existing snapshot rather than silently refreshing source. Initial metadata
resolution intentionally replaces the original git dependencies; compilations
then use `--locked`. The selected-test runner requires one actual passing test
per filter, so a zero-test success cannot satisfy the check. Outer Cargo/test
deadlines terminate the owned process group; owned Redis startup and teardown
also have explicit deadlines.

The current selected reference-test host is `/opt/homebrew/bin/redis-server`.
The owned-load check accepts `--redis-server` for an explicit alternate binary;
that does not change the selected reference tests' pinned host. This is a local
macOS baseline, not portable runner or CI validation.

## Harness adaptations and limits

- JMESPath receives an empty workspace table so the snapshot is independent of
  the enclosing SDK workspace. Carolina's snapshot retains only `klog-module`
  as a workspace member; broker/client/benchmark acceptance is separate work.
- Nested existing test builds are scoped to the module package and locked.
  Carolina's test helper additionally honors `CARGO_TARGET_DIR` and selects the
  pinned server binary. The three small harness patches are retained beside
  the JSON evidence. These edits affect copied test infrastructure only.
- The first stream-filter test compilation found an omitted snapshot asset,
  `functions/sf.lua`, embedded by its shared test helper. The selector now copies
  it; that compile attempt ran zero tests. Its failed log is retained, followed
  by the passing rerun. No SDK or module implementation fix was needed.
- Six selected existing runtime tests pass, in addition to the four load checks
  and two JMESPath command assertions. This does not claim complete acceptance
  of any of the four modules, replication coverage, or support on Redis 7.2.
- JSON/Search/TimeSeries integration remains unexercised. JMESPath's final-key
  metadata needs a separate correctness review; its MQUERYREF and key-discovery
  paths were not exercised. Command-metadata inspection did not validate every
  semantic contract, and normal command results are not ACL/cluster evidence.
- These checks establish a current fork baseline. They do not compare speed,
  allocation counts, or output against a separately rebuilt original SDK.

Redis's [command-info implementation](https://github.com/redis/redis/blob/8.0/src/module.c)
and [key-spec evaluation](https://github.com/redis/redis/blob/8.0/src/db.c) were
consulted for static metadata review; that review did not execute defect probes.
