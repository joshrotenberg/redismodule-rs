# F22 — Next SDK improvements and first repair specification

2026-10-15. Source-only planning from clean post-F21 master
`0f4ab807ebe82954371a8db902bbae04cde12563`. Josh explicitly asked to dig deeply,
plan the next few SDK improvements, schedule them and work through them. This
supersedes F21 standby. It does not resume CI or configuration metadata reclamation.
The first bounded chunk is this roadmap plus the stream repair specification.
Proposed code below is not implemented, compiled or run in F22.

## Chosen sequence

| Order | Improvement | Why now | Done when |
|---|---|---|---|
| 1 | Stop stream iterators correctly and stop traversal before mutation in the stream example | A concrete mismatched native operation and an adjacent real example lifetime; native stop has an available boundary, unlike F21 metadata retirement | Corrected early-drop/exhaustion/restart behavior, unchanged stream bytes, stop-before-close and a legal unchanged STREAM_POP protocol are independently checked |
| 2 | Make consuming raw-key extraction transfer the existing close obligation | `to_raw_parts(self)` currently returns fields and then runs `Drop`; the advertised ownership handoff is contradicted by the body | One owner before extraction, zero premature closes, exactly one close after legal reconstruction/manual cleanup, explicit context/AM/exclusivity obligations |
| 3 | Correct the single-field hash result contract and add a consistent existing-string field path for writes/deletion | Native HashSet returns a count, while the SDK treats it as Status; the byte-capable read path has no corresponding writer, and C fields cannot represent NUL | A reviewed count/error/old-host policy, exact-byte field/value checks, typed write/delete ergonomics and preserved command/persistence/replication choices |

This is a finite three-improvement batch. Correctness comes before convenience;
no whole-project speed or general FFI safety claim is planned. Rust source/compiler
contracts, native effects, resource-operation counters and measured author code
must be reported separately. Timing/allocation benchmarks are not necessary to
show these repairs; add them only to substantiate a specific claimed gain.

## Why these instead of other backlog

The prior intake's AF04, AF02 and AF10 already identify stream teardown, raw key
extraction and binary direct-key contracts. Current source still supports these
narrow candidates. F15's reply conversion and LoadConfigs priorities are already
implemented by F17/F19, so they are not candidates again.

| Candidate | Disposition and evidence |
|---|---|
| Stream teardown (AF04) | First. `src/stream.rs:96` calls Delete in Drop; both pinned native implementations distinguish Delete from Stop. Actual `examples/stream.rs` retains the iterator across trim. |
| Raw key extraction (AF02) | Second. `src/key.rs:100` returns a raw tuple without suppressing owner Drop at line 222. Public unsafe reconstruction already exists. This is not a general raw-key or Context audit. |
| Single-field hash (AF10 / PR247 topic) | Third, separate scope before code. `src/raw.rs:622,639` use CFIELDS/CString and convert native counts to Status. Existing `hash_get_by_string` already accepts binary fields; do not claim all binary hashes are unsupported. |
| RDB binary write issue271 | Not selected: `save_slice`, `write_slice` and `read_string_buffer` already exist. The lossy text read is a distinct policy; do not invent a missing-byte API or change stored formats. |
| Error trait PR467 | Useful later, but current blanket `impl<T: Error> From<T> for RedisError` makes adding Error a coherence/migration task, not a one-line improvement. No error-prefix policy selected. |
| Allocator alignment PR483 / AF01 | Important separate layout/native-allocator scope, with active upstream overlap. Do not mix into stream/key work or infer arbitrary alignment from current host observations. |
| Timer/thread/context/event audit groups | Remain separate. They require callback identity, thread/phase and native lifetime proofs; no umbrella safety audit is authorized by this finite batch. |
| F21 accepted configuration metadata | Parked. Retain accepted boxes/captures/Rc; no available last-use/quiescence/destructor boundary has been proved. |
| CI / remote draft handoff | Held. Five existing drafts stay draft; no CI modernization or GitHub merge/release. |

## Source and author evidence

All four original modules were read-only. Head/status and 135 tracked Rust hashes
match F21. Selected observations are not a claim that every author uses each API:

* Stream-filter has its own `StreamKey` (`src/iter.rs:259–346`), uses Start/Stop
  and stops before CloseKey. It deliberately fetches fields selectively, whereas
  the SDK iterator materializes every field. Keep that implementation and behavior;
  do not migrate it to an eager SDK iterator to make an adoption statistic.
* SDK `examples/stream.rs` and `examples/string_ownership.rs:217` actually use
  the public stream iterator. These are the first repair's direct consumers.
* No `to_raw_parts` consumer was found in the selected SDK/example/test and four
  original tracked-Rust inventory beyond its definition. External callers still
  matter; a fresh public consumer is necessary for the handoff contract. Existing
  `from_raw_parts` scan adapters at `src/context/key_cursor.rs:93` and
  `src/context/keys_cursor.rs:28` immediately use `ManuallyDrop` for native-borrowed
  handles. F24 must preserve/classify these non-closing aliases separately from
  genuine close-owning reconstruction; a borrowed handle is not a fresh owner.
* Stream-filter `src/native.rs:175–219` uses typed RM_Call HGET/HSET/HDEL helpers,
  with write calls requesting replication. A future direct-key API does not
  automatically replace those commands or preserve replication/AOF semantics.
  Use their byte patterns in isolated fixtures; keep original paths unchanged.
* Event-stream/JMESPath/Carolina did not supply a selected direct single-field
  writer migration. No migration is required merely to use all four references.

Native source pins reused: Redis 7.2.8 release tarball and Redis 8.10.2 commit
`498ecd0d6d007db11ddb3aea9428552598a78622`. F22 rechecks relevant source hashes
against F21 provenance; existing origin verification is retained, not represented
as a new download/build. Selected host profiles are future validation targets,
not a new platform/server support policy.

## F23 — Implementation-ready narrow stream repair

### Current evidence and required behavior

`StreamIterator` borrows `&RedisKey`. A successful Start constructs it; failures
return Err without constructing an owner. Drop currently invokes
`RedisModule_StreamIteratorDelete`, which deletes the current entry on a writable
handle and does not stop traversal. Normal read-only handles instead reject Delete;
this is not a claim that ordinary read-only iterator drops currently delete data.
With the live stream value still present, `moduleCloseKey` frees a remaining
iterator inside its value/kv check, followed by an assertion that no iterator
remains (7.2:4084–4097; 8.10:4239–4252). This is not an unconditional cleanup
promise after arbitrary mutation/alias invalidation, or an unbounded process-leak
claim for ordinary legal traversal. Earlier release at iterator Drop matters for another
traversal on the same still-open key.

Both native pins have the same relevant behavior:

* Start rejects NULL/non-stream/already-active handles before allocation, and
  sets `key->iter` only on success (7.2 module.c:5563; 8.10:6151).
* Stop releases traversal storage and clears `key->iter`; its body allows read
  handles (7.2:5614; 8.10:6202). The adjacent comment mentions writing in EBADF,
  but the body has no write-mode check. Do not copy that comment as a requirement.
* Delete mutates the current entry and requires write mode (7.2:5755; 8.10:6343).
* NextField creates owned strings registered against the actual key context
  (7.2:5712; 8.10:6300). F09's existing adoption shim is already used; preserve it.
* Key close also frees an active iterator (7.2:4080–4107; 8.10:4235–4262).

Implement Drop with Stop, never Delete. Resolve all four required iterator APIs
(Start, Stop, NextID, NextField) before Start can acquire traversal, keeping the
resolved functions in private iterator state as needed. This avoids assuming
co-availability and avoids a later optional-pointer unwrap after acquisition.
A missing required iterator API returns a descriptive Err before acquisition. A failed Start must not stop another active iterator. Do not
mutate global native function pointers to test missing APIs. Keep public factory
signatures, `Iterator<Item = StreamRecord>`, ID/range/reverse/exclusive semantics,
field adoption and materialization unchanged. No new batch/borrowed-field API,
explicit-delete method, FusedIterator promise or generalized errno error redesign.
Stop's non-OK result cannot be returned from Drop; document the legal-handle
invariant and do not invent retry/panic behavior or a broad alias-safety guarantee.

The stream example reads two records to choose the trim boundary, but currently
opens a writer and trims while the iterator remains live. Correct it to end both
iterator and read-handle scopes before opening the writer. Preserve the selected
ID returned and nonempty trim semantics. The zero-record Null gate below is
an intended result: F23 validation later confirmed the old trim helper rejected
a zero native count, so that gate requires skipping the unnecessary empty trim.
Do not change
replication policy or interpret this as fixing all native handle aliases.

Current code (abbreviated, actual operations):

```rust
let mut iter = stream.get_stream_iterator(false)?;
let element = iter.next();
// Read second ID into id_to_keep.
let stream = ctx.open_key_writable(&stream_key);
stream.trim_stream_by_id(id_to_keep, false)?;
```

Proposed shape, not yet compiled:

```rust
let (element, id_to_keep) = {
    let stream = ctx.open_key(&stream_key);
    if stream.key_type() != KeyType::Stream {
        return Err(RedisError::WrongType);
    }
    let mut iter = stream.get_stream_iterator(false)?;
    let element = iter.next();
    let id_to_keep = iter.next().as_ref().map_or_else(
        || RedisModuleStreamID { ms: u64::MAX, seq: u64::MAX },
        |entry| entry.id,
    );
    (element, id_to_keep)
};
let stream = ctx.open_key_writable(&stream_key);
stream.trim_stream_by_id(id_to_keep, false)?;
```

The proposed block uses the existing example's imports and preserves its return
conversion after trim. It is not compiled here. The practical contract is
Stop -> Close(read) -> Open(write) -> Trim.

### Acceptance gates for F23 (not executed by F22)

1. **Source and compiler:** review exact Drop/call-order diff; uninvoked public
   consumers prove a traversal cannot outlive or consume its borrowed key and
   retains existing Item/range types. No fake key/Context or Redis pointers in pure
   tests. If API selection/cleanup logic is factored for testing, test actual
   production logic rather than a duplicate demonstration.
2. **Owned native hosts, corrected code only:** 7.2.8 and 8.10.2 debug/release;
   consume zero/one/all records then drop, early Result return, empty existing
   stream, forward/reverse and inclusive/exclusive range. Reopen iteration on the
   SAME still-open key after drop and after exhaustion. Compare IDs and exact
   field/value bytes including NUL and non-UTF8. Missing/wrong-type starts return
   errors; an ordinary second-start rejection must leave the first traversal usable.
3. **Teardown and non-mutation:** corrected isolated fixture records Stop before
   Close with legal wrappers forwarding to the actual APIs, with each accepted
   traversal stopped once and no Delete issued. Include a legally opened write
   handle adapted by the existing unsafe constructor with exclusive ownership and
   actual opening context, then prove drop leaves contents unchanged. Do not use
   the broken `to_raw_parts` path or fabricate handles. Operation counts are not
   full native heap/refcount accounting. Exclude mutation while an iterator lives.
4. **Actual example:** stop/read-close before writable trim; exercise STREAM_POP
   missing key -> WRONGTYPE (existing behavior), existing empty stream -> Null
   with unchanged XLEN, and one/many entry behavior. Check exact remaining IDs and
   a later PING on the same connection. Use corrected source; no execution of the former live-trim path.
5. **Regression and packaging:** relevant pure/docs/fmt/Clippy/example builds,
   selected existing F09 stream-owner lifetime fixture, fresh archived SDK/public
   consumer, provenance and independent other-provider review. No unchanged full
   reference/runtime replay. Stream-filter's custom path informs ordering but is
   not a repaired-SDK runtime proof. No allocation/throughput claim.

No exact open issue or competing focused stream repair was found in the refreshed
45 open upstream PRs/55 issues and five fork drafts. PR229's broad stream ancestry
is not an accepted patch or competing narrow cleanup implementation. Refresh again
before code. F23 is local issue-independent unless fresh scope maps to an issue;
then satisfy visibility before editing. Do not create an issue just to publish.

## F24/F25 — Raw-key handoff scope, then repair

F24 must pin the ownership proof before F25 edits implementation: consuming
`to_raw_parts(self)` currently returns `(ctx, key_inner)` then ordinary Drop closes
that key. Candidate repair suppresses that destructor (e.g. ManuallyDrop) while
preserving the existing public method name/signature. An idiomatic `into_raw_parts`
alias is optional and must earn its migration cost; it is not needed to fix the bug.
No public mutable field or new writable-key interop API is in scope.

The tuple transfers only the existing close obligation within a valid native
phase. It does not own/extend Context, disable native AutoMemory, make raw aliases
exclusive, or create cross-thread rights. Native OpenKey adds an AM key entry;
CloseKey removes it. Reconstruction must state actual producer context, exclusive
close ownership, valid handle or supported NULL case, no prior close/native AM
cleanup, and lifetime/phase obligations. A borrowed raw pointer cannot be adopted
as a fresh close-owning owner. The existing ManuallyDrop scan wrappers are
explicitly non-closing borrowed aliases and must remain so; include both adapters
in source/compiler controls if constructor documentation changes. Read-only
missing-key NULL remains valid. Do not infer a general
key soundness proof from suppressed Drop.

F24 must decide docs/API compatibility, native AM/non-AM recipes and direct
callers. F25 gates: production pure drop-policy checks only if meaningful;
uninvoked move/reuse consumers; corrected legal host extraction->native use->
reconstruction->single close and extraction->manual close, missing keys, AM on/off
within one valid command phase; no former dangling path executed; real close
forwarding counts and later PING; source-verified archive consumers and independent
review. Explicitly exclude general mutable native values/DMA aliases.

This partially relates to upstream issue205; PR245 by ephraimfeldblum at
`16487cd1ec91090da4a783162d33dcf9e702fb53` is historical accessor context, not a
patch accepted for porting. F25 requires refreshed overlap and dedicated clean
baseline/zero-content kickoff/push/scoped draft listing every scoped issue BEFORE
code. Document that the baseline uploads intervening local history. Existing
issue-visibility/history authority applies; if blocked, stop implementation.
Leaves upstream205 open. No fork-master push or GitHub merge/release.

## F26/F27 — Single-field hash contract scope, then implementation

F26 must settle one coherent result/byte contract before adding convenience.
Both pinned HashSet bodies return a count; without COUNT_ALL, newly inserted
fields produce 0/ENOENT, updates/deletions can produce 1. Wrong type/invalid key/
read-only handles also return 0 but with different errno. The current `.into()`
treats 0 as Ok and 1 as Err. This source-derived mismatch is not a runtime result.
Capture errno immediately where relevant; never infer failure from count alone.
New count-returning paths must never pass through `Status::from`. That conversion
unwraps enum decoding and would panic for counts >= 2; current single-field calls
cannot produce such a count. Keep any legacy Status adapter single-field only;
COUNT_ALL does not make a one-field operation return multiple fields, and a
multi-field expansion remains out of scope.

Compare additive existing-string field methods with changing text wrappers.
Recommended starting point: typed field/value borrows for writes/deletion matching
existing `hash_get_by_string`, explicit Result/count semantics, optional byte
convenience only if needed. Preserve old free-function consumers deliberately;
if legacy Status adapters are retained, specify their corrected mapping and
NUL policy rather than leaving contradictory hidden behavior. New methods must
not advertise Redis HSET's inserted-count protocol if they return native
updated-count or COUNT_ALL semantics. No automatic use of NX/XX/TTL options,
variadic batch expansion, stored-format or replication/AOF policy changes.

The native comments explicitly say pre-6.2 return semantics differ. F26 must
resolve version/capability handling from real sources without choosing a new
support floor or claiming that an existing compatibility feature proves runtime
behavior. If a safe compatible contract cannot be established, stop F27 code and
report the specific design decision/evidence gap. Do not silently restrict support.

F27 gates after that review: pure production count/errno and input classification;
public inference/borrow/Result compiler controls; corrected legal host create,
update/delete-existing/delete-missing/wrong-type behavior; exact empty/Unicode/NUL/
non-UTF8 fields and values using existing-string reads and ordinary HGET; source-
verified ownership of borrowed inputs/owned outputs; preserved text controls;
fresh archives/consumers, selected byte-pattern fixtures and independent review.
Measure equivalent formatted caller code including error handling if claiming
boilerplate savings. No forced replacement of stream-filter's replicated RM_Call
writers, no historical CString-panic path, no zero-copy/allocation/throughput claim.

Scope likely intersects upstream134/246 and PR247 by tBuLi12 at
`99b56630f7575e20f55a8601d01d3353732199f3`; refresh patch and exact issue mapping in
F26. Current API and native bodies supersede any stale patch assumptions. Keep
upstream credit if adapted. F27's mapped-issue draft must list every scoped issue
before code; leaves upstream134/246 open. This does not reopen issue471 or add a
full binary API audit. No exact narrow hash-count issue is manufactured here.

## Execution and stop rules

Use the existing two-hour owning-chat heartbeat, one bounded chunk per activation.
Cadence is not an execution deadline. Resume unfinished work; observe another
owner instead of duplicating it. The explicit queue is:

| Chunk | Deliverable | State after F22 |
|---|---|---|
| F22 | Deep roadmap + reviewed first stream specification | This activation |
| F23 | Stream cleanup and example repair, validation/review | Next |
| F24 | Raw-key handoff specification and visibility plan | Queued |
| F25 | Reviewed raw-key repair and validation | Gated by F24/visible draft |
| F26 | Single-field hash count/bytes/compatibility specification | Queued |
| F27 | Reviewed hash repair and validation | Gated by F26/visible draft |
| F28 | Concise results/migration scorecard using fresh receipts | Final batch checkpoint |

This is a queue, not a promise of seven successful two-hour completion times.
No new focus approval is needed for the selected bounded lanes. A real contract,
visibility or material policy blocker is reported rather than guessed. Do not let
F21's parked blocker prevent independent scheduled work. After F28, remove this
batch schedule and report completion; do not invent F29 or an endless audit.

Every interval: Done / Next / Blockers / Decisions, real or clearly proposed
snippet and practical improvement; actual Pacific timestamp as the final line.
Keep CI/from-scratch SDK/metadata reclamation held. No original migrations,
package/MSRV/server/platform support/release selections or global settings.
Tracked issue visibility applies before code; issue-independent work remains
reviewed local branches/no-ff merges. All model delegation uses Due; caller checks
contracts/effects, preserves native permissions and routes sanitized feedback to
due-expert. No retries/other delegation or historical invalid native execution.
