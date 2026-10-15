# F19 — Fallible configuration loading

`configuration::load_configurations(&Context) -> Result<(), RedisError>` now
checks API availability and native status. The macro and selected manual callers
propagate failure at their existing call positions. This implements the reviewed
[F18 specification](F18_LOAD_CONFIGS.md), with no init/default reordering.

```rust
redis_module::configuration::load_configurations(ctx)?;
```

For a helper returning `Result<(), String>`, retain the explicit conversion:

```rust
redis_module::configuration::load_configurations(ctx)
    .map_err(|error| error.to_string())?;
```

## Contract and direct consequences

The private production callable adapter resolves availability and invokes the
native API once per helper invocation. Missing API returns `LoadConfigs API is
unavailable`; non-OK returns `Module configuration loading failed; see Redis
server log`. Native details remain in the server log. No errno attribution,
retry, deduplication, rollback, default cache, dependency, phase token, callback
allocation or stored Context is introduced. The intended phase is real OnLoad;
existing valid Context/native/GIL/thread obligations remain, not a new complete
Rust Context/FFI safety contract.

The macro uses the fully qualified `$crate::configuration` path, logs and returns
ERR while its F11 scope remains pending. Numeric/string/bool/enum registration
still precedes loading; helper commands/events/user init and scope commit still
follow it. Registration rejection is distinct and returns before loading.
Absent configurations block means no generated call. A present empty block
retains its position but deliberately reports a missing API instead of skipping.
Successful default/command/parser/UTF-8/numeric/enum/stored-format policy stays.
Repeated/late defaults and LoadDefaultConfigs remain separate native policies.

On load failure, pending-guard Drop closes cached getter owners before native
removal; user init after a failing macro load is skipped. For manual init, the
Result must reach init's log/Status::Err return. Failed OnLoad does not invoke
user deinit/OnUnload. Ordinary canceled/successful unload and later post-load
forced rejection retain their existing policy. No refusing deinit is executed
on the forced path, which calls deinit under a native assertion.

Selected SDK manual examples use the helper at their prior positions. The F11
ownership fixture adds isolated optional native-load rejection modes and logs,
with its existing real FreeString forwarding instrumentation unchanged. No
LoadConfigs pointer is replaced to simulate absence; absent-API evidence is pure.
The fixture's accounting counts freshly returned getter owners/API releases,
not native refcounts, process heap, static backing or accepted private metadata.
Full callback/closure/static-resource reclamation remains deferred.

Isolated F17-prerequisite stream-filter keeps `Result<(), String>`/map_err;
JMESPath keeps its migrated `Result<(), RedisError>` and init error propagation.
Only their raw load tails/imports change, with fresh metadata-f19 and migration
patch/source/graph records. Domain parser/callback/default bodies remain.
Event-stream tuples/custom storage stay; Carolina has no selected migration.
All original heads/status/135 tracked Rust hashes remain unchanged.

## Caller validation checkpoint

These are caller-owned executions/provenance checks, separate from the approving
independent source review. Selected host evidence is macOS arm64/libc, not a
support matrix or whole-project safety/throughput certification.

- 38 pure library/dependency tests pass, including three new production-policy
  tests for absent API, exact single call/status mapping and propagation with
  retained prior effects. No Redis APIs, native allocator or fake Contexts.
- Ten metadata-only public compiler controls pass against checkout and ten
  against the actual archived SDK rlib: helper signature, RedisError/String
  propagation, external absent/empty/bool macro expansions, raw lifecycle recipe, ignored Result lint,
  missing map_err and wrong-unit diagnostics. Uninvoked bodies never call Redis.
- All 38 examples build debug/release; all-target Clippy with warnings denied,
  fmt and 18 doc tests pass (four ignored).
- Four corrected F19 host/profile cells pass on owned 7.2.8/8.10.2 hosts,
  debug/release, with four actual startup-queue controls and four separate
  F13 native registration-rejection controls. Defaults, setter/change order,
  enum/numeric/text policy, valid LOADEX, queued/default rejection before init,
  later PING, module/config removal and fresh reload are checked. A valid native
  command Context verifies the documented phase error; no fabricated context.
  The present-empty runtime control passes. Macro source plus the absent-block
  compiler control establish no generated call; manual-fixture logs show the
  checked post-registration call, not a trace of every native invocation.
- The selected manual failure shows one issued/live getter snapshot after a
  valid queued string and before a later rejected bool; pending guard releases
  it to zero without OnUnload. Default-only rejection shows zero snapshots.
  Four existing F11 ownership cells pass with updated propagated-error
  expectations, retaining repeated GET/SET, REWRITE/startup, commit,
  canceled/successful unload, image isolation and cached-owner release evidence.
- Six isolated reference builds and 12 selected domain host/profile flows pass;
  eight JMESPath/stream-filter queued range-rejection/removal/PING/reload controls
  additionally verify migrated helper errors. Optional products, full app suites,
  persistence/replication/Cluster and platform expansion are not certified.
- Four fresh archives, three external consumers/six profile builds and
  registration link tests pass; 92 packaged Rust/native/manifest-origin files
  match current source bytes. Metadata/lock/registry/source graphs are fresh;
  task-local compiler/dependency caches are reused. Archive runtime is skipped;
  separate owned-host fixtures supply selected native evidence.

Passing receipts are under `target/f19`: `pure-corrected.log`,
`compiler-complete/report.json`, `archive-compiler-complete/report.json`,
`runtime-final/report.json`, `ownership-runtime/report.json`,
`references/report.json`, `reference-runtime/report.json`, `packages/report.json`,
`archive-source-match.json` and `verification.json`. Native records include owned
PID/version/source/module/server hashes, with source snapshots unchanged during
checks. Native behavior/API release counts are not complete release-balance or
general alias/thread/reentry/panic proofs.

Retained failed attempts are separate: the initial Rust closure capture was
corrected with move; new fixture macro spelling/trailing syntax was corrected;
an initial debug run passed 7.2 but selected a nonexistent 8.10 executable path;
and a metadata-only compiler fixture omitted the macro's required comma.
Final passing reports above use corrected source, syntax and selected executables.
No historical unchecked native path or crash/leak sampler was run.

## Independent review and limitations

One Due/Claude source review approves; true exit 0, 188,970 ms. Request/result/
answer, 141 trace records and offline summary independently validate. All 27
activities complete successfully with no gaps/drops/unmatched events; reviewed
inputs stay unchanged through completion. The reviewer did not run tests, inspect
raw execution/archive receipts, recompute hashes or verify original/remote state.
Those are caller checks. Its one nonblocking evidence-wording clarification above
was applied; production/fixture/runner code stays unchanged. No second review or
unchanged SDK test rerun is needed for that doc-only correction.

The raw custom lifecycle recipe compiles in an uninvoked consumer; no separate
new raw-entrypoint native fixture is claimed. Existing accepted metadata/native
Context/GIL/thread/alias/panic/platform gaps remain. Reference/domain coverage is
selected; the native no-user/template-hash and other prior gaps are unchanged.

Next recommendation is F20, a source-only evidence/priorities checkpoint updating
the F15 scorecard for the new F17/F19 changes and their compatibility effects.
Use existing receipts and actual before/after author snippets; do not rerun
unchanged tests/benchmarks or start a feature/CI/metadata audit as extra F19 work.
Later human focus supersedes that recommendation.

## Scope and process

Fresh overlap snapshot: five fork drafts, 45 upstream PRs, 55 upstream issues;
no matching narrow tracked LoadConfigs issue was found. This chunk uses a local
issue-independent branch from post-F18 `f2dd7ed32bca749eb011f224813fd0b87499437e`.
No SDK push/PR/publication or CI work is included. Existing drafts1–5 stay open;
fork master/GitHub merges/releases remain human-owned.

Configuration API ancestry includes MeirShpilraien's PR229 at refreshed unchanged
head `566ad273797871acc6f50b5345a0c1cfa16fc34b`; its retained diff introduces
optional loading with an ignored return. This is a narrow adaptation to current
API/lifecycle code, not a full port or superseding that broad PR. Preserve
inherited BSD/F02/F09/F17 credits. Issue378's broader Redis 6 CI request and
issue471 remain excluded; existing upstream234/134/246/358/473 stay open.
No CI/MSRV/package/server/platform/support/release policy is selected.
