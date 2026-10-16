# F26 — Single-field hash results and binary fields

Source-only specification from post-F25 `a6b6735d3b5331c3d5294fd1a60dd3fec612e8f2`.
No proposed API below is implemented, compiled or native-tested in F26. F27 is
one later implementation chunk, gated by this review and visible tracked-issue
work. F28 remains the final batch scorecard. F21 retirement and CI stay parked.

## Decision and exact boundary

Replace the selected hash mutators' misleading `Status` with
`Result<usize, RedisError>` and add existing-string field setter/deleter methods.
The count is **native preexisting fields replaced/deleted**, always zero or one
for this API. It is **not HSET's newly inserted field count**, a changed-byte
count or proof of replication. On hash-field-expiration hosts, physically stored
but lazily expired fields can still count as replaced/deleted even though HGET
reports absence; this is native storage behavior, not a logical-live-field count.
A new field is `Ok(0)`; replacing an existing
field (including the same value) is `Ok(1)`; deletion is `Ok(1)` if present and
`Ok(0)` if absent. No COUNT_ALL, NX, XX, TTL option or variadic batch interface.

Use a narrow, explicit writable-key precondition for the raw mutators. Make the
two existing raw wrappers and two new raw existing-string wrappers `unsafe`.
This is a justified Rust source break: on pre-6.2 hosts a rejected read-only
write and successful insertion both return zero without defined errno, and the
opaque handle exposes no API to inspect its actual open mode. Typed
`RedisKeyWritable` constructors already request ReadWrite and can discharge that
precondition. Do not read native struct layouts or invent a mode probe by writing.

The raw validity contract does not prove the SDK's general Context/key/alias
safety. It is local to these calls; existing raw getters and other wrappers are
outside this repair. Wrong-type keys remain ordinary errors, not a safety
precondition. NULL is rejected before dereference/native HashSet. A non-NULL
stale pointer is never an ordinary error case or a test input.

## What the current source actually does

`src/raw.rs:622–656` creates a CString with unwrap, calls native HashSet with
CFIELDS and one field/value pair, and converts the returned integer to Status.
`src/key.rs:315–321` exposes that Status. Native zero/one are not OK/ERR status
codes, so updates/deletions can be reported as failure and some native failures
as success. This is a source observation; no historical path was executed.

The existing `hash_get_by_string` on both key types already takes `&RedisString`
and supports byte fields; its returned string is adopted as an owned result.
The new write side must match that field representation, without rewriting the
getter, its owner protocol or strict text extraction. Existing readonly
`hash_get` also accepts AsRef bytes but CFIELDS still rejects interior NUL;
F26/F27 do not imply a complete binary getter audit.

Tracked-source inventory is retained in `target/f26/inventory.json`: selected
mutators have no direct author-module callers in the four original trees.
`examples/response.rs` and `examples/string_ownership.rs` use the existing-string
getter; direct new fixture commands can pair with it. Macro HashSet fields and
client redis::cmd calls are lexical matches, not SDK mutator consumers.

Stream-filter `src/native.rs:188–219` deliberately calls HSET/HDEL with replication;
its option builder sets replicate. Keep these command bodies, their returned
HDEL inserted/deleted protocol and their AOF/replica behavior unchanged. The
other references have client-side hash setup or no selected mutation API use.
No original migration is required. Read-only original heads/status and 135
tracked Rust file hashes were captured anew; no F26 reference builds ran.

## Pinned native proof and compatibility

Fresh official source pins are listed in the compact evidence and the durable
[bounded excerpts](evidence/f26-hash-source-excerpts.md). `module.c` and `t_hash.c`
were inspected for Redis 4.0.14, 5.0.14, 6.0.20, 6.2.19, 7.2.8 and 8.10.2. These
are source comparison points, not a new supported-version matrix.

| Native family | Success count with this fixed flag subset | Failure information |
|---|---|---|
| 4.0.14 / 5.0.14 / 6.0.20 | HashSet adds hashTypeSet's 0-insert/1-update or hashTypeDelete's 0/1 | Read-only/wrong-type returns 0; no HashSet errno contract; NULL is dereferenced |
| 6.2.19 / 7.2.8 / 8.10.2 | Same counts when COUNT_ALL is unset | Zero sets EINVAL (NULL/flags), ENOTSUP (type), EBADF (mode), or ENOENT (no counted field, successful) |

The broad native pre-6.2 warning is real. It does not require a new server floor
for the restricted operation once the mode/type/NULL ambiguity is removed.
All selected old bodies reject only nonwritable/nonhash keys before the fixed
one-field operation; all selected hashTypeSet bodies return zero on insertion
and one on replacement. The body may copy field/value bytes; caller strings are
borrowed, not transferred or retained by the SDK. CFIELDS transfers its own
transient field SDS internally, not the caller's Rust text. Deleting the last
field follows native moduleDelKeyIfEmpty; preserve that behavior.

Proposed common raw-call sequence:

1. Validate text field CString fallibly where applicable; reject interior NUL
   without native work. Validate required raw inputs (NULL key/field/value)
   before a dereference; arbitrary invalid pointers remain outside contract.
2. Resolve HashSet and KeyType optional APIs before mutation. Missing API is a
   named RedisError. No global API replacement for tests. Snapshot optional
   GetServerVersion; no Context allocation, INFO call or stored version cache.
3. Under the caller's live writable-handle/native phase/GIL obligations, inspect
   KeyType as a raw native integer. Compare with EMPTY/HASH constants only;
   never use `raw::KeyType::from`, whose enum conversion unwrap can panic on
   new type codes (the header already defines GCRA=8). All other codes return
   an ordinary wrong-type error. This removes the old wrong-type ambiguity.
   Typed constructors justify WRITE; raw callers must establish it themselves.
4. If GetServerVersion exists and returns at least packed `0x00060200`, use the
   modern errno policy. If absent or older, use the conservative legacy policy.
   Absence does not claim an exact server version: the restricted legacy path
   is valid for the selected modern pins too given the same writable/type
   preconditions. An unrecognized/non-Redis host is not newly certified.
5. Invoke native HashSet once, with NONE for existing RedisString fields or
   CFIELDS for text, one field/value (or native DELETE sentinel), then a NULL
   variadic terminator. Never pass COUNT_ALL or arbitrary flags.
6. Immediately capture OS errno on a modern zero return, before formatting,
   CString destruction, RedisString drop, any other API or cleanup. Positive
   counts ignore stale errno. Legacy mode ignores errno entirely, including
   stale values; it has no usable failure meaning on these source pins.
7. Modern `(0, ENOENT)` becomes `Ok(0)`; modern `(0, anything else)` is an error
   (including unexpected zero errno). Modern 1 is `Ok(1)`. Legacy 0/1 are
   `Ok(count)` after the established preconditions. Negative or >1 returns are
   unexpected-result errors; never feed a count through `Status::from`.

GetServerVersion is present in the selected 6.0+ sources and absent in selected
4/5 sources. It returns a packed integer without Context. The earlier version
branch needs no new API or server version assertion. No missing API fallback
may mutate the key to determine capabilities. No silent new support floor.

### Proposed public surface (not implemented)

```rust
// Existing typed argument shapes retained; return type deliberately changes.
fn hash_set(&self, field: &str, value: RedisString) -> Result<usize, RedisError>;
fn hash_del(&self, field: &str) -> Result<usize, RedisError>;

// Add to RedisKeyWritable; match the existing typed field getter.
fn hash_set_by_string(&self, field: &RedisString, value: &RedisString)
    -> Result<usize, RedisError>;
fn hash_del_by_string(&self, field: &RedisString) -> Result<usize, RedisError>;
```

Raw functions use their existing pointer/text arguments, return the same Result,
and become unsafe. New raw `hash_set_by_string` / `hash_del_by_string` likewise
are unsafe, use string pointers and return Result. Safety docs require live
writable key, real compatible native phase/thread/GIL and valid live field/value
strings until return, no invalidation or conflicting native aliases. The public
setter must reject the native DELETE sentinel as a value input (it is a distinct
deleter operation); do not expose arbitrary flags. Keep the internal sentinel
spelling ABI-correct: the pure check must establish
`REDISMODULE_HASH_DELETE as usize == 1`, not assume bindgen opaque alignment.
This check invokes no native API. Raw pointer validity
cannot be checked by Rust; the `unsafe` boundary is admission, not validation.

The typed old setter still consumes its RedisString, as before, and drops it
only after the checked native result/errno capture. The additive setter borrows
both owners; caller can reuse them after return. Neither creates a lasting native
borrow, new owner protocol, stored Context, dependency or lifetime extension.

## Migration choices and before/after

Chosen: visible Result source break plus unsafe raw admission. Retaining Status
adapters would hide count semantics and detailed errors; on old raw read-only
handles it cannot promise a corrected success mapping. Returning Unknown would
spread native ambiguity into typed author code despite its WRITE guarantee.
Version-gating all calls to 6.2 would silently narrow compatibility. Raw field
layout inspection or re-opening needs a broader contract. None is selected.

Existing source pattern (illustrative consumer of current API, not an observed
original-module mutator call):

```rust
match key.hash_set("field", value) {
    raw::Status::Ok => Ok(()),
    raw::Status::Err => Err(RedisError::Str("hash write failed")),
}
```

Proposed equivalent success/error handling, not compiled:

```rust
key.hash_set("field", value)?;
Ok(())
```

Proposed binary field use, not compiled:

```rust
let field = RedisString::from_bytes(b"field\0\xff");
let value = RedisString::from_bytes(b"value\0\xff");
let replaced = key.hash_set_by_string(&field, &value)?;
let fetched = key.hash_get_by_string(&field)?;
let deleted = key.hash_del_by_string(&field)?;
```

Document `replaced == 0` on insertion and `1` on replacement; it is not an
HSET command result. Use String::try_from only for callers choosing strict text.
Text wrappers keep accepting &str (including &&str coercions); embedded NUL
changes from panic to Err. New byte methods accept NUL/non-UTF8 exactly. Values
already support arbitrary bytes. No arbitrary generic conversion/batch API.
Raw migrations must add an actual safety justification around the call and
handle Result, not merely wrap arbitrary pointers in unsafe. Ignored Result
is caught by an explicit deny(unused_must_use) consumer gate; no general lint
policy changes. No code-size/allocation/performance claim from these snippets.

## Overlap, credit and implementation visibility

Fresh open upstream inventory has 45 PRs and 55 issues. The binary field portion
partially maps to upstream [134](https://github.com/RedisLabsModules/redismodule-rs/issues/134)
and [246](https://github.com/RedisLabsModules/redismodule-rs/issues/246). The narrow
unsafe raw-mutator admission also partially maps to
[205](https://github.com/RedisLabsModules/redismodule-rs/issues/205). Leaves all
three open. Issue54 is historical broader API inventory, not accepted new scope.
No separate hash-count issue is manufactured. Issue471 remains excluded.

Credit [PR247](https://github.com/RedisLabsModules/redismodule-rs/pull/247) by
**tBuLi12**, refreshed exact head `99b56630f7575e20f55a8601d01d3353732199f3`, as
binary-API motivation/adaptation context. Its byte wrapper still uses CString
and Status and therefore does not implement this NUL/count contract. Do not
port its broad StringDMA/read/lib conversion changes or resurrect obsolete
RedisString ownership. PR375 at refreshed head `68a0705b7f3d95a893eda19390e4dff51d3a4263` remains
excluded as broad raw/FFI work; preserve
its separate overlap context and inherited BSD/F02/F09/F17 credit.

Fork PR18 directly touches the existing HashSet call sites for generic missing-
API diagnostics, while PR13 touches adjacent ownership/getters. PR19 is completed
F25. This is file/site overlap to reconcile before F27; PR18 does not implement
count/errno/byte/legacy semantics. Do not import its global API macros or change
other PRs. External planPR6 is not new human authority for this owning checkout.
No focused count/byte repair worker was found; refresh immediately before code.

Before F27: clean post-F26 `review/f27-baseline`, dedicated
`fix/f27-hash-results-and-bytes`, zero-content kickoff, push baseline/kickoff and
open scoped draft listing **upstream134, upstream246 and upstream205** with exact
PR247 credit, changes/migration and validation plan. Baseline uploads intervening
F25 merge/F26 specification ancestors under existing issue-visibility/history
authority; record actual ancestry. Verify visibility before implementation. If
history authority, visibility or ownership remains unresolved, stop code and
report. Keep draft throughout validation/CI hold. Never push master, merge on
GitHub, alter release/settings or close unrelated issues/PRs.

## F27 corrected-code acceptance gates (all pending)

1. **Production pure policy:** actual classifier/preflight helpers tested with
   scalar facts/closures only: old and modern branches, optional version/API,
   count0/1/negative/>1, modern ENOENT/ENOTSUP/EBADF/EINVAL/unexpected0, positive
   stale errno, legacy stale errno ignored, raw type/NULL validation (including
   unknown type codes such as 8 and an arbitrary future code), text NUL rejection
   and ordinary text/empty acceptance. Capture errno before any cleanup; prove
   source call ordering. Do not initialize native pointers, fake Context/Key,
   invoke raw FFI or relabel a standalone model as production coverage.
2. **Metadata-only public controls:** exact Result/signatures, typed borrowed
   field/value reuse and existing consuming setter, raw unsafe required,
   ignored Result rejected with deny(unused_must_use), mismatch/borrow misuse
   rejected; no executed native setup. Same controls against fresh actual archive.
3. **Legal native fixtures:** owned 7.2.8 and 8.10.2 debug/release, plus a focused
   owned 6.0.20 compatibility cell using corrected code. New isolated write
   command registrations; real writable handles and retained argv/name/input
   owners. New/update-same/update-other/delete-present/delete-absent/last-field
   removal/missing-key/wrong-type; text and byte-field controls for empty,
   Unicode, NUL, non-UTF8, exact value bytes and no content change on preflight
   error. Pair with existing-string getter and ordinary HGET/HEXISTS/HLEN, check
   same-connection later PING and fresh legal reload. These count/HEXISTS
   pairings use no field TTL; do not assert logical-existence equivalence for
   lazily expired stored fields. Record actual encodings
   (ziplist/listpack/hashtable as applicable); template/expiry behavior remains
   native policy and is not a full matrix. No direct raw read-only write since
   the new unsafe admission excludes it; pure modern EBADF classifier evidence
   is not an exercised invalid raw-call test. Missing APIs/NULL rejection use
   pure checks only; no fabricated objects or global pointer mutation.
4. **Older compatibility limit:** if a genuine 6.0.20 host cannot be prepared
   without unrelated server/platform changes, retain the named runtime gate
   and F27 checkpoint. Do not silently claim old runtime coverage or change
   the support floor. 4/5 source proof remains source-only unless separately
   justified within this chunk; no unnecessary host matrix expansion.
5. **Effects/ownership:** paired values remain usable until drop, owned getter
   result valid after handles close within a legal phase; no retained raw field
   views across mutation. Source confirms caller input ownership and no SDK
   retain/transfer. Native behavior/counts do not certify heap/refcount balance,
   persistence or replication. No RM_Call rewrite in stream-filter and no new
   auto replication. Native stored/command/default/expiry policy stays intact.
6. **Archive/source/reference:** relevant pure/examples/docs/fmt/Clippy, fresh
   source-verified archives/public consumer both profiles, isolated selected
   stream-filter binary registry flows with existing F07/F09/F11/F13/F14/F17/F19
   prerequisites and dependency/source/patch receipts; leave its HSET/HDEL
   replicate bodies unchanged. Other original code needs no direct migration;
   verify original heads/status/hashes. No need to rebuild unrelated references
   solely to populate counts. Independent Due other-provider source review and
   caller contract/effect checks before local no-ff merge.

Any unresolved native source contradiction, old-host ambiguity, overlap ownership,
missing independent review or incorrect Result/count meaning stops F27 completion.
No historical broken Status/CString execution, fake Context, arbitrary callbacks,
raw invalid pointer, crash/leak sampler, broader key/DMA/Context/GIL/thread/alias/
reentry/panic audit, metadata retirement or new support/release policy.

## F26 evidence limits

The repository's entire ignored target directory was absent at activation.
F25's committed implementation and compact evidence remain, but old trace/runtime/
archive files and checkpoint were not available for renewed verification. Do not
recreate their claimed passes. F26 uses fresh pinned sources and original/tree
hash snapshots and never reruns completed SDK tests. A first Python HTTPS source
read failed local CA verification; authenticated gh content reads then retrieved
pinned blobs with normal TLS, without certificate bypass or permission changes.
A first snapshot script used a relative parent incorrectly; fixed path resolution
before writing the baseline. These are caller acquisition failures, not SDK failures.

The app schedule still named F25 at activation; local STATUS/committed F25 take
precedence. App request delivery/feedback reconciliation is recorded in the new
checkpoint. No new schedule or new backlog is authorized by that stale prompt.

Independent Due/Claude source review approves (exit0); four nonblocking findings
were reconciled: raw integer type validation, physical-expiry count wording,
explicit sentinel value and final review metadata. Caller checked these against
actual source. No second unchanged review or SDK execution followed. Reviewer
did not read originals or recompute source hashes; caller verified those separately.
