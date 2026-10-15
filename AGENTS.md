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
F16's CallReply conversion specification is reviewed and locally complete. Read
docs/F16_CALL_REPLY_CONVERSION.md, its compact evidence and checkpoint before F17.
Next is only the reviewed narrow repair: implicit valid text becomes BulkString,
invalid text remains StringBuffer; reuse the failed owned buffer. Preserve key
String/byte variants and Eq/Hash/Ord, public signatures, strict extraction,
explicit statuses and unrelated reply policy. Migrate the two direct SDK text
consumers and validate module_config_get's forwarding plus selected fixtures.
No all-byte redesign, new borrowed API, broad reply/context/GIL/thread/alias/panic
or native-owner audit. Source proposal is not compiled/runtime evidence.
F17 relates directly to upstream234: refresh issue/PR overlap, credit PR93 at
f1ec666ecc6891e11130df853e8b80c758df5230, then reconcile review/f17-baseline at
post-F16 and fix/f17-call-reply-bulk-strings. Required baseline/empty kickoff push
and scoped draft must exist before code; record upload of local F11 merge and
F12–F16 ancestor history under existing issue-work visibility authority. Stop
implementation if visibility/history authority is blocked. Do not push master
or merge/release on GitHub. Originals remain read-only; selected isolated
snapshots carry previous prerequisites. Use only corrected-code tests/legal
native host phases; never old invalid wire/panic paths. F16 scope did no SDK
implementation/tests/builds/native execution. Do not replay unchanged reviews.
CI stays deferred; drafts1–4 remain open, no new support/package/MSRV commitment.
Preserve tuples, custom traits, successful defaults/commands/LoadConfigs policy
and F11 string lifecycle. Accepted metadata reclamation/general native audit stay
separate. Do not duplicate completed F13/F14 reviews/tests without new changes.
Refresh overlaps and follow local issue-independent process unless mapped to
tracked issues, which requires draft visibility before code. CI stays deferred;
originals remain untouched. Do not repeat completed F11/F12 checks without changes.
Do not expand
into a general FFI audit or execute historical invalid native paths. This trial grants no additional SDK push,
merge, release, credential, settings or cross-project authority. See the current
[trial checkpoint](docs/DUE_TRIAL.md).
