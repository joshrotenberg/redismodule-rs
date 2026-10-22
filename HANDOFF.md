# Handoff

How to pick up work on this fork: the `redis-module` SDK on `master`, and the
wasm host on the `experiment/wasm-host` branch. State as of 2026-10-22.

## What is where (baseline before the INFO guide)

| | where | state |
|---|---|---|
| SDK, 3.0.0 (unpublished) | `master` of [joshrotenberg/redismodule-rs](https://github.com/joshrotenberg/redismodule-rs) | `3b2b845`, early compatibility-floor validation and workspace guide merged after independent review and all 20 PR checks passed |
| wasm host, crate `redis-module-wasm` | `wasm/` on branch `experiment/wasm-host` | `master` plus wasm commits; experiment changes target this branch, not SDK master |
| What the fork changes from upstream | [CHANGES-FROM-UPSTREAM.md](CHANGES-FROM-UPSTREAM.md) | current through `3b2b845` |
| Changes by version, migration | [CHANGELOG.md](CHANGELOG.md), [docs/MIGRATING-3.0.md](docs/MIGRATING-3.0.md) | current |
| wasm host work list | [wasm/TRACKING.md](wasm/TRACKING.md) (this branch) | H, S, G, Q items and a dated log |
| wasm host usage | [wasm/README.md](wasm/README.md) (this branch) | current |

Latest SDK checkpoint: `d0bba7d` binds server INFO snapshots to their producing
runtime context. Earlier INFO guidance (`a14b71b`) and release-note indexes
(`084cd23`) are included. Each SDK change received independent native review
and all 20 latest exact-head CI checks passed before merge. CHANGES-FROM-UPSTREAM
and the migration guide include this work. The layout table retains the shared
baseline while experiment CI proposal PR 79 awaits approval; its source is not
included on this branch. The checkpoints below have validation details.

The fork branches from upstream master at `a808377`. Upstream has two commits
since: `cca5360` (the `ServerInfo::field` leak fix; the fork has its own fix) and
`4ab88e7` (upstream PR 489, a new `REDISMODULE_OPTIONS_REQUIRE_LOADED_KEYS_IN_RAM`
define in `redismodule.h`, not in the fork's header or committed bindings).
CHANGES-FROM-UPSTREAM.md's Base section has the details.

The branches of merged fork PRs are still on the fork; only `master` and
`experiment/wasm-host` are live.

## Setup

```bash
git clone https://github.com/joshrotenberg/redismodule-rs
cd redismodule-rs
git remote rename origin fork
git remote add origin https://github.com/RedisLabsModules/redismodule-rs
git fetch fork && git fetch origin
```

- Remotes: `fork` is this repository, where all pushes go. `origin` is
  upstream and is read-only for this work: no pushes, pull requests, issues or
  comments there, and no crates.io publishing.
- Toolchain: `rust-toolchain.toml` pins 1.94.0; the MSRV is 1.87. If another
  `rustc` (for example Homebrew's) comes first on the `PATH`, run cargo with
  `PATH=$HOME/.cargo/bin:$PATH` so rustup's pinned toolchain is used.
- Redis: the integration tests start `redis-server` from the `PATH` (run
  locally with 8.10.2). CI builds pinned Redis 7.2, 7.4 and 8.0 through 8.10 releases, plus Valkey
  8.1 and 9.1 at the default SDK floor. Focused native-floor checks cover Redis
  6.0 blocking/timers and Redis 7.0 configuration ownership. For other
  versions, the `redis:<version>` Docker images work; with colima on macOS only
  `$HOME` is mounted, so copy a built module into a container with `docker cp`.
- wasm branch only: `rustup target add wasm32-unknown-unknown`. The JavaScript
  guest needs `node` and `npm` (its test is skipped without `node`; the first
  run does `npm ci` in `wasm/guests/js`). `wasm-tools` is optional.
- Active work is tracked in [GitHub issues](https://github.com/joshrotenberg/redismodule-rs/issues).
  Area and priority labels distinguish core SDK, examples/documentation and wasm.
  Design questions carry `needs-design`; implementation still uses draft PRs.
  `wasm/TRACKING.md` retains experiment details and historical measurements.
- The fork's Actions allow only GitHub-owned actions,
  `actions-rust-lang/setup-rust-toolchain` and `swatinem/rust-cache`. The
  release workflows run only in the upstream repository.

## Conventions

- Use `codex/<slug>` for new work branches and conventional-commit prefixes
  for PR titles and commit subjects.
- Commit messages, PR bodies and docs are technical: what changed, why in code
  terms, which gates ran. No em dashes, and no AI attribution lines or
  trailers.
- `#N` in a fork commit message or PR body links to fork PR N, so upstream
  items are written out as "upstream PR N" or "upstream issue N". Squash commit
  messages carry no PR numbers at all.
- Every SDK change updates CHANGELOG.md and CHANGES-FROM-UPSTREAM.md, and
  docs/MIGRATING-3.0.md when it breaks code. Check any upstream issue or PR
  number with `gh api` (type, title, state) before citing it.

## Making an SDK change

1. Branch off `fork/master`: `git switch -c codex/<slug> fork/master`.
2. Push an empty `chore: start <topic> [skip ci]` commit and open a draft PR
   against `master` whose body has Plan, Changes, Verification and Out of
   scope sections.
3. Implement, with tests in `tests/integration.rs` (helpers in `tests/utils.rs`)
   and an example under `examples/` when the change adds API.
4. Run the gates on the pinned toolchain:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo clippy --workspace --all-targets --no-default-features --features min-redis-compatibility-version-7-2 -- -D warnings
   cargo build --examples
   RUST_TEST_THREADS=4 cargo test --workspace --no-fail-fast
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --features all-features-but-xor
   ```

   On `experiment/wasm-host`, exclude `redis-module-wasm` from alternate-floor
   SDK Clippy and documentation checks, as shown below; the host uses the default
   floor. Default-floor documentation includes the full workspace.

   The integration tests load example modules beside their executable's build
   profile, including with `CARGO_TARGET_DIR`. Build examples explicitly before
   running filtered integration tests to avoid loading stale modules.
5. Record the gate results in the PR body and have an independent native
   subagent review the actual PR diff. Fix findings, run the appropriate checks,
   and have the resulting changes reviewed again until no blocking findings remain.
6. The user has authorized future SDK PRs from this workflow to be merged once
   independent review is clear and every CI check passes on the reviewed head.
   Squash-merge that exact head with a conventional subject without PR numbers
   or attribution trailers. Unresolved API designs still need discussion: record
   the needed decision, park the item and move to independent actionable work
   in priority order. A pending decision must not stall the whole workflow.
7. Rebase `experiment/wasm-host` onto the new master (below).

## SDK changes and open work

The [issue backlog](https://github.com/joshrotenberg/redismodule-rs/issues?q=is%3Aissue+is%3Aopen+sort%3Acreated-asc)
is the source for active work. Priorities are core SDK first, examples and
migration material second, and new wasm features third. The following records
completed SDK changes.

From a survey of
[redis-developer/redis-event-stream-module](https://github.com/redis-developer/redis-event-stream-module)
(commit `f0ebe1f`), a module built on this SDK, in the order to take them:

Module-argument configuration fixes merged in `5bf61ff`: numeric bounds are
enforced after memory-unit conversion, booleans accept case-insensitive
`yes`/`no`, and arguments are read as pairs.

The ACL category reload fix merged in `efad730`: an existing category is
reused only when Redis reports `EBUSY`. Invalid names and other registration
failures still reject the module load. Regression tests verify retained user
permissions across two reloads and rejection of invalid names.

The integration harness improvements merged in `2f8e2a9`: module arguments,
fallible startup with retained logs, bounded probes and polling, replica startup,
and graceful persistent restarts. Fresh launches retry confirmed bind collisions
with isolated directories and logs; restarts retain their original port.

Database selection helpers merged in `e9425f8`: `Context::select_db`,
`get_selected_db`, and `with_db`, which restores the original selection after
return or unwind. Integration tests cover nested scopes, invalid indices,
errors, and panics.

Error-code and notification helpers merged in `1d918e7`: borrowed error
prefixes on both call-error wrappers and byte event names with NUL rejection.

Optional command builders merged in `bfacc11`: `redis_command!(name, handler)`
with typed flags, named key positions, and ACL settings. Builder expressions
and legacy tuples share registration and can be mixed in `redis_module!`.

Runtime keyspace subscriptions merged in `00b7c4d`: a fallible Context method
and a reusable event-handler adapter, with proven module ownership required
for unload cleanup. The migration guide covers the tightened ownership
requirement on the legacy three-argument event macro.

Registration contexts merged in `960cd99`: `RegistrationContext` limits command,
configuration and data-type registration to module load.

Event and worker views merged in `5095c06`: `RuntimeContext` provides shared
calls, keys, databases, logging and scoped ACL-call users. `EventContext` carries
module-owned callback operations; borrowed worker guards expose `WorkerContext`.
Client command and blocked-client reply handlers keep `Context`. Init hooks
forward through the event view to the shared runtime view; shared helpers use
`&RuntimeContext`. Event and worker views cannot reply to or block clients.
The wasm loading and unload callbacks now take `EventContext`.

Scoped call user naming merged in `ffc7ac4`: `scoped_call_user` returns the
root-exported `CallUserScope`, replacing the old authentication names.
Nested call users merged in `d44a210`: `with_call_user` preserves each context
view, and nested guards restore the latest active user on return or unwind.
Forgotten guards are cleaned up before worker unlock. No host adaptation needed.

Database event payloads merged in `56c2a2b`: `flush_info_event_handler`
receives the affected database, and `swapdb_event_handler` receives both IDs.
Existing phase-only flush handlers remain supported. No host adaptation needed.

Notification writes merged in `9e86313`: rejected post-notification jobs now
release their captured data. The guide and `keyspace_to_stream` example cover
owned callback data, loop prevention, replication and teardown. No host
adaptation needed.

Expanded CI merged in `8969a12`: checksum-pinned server builds are cached per
runner image and selected through `TEST_REDIS_SERVER_BIN`. Failure logs are
retained for seven days. All six Linux API floors and both macOS floor checks
remain covered; newer Redis and default-floor Valkey legs are required.
No host adaptation needed.

Stable slot keys merged in `69bc762`: `cluster::key_for_slot` produces a tagged
ASCII key for every valid slot without a server API. Exact names are stable
across SDK and server versions, pinned by an exhaustive compatibility fixture.
The harness also retries confirmed TCP listen-stage port collisions. No host
adaptation needed.

Server wall-clock time merged in `15389f7`: `milliseconds()` reads Unix
milliseconds without a context or Redis lock after API initialization.
No host adaptation needed.

Validated string configurations merged in `55ec6f1`: `ParsedStringConfig<T>`
validates raw text before publishing the raw and parsed values together.
Existing registration, defaults, arguments and callbacks remain supported.
No host adaptation needed.

Cargo module versions merged in `d34713b`: `module_version!()` derives the
integer from the calling crate version with const validation. Existing explicit
versions remain supported. No host adaptation needed.

The module upgrade guide merged in `083f98b`: unload/load eligibility,
configuration replay, repeated initialization, cleanup and failure recovery,
with a tested lifecycle example. Strict documentation builds now deny warnings
after correcting existing API links. No host adaptation needed.

Configuration notes merged in `1a715a0`: load precedence, argument parsing,
validation, observers, rollback, storage and reload behavior, with regression
coverage. No host adaptation needed.

Safe module-name initialization merged in `1915b2f`: the macro builds a
terminated constant byte array sized to the name and rejects NUL bytes at
compile time, replacing the unchecked fixed-size copy before allocator setup.
No host adaptation needed.

Configuration observer thread bounds merged in `43fff6b`: all four configuration
registration helpers require `Send` captures, including macro declarations.
Callbacks still need no `Sync` bound because Redis serializes invocation.
A tested example covers client and worker updates and safe worker teardown.
No host adaptation needed.

Guard thread bounds and binary storage merged in `c2566c9`: shared guards
require `Send` values, binary configurations use owned `Vec<u8>` storage, and
private call-user bookkeeping is thread-local. Native strings remain non-`Send`.
No host adaptation needed. General native-handle storage remains separate.
Checked client blocking merged in `e94c0ff`: `block_client()` returns a result,
both blocking helpers reject denied invocation contexts before native blocking,
and direct handles are non-null. A focused Redis 6.0 CI regression covers its
older context flags. No host adaptation needed.

Call protocol selection merged in `f1cdbd2`: repeated `CallOptionsBuilder::resp`
calls replace the prior selection, including a reset to RESP2. Native tests
cover RESP2 and RESP3 clients and a Redis 6.0 reset to RESP2. No host adaptation
needed.

Typed timers and lifecycle ownership merged in `5a99fb1`: creation returns a
`TimerHandle<T>` result with `Send + 'static` callback and payload bounds.
Cancellation is explicit and recovers the payload once; queries expose remaining
time only, and dropping a handle leaves its timer scheduled. The SDK protects
pending/executing timers from unload and rejects creation during initialization
and unload. Start timers from runtime commands or events. No host adaptation
needed. Cancellation-on-drop and payload inspection have separate design issues.

Configuration registration ownership merged in `cbf3f3f`: accepted private
allocations and final cached string getter replies belong to the macro-managed
load generation. Caught load failures and successful deinit release them under
the Redis lock; refused deinit retains them. Unload waits for native CONFIG
execution to finish, using regular post-notification jobs or a main-thread timer
fallback. Workers without a usable completion job conservatively prevent unload
for that load generation; joining does not clear it. General native-handle storage
remains separate. The host's synchronous numeric/bool registration needs no source
adaptation, and its existing unload/reload regression covers this integration.

Internal macro parsing moved to Syn 2 in `0f6e76b`: API wrappers and mixed
command declarations retain their existing syntax and generated code. Public
metadata parsing followed in `6c1f3a1`, replacing `serde_syn` with a private parser
for the existing grammar and closing issue 57. Baseline compatibility tests cover
typed/quoted names, options, key specs, recursive help/ACL data and integer rules.
Registration/reply generation and helper names are unchanged; invalid-input
diagnostic wording may differ. CI executes macro tests on the matrix and Rust
1.87. No host source adaptation needed.

The SDK test client moved to pinned Redis 1.2.2 in `ef5b2c9`, preserving the
Rust 1.87 minimum, bounded sockets, binary distinctions and pipeline alignment.
Direct RESP3 parsing replaces handshake reply suppression. Native Redis 6.0 CI
covers supported client and transport controls; the full matrix retains cases
that require newer APIs. No SDK runtime dependency or host source changes.
The wasm host keeps its independent development client for this sync.

Strict public macro documentation was fixed in `85791c4`: qualified SDK type
text and public links replace inaccessible references, and the derive docs name
`RedisValue::OrderedMap` correctly. CI documents every SDK workspace package with
warnings denied at each API floor. Generated code is unchanged; the wasm branch
can now run full strict workspace docs instead of selecting only SDK/host packages.

The [native module shipping guide](docs/SHIPPING-MODULES.md) merged in
`00cb5d2`. It records platform/build inputs, packages checksums and verifies an
extracted `hello` release artifact in a bounded native Redis process. Its pinned
recipe and corruption/load-failure controls were validated on ARM64 macOS,
including Python optimization. SDK runtime and host source are unchanged.

The [module testing guide](docs/TESTING-MODULES.md) merged in `be41e00`.
It reuses existing modules for companion event input and adds an isolated pure
parser/fuzz fixture. The pinned libFuzzer setup validates ordinary allocation,
explicit SDK test fallback and refusal without fallback. The normal SDK graph,
production allocator guards and host source are unchanged. Check the fixture
separately; its manifests declare independent workspaces.

The [compatibility-floor workspace guide](docs/COMPATIBILITY-FLOORS.md) merged
in `3b2b845`. Its multi-consumer controls exposed a missing early guard: mixed
floors could select lower-version API signatures. The SDK build script now
rejects zero or multiple floors before C compilation or wrapper expansion.
Valid single-floor builds and runtime API availability are unchanged. Separate
package builds can use different floors; a combined build must agree on one.
Issue 59 stays open for the parked feature-design decision. No host adaptation
needed; the existing alternate-floor host exclusion remains required.

The [INFO handler guide](docs/INFO-HANDLERS.md) merged in `a14b71b`, with
release-note indexes in `084cd23`. It documents the existing output-only view,
callback lifetime and native crash/thread paths across Redis 6.0 through 8.10.
An atomic telemetry example and native test cover ordinary INFO and nested
`GetServerInfo`; compile controls reject lock-proof or runtime promotion. This
adds no context capability and makes no async-signal-safety promise. Issue 61
stays open for the parked API design. The host requires no adaptation.

The server INFO snapshot lifetime fix merged in `d0bba7d`. Its handle now
borrows the producing runtime context, preventing cleanup after a worker guard
has freed that context or AutoMemory has reclaimed the allocation. Copy needed
fields into Rust-owned values before leaving the context/lock scope. The field
string remains an independent native owner with its existing requirements.
Compiler controls reject scope escape and early unlock; native workers pass
with AutoMemory off and on. This is separate from the general storage API in
issue 51, and the host needs no source adaptation.

Open SDK design and maintenance work is recorded in
[priority 1 issues](https://github.com/joshrotenberg/redismodule-rs/issues?q=is%3Aissue+is%3Aopen+label%3Apriority%3A1+sort%3Acreated-asc).
The [priority 2 issues](https://github.com/joshrotenberg/redismodule-rs/issues?q=is%3Aissue+is%3Aopen+label%3Apriority%3A2+sort%3Acreated-asc)
cover examples and author documentation; the
[priority 3 issues](https://github.com/joshrotenberg/redismodule-rs/issues?q=is%3Aissue+is%3Aopen+label%3Apriority%3A3+sort%3Acreated-asc)
cover new wasm experiment work. Do not treat design questions as settled APIs.

## The wasm host

`wasm/` is a module that runs WebAssembly components as functions, with the
shape of Redis functions: `WASM.LOAD`, `WASM.CALL`, `WASM.CALL_RO`,
`WASM.LIST`, `WASM.DELETE`, `WASM.KILL`, `WASM.DUMP`, `WASM.RESTORE`,
`WASM.FLUSH` and `WASM.STATS`. Function flags and the checks before a function
starts follow the ones Lua functions have. Guests are in Rust
(`wasm/guests/hello`, on the `wasm/guest-sdk` helper crate) and JavaScript
(`wasm/guests/js`); the same integration tests run against both. Built on
wasmtime 36, the long-term support release whose MSRV fits the pinned 1.94
(later releases need 1.95).

State (wasm/TRACKING.md has each item and the log):

- Done: H1 to H12 and H15 to H19.
- Open: H13, the footprint: the compiler's temporary allocations count in
  `used_memory` with Redis's allocator (peak 1.05 GB loading the JavaScript
  library) and not with the system allocator. Which allocator to use is an
  open decision. H14, CI: nothing runs on this branch, since CI runs on master
  pushes and PRs only.
- Module API gaps G1 to G8 are recorded with what each costs the host.
- Run: the whole suite on macOS with Redis 8.10.2 and on Linux arm64 with
  8.0.2 (JavaScript tests skipped on that Linux run without Node); a smoke
  test of every feature and a two-node cluster on 7.2.16,
  7.4.11, 8.0.6, 8.2.10 and 8.10.2. Redis 7.0 does not load the module, since
  its commands declare `@scripting`. The one-slot rule has not been run on 7.2
  since it moved to `cluster::key_slot`.

Gates on this branch: the SDK gates above, except that the Redis 7.2 Clippy check
excludes the wasm crate. Its SDK dependency enables the default 6.0 floor;
combining it with another floor in the same build fails (S5). The full
workspace Clippy and test runs use the default floor. Then check the guests:

```bash
cargo clippy --workspace --exclude redis-module-wasm --all-targets --no-default-features --features min-redis-compatibility-version-7-2 -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude redis-module-wasm --no-deps --no-default-features --features "all-features-but-xor min-redis-compatibility-version-7-2"
for m in wasm/guest-sdk wasm/guests/hello wasm/bench/guest; do cargo fmt --manifest-path $m/Cargo.toml -- --check; done
cargo clippy --manifest-path wasm/guest-sdk/Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path wasm/guests/hello/Cargo.toml --target wasm32-unknown-unknown -- -D warnings
cargo clippy --manifest-path wasm/bench/guest/Cargo.toml --target wasm32-unknown-unknown -- -D warnings
cargo test --manifest-path wasm/guest-sdk/Cargo.toml
```

The author parser fixture has its own workspace. Check it separately:

```bash
cargo fmt --manifest-path examples/module-testing/Cargo.toml -- --check
cargo fmt --manifest-path examples/module-testing/fuzz/Cargo.toml -- --check
cargo clippy --manifest-path examples/module-testing/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path examples/module-testing/Cargo.toml
```

The compatibility-floor fixture is also an independent workspace:

```bash
cargo fmt --manifest-path examples/compatibility-floors/Cargo.toml --all -- --check
python3 examples/compatibility-floors/check.py
# With the minimum toolchain installed:
python3 examples/compatibility-floors/check.py --toolchain 1.87.0
```

Its negative controls must fail with the SDK's floor-selection diagnostic.
The [testing guide](docs/TESTING-MODULES.md) has the native companion and pinned
fuzz/allocator controls. Run those when changing the fixture or its SDK integration.

`cargo test -p redis-module-wasm` builds the module and the example guests
itself before starting servers. Plain `cargo clippy` and `cargo test` at the
root skip the wasm crate; use `--workspace`.

Keeping the branch current: after each merge to master,

```bash
git switch experiment/wasm-host
git fetch fork
git rebase fork/master
# adapt wasm/ to any SDK change, run the gates, update wasm/TRACKING.md
git push --force-with-lease fork experiment/wasm-host
```

While an experiment PR awaits review, preserve its base ancestry by merging
merged SDK changes instead of rebasing the experiment branch. The INFO guide
sync uses this approach to keep PR 79's reviewed diff intact. That PR remains
unmerged; do not include its proposed workflow in an SDK sync.

Refer to the branch's commits by subject in notes, since a rebase changes
their hashes.

Linux runs: build in the `rust:1.94-trixie` image (run
`rustup target add wasm32-unknown-unknown` inside it, so the target matches the
pinned toolchain), then `docker cp` the module into `redis:<version>`
containers. The benchmark against the same functions in Lua is in
`wasm/bench/`.

## Untracked files

A `local-docs/` directory next to the checkout, if one is copied over, holds
working notes. It is not part of the repository and stays uncommitted; add it
to `.git/info/exclude`.
