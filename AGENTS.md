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

Next is one bounded F19 implementation of only the reviewed additive
configuration::load_configurations(&Context) -> Result<(), RedisError> and
macro/manual propagation, unless Josh redirects. Refresh overlaps/issue mapping,
current changes and owned workers first; resume unfinished owned scope. Use
fix/f19-load-configs-propagation from clean post-F18 master if issue-independent,
following validated local no-ff merge with no push/PR. If mapped to tracked
issues, satisfy dedicated baseline/empty kickoff/push/scoped draft visibility
before code; do not manufacture issue scope. Preserve PR229 MeirShpilraien at
566ad273797871acc6f50b5345a0c1cfa16fc34b as broad API ancestry, not a full port.

Keep existing optional configurations-block position and macro registration,
helper-command/event/init/commit order; absent block does not auto-load. Present
empty block now deliberately reports missing API. Resolve availability and call
native once per helper invocation; distinguish unavailable API/native failure
with static messages, no errno claim/retry/rollback. Use fully qualified $crate
macro path, log and return ERR while F11 guard is pending. Direct stream-filter
snapshot preserves Result<(), String>/map_err; JMESPath keeps migrated Result
and init propagation; event-stream tuples/custom storage stay. Migrate the two
selected SDK manual examples. No call deduplication/reordering, repeated-default
repair, LoadDefaultConfigs addition or new lifecycle/metadata/Context/GIL/thread/
alias/panic audit. Original modules remain read-only; isolated snapshots carry
all prior prerequisites/provenance.

Follow F18 acceptance gates: actual production pure availability/status adapter
without Redis APIs/fake Contexts; generic slot probes separate from legal native
guard integration; metadata-only compiler controls; owned legal OnLoad/default/
LOADEX/setter-error/order/owner-close/reload/unload checks; fresh archives/public
consumers/selected references, examples/pure/Clippy/fmt/docs and independent other-
provider source review. Never run old unchecked/invalid native paths or crash/
leak samplers. Forced post-load rejection still calls deinit under a native
assertion; do not use refusing deinit there. Native behavior is not release-
balance/general safety proof. Carry unfinished F19 rather than opening another
chunk; no new CI/MSRV/package/support/release commitment.
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
