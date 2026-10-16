# F27 — Single-field hash counts and byte fields

Implementation checkpoint on `fix/f27-hash-results-and-bytes`, draft
[PR20](https://github.com/joshrotenberg/redismodule-rs/pull/20). **Incomplete: the
required Redis 6.0.20 runtime gate is blocked. Do not merge or advance to F28.**
Source review and selected modern runtime results cannot replace that gate.

## Implemented contract and migration

The selected wrappers implement [F26](F26_HASH_CONTRACT.md): native HashSet's
preexisting-field replacement/deletion count becomes `Result<usize, RedisError>`.
Insertion returns `Ok(0)`; replacing even identical bytes returns `Ok(1)`;
present/missing deletion returns `Ok(1)`/`Ok(0)`. This is not HSET's inserted count
or a changed-byte count. Counts describe physical native storage, including
lazily expired fields; paired fixtures deliberately have no field TTL.

`RedisKeyWritable::hash_set(&self, &str, RedisString)` still consumes its value;
`hash_del(&self, &str)` now also returns Result. Text fields containing interior
NUL return an error before native mutation/key preflight. The consuming setter
still drops its value normally after returning the checked result. `hash_set_by_string` borrows field and
value; `hash_del_by_string` borrows field. Empty/Unicode/NUL/non-UTF8 bytes remain
intact. The existing getter and ownership protocol are unchanged.

```rust,ignore
// Before: Status was interpreted from a native count.
let status = key.hash_set("field", value);
// After: propagate the error, then interpret the native replacement count.
let replaced = key.hash_set("field", value)?;
// Reusable binary owners:
let replaced = key.hash_set_by_string(&field, &value)?;
let deleted = key.hash_del_by_string(&field)?;
```

These are alternative call-site shapes, not sequential use of the consumed
`value`. Direct raw wrappers are now unsafe: callers must establish a genuine
live writable handle, valid strings and native phase/thread/GIL/alias duties.
NULL and the setter's DELETE sentinel reject before native work; arbitrary stale
pointers are outside the contract. No raw-readonly fixture violates this admission.

Required HashSet/KeyType availability and raw EMPTY/HASH type are checked before
mutation. Unknown native integer types return an error without enum unwrap.
Optional GetServerVersion >= packed `0x00060200` chooses modern errno; absent/older
uses the restricted legacy contract. There is no Context/INFO fallback or cache.
Modern zero captures errno immediately before cleanup/formatting/other APIs:
ENOENT is successful zero, every other errno (including zero) is an error.
Positive one ignores stale errno; restricted legacy zero/one ignores errno.
Negative or greater-than-one counts are errors. One fixed NONE/CFIELDS call,
DELETE only for deletion, NULL terminator; no COUNT_ALL/NX/XX/TTL/batch options.

No retain, stored Context, new dependency, automatic replication, native owner
protocol, getter rewrite or support-floor selection. Stream-filter's replicated
RM_Call HSET/HDEL and selective iterator bodies remain its author policy.

## Visibility and attribution

The dedicated baseline is clean post-F26 `7929be9af6593f948e210cbcfe87edda5d6ab749`.
Empty kickoff `28fe134622266f6aeb6270f5cfd1f7878c91fdfb` and draft20 existed before
code. The required baseline upload includes F25 merge/F26 ancestors under existing
issue-visibility/history authority; fork master was not pushed.

Partial upstream scope: leaves #134 open, leaves #246 open, leaves #205 open.
PR247 by tBuLi12, refreshed head `99b56630f7575e20f55a8601d01d3353732199f3`, is
motivation/adaptation context. Its old CString/Status patch is not an accepted full
port. Fork PR18 has adjacent availability edits, historical PR13 adjacent string
ownership/getters; neither supplies this focused count/byte/legacy implementation.
PR375's broader FFI work remains excluded. Inherited BSD/F02/F09/F17 credits remain.
No changes to other PRs, CI, publishing, merges/releases or original modules.

## Fresh validation and limits

- 43 pure tests pass (37 SDK + six dependency controls), including four new
  production policy/ABI probes. Scalars only; no Redis API initialization or fake
  Context/RedisKey. Legacy status and modern errno branches are pure evidence.
- Eleven checkout and eleven actual-archive metadata-only controls pass: Result
  signatures, unsafe raw admission, borrowed reuse, consumed-value rejection and
  ignored-Result rejection. No uninvoked function is executed.
- All 41 examples build in debug/release, Clippy with warnings denied, formatting
  and 18 doc tests pass (four ignored). These are not runtime coverage.
- Four corrected owned 7.2.8/8.10.2 host/profile cells pass, 11 case groups each:
  exact empty/Unicode/NUL/non-UTF8 fields/values, insert/replacement/same/delete
  counts, missing/last-field removal, HGET/HEXISTS/HLEN, actual listpack/hashtable,
  NUL/wrong-type errors, owned getter after CloseKey in the same phase, input owner
  reuse, later PING and fresh legal reload. No TTL or replication claim.
- Two isolated stream-filter builds and four owned host/profile cells (eight
  selected flows) pass. Prior pointer/ownership/builder/fallible-load prerequisites
  are adapted only in the snapshot, with explicit map_err preserving its
  Result<(), String>. Binary registry create/get/replace/NONE/destroy and both
  typed binary XRANGE engines pass. Original HSET/HDEL source is byte-identical.
- Four fresh archives and three public consumers/six profile builds pass. Final
  archive controls use `packages-final`; 96 Rust/native/manifest-origin files match
  current sources. Generated packaged Cargo.lock is dependency metadata, not a
  byte-identical source-origin file.
- Independent Due/Claude source review approves (true exit0, 251 complete trace
  records, 46 matched successful activities, no failed activities). Caller
  validated request/result/answer/trace schemas and unchanged frozen source inputs.
  Two nonblocking findings were addressed: exact 6.2.0 source evidence and stricter
  error assertions with a fresh four-cell runtime run. Edition2024 unsafe-block
  styling and runner portability remain deferred; no platform migration selected.

The exact modern threshold is additionally source-verified against official
6.2.0 commit `445aa844b946a8f1bc21ac8554b44adb1ecb4018`, module blob
`fe0cd8345c0d2532480413f3174b4bb03abdf91c`: zero sets ENOENT. This is source-only.

The official 6.0.20 source builds its old Apple configuration branch against the
current macOS SDK, selecting removed `stat64`/`fstat64`; compilation fails in
replication.c (actual make exit2). No module/native test ran on 6.0.20. No server
patch, platform-header workaround or support-floor change was made. A genuinely
available compatible old host/build is required before completion. Old source
proof and scalar policy tests do not establish old runtime behavior.

Initial evidence retains a fixture macro-arity compilation error, an isolated
snapshot missing unsafe block caught by compilation, and a reference runner's
incorrect XRANGE expectation (the author protocol is `[cursor, entries]`). These
were corrected before passing receipts; the failed legal reference run shut down
its owned host normally. No historical invalid SDK/native path was executed.

Final runtime receipts are `target/f27/runtime-reviewed/report.json`; reference
receipts are `target/f27/references/report.json` and `reference-runtime-final/`.
Earlier receipts retain as-run hashes. Historical ignored target evidence through
F25 was absent at F26 activation and is not claimed freshly reverified here.
The owned 7.2.8 release-tarball build reports the enclosing SDK kickoff Git ID
`28fe1346`; that field is not a Redis source revision. Its official archive digest
and module.c match the retained F26 pin, and actual binary hashes are retained.
Selected macOS arm64/libc host coverage, issued native counts and source/compiler
proof are not heap/refcount balance, general memory/FFI safety, throughput,
zero-copy or product/platform support. F21 reclamation and broader native audits
remain parked. F27 carries forward; only F28 remains after its gates pass.
