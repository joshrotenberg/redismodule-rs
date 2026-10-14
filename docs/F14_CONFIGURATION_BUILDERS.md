# F14: typed direct configuration builders

`redis_module::configuration::config` now provides `integer`, `boolean`, `string`
and `enumeration`. These additive builders consume themselves in
`.register(&Context) -> Result<(), RedisError>` and delegate to the existing
fallible functions. Existing free functions, tuple macros and storage traits
remain available. No second native registration or validation engine is added.

## Author code and actual comparison

Actual isolated stream-filter call, compiled and exercised against both selected
hosts:

```rust
config::boolean("use-iterator", &USE_ITERATOR, use_iterator)
    .register(ctx)
    .map_err(|error| error.to_string())?;
```

Required constructor data is name, typed static storage and typed default;
`integer` additionally requires `RangeInclusive<i64>`. Optional `.flags(...)`
replaces DEFAULT flags; `.on_changed(closure)` stores the existing callback shape
and supplies its Box internally. Context is supplied only when registering.

| Real registration call sites | F13 free functions | F14 builders | Fewer lines |
|---|---:|---:|---:|
| Stream-filter, six calls | 62 | 39 | 23 |
| JMESPath, four calls | 60 | 41 | 19 |

Method: the same rustfmt 1.94.0 defaults/edition 2021 formats extracted call-site
function bodies, then counts nonempty body lines, excluding the wrapper. All
error handling, JMESPath callback bodies, names, storage, default expressions,
numeric bounds and the immutable string flag remain. The call fragments are
retained under `target/f14/references/*/calls-before.rs` and `calls-after.rs`;
full migrated helpers actually compile and run. The baseline is F13, after its
five scalar String-error conversions. F12's earlier 57-to-39 figure used the
F11 baseline and an uncompiled `i64` factory proposal. Final spelling is
`integer`, and these are the implemented comparisons.

This is selected call-site source evidence, not whole-helper size, throughput,
allocation count or complete semantic equivalence across all workloads. Removing
`Some(Box::new(...))` syntax does not remove the callback allocation. The factory
and modifier methods only store options; native allocations/conversions occur
through the existing registration functions. No copy/performance saving is claimed.

## Type and ownership contract

- Storage remains `&'static T: ConfigurationValue<...>`; existing custom storage
  and enum traits are preserved. No new callback Send/Sync bounds are added.
- Name and string default are borrowed until registration. Caller-owned text may
  end after the free-function/native copying call; it cannot end before register.
- All four builders are must-use, not Clone/Copy. Register consumes the value;
  Rust rejects reusing that builder. Native duplicate-name policy still applies
  to independently constructed registrations.
- `on_changed` accepts `Fn(&ConfigurationContext, &str, &'static T) + 'static`.
  Replacing/discarding an unregistered callback releases its captured resources
  without invoking it. Accepted native userdata cleanup remains deferred.
- The private common Options type only requires static T; each public concrete
  builder separately enforces its value/storage trait. It carries no Context or
  native registration capability beyond the existing functions.
- Native phase, allocator, callback/context/GIL and raw-entry obligations remain.
  String registration still requires the active SDK load scope, supplied around
  macro user init. Custom entry points retain F11's unsafe lifecycle recipe.
- Flags/default validation and CString/NUL/enum assertion policy remain native/
  existing-function policy. No new panic, general alias/thread or rollback claim.

## Validation and practical limits

| Gate | Actual result |
|---|---|
| Pure tests | 30 pass, including one new callback replacement/discard resource probe with zero invocations and no Context/native API initialization |
| Compiler | 26 metadata-only controls pass: four factory/callback inference cases, consumed-value errors for each type, builder/result must-use checks, nonstatic storage/borrowed callback, wrong storage/default, required range, borrowed name/default, local text/String errors, owned non-Send callback and custom storage |
| Exported API | The same 26 controls pass against the actual archived SDK rlib |
| Docs/examples | 18 doc tests pass, four ignored; all 35 examples build debug/release; Clippy warnings denied and fmt pass |
| Native pairs | 64 backend/host/profile/mode cells (32 pairs) pass across Redis 7.2.8/8.10.2 debug/release; successful observation snapshots match free-function and builder backends |
| Native behavior | Typed defaults, callback on change/suppression on same value, GET/SET, custom setter/native range rejection, enum text/integer mapping, strict UTF-8, immutable flag, REWRITE, ordinary unload/reload and post-commit string admission |
| Error ownership | Each duplicate/name/flag rejection drops the rejected callback once; an actual prior text getter snapshot closes on load failure. Each rejected load is repeated on a second load |
| References | Four isolated stream-filter/JMESPath builds and eight selected native flows pass, preserving F07/F09/F11/F13 prerequisites and original domain/default/LoadConfigs policies |
| Archives | Four fresh archives/three external consumers/six profile builds on 1.94.0 pass; production SDK source bytes match archives, including the new module |
| Review | Independent Due/Claude source review approves; caller separately verifies contracts, actual effects, original heads/status/hashes and measurement/provenance |

Native fixtures forward genuinely initialized host APIs in legal owned OnLoad,
command and unload phases. Only freshly owned text getter objects are counted;
this is not process-heap/leak sampling. Scope is selected macOS arm64 cells, not
a server/platform support policy. Archive consumer runtime was skipped; owned
SDK/reference fixtures supply native evidence. API-unavailable handling delegates
to F13; no new real missing-API host cell was invented.

The first negative compiler expectation used E0277 for the wrong enum default;
the actual typed mismatch is E0308, corrected in retained compiler controls.
The first native string-flags expectation used MEMORY as a rejection. Both retained
hosts pass NUMERIC_CONFIG in RegisterStringConfig flag verification, so that bit
is accepted and masked; the fixture now uses BITFLAGS for ordinary EINVAL.
Builder and free-function policy stay identical. No SDK/server flag-policy change
or broad native audit belongs to F14. Failed attempts remain under `target/f14`;
passing runs are `compiler-corrected`, `runtime-corrected` and `reference-runtime`.

Reproduce paired native checks after building the new example in both profiles:

```sh
python3 tools/configuration_builders.py \
  --server 7.2.8=target/f01/servers/redis-7.2.8/src/redis-server \
  --server 8.10.2=/opt/homebrew/Cellar/redis/8.10.2/bin/redis-server \
  --profile debug --profile release --output target/f14/runtime-new
```

## Process and next checkpoint

F14 is local issue-independent work on its dedicated branch, reviewed and merged
with no-ff; no push/PR/publication. Fresh overlap found four fork drafts, 45
upstream PRs and 55 issues without a matching scoped builder repair. Original
module working directories remain unchanged. Existing drafts 1–4/CI handoff,
accepted metadata reclamation, broader native contracts, MSRV/package identity/
support and release commitments remain deferred. Leaves upstream issue 358 open;
issue 471's unrelated allegation is excluded. F09 inherited attribution remains.

Next recommendation is one source-only F15 evidence/priorities checkpoint: gather
what F02–F14 actually improved against the three project goals, list explicit
remaining gaps and propose the next bounded SDK slice. Use existing evidence;
do not repeat passed tests/reviews or automatically start another feature/CI/
general native audit. This closes the requested initial repair/ergonomics sequence
with a reviewable view of the whole fork before choosing more implementation.
