# F24 — Raw-key handoff specification

Source-only scope at clean post-F23 master
`6a21613cbbaa22a78887cba5e4d78151957ed60d`, following the finite F22 roadmap.
This document proposes F25; no SDK implementation, compiler/test/benchmark or
native execution occurred in F24. Original reference checkouts remain read-only.

## Decision and exact repair

Preserve the existing public signatures:

```rust
pub fn to_raw_parts(self) -> (*mut raw::RedisModuleCtx, *mut raw::RedisModuleKey)
pub const unsafe fn from_raw_parts(
    ctx: *mut raw::RedisModuleCtx,
    key_inner: *mut raw::RedisModuleKey,
) -> Self
```

The current consuming `to_raw_parts` copies two raw pointers into its result, then
ordinary Rust destruction runs `RedisKey::drop`, which closes a non-null native
key. That contradicts its documented transfer intent. This conclusion comes from
source; the former dangling-handle path was not executed.

**Proposed implementation, not compiled or applied:**

```rust
pub fn to_raw_parts(self) -> (*mut raw::RedisModuleCtx, *mut raw::RedisModuleKey) {
    let key = std::mem::ManuallyDrop::new(self);
    (key.ctx, key.key_inner)
}
```

This suppresses the Rust owner's destructor while returning exactly the actual
stored context/key pointers. It does not invoke a Redis API, allocate, retain,
reopen, change native modes, alter AutoMemory registration or extend any lifetime.
The tuple is a transfer of the existing manual close obligation, subject to native
cleanup deadlines. It is not an independently retained native owner like F09's
string contract. Return of raw pointers remains safe; use/adoption obligations
are explicit. This does not repair the broad safe raw-pointer wrappers in issue205.

No `into_raw_parts` alias/rename, deprecation, public field, `as_ptr` accessor,
Result, new lifetime parameter, writable-key interop, generic transfer engine or
new dependency is selected. `mem::forget` after copying would work, but
ManuallyDrop expresses suppression before field access. Nulling a field before
ordinary Drop is unnecessary. Future owned Rust fields added to RedisKey require
reassessing this whole-owner suppression; the current struct contains two pointers.

## Native proof and its boundaries

Both retained native source pins establish the following, with fresh byte checks
against the origins recorded in F22:

| Obligation | Redis 7.2.8 module.c | Pinned 8.10.2 module.c |
|---|---|---|
| Initialize actual context, DB, name ref and mode | 3996–4005 | 4148–4157 |
| Open: readonly missing returns NULL; otherwise allocate and register AM key | 4036–4060 | 4189–4214 |
| Close: NULL is a no-op; non-null cleanup, AM removal, then free | 4102–4107 | 4257–4262 |
| AM admission only when enabled; early free removes queue entry | 2479–2524 | 2612–2657 |
| AM collection closes remaining keys, disabling bookkeeping during collection | 2527–2550 | 2660–2685 |
| Context teardown invokes AM collection | 808–814 | 875–881 |
| Keys scan creates stack key and cleans it after callback | 10895–10909 | 12173–12187 |
| Per-key scan calls back with the supplied key | 11009–11028, 11078 onward | 12288 onward, 12368 onward |

The actual native key also stores its context. `CloseKey` uses that stored
context; the separate tuple context is used by SDK string getters/adoption.
Passing a different context does not fix or renew the native key's lifetime.
No context is refcounted/retained by the tuple. Even with AM disabled, native
key/context/DB/phase validity must hold until use and close. Never store these
pointers for a later command, thread, callback or after context cleanup on the
strength of this API. The specification grants no such lifetime extension.

AM is a fallback cleanup registration, not a second independent manual owner:

- Enable AM before opening for an AM-managed recipe. Extracting must leave the
  registration intact. Within the same legal command phase, either reconstruct
  one Rust closer or manually call native CloseKey once. That close removes the
  AM entry, so later collection does not close that registration again.
- With AM off, the recipient must arrange exactly one close of a non-null handle
  while its native context remains valid. Forgetting raw ownership leaks the
  obligation; suppressing Drop does not make abandonment correct.
- AM only tracks additions made while enabled. Enabling it after opening must
  not be documented as retroactively registering the existing handle.
- If an AM-managed raw handle is deliberately left for collection, all access
  ends before that phase finishes, and it must never later be reconstructed or
  manually closed. This is a native fact, not the recommended F25 fixture recipe.
  F25 validates explicit same-phase close in both AM modes, not an abandoned raw
  pointer or a post-return dereference.

The caller must end outstanding SDK borrows/iterators and native users before
transfer or close. Rust rejects consuming a key while a later-used borrow exists;
raw aliases remain a caller obligation. No DMA/value mutation, key deletion during
iteration, cross-thread, reentry or broader Context/GIL soundness audit is included.
F23's Stop-before-Close behavior stays intact.

## Constructor documentation and the two borrowed exceptions

F25 must replace the incomplete `from_raw_parts` safety text with two explicit
use modes, without changing its implementation or const signature:

1. **Adopt a close obligation or a supported empty result:** actual opening/producer
   context remains valid. A non-null handle is live, originated from a compatible
   native opening and has not been closed or collected; caller relinquishes its
   sole manual close obligation to the returned RedisKey. Alternatively, the NULL
   result of a readonly missing-key open carries no close obligation. Both cases
   require the actual valid opening context. Existing native modes, legal
   phase/GIL and AM coordination still apply. Do not reconstruct twice.
2. **Internal callback borrow:** a native-owned handle can be represented only by
   an immediately `ManuallyDrop`-wrapped adapter, with no Rust closing destructor,
   no extraction/ownership adoption and no escape beyond the callback. This is
   the existing scan-adapter pattern, not a new public borrowed-key abstraction.

The two existing adapters must retain their non-closing behavior:

- `src/context/keys_cursor.rs:28–35`: the native Keys scan uses a **stack key**.
  Redis calls internal `moduleCloseKey` after the callback, not heap-freeing
  `RM_CloseKey`. Its Rust adapter must not close, free or adopt that pointer.
- `src/context/key_cursor.rs:93`: per-key scanning borrows the outer cursor's
  owned key. That outer owner ultimately closes it. A callback adapter does not
  obtain a second close obligation. Preserve the actual opening context and
  optional set value handling from F09.

A normal Rust owner of a missing readonly key may carry `(actual_ctx, NULL)`.
The context must still be an actual valid supplied context; NULL here means the
key result, not permission to invent a NULL Context. `is_null`/selected empty
operations remain available, and SDK Drop skips native CloseKey for NULL. Native
CloseKey itself accepts NULL, so a manual-close recipe may forward NULL once as a
no-op; distinguish API calls from actual released handles. No new blanket
NULL acceptance for arbitrary getters or constructors is implied.

## Inventory and migration

The selected source inventory searches tracked Rust in the SDK and all four
original reference checkouts; compact evidence retains lexical matches and hashes.

| Site | Meaning / F25 action |
|---|---|
| `src/key.rs:59–103,222–228` | Two-pointer owner, native opener, unsafe constructor, consuming extractor and Drop. Fix only extractor body plus precise extractor/constructor docs. |
| `src/raw.rs:348–361`; `src/context/mod.rs:631–652` | Close/Open helpers and context factories. No implementation/signature changes; broader unsafe raw-wrapper issue remains open. |
| Two scan adapters above | Borrowed ManuallyDrop wrappers, not consumers of transfer. Keep source behavior and verify source/compiler/native selected controls. |
| `examples/stream_cleanup.rs:195` | F23's exclusive real raw-open adoption. Already closes exactly once; do not replace it with a transfer fixture or rerun all F23 checks. |
| Stream-filter `src/iter.rs:270,345` | Custom selective native Open/Stop/Close owner; no SDK extraction/adoption call. Preserve original, do not impose eager SDK iteration or migration. |
| Carolina | Four lexical `RedisKeySnapshot` occurrences are unrelated application DTO names. |
| Event-stream / JMESPath | No selected raw extraction/adoption uses found in tracked Rust. |

No selected current SDK or original-reference call uses `RedisKey::to_raw_parts`
outside its definition; F23's metadata rejection control is retained evidence,
not executed transfer. No existing module migration is required. Unknown external
consumers keep source signatures but receive the intended live-handle transfer.
They must discharge their close obligation; code that relied on premature close
or independently duplicated the old pointer needs correction, not preservation.
Discarding an AM-disabled transferred handle without closing abandons the resource;
with AM enabled, native collection can still close its registration. Neither case
justifies using the handle beyond its valid phase.
The old extractor docs' “only way” cleanup wording must be removed: either one
reconstructed owner **or** one valid manual native close is allowed.

**Proposed legal recipe, uncompiled and unexecuted in F24:**

```rust
fn round_trip(ctx: &Context, name: &RedisString) -> bool {
    let key = ctx.open_key(name);
    let (opening_ctx, native_key) = key.to_raw_parts();
    // Same legal command phase; transfer only this manual close obligation.
    let key = unsafe { RedisKey::from_raw_parts(opening_ctx, native_key) };
    !key.is_null() // key closes on scope exit; NULL needs no native close.
}
```

Imports are omitted in this excerpt. In AM mode, call `ctx.auto_memory()` before
opening; do not let either raw parts or reconstructed key survive command return.
The alternate manual recipe calls real native CloseKey once and never adopts
or uses the pointer afterward. No API to detach native AM registration is added.

## F25 corrected-code acceptance gates — not run in F24

1. **Source/compatibility:** public signatures and two-pointer payload unchanged;
   no extraction-time native call, new unsafe implementation block, allocation,
   retained context or new API. Exact raw tuple equality within the legal command
   phase. Preserve both borrowed scan wrappers and native phase constraints.
2. **Pure/compiler:** run relevant existing pure regression tests without native
   APIs. Do not fabricate RedisKey/Context values merely to unit-test Drop. A
   standalone ManuallyDrop DropProbe would demonstrate std mechanics, not this
   production method; do not count that as repair validation or invent a generic
   production helper solely to test it. Metadata-only uninvoked consumers check
   exact tuple/const-unsafe constructor compatibility, consume-once rejection,
   rejection of extraction while a read/iterator borrow is subsequently used,
   unsafe-constructor requirement and existing scan callback signatures. Negative
   cases use a compile-only harness that requires the expected rejection/diagnostic;
   positive controls compile to metadata without linking/running a program. Neither
   kind executes native APIs or a broken historical transfer.
3. **Corrected isolated host fixture:** owned 7.2.8/8.10.2, debug/release, AM off/on
   enabled before opening. Real command context, initialized native APIs and a
   unique key per case. Extract → legal native KeyType/use → reconstruct → scope
   exit; extract → explicit native close; reconstruct and return early through a
   controlled Result error. Retain key-name/argv owners through these operations.
   No key mutation/aliasing/close while a borrow or stream iterator is live.
4. **Issued-close accounting:** install a module-local forwarding CloseKey wrapper
   only at legal OnLoad, after RedisModule_Init has populated the APIs. Capture
   the original CloseKey pointer before installing the fixture-only forwarder;
   SDK production code does not swap pointers. Record a zero close delta immediately after extraction
   and before native use; compare actual OpenKey handle identity in that same
   phase, then exactly one non-null close at reconstruction Drop/manual close.
   Keep counters separate for NULL API calls and real non-null handles. Repeated
   legal open/transfer/close cycles followed by PING establish selected host
   behavior, not a leak sampler or allocator/refcount proof. Redis core's own AM
   collection calls its internal function directly, not the module's pointer
   wrapper: a wrapper count cannot prove complete native AM/free balance. Source
   confirms removal; explicit-close runtime results and later PING are separate.
5. **NULL and regression controls:** real readonly missing-key Open returns NULL;
   extraction preserves `(actual_ctx,NULL)`; reconstruction/is_null/scope exit
   issues no CloseKey; optional direct native CloseKey(NULL) is documented no-op.
   Existing-key ordinary non-transferred owner still closes once. Missing keys
   never use fabricated pointers. Exact ordinary value/type and later PING on the
   same connection stay correct. No live handle is carried into a later command.
6. **Borrowed adapters:** source checks keep both ManuallyDrop wrappers. Selected
   legal Keys scan and ScanKey callbacks use callback-only views, with no early
   close from either adapter. A CloseKey forwarding count can observe SDK closes,
   but not native stack-key internal cleanup; label the evidence accordingly.
   Use known corrected simple encodings (with actual encoding evidence), not a
   general scan/RESP/alias audit or template-hash coverage claim.
7. **Packaging/review:** relevant examples, docs, fmt/Clippy, fresh source-matching
   archives and public metadata consumers, independent other-provider source
   review plus caller effects/contract checks. No original module changes or
   unnecessary reference rebuild where there is no direct migration. No historical
   invalid execution, fake Context, arbitrary callback, assertion/crash/leak path.

Stop if source/runtime ownership or AM/context proof fails, callbacks gain close
ownership, the corrected fixture would need an invalid phase, archive/source
provenance mismatches, or independent review is missing. Selected pinned-source
proof supports this narrow design; F25 runtime gates remain pending. No new server
support floor, MSRV, package identity, release, performance or general safety
commitment follows.

## Overlap, history authority and sequencing

Upstream issue [205](https://github.com/RedisLabsModules/redismodule-rs/issues/205)
is a broader safe-raw-pointer soundness report. F25 is a partial adjacent ownership
repair and **leaves upstream205 open**. PR
[245](https://github.com/RedisLabsModules/redismodule-rs/pull/245), ephraimfeldblum,
refreshed head `16487cd1ec91090da4a783162d33dcf9e702fb53`, publicizes `key_inner` and
also changes CI/setup/example files. It is historical interop context, not an
accepted port; copy none of those unrelated changes. PR375 at the refreshed
head recorded in compact evidence touches key/raw/status helpers but its key patch
does not implement this handoff. Do not port that broad work.

The fresh fork snapshot has nine open drafts: 2,3,4,6–11. Prior drafts1/5 are now
closed, unmerged; 9/11 describe replacements. New plan draft6 discusses rebuilding
the F-series on another base and contains unresolved decisions. These PR bodies
are external project context, not direct human instructions to change this owning
chat's authorized hybrid checkout, CI hold or finite queue. No focused raw-key
implementation is present in the refreshed snapshots. No SDK publication happens
in F24. Do not reopen/close/merge others' PRs or duplicate replacement work.

Before F25 code, refresh remote overlap **again**, reconcile latest direct human
focus and any concurrently owned raw-key work, then establish `review/f25-baseline`
at the clean post-F24 checkpoint and `fix/f25-raw-key-handoff` with a zero-content
kickoff. Push the required baseline/kickoff and open a scoped draft listing every
mapped issue (currently upstream205), this narrow plan and PR245 attribution/context.
That baseline uploads intervening local ancestors (including the local F17 merge
and F18–F24), under existing issue-visibility/history authorization. This is not a
fork-master push. If current scope/history authority or draft visibility cannot
be reconciled, retain the blocker and do not implement invisibly. Keep the draft
through validation and deferred handoff. GitHub merges/releases remain human-owned.

After reviewed F24 local no-ff merge, F25 is next; F26/F27 hash scope/repair and F28
batch scorecard/schedule completion remain the finite queue. F21 reclamation, CI,
from-scratch SDK and broad native audits remain parked. No second chunk this run.
