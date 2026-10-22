# Wasm experiment CI

[The experiment workflow](../.github/workflows/wasm.yml) runs one Linux
`ubuntu-24.04` job on pushes to `experiment/wasm-host` and PRs targeting that
branch. Its check is **Wasm host and Rust/JavaScript guests**. Re-run an
existing Actions run to repeat the same head. There is no release, publication or
scheduled trigger.

The SDK's [normal workflow](../.github/workflows/ci.yml) keeps the existing server
matrix for master pushes and PRs targeting `master`. The experiment does not run
that matrix: its host selects the SDK's default 6.0 floor, which conflicts with
alternate floors in a combined workspace build. The experiment instead checks the
full default-floor workspace and a host-excluded SDK 7.2 build. Wasm source and
this workflow stay on the experiment branch; SDK master remains independent.

## Inputs and commands

| Input | Pinned selection |
|---|---|
| Rust | 1.94.0, with rustfmt, Clippy and `wasm32-unknown-unknown` |
| Runtime and host dependencies | `ci/locks/workspace.lock`, including Wasmtime 36.0.19 |
| Rust guest dependencies | `ci/locks/guest-sdk.lock`, `hello.lock` and `bench.lock` |
| Redis | 8.10.2; source checksum in `.github/servers.json` |
| JavaScript runtime | Node.js 24.22.1, from the [official Node release index](https://nodejs.org/dist/index.json) |
| JavaScript guest tools | Existing `guests/js/package-lock.json`; componentize-js 0.18.5 and jco 1.14.0 |

The workflow copies the Rust lock snapshots into a fresh checkout before fetching
with `--locked`. Validation then runs with Cargo offline, including the nested
host/guest builds started by the integration tests. The validation script checks
that all four lockfiles remain byte-identical to the snapshots, even on failure.
The guest build uses no WASI adapter or newly installed wasm-tools executable.

JavaScript is required in CI. Node and npm must work, `npm ci` installs the locked
guest tools, and `WASM_TEST_REQUIRE_JS=1` makes missing Node a test failure rather
than an optional skip. Local tests outside this workflow retain their optional
JavaScript behavior. Required build or test failures fail the job.

After setup and dependency fetching, [ci/check.sh](ci/check.sh) runs these gates:

| Gate | Commands or behavior |
|---|---|
| Default SDK and host | Workspace formatting, strict all-target Clippy, explicit SDK example and host builds |
| Native behavior | `cargo test --locked --workspace --no-fail-fast`, including host integrations and both Rust and JavaScript guests; separate allocator-fallback test |
| Documentation | Strict full-workspace default-floor docs; strict SDK docs at 7.2 with the host excluded |
| Alternate SDK floor | Strict all-target SDK Clippy at 7.2, excluding the host |
| Guest helpers | Format, strict native all-target Clippy and unit tests |
| Rust hello and benchmark guests | Format, strict wasm32 Clippy and release wasm32 builds |

The harness builds the native host explicitly before loading it and builds the
Rust hello component using the pinned guest lockfile. This prevents a test from
loading a stale module. The pinned Redis directory is added to `PATH` for host
tests, and `TEST_REDIS_SERVER_BIN` selects the same binary for SDK tests.

## Bounds, diagnostics and cost

The job has a 30-minute hard limit. Validation commands each have a 12-minute
limit and a further 20 seconds before forced termination; dependency fetching has
separate six-minute host/two-minute guest limits. Cargo build parallelism is two,
and Rust tests use two threads. CI disables development/test debug info to
reduce runtime build disk and memory pressure; release profiles are unchanged.
A timeout is a failure, not a skipped test.

There is one Linux job rather than a server/OS matrix for the larger runtime.
Cold runs compile Wasmtime/Cranelift and build Redis; warm runs reuse Rust build
products, Cargo downloads, npm downloads and the version-verified Redis binary.
Rust/server cache keys include the runner image and pinned dependency/source
inputs. `node_modules` is recreated by `npm ci`, not restored as a cached install.
Changes to locks or the toolchain deliberately invalidate the relevant caches.
This job provides Linux coverage; it does not establish a macOS release-stripping
result or replace testing additional server versions when those contracts change.

Stage output goes to `target/wasm-ci-logs/`. Actions retains those files, SDK
integration directories, native host server directories/logs and the Redis build
log for seven days, including failed runs.

For local reproduction in a fresh checkout, install the selected Rust target,
Node and Redis. Restore the four lockfiles using the workflow's copy commands,
fetch them with `cargo fetch --locked` at the root and each guest manifest, and
run from the repository root:

```bash
bash wasm/ci/check.sh
```

The script does not replace local lockfiles. They must already match the tracked
snapshots. It uses GNU `timeout`; on macOS with GNU coreutils use
`TIMEOUT_BIN=gtimeout bash wasm/ci/check.sh`. The documented CI result uses the
pinned Linux inputs above, not whichever tool versions happen to be on a laptop.

After an intentional dependency change or SDK sync, regenerate the applicable
lockfile with the pinned toolchain, review its dependency diff, update its
snapshot and run the full workflow again. Keep runtime/toolchain changes and
macOS stripping work separate from routine SDK syncs.
