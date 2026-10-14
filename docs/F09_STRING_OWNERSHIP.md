# F09: independent RedisString ownership

The reviewed implementation and selected local validation are complete. Two
independent Due/Claude source reviews approve; the caller checked the actual
answers, changes and tests. Remote CI remains a deferred handoff on draft PR 3.

## Changed contract

RedisString holds one private NonNull native object pointer, with no cached
RedisModuleCtx. Its owner is 8 bytes on the tested macOS arm64 compiler; the
previous two-pointer source layout was 16 bytes. Option<RedisString> occupies
one pointer too. This is representation evidence, not a throughput claim.

Creation with from_bytes copies binary bytes once into a null-context native
owner. The context-accepting create/create_from_slice/from_raw_parts compatibility
entry points use that same independent ownership. create retains its existing
CString/interior-NUL policy; serde/strict UTF-8/numeric behavior stays unchanged.

```rust
let value = RedisString::from_bytes(b"a\0\xff");
let pointer = value.as_ptr(); // borrowed object pointer; no ownership transfer
```

Raw borrowed acquisition (retain_from_ptr) and owned adoption (from_owned_ptr)
are explicit unsafe operations. With an actual producer context, adoption retains
with NULL and immediately frees the original with that valid producer context,
resolving an AM entry whether AM is enabled or not. Without such an entry, one
manual reference is adopted directly. Drop releases with NULL. NonNull excludes
null; it does not prove provenance, initialization or native access legality.

Native-owned INFO fields are requested with NULL and adopted once. List/hash/
stream/current-user results normalize against their actual producer context;
RDB LoadString adopts its manual reference. Current user returns Option; its NULL
branch is purely checked without initializing APIs. No native absence query is
run on 7.2; no fake or presumed legal no-user context is created on 8.10.

Legacy new and from_redis_module_string are now unsafe shims; adoption is no
longer const. decode_args is unsafe, retains callback inputs without taking their
references, and both declarative/procedural adapters use narrow unsafe blocks.
Public inner reads become as_ptr; storage cannot be fabricated/written safely.
Consuming take uses ManuallyDrop, and defrag replacement remains crate-private.

Clone remains a deep native copy; safe_clone retains an alias and its Context
argument is advisory, not an unforgeable GIL/phase token. Native append refuses
shared strings. Correcting an accidental extra retain permits append to a fresh
unshared result; this preserves native policy rather than the old accounting bug.

Retain argv owners across a command boundary, then derive fresh views in a later
legal native phase. A buffer can be automatically trimmed at command return;
its old byte pointer/view is not reused. The native object pointer and its byte
buffer are distinct. No new safe trim/sharing API or thread certification exists.

## Direct scan migration

KeysCursor::scan takes &mut F. ScanKeyCursor accepts an owned FnMut with a valid
exclusive mutable origin, propagates the actual opening context to the borrowed
key adapter, and uses Option<&RedisString> for values (None for sets). Temporary
strings acquire/release independent references. Borrowed keys use ManuallyDrop
and remain Redis-owned; broader key/panic/reentry contracts are separate.

```rust
let mut count = 0;
while cursor.scan(ctx, &mut |_, _, _| count += 1) {}
// Key scans: |key, field, value: Option<&RedisString>| ...
```

The corrected isolated fixture exercises mutable state, retained/copied entries,
optional set values, actual-context hash getters inside scan callbacks, early
stop and completed-scan cleanup. Ordinary scan command replies are preserved.

## Validation and limits

- 28 compiler controls pass, including F07 lifetime controls, private owner
  construction/read/write, unsafe ingress/decoder and positive migrations.
  Uninvoked functions/metadata only; no native pointer initialization.
- 28 pure/harness tests and 14 focused docs pass; Python harness checks pass.
  The pure absent-owner mapping and layout checks construct no native owner.
- All 31 examples build debug/release at the selected 7.2 API floor; Clippy with
  warnings denied and formatting pass.
- 64 selected SDK executions pass on 7.2.8/8.10.2 debug/release: corrected native
  ownership, producers, scans, ordinary binary DUMP/RESTORE and outbound transfer;
  F07 views/configuration/static data and F05 parsing/allocation regressions.
- Four extra encoding executions pass: text-set listpack on both hosts and
  expiring hash listpackex on 8.10. Actual OBJECT ENCODING is asserted/retained.
  Common hash listpack/hashtable, zset listpack/skiplist, set intset/hashtable are
  checked AM off/on. Additional template-specific 8.10 TMPL_LP/TMPL_ARRAY encodings remain untested;
  module.c:12413-12480 supplies source evidence only for retention eligibility.
- Eight isolated reference library builds pass (four modules, two profiles),
  with F07 prerequisites and selected F09 raw-decoder/pointer/stream-producer
  migrations. SDK dependency identities and live source hashes are checked around
  metadata/builds. Original directories remain unchanged. Selected JMESPath and
  event-stream flows run separately; full module suites are not claimed.
- Native fixture hooks count real API calls, not native refcounts or heap usage.
  Its lifecycle returns [3 creations, 1 deep copy, 2 retains, 5 NULL-context frees,
  1 producer-context free] in both AM modes. Fresh adopted append succeeds;
  retained-alias append fails without changing bytes. These counts describe just
  that corrected local scope, not all Redis internals or a memory benchmark.

Failed attempts are retained: one manual-example mutable-callback compile
migration, fixture compile/registration repairs, compiler dependency search path,
legacy Redis-client nested scan decoding and a binary CallReply boundary. The
transfer fixture uses SET/STRLEN with client GET verification, keeping the existing
CallReply binary redesign outside scope. Fresh archive consumers exposed the
remaining manual fixture pointer read; its migration precedes a fresh package run.
No historical invalid native path, crash or leak sampler is executed.

## Attribution and process

Leaves upstream issue 358 open. INFO ownership findings are credited to
jonnyomerredis's PR 487 at c7ca18e4a6fce2f2f32e390f39abef4d67e1c9c6 and Omer
Shadmi (oshadmi)'s new PR 488 at 01ff52181d9d2efd136b1ef38143ad05223adca6.
The latter corrects both null/non-AM contexts; its additional NUL-name policy
is outside F09. PR 482's fallible scan redesign remains separate. Original BSD
notices are preserved; no native source is transplanted.

Dedicated baseline review/f09-baseline at dd6daec and empty kickoff 50fdf3f were
pushed before draft fork PR 3 existed and before implementation. The baseline
uploads F07's local merge and F08 specification history; fork master is not
pushed. GitHub merge/release/publication remain human-owned; drafts 1/2 and CI
modernization remain deferred. No MSRV/package/support commitment is selected.

This repair narrows ownership boundaries; it does not complete Context/GIL/raw
module/key/phase/panic/general alias/thread audits or certify all native producers,
allocators, platforms, persistence/replication or third-party applications.

## Newly identified follow-up

Independent review identified an existing string-configuration getter leak.
Native getModuleStringConfig copies val->ptr with sdsdup but never releases val
(7.2.8 module.c:12550; pinned 8.10.2 module.c:14116). The SDK's getter suppresses
Drop through take. F09 preserves that existing policy as specified, documents it
honestly, and proposes a narrow later scoping chunk. Configuration behavior tests
are not release-accounting certification. No sampler or old invalid path is run.
The overall string/native context, alias and phase limitations still apply.

## Final evidence provenance

The 64-case matrix and four encoding cases cover 68 unique host/profile cases.
After the source-review fixture corrections, all four ownership fixture tests
were rebuilt and re-executed in all four host/profile cells (16 rechecks), with
final module hashes in target/f09/runtime-final-fixture/report.json. Earlier
failed/partial/intermediate records remain distinct; they are not final passes.
The final compiler report covers the current take() documentation revision;
its fenced contracts are unchanged. Source-only review cannot certify these runs.
The scan fixture reports the last error in one synchronous native batch; the
optional first-error diagnostic refinement remains nonblocking and unimplemented.

Four fresh archive digests and every facade/member production Rust source file
are verified against the tested implementation. Six external consumers produce
12 debug/release builds on Rust 1.94.0 and 1.98.1; registration tests and selected
archive runtime flows pass. Task-local compiler/dependency caches were reused;
registry endpoints, archives, consumers and lock graphs were fresh. No global
Cargo configuration or publication is involved. The archives precede final
status prose and an example-only empty-unit stylistic cleanup; no claim is made
that those final documentation/example bytes are inside the consumed archives.
Core production bytes match. Fresh archives/consumer compilation are distinct
from native runtime evidence.

An archive attempt exhausted disk during the second compiler's release build.
Partial effects were inspected and retained, only generated SDK target/debug
cache was reclaimed (2.6 GiB), and a new complete archive run passes. No provider
retry or permission expansion occurred. The first review remained intact with
exit 0 and complete trace. Neither CI nor platform support is inferred from
these local checks.

Next proposed bounded chunk: F10 scoping for the string-configuration getter's
borrowed-return versus leaked-owner contract, using source/overlap inventory and
corrected-code criteria before any implementation. No F10 code is started here.
