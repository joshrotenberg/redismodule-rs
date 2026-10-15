# redis-module improvement plan

Inventory and plan for the fork effort at joshrotenberg/redismodule-rs, written
2026-10-15. Baseline: upstream master a808377 (2026-09-27), which is also fork
master. Upstream is read-only for this effort: no PRs, issues, comments, pushes,
or crates.io publishing.

The goal is to fix what is broken in the current crate and make sensible
improvements, breaking ones included. The ranking lens is a later Rust module
that hosts wasm guests (Lua-like scripting for any language). That module is not
part of this effort.

Contents:

1. Current state
2. Findings
3. Upstream open issues (55)
4. Upstream open PRs (45)
5. F-series and fork PRs #1 to #5
6. wasm-host coverage
7. Fix sequence
8. 3.0 scope versus patch fixes
9. Decisions

## 1. Current state

### Crates

| crate | version | path |
|---|---|---|
| `redis-module` | 2.0.8 | `.` |
| `redis-module-macros` | 2.0.8 | `redismodule-rs-macros/` |
| `redis-module-macros-internals` | 2.0.8 | `redismodule-rs-macros-internals/` |
| `redis-module-common` | 0.1.1 | `common/` (path dependency, so an implicit workspace member) |

- Edition 2021. `rust-toolchain.toml` pins 1.94.0. No `rust-version` (MSRV) is declared.
- `build.rs` compiles `src/redismodule.c` with `cc` and runs bindgen over the vendored `src/include/redismodule.h` on every build. Every build needs a C compiler and libclang.
- The vendored header was last changed by upstream #486 (open-files reservation API) and #478 (BigRedis swap-prefetch API). It carries BigRedis extension APIs that OSS Redis does not have (43ac423).
- 27 examples, each a `cdylib`. `tests/integration.rs` starts one `redis-server` per test on sequential ports from 6479 and loads an example.

### Feature flags

- `default = ["min-redis-compatibility-version-6-0", "bindgen-runtime"]`.
- `min-redis-compatibility-version-{6-0,6-2,7-0,7-2,7-4,8-0}`: mutually exclusive API floors. They gate wrappers at compile time; they do not check the running server.
- `bindgen-runtime` (load libclang dynamically) and `bindgen-static` (link `libclang.a`). One of the two is required.
- `all-features-but-xor`: used only by the CI doc step.

### Gates on master a808377

Local: macOS arm64, Redis 8.10.2 (Homebrew, `malloc=libc`).

| gate | 1.94.0 (pinned) | 1.99.0 (Homebrew stable) |
|---|---|---|
| `cargo fmt --all -- --check` | pass | pass |
| `cargo clippy --all-targets -- -D warnings` | pass | pass |
| `cargo test` unit | 6 pass | 6 pass |
| `cargo test` integration | 25 pass, 1 fail | 25 pass, 1 fail |
| `cargo test` doc | 3 pass, 4 ignored | 3 pass, 4 ignored |
| `cargo doc --no-deps` | 8 warnings (unresolved or private links) | not run |

Per feature row (pinned, `cargo clippy --workspace --all-targets --no-default-features --features min-redis-compatibility-version-<v>,bindgen-runtime -- -D warnings`):

| floor | 6-0 | 6-2 | 7-0 | 7-2 | 7-4 | 8-0 |
|---|---|---|---|---|---|---|
| result | pass | pass | pass | pass | pass | fail: `BlockingCallOptions.options` never read |

- The failing integration test is `test_redis_value_derive`. The `proc_macro_commands` example declares a key spec on a keyless command, and Redis 8.10.2 crashes on it (SIGBUS in `getKeysFromCommandWithSpecs`). F04 fixes the example.
- `bindgen-static` fails locally because no `libclang.a` is installed. Expected; not a crate defect.

### Supported Redis versions and CI

- `.github/workflows/ci.yml` runs on `pull_request` only. The matrix is ubuntu and macos times feature floors 6-0, 6-2, 7-0, 7-2, and every row runs a Redis 7.2 server built from source. No Redis 6 or Redis 8 runtime, and no 7-4 or 8-0 build row.
- CI clippy runs without `--all-targets` and without `-D warnings`.
- `release.yml` runs release-plz on every push to master. `release-pr.yml` is manual. On the fork, Release-plz is `disabled_manually`, so merging into fork master does not publish.
- Release state upstream: crates.io and the `redis-module-v2.0.8` tag are at 2.0.8 (2026-05-05). The latest GitHub release is `redis-module-macros-v2.0.8`. Legacy tags `v2.1.0` to `v2.1.3` are not ancestors of master and were never published.

### Consumers

- RedisGears and LibMR follow upstream `master` by git. RediSearch pins a master commit. RedisJSON pins tag `v2.1.3`.
- crates.io: about 820K total downloads, about 20K in the last 90 days, five small reverse dependencies.
- Breaking changes in the fork affect none of these until the fork's direction is decided.

### Local environment notes

- A Homebrew `cargo` and `rustc` 1.99.0 sit ahead of `~/.cargo/bin` on PATH, which silently overrides `rust-toolchain.toml`. Even rustup's `cargo` calls the Homebrew `rustc` unless `~/.cargo/bin` comes first. Pinned runs here used `PATH=$HOME/.cargo/bin:$PATH`.
- `cargo test --test integration` does not rebuild examples. Run `cargo build --examples` first, or run plain `cargo test`.

## 2. Findings

A1 to A6 come from the klogmini study. A7 to A15 were found in this inventory. Each was checked against master a808377.

| id | finding | where | status |
|---|---|---|---|
| A1 | `RedisAlloc` ignores alignment above 16 bytes; misaligned `Box` is UB, and a debug build aborts the server | `src/alloc.rs` | fixed in fork PR #7; upstream #483 is the same scheme |
| A2 | `get_value` returns "Existing key has wrong Redis type" instead of the standard WRONGTYPE error | `src/key.rs:721` (`verify_type`) | open; `RedisError::WrongType` already exists |
| A3 | No order-preserving map reply: `Map` is a `HashMap`, `OrderedMap` is a sorted `BTreeMap` | `src/redisvalue.rs:33-36` | open; no PR |
| A4 | `cargo test` cannot run module logic in-process: `RedisAlloc` aborts when `RedisModule_Alloc` is unset, and `RedisString::create` unwraps a missing API | `src/alloc.rs`, `src/redismodule.rs:158` | open; issues #89, #221 |
| A5 | Native type name length (9) is checked at load; command flags are a raw string, so a typo fails at load | `src/native_types.rs:35`, `src/macros.rs:16` | open; issues #365, #332, #330 |
| A6 | Every build needs libclang (bindgen runs in `build.rs`; the features only choose dynamic or static) | `build.rs`, `Cargo.toml` | verified; issue #150 |
| A7 | No `catch_unwind` at any `extern "C"` boundary: a panic in a command, notification, config or post-notification callback aborts redis-server | `src/macros.rs:20-30` and all callback trampolines | fork PR #17 |
| A8 | Logging passes the message as the printf format string, so `%s` or `%n` in logged data is UB; an interior NUL panics | `src/logging.rs:41-43,55-61` | fork PR #9 (F02); upstream #476 fixes only the NUL |
| A9 | 155 `RedisModule_*.unwrap()` call sites abort the server when the running Redis lacks an API | `src/**` | fork PR #18; issue #287 |
| A10 | `call_blocking` and `build_blocking` disappear under the 8-0 floor: the cfg lists 7-2 and 7-4 only | `src/context/mod.rs:145,453` | fork PR #16 |
| A11 | `KeysCursor` scan retains each key name and then `mem::forget`s it: one leaked string per scanned key | `src/context/keys_cursor.rs:20,34` | fork PR #13 (F09) |
| A12 | `RedisString::create` and `Context::create_string` go through `CString::new(..).unwrap()`: panic on NUL, not binary safe | `src/redismodule.rs:156` | open; issue #134 |
| A13 | `str_as_legal_resp_string` maps each `char` with `c as u8`, truncating non-ASCII text in simple strings and errors | `src/context/mod.rs:471` | open |
| A14 | `unsafe impl Sync for RedisType` while it holds a `RefCell` (non-atomic borrow flag) | `src/native_types.rs:11,16` | open; low risk while the GIL serializes access |
| A15 | String configuration getters leak one `RedisModuleString` per CONFIG GET | `src/configuration.rs` | fork PR #14 (F11 reworked) |

## 3. Upstream open issues (55)

Categories follow the brief: bug (soundness marked), feature, devex, obsolete, duplicate.
Dispositions: `patch` (fixable without an API break), `3.0` (needs a break), `later`
(valid, low priority), `close` (with the reason). Overlap names F-series steps, A ids,
and upstream PRs.

Counts: feature 21, devex 21, bug 8 (2 of them soundness), obsolete 4, duplicate 1.
Dispositions: patch 15, 3.0 6, later 18, close 16.

| # | title | category | status on master | overlap | disposition |
|---|---|---|---|---|---|
| 7 | Command params should be borrowed | devex | partly: args are `Vec<RedisString>` since #157 | F05 | close (F05 `ArgCursor`) |
| 54 | HashGet/HashSet wrappers | feature | fixed: `hash_get`, `hash_set`, `hash_get_multi` | | close |
| 55 | Improve thread-safe context and locks | obsolete | empty body; API reworked since | | close |
| 61 | Access to blocking client | feature | partly: `block_client`, `call_blocking`; no reply or timeout callbacks, no BlockClientOnKeys | A10 | later |
| 69 | Improve developer docs | devex | valid | | later (docs umbrella) |
| 74 | EmitAOF wrapper | feature | valid: raw only | | later |
| 79 | Aux load/save | feature | partly: raw type methods only | | later |
| 89 | cargo test panics with redis_module dep | devex | partly | A4 | patch |
| 92 | Document init | devex | partly | | patch (macro docs) |
| 134 | Binary-safe data | feature | partly: key `write(&str)`, hash field `&str`, `save_string(&str)`, `create` NUL panic remain | A12, F05, F17, PR 247 | patch |
| 150 | Building needs libclang | devex | valid | A6 | patch |
| 173 | ctx.call GRAPH.QUERY crash | obsolete | Redis-side blocking limitation; not re-run | | close |
| 175 | RedisJSON load fails on Redis 5 | obsolete | Redis 5 unsupported; not re-run | A9 | close |
| 205 | Safe fns deref raw pointers | bug: soundness | valid: 81 `not_unsafe_ptr_arg_deref` allows, `raw` public and glob re-exported | F07, F09 (partial) | 3.0 |
| 206 | mem_usage2 | feature | partly: raw only | | later |
| 221 | RedisString::create panics in cargo test | devex | valid | A4 | later |
| 226 | Load fails after adding avro-rs | obsolete | user environment, resolved | | close |
| 234 | Context::call returns SimpleString | bug | valid: CR/LF corrupts RESP, NUL panics in `reply()` | F17, PR 93 | patch (reply side) or 3.0 (F17 variant change) |
| 235 | Sorted set API | feature | valid | | later |
| 236 | Cluster API | feature | valid: raw only | | later |
| 237 | Schema evolution question | devex | question | | close (fold into #69) |
| 246 | Binary key read/write | feature | partly: `write(&str)` remains | PR 247 | patch |
| 251 | key.delete always OK | bug | valid: native result ignored | | 3.0 (or additive method) |
| 260 | arm64 i8/u8 mismatch | bug | fixed: `c_char` throughout | | close |
| 271 | save_string binary safe | feature | fixed: `save_slice` (#302) | | close |
| 275 | Remove key expiration | feature | fixed: `remove_expire` (#276) | | close |
| 287 | unwrap to expect | devex | valid: 155 unwraps | A9 | patch |
| 295 | API availability validation | devex | partly: `api!` macro, floors | A9 | later |
| 303 | CallOptions as bitflags | devex | valid | | later |
| 328 | Drop `Redis` prefix from names | devex | valid | | 3.0 (not proposed, see section 8) |
| 330 | Compile-time key spec validation | feature | valid | A5 | later |
| 331 | Sanitizer CI | devex | valid | | later |
| 332 | Command flags as bitflags | devex | partly: proc macro typed, `redis_module!` string | A5 | 3.0 |
| 351 | Separate detached and thread-safe context types | devex | valid: guards deref to full `Context` | | 3.0 |
| 358 | Refactor RedisString API | bug: soundness | valid: stored ctx, unbounded `try_as_str` lifetime | F06 to F09 | 3.0 |
| 363 | Access Context from a trait | devex | partly: `DetachedContext` answers it | | close |
| 365 | Type name must be 9 chars | devex | partly: length checked, charset not | A5 | patch |
| 378 | Redis 6 in CI | devex | valid: every row runs 7.2 | | patch |
| 379 | Set API | feature | valid | | later |
| 380 | Windows support | feature | valid: `std::os::unix`, `nix` | | later |
| 381 | RegisterAuthCallback | feature | valid: raw only | | later |
| 382 | RdbLoad/RdbSave | feature | partly: raw only | | later |
| 383 | CLIENT_CHANGE event | feature | valid | | later |
| 390 | Replica model upload question | devex | question | | close |
| 398 | GetCommand missing on Redis 6 | bug | fixed by #403; no Redis 6 runtime CI | | close (after #378) |
| 404 | Add Redis 6 CI | duplicate | duplicate of #378 | | close |
| 411 | Redis as HTTP server question | devex | question | | close |
| 415 | Publish to crates.io | devex | valid: v2.1.x unpublished | PR 474, PR 466 | patch (upstream release work) |
| 416 | rust-analyzer workaround | devex | valid: `///` inside macro matcher | PR 427 | patch |
| 438 | SendClusterMessage | feature | valid: subset of #236 | | later |
| 441 | Missing NOTIFY events | feature | valid | PR 442 | patch |
| 443 | LRU/LFU APIs | feature | valid | PR 444 | patch |
| 445 | RandomKey | feature | valid | PR 446 | patch |
| 471 | c_void unsoundness claim (enum config) | bug | claim not substantiated (Redis copies option names); residual: ignored return, leaked privdata | F13 | close |
| 473 | Logging panics on NUL | bug | valid | A8, F02, PR 476 | patch |

Clusters: Redis 6 support (#378, #404, #398, #175, #295); binary safety (#134, #246,
#271, #234); in-process tests (#89, #221); compile-time validation (#330, #332, #365);
raw-only gaps with open PRs (#441/442, #443/444, #445/446); questions (#237, #363, #390,
#411). No open issue covers A1, A2, A3, A7, A10, A11, A13, A14, A15, F03 or F04.

## 4. Upstream open PRs (45)

Verdicts: `land` (good as is or nearly), `rework` (wanted, needs changes), `harvest`
(take the idea, rewrite), `close` (with the reason). "Land" here means "worth taking";
in the fork that means porting the change, since upstream merges are not ours to make.

Counts: feature 15, devex 12 (including 2 release), obsolete 10, bug 6 (1 soundness),
duplicate 2. Verdicts: land 7, rework 13, harvest 5, close 20.

| # | title | category | verdict | note |
|---|---|---|---|---|
| 488 | ServerInfo field string leak | bug | land | approved upstream; F09 carries the same fix |
| 484 | WBM budget header APIs | feature | rework | depends on a private Redis PR; body and diff disagree |
| 483 | RedisAlloc align > 16 | bug: soundness | rework | A1; changes requested 2026-08-19 (test isolation, RediSearch run); same scheme as fork PR #7 |
| 482 | try_scan/try_for_each | feature | rework | copies the A11 key-name leak |
| 477 | Shutdown server event | feature | land | no test; registration should use F03's emitter |
| 476 | Logging total on NUL | bug | rework | A8 format-string bug remains; F02 supersedes |
| 474 | Release line 2.2.0 | devex (release) | rework | rebase; upstream release work, out of fork scope |
| 469 | RedisKey::name | feature | rework | frees the borrowed key name one time too many |
| 467 | std::error::Error for RedisError | feature | rework | breaking; 3.0 candidate |
| 466 | Publish dry-run CI | devex (release) | land | installs libclang, does not remove A6 |
| 459 | KeyFlags::ACCESS_TRIMMED | obsolete | close | landed as bc94ab6 |
| 452 | Complete redismodule.h | devex | close | OSS part landed in cb1464e; rest is from an unmerged Redis branch |
| 446 | Context::random_key | feature | rework | leaks one reference with auto-memory off |
| 444 | LRU/LFU key APIs | feature | rework | stacked on #442 and #437; rebase alone |
| 442 | NOTIFY OVERWRITTEN/TYPE_CHANGED | feature | rework | stacked on #437; keep only the flags |
| 437 | Command filter bindings | feature | rework | unbounded `&str` from `from_ptr`; needs F07-style borrows |
| 431 | Remove crate-owner workflow | obsolete | close | landed as 927bc5c |
| 427 | `///` to `//` in macro matcher | devex | land | fixes #416 |
| 420 | Enum repr isize to i32/u32 | devex | harvest | 3.0 raw-type cleanup |
| 417 | README modules API link | devex | land | one line |
| 405 | decode_args null guard on 1.0.1 | obsolete | close | dormant branch; master fixed by 0bb8c12 |
| 400 | Update headers | obsolete | close | superseded by cb1464e |
| 397 | Context::dbsize | feature | land | four lines |
| 391 | get_expire/get_abs_expire | feature | rework | return `Option<Duration>` like `set_expire` |
| 386 | Derive Eq/Ord/Hash for RedisAlloc | feature | land | no behavior change; touches `src/alloc.rs` |
| 375 | Deps plus `redis_call!` FFI refactor | devex | harvest | private `raw` for 3.0 |
| 374 | CI spellcheck, pants | obsolete | close | superseded by #428 CI |
| 368 | version_from_info rewrite | bug | harvest | guard empty captures (index panic on master) |
| 361 | bindgen 0.68 | devex | close | dependabot; bindgen is 0.72.1 |
| 360 | nix 0.27 | devex | close | dependabot; one fresh deps PR instead |
| 354 | Module users | feature | rework | rebase; user Drop disconnects clients |
| 348 | strum_macros 0.25 | devex | close | dependabot; one fresh deps PR instead |
| 336 | OnLoad ctx/argv checks | obsolete | close | landed as 0bb8c12 |
| 325 | Publish test workflow | obsolete | close | replaced by release-plz and #466 |
| 247 | Bytes-accepting key methods | feature | harvest | byte `hash_set` and `write` |
| 245 | Make key_inner pub | obsolete | close | superseded by `to_raw_parts` |
| 240 | ERR prefix on all errors | bug | close | double-prefixes WRONGTYPE and custom codes |
| 229 | API extensions umbrella | obsolete | close | split and landed |
| 228 | Dictionary stub | duplicate | close | duplicate of #169 |
| 193 | Codecov workflow | devex | close | stale |
| 169 | Dictionary support | feature | close | no demand since 2021; Drop leaks values |
| 168 | Command filter (2021) | duplicate | close | duplicate of #437 |
| 100 | hash_get! macro | obsolete | close | superseded by `hash_get_multi` |
| 93 | Call string reply as BulkString | bug | harvest | F17 is the rewrite |
| 85 | enum-primitive-derive 0.2 | devex | close | dependabot |

Ownership bugs in open PRs were found by reading source, not by running them:
#469 over-release, #446 leak, #482 inherits A11.

Breaking PRs: #467 (`RedisError` shape), #420 (raw enum reprs), #375 (private `raw`),
#452 and #400 (remove `init_api`), #240 (wire text), #93 and F17 (reply variant).

## 5. F-series and fork PRs #1 to #5

`fork/fix/f17-call-reply-bulk-strings` is a linear stack of 44 commits directly on
a808377: 19 `--no-ff` merges, 5 empty kickoff commits, 8 spec-only commits,
2 infrastructure commits (M01 crate move to `crates/`, F01 test harness and Python
tools), and 10 code commits. About 70% of its +21.5K lines are docs, evidence JSON,
AGENTS and STATUS files, and Python tools. Production code is about +1.8K/-250.

### Verdicts

| item | verdict | reason |
|---|---|---|
| #1 F02 logging | keep | fixes A8 (format-string UB) and the NUL panic; supersedes upstream #476 and `fork/fix/logging-nul-panic` |
| #2 F07 string views | keep | removes the unbounded `try_as_str` lifetime and the `next_str` use-after-free; lands with F05 |
| #3 F09 string ownership | keep | `RedisString` without a stored ctx; fixes A11, the ListPop and ServerInfo leaks, a StreamIterator double free, a `&F` to `&mut F` cast, a NULL set value; follow up with a null-argv guard and `from_bytes` in serde and `create_string` |
| #4 F11 config getter | rework | fixes A15, but adds a 386-line thread-local load-scope registry and makes string registration fail outside it; caching the owner in the existing private data does the same in about 20 lines |
| #5 F17 call replies | keep | byte-exact bulk forwarding; fixes #234; port the Python framing checks into `tests/integration.rs` |
| F03 macro hygiene (no PR) | keep | modules no longer need a direct `linkme` dependency |
| F04 proc fixtures (no PR) | keep | fixes the example that crashes Redis 8.10.2 |
| F05 ArgCursor (no PR) | keep | additive borrowed argument parsing |
| F13 fallible registration (no PR) | keep | surfaces silent registration failures; returns `Result` (break) |
| F14 config builders (no PR) | defer | optional sugar; decide after the configuration API settles |
| M01 workspace layout | drop | directory moves and fork URL, no functional change |
| F01 harness and tools | drop | the port-0 race in `tests/utils.rs` causes every red CI job on #1 to #5 (failure counts equal "port occupied" or "reset by peer" errors) |
| spec and process docs | drop from tree | harvest the tables below |

### Base branch

Recommendation: base new work on upstream master and re-land the code as small PRs.

- The stack needs no rebase, but it carries the docs weight, the flaky harness, and the M01 moves.
- All 10 code diffs apply to master with conflicts only in `[[example]]` entries and about 10 harness lines (`--logfile` and `server_log()` for F02's tests).
- Master plus the code-only diffs: fmt and `clippy --all-targets -D warnings` pass; 29 unit, 18 doc, 37/37 integration, stable across 6 parallel runs (pinned 1.94.0, Redis 8.10.2).
- The F17 head: 46 integration tests pass serially, but a parallel run is clean in only 3 of 9 attempts.

### Harvest

- The F08 ledger: for each Redis API that returns a string, whether it is borrowed or owned, auto-memory registered or not, and whether to adopt or retain.
- Retained argv is trimmed when the command returns. Hold the owner across commands, never a byte view.
- `Context::new` and `Context::dummy` are safe constructors, so a `&Context` "GIL token" proves nothing.
- Status and bulk call replies both report `REDISMODULE_REPLY_STRING`. A Lua-like `redis.call` that keeps `{ok=...}` needs `RedisModule_CallReplyProto`.
- Key specs reach Redis unvalidated; an invalid spec crashes Redis 8.10.2. Validate at registration.
- Tests: F02 unit and log integration tests, F05/F07 `compile_fail` doctests, F09 integration tests, F17 unit tests.

### Other fork branches

| branch | state | disposition |
|---|---|---|
| `fix/logging-nul-panic` | upstream #476 | superseded by F02 |
| `chore/release-line-2.2.0` | upstream #474, stale | upstream release work; out of fork scope |
| `modernize-release-process` | no PR | superseded by upstream #428 |
| `fix-cratesio-publish` | upstream #402, closed | obsolete |

## 6. wasm-host coverage

| surface | crate today | gap | existing work |
|---|---|---|---|
| `RedisModule_Call` (guest `redis.call`) | `Context::call`, `call_ext`, `CallOptionsBuilder` with W, S, C, M, E, !, 0/3, K; typed `CallResult` | string replies become `SimpleString` (#234); no status versus bulk distinction; flags built as a string; no D (dry run), A or R flags | F16, F17, #303 |
| effect replication | `replicate`, `replicate_verbatim`, call flag `!` | none found | |
| thread-safe contexts | `ThreadSafeContext`, `DetachedContext`, `RedisGILGuard` | guards deref to the full `Context` including reply APIs; no TryLock; safe `Context::new` makes the GIL token forgeable | #351, F-series harvest |
| blocked clients | `block_client`, `call_blocking` with an unblock handler | no reply or timeout callbacks, no privdata, no BlockClientOnKeys; `call_blocking` missing under the 8-0 floor | #61, A10 |
| `RedisModule_Yield` | none | needed to keep the server responsive during long guest runs | none |
| ACL checks | `acl_check_key_permission`, `get_current_user`, call flag C | no ACLCheckCommandPermissions, ChannelPermissions or ACLAddLogEntry wrappers | #354, #381 |
| memory accounting | none | no MallocSize, MallocUsableSize or GetUsedMemoryRatio wrappers; guest linear memory is mmapped outside `RedisAlloc`, so `used_memory` and `maxmemory` do not see it | #206 |
| allocator | `RedisAlloc` as `#[global_allocator]` | A1 (fixed in PR #7); A4 | #483 |
| panics across FFI | none | A7: a guest trap that surfaces as a Rust panic aborts the server; A9 adds 155 abort sites | #287 |
| guest-held strings | `RedisString` | stored ctx and unbounded lifetimes | F07, F09, #358 |
| event loop | none | no EventLoopAdd wrapper | none |
| command filters | none | | #437 |

## 7. Fix sequence

The F-series code is re-landed as fork PRs #8 to #15 (decisions 1 and 2 in section 9).
Each row is one PR, in merge order. Stacked PRs name their base; retarget each one to
`master` after its base merges, since the fork does not delete merged branches.

| step | change | fork PR | base | breaking |
|---|---|---|---|---|
| 1 | F04: `proc_macro_commands` key spec | #8 | master | no |
| 2 | F02: logging format string and NUL (A8) | #9 | master | no |
| 3 | F03: macro `linkme` re-export | #10 | master | no |
| 4 | F17: call string replies as bulk strings | #11 | master | behavior |
| 5 | F05 `ArgCursor` and F07 string views | #12 | master | yes |
| 6 | F09: string ownership (A11) | #13 | #12 | yes |
| 7 | F11 reworked: string configuration getter leak (A15) | #14 | #13 | no |
| 8 | F13: fallible configuration registration | #15 | #14 | yes |
| 9 | A1: allocator alignment | #7 | master | no |
| 10 | A10: `call_blocking` under the 8-0 floor; 7-4 and 8-0 CI rows | #16 | master | no |
| 11 | A7: catch panics at FFI boundaries | #17 | master | no |
| 12 | A9: name the missing API instead of unwrapping `None` | #18 | master | no |

Merged into master in this order, steps 1 to 10 merge without conflicts. Steps 11
and 12 merge cleanly on master alone, but after steps 1 to 8 they conflict where
F03, F09, F11, and F13 rewrote the same lines (10 and 17 hunks). Rebase them last;
fork PR #18 carries the script that regenerates its change. Merging them first
instead moves the conflicts into five F-series PRs. Steps 1 to 9 merged together
pass `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
29 unit, 42/42 integration, and 17 doc tests (46/46 integration on the 7-2 floor), on
1.94.0 against Redis 8.10.2. F14 (configuration builders) stays deferred.

Next, not started:

1. Port upstream #427, #417, #397, #386, #477. The #488 ServerInfo fix is part of fork PR #13.
2. A12, A13: binary-safe `RedisString::create`; non-ASCII-safe simple strings and errors.
3. A4: opt-in test allocator feature that falls back to `System` only in test builds, with a hard failure in a loaded module.
4. A2 WRONGTYPE; A3 insertion-ordered map reply; A5 const type-name check and typed command flags.
5. `RedisModule_CallReplyProto` for status versus bulk in call replies.
6. wasm-host wrappers: Yield, ACLCheckCommandPermissions, memory-size APIs, blocked-client callbacks, EventLoopAdd.
7. CI: run on push to master, MSRV declaration, sanitizer job (#331), Redis 6 server rows (#378).

## 8. 3.0 scope versus patch fixes

Patch level (no public API break; some change wire behavior and need a release note):

- A1 allocator, A7 panic containment, A8 logging, A9 missing-API errors, A10 cfg, A11 and #488 leaks, A12, A13, A15 (reworked F11).
- F03, F04, F05, and ported upstream additions (#397, #427, #417, #386, #477).
- A4 test allocator (additive feature) and A5 const type-name check (bad names fail at compile time instead of load).
- A2 WRONGTYPE: no API change, but the wire text changes.

3.0 (breaking):

- F07 and F09: `RedisString` lifetimes and ownership; `inner` private; `decode_args`, `RedisString::new`, `from_ptr` unsafe; `get_current_user` returns `Option` (#358, part of #205).
- F17: call string replies become `BulkString` (#234).
- F13: configuration registration returns `Result`.
- #205: the remaining safe functions over raw pointers become `unsafe` or private; `raw` stops being glob re-exported (#375 harvest).
- #351: split detached and thread-safe context types; remove the forgeable GIL token.
- A3: an insertion-ordered map variant in `RedisValue` (new variant breaks exhaustive matches).
- A5 and #332: typed command flags in `redis_module!`.
- A14: `RedisType::raw_type` stops being a public `RefCell`.
- A6: pre-generated bindings as the default, with bindgen behind a feature.
- #467 `RedisError` as `std::error::Error`; #420 raw enum reprs; #251 delete result.

Not proposed: #328 (drop the `Redis` prefix) is churn for every consumer with no behavior gain.

## 9. Decisions

Decided 2026-10-15:

1. Base branch: master. The F-series code is re-landed as small PRs (fork PRs #8 to #15).
2. F-series PRs: fork PRs #1 to #5 are closed, each replaced by fork PR #9, #12, #13, #14, and #11.
5. Panic policy (A7): reply with an error and keep running (fork PR #17).

Open:

3. Allocator: keep fork PR #7's implementation, or take upstream #483's `src/alloc.rs` and keep fork PR #7's live test and defrag note. Both are correct.
4. 3.0 scope: confirm the breaking list in section 8, and whether A2's wire change ships as a patch or in 3.0.
6. Upstream sync: how often fork master merges upstream master (for example when #488 or #483 lands).
