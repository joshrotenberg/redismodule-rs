# F06: scope the legacy string-view repair

This chunk specifies a narrow follow-up to F05. It changes documentation only;
no SDK source, API, reference-module code or runtime behavior is changed or
tested anew. Starting SDK checkpoint: `f066bb1f9a4320f75a3353a0f66901f7732bdab2`.
The proposed implementation chunk is F07. Justified Rust API breaks are allowed
under the hybrid-fork direction; server behavior remains a separate contract.

## The contract to repair

`RedisString::try_as_str` currently exposes a caller-selected output lifetime
independent of `&self`. `NextArg::next_str` consumes a `RedisString` and produces
such a reference while the consumed owner is dropped. The raw `from_ptr` and
`string_as_slice` helpers also expose caller-selected lifetimes without requiring
an unsafe call. F05 avoids the text helpers and borrows through `as_slice(&self)`.

The repair should make owner-bound views the normal safe interface and make
the caller's obligations explicit for raw views. That establishes a Rust borrow
relationship for correctly owned native strings; it does not certify all
existing constructors, context lifetimes, native aliases or threading paths.
Internal safety justifications depend on `inner` identifying a valid, retained
native string with a stable buffer during each view. Public `inner` and safe
raw-pointer constructors can violate that assumed invariant today; F07 must state
this remaining hole instead of claiming that its comments certify the type.

## Recommended F07 boundary

| Surface | Recommended change | Migration and behavior |
|---|---|---|
| `RedisString::try_as_str` | Tie returned text to `&self`, using the owner-bound `as_slice` and strict UTF-8 conversion. | Existing owners that remain alive need no conceptual rewrite. Explicit caller-selected lifetime uses must change. Preserve the current UTF-8 error text. |
| `NextArg::next_str` | Remove this method from the consuming trait and blanket implementation. Keep all other `NextArg` methods. | For immediate borrowed parsing, keep `argv` alive and use `ArgCursor`. For one consumed argument, bind `next_arg()` to a local owner before calling its text view. Copy explicitly when storing text. |
| `RedisString::from_ptr` | Keep name, return type and zero-copy behavior, require an unsafe call with documented preconditions. | Raw integration callers must establish native buffer validity and stability for the entire returned lifetime. This is a source-level break. |
| `RedisString::string_as_slice` | Same explicit unsafe boundary for raw bytes. | Update the SDK's internal calls and notification macro with narrow safety justifications. Do not manufacture an owning RedisString around a borrowed callback key. |
| SDK examples/callers | Migrate only affected SDK call sites and compile-check remaining text callers. | Preserve command names, accepted argument counts, strict/lossy choices, return values and persistence/replication behavior. |

**Proposed signature and implementation**, not applied in F06:

```rust
pub fn try_as_str(&self) -> Result<&str, RedisError> {
    std::str::from_utf8(self.as_slice())
        .map_err(|_| RedisError::Str("Couldn't parse as UTF-8 string"))
}
```

Raw helper documentation must require an initialized Redis API, a valid native
string/buffer for the chosen lifetime, legal server/thread access, and no freeing,
trimming, mutation or other buffer invalidation during the view. References are
read-only. Passing ownership-free raw pointers still gives the SDK no owner from
which it can infer a lifetime. Keep unsafe calls confined to the existing raw
conversion layer and callback adapter; do not add an ownerless safe shim.

The retained Redis 7.2.8 source (`src/module.c`) documents `RM_StringPtrLen` as a
read-only pointer/length view; `RM_StringAppendBuffer` requires a singly referenced
user-created string, and keyspace callbacks execute synchronously. These facts
inform the boundary. They are not a proof that every native alias is immutable.
The Redis 7.2.8 notification note at `src/module.c:8610` says LOADED-event keys
cannot be retained; the adapter must borrow within the callback rather than
creating an owning wrapper. `safe_clone` can also produce a separate Rust owner
of the same native string. Rust's same-owner borrow check does not exclude that
alias; append behavior relies on Redis's unshared-string check. No native alias
soundness claim follows from E0502.
The raw helpers' safety contract should conservatively require a valid non-null
string; the native diagnostic behavior for null pointers is not a safe-view
guarantee. F07 should document this source-level contract restriction explicitly.

## Alternatives and why the recommendation breaks Rust code

| Option | Assessment |
|---|---|
| Deprecate the existing consuming borrowed method but keep its implementation | Warning-only migration does not correct the lifetime contract. Not the recommended repair. |
| Return owned String from the existing `next_str` name | Removes the borrowed result but silently adds copying and duplicates the `next_string` concept. It also requires reference adjustments at callers. |
| Make the consuming method unsafe | Pushes a difficult ownership obligation onto authors even though `ArgCursor` provides an owner-bound path. Do not use as the normal migration. |
| Remove it; tighten owner-bound text and explicit raw views | Small coherent break with clear alternatives. Recommended. |

Do not silently replace strict text parsing with `next_string()`: that method is
lossy. An author needing owned strict text can keep a consumed local RedisString,
validate its text and then call `to_owned()`. F07 need not introduce another helper
unless a real caller makes it necessary. Custom downstream NextArg implementations
for local non-Iterator types are possible despite the blanket Iterator impl;
removing a trait method also requires these implementations to remove their old
method definition. The scope of this source break is not limited to call sites.

## Affected SDK and reference uses

The inventory searches tracked Rust source across whitespace for method calls.
It is textual evidence, not a full name-resolution or downstream ecosystem
survey. Definitions/docs are distinguished from runtime call sites; borrowed
ArgCursor calls are distinguished manually from consuming NextArg calls.

| Tree | Findings | F07 handling |
|---|---|---|
| SDK, 73 tracked Rust files | Three consuming NextArg text calls: two in `examples/info.rs`, one in `examples/threads.rs`. Two example calls already use ArgCursor. Twelve `try_as_str` calls, including the blanket implementation to remove. One raw `from_ptr` call and three raw byte-view calls, including internal wrappers and the keyspace macro. | Migrate INFO to a borrowed cursor and static-data setter to a local owner/strict owned text. Preserve INFO's existing allowance for extra args and setter semantics. Tighten wrappers and macro raw calls. |
| JMESPath, 35 files | Six consuming text calls in five handlers (`query`, `eval`, `exists`, `type`, `len`); four owner-backed text calls in `timeseries.rs`. | Document migration; validate only in an isolated snapshot later. Its formatting helper currently consumes `Iterator<Item=RedisString>`: a cursor migration also needs a borrowed option parser or an explicitly copied option tail. |
| Event stream, 37 files | Eight owner-backed text calls (one mode command, seven config setters) and one raw byte view in the synchronous keyspace callback. | Owner lifetime tightening appears source-compatible for inspected calls; the raw view needs an unsafe block plus the callback lifetime justification in a later isolated snapshot. |
| Carolina, 23 files | No calls to these selected legacy views. Its owned `next_string` calls are separate. | No targeted migration inferred; later consumer builds remain the check. |
| Stream filter, 40 files | No calls to these selected legacy views. Existing parsing uses borrowed bytes. | No targeted migration inferred; retained blocking requests still need ownership. |

For JMESPath, the smallest source migration can bind consumed arguments to local
owners and preserve the existing formatting iterator; that avoids an unrelated
grammar rewrite. A borrowed-cursor variant is a useful second example, but an
option-tail copy must be counted and disclosed. `split_at_format` already returns
references with the single input-slice lifetime; the inspected direct text owners
remain alive. These predictions have not been tested against the proposed APIs.

Other SDK callers also keep owners alive: configuration defaults already tie
both argv and fallback to one lifetime; configuration setters and RedisValue
conversion copy before their owner drops; `is_enterprise_internal` matches
`&info_res`, not a moved reply. Do not invent migration edits to these callers
unless a corrected build identifies a real lifetime mismatch. The migrated
static-data setter must reject invalid UTF-8 before assigning any static data;
relative lock ordering is not a new contract in this slice.

Original module directories remain untouched. Exact matching file hashes, Git
heads and dirty flags are retained in `target/f06/inventory.json` and summarized
in [F06 evidence](evidence/f06-string-lifetimes.json).

## Issue visibility and overlapping work

The refreshed GitHub inventory has 55 open upstream issues, 45 open upstream
PRs, zero fork issues and one fork draft PR (F02 logging). Title/body searching
and selected relevant diffs found no competing implementation of this narrow
view contract. This is not a review of every upstream patch.

[Upstream issue 358](https://github.com/RedisLabsModules/redismodule-rs/issues/358)
asks for a broader RedisString/context-ownership refactor. F07 should list this
partial relationship and **leaves upstream issue 358 open**: context storage,
allocation provenance, adoption/retention and the entire RedisString design are
outside the selected repair.

Selected open PRs were inspected at exact heads:

- [PR 375](https://github.com/RedisLabsModules/redismodule-rs/pull/375),
  `68a0705b7f3d95a893eda19390e4dff51d3a4263`: shared files, but selected hunks
  change imports/status/native-call plumbing rather than these lifetime contracts.
- [PR 487](https://github.com/RedisLabsModules/redismodule-rs/pull/487),
  `c7ca18e4a6fce2f2f32e390f39abef4d67e1c9c6`: ServerInfo null-context ownership
  leak; separate from borrowed view lifetime repair. Do not absorb it into F07.
- [PR 427](https://github.com/RedisLabsModules/redismodule-rs/pull/427),
  `40c57a632c8d8cd4f63c5c95334742af0b7e0cf9`: allocator macro comments;
  different hunk from the keyspace byte-view adapter.

F06 is documentation-only scoping and stays local. **Before F07 implementation**,
refresh overlap and source state, create a dedicated branch from a clean checkpoint
with a zero-content kickoff, push it and open a scoped draft PR listing upstream
issue 358 and the planned partial repair. Use fork branch `review/f07-baseline`
at the clean post-F06 merge checkpoint as the draft base, and a dedicated
`fix/f07-string-view-lifetimes` kickoff branch for the change; reconcile any existing
branches/PRs with those names first. Push the required baseline and kickoff under
the issue-work authorization, keeping fork master and release settings unchanged.
This keeps the F07 diff bounded despite locally ahead master. Publishing that baseline/kickoff also
uploads its required ancestor commits, including previously local chunks; record
that consequence explicitly before the authorized issue-work push. It is not a
package release or permission to merge the PR. If the required base/history upload
is outside the current authorization or the prerequisite fails, stop implementation
and retain the blocker. Keep the PR draft through validation; GitHub merges,
releases and publication remain human-owned. Remote CI modernization is deferred.

## Corrected-code acceptance criteria for F07

1. Compile the corrected public signatures and all affected SDK examples on the
   pinned Rust 1.94 toolchain with `--locked`, the existing mutually exclusive
   feature choices, and workspace/all-target Clippy with warnings denied.
2. Add owner-bound `try_as_str` compile-pass cases as uninvoked function bodies
   or `no_run` documentation examples, so they never call Redis global API pointers
   in pure tests. Any executed view test belongs in the live-server harness.
   Add compiler-checked rejection
   of escaping its owner, dropping the owner early, and mutable append while a
   text view through that same Rust owner is used. This is not an aliasing check.
   Verify expected diagnostics; do not compile or execute
   historical dangling-reference paths against the old implementation.
3. Compile-test that raw helpers require an unsafe call (expected E0133), and that
   the removed consuming trait method is unavailable (E0599), with ArgCursor
   as its positive control. Check a migrated custom NextArg implementation too;
   its stale extra method should fail with E0407 against the corrected trait.
   Give each negative case a positive corrected control. Preserve F05 cursor lifetime tests when relevant.
4. Test corrected owner-backed views for empty/Unicode/NUL text, strict UTF-8
   rejection and exact binary byte preservation on owned loopback Redis 7.2.8
   and 8.10.2, debug/release. Use only valid live strings/native pointers; no freed,
   forged, aliased-mutation, callback-after-return or crash fixture.
5. Cover affected INFO behavior, strict owned static-data storage, ordinary
   configuration set/get and a new binary-safe notification-key fixture. The
   existing event handler formats keys with `from_utf8(...).unwrap()`, so do not
   send a binary key through it; use a corrected dedicated handler that copies
   bytes only when retaining them beyond its synchronous callback. Put this fixture
   in a new example module with isolated registrations; do not let the existing
   events module's other handlers receive the binary key. Preserve
   command arity/error/reply semantics and demonstrate later valid commands.
6. Validate a fresh archived downstream consumer using the owner-bound public
   API and macro callback adapter without path/patch shortcuts. Retain compiler,
   archive/lock/source and observed host identity rather than relying on a build
   alone for runtime claims.
7. Use isolated reference snapshots only when the selected migration evidence
   needs them. Compile the smallest JMESPath local-owner migration and event-stream
   raw callback adjustment; label build-only results. Selected ordinary flows can
   follow with pinned host/artifact evidence. Do not migrate original directories
   or impose SDK adoption on their maintainers.
8. Independently review the changed contract through Due, verify actual effects
   and source provenance, and record breaks, tests, limits and unresolved native
   ownership questions. No broad safety or performance certification follows.

## Current checkpoint

F06's implementation-file baseline and matching reference files are hashed.
The caller checked the source inventory, configuration/reply ownership and
selected PR hunks directly. Independent Due/Claude specification review approves
this boundary. Actual exit 0; request/result/answer/all 167 trace records/summary
independently validate, with no failed, dropped, gapped or unmatched activities.
Reviewed input hashes stayed unchanged. The reviewer did not compile proposed
APIs, verify hashes, open original module code or audit selected PR diffs; the
caller checked source/provenance and selected patches independently.

The caller applied all eight nonblocking documentation tightenings: isolated
binary fixture registration, compile-only controls, conditional native invariants,
LOADED-key source anchor, alias limits, diagnostic expectations, a concrete review
base, and no assignment before UTF-8 validation. The review also overstated that
the blanket trait impl excludes custom downstream implementations. One Rust 1.94
`--emit=metadata` consumer check against the unchanged F05 SDK artifact confirms
a local non-Iterator type can implement NextArg (exit 0); its bodies return ordinary
errors and were never run. The migration plan now covers those implementations.
No proposed API, Redis runtime, package consumer build or historical invalid native
path was tested; F05 results remain historical evidence. This source-only
specification refinement needed no second model review. Result/trace/contracts
and dispositions are retained in `target/f06/review/`; the pure consumer evidence
is `target/f06/custom-nextarg/report.json`.

F07 should use the recommended narrow break unless later human direction changes
it. Native context/alias ownership, constructor safety, Borrow<str> semantics,
global threading, full reference suites, CI, MSRV, package identities and support
commitments remain separate. The unsafe raw boundary needs clear documentation
and corrected tests, not a new callback/ownership subsystem.
