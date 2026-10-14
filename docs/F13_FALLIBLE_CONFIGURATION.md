# F13: fallible scalar configuration registration

Numeric, boolean and enum registration now return `Result<(), RedisError>`,
matching string registration. Missing APIs and native rejections are visible.
The macro logs each failure and returns OnLoad ERR; direct callers must propagate
it. Successful defaults, command behavior, tuple syntax, storage/callback traits,
parsers, text/numeric policy and existing LoadConfigs order/result stay unchanged.

## Ownership and native boundary

The private `PendingRegistration` helper resolves API availability before
allocating/transferring a Box. It holds the exact non-null raw allocation pointer
through the native call. Rejection or an unsubmitted guard drops that Box and its
captured callback once. Success disarms the guard, retaining the stable accepted
private data that native callbacks use. No Box is moved/reborrowed after native
stores its pointer; no cached Context or Send/Sync transport is introduced.

`finish` captures errno immediately after native return, before formatting or
callback-data cleanup can change it. Enum strings, values and their pointer array
stay in named locals through the native call; examined hosts copy the table.
Native rejection precedes acceptance and invokes no user getter/setter. The
implementation uses the retained native contract identified in F12; it is not
an audit of arbitrary contexts, callback reentry or user code.

Accepted userdata/closure metadata reclamation remains deferred. F13 fixes the
rejected allocation path, not every configuration allocation. Existing CString/
NUL and malformed enum assertions remain; Result is not a panic-safety promise.
The NonNull construction's unwrap is guaranteed by Box's non-null pointer contract;
API availability no longer uses unwrap.

## API and migration

All three scalar free-function argument lists stay the same. Returning Result
breaks code expecting unit/function-pointer signatures. Semicolon callers can
still compile with unused-result warnings, so they must explicitly handle errors.
Macro tuples need no syntax migration; native registration failure now aborts
load instead of being silently ignored.

Actual isolated stream-filter migration:

```rust
register_bool_configuration(
    ctx,
    "use-iterator",
    &USE_ITERATOR,
    use_iterator,
    ConfigurationFlags::DEFAULT,
    None,
)
.map_err(|error| error.to_string())?;
```

Five scalar calls gain that conversion in its `Result<(), String>` helper,
matching the already migrated string call. JMESPath's four scalar calls use `?`
in a `Result<(), RedisError>` helper; init logs Err and returns `Status::Err`.
Its manual LoadConfigs call/order is preserved. Event-stream tuples and custom
storage stay unchanged. Carolina has no selected configuration migration.

Original modules remain untouched. Isolated event-stream/stream-filter snapshots
start with F11 prerequisites; JMESPath starts with its F09 snapshot. The explicit
F13 migration ledger records before/after hashes and patches. Fresh metadata
verifies all four SDK packages resolve to this checkout. Inherited snapshot
`metadata.stdout` and `migrations.json` describe earlier stages; use
`metadata-f13.stdout`, `f13-migrations.json` and the F13 report for this stage.

## Actual validation

| Check | Result and scope |
|---|---|
| Pure Rust | 29 tests pass, including four new production-helper probes: unavailable API, rejected/unsubmitted callback Drop, stable accepted pointer/owner. No Redis API initialization or fake Context |
| Compiler | 10 metadata-only controls pass: all four Results, explicit String conversion, custom storage/callback, four denied ignored Results, three old-unit E0308 diagnostics |
| Exported API | The same 10 controls pass against the actual archived SDK rlib |
| Examples / Clippy | All 34 examples build debug/release with the 7.2 API floor and runtime bindgen; Clippy all-targets passes with warnings denied; fmt passes |
| Native | 56 owned host/profile/mode cells pass on Redis 7.2.8/8.10.2, debug/release: success plus 13 selected legal rejection modes, each rejected load repeated on a second load |
| Selected behavior | Defaults/module args, GET/SET, native range/custom setter rejection, callback on change and suppression on same value, enum text and typed integer mapping, REWRITE, unload/reload |
| Direct error ownership | Each scalar duplicate/name/flag rejection drops its captured callback once. Each direct-init error follows one actual OnLoad text GET and releases that one owner on load failure |
| Macro lifecycle | Numeric rejection precedes strings; bool/enum duplicate rejection follows accepted strings but precedes defaults/init, so zero cached snapshots is valid. F11's existing load guard closes the pending state |
| References | Six isolated builds and 12 selected host/profile flows pass for event-stream, stream-filter and JMESPath; source heads/status/hashes in all four original directories unchanged |
| Archives | Four fresh archives, three external consumers/six profile builds on 1.94.0 pass. All packaged SDK production-source bytes, including the new helper, match current sources; consumer graph/checksums verified |
| Review | Core and focused validation/migration Due/Claude source reviews approve; caller independently checks result/answer/trace contracts and actual effects |

Reproduce the new native checks after building the example:

```sh
LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib cargo build --locked \
  --examples --no-default-features \
  --features min-redis-compatibility-version-7-2,bindgen-runtime
# Repeat build with --release for release cells.
python3 tools/configuration_registration.py \
  --server 7.2.8=target/f01/servers/redis-7.2.8/src/redis-server \
  --server 8.10.2=/opt/homebrew/Cellar/redis/8.10.2/bin/redis-server \
  --profile debug --profile release --output target/f13/runtime-new
```

Invoke the Python runner as a script; isolated `python -I` invocation is not
validated. Artifact suffix follows the platform, but actual host/profile evidence
is macOS arm64 only and creates no new support/MSRV policy. API-unavailable
coverage is pure helper evidence, not a real missing-API native-host cell.
Archive-consumer runtime was skipped; native evidence belongs to the owned SDK/
reference fixtures. Build-only checks do not establish runtime or ownership.

The authoritative 56-cell run is `target/f13/runtime-corrected/report.json` and
`runtime-corrected.log`; `runtime-final.log` retains the earlier harness failure.
Initial editor/fixture compile errors, counter-reset assumptions, Redis 8.10's
bundled MODULE LIST entries, JMESPath package-name typo and a retained dylib
execute-mode copy omission were corrected; their failed/partial artifacts remain.
No historical invalid native path, crash/leak sampler or fabricated context ran.
A post-review runner comment clarification changes no Python AST/runtime behavior.

## Process and next chunk

Fresh overlap found four fork drafts, 45 upstream PRs and 55 issues, with no
matching scoped registration repair. F13 follows the local issue-independent
branch/no-ff merge process; no push/PR/publication. Existing fork drafts 1–4 and
CI handoff remain deferred. Leaves upstream issue 358 open; its broader string/
context contract and issue 471's unrelated allegation are outside this repair.
Inherited F09 PR487/488 contribution credit remains in its existing ledger.

Next is the additive typed-builder layer from F12: named flags/callbacks and
consuming fallible registration delegating to these same functions. Prefer
consistent `config::integer`, `boolean`, `string`, `enumeration` factory names;
keep explicit integer range, static storage and existing callback shape. Validate
real type inference, compile rejection/migration cases, selected native behavior
and formatted author-code evidence before claiming that proposed savings exists.
No builder implementation is included in F13. CI, accepted metadata cleanup,
general native safety, package identity, MSRV/support and releases stay separate.
