# Due trial checkpoint — 2026-10-12

The caller verified Josh's direction to trial Due in the PM chat. The PM owns
scheduling; this project created no timer or host configuration. The hybrid-fork
direction, local scope, issue-visibility requirements and release boundaries
remain unchanged.

## First run

Reviewed clean F01 checkpoint `70815526e97cf054cd9cbc761f8907793e867b2b` with one
Due/Claude static review. The worker had repository read/search tools only,
disabled native Agent tools, no shell, no configured MCP servers and no recursive
delegation. It was allowed no edits, tests, external actions or defect
reproduction, and stopped after a structured diagnosis.

| Observation | Result |
|---|---|
| Due / provider | Due 0.1.0; Claude Code 2.1.296 |
| Available Due source | `76d33d410319d860425a2dd3609863a4ba5ca565`; tracked tree clean, one untracked entry |
| Due executable SHA-256 | `11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b` |
| Compiled source, observed model/effort | Unknown; not inferred from version or native defaults |
| Execution / review verdict | Exit 0, `succeeded`, structured `concerns` |
| Elapsed / reported cost | 262,636 ms; provider-reported USD 2.61266475 |
| Schema checks | Result, answer, all trace records and trace summary independently validate |
| Trace | 174 records; complete, zero dropped records, no sequence gaps or unmatched activities |
| Observed activities | 14 Read, 14 Grep, 2 Glob, 1 StructuredOutput; no failed activities |
| Effects | Reviewed input hashes and Git status unchanged after worker completion |
| Caller checks | 13 existing Python tooling tests pass; no Redis suite rerun |

The prompt, result, exit/signal, trace, summary, schemas and input hashes remain
local under `target/due-trials/20261012T183609Z/`. They are not public feedback
attachments. [Compact evidence](evidence/due-2026-10-12.json) records the outcome
and checked dispositions. Historical F01 runtime passes remain October 8 evidence.

## Findings after caller verification

| Finding | Disposition and next correction |
|---|---|
| Reference selected-test entries omit per-invocation server identity | Confirmed reporting gap. Record the selected executable/hash/version and, where the copied harness exposes it, observed identity. The possible pinning concern was narrowed: retained eventstream and streamfilter helpers already honor `TEST_REDIS_SERVER_BIN`; Carolina's copied helper does too. Do not label the original tests unpinned. |
| Reference runner records SDK HEAD without the live SDK input tree | Confirmed. Path dependencies compile the working tree. Capture SDK source/manifest/lock digests and status around compilation, including reused snapshots. Caller comparison found no SDK implementation/dependency changes between the F01 base and reviewed checkpoint. This does not invalidate the historical builds. |
| Normal-exit leftover cleanup sends only SIGTERM | Confirmed by source review. Follow up with bounded process-group cleanup and a benign helper-process test. No leaked process was observed or reproduced in this review. |

Caller verification read retained snapshots at
`target/f01/references/eventstream/tests/common/mod.rs:472` and
`target/f01/references/streamfilter/tests/common/mod.rs:135`, rather than inferring
behavior from their adaptation patches. Original source repositories were not
edited. A missing evidence field is not a demonstrated wrong runtime result.

A narrow local reference-runner follow-up is useful before extending this
infrastructure. Keep it separate from F02's logging implementation. No SDK source,
runner implementation, dependency, CI workflow or release setting changed in
this review chunk.

## Feedback and trial boundaries

The first invocation worked without retries, fallback or permission expansion.
Structured output, explicit limitations and complete traces were useful. The
caller still needed broader local context to narrow one recommendation. Cost
and latency are recorded without attributing either to Due or ranking models
whose actual identities/settings were not exposed.

A two-arm comparison was considered and deferred for this initial permission
and provenance calibration; no controlled model/effort question justified
duplicating the complementary review. Consider a fair two-arm read-only
comparison in a later suitable batch. Missing compiled-build and observed-model
identity are already tracked in Due issues 2 and 1.

[Sanitized feedback — Due issue 15](https://github.com/joshrotenberg/due/issues/15)
records positive outcomes and limitations. Only the outer caller published it;
no private source, prompts, raw logs, project identity or customer data were
included. Future delegated work continues through the plugin's local CLI, with
independent verification and other-provider implementation review when available.

## F02 refresh

On October 12 the fork has no open PRs. Upstream issue 473 and Josh's upstream
PR 476 remain open; the latter's head is
`437bf22b933c31d04573943b692542032edd9538`. No competing implementation was started.
Before F02 edits, reconcile scope again and complete the existing dedicated
branch, empty kickoff and draft-PR requirement. Keep CI deferred and leave MSRV,
package identities and support commitments undecided.

## October 13 — F05 source review

Due/Claude approves the bounded borrowed-cursor chunk. Actual exit 0, 181,991 ms,
provider-reported USD 2.44096925. Complete result/answer/request/146 trace records/
summary independently validate; no dropped records, sequence gaps, failed or
unmatched activities. SDK source was read and reviewed hashes stayed unchanged.
The reviewer ran no tests; caller-owned runtime/compiler checks establish effects.
The source-only reviewer suggested clearer observed measurement output; the caller
applied it and reran affected builds/runtime/Clippy/fmt. Other nonblocking ideas
remain SDK followups. Exact current source/binary/provider identity is retained
under `target/f05/review/`; no plugin/binary refresh or deadline was applied.
Requested/observed model and effort, and compiled source identity, remain unknown.
A two-arm comparison was considered and deferred because equivalent native
read-only permissions/tools are not established and SDK delivery has priority.
Sanitized aggregate findings route through due-expert; raw prompts/logs remain
local. See [F05 results](evidence/f05-arguments.json) for checked effects and limits.

## October 13 — F06 specification review

Due/Claude approves a documentation-only string-view repair specification.
Actual exit 0, 214,524 ms, provider-reported USD 2.37925025. Complete request,
result, answer, all 167 trace records and summary independently validate; 34
matched activities, no failures/drops/gaps/unmatched. Reviewed SDK/source/spec
hashes stayed unchanged. Existing native permission scopes worked; no timeout,
retry, recursive delegation or policy update. Model/effort/compiled source remain
unknown; exact source/binary/provider provenance is retained locally.

Caller applied eight nonblocking doc refinements and corrected one model
review overstatement: a pure metadata-only consumer (exit 0) shows a local
non-Iterator type can implement NextArg despite its blanket Iterator impl.
This is a model analysis limitation, not evidence of a Due CLI defect. No proposed
API was compiled or executed, no original module code changed, and no runtime
or invalid native path was run. Source/effect checks remain separate from review
approval. A two-arm comparison was considered/deferred pending equivalent native
read-only permission/tool settings; SDK delivery remained primary. Sanitized
positive/negative observations route to due-expert. See [F06 evidence](evidence/f06-string-lifetimes.json).

## October 13 — F07 implementation and delta reviews

Due/Claude approves the core repair (192,047 ms, reported USD 2.555963) and a
focused callback-anchor/assertion delta (298,448 ms, USD 3.39145625). Both true
process exits are zero; complete requests/results/answers, 141 and 161 trace
records and summaries independently validate. Input hashes stayed unchanged
during each review. All 49 activities match; the core review marks one Grep
failed, with cause unknown in the metadata-only trace. Neither reviewer reports
denied reads. Model/effort/compiled-source identity remain unknown; available
source/binary/provider provenance is retained unchanged from this batch.

The caller applied the reviewers' concrete findings, checked diagnostic/source
claims and revalidated final source through compiler, native, archive and
isolated-reference tests. Reviewers only read/searched; no tests or hash checks
were performed by them. A new static-handler negative control reports E0716
on the pinned compiler, with an otherwise identical borrowed-handler positive
control. Missing evidence links/hashes and historical counts were corrected.

One caller CLI invocation passed inline JSON where --request requires FILE|-.
Due rejected it as invalid_input, true exit 2, before starting a provider or
creating a trace. After inspecting the absence of effects, the caller corrected
the file invocation with a fresh trace. This is caller misuse, not a Due defect.
There was no automatic retry, fallback, recursive delegation, deadline, provider
permission change or binary/plugin refresh. SDK disk pressure was handled only
by removing owned rebuildable caches and retaining evidence/binaries.

A two-arm comparison was considered/deferred because equivalent native tool and
permission settings are not established and SDK delivery remains primary.
Sanitized aggregate positive/negative observations route to due-expert; prompts,
code and raw provider output remain local. See [F07 evidence](evidence/f07-string-views.json)
for actual effects and limitations.

## October 13 — F08 ownership specification review

Two Due/Claude source-only reviews completed with true exit 0 and valid results.
The first returned needs_changes (359,140 ms, reported USD 4.75023175): it found
a missing native nullable-result case in the proposed ownership ledger. The
caller verified both native versions, revised the Option/phase/migration criteria
and related source-grounded details, then received approve on a focused delta
review (144,678 ms, USD 1.6711555). These are model review verdicts, not CLI errors
or implementation/runtime outcomes.

All request/result/answer contracts, 288 and 132 trace records and summaries
independently validate. There are 89 matched activities, no failures, drops,
gaps or unmatched starts. Reviewed inputs stayed unchanged during each worker.
All 209 production/reference Rust source files and six native source files are
unchanged after the caller's doc refinements. No proposed API was implemented or
compiled, and no native function/historical invalid path was executed. Reviewers
read/search source only; native/hash/Git effects checks remain caller-owned.

Available Due source is clean 76d33d410319d860425a2dd3609863a4ba5ca565; binary SHA
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b; Due 0.1.0 and
Claude Code 2.1.296. Compiled-source/model/effort identities remain unknown. No
retry, deadline, recursive delegation, permission expansion or binary/plugin
refresh. A two-arm comparison was considered/deferred because equivalent native
read-only tool/permission settings are not established; SDK specification delivery
remains primary. Sanitized aggregate evidence routes to due-expert; no direct Due
issue publication from this project. See [F08 evidence](evidence/f08-string-ownership.json).

## October 14 — F09 ownership repair

The caller implemented the reviewed string ownership contract. Two independent
read-only Due/Claude runs approve; actual exits are 0, answer contracts and
complete traces are independently validated. Initial review source-identified
an existing string-configuration getter release gap; caller confirms both native
sources and records it as a separate scoped follow-up. Fixture error handling,
provenance and lint findings are applied or explicitly bounded. No worker edits,
external publication, recursive delegation, provider retry or expanded grants.

Due 0.1.0/source 76d33d410319d860425a2dd3609863a4ba5ca565 (clean), binary SHA256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b;
Claude Code 2.1.296. Requested native model/effort omitted, observed values and
compiled binary source identity unknown. Core: 743992 ms, 456 trace records,
91 matched activities, USD 7.42270625. Delta: 359676 ms, 236 records, 42 matched
activities, USD 4.97235175. Both traces report zero drops/gaps/failed activities.
These are source-review events/costs, not proof of tests or side effects.

Caller separately validates selected native/compiler/archive/reference effects;
failed harness attempts and a disk-full package build remain retained. Only
generated SDK target/debug cache was reclaimed before a fresh package run.
Source-only review is expensive and initially relied on a stale fixture hash;
final affected rechecks/provenance resolve that. No wrapper defect or model
ranking is inferred. A/B deferred: equivalent native permission/tool setup is
not established, and SDK delivery takes priority. Sanitized aggregate feedback
is routed once to due-expert; raw prompts/provider logs stay local and ignored.

See [F09 evidence](evidence/f09-string-ownership.json) and retained target/f09/
review/result, trace, summary, validation, provenance and focused delta records.

## F10 — String-configuration getter specification

Two bounded read-only Due/Claude reviews completed through the installed plugin:
initial needs_changes (243,210 ms, reported USD 2.062249), focused revised-scope
approve (193,847 ms, reported USD 1.80372325). Both actual exits are 0 and Due
status succeeded; approval and execution are recorded separately. Caller validates
both result/answer/all trace records/summary contracts independently. Traces have
172/150 records, zero drops/gaps, terminal records and balanced activities.
The delta has two denied outside-cwd reference reads; permitted caller excerpts
and original hashes were independently checked without permission expansion.
No SDK source was changed or compiled/native-executed by F10.

The initial reviewer found an unreachable native pending-removal fixture gate;
caller moved it to Rust-only state validation, clarified stable registry pointers,
and read all three NULL consumers. Caller rejected two source assumptions:
default loading bypasses the getter comparison and stream-filter's actual macro
has no configurations block. Focused review confirms those dispositions. Its
suggested additional human permission gate is covered by existing API-break and
issue-visibility instructions; F11 code remains gated on its scoped draft.
Registry transport requires a narrow access justification, not a blanket unsafe
Send/Sync for private user data. Full userdata allocation reclamation stays deferred.

Due source remains 76d33d410319d860425a2dd3609863a4ba5ca565, tracked tree clean;
Due 0.1.0 binary SHA-256 11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b,
Claude Code 2.1.296, macOS arm64. Model/effort omitted; observed settings and compiled
source identity unknown. Read/Grep/Glob only, Agent disabled, no shell or MCP;
no automatic retry, recursion or altered permissions. No two-arm comparison:
equivalent provider permissions/setup is unestablished and SDK delivery prioritized.

Artifacts remain local under target/f10/review and review-delta. During delta
monitoring the caller briefly appended and removed a non-contract disposition
paragraph; original input hash restored, exact worker read timing unknown. The
proposed contract/production/native inputs remained unchanged; final documentation
contains verified nonblocking refinements. Do not claim continuous doc immutability.
Caller verified 210 tracked Rust hashes, reference heads/status and four native
hashes/Git blob. Sanitized aggregate feedback routes to due-expert; private prompts,
code, answers and raw logs remain local. See [compact F10 evidence](evidence/f10-configuration-getter.json).

## F11 — Selected configuration getter implementation

Core and final fixture/delta Due/Claude reviews approve, both true exit 0 and
status succeeded; caller independently validates result/answer/97 and 250 trace
records/summary. Complete traces, zero drops/gaps, all activities balanced, no
failed/denied reads or permission expansion. Exact result costs/durations and
provenance are in compact evidence. Native proof belongs to caller runs, not
review success. Worker source reads did not certify runtime/full ownership.

Caller applied nonblocking errno/phase/TLS/raw error propagation clarifications,
added a pure dispatch gate used by the actual setter, scoped log assertions and
logged fixture invariant failures. A construction-only canceled-unload unit was
removed; actual native refusal is the evidence. Compiler/native/copy/log harness
failed attempts are retained and corrections checked. No Due retry/fallback,
recursive agent, arbitrary deadline, global setting or external worker action.

Installed Due 0.1.0/source 76d33d410319d860425a2dd3609863a4ba5ca565 clean, binary
SHA-256 11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b; Claude
2.1.296. Models/effort omitted; observed settings/compiled source identity unknown.
Comparison deferred because equivalent native permission/tool setup is not
established and bounded SDK delivery takes priority. Full artifacts stay local
under target/f11; outer caller sends sanitized positive/negative batch metadata
to due-expert. No private prompts/code/answers/raw logs are feedback attachments.

See [compact F11 evidence](evidence/f11-configuration-ownership.json).

## F12 — Source-only configuration registration proposal

One independent read-only Due/Claude review approves, actual exit 0/status
succeeded, duration 189,240 ms and reported USD 2.339982. Caller independently
validated request/result/answer/all 140 trace records/summary schemas. The trace
is complete with zero drops/gaps, one terminal record and 24 balanced successful
activities; no denied reads or permission changes. Review success is source
evidence only. No SDK implementation, proposed API compilation or native execution.

Five nonblocking findings were checked and applied: resolve availability before
transferring callback private data, distinguish raw/formatted per-call metrics,
record RedisError's blanket conversion/coherence limit, retain complete selected
reference excerpts and keep factory names provisional. Caller additionally tied
load-error gates to actual macro order and isolated ignored-Result compiler
validation from JMESPath's existing warning suppression. These are doc/evidence
clarifications after approval; no unchanged model review was repeated.

The same 214 tracked Rust hashes/four native source hashes and original reference
heads/status remain unchanged. Source/format measurement is 57 to 39 nonempty
lines, with no compiled/runtime/throughput/allocation claim. Reviewed inputs were
immutable through completion and checked before refinements; original reviewed
specification/comparison/excerpts remain under target/f12/review.

Installed Due 0.1.0/source 76d33d410319d860425a2dd3609863a4ba5ca565 clean, binary
SHA-256 11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b; Claude
2.1.296, macOS arm64. Model/effort omitted; observed settings/compiled source
identity unknown. Read/Grep/Glob only, no shell/MCP/Agent/recursion/retry or
arbitrary deadline. Requested answer brevity was approximate (775 whitespace
tokens across JSON serialization, not a token-usage metric). Comparison deferred:
equivalent provider permission/tool setup is unestablished; SDK delivery first.
Sanitized aggregate positive/negative observations route only to due-expert;
private prompts/code/answers/raw logs remain local.

See [compact F12 evidence](evidence/f12-configuration-api.json).

## F13 — Fallible scalar registration implementation

Core and focused validation/migration Due/Claude source reviews approve; both
actual exits are 0/status succeeded. Durations 189,732/273,004 ms, reported costs
USD 1.72763925/3.51490325. Caller validates request/result/answer/all 94/214 trace
records/summary contracts, complete delivery, zero drops/gaps and balanced
activities. No failed/denied reads, permission changes, recursive agents, retry
or arbitrary model deadline. Reviewed inputs stayed unchanged until each completed.

Core review checked the stable raw pending owner, native errno/rejection/drop
contract and macro propagation. Caller applied per-mode errno checks and platform
suffix, independently confirmed Clippy's expectation fires, and retained the
guaranteed non-null Box pointer construction. Focused review checked corrected
MODULE LIST behavior, compiler fixtures, archive/reference graph/migrations;
caller added fresh metadata/F13 patch ledgers, clarified repeated rejected loads
and identified the authoritative passing runtime output. Production source did
not change after core approval. A runner comment-only edit has identical AST.

Actual native/compiler/archive/reference evidence belongs to caller runs, not
source review success. Caller caught/retained editor/fixture compile/counter/
builtin-module/package-name/retained-execute-mode harness mistakes and corrected
them; they are not attributed to Due. Source-only reviewers could not hash or
execute. Archive production bytes, original reference heads/status/hashes and
actual compiler diagnostic codes were separately checked.

Due 0.1.0/source 76d33d410319d860425a2dd3609863a4ba5ca565 clean, binary SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b; Claude
2.1.296/macOS arm64. Model/effort omitted; observed settings and compiled source
identity unknown. Read/Grep/Glob only. Fair A/B deferred: equivalent provider
permission/tool setup is unestablished and bounded SDK delivery takes priority.
Sanitized aggregate positive/negative observations route to due-expert only;
private prompts/code/answers/raw logs remain local under target/f13.

See [compact F13 evidence](evidence/f13-fallible-configuration.json).

## F14 — Additive typed configuration builders

One independent Due/Claude read-only source review approves, true exit 0/status
succeeded, duration 136,864 ms, reported USD 2.06370875. Caller validates request/
result/answer/all 127 trace records/summary contracts and immutable recorded
inputs through completion. Complete delivery, zero drops/gaps and balanced
successful activities; no denied/failed reads, edits, permission change,
recursion, retry or arbitrary deadline. Due success is not runtime proof.

Reviewer confirmed typed borrows/storage/callback bounds, consuming delegation,
fixture/metadata-only controls and actual reference-call migrations/counts. Two
nonblocking notes: F12's historical factory spelling/baseline now link to final
F14 results, and private Options intentionally leaves value bounds to each public
builder. No production change or unchanged model review was needed after approval.

Caller separately verified native pairs, compiler/archived exports, package and
reference graphs/hashes, original working trees and formatted comparisons. A
wrong expected compiler diagnostic and a native string-MEMORY rejection
assumption were corrected with retained evidence; host flag policy is unchanged,
and neither is attributed to Due. No historical invalid native/crash/leak path.

Due 0.1.0/source 76d33d410319d860425a2dd3609863a4ba5ca565 clean, binary SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b; Claude
2.1.296/macOS arm64. Model/effort omitted; observed settings/compiled source
identity unknown. Read/Grep/Glob only. Fair comparison deferred: equivalent
provider permission/tool setup unestablished; SDK delivery first. Outer caller
routes sanitized aggregate metadata to due-expert; private prompts/code/answers/
raw logs remain local under target/f14.

See [compact F14 evidence](evidence/f14-configuration-builders.json).

## October 14 — F15 new scorecard review

One Due/Claude read-only review approves the new source-only evidence/priorities
scorecard. True exit 0; duration 217,200 ms; provider-reported USD 2.44205525.
Request/result/answer, all 206 trace records and offline summary independently
validate. No gaps/drops/unmatched activities; one Grep activity failed with
metadata-only cause unknown. No permission blocker is reported; no provider
retry, permission expansion, fallback, recursive delegation or new comparison.
Inputs stayed unchanged through review completion. Available Due source remains
76d33d410319d860425a2dd3609863a4ba5ca565 (clean), binary SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b;
Due 0.1.0 / Claude 2.1.296. Requested model/effort omitted; observed settings
and compiled-source identity stay unknown. Plugin/binary were not refreshed.

Caller checked 54 claims/59 receipt hashes, current protected SDK inputs and
original module heads/status/135 Rust hashes. No completed SDK tests, builds,
benchmarks or native checks were replayed. Review caught an early CI snapshot
with one still-running job versus later prose's terminal five/three; caller's
fresh read-only GitHub snapshot confirms the terminal result without running CI.
Other nonblocking refinements identify the next-scope emitter/key boundary and
clarify production-helper probe wording. These documentation edits were checked
by the caller; no second model review or new SDK safety proof is claimed.

A two-arm comparison is deferred: this chunk authorizes one new scorecard review,
and equivalent native tool/permission settings remain unestablished. Positive
workflow evidence and the failed-Grep limitation route in one sanitized aggregate
to due-expert; raw prompts/source/answers/logs stay local. See the
[F15 evidence](evidence/f15-evidence-priorities.json) and target/f15/review.

## October 14 — F16 new source specification review

One Due/Claude source-only review approves the call-result conversion scope.
True exit 0; 167,278 ms; provider-reported USD 1.96762025. Request/result/answer,
177 trace records and offline summary independently validate. All 41 activities
match, with no failures/drops/gaps/unmatched records. Inputs stayed unchanged
through review. Five nonblocking doc/evidence refinements were checked/applied
by caller: checkpoint, config-forwarding gate, excerpt ledger, line anchors and
retained explicit-text parser control. No second model review or SDK implementation,
compilation/test/native/benchmark claim. Caller separately checked sources/hashes,
originals and final diff. Source/native snippets define later gates, not a pass.

Due 0.1.0, Claude 2.1.296; available Due source
76d33d410319d860425a2dd3609863a4ba5ca565 clean; executable SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b.
Requested model/effort omitted; observed settings/compiled-source identity unknown.
No binary/plugin refresh, deadline, permission expansion, provider retry,
fallback, recursive delegation or extra comparison. A/B deferred for the single
bounded source review and unestablished equivalent provider tools/permissions.
Sanitized aggregate feedback routes through due-expert; raw prompts/answers/code/
logs stay local. See [F16 evidence](evidence/f16-call-reply-conversion.json).

## October 14 — F17 implementation review

One monitored Due/Claude read-only source review approves the new implementation
and selected fixtures. True exit 0; 160,575 ms; provider-reported USD 1.92202025.
Request/result/answer, 163 trace records and offline summary validate. All 36
activities complete successfully, with zero gaps/drops/unmatched activity.
Reviewed inputs stayed unchanged through completion. Two nonblocking refinements
added whole-buffer test equality and a strict-text fixture comment; caller
checked production bytes unchanged and Python AST equivalence. Final pure/fmt
and fresh archive/consumer/compiler checks pass. The source reviewer did not run
checks or inspect raw receipts; the caller independently verifies execution,
archive provenance, remote visibility and unchanged originals.

Due 0.1.0, Claude Code 2.1.296; available clean Due source
76d33d410319d860425a2dd3609863a4ba5ca565, executable SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b.
Requested model/effort omitted; observed settings and compiled-source identity
remain unknown. No binary/plugin refresh, provider retry, deadline, permission
expansion, fallback or recursive delegation. A/B comparison deferred because
equivalent native tools/permissions remain unestablished and complementary SDK
review is the bounded task. Sanitized aggregate feedback was sent once to the
authorized due-expert hub and acknowledged. Private prompts/source/raw answers/
logs remain local. See [F17 evidence](evidence/f17-call-reply.json).

## October 14 — F18 source-specification review

One monitored Due/Claude read-only review approves the new LoadConfigs scope.
True exit 0; 273,868 ms; provider-reported USD 3.340327. Request/result/answer,
219 trace records and offline summary independently validate. Reviewed inputs
stayed unchanged through completion; all 46 activities match, no gaps/drops.
Three activities failed: two Read and one Grep. Reviewer reports denied original
reference reads outside its workspace, using retained excerpts instead. Caller
read the original signatures/tails and verified heads/status/135 Rust hashes.
The Grep diagnostic is not exposed and its cause remains unknown. No permission
expansion, provider retry, fallback, recursive delegation or native permission
bypass. Approval does not certify denied reads, caller hashes or remote state.

Caller checked 49 source/receipt predicates, native pinned blobs/initialization
bits and final scope. The review's nonblocking forced-unload clarification was
checked/applied; caller clarified pure slot versus real native guard tests and
proposed static error messages. No SDK implementation, compiler/test/benchmark
or native execution. Due source remains clean
76d33d410319d860425a2dd3609863a4ba5ca565; executable SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b,
Due 0.1.0 / Claude Code 2.1.296. Requested model/effort omitted; observed settings
and compiled-source identity unknown. No binary/plugin refresh or deadline.
Comparison deferred: bounded complementary review and unestablished equivalent
native tool/permission controls. One sanitized aggregate success/permission-limit
report was acknowledged by due-expert; raw private prompts/source/answers/logs
remain local. See [F18 evidence](evidence/f18-load-configs.json).

## October 15 — F19 implementation review

One monitored Due/Claude read-only review approves the implementation/fixtures.
True exit 0; 188,970 ms; provider-reported USD 2.55948325. Request/result/answer,
141 trace records and offline summary independently validate; all 27 activities
complete successfully, no gaps/drops/unmatched events. Inputs stayed unchanged
through review completion. One nonblocking wording correction distinguishes
absent-block source/compiler evidence from runtime call-count traces; applied
as documentation only, without changing production/fixtures/runners or repeating
unchanged SDK checks. Reviewer did not execute checks or re-derive raw receipt,
archive/hash/remote/original-state evidence; caller verifies those separately.

Due 0.1.0 / Claude Code 2.1.296; available clean Due source
76d33d410319d860425a2dd3609863a4ba5ca565, executable SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b.
Requested model/effort omitted; observed settings/compiled-source identity unknown.
No provider retry, deadline, permission expansion, fallback, recursive delegation
or plugin/binary refresh. Comparison deferred: complementary implementation
review and unestablished equivalent native provider tool/permission controls.
One sanitized aggregate was acknowledged by the authorized due-expert hub;
private prompts/code/raw answers/logs remain local. See [F19 evidence](evidence/f19-load-configs.json).


## October 15 — F20 source-only checkpoint review

One monitored Due/Claude read-only review of the NEW evidence scorecard approves.
True exit 0; 185,254 ms; provider-reported USD 3.16194225. Request/result/answer,
172 trace records and offline summary validate; 36 matched activities succeed,
no gaps/drops. Frozen reviewed inputs remain unchanged through completion.
Caller checked/applied two nonblocking doc-only clarifications: empty-block
failure depends on absent API, and keyspace handlers precede load while info/
server events follow. No SDK execution or unchanged model review was replayed.
Reviewer verified selected source/snippets/evidence consistency, not raw digest,
remote/original-state or runtime certification; caller checked those receipts.

Due 0.1.0 / Claude Code 2.1.296; clean available Due source
76d33d410319d860425a2dd3609863a4ba5ca565, executable SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b.
Requested model/effort omitted; observed settings and compiled-source identity
unknown. No deadline/retry/fallback/recursive delegation/permission expansion or
binary/plugin refresh. Controlled comparison deferred: bounded complementary
source review and unestablished equivalent native tool/permission controls;
no spend solely for cadence. One sanitized aggregate was acknowledged by the
authorized due-expert hub; raw prompts/source/answers/provider logs remain local.
See [F20 evidence](evidence/f20-evidence-checkpoint.json).


## October 15 — F21 source-only retirement specification review

One monitored Due/Claude Read/Grep/Glob review of the NEW specification approves.
True exit 0; 151,813 ms; provider-reported USD 2.47549025. Request/result/answer,
124 trace records and offline summary validate; all 23 matched activities succeed,
no gaps/drops/failures. Frozen reviewed inputs remain unchanged through completion.
Caller checked/applied three nonblocking source refinements: exact unload-body
ranges, 8.10 module-side config query/set exports and forced-unload NULL errmsg.
Source-only review does not certify hashes, original/remote state or runtime;
caller checked those separately. No SDK execution or unchanged model replay.

Due 0.1.0 / Claude Code 2.1.296; available clean Due source
76d33d410319d860425a2dd3609863a4ba5ca565 and binary SHA-256
11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b.
Model/effort requested omitted; observed settings and compiled-source identity
unknown. No deadline/retry/fallback/recursive delegation/permission expansion or
binary/plugin refresh. Comparison deferred: one complementary new-spec review,
unestablished equivalent native provider read/permission controls, no spend just
for cadence. One sanitized aggregate was acknowledged by authorized due-expert;
raw prompts/source/answers/provider logs remain local. See [F21 evidence](evidence/f21-configuration-retirement.json).


## F22 — Finite SDK roadmap and stream repair source review (2026-10-15)

Latest direct human direction resumes a planned finite SDK batch; F21 retirement
and CI stay parked. Caller source planning selected stream cleanup, raw-key
handoff and single-field hash result/byte contracts. One monitored read-only
Due/Claude review approved the new roadmap/first specification. Actual process
exit 0; request/result/answer/233 trace records/offline summary validate; 50 matched
successful activities, no failures/gaps/drops. Frozen inputs stayed unchanged
through review completion. Six nonblocking refinements were independently
source-checked and applied to docs only; no second unchanged review or SDK
compile/test/benchmark/native execution. Final records are target/f22/review.

Due 0.1.0, available clean source 76d33d410319d860425a2dd3609863a4ba5ca565;
binary SHA256 11d68ea7f0a5109cac6757db341aabb2d4c3068206c22254e1be48d696dcb29b;
Claude Code 2.1.296; macOS 26.7.1 arm64. Requested provider Claude, model/effort
omitted, observed model/effort unknown. Compiled source identity unknown; no
binary/plugin refresh. Duration 251251 ms; provider-reported USD 2.61984275.
No retries, permission expansion, alternate delegation or arbitrary deadline.
A/B comparison deferred: one complementary new-plan review was useful; another
comparison would not verify native effects or solve the parked contract gap.
Caller verified source/origin/protected-file/original/remote evidence separately;
source approval does not establish runtime coverage. Sanitized aggregate is routed
to the human-authorized due-expert hub, not directly to Due GitHub issues.
