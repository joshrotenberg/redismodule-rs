# F12: configuration registration ergonomics

This is a source-reviewed proposal, not an implemented API. F12 changes only
documentation. Proposed signatures/builders and migrations have not been
type-checked, compiled or run against Redis. F11 remains complete.

## Recommendation and implementation order

Make all four registration functions report native rejection consistently, then
add thin typed builders for direct registrations. Keep the existing compact
`redis_module!` configuration tuples and custom `ConfigurationValue` traits.
These are two bounded implementation chunks within one registration proposal:

1. **F13:** return `Result<(), RedisError>` from numeric, boolean and enum
   registration, propagate failures through the macro, and reclaim only rejected
   private data. Migrate selected direct callers in isolated snapshots.
2. **Later builder chunk:** additive named options, an explicit numeric range,
   and `.on_changed(closure)` convenience, delegating to those same functions.
   Complete F13 validation before starting this ergonomic layer.

The first stage deliberately changes failure behavior: rejected registration
must no longer disappear behind a unit return. Successful command protocols,
defaults, custom parsers, registration order, stored formats and the existing
`LoadConfigs` call/result policy remain. No configuration batching, macro grammar
rewrite, new callback trait or general lifecycle audit is proposed.

## Actual author patterns

The inventory covers 214 tracked Rust files across the SDK and four references.
Literal selected-syntax searches plus focused reads are not an exhaustive AST
or downstream compatibility inventory. Exact heads, hashes and excerpts are
retained locally under `target/f12`.

| Author pattern | Evidence | Consequence |
|---|---|---|
| SDK macro example | `examples/configuration.rs`: nine tuples (2 numeric, 3 string, 2 boolean, 2 enum), shared callback | Preserve tuples; they already compress declarations well |
| Event stream | `src/lib.rs:534–606`: 20 tuples (7 numeric, 7 string, 4 boolean, 2 enum); seven custom string storage implementations in `src/config.rs` | Preserve storage traits, domain setters, defaults and existing callback bodies |
| Stream filter | `src/config.rs:131–205`: six direct registrations (4 numeric, 1 boolean, 1 string), custom argument/default validation, helper returns `Result<(), String>` | Target positional options and ignored scalar errors; retain parsing and explicit error conversion |
| JMESPath | `src/config.rs:54–132`: four direct registrations (2 numeric, 2 boolean), four boxed change closures; helper currently returns unit, init calls it at `src/lib.rs:91–94` | Make helper fallible and handle it in init; hide boxing syntax without claiming fewer allocations |
| Carolina | Actual module is `crates/klog-module`; no selected configuration registration matches | No selected configuration migration |

All original module directories remain unchanged. The comparison uses the F11
isolated stream-filter snapshot, which already carries the necessary F07/F09/F11
migrations. Its macro has no configurations block; its manual `LoadConfigs` is
the first call. JMESPath's current helper has its own manual `LoadConfigs` too.
Do not invent a common exactly-once load order or silently fix repeated-load
behavior while changing registration.

Current SDK `src/configuration.rs:236,412,480` calls native numeric/bool/enum
registration and discards its status; the functions return unit. Their macro
emitters (`src/macros.rs:394,429,445`) consequently cannot propagate errors.
String registration at `src/configuration.rs:337` already returns `Result` and
the string macro emitter handles it. F13 makes that contract consistent.

## Stage one: explicit failure contract

Proposed signatures, **not compiled** (arguments otherwise unchanged):

```rust
pub fn register_i64_configuration<T: ConfigurationValue<i64>>(
    /* existing arguments */
) -> Result<(), RedisError>;
pub fn register_bool_configuration<T: ConfigurationValue<bool>>(
    /* existing arguments */
) -> Result<(), RedisError>;
pub fn register_enum_configuration<G: EnumConfigurationValue, T: ConfigurationValue<G>>(
    /* existing arguments */
) -> Result<(), RedisError>;
```

Resolve API availability before `Box::into_raw`, matching F11's string path;
keep the private-data pointer and enum name-pointer Vec in named locals alive
through the native call. Check availability without `unwrap`, return an error, and
capture native errno immediately on an ordinary native rejection, before any
cleanup can change it. Reconstruct/drop the rejected private-data Box including
its captured callback. Successful registration keeps the existing stable
private-data allocation; reclaiming accepted metadata is deferred. No blanket
Send/Sync or new string registry is needed for scalar-only registration.

Native rejection precedes creation/list insertion/private-data acceptance in the
examined hosts. Neither registration invokes the user getter/setter on that path.
Keep enum name/value temporary arrays alive until the native call returns: the
host copies both before returning. This is a narrow source prerequisite for
rejection cleanup, not a certificate for arbitrary raw contexts or enum tables.

Inside the c_int-returning generated OnLoad, use the existing string pattern:
match/`if let Err`, log, then return native ERR. Do not emit `?` there. Preserve
the F11 load guard so any earlier string snapshots close on that error return.
Keep the success path and native `LoadConfigs` result/order unchanged. Direct
custom OnLoad callers must propagate/log registration errors and return ERR.

Existing CString/NUL panic policy, malformed enum-table assertions, custom
getter/setter behavior and default/text/numeric conversion remain. `Result` does
not promise all possible input failures become errors or provide panic safety.
Continue native name/flag validation; both examined hosts accept underscore
names, so JMESPath's existing configuration names need no migration.

Source compatibility: calls ending in a semicolon can compile with an unused
Result warning, while code expecting unit/function-pointer signatures breaks.
Silently ignoring the new result forfeits the improvement. Do not retain a
deprecated unit-returning wrapper that continues hiding rejection. The macro
tuple syntax stays compatible, but its failed-registration behavior changes.

Stream-filter's five scalar calls need `.map_err(|error| error.to_string())?`,
matching its already migrated string call. JMESPath's helper should return
`Result<(), RedisError>` and use `?` on four calls; its init must log an error and
return `Status::Err`. Preserve existing successful init and manual load ordering.
`RedisError` currently has no `std::error::Error` implementation or `From<String>`;
its blanket `From<T: std::error::Error>` impl is not a String conversion and
prevents simply adding an Error impl without a coherence conflict.
do not assume generic application error conversions or expand this chunk into
an error-type redesign.

## Stage two: additive typed direct-registration builders

Proposed factory/API shape, **not implemented or compiled**:

| Factory | Required data | Optional named methods |
|---|---|---|
| `configuration::config::i64(name, storage, default, range)` | `&'static T: ConfigurationValue<i64>`, i64 default, `RangeInclusive<i64>` | `.flags(...)`, `.on_changed(...)` |
| `config::boolean(name, storage, default)` | `&'static T: ConfigurationValue<bool>`, bool default | Same |
| `config::string(name, storage, default)` | `&'static T: ConfigurationValue<RedisString>`, borrowed text default | Same; retains F11 lifecycle admission |
| `config::enumeration(name, storage, default)` | Existing enum/value traits and typed enum default | Same |

Factory spelling is provisional: consider consistent `integer`/`numeric`
instead of `i64` when implementing the builder layer, then recompute examples.
Each concrete builder consumes itself in `.register(&Context) -> Result<(),
RedisError>`, delegates to the fallible free function, and stores no Context.
Borrow name/string default only until registration; retain the static storage
requirement and typed enum default. Numeric range is mandatory: no hidden full
range/default or validation-policy change. Flags default to `DEFAULT`; callback
defaults to absent. `.on_changed` accepts the existing
`Fn(&ConfigurationContext, &str, &'static T) + 'static` shape and boxes it
internally; no added Send/Sync bound or captured-borrow support. Preserve callback
timing, cardinality and allocator behavior rather than promising zero allocation.
Do not introduce heterogeneous builder vectors or a second registration engine.

Actual F11 isolated stream-filter call, `src/config.rs`:

```rust
register_bool_configuration(
    ctx,
    "use-iterator",
    &USE_ITERATOR,
    use_iterator,
    ConfigurationFlags::DEFAULT,
    None,
);
```

Proposed equivalent successful registration with new error handling:

```rust
config::boolean("use-iterator", &USE_ITERATOR, use_iterator)
    .register(ctx)
    .map_err(|error| error.to_string())?;
```

The six selected stream-filter calls have **57 nonempty call-site lines before,
39 proposed**, 18 fewer, after the same rustfmt 1.94.0 default formatting with
edition 2021. Preserve every name, storage reference, default expression,
numeric bound, immutable string flag and application error conversion. The
proposal adds five previously missing error conversions, removes six explicit
`None` slots and five default-flag slots. Method: format unresolved illustrative
function bodies and count nonempty body lines; exclude wrapping function/braces.
This is format-only source evidence, not API type checking, complete-helper LOC,
allocation, throughput or runtime equivalence evidence. Raw unformatted counts
are retained but must not be used as a comparable savings claim.

Actual JMESPath wraps each change closure in `Some(Box::new(...))`. Proposed:

```rust
config::boolean("cache_enabled", &CACHE_ENABLED, true)
    .on_changed(|_ctx, name, _var| {
        redis_module::logging::log_notice(format!(
            "Config {} changed to {}",
            name,
            CACHE_ENABLED.load(Ordering::Relaxed)
        ));
    })
    .register(ctx)?;
```

This hides optional/boxing syntax, preserving the real callback body and one
callback allocation. The containing helper must be fallible; this snippet
does not imply it can use `?` in its current unit-returning version. Event-stream
custom storage implementations and tuple callbacks are intentionally retained.

## Corrected-code acceptance gates for later implementation

| Gate | Required evidence |
|---|---|
| Compiler/API | Existing tuple syntax and all four fallible free functions; direct custom storage and callback examples; JMESPath helper plus init; stream-filter's String error boundary; an isolated deny-unused-Result compiler fixture (JMESPath currently allows unused_must_use). Pure compiler consumers never initialize Redis APIs |
| Pure rejection ownership | Rust-only probes for API-unavailable/rejected private data and captured-callback drop exactly once; success keeps accepted data stable. Test a small policy helper used by production, without fake Context/native-vtable initialization |
| Legal native registration | Corrected isolated OnLoad fixtures: native-success numeric/bool/enum, supported ordinary duplicate/name/flag rejection, macro and direct-init ERR propagation, fresh legal reload. Check host source/encoding/context/provenance; no invalid range/table or historical crash path |
| Native behavior | Existing selected defaults/startup args/custom setters/errors/change callbacks, GET/SET and enum text/integer behavior; compare successful behavior to current fixtures. New native rejection must be observable. Preserve manual/macro LoadConfigs call/result policy; behavior alone is not ownership accounting |
| F11 integration | Macro boolean/enum failure after an accepted string registration, and a direct-init numeric failure after strings, follow the existing load-error owner cleanup (macro numeric rows precede string rows); selected string owner lifecycle regression only when that interaction changed. Accepted metadata/full userdata reclamation remains outside the claim |
| Downstream/archive | Fresh relevant package consumers and isolated event-stream/stream-filter/JMESPath migration with F07/F09/F11 prerequisites. Originals stay read-only; Carolina requires no selected config migration. Check actual archived source and dependency resolution when exporting a package |
| Builder follow-up | Typed factories and callback inference; compiler rejection of nonstatic storage/borrowed closure/wrong value kind/reuse after register; delegation to the existing contract; equivalent selected runtime behavior and consistent formatted source measurement |

Use ordinary corrected host paths; native guards must have genuinely legal
OnLoad/owner-thread contexts and isolated registration state. No worker-thread
callback, broad GIL/context/alias/panic, native crash/reproduction, leak sampler
or global performance/safety claim is part of this proposal. Do not rerun
completed F11 tests merely for this source-only specification.

## Native/overlap ledger and process

Source inputs are retained Redis 7.2.8 and pinned 8.10.2 commit
`498ecd0d6d007db11ddb3aea9428552598a78622`, with four hashes locally recorded.
Relevant ranges in module.c: 7.2.8 name validation 12482–12500, common admission
12651–12667, bool 12787–12801, enum 12832–12857, numeric 12863–12876;
8.10.2 name validation 13980–13998, admission 14290–14338, bool 14457–14470,
enum 14501–14522, numeric 14528–14544. Enum metadata copying is visible in
these functions and config.c (7.2.8:3346–3353; 8.10.2:3769–3789). Native-version
alias/flag differences remain host policy, not duplicated SDK validation.

Fresh overlap snapshots contain four fork drafts, 45 upstream PRs and 55
upstream issues. PR229 is the broad configuration API origin, not a competing
scoped repair. No matching open direct-registration ergonomics/fallibility issue
was found in the selected search. Existing fork drafts 1–4 remain for deferred
CI handoff. Leaves upstream issue 358 open; broader string contracts and issue
471's unrelated unvalidated enum allegation are excluded. Inherited F09 PR487/
PR488 INFO attribution remains in its existing documentation; no duplication.

F12 uses a reviewed local docs branch and no-ff merge, with no push/PR. F13 can
follow the local issue-independent process if fresh overlap still finds no
tracked scope. If it is mapped to any tracked issue, list all scoped issues and
complete the dedicated baseline/empty-kickoff/push/scoped draft before code.
Do not manufacture issue scope or use this proposal to authorize a push of
fork master. CI, package identity, MSRV, server/platform support and release
decisions remain deferred. The builder spelling is proposed; F13 is only the
fallible registration stage, with builder implementation a later bounded chunk.
