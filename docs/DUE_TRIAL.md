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
