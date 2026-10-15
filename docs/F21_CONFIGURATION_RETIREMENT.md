# F21 — Accepted configuration metadata retirement scope

**Decision: retain accepted metadata. A reclamation implementation is blocked on
an available, proved native retirement boundary.** The selected Redis 7.2.8 and
8.10.2 sources remove configurations after the module's OnLoad/OnUnload callback
has returned. Their registration APIs do not accept a private-data destructor,
and their configuration API exports provide no unregister/retirement-completion
hook. The SDK's existing close point is not that boundary. This is a completed
source-only specification, not an implemented reclamation API or a runtime pass.

Scope: the four SDK configuration registration private-data boxes, their owned
`on_changed` captures, string reply-state Rc, and a possible future registry of
those allocations. Broader statics, native wrapper leaks, Context/GIL/thread/
reentry/panic audits, unrelated callbacks and general resource engines stay out.
Baseline is post-F20 `5ee09dd0b8124c89a9bbb51e079112df595fd18b`.

## Ownership inventory

| Resource / source | Present owner and release policy | Narrow consequence |
|---|---|---|
| `ConfigrationPrivateData<G,T>` in `src/configuration.rs` | Box contains `&'static T`, optional boxed `Fn`, and PhantomData. Native retains its raw address on successful numeric/bool/enum registration. `PendingRegistration` drops only unaccepted data. | The static storage is borrowed, not owned by this Box. Dropping the Box would drop owned callback captures, potentially including native resources or non-Send Rc. |
| `StringConfigurationPrivateData<T>` | Box additionally owns an Rc reply state. Native acceptance retains the stable address. Native rejection reconstructs/drops the Box; defensive tracking failure after acceptance keeps stable closed private data. | An accepted-but-untracked error must never be classified as native rejection and freed. |
| F11 `OWNER`/`ModuleReplies` | Owner-thread TLS holds generation/context-admission and Rc reply slots. Pending guard Drop or successful close takes the registry entry, then closes slots. Commit clears loading Context. | Current registry removal releases cached RedisString owners, not the Box, its captures or every Rc. Box-held Rc keeps the closed state alive. |
| `PendingRegistration` / builder options | Missing API and rejected/unsubmitted scalar data drop; accepted finish sets `accepted = true`. F14 consuming builders delegate to these free functions. | Existing F13 rejection cleanup is complete for its selected contract; it is not accepted-data cleanup. No builder or callback ABI change is needed merely to describe this gap. |
| Native `ModuleConfig` and `standardConfig` | Native stores raw SDK privdata and adapters; native removal frees its own names/defaults/enum tables/config wrappers. `moduleListFree` frees the native wrapper, not SDK privdata. | Native C wrapper freeing is not Rust Box destruction or callback capture destruction. Name/default/enum registration inputs remain copied by native code under the existing contract. |

The lexical SDK inventory reads 86 tracked Rust files and records 40 declaration/
import/call sites, not 40 distinct registrations. The macro, free functions and
typed builders all reach the same four registration paths. Numeric/bool/enum raw
SDK callers currently need a legal native OnLoad but do not require F11 string
scope admission; changing that is a migration decision. String raw entry points
already need the explicit unsafe begin/commit/close recipe. SDK examples use
captured callback drop probes; the generic callback type accepts owned/static
captures without adding Send/Sync. Do not replace this with a blanket Send/Sync
registry. Direct native registrations outside SDK functions are not owned here.

Selected reference patterns remain the F19 isolated stream-filter/JMESPath direct
helpers and event-stream tuple/custom storage. Existing source/provenance and all
four original heads/status/135 Rust hashes are preserved; no original is migrated.
This scope adds no requirement to static storage implementations or domain bodies.

Actual current scalar handoff (`registration.rs`), retained unchanged:

```rust
// Redis accepted this pointer. Keep the existing stable allocation;
// native removal/quiescence needed for accepted-data cleanup is deferred.
self.accepted = true;
Ok(())
```

That branch deliberately prevents pending Drop from reclaiming an address still
registered with native code. No new allocation/layout/leak/throughput measurement
is supplied by this source observation.

## Native lifetime and last-use evidence

Pinned source evidence is retained in `target/f21/source-provenance.json` and
line-numbered `excerpts.json`. Redis 7.2.8 files match the retained release tarball,
SHA-256 `6be4fdfcdb2e5ac91454438246d00842d2671f792673390e742dfcaf1bf01574`.
Redis 8.10.2 module/config files match recorded Git blobs at
`498ecd0d6d007db11ddb3aea9428552598a78622`. These observations apply to these
sources; they do not select a new support matrix.

| Boundary | Redis 7.2.8 module.c | Redis 8.10.2 module.c | Implication |
|---|---|---|---|
| Retain SDK pointer | `createModuleConfig` 12641; registrations 12764–12873 | `createModuleConfig` near 14272; registrations 14438–14543 | Pointer is stored; no private-data destructor parameter or immediate callback in these registration bodies. |
| Native record destruction | `moduleListFree` 2224; `moduleFreeModuleStructure` 12028 | 2351; 13500 | Free native names/wrapper/list, not `privdata`. |
| Failed OnLoad | `moduleLoad` 12217–12230 | `moduleOnLoad` 13697–13712 | Callback returns ERR first; native unregister/removal/module free then run, with no OnUnload call on this branch. SDK pending guard has already run. |
| Successful/refused unload | `moduleUnload` 12281–12345 | 13766–13833 | Ordinary native refusals precede OnUnload; user refusal returns before native cleanup. After success, native cleanup precedes dlclose. SDK close inside OnUnload precedes cleanup. |
| Config removal | `moduleRemoveConfigs` 11982; `moduleUnregisterCleanup` 12174 | 13431; 13650 | Native removes config lookup entries during cleanup, after SDK callback return; 8.10 also removes alias entries. |
| Config dictionary destruction | config.c `removeConfig` 3304–3324 | config.c 3692–3729 | Frees native names/defaults/enum tables and deletes lookup entry. It invokes no SDK private-data destructor. |

`moduleUnregisterCleanup` is not an empty gap: before config removal it calls
`moduleFreeAuthenticatedClients`, which may revoke authentication and invoke
`moduleNotifyUserChanged`'s registered auth callback (7.2 module.c 9464–9512;
8.10 10656–10696). This is evidence that returning from OnUnload is not the last
module-code callback in the teardown sequence. It is **not** a reproduction or
proof that a configuration query from that callback is legal on every host.
No auth audit/test or new callback scenario is executed here. A future retirement
proof cannot simply assume all module code has ceased at SDK close.

Redis 8.10 additionally exports module-side ConfigIterator/Get/Set APIs absent
from 7.2's export list (8.10 module.c 16072–16083). That is another pin
difference relevant to possible configuration reachability; no legality of
using those APIs from a cleanup callback is established here.

The selected config-removal/list-free bodies do not invoke config adapters;
that local observation does not prove global callback quiescence before removal.
During native removal there is no new callable SDK window. After removal there
is no selected public SDK cleanup callback before code unmapping. Static/TLS or
dynamic-library finalizers are not a portable substitute: allocator phase,
thread, ordering, pinned images and old callback vtables would all need a proven
contract. A fresh load/generation number alone does not establish that proof.

The existing F11 string cache close and F19 error propagation remain unchanged.
Their owner-thread/GIL/valid-context/no-live-getter-view obligations still apply;
F21 neither certifies nor broadens them.

## State/phase obligations for a later repair

| Phase | Required behavior |
|---|---|
| API absent, unsubmitted or native rejected | Preserve existing immediate owned-data cleanup and exactly-once callback capture Drop. No retirement ticket required because native never retained this pointer. |
| Native accepted during pending load | Keep pointer and captures usable through actual configuration/default/get/set callbacks and user init. Record ownership before any later fallible bookkeeping. An allocation/track failure cannot free an accepted pointer. |
| OnLoad returns ERR | Close cached owners as today; keep accepted metadata until a proved post-removal/quiescence boundary. Failed OnLoad does not run deinit. Do not invent a callback after return. |
| Committed | Keep metadata active and stable. Successful GET/SET/callback/default/strict text/numeric/enum policies stay; F19 load result and call positions stay. |
| Native refuses before OnUnload or user deinit refuses | Do not detach/drop metadata, captures, slots or generation. Subsequent ordinary config use must retain state. |
| Successful OnUnload | User deinit precedes existing cache close; native config removal follows callback return. Do not equate close with native detachment. |
| Forced post-load rejection | Real OnUnload/deinit still runs under native assertion in the caller. Never use a refusing deinit or execute that assertion path as a test. |
| Native-detached and quiescent, if later supplied | First retire every registry reference to an allocation; only then run its destructor exactly once on the legal owner thread while destructor code/allocator/native resources are valid. No private RefCell borrow/lock held across destructor user code. |
| Fresh reload | No stale entry, pointer or old-image destructor may be reused merely because names match. Image/generation ownership and canceled-unload distinction must be established; no platform-pinning assumption. |

The forced post-load calls supply `NULL` for `errmsg`; native pre-OnUnload
refusal branches write through that argument, while user deinit refusal reaches
the caller's assertion. These are source observations only, reinforcing the
exclusion of forced-refusal execution from validation.

Proposed state contract, **not code or an available native token**:

```text
Accepted -> NativeDetachedAndQuiescent -> RegistryDetached -> Dropped
```

The missing edge is `Accepted -> NativeDetachedAndQuiescent`. A GIL marker,
Context, SDK pending/committed flag or closed getter slot does not construct it.
The future registry must not expose deallocation while any callback/native lookup
can still reach the allocation. Erasing a private typed owner/drop function for
heterogeneous registration kinds would be limited to this bookkeeping, not a
new public batch/type-erasure engine. No stored Context or cross-thread transport.

## API and migration options

| Option | API/source effect | Decision |
|---|---|---|
| Retain accepted metadata and document the residual | No new API or caller migration. Preserve current free functions, builders, tuple DSL, raw string lifecycle, LoadConfigs/error policy. | **Selected now.** Honest stable storage while native retirement proof is unavailable. No claim of complete module-memory reclamation. |
| Free boxes in pending Drop/successful SDK close | Could look source compatible; destructors/captured resources would begin running earlier while config lookup entries still exist. Registry detachment alone does not remove native references. | **Not implementation-ready.** Requires last-use/quiescence proof covering cleanup callbacks and destructor reentry/phase; do not implement by assuming absence of config calls. |
| Keep stable tombstones but drop captured payloads early | Partial reclaim; requires every scalar/string adapter and user destructor to obey a closed-state contract. Existing scalar adapters are not gated. A synthetic closed getter/setter result would be a behavior decision. | **Deferred**, not the selected repair. Do not infer a legal late-callback host window or invent stale/zero-value native responses. |
| Managed accepted-owner registry plus native retirement capability | Could be additive for macro/opt-in callers, or require a new unsafe lifecycle admission for all direct scalar callers. Existing raw scalar calls could retain their documented residual under a compatibility path. | Conditional future design only. No safe token from Context; no new public export until a real host boundary exists. Requiring managed admission everywhere would be a Rust behavior/migration break to review explicitly. |
| Host-side destructor/unregister-completion contract | Needs a real native API/embedding hook after last use and before image/allocator removal, including failure and canceled-unload semantics. | Potential prerequisite to reconsider, **not authorized server patch, platform-specific finalizer, new dependency or support commitment**. Do not call private Redis internal symbols. |

No signature or implementation change is selected in F21. In particular, do not
add a public unsafe `free_all` whose callers cannot satisfy its documented
precondition in normal module callbacks. That would relocate the proof gap.
Closure Drop may own native objects or arbitrary user code; no general
panic/reentry safety claim follows from a type-erased destructor or catch_unwind.

## Corrected-code acceptance gates, only for a later unblocked implementation

1. **Source prerequisite:** identify an available, pinned, callable retirement
   boundary for each selected host. Prove config lookup/alias removal and no
   outstanding/in-flight borrowed getters/callbacks, and executable destructor/
   allocator/native-resource lifetime. If any part is unknown, stop and retain
   accepted metadata. No runtime crash/leak reproduction can replace the proof.
2. **Ownership design:** explicit accepted/rejected distinction, image/generation
   ownership and exactly-once registry-detach-before-drop. Include native
   acceptance followed by defensive tracking failure and no lost accepted owner
   during fallible bookkeeping. Preserve non-Send captures; no blanket Send/Sync.
3. **Rust-only production state/drop probes:** unavailable/rejected/unsubmitted
   cleanup, pending/committed/canceled/retired transitions, all four value kinds,
   heterogeneous captured drop counters, registry removal before user Drop,
   destructor reentry probes limited to pure state, stale generation rejection,
   double-retire and failed tracking. No Redis APIs/allocator, fake Contexts,
   raw native callback invocation or synthetic token passed as a native proof.
4. **Uninvoked public compiler controls:** actual exported free/builder/macro
   APIs, any reviewed migration, nonstatic storage/captures, non-Send owner/ticket,
   no safe construction from Context and invalid consuming reuse. Compile actual
   archived SDK controls as well. Compiler acceptance is not native legality.
5. **Legal isolated owned-host fixtures:** only after gate 1, actual successful
   startup/LOADEX, registration and native LoadConfigs rejection, pending failure,
   commit, ordinary canceled/successful unload, reload and subsequent PING/config
   removal. Use native-supplied valid OnLoad/OnUnload phases/isolated registrations.
   Zero/one cached snapshot counters and accepted/rejected callback-data counters
   are separate; prove drops at the actual legal boundary, not only disappearance
   from CONFIG GET. No removal-interval callback, fabricated context, refusing
   forced deinit, unsupported unload refusal/assertion or historical invalid path.
6. **Compatibility/archive/reference:** preserve default/parser/text/numeric/enum/
   command/stored-format/LoadConfigs order and Results. Appropriate pure/docs/
   examples/fmt/Clippy and fresh source-verified archives/public consumers. If API
   admission changes, migrate selected isolated stream-filter/JMESPath/event-stream
   callers with their existing error handling/traits; originals remain untouched.
   No need to migrate Carolina without an affected caller. Independent other-
   provider source review and caller contract/effect checks are required.

All gates are proposed; none was compiled/native-executed in F21. F11/F13/F19
execution receipts remain historical, not tests of reclamation. Counters would
establish selected ownership effects, not native refcounts/heap leak freedom,
all thread/FFI safety, full apps or a support matrix.

## Overlap, handoff and stopping point

Fresh read-only snapshots contain five fork drafts, 45 upstream open PRs and
55 open issues. No narrow accepted-configuration-metadata retirement issue/PR
was identified in the refreshed list; issue471's unrelated unvalidated allegation
is excluded. F21 remains local source-only; it creates no SDK push/PR/history
upload. Any future tracked implementation must refresh mapping/history authority
and meet baseline/empty-kickoff/push/scoped-draft visibility before code.
Existing drafts1–5 and CI stay deferred, with GitHub merges/releases human-owned.

Preserve BSD and F02 PR476 Josh `437bf22b933c31d04573943b692542032edd9538`,
F09 PR487 jonnyomerredis `c7ca18e4a6fce2f2f32e390f39abef4d67e1c9c6` and
PR488 oshadmi historical reviewed `01ff52181d9d2efd136b1ef38143ad05223adca6`,
F17 PR93 MikkelSnitker `f1ec666ecc6891e11130df853e8b80c758df5230` and
configuration ancestry PR229 MeirShpilraien
`566ad273797871acc6f50b5345a0c1cfa16fc34b`. No new upstream adaptation.
Leaves upstream234/134/246/358/473 open; issue378 remains broader Redis6/CI
background. Known worker-thread/special-unload/no-user/template-hash/platform
cells, broader statics and native audits stay deferred.

One new Due/Claude source review approves, true exit 0. Request/result/answer,
124 trace records and offline summary independently validate; all 23 activities
complete successfully, with no gaps/drops and frozen reviewed inputs unchanged
through completion. The caller checked/applied three nonblocking source
clarifications: exact unload-body line ranges, 8.10 config query/set exports and
the forced-unload NULL error argument. Final document hashes distinguish these
refinements from review inputs. Source approval does not certify provenance,
remote/original state or native execution; those remain separate caller checks.
No second model or unchanged SDK execution review is claimed.

The [compact evidence](evidence/f21-configuration-retirement.json) and
`target/f21/` retain source/hash/inventory/review records.

The next implementation is blocked on retirement proof, not on permissions or
a failed build. The source specification itself can finish at a reviewed local
checkpoint. Request Josh's next focus rather than invent a reclamation feature
or repeat this review each interval. Until direction or new native evidence,
retain the blocker and report it without additional model/test spend. CI remains
held; there is no automatic F22 implementation or broader audit.
