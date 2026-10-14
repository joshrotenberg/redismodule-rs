# F11: owned string configuration replies

String configuration getters now keep their returned RedisString owner alive
while Redis copies its bytes after the callback. Each registered string retains
zero or one reply snapshot. A later getter replaces/releases the prior snapshot;
failed load and successful unload release the final cached owner. Setter input
retention and transferred error messages remain separate native contracts.

## Implementation and author experience

The private adapter now uses the real owner-preserving path:

```rust
private_data
    .reply
    .read(|| private_data.value.get_val(), RedisString::as_ptr)
    .unwrap_or(std::ptr::null_mut())
```

The prior `.get(...).take()` transferred a reference the native getter never
released. The new read obtains the user result before borrowing the cache, keeps
that owner after return, and drops the replaced owner outside the slot borrow.
Closed reads skip user code; the setter uses the same pure closed-state dispatch
principle. The cache itself adds no per-get retain or byte copy. Custom getters
still allocate/copy as before; no zero-allocation or throughput claim is made.

ConfigurationValue traits and macro declarations remain intact. Direct string
registration now returns Result, with native errno in rejection diagnostics.
The isolated stream-filter migration is one error-propagation change:

```rust
register_string_configuration(/* existing arguments */)
    .map_err(|error| error.to_string())?;
```

This snippet abbreviates arguments; the actual isolated helper compiles. Macro
OnLoad uses match/log/return-ERR instead of `?`. Bool/numeric/enum registration
signatures, strict UTF-8/default CString policy, command behavior, and existing
LoadConfigs ordering/result policy are unchanged. Event-stream's seven custom
string configuration implementations need no F11 trait migration.

## Lifecycle and raw module migration

The standard macro enters StringConfigurationLoadScope after native initialization,
spans later errors and user init/direct registrations, and commits immediately
before returning OK. Its pending guard closes only its own accepted adapters on
error. Successful OnUnload closes after user deinit; failed deinit/refused unload
leaves publications active. Native post-load rejection follows real OnUnload.

The registry is owner-thread TLS holding shared Rc reply states, with brief
RefCell borrows; no Send/Sync implementation or raw-pointer registry transport is
added. A callback context is retained only for admission during that OnLoad and
cleared at commit. Raw exported callbacks require an explicit unsafe recipe:

1. After successful native initialization in the actual owner-thread OnLoad,
   call unsafe StringConfigurationLoadScope::begin with that live Context.
2. Keep the guard through every registration/user-init error return. Propagate
   every string registration Err into an OnLoad ERR, never continue with an
   admitted-but-closed defensive adapter.
3. Call unsafe scope.commit immediately before returning OK. A guard must not
   escape its callback or owner thread.
4. In real successful OnUnload, after user deinit succeeds, call unsafe
   close_string_configurations. Refusal/error must leave the registry intact.

A fabricated Context, GIL token or post-callback pointer does not establish these
phases. One live module per SDK image is supported by this coordinator. Registry
load/unload and selected native fixtures use the actual owner thread/GIL; no
broader worker-thread/config-callback or context/GIL/panic/reentry contract is
certified. Native refusals due to types/timers/blocked clients were not separately exercised.
TLS image pinning/destructor behavior is platform dependent. Observed
reload works on the tested macOS arm64 hosts; this is not platform support policy.

Native accepted private-data Boxes remain stable through removal, as required by
the reviewed specification. The inherited per-registration metadata/closure
allocation residual is deliberately deferred. Cache closure releases its owned
native reply; it does not reclaim all module memory or captured/static backing
resources. Full userdata reclamation would require registry retirement and a
proven native removal/quiescence barrier. Native rejection drops only the
unaccepted Box. General FFI/context/alias/thread audits remain separate.

## Validation and practical limits

Final results and artifact hashes are recorded in compact evidence and target/f11.
Independent reviews read/search only; the caller owns execution/provenance checks.

- Six pure Rust state tests cover replacement/drop ordering, closed getter and
  setter user-code gates, closure during get, nested completion, scope isolation,
  and scope-owned closure. No native API pointers are installed; canceled unload
  is proved by the actual native refusal case, not a construction-only unit test.
- Seven compiler controls check unsafe begin/commit/close, non-Send/non-Sync guard,
  custom trait/direct Result migration and raw lifecycle recipe. Compiler-only
  function bodies are not executed.
- Corrected isolated native fixtures exercise native/Rust-text/custom-binary
  backings across Redis 7.2.8/8.10.2, debug/release: repeated GET, same-value SET,
  empty/Unicode/numeric-looking/binary values, strict-text/reject errors/recovery,
  direct backing refresh, CONFIG REWRITE, canceled/successful unload, two-image
  isolation, reload, failed OnLoad after read, duplicate rejection, invalid LOADEX
  forced unload, configured LOADEX and actual rewritten-file startup.
- The fixture records freshly owned returned objects and real SDK FreeString
  forwarding. Issued minus released remains three while all three configs have
  publications; final close logs zero. Static native backing is excluded. This
  proves selected owner accounting, not process heap/native refcount/all memory.
  Each cell has 225 repeated GETs; same-value SET invokes two observed getters
  (backup and comparison), each with balanced replacement.
- Existing configuration behavior is checked separately on both hosts/profiles,
  including native/GIL-String/Mutex-String and unchanged scalar declarations.
- Two isolated references build in both profiles and run eight selected
  host/profile flows. Stream-filter retains its first manual LoadConfigs/default
  behavior; event-stream keeps ordinary capture and custom config behavior.
  All four original reference directories remain unchanged. No full module suite
  or downstream migration is imposed.
- Four fresh archives, six consumers and twelve builds use Rust 1.94.0/1.98.1;
  production archive bytes match the final sources. This is packaging/consumer
  compile evidence; archived runtime flows are not re-certified in this chunk.
  All 33 examples build in both profiles, Clippy/fmt/docs and Python tests pass.

Retained failed attempts: the first macro guard placement failed all-target
compilation before native execution; first native assertion incorrectly assumed
one getter per same-value SET; first compiler control missed dependency search;
first retained reference copy lost executable mode; a log-hardening edit
double-sliced the startup tail. Each is corrected and fresh
validation retained. One interim native run used prior library bytes after a
nonbehavioral diagnostic/doc edit; final rebuilt cells supersede it. No historic
invalid native path, fabricated host callback, crash or leak sampler was run.

## Process and next direction

Fork draft PR 4 was opened on a pushed zero-content kickoff before implementation.
Its review baseline 25b0c2e3c438c5fe6d78aedfa8687844fa66a59c uploads the local F09
merge/F10 specification ancestors; fork master remains unpushed. Leaves upstream
issue 358 open; issue 471 is outside scope. Preserve F09's inherited PR 487
jonnyomerredis/head c7ca18e4a6fce2f2f32e390f39abef4d67e1c9c6 and PR 488
oshadmi/historical head 01ff52181d9d2efd136b1ef38143ad05223adca6 credit. CI stays
deferred, and drafts 1–4 remain open for the remote handoff. No GitHub merge,
release, MSRV, package identity or server/platform commitment is made.

The proposed next focus is a bounded configuration API ergonomics specification,
using real reference patterns and before/after examples; implementation remains a
separate chunk. Josh can steer that focus or explicitly resume the deferred CI
workstream. F11 validation takes precedence over any next work.
