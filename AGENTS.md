# Working instructions

## Direction

Improve this fork of `RedisLabsModules/redismodule-rs` for external module
authors, with a path for Search and RedisJSON. Preserve useful macros, parsers,
reply conversions and direct key operations. Justified Rust API breaks are
allowed; command behavior, replication, stored formats and supported hosts are
separate contracts. The from-scratch SDK remains paused.

## Current local process

The user selected local work with good branching before pushing. Issue-independent
baseline work stays local: do not push, open PRs, change GitHub settings or publish
for that work. The automatic publisher is disabled remotely.

The user's newer session-provided issue-visibility instructions apply before
implementation of tracked issues: check open PRs for overlapping scope, create
a dedicated branch with an empty kickoff commit, push it and open a draft PR
listing every scoped issue and the planned work. Do not implement until the draft
exists; if GitHub access is blocked, stop and report it. Keep it draft until the
intended implementation and validation finish. This issue-specific workflow
takes precedence over the local-only default. Publication remains unauthorized.

CI modernization is a separate deferred workstream. Existing workflow edits are
local and provisional; local command results do not validate GitHub runner jobs.
Wait for the user to resume that work or request remote verification.

1. Read `STATUS.md` and the relevant plan before starting.
2. Start each bounded chunk from clean local `master` on a descriptive branch.
3. Implement and validate the changed contract; preserve unrelated user work.
4. Record actual results, limitations and decisions in the same branch.
5. For local-only chunks, commit and merge with `git merge --no-ff` once ready, delete the merged branch,
   and leave local `master` clean. Do not merge incomplete work to finish a report.
6. Report **Done / Next / Blockers / Decisions** after every chunk.

Before future remote collaboration, reconcile overlapping work and map local
changes to reviewable issues/PRs. Preserve upstream credit and exact contribution
revisions. Say “leaves #123 open” when an issue remains open, and distinguish
upstream references from fork references.

## Validation

- Keep `Cargo.lock` tracked. Use the pinned compiler and `--locked` for local/CI
  checks; separately validate fresh downstream resolution when dependencies change.
- See [development instructions](docs/DEVELOPMENT.md) for feature selection and
  pure checks. Do not use `--all-features`: bindgen linkage choices are exclusive.
- Test behavior appropriate to the change. Pure tests and build checks do not
  establish Redis runtime, persistence, replication or complete FFI soundness.
- Test corrected contracts; historical reports/source analysis can define them
  without new execution of invalid paths. Keep pure tests independent of Redis
  global API pointers and its allocator.
- A lock does not prove Rust reference uniqueness; a caught panic does not undo
  partial mutations or replies. Track ownership, aliasing, callback phases,
  reentry, threading, allocation provenance and failure cleanup explicitly.
- Leave the original checkout and reference module working directories unchanged;
  use isolated snapshots for comparisons and migrations.

Keep inherited BSD notices and source provenance. Package names, release versions,
MSRV and server support must be chosen from evidence, not inferred from a toolchain
pin or compatibility-feature name.

## Due delegation trial

Josh's October 12 direction enables the Due caller-policy trial in this project.
Use the installed `due:delegate` plugin and local CLI for delegated model work;
do not substitute native agents, other delegation tools or direct provider runs.
The owning caller may work directly. Read installed help, the plugin skill and
Due's run/contracts/observe guides. Follow Due's
[caller policy](https://github.com/joshrotenberg/due/blob/main/docs/caller-policy.md)
and [reporting guide](https://github.com/joshrotenberg/due/blob/main/docs/dogfooding.md).

Carry exact scope, permissions, instructions, deliverable and stopping condition
through stdin. Use JSON results and fresh traces; disable recursive/native agent
delegation where supported. Preserve native permissions and inspect partial
effects before follow-up. Independently validate answers and actual changes/checks.
Use the other provider for implementation review when available; unavailable
review stays pending, without silent fallback or permission bypass.

Josh's October 13 direction routes both positive and negative Due feedback through
the existing `due-expert` chat (thread `01a13585-db9c-79b1-b0e6-60810ec05310`, local).
The outer caller sends sanitized aggregate observations there; due-expert owns
duplicate checks, classification and Due issue publication. Do not directly file
or comment on Due issues from this project. Workers return observations without
publication or recursive delegation. Consider fair bounded comparisons when
useful and record deferrals. Josh set a two-hour cadence: one bounded chunk per
run, followed by Done / Next / Blockers / Decisions, a relevant snippet and an
actual Pacific finish timestamp. Resume unfinished work rather than duplicating
it. F02 logging, F03 macro dependency hygiene, F04 proc-command fixtures and
F05 borrowed cursor, F06 string-view scope and F07 owner-bound string views are
complete; F08's ownership specification, F09's repair, F10's getter scope and
F11's selected getter/lifecycle repair are reviewed and validated locally. F12's
source-only registration ergonomics specification preceded F13/F14; the selected
fallible registration and builder proposals are now implemented. Read STATUS.md,
docs/F12_CONFIGURATION_API.md and compact evidence. F11 leaves fork PR4 draft for deferred CI/remote handoff.
F13's uniform fallible numeric/bool/enum registration, rejected-data cleanup,
macro propagation and selected isolated migrations are validated locally. Read
docs/F13_FALLIBLE_CONFIGURATION.md and its evidence. F14's additive typed builders
are complete and validated locally: integer/boolean/string/enumeration factories,
explicit range, named options, consuming Result and measured author-code savings.
Read docs/F14_CONFIGURATION_BUILDERS.md/evidence. F15's source-only scorecard is
reviewed and locally complete: docs/F15_EVIDENCE_PRIORITIES.md and compact
f15-evidence-priorities.json distinguish scoped measurements, compiler/native
contracts and remaining gaps. No SDK tests/builds/benchmarks were rerun in F15.
F16's CallReply conversion specification and F17 implementation are reviewed and
locally complete. Read docs/F17_CALL_REPLY.md, docs/evidence/f17-call-reply.json
and target/f17/checkpoint.json. F17 valid text is BulkString, invalid is
StringBuffer; private helpers consume/recover one owned buffer. Key variants,
strict extraction and explicit statuses stay; wrapped native status is bulk.
Selected parser/config/DUMP/host/archive/reference checks pass. Source review
approves; final pure/archive checks include two test/comment refinements without
production changes. No allocation/throughput/general FFI safety claim.

F18's source-only LoadConfigs specification is reviewed and locally complete.
Read docs/F18_LOAD_CONFIGS.md, docs/evidence/f18-load-configs.json and
target/f18/checkpoint.json. Caller verified 49 source/receipt predicates, 84 SDK
and 135 original Rust files/excerpts/provenance. Due/Claude approves with one
nonblocking clarification, applied with caller refinements. Two original reads
were denied to the worker; caller verified originals independently. No SDK
implementation, compilation/test/benchmark/native execution occurred in F18.

F19's additive load_configurations helper and same-position macro/manual error
propagation are complete and validated locally. Read docs/F19_LOAD_CONFIGS.md,
docs/evidence/f19-load-configs.json and target/f19/checkpoint.json. Missing API
and native failure return distinct static errors, with no errno/retry/rollback.
F11 guard stays pending on error; helper-command/event/init/commit order remains.
Absent configurations block has no generated call; source/compiler controls
establish that property, not a native call-count trace. Selected manual snapshots
preserve Result String/map_err and JMESPath Result/init handling. Originals stay
unchanged. Thirty-eight pure tests, ten checkout plus ten actual-archive controls,
38 examples both profiles, Clippy/fmt/docs, selected native/ownership/reference
checks and source-verified archive consumers pass. Independent Due/Claude review
approves; one doc-only evidence clarification applied. No SDK push/PR or CI work.

F20's source-only evidence checkpoint is reviewed and locally complete. Read
docs/F20_EVIDENCE_CHECKPOINT.md, docs/evidence/f20-evidence-checkpoint.json and
target/f20/checkpoint.json. Caller checked 33 claims, 96 retained input/receipt
hashes (three changed F15 sources matched historical Git blobs), 176 protected
preexisting files and four original heads/status/135 Rust hashes. One Due/Claude
review approves; two nonblocking wording clarifications were applied. No SDK
implementation, compilation/test/benchmark/native execution occurred in F20.
F17 bulk/wrapped-status compatibility and F19 error-phase cleanup/order effects
are distinct from scoped parser/layout/F14 line measurements. F19 author code
comparison is unmeasured; source review is not a runtime/hash certification.

F21's source-only accepted configuration metadata retirement specification is
reviewed and locally complete. Read docs/F21_CONFIGURATION_RETIREMENT.md,
docs/evidence/f21-configuration-retirement.json and target/f21/checkpoint.json.
Caller checked 44 source predicates, five pinned native source origins/28 excerpts,
86 SDK Rust files/40 lexical sites (not registrations), 178 protected files and
four original heads/status/135 Rust hashes. Due/Claude approves; three nonblocking
source clarifications applied. No SDK implementation, compile/tests/benchmarks or
native execution occurred. Selected native removal follows callback return; no
private-data destructor/unregister-completion hook or post-removal SDK cleanup
window is available in these pins. Cleanup can invoke auth callbacks before
config removal; no arbitrary query legality, auth audit or execution is claimed.
Keep accepted boxes/captures/Rc stable. F11 cached owners/rejected-data cleanup
remain distinct and unchanged. Registry retirement must precede any future free,
with proved native last-use/quiescence plus image/thread/allocator validity.

No F22 implementation or new audit/default backlog is selected. The retirement
implementation is blocked on native contract evidence, not permissions or builds.
An optional next-focus question was posted; reconcile any direct human answer
before choosing the next lane. Until a new authorized focus or substantive native
retirement evidence arrives, preserve this safe checkpoint and specific blocker;
do not duplicate F21, rerun unchanged reviews/tests or spend on model work merely
because the schedule fires. Keep the existing two-hour report cadence; CI stays
held unless Josh explicitly resumes it. No pointer free, tombstone/native callback
experiment, broad Context/GIL/thread/alias/reentry/panic/static-resource audit,
original migration, push/PR/publication, server change, MSRV/package/support/release
choice, invalid historical execution, fake Context, arbitrary raw callback or
crash/leak sampler. If new in-scope evidence arrives, scope one bounded follow-up
and its independent validation before implementation; issue visibility still
applies to any tracked implementation. Later direct human direction supersedes
this standby checkpoint. Do not turn the proposal's future test gates into
already-executed coverage or automatic work.
F17 draft PR5 and drafts1–4 stay open for deferred CI/remote handoff; master was
not pushed. Required F17 baseline uploaded F11 merge and F12–F16 history under
issue-visibility authority. Leaves upstream234/134/246/358/473 open; issue471 is
excluded. Preserve PR93 MikkelSnitker at f1ec666ecc6891e11130df853e8b80c758df5230,
prior BSD/F02/F09 credit and selected native gaps. No replay of completed tests/
reviews or new CI/MSRV/package/support/release commitments.
Preserve tuples, custom traits, successful defaults/commands, existing load order
and F11 string lifecycle; F19 deliberately makes load failure observable. Accepted metadata reclamation/general native audit stay
separate. Do not duplicate completed F13/F14 reviews/tests without new changes.
Refresh overlaps and follow local issue-independent process unless mapped to
tracked issues, which requires draft visibility before code. CI stays deferred;
originals remain untouched. Do not repeat completed F11/F12 checks without changes.
Do not expand
into a general FFI audit or execute historical invalid native paths. This trial grants no additional SDK push,
merge, release, credential, settings or cross-project authority. See the current
[trial checkpoint](docs/DUE_TRIAL.md).
