# F18 — LoadConfigs failure propagation scope

This is a source-only repair specification, not an implemented or compiled API.
The recommended next chunk adds a fallible `configuration::load_configurations`
helper and propagates its result at existing macro/manual call sites. Keep call
placement, successful defaults, registration order and callback policy unchanged.
There is no SDK test, benchmark or native execution in F18.

## Observed contract and author paths

Baseline: local master `41f84bedb78f1e35e97babd68448e64b4b3dba22` (post-F17).
Inventory covers 84 tracked SDK Rust files and 135 tracked Rust files across the
four original references. Literal matches are a bounded source inventory, not
an exhaustive semantic call graph. Seven retained excerpts and selected source
hashes are in `target/f18/inventory.json` and `excerpts.json`.

| Path | Current source behavior | Narrow consequence |
|---|---|---|
| SDK macro, `src/macros.rs:457–459` | Inside the optional configurations block: call only when the API exists; discard the return. Registration groups are numeric/string/bool/enum. Load precedes config helper commands, info/events, user init and F11 scope commit. | Native load failure can be followed by user init and a successful SDK return. Native post-load rejection is a separate path, not SDK propagation. |
| Macro without a configurations block | No automatic LoadConfigs call. | Do not insert an unconditional call in every macro expansion. |
| Present but empty configurations block | Current expansion still attempts the optional native call. | Preserve that position; explicitly document the proposed missing-API error even with no registrations. An absent block remains different. |
| Original stream-filter | Manual register helper returns `Result<(), String>`; after its registrations it optionally calls LoadConfigs and returns Ok regardless of status. Its macro has no configurations block. | This is its first call, inside init. Preserve position and domain/parser/error conversion bodies. |
| Original JMESPath | Void register helper, called by init, optionally loads after registration and ignores status. No macro configurations block. | Use the already-migrated F17 isolated snapshot for a later port: its helper is now `Result<(), RedisError>` and init logs/returns Err on helper error. |
| Event-stream | Macro tuple configurations/custom storage; no selected manual load. | Tuple declarations and domain setters stay unchanged; propagation is through the macro. |
| Carolina | No selected configuration/LoadConfigs call in tracked Rust. | No migration selected. |
| SDK direct examples | `configuration_builders.rs:223–226` and `support/configuration_ownership.rs:141–144` explicitly preserve an unchecked manual call. | Migrate these corrected fixtures at the same position; keep their other modes/accounting separate. |

Original module directories remain read-only. Future isolated stream-filter and
JMESPath snapshots must retain the F07/F09/F11/F13/F14 prerequisite patches and
F17 graph/source/patch provenance. Their current selected tails still ignore the
native load result; prior passes did not validate the proposed error propagation.
Original JMESPath's void helper is not the current isolated Result helper.

## Native source evidence

The retained 7.2.8 source is from the F01 release archive/provenance. The 8.10.2
module source is pinned to `498ecd0d6d007db11ddb3aea9428552598a78622`; its config.c
was read at the same revision through GitHub's content API and checked against
the Git blob SHA. See compact evidence and `target/f18/native-8.10.2/provenance.json`.
These are source observations, not runtime results or a server support policy.

| Claim | Retained source anchors |
|---|---|
| LoadConfigs rejects a missing context/module or a module outside OnLoad, then returns loadModuleConfigs' status. It does not provide an owned error object or documented errno. | 7.2.8 module.c:12876–12889; 8.10.2 module.c:14562–14575 |
| Configs apply sequentially in registered-list order. Queued values use the config interface; otherwise defaults use direct module setters. On failure, log the config error, clear the pending queue and return ERR. Earlier setter effects are not rolled back by this loop. | 7.2.8 module.c:12565–12595; 8.10.2 module.c:14150–14181 |
| Default setters dispatch directly; string queued values can call a getter for comparison. Thus zero cached string snapshots at first load is valid; an explicit queued string can publish one before a later config rejects. | 7.2.8 config.c:756–772,1902–1923; pinned 8.10.2 config.c:839–855,2009–2030 |
| First failure does not set initialized success state. Repeated calls differ: 7.2 reapplies absent defaults; 8.10 skips them when defaults were already applied. LoadDefaultConfigs is an additional 8.10 path, not a proposed prerequisite. | Same loadModuleConfigs ranges; 8.10.2 module.c:14545–14560 |
| OnLoad returning ERR leads to native unregister/config removal, context/module cleanup and image close, without the ordinary OnUnload callback. OnLoad success can still be rejected afterward for missing initialized configs or leftover LOADEX arguments, using forced unload. | 7.2.8 module.c:12214–12226,12250–12268; pinned 8.10.2 module.c:13700–13713,13734–13753 |

Do not report registration errno as a LoadConfigs diagnostic: F13 registration
and this native API have different contracts. A custom callback may affect errno;
that does not make it the load error. Native logs contain the detailed cause. The caller also read the pinned 8.10.2
server.h definitions: DEFAULTS=0x1, USER_VALS=0x2, ALL_APPLIED=0x3, confirming
the repeat-call bit interpretation; see server-provenance.json.
Neither the helper nor a new error return provides transactional rollback. A
later failed call can also follow earlier initialized state; do not rely on the
post-load initialized check to catch every ignored failure. No such historical
path was executed for this scope.

## Proposed API and migration

Recommended additive public API (proposed, uncompiled):

```rust
pub fn load_configurations(ctx: &Context) -> Result<(), RedisError>;
```

Resolve the optional API, call it exactly once for each helper invocation, and
map `REDISMODULE_OK` to Ok and every other native result to Err. Distinguish
unavailable API from native failure with proposed stable SDK messages:
`LoadConfigs API is unavailable` and
`Module configuration loading failed; see Redis server log`. Do not attach
`last_os_error`, claim access to the native detailed error, retry or log from the
helper; the macro/direct init owns logging. No new dependency, stored Context,
registry, default cache, phase token, batch engine or callback allocation.

The helper wraps the existing raw invocation at the configuration layer. Its
intended use is the real native OnLoad with a valid live Context under existing
host/SDK obligations. Native phase checks are not a Rust proof of arbitrary
Context validity, GIL/thread safety or reentry. Do not widen the Context contract
or certify raw/fabricated contexts. An existing valid command Context can be a
later legal error control; pure tests must never construct a fake Context.

Proposed macro replacement, at the existing point (uncompiled):

```rust
if let Err(error) = $crate::configuration::load_configurations(&context) {
    context.log_warning(&error.to_string());
    return $crate::raw::Status::Err as c_int;
}
```

Use the fully qualified `$crate` path so external consumers need no extra
imports. F13 registration errors still return before this call. If the optional
configurations block is absent, no call is generated. A present empty block
continues to call when available but now reports missing API rather than silently
skipping it. That is a deliberate failure-policy change, not a new minimum-server
commitment. No runtime missing-API test on an unselected old server is implied.

Selected manual snapshots, after the existing registration statements (proposed):

```rust
// stream-filter's existing Result<(), String> helper:
load_configurations(ctx).map_err(|error| error.to_string())?;
// JMESPath's F13/F14 Result<(), RedisError> helper:
load_configurations(ctx)?;
```

Keep their existing `Ok(())` tails and init log/Status::Err handling. Keep builders,
custom ConfigurationValue bodies, enum/string/numeric/UTF-8 parsing, module args,
defaults, command protocols and stored formats. Existing tuple/free-function
registration APIs stay. Update selected SDK examples to log and return Status::Err
on load failure, retaining their legal F11 lifecycle.

| Option | Assessment |
|---|---|
| Check status only in the macro | Small patch but leaves manual authors using raw pointers/ignored status; not recommended. |
| Add helper plus propagation, preserving call locations | Recommended narrow repair. Additive API; intentionally earlier failure and no user init after macro load failure. |
| Move loading after init, auto-load all modules, or deduplicate calls | Changes observable default/callback/order behavior and late registration. Separate design; excluded. |
| Add builder finish/batch or phase-typed configuration engine | Broader ergonomics/lifecycle redesign; excluded. |

## F11 lifecycle dependency and failure behavior

The existing pending StringConfigurationLoadScope starts after native init,
spans registration and user init, and commits just before OnLoad success.
Propagating macro/helper failure must return while that guard is still pending.
Its Drop closes cached string owners before native unregister/removal; do not
commit on failure, manually call successful-unload cleanup as a substitute, or
expect user deinit/OnUnload to run. A direct helper inside user init must return
the error through init so the same macro guard drops. A raw custom OnLoad must
retain the documented unsafe begin/commit/close lifecycle recipe and return ERR
with its pending guard live; a Result helper does not create that scope.

No private registry borrow/lock is held over LoadConfigs: it invokes arbitrary
selected user setters/change callbacks and can read string getters. Keep short
F11 slot operations, accepted private-data stability and generation rules. Do
not free accepted callback metadata on LoadConfigs failure; native still owns
its pointer until removal, and full reclamation remains deferred. User statics,
callback effects and unrelated resources are not rolled back. Successful unload,
canceled deinit and later host post-load rejection keep their existing policies.
The latter still invokes ordinary OnUnload/deinit/close under
`serverAssert(moduleUnload(...) == C_OK)`; refusing deinit there is not a safe
canceled-unload control. Later fixtures must use successful deinit on that path.
This is a source observation only, not a path to reproduce.

## Later corrected-code acceptance gates

All items below are requirements for a later implementation, not F18 passes.

| Gate | Required evidence |
|---|---|
| Pure production policy | If needed, a small private callable adapter can exercise absent API, OK/non-OK mapping, exact one call and caller short-circuit without Redis pointers/allocator/Context. Test actual production adapter, not a parallel model. Reuse generic F11 slot/drop probes for owner replacement/close mechanics only. Real guard begin/commit/Drop integration belongs to the legal native gates below, not a pure test with a synthetic Context. Zero/one snapshots follow actual getters. No raw API-pointer initialization, dummy Context or fabricated callback. |
| Public compiler controls | Metadata-only uninvoked consumers of the additive helper and Result-returning reference shapes, plus external macro hygiene. A paired `deny(unused_must_use)` control catches ignored Result even where a reference normally allows it. No runtime/native call. |
| Legal success | Isolated corrected OnLoad fixtures on owned 7.2.8/8.10.2 hosts, debug/release: default and valid LOADEX/startup values, custom setter/change behavior, strict text and enum/numeric values; capture order and first-call count per fixture. Compare expected successful behavior without running old unchecked code. Module absent/present-empty config blocks need separate controls. No exactly-one-call claim across all modules. |
| Ordinary failure | Registered custom setter rejects a selected default or validly supplied queued value. Macro path logs and returns ERR before user init; direct-init path returns Result and init logs/returns ERR before later work. Distinguish F13 registration failure (LoadConfigs never reached) from native load failure (registration succeeded). Native detailed log and stable SDK message are separate. |
| Cached-owner cleanup | Use ordered valid queued string then later rejecting bool/enum to permit a real getter comparison before failure. Record actual getter/owner counts; owners close on pending-guard Drop, not OnUnload. Default-only failure may publish zero. Show native cleanup, host PING afterward, absence of module/configs and fresh legal reload. Ownership accounting excludes process heap/statics/accepted metadata; no leak sampler. |
| Lifecycle controls | Retain successful/canceled unload and successful commit; raw custom lifecycle recipe documented/selected valid fixture. Missing symbol is pure adapter evidence, not mutated global pointers. A valid owned command-context call may verify native ERR outside OnLoad without fake/raw contexts or user callbacks. No historical invalid paths or native removal callback window. |
| Reference behavior | Isolated F17-prerequisite stream-filter helper keeps Result<(), String> conversion; JMESPath keeps Result<(), RedisError> and init propagation; selected defaults/CONFIG GET/SET/command flows and ordinary rejection/reload. Event-stream tuples/custom setters remain; selected flows suffice, not full application/optional products/Cluster/replication coverage. Carolina has no selected migration. Originals untouched, with hashes/graph/provenance checked. |
| Packaging and ordinary checks | Appropriate pure tests, examples both profiles, Clippy/fmt/docs; fresh four archives and external consumers must expose the helper and expand the macro from actual archive bytes, source/graph verified. Independent other-provider source review and caller execution/contract checks. No CI/MSRV/package/support/release commitment. |

Do not add a retry after ERR, synthesize rollback, move init/defaults, implement
LoadDefaultConfigs support or repair genuinely repeated/late-call policy. If
those become required to satisfy the narrow repair, stop and rescope explicitly.

## Overlap, process and limits

Fresh read-only inventory finds five fork drafts, 45 upstream PRs and 55 upstream
issues. No matching open issue for this narrow load-result scope was found.
PR229 by MeirShpilraien (Meir Shpilraien), head
`566ad273797871acc6f50b5345a0c1cfa16fc34b`, has broad configuration-API ancestry:
its retained diff adds optional LoadConfigs with an ignored result. It is not a
port or the entire broad PR to implement. Record this contribution context for
any later adaptation; preserve existing BSD/F02/F09/F17 credits. Issue378 is
broader Redis 6 CI compatibility background, not selected implementation scope;
issue471 remains excluded. Existing fork PR4/F11 is a lifecycle dependency, not
competing LoadConfigs work. Leaves existing upstream issues open.

F18 stays on a dedicated local docs branch and has no push/draft/publication.
Before future implementation, refresh overlaps and issue mapping. If mapped to
tracked issues, satisfy dedicated baseline/empty kickoff/push/scoped draft
visibility and reconcile ancestor-history authority before editing code. Never
manufacture issue scope for publication, push fork master or merge on GitHub.
Drafts1–5 and CI remain deferred. Native safety/platform/worker-thread/special
unload/no-user/template-hash/metadata gaps remain unchanged. Reference/provenance
and Due evaluation support the SDK work; they are not new workstreams.

One focused Due/Claude source review approves the specification. True process
exit 0; request/result/answer, 219 trace records and offline summary validate.
All 46 activities match, with no gaps/drops; three activities failed (two Read,
one Grep). The reviewer reports denied original-reference reads outside the fork,
used retained excerpts, and did not recompute hashes/remote state. Caller
independently read those originals and verified their heads/status/135 Rust
hashes and selected source/provenance/overlap receipts. No permission expansion,
provider retry or fallback occurred. The Grep failure has no exposed diagnostic;
its cause is not assigned to Due.

The review's nonblocking forced-unload clarification was source-checked and
applied. Caller refinements specify error strings, verify native initialization
bits from the pinned header and distinguish generic pure slot probes from real
native guard integration. These are source/documentation clarifications with
no new implementation or test execution. Reviewed inputs stayed unchanged
through review completion; later doc refinements have their own hash/disposition
record. Caller source, hash and scope checks are separate from the verdict.
No API was implemented, compiled or natively executed; earlier F11/F13/F14/F17
runtime passes do not certify this proposed LoadConfigs repair.

Next recommendation is F19 implementation of exactly this reviewed helper and
direct caller consequences, with the acceptance gates above, after fresh source/
overlap/issue-process reconciliation. Do not start it in the F18 activation.
