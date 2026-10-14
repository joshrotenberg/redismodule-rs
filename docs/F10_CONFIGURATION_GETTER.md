# F10: string-configuration getter ownership specification

This is a source-reviewed repair proposal. No API, getter adapter, lifecycle
hook or native fixture is implemented/compiled/executed in F10. F09 remains
complete; its configuration behavior tests do not certify getter release balance.

## Native contract and the current mismatch

Both retained Redis 7.2.8 and pinned 8.10.2 document the same three contracts:

| Boundary | Native responsibility | SDK responsibility |
|---|---|---|
| String getter | Borrows the returned object, copies its SDS bytes immediately, never releases the object | Keep one owner alive after returning; do not transfer its release responsibility |
| String setter | Creates a temporary input, invokes the setter, releases its input reference afterward | Acquire an independent reference if retaining the input; F09 already does this |
| Setter/apply error | Copies the message and releases the returned error object | Transfer one owned reference; the existing error `.take()` is appropriate |

Primary source ranges: 7.2.8 module.c:12506–12555 and registration notes at
12731–12734; pinned 8.10.2 module.c:14052–14120 and 14404–14407. The SDK getter
at configuration.rs:296–305 obtains an owned `RedisString` from
`ConfigurationValue<RedisString>::get` and calls `.take()`. Redis copies the
bytes, leaving that native reference without a release owner. This occurs for
fresh text conversions and deep clones of native-string backing storage.

The native SDS copies are separate allocations with separate cleanup. config.c
uses the getter for ordinary GET, SET comparison and REWRITE. Freeing those SDS
copies does not release the module's returned RedisModuleString. Exact 8.10.2
config.c was fetched at commit 498ecd0d6d007db11ddb3aea9428552598a78622 and its
Git blob hash independently checked; this is source evidence, not runtime proof.

A local temporary followed by `.as_ptr()` is also insufficient: its owner must
remain alive when the host reads after the callback returns. Neither an immediate
Drop nor AutoMemory against an invented context establishes that lifetime.

## Bounded recommendation

Preserve `ConfigurationValue<T>::get/set` and existing custom string traits.
Introduce a private string-specific adapter that owns at most one published
reply snapshot per registered string configuration. Move the owned `get` result
into the adapter; return its borrowed object pointer. Replace and drop the
previous snapshot during a later legal callback, after its native consumption
has completed. No new byte copy, retain or stored context is required by this
move itself. Normal custom getters can still allocate/copy; no zero-allocation
or throughput claim follows.

Proposed internal shape, **not compiled**:

```rust
struct StringReplyState {
    published: Option<RedisString>,
    closed: bool,
}

// Inside string-specific private data; callbacks receive shared private data.
// Borrow briefly only after user code finishes, then release before returning.
type StringReplySlot = std::cell::RefCell<StringReplyState>;
```

Bound registry access to the legal owner-thread/GIL callback phases. A global
registry transporting raw pointers needs an explicit unsafe transport/access
argument; do not add blanket Send/Sync to RefCell or captured user closures.
An owner-thread registry may avoid that transport. Choose and justify the
narrow mechanism in F11; this is not a general threading certificate.

The host's immediate copy finishes before its next ordinary callback. Obtain a
new snapshot from user `get` code before borrowing/mutating the private reply
state. Do not hold a mutable slot borrow or mutex across user getter/setter/
on-changed code. This accommodates synchronous nested getter calls whose native
copy completes before returning to the outer call; it is not a general reentry
or threading certification. Keep the previous publication alive until the new
owned result is ready. Clear/replace snapshots only in legal native phases.

On the selected host contract, the one snapshot can remain until the next getter
or until the adapter closes. Setters leave the publication untouched. The
examined hosts finish the comparison copy before invoking a setter; no
pending-copy tracker is proposed. A user setter can mutate
before returning an error today; F11 must not silently claim transactional rollback
or redesign that behavior. Same-value SET may perform a getter comparison without
calling the setter; acceptance must include that path.

Use initialized module-string objects with the SDS-backed representation expected
by the native getter. SDK creation and ordinary setter inputs meet this selected
contract; arbitrary native object encodings/raw constructors are not certified.
Preserve current strict UTF-8/CString/NUL/default conversion policy and custom
validation; a broader binary/text configuration redesign is separate.

### Lifecycle required for cached native owners

A cache alone leaves a final native reference behind when its image unloads.
A bare successful-unload hook misses failed OnLoad. A failed/canceled unload
must retain the active state. Therefore the proposal includes narrowly scoped
string-adapter lifecycle coordination:

1. Enter a load scope after successful native SDK initialization, before string
   registration/user init. Track only adapters from that scope. A successful
   load commits them; an error return closes their cached replies while native
   API access is still legal. Post-load host rejection follows OnUnload.
2. On successful OnUnload, close adapters after user deinit succeeds. If user
   deinit returns an error, retain the active registry/publications untouched.
   Host refusals before OnUnload likewise leave them intact.
3. Closing is idempotent: set `closed` before releasing the cached native owner.
   A late native getter against stable closed private data returns NULL; its
   setter rejects without invoking user state. No new snapshot is published.
   The native getModuleStringConfig wrapper maps a NULL callback pointer to
   NULL SDS (7.2.8 module.c:12550–12553; pinned 8.10.2:14116–14120).
   All three examined consumers handle that NULL: GET maps it to an empty SDS,
   SET comparison checks it before comparing, and REWRITE's
   rewriteConfigSdsOption marks the option processed and omits its line.
   This is closed-state robustness, not a change to ordinary replies or a
   claim that hosts expose a callback after successful unload. Exercise this
   branch with pure state probes, not invented native callback contexts.
4. Keep registered callback private-data storage valid through native config
   removal. Do **not** free its Box merely because OnLoad/OnUnload has returned:
   native removal follows that return and other cleanup can still run. Scope
   closure releases the added cached native owner, not an unproven callback
   allocation. Check release before library close with corrected fixtures;
   examined native removal itself does not invoke a config callback.

Native load error bypasses OnUnload; successful OnLoad followed by host rejection
uses forced unload. Successful unload invokes OnUnload before removing configs
and closing the dynamic library. Native `moduleListFree` frees native config
metadata/name/alias, not SDK privdata. The current SDK `Box::into_raw` private data
already has an unrelated per-registration allocation leak. Retain stable retired
adapter metadata through removal, with no published native snapshot; full userdata/
closure allocation reclamation is explicitly deferred. This prevents claiming
that F11 fixes all configuration memory. It trades a bounded existing metadata
residual for eliminating unbounded getter-snapshot growth and releasing the new
cached snapshot at lifecycle close. Captured user resources/static backing values
remain outside that cache-only accounting.

Stable private data is also a correctness invariant for any registry pointers
used to close these adapters. Any future Box reclamation must retire those
registry entries before freeing the allocation, as well as establish the native
removal/quiescence barrier. Do not invent one, free storage early, or expand into
a general callback audit to force completion. If the narrow implementation cannot
establish these state/ordering requirements, stop at a reviewable blocker rather
than ship an ambiguous cache. Process shutdown/static Rust destructor behavior
is not a lifecycle release proof.

### Registration API and raw entry points

Recommend `register_string_configuration -> Result<(), RedisError>` for admission
and native registration failure; leave bool/numeric/enum signatures unchanged.
Track native-success registrations only. A rejected registration never transfers
private-data ownership to native code, so its temporary allocation can be dropped
immediately. An untracked raw call must return an error before native registration
rather than create unmanaged cached state. Preserve existing name/default CString
policy; no extra NUL conversion change belongs to this repair.

The declarative macro automatically supplies the load/close hooks and propagates
string registration failures with match/log/return-ERR inside its c_int-returning
OnLoad, rather than the `?` used in a Result-returning helper. The guard must
span user init because stream-filter
registers a string directly inside init. Existing macro configuration declarations
and seven event-stream trait implementations need no signature migration.

For custom exported OnLoad/OnUnload outside the macro, propose an explicit unsafe
lifecycle entry/finish/close recipe with valid native phase/host context obligations;
Context::new/dummy are not proof. The recipe must handle every failure return and
canceled unload, be module/load isolated, and store no context past that callback.
Raw entry-point authors incur this deliberate source migration. Check for an
already active load/image registry; do not clear another live module's state.
Two independently loaded fixture images must demonstrate isolation; do not claim
arbitrary multiple logical modules sharing one SDK image are certified.

Proposed direct caller migration, **not compiled**:

```rust
register_string_configuration(/* existing arguments */)
    .map_err(|error| error.to_string())?;
```

This fits stream-filter's `Result<(), String>` init helper. The helper remains
inside the macro-managed load scope. Do not edit its original working directory.
Result propagation and lifecycle recipes must be validated in isolated snapshots
before a release choice; no package/MSRV/server support commitment is selected.

## Impact inventory and alternatives

Tracked Rust inventory: SDK 75, Carolina 23, event-stream 37, JMESPath 35,
stream-filter 40 (210 total). Literal matches are not AST/exhaustive resolution.

- SDK: two explicit string backing impls (RedisGILGuard<String>, Mutex<String>),
  one generic `T: Clone` guard impl also supplies native RedisString values;
  getter/setter/registration/error adapter and one declarative registration call.
  Example has three string declarations; existing integration behavior checks.
- Event-stream: seven custom ConfigurationValue<RedisString> getters/setters and
  seven macro string declarations; each getter creates an owned native string.
  Validate same-value SET, errors and normal capture in isolated snapshots.
- Stream-filter: one direct registration, Mutex<String> sidecar-prefix backing,
  and manual LoadConfigs inside a macro init callback. Result must be propagated.
- No string-config uses selected in Carolina/JMESPath; their non-string config
  paths remain outside implementation scope. Downstream raw registrations/custom
  macros are unknown and must receive a documented migration recipe.

### Load-order evidence and fixture expectations

The macro's LoadConfigs call is inside its optional `configurations` expansion
(macros.rs:368–454), before user init when that block exists. Stream-filter's
actual invocation has **no configurations block** (src/lib.rs:104–119); its
manual call after registration is its first LoadConfigs call. Do not assert a
late-default bug for this reference. Keep its existing order and defaults.

Both native performModuleConfigSetDefaultFromName implementations dispatch
straight to setModuleStringConfig, without the sdsConfigSet getter comparison
(7.2.8 config.c:756–772; pinned 8.10.2 config.c:839–855). Explicit queued values
use the interface setter and can publish a snapshot through comparison during
load. The initial bound is therefore **zero or one** snapshot per registration,
not exactly one immediately after every successful load; fixture expectations
must record whether a getter was actually invoked.

For genuinely repeated/late calls, 7.2.8 loadModuleConfigs reapplies a missing
default; 8.10.2 skips missing defaults once MODULE_CONFIGS_DEFAULTS is set
(module.c:12565–12591 and 14150–14180 respectively). This version distinction
does not apply to stream-filter's first call. Do not reorder init, redesign late
registration or repair repeated-load defaults in F11.

The current macro ignores the LoadConfigs result. Failed first initialization
can subsequently cause the host's post-load configs_initialized rejection and
forced OnUnload (7.2.8 module.c:12250–12265; pinned 8.10.2:13735–13749).
Preserve that existing result policy in this repair; do not promise all invalid
values follow one path or change how load errors surface. The new registration
Result propagation is a distinct admission contract.

| Option | Tradeoff |
|---|---|
| Continue `.take()` | Loses one release responsibility per owned getter snapshot |
| Drop temporary / return its pointer | Cannot supply a live owner for native post-return consumption |
| Per-get retain without later owner release | Still grows the outstanding reference count |
| Redesign all getters around static borrowed native values | Large migration; Rust String/Mutex backings and custom text getters still need persistent conversion storage |
| One private reply owner plus narrow lifecycle close | Recommended; preserves traits, adds registration/lifecycle handling, bounded snapshot ownership; existing metadata allocation remains separate |
| Reclaim every config privdata/closure allocation now | Needs a full removal/quiescence contract across types; outside this narrow repair |

## F11 corrected-code acceptance criteria

F10 does **none** of the following executions; these are future gates.

- Pure state tests with Rust-only drop probes: replace one published owner,
  preserve active state on canceled unload, scope failure closes only its own
  adapters, repeated close is idempotent, closed adapters never publish/call user
  code, closed getter returns a NULL callback pointer/closed setter rejects
  without a user call (no SDS/native conversion executed by the pure probe),
  and nested getter completion leaves the outer snapshot alive. No Redis
  API pointer installation or fake native context in pure checks.
- Compiler controls/positive migrations: existing custom traits and macro string
  declarations compile; direct Result caller migration compiles; raw lifecycle
  entry requires unsafe; examples/consumer registration adapters remain coherent.
  Proposed shapes are not currently exported/compiled.
- Corrected isolated config-only host fixtures, Redis 7.2.8/8.10.2 debug/release:
  repeated ordinary GETs and same-value SET comparisons create/replace at most
  one adapter snapshot per registration; zero before the first getter is valid.
  Read bytes after getter return; final
  close releases its cached reference. Use initialized real API forwarding/counts
  and owned server identity/log/library hashes. Count owned snapshots/API calls,
  not a historical leak sampler or total process heap.
- Exercise native RedisString, Rust String/Mutex and one custom string backing;
  getter refresh after direct backing update, empty/Unicode/integer-looking and
  binary bytes where the current backing policy permits them; preserve strict
  parsing, default/macro argument behavior, flags, error and on-changed counts.
  Existing NUL-panic/text-policy gaps are deferred, never invoked as crash tests.
- Normal invalid-value rejection with recovery, successful/failed setters and
  same-value short-circuit behavior. Preserve temporary setter retention and
  transferred error-string cleanup; do not alter scalar configuration behavior.
- Real CONFIG REWRITE using a temporary config file on the owned fixture server,
  validating effective data before/after and return-message release accounting.
  Normal startup/LOADEX defaults and arguments, documented registration failure
  (valid native OnLoad), failure after successful registration and a read,
  post-load rejection/forced unload, successful unload, canceled unload and
  permitted later reload. No raw invalid pointers/context or crash path.
  Native removal after OnLoad/OnUnload return is not a host callback test window;
  closed-adapter behavior is covered by the pure state gates above.
- Close/release before code unload; distinguish active snapshot count from
  deliberately retained userdata metadata and captured user/static resources.
  Never claim all module memory is reclaimed. Refusal due to type/timer/blocked
  client must not clear active state; select only legal minimal fixtures.
- SDK macros plus isolated event-stream/stream-filter snapshots (F07/F09
  prerequisites); fresh archive consumers and relevant pure/docs/examples/
  Clippy/fmt checks. Hash source around commands and check actual dependency graph.
  Original directories unchanged. No full product suite/platform certification.

## Independent review dispositions

Initial Due/Claude review returned needs_changes. Caller verified and corrected
its unreachable native pending-removal gate, added registry retirement before
future storage reclamation, source-checked all three NULL consumers, and made
interior mutability/macro Result handling explicit. Two reviewer assumptions
were rejected with source evidence: default loading invokes the native setter
directly, and stream-filter has no macro configurations block. Initial snapshots
are zero or one, and the reference manual LoadConfigs is its first call. The
focused delta review approves. Caller applied nonblocking wording/citation
clarifications; a suggested new permission gate is already covered by Josh's
existing justified-API-break and issue-visibility authorization. No F11 code
starts in this activation. Registry transport remains an explicit implementation
justification, not a blanket unsafe Send/Sync mandate. The reviewer's two
outside-cwd reference reads were denied; retained caller excerpts/hashes supply
that evidence without permission expansion. Execution success alone is not
review approval. Compact evidence records actual outcomes and limitations.

## Overlap, process and provenance

Refreshed 55 upstream open issues, 45 open PRs and three fork drafts. No competing
string-getter ownership repair found by title/body inspection. Upstream issue
358 is the broader string/context redesign; leaves it open. Issue 471 reports an
enum c_void cast concern, which this string-getter proposal neither validates nor
resolves; leaves it open. PR 229 is the broad configuration API origin, not a
competing getter repair. F09 draft 3 is a dependency, not scope to expand.

PR 487 is now closed (head c7ca18e4a6fce2f2f32e390f39abef4d67e1c9c6).
PR 488 is still open and advanced to 2ad5362919fbfa3d7e85e33f72f2cdb6da587934.
F09's original INFO attribution/reviewed heads remain accurate historical records;
these INFO PRs do not repair the config getter. Do not repeat their implementation.

F10 branch docs/f10-configuration-getter-scope starts from clean F09 merge
02660b2d0e07d33fa91808ca0ae9000235ccfcc9; documentation/evidence only, local reviewed
no-ff merge, no push/PR/issue publication. F11 must refresh overlap and map the
partial tracked issue scope, create/push its dedicated review baseline/empty
kickoff/scoped draft BEFORE implementation. Record ancestor-history upload;
never push fork master or merge/release on GitHub. CI remains deferred.

Primary sources: src/configuration.rs; src/macros.rs; src/context/thread_safe.rs;
[7.2.8 module.c](https://github.com/redis/redis/blob/7.2.8/src/module.c) and
[config.c](https://github.com/redis/redis/blob/7.2.8/src/config.c);
[pinned 8.10.2 module.c](https://github.com/redis/redis/blob/498ecd0d6d007db11ddb3aea9428552598a78622/src/module.c)
and [config.c](https://github.com/redis/redis/blob/498ecd0d6d007db11ddb3aea9428552598a78622/src/config.c).
Retained hashes, inventory, overlap snapshots and independent review are under
target/f10; compact finalized evidence accompanies this specification.
