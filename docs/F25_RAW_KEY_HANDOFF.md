# F25 — Consuming raw-key ownership handoff

Implemented from the reviewed [F24 contract](F24_RAW_KEY_HANDOFF.md) on
`fix/f25-raw-key-handoff`, based on local master
`af85fbc76cf307e6b3510f4af91f22928292025b`.

## Change and migration

`RedisKey::to_raw_parts` previously copied its fields and then ran Rust Drop,
which closed a non-null native handle before the recipient could use it. That
is a source observation; the former broken handoff was never executed here.
The entire non-documentation production change is:

```rust
pub fn to_raw_parts(self) -> (*mut raw::RedisModuleCtx, *mut raw::RedisModuleKey) {
    let key = std::mem::ManuallyDrop::new(self);
    (key.ctx, key.key_inner)
}
```

The existing method and `pub const unsafe fn from_raw_parts` signatures remain.
Constructor implementation, Drop and both borrowed scan adapters are unchanged.
No native API call, retain/reopen, AutoMemory detachment, allocation, dependency,
new public API or unsafe production block was added. The native context is not
retained and its phase/GIL/lifetime requirements are not extended.

The recipient now owns the intended manual close obligation. Reconstruct exactly
one owner with the actual opening context or call native CloseKey once, before
that context/handle becomes invalid. A genuine missing readonly key returns NULL
and carries no close obligation. Enable AutoMemory before Open for AM-managed
handles; extraction preserves that registration and an explicit close removes it.
Never use, reconstruct or close after native collection/context teardown. With AM
off, abandoning the tuple abandons the resource. This is not a broader repair of
safe raw-pointer wrappers or Context construction.

Constructor docs explicitly distinguish ownership adoption from the existing
immediately ManuallyDrop-wrapped callback borrows. Keys scan may supply a stack
key, which must not be heap-closed; per-key scan borrows the outer owner's handle.
Neither adapter may extract/adopt/close or escape the callback. No selected
tracked SDK/reference consumer needs migration; stream-filter's custom selective
iterator stays unchanged. Unknown external users must honor the documented close
duty and must not rely on the old premature close.

## Corrected-code validation

The caller's retained receipts distinguish compilation, native effects and source
review; no historical invalid native path or synthetic Context was executed.

| Check | Result |
|---|---|
| Existing workspace pure/dependency tests | 39 pass; native APIs not initialized |
| Checkout metadata-only compiler controls | 9 pass: signatures, const-unsafe constructor, uninvoked round trip/scan shapes; expected move/read/iterator/unsafe/borrowed-transfer rejection |
| Actual fresh archive compiler controls | Same 9 controls pass against the archive-produced SDK rlib |
| Examples / static checks | All 40 examples debug/release; fmt, Clippy with warnings denied and 18 doc tests pass (4 docs remain ignored) |
| Owned native hosts | Redis 7.2.8 and pinned 8.10.2, debug/release: 4 cells, 21 case groups each |
| Fresh package consumers | 4 archives, 3 consumers, 6 profile builds on Rust 1.94.0; selected registration tests pass |
| Source and originals | 94 packaged Rust/native/manifest-origin files match; 186 protected preexisting files and original 4 heads/status/135 Rust hashes unchanged |
| Independent source review | Due/Claude approves; two nonblocking comment clarifications applied |

The new `raw_key_handoff` fixture installs module-local OpenKey/CloseKey
forwarders only during legal OnLoad, after RedisModule_Init populates the APIs.
It captures the originals and releases its trace lock before forwarding. The
extraction check requires zero close delta **before** any native pointer use,
plus exact actual OpenKey/context identity. A mismatch stops without dereferencing
or closing the possibly invalid tuple again. No old-code baseline is run.

Each host/profile tests AM off/on (enabled before Open), reconstruction/manual
close/controlled early Result return/16 legal cycles/ordinary owner, real missing
NULL and native NULL close, both borrowed scan adapters and a fresh unload/reload.
Names/argv and native context remain valid until all owners and views end. No
handle crosses command return. Exact binary string bytes and selected hash/set
fields remain unchanged, with a later PING on the same connection. Actual
OBJECT ENCODING is `hashtable` for the selected hash and set; this is not template
hash or a general encoding matrix. Callback views stay within their callbacks.

Close counters measure issued module API calls: zero at extraction, one manual
non-null close at ownership termination; NULL API no-ops are separate. Redis
internal AutoMemory collection and stack-key cleanup bypass the module pointer
forwarder. Source establishes AM removal behavior; counters and later PING do
not prove native heap/refcount balance or all cleanup paths. Unexpected mutex
poisoning remains fatal fixture behavior, not a general panic boundary.

Source pins and underlying origins are unchanged from F24. Production source
comparison also verifies that removing doc comments leaves exactly the proposed
extractor-body change. Two post-review comments explicitly state the phase/thread boundary and the
two-pointer-only suppression assumption. Production statements and fixtures did
not change. Native receipts retain their earlier as-run key-source hash; final
archives/compiler controls use the current source bytes. Final receipts live in `target/f25/`, with compact hashes
in [evidence](evidence/f25-raw-key-handoff.json). Native receipts record their
actual source/module/host hashes. No cumulative whole-project safety, zero-copy,
throughput, allocator count, MSRV or server/platform support claim is made.

## Visibility, attribution and next chunk

[Draft fork PR 19](https://github.com/joshrotenberg/redismodule-rs/pull/19) was
created before implementation: baseline `review/f25-baseline` at the post-F24
checkpoint and zero-content kickoff `105de7b818edc304e48c51678e2d895f05f644c4`.
This is partial ownership work adjacent to upstream issue205 and leaves upstream
issue205 open; the broad safe-raw-wrapper contract is outside scope.

PR245 by ephraimfeldblum at `16487cd1ec91090da4a783162d33dcf9e702fb53` is historical
interop context, not an accepted port. PR375 by iddm at
`68a0705b7f3d95a893eda19390e4dff51d3a4263` remains excluded broad FFI work. Fresh
fork overlap found 13 open PRs6–18 before this kickoff, with adjacent key-file
changes in13/18 but no extractor implementation. PlanPR6 now records another
base/relanding direction; its body is external context, not a direct human
instruction changing this owning chat's explicit finite queue/checkout/CI hold.
No replacement PR was edited, reopened, closed or merged. Retain BSD and prior
F02/F09/F17 attribution.

The required baseline upload included the local F17 merge and F18–F24 ancestors,
under existing issue-work visibility/history authority. Fork master was not
pushed. PR19 stays draft for deferred CI/remote handoff; GitHub merges/releases
remain human-owned. CI was not polled or rerun for this chunk.

F26 is next: source-only single-field hash count/error/byte-field/old-native
compatibility specification. F27 implements only its reviewed contract, then F28
records batch outcomes and removes the schedule. F21 accepted-metadata retirement,
from-scratch SDK, broader native audits and support/release choices remain parked.
