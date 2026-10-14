# F08: RedisString ownership repair specification

This is a specification, not an implementation. F07 established the Rust borrow
relationship for a correctly constructed RedisString. The next repair should
establish how that owner is constructed, which native reference it releases,
and how authors cross the raw boundary. Keep this inside the string family and
its direct producers; a general Context, key, callback or FFI audit is separate.

## Recommendation

For F09, make RedisString an independent native owner with a private non-null
string pointer and no stored RedisModuleCtx. Distinguish adopting an owned
reference from acquiring a reference to a borrowed string. Free its owned
reference with a null context. Resolve AutoMemory registration while the
producer's context is still valid, rather than keeping that pointer for Drop.

Preserve command-handler argument/reply shapes and `Context::create_string`, deep-copy
`Clone`, retained-alias `safe_clone`, UTF-8/numeric/reply behavior and current
append status semantics. Explicit raw construction and `decode_args` become
unsafe; `.inner` becomes a documented `.as_ptr()` access. Low-level raw wrappers,
native access phases, public Context construction and GIL enforcement remain
separate obligations. This is an incremental facade repair, not a global
soundness claim.
The current-user getter must return `Option<RedisString>`: Redis 8.10.2 can
return no user string, which is not representable by a non-null owner.

## Evidence gathered in F08

The tracked-source inventory covers 209 Rust files: SDK 74, Carolina 23,
event-stream 37, JMESPath 35, stream-filter 40. It records revisions, dirty state,
every source hash and literal call sites. These are syntactic counts, not an AST
or an exhaustive resolution of aliases and generic `.clone()` calls. Line
comments are excluded; `.inner` results are manually reviewed candidates.

| Calls / candidates | SDK | Carolina | Event-stream | JMESPath | Stream-filter |
|---|---:|---:|---:|---:|---:|
| `RedisString::new` | 6 | 0 | 0 | 0 | 0 |
| `from_redis_module_string` | 9 | 0 | 0 | 0 | 2 |
| `create` | 12 | 0 | 7 | 3 | 0 |
| `create_from_slice` | 1 | 0 | 0 | 0 | 1 |
| `safe_clone` | 1 | 0 | 0 | 0 | 0 |
| `decode_args` | 4* | 2 | 0 | 0 | 1 |
| `.inner` candidates in files mentioning RedisString | 43 | 2 | 1 | 0 | 4 |

*The SDK decoder count includes its definition and three macro/generated-call
sites. It is not four invocations. Documentation and qualified/aliased names can
require manual reconciliation; the inventory is an impact map, not proof of no
other downstream users. All four original repositories remain read-only.
One known uncounted qualified-name site is `Self::from_redis_module_string`
inside `Clone` (src/redismodule.rs:449); it belongs in the adoption migration.

Native evidence is source inspection of retained Redis 7.2.8 and exact Redis
8.10.2 commit `498ecd0d6d007db11ddb3aea9428552598a78622`. The latter files were
obtained through the official GitHub API and verified against their Git blob
hashes. No native function or historical invalid path was executed in F08.
The examined ownership/append operations agree in both sources; this does not
establish all native callback/iteration guarantees across versions.

## Ownership ledger, before changing a call

`RetainString(ctx, p)` is not always an increment: if an AutoMemory entry can be
removed from that context, it detaches that entry instead. A nonnull context
alone does not mean AutoMemory is enabled or that the pointer has an entry.
`FreeString(ctx, p)` releases the reference and removes an AutoMemory entry when
applicable. Null-context creation produces manually managed ownership.

| Origin / SDK site | Native responsibility | Required owner conversion |
|---|---|---|
| `CreateString`, `CreateStringFromString`; creation / deep `Clone` | Fresh owned result; optional AM registration if created with a context | Prefer creation with null context, then adopt once without another retain. Preserve existing text/binary conversion policy in this chunk. |
| Command/configuration callback input; `decode_args`, `string_configuration_set` | Borrowed input | Acquire an independent null-context reference while native retention is legal; do not consume the callback's reference. |
| `ServerInfoGetField`; `ServerInfo::field` | Fresh owned result, optional AM entry | Request the field with null context where permitted, then adopt once. Do not use the borrowed-input retain recipe. |
| `ListPop`; head/tail helpers | Owned return, possibly AM registered through the key's context | Transfer exactly one ownership responsibility and detach the producer's AM entry when applicable. |
| `HashGet`; direct and batched hash helpers | Owned result, possibly AM registered through the key | Same owned-result conversion; keep null results as None. |
| `StreamIteratorNextField`; StreamIterator | Fresh field/value owners registered through key context when AM is active | Detach/adopt each owned result using the actual producer context; a null release context does not by itself remove that registration. |
| `GetCurrentUserName`; Context getter | Owned result with optional AM entry, or NULL on 8.10.2 when no client/user/name exists | Return Option; adopt/detach only Some using the real producer context. NULL never constructs an owner. Only call the native API in contexts legal for that host; 7.2.8 lacks this absence guard. |
| `LoadString`; RDB wrapper | Owned result with explicit release responsibility | Adopt without another retain; preserve IO error handling. |
| Scan callbacks; KeysCursor / ScanKeyCursor | Borrowed strings; native caller releases after callback | Acquire/release independent references during the callback, or use a separately reviewed borrowed adapter. Never adopt these as transferred results. |
| Configuration getters/errors and StrCallArgs `.take()` | Outbound ownership transfer | Suppress exactly one Rust Drop; document which native recipient releases the reference. Keep its current ownership contract. |
| `DefragContext::defrag_redis_string` | Consumes an owner; native function may return a replacement pointer | Private consuming pointer-replacement helper preserves the same ownership responsibility; no public field write. |

The default implementation should use the existing RetainString/FreeString
capabilities rather than require a new server API. `HoldString` has a distinct
static-string/copy policy; do not replace retention indiscriminately or infer a
minimum supported Redis version from its presence in the header. Non-retainable
LOADED-event keys still require copying, as F07 already documents.

For a transferred owned result that may have an AM entry, one proposed
normalization is acquiring a null-context reference and releasing the original
with its valid producer context immediately. The net ownership is one manually
managed reference and the producer entry is removed. For a genuinely null-context
owned result, directly adopt it. Verify these paths against both AM states;
do not infer the ledger from pointer non-nullness or assume every physical object
starts with numeric refcount 1 (shared/native encodings can differ).

For the legacy `new(ctx, p)` shim, retaining with the provided valid context can
keep its existing detach-or-increment behavior, while the returned owner stores
no context and releases with null. Its unsafe documentation must distinguish
this legacy acquisition policy from the explicit borrowed-reference recipe.
Migrate SDK sites through the ledger; do not hide a fresh-owned result behind
this ambiguous shim.

## Proposed API and migration

The following shapes are proposed; they are not compiled or exported yet:

```rust
// Private NonNull<raw::RedisModuleString>; no stored context.
pub fn as_ptr(&self) -> *mut raw::RedisModuleString;
pub fn from_bytes(bytes: &[u8]) -> Self;
pub unsafe fn from_owned_ptr(
    producer_ctx: Option<NonNull<raw::RedisModuleCtx>>,
    owned: NonNull<raw::RedisModuleString>,
) -> Self;
pub unsafe fn retain_from_ptr(borrowed: NonNull<raw::RedisModuleString>) -> Self;
```

Raw ownership preconditions include initialized APIs, a valid native object,
the correct owned/borrowed provenance, legal native access, retention eligibility
and a valid producer context when AM detachment is needed. NonNull expresses
only null exclusion. The pointer getter performs no native operation and does
not transfer ownership, extend lifetime or authorize mutation through an alias.
The producer context must be the native operation's actual AM context, not a
null or guessed context saved in an SDK wrapper.

Retained command argv can be automatically trimmed at command return. Keep the
owner across that boundary, then derive a fresh byte/text view in the later
legal native phase. Do not keep an in-command byte pointer/view across trimming.
`as_ptr()` points to the native string object; trimming its SDS buffer does not
itself move that object. Its use still requires a live owner and legal native
access. Native threading guidance requires explicit trimming before exposing
retained argv to other threads; F09 adds no safe trim/sharing API or thread-safety
certification. See the retained native command-return/trim implementation.
The argv trim loop is at module.c:932–937 in 7.2.8 and module.c:1000–1003
in the pinned 8.10.2 source; the corresponding object.c function can resize SDS.

`Context::new` and `Context::dummy` are public safe const constructors
(src/context/mod.rs:339,344). Thus safe_clone's context argument is advisory,
not an unforgeable GIL or legal-phase proof. F09 documentation must say so.

Keep the current context-accepting `create` / `create_from_slice` as documented
compatibility entry points that create independent null-context owners; the
context parameter no longer controls their lifetime. Add `from_bytes` as the
context-free binary recipe; `create_from_slice` forwards to it. Keep `create`'s
existing CString/NUL behavior in F09; changing that or serde byte semantics is a
separate compatibility choice, not necessary for this ownership repair.

Mark `new` and `from_redis_module_string` unsafe compatibility shims for their
respective acquisition/adoption contracts. Extend `from_raw_parts` safety docs
to cover initialized APIs/native access; it already requires unsafe for source
bytes. The public decoder accepts raw argv/context data, so it must become unsafe
too, with explicit callback validity/count/input obligations. Add narrow unsafe
blocks to both declarative and procedural generated adapters. No command parser,
panic boundary or macro registration behavior is redesigned.
The current const adoption shim cannot remain const if it performs native AM
detachment; document that source break for downstream static initializers.

Internal ownership transfer should use a consuming non-null implementation,
such as ManuallyDrop, instead of leaving a null pointer in a live owner. Preserve
the existing crate-private transfer interface where callers need it. Defrag gets
a crate-private consuming replacement operation, not writable public storage.

| Caller | Migration |
|---|---|
| Reads `s.inner` | `s.as_ptr()`; keep the owner alive and follow the called native API's obligations. |
| Fabricates/writes inner storage | No safe field mutation; use the appropriate explicit unsafe raw constructor, with verified provenance. |
| Wraps a native owned result | `from_owned_ptr` with its true producer context; no extra borrowed retain. |
| Wraps callback argv | Unsafe decoder/acquisition within the native callback, preserving the caller's ownership. |
| Calls `create(None, text)` | Continues compiling; use `from_bytes` for an existing byte slice. |
| Stores a result across commands | Retain an independent owner, then access/drop it only in a legal native phase. Context-free ownership does not prove GIL or thread safety. |
| Uses `Context::get_current_user` | Handle Option; Some yields the independent owner, None reflects a permitted native absence. Adapt examples/acl.rs while preserving ordinary command-user replies; do not manufacture a null string. |

### Necessary direct-caller consequences

KeysCursor currently retains a scan key and then forgets its wrapper. Reconcile
the acquired reference instead of retaining-and-forgetting it. ScanKeyCursor
currently fabricates owning wrappers for borrowed field/value pointers and
forgets them. Its value can be null for a set; a non-null owner cannot represent
that absence. Recommend `Option<&RedisString>` for the value argument of both
`scan` and `for_each`, with balanced temporary retained owners for present
strings. Update SDK example/docs and downstream closure signatures where used.
Existing hash field/value command replies must remain unchanged. Leave the
borrowed RedisKey adapter itself for a separate key-ownership audit.
One context propagation change is necessary here: pass the cursor's valid
opening context in its synchronous callback data alongside F, so the temporary
RedisKey wrapper carries the true native key context rather than null. Otherwise
owned string getters called through that wrapper cannot detach the key's AM
entries correctly. This is metadata propagation for string producers; keep the
key's borrowed/no-Drop behavior and broader key lifecycle separate. Unsafe
external wrappers with an unknown/mismatched producer context are outside the
string-returning methods' contract; document the actual-context obligation.

The two direct scan adapters also need a valid mutable origin for FnMut state
before they can serve corrected-code fixtures: KeysCursor::scan should borrow
`&mut F`, and ScanKeyCursor::scan should bind its owned F mutably and pass an
exclusive mutable pointer. Both currently derive the callback pointer from a
shared reference. Change only these adjacent origins and their direct callers;
do not expand this into a general callback/panic/reentry audit. Verify mutable
state accumulation in normal synchronous callbacks on the corrected adapters.

Native append rejects shared strings before changing their encoding/buffer.
Correcting an accidental extra retain on a fresh result can make it unshared,
and therefore change its observed append status from error to success. Preserve
the native policy (no forced copying/success for shared aliases), not that bug.
Record this observable correction; preserve the selected command protocols and
do not claim universal downstream behavioral compatibility.
Deep `Clone` and retained `safe_clone` are different: the latter creates a native
alias, not a byte copy or a Rust uniqueness guarantee. Defrag's string operation
requires a single reference, while trimming has a different contract and may
reallocate a raw buffer. Therefore retain/refcount checks alone do not certify
all buffer stability, native aliases, reentry or threading. Preserve those
limitations and do not add a public safe trim method as part of F09.

Reference migration impact:

- Carolina: two raw decoder callbacks and two native key-pointer reads.
- Event-stream: one example pointer read; seven context-free creation calls stay
  source-compatible. Its F07 callback migration is a prerequisite in snapshots.
- JMESPath: three creation calls stay source-compatible; retain F07's six owner
  bindings in snapshots rather than editing its original directory.
- Stream-filter: two owned stream-field conversions, one byte creation, one
  raw decoder adapter and four pointer reads. Its native stream producer context
  must be recorded rather than guessed from the old null-context wrapper.

These are selected tracked uses, not a migration of the original repositories
or a claim about every third-party consumer. No scan callback use was found in
those reference files; other downstream authors may need the Option migration.

## Alternatives considered

| Choice | Result / tradeoff |
|---|---|
| Hide inner and mark raw constructors unsafe, retain the stored context | Useful first boundary, but safe high-level construction can still yield an owner whose Drop keeps a context pointer beyond its lifetime. Insufficient as the selected ownership repair. |
| Add a context lifetime parameter to RedisString | Makes that relationship explicit but spreads lifetime parameters through args, replies, stored datatypes and persistence. Independent retained storage still needs another owner type; much larger migration. |
| Independent native owner with explicit adoption/acquisition | Recommended: no retained context pointer, distinct reference ledger, modest direct-caller changes. Native phase/alias guarantees remain conditional. |
| Copy every raw/native string on ingress | Simpler independence but adds unnecessary native copies and changes retained-alias/append behavior. Copy only where required by provenance; measure any necessary cost. |

## F09 corrected-code acceptance criteria

Do the following only after the scoped draft prerequisite, on the corrected
implementation. F08 does not execute or claim these tests.

1. Compiler-only, uninvoked function controls: inaccessible inner construction
   and writes, raw constructors/decoder require unsafe, safe getter/byte recipes
   compile, F07 owner lifetime/drop/mutation controls remain meaningful. Check
   exact diagnostics with positive controls; never initialize native API pointers
   in these compiler checks. Size/layout checks instantiate no native owner.
2. Isolated ownership fixture on owned servers, debug/release, selected Redis
   7.2.8/8.10.2: create/retain/copy/adopt/release and outbound transfers balance
   one ownership responsibility. Exercise AutoMemory off and on with real valid
   contexts; retain storage across one command boundary, drop it in a later
   valid command, and verify bytes, normal errors and recovery. No stale-context,
   invalid pointer or freed-view path is run, including against the old SDK.
   The stored-argv flow reads through a fresh later-command view after possible
   auto-trimming; no pre-boundary byte pointer is used afterward.
3. Native-owned producers: present/absent INFO field, list pop, hash direct/batch,
   current user and stream iteration. Verify detachment and ordinary cleanup
   without changing replies or stored formats. RDB regression uses corrected
   ordinary save/load only, without changing persistence layout or claiming a
   general persistence/replication suite. Record server/PID/library identity.
   For current user, check Some in normal command contexts on both hosts and
   pure NULL-to-None mapping without initializing native API pointers. A native
   no-user cell is 8.10.2-only and requires an actually legal context supplied
   by the host; a timer/detached context must not be assumed to return NULL.
   Never fabricate a context or run a missing-client query on 7.2.8. If no
   legal no-user fixture is available, retain the source/pure evidence and mark
   that native cell untested, rather than inventing an invalid reproduction.
4. Corrected scan fixtures: ordinary DB keys and hash/set/sorted-set fields;
   set value is None; present callback strings can be retained/copied legally;
   release balanced temporary references. Avoid loading old handlers into new
   binary/absent-value flows. Include a stopped/finished scan cleanup check;
   panic/unwind and key-lifetime audits stay separate.
   Cover compact and dictionary/skiplist hash/sorted-set modes, plus intset and
   hashtable sets on both selected hosts. Record actual OBJECT ENCODING results;
   include other reachable host encodings where supported, and explicitly
   identify any additional 8.10 template/expanded-listpack mode not covered.
5. Alias behavior: deep-copy mutations preserve the original; retained-alias
   append returns the native error and preserves bytes. Tests use valid current
   objects and legal access, never a historical invalid alias/trim sequence.
   Make no general claim that arbitrary aliases cannot invalidate a view.
6. Fresh archive consumers and isolated snapshots of all four reference modules
   validate the selected migration. Apply F07 prerequisites in snapshots; hash
   current SDK inputs around metadata/builds and validate actual dependency
   provenance. Original directories remain unchanged. Preserve input/command
   failure artifacts and separate compile evidence from runtime coverage.
7. Keep strict text/numeric/command contracts: F07 fixtures, configuration,
   INFO/static data, parser allocation regression and relevant direct producer
   tests. Run pure tests, all examples, Clippy/fmt and package checks appropriate
   to the changed interfaces. Native calls/reference or byte-copy counts and
   type size are evidence to report, not an unmeasured throughput claim.

A production design that cannot establish the ledger should stop at a reviewed
specification/blocker rather than merge an ambiguous partial adoption. Keep F09
one bounded ownership chunk; carry unfinished work to the next interval.

## Overlap, attribution and workflow

Refreshed 55 upstream issues, 45 upstream PRs and two fork drafts. Upstream
[issue 358](https://github.com/RedisLabsModules/redismodule-rs/issues/358) covers
the larger RedisString/context problem; leaves upstream issue 358 open.
[PR 487](https://github.com/RedisLabsModules/redismodule-rs/pull/487), by
`jonnyomerredis`, head `c7ca18e4a6fce2f2f32e390f39abef4d67e1c9c6`, identifies the
fresh-owned INFO result versus an unnecessary retain. Preserve that attribution
and exact revision if adapting the finding/code. Its nonnull-context branch
still depends on AM being enabled/registered; do not treat nonnull as proof.
Its patch is inspected only; no historical leak sampler is run here.

Issues [134](https://github.com/RedisLabsModules/redismodule-rs/issues/134) and
[221](https://github.com/RedisLabsModules/redismodule-rs/issues/221) are related
binary/API-initialization context, not additional F09 fixes; leaves both open.
F07 [fork draft PR 2](https://github.com/joshrotenberg/redismodule-rs/pull/2) is
locally complete. Shared files are a dependency, not authorization to broaden
that PR. The two fork drafts have no competing construction/ownership repair.
Recheck overlap immediately before F09 implementation.

F08 uses local branch `docs/f08-string-ownership-scope` from clean F07 merge
`e53e632b0c8e87fb4b6887323329f12ee29c0c3f`. Review this specification, record
verified evidence and merge locally with no-ff. No push or issue implementation
belongs to F08. For F09, reconcile `review/f09-baseline` and
`fix/f09-string-ownership`, create/push the dedicated baseline and zero-content
kickoff, and open a scoped draft listing the partial issue-358 relationship and
PR-487 dependency/credit before editing implementation. The baseline upload
includes the F07 local merge and F08 specification history; record that fact and
stay within existing issue-visibility authority. Never push fork master, merge
on GitHub or change publication settings as part of this work.

CI remains deferred: F07 implementation-head CI completed with five successes
and three failures; local tests do not certify those runners. No MSRV, package,
license change, server/platform support commitment or unrelated migration is
selected. The existing BSD notices remain; native source is inspected with its
original notices and is not transplanted into the SDK.

## Primary sources and retained evidence

SDK: src/redismodule.rs, configuration.rs, raw.rs, stream.rs, key.rs, direct
context producers, defrag and command adapters, pinned to the F07 checkpoint.
The [Redis 7.2.8 module source](https://github.com/redis/redis/blob/7.2.8/src/module.c)
defines creation, retain/free, INFO and callback ownership; its object.c and
defrag.c distinguish trimming and movement. Exact
[Redis 8.10.2 module source](https://github.com/redis/redis/blob/498ecd0d6d007db11ddb3aea9428552598a78622/src/module.c),
[object source](https://github.com/redis/redis/blob/498ecd0d6d007db11ddb3aea9428552598a78622/src/object.c)
and [defrag source](https://github.com/redis/redis/blob/498ecd0d6d007db11ddb3aea9428552598a78622/src/defrag.c)
are independently fetched and fingerprinted.

Full inventory, API/patch snapshots, native hashes and subsequent read-only
review evidence remain under `target/f08/`. Compact results are in
[the F08 evidence record](evidence/f08-string-ownership.json). No proposed API
has been implemented, compiled or native-tested in this scoping chunk.
