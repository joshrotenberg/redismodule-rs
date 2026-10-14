# Fork status

Updated 2026-10-13. Direction: improve the original SDK with justified Rust API
changes. Work is local; automatic publication remains disabled. Package names
and versions are inherited, not a new fork release.

**CI follow-up:** the user deferred the broader CI pass. Workflow work remains
provisional; F02 draft jobs ran with failures, so remote CI is not certified.
Continue local SDK validation independently. Revisit actual server/platform
coverage, package-consumer jobs, failure handling and retained evidence when
the CI workstream resumes.

## F08 — RedisString ownership specification: reviewed locally

| Status | Report |
|---|---|
| **Done** | Inventoried 209 tracked Rust files across SDK/four references; classified acquisition, adoption, copying, AM detachment and outbound transfer. Compared retained Redis 7.2.8 and exact 8.10.2 native source with hashes. Specified an independent private non-null owner, no cached context, explicit unsafe raw ingress and corrected producer/scan migrations. Independent Due/Claude review identified an omitted nullable current-user result; source-verified Option mapping and other refinements were added, and focused re-review approves. All production/reference/native inputs remain unchanged. |
| **Next** | F09: implement the reviewed bounded ownership repair after fresh overlap/history checks and the required dedicated baseline/empty kickoff/scoped draft. Read the F08 ledger and validation criteria first; carry this one chunk forward if unfinished. |
| **Blockers** | None for F08 scoping. No proposed API was implemented, compiled or runtime-tested. F09 is gated on draft visibility. F07 remote CI finished five successes/three failures; CI remains deferred and PR 2 stays draft. Legal native no-user coverage is 8.10.2-only and must not be invented through a fake context. |
| **Decisions** | Recommend independent native ownership with no stored Context pointer; preserve useful creation/command interfaces and native append policy while correcting ownership accounting. Explicit source breaks include private inner, unsafe raw decoder/constructors, non-const adoption, current-user Option, optional scan value and mutable FnMut origins. Pass the real scan producer context. Leaves upstream issue 358 and related 134/221 open; preserve PR 487 author/revision attribution. F08 stays local, original directories untouched; no package/MSRV/support/release or general FFI audit. |

Details: [F08 repair specification](docs/F08_STRING_OWNERSHIP.md),
[compact evidence](docs/evidence/f08-string-ownership.json).

## F07 — Owner-bound string views: validated locally

| Status | Report |
|---|---|
| **Done** | try_as_str borrows its RedisString owner; consuming NextArg::next_str is removed; raw text/byte helpers require unsafe with explicit lifetime/access obligations. The notification adapter anchors its view to the local Context. SDK examples and isolated JMESPath/event-stream snapshots migrate without command changes. Final checks pass: 18 compiler controls, 14 docs, 26 pure/harness tests, 22 Python tests, all 30 examples in both profiles, 32 native SDK executions, eight archive cells and eight selected reference flows. Fresh archives/consumer builds, Clippy and fmt pass. Due/Claude core and delta source reviews approve; findings are applied and actual effects independently checked. |
| **Next** | F08: one scoping chunk for RedisString construction/ownership/public-inner boundaries, native alias obligations and migration choices. Specify corrected-code checks before implementation; stay within this type family. |
| **Blockers** | No local F07 implementation blocker. Empty-kickoff GitHub CI failed four of eight jobs; CI is deferred and fork PR 2 stays draft for the remote handoff. Native constructors/context/aliases remain separate unresolved contracts, not certified by these borrow checks. |
| **Decisions** | Deliberate Rust source break; strict UTF-8, numeric and existing command behavior remain unchanged. Leaves upstream issue 358 open. Required review baseline/kickoff uploads include earlier local F03–F06 history; fork master, release settings and original module directories remain untouched. Local validated merge follows the existing process; GitHub merge/release remain human-owned. No MSRV/package/support commitment. |

Details: [F07 implementation and migration](docs/F07_STRING_VIEWS.md),
[compact evidence](docs/evidence/f07-string-views.json),
[fork draft PR 2](https://github.com/joshrotenberg/redismodule-rs/pull/2).

## F06 — Legacy string-view repair scope: reviewed locally

| Status | Report |
|---|---|
| **Done** | Inventoried 208 tracked Rust files across SDK/four references; nine consuming text calls (three SDK, six JMESPath), direct owner-backed views and one external raw callback. Specified owner-bound try_as_str, removal of consuming NextArg::next_str and explicit unsafe raw views, with migration choices and corrected-code acceptance criteria. Reconciled 55 upstream issues/45 PRs and selected overlap patches. Due/Claude approves the specification; eight doc suggestions applied. One compiler-only consumer confirms custom NextArg implementations remain a migration concern, correcting a reviewer overstatement. |
| **Next** | F07: refresh overlap, publish the required dedicated baseline/zero-content kickoff and scoped draft for the partial issue-358 relationship, then implement and validate the reviewed narrow contract. |
| **Blockers** | None for F06. F07 implementation is gated on draft visibility; required baseline upload includes local ancestor chunks and must remain within issue-work authorization. CI remains deferred. Proposed APIs and migrations have not been compiled or runtime-tested. |
| **Decisions** | Recommend a deliberate Rust API break over warning-only deprecation or silent text copies. Preserve strict UTF-8 and command behavior; leaves upstream issue 358 open. No implementation, original module edits, pushes or GitHub changes in F06. No MSRV/package/support/release commitments. |

Details: [F06 scope and migration](docs/F06_STRING_LIFETIMES.md),
[compact evidence](docs/evidence/f06-string-lifetimes.json).

## F05 — Borrowed argument cursor: validated locally

| Status | Report |
|---|---|
| **Done** | New additive ArgCursor borrows strings/text/bytes from an explicit slice, retains native numeric parsing and provides exact-arity/tail handling. Synthetic comparison: three Rust allocations become zero (11/42/4106 requested bytes avoided); 360 paired parses across Redis 7.2.8/8.10.2 debug/release. Eight focused runtime executions, 26 pure/harness tests, four docs, expected E0515/E0505/E0502 diagnostics, Clippy with warnings denied, fmt and package listing pass. Independent Due/Claude source review approves; observed measurement reporting was improved and affected checks rerun. |
| **Next** | F06: scope a narrow legacy string-borrow lifetime repair, inventory affected uses and overlapping issues/PRs, and document required Rust API changes and migration/validation choices before implementation. |
| **Blockers** | None for F05. CI remains a deferred workstream. Native aliases/raw pointers and legacy borrowed text APIs are not certified; fresh archives and full reference-module suites were not rerun for this addition. |
| **Decisions** | Additive local API; strict UTF-8 is opt-in, binary bytes remain available, and u64 retains the existing i64::MAX limit. Original module directories stay untouched. No push/publication or MSRV/package/support commitment. Keep the next interval bounded to F06 scoping, with implementation as a later chunk. |

Details: [F05 API, before/after and limits](docs/F05_ARGUMENTS.md),
[compact evidence](docs/evidence/f05-arguments.json).

## F04 — Proc-command runtime fixtures: validated locally

| Status | Report |
|---|---|
| **Done** | Corrected keyless example metadata and module name; runtime assertions cover command metadata/key discovery and all derived reply fields/nesting/flattening/enum branch. Eight example tests pass on Redis 7.2.8/8.10.2, debug/release. Eight fresh archive proc flows pass across both hosts/profiles and Rust 1.94/1.98.1, including i64 endpoints, ordinary arity/parse errors and later valid commands. Four archives, 12 consumer builds, four registration tests, 19 Python tests, targeted builds, fmt and Clippy with warnings denied pass. Due/Claude approves; complete result/trace evidence is retained and independently validated. |
| **Next** | F05: one focused Rust API/ergonomics slice with before/after examples and measured evidence, informed by the four reference modules. |
| **Blockers** | None for F04. The first archive attempt exposed the string-only RESP2 harness limitation; the required decoder and its tests are corrected, and a fresh package run passes. Full 27-case baseline, RESP3, Cluster, persistence/replication and CI remain outside this chunk's evidence. |
| **Decisions** | Issue-independent local branch/no-ff merge, no push/publication. No production SDK API/dependency changes. Archive recovery checks use fresh connections; same-connection behavior is only claimed where tested. No MSRV/package/support commitments. |

Details: [F04 runtime contracts and limits](docs/F04_PROC_RUNTIME.md),
[compact evidence](docs/evidence/f04-proc-runtime.json).

## F03 — Public macro dependency hygiene: validated locally

| Status | Report |
|---|---|
| **Done** | All 11 registration macros use the SDK's linkme dependency through a shared ordered emitter. Authors declare only the facade and macro crate. The formerly failing no-linkme consumer and direct-linkme control pass; 11 linked-list assertions pass in four fresh archive compiler/profile cells. Four archives, six consumers and 12 builds pass on Rust 1.94.0/1.98.1. The 26 Rust and 15 Python tests, all 28 debug/release examples, formatting and Clippy pass. Due/Claude reads SDK source and approves the implementation; full result/trace evidence is retained. |
| **Next** | F04: meaningful proc-command runtime fixtures, then one focused Rust ergonomics slice informed by the four reference modules. |
| **Blockers** | Caller checkout writes and independent SDK source reads now work after Josh restored access. No F03 permission blocker remains. F03 checks compile/link behavior on native macOS arm64; Redis runtime is deliberately not rerun. F02 remote draft CI remains a separate deferred blocker. |
| **Decisions** | Issue-independent local branch and validated no-ff merge; no SDK push/publication. Preserve the two-crate public interface and conventional names. Alias resolution, legacy INFO branch and CI remain separate. No MSRV/package/support commitment. |

Details: [F03 implementation and limits](docs/F03_MACROS.md),
[compact evidence](docs/evidence/f03-macros.json).

## F02 — Logging correctness: validated locally

| Status | Report |
|---|---|
| **Done** | Shared NUL-to-visible-text conversion, literal message arguments at the C boundary, static C level strings and raw notice delegation. Five new pure tests; 26 Rust pure/harness tests and 13 Python tests pass. All 28 examples build debug/release. Both focused logging tests pass on Redis 7.2.8 and 8.10.2 in both profiles (eight executions), including real IO callbacks and exact retained log text. Due/Claude independently approved the implementation; its two nonblocking suggestions were applied and the affected checks rerun. Formatting and Clippy pass. |
| **Next** | F03: public macro dependency hygiene. Keep meaningful proc-command fixtures and one focused Rust ergonomics slice as subsequent chunks. The reference-runner evidence follow-up remains bounded and deferred unless selected validation needs it. |
| **Blockers** | Remote CI is not certified. Four of eight jobs failed in their Test step on the zero-content kickoff, before F02 code was pushed. CI modernization remains deferred; PR 1 stays draft pending remote failure triage. This limits the remote handoff, not the completed local logging checks. |
| **Decisions** | Preserve public signatures and upstream attribution. Use the existing local merge process for validated code; retain the remote draft for Josh's review and merge authority. SDK logger/context ownership remains a separate follow-up. Due feedback now goes to due-expert; one SDK chunk per two-hour interval, with a snippet and actual Pacific finish timestamp. No package/MSRV/support decision made. |

Details: [F02 implementation and evidence](docs/F02_LOGGING.md),
[compact validation](docs/evidence/f02-logging.json),
[fork draft PR 1](https://github.com/joshrotenberg/redismodule-rs/pull/1).

## October 12 — Due review checkpoint

| Status | Report |
|---|---|
| **Done** | Due/Claude performed a bounded read-only F01 review. Exit 0; structured answer and all 174 trace records independently validate; no worker edits. Caller re-ran 13 Python tests successfully. Sanitized feedback published as Due issue 15. |
| **Next** | Small local reference-runner follow-up: record selected-test host identity and live SDK inputs, strengthen leftover cleanup. Keep this separate from F02 logging and its draft-PR prerequisite. |
| **Blockers** | None for the Due trial. Review found evidence/cleanup limitations, not failed historical runtime cases. No SDK implementation or CI work started. |
| **Decisions** | Delegated model work uses Due; PM owns the schedule. A/B deferred for this first bounded review. MSRV, package names, support, publication and broader migration choices remain open. |

Details and caller-verified findings: [Due trial](docs/DUE_TRIAL.md).

## F01 — Local runtime, package and consumer baseline: complete

| Status | Report |
|---|---|
| **Done** | Bounded, owned-server harness; 21 Rust tests and 13 Python tests pass. All 27 examples build debug/release. 23 ordinary integration tests pass on Redis 7.2.8 and 8.10.2 in both profiles (92 executions). Four archives prove exact downstream provenance; eight fresh consumer builds pass on Rust 1.94/1.98.1. All four real modules adopt the fork without Rust-source changes and pass selected runtime flows. Rust 1.88 checks the locked workspace/all-target graph. |
| **Next** | F02: narrow logging-correctness repair with attribution and corrected-code validation. Check overlapping issue scope and complete the required draft-PR kickoff before tracked-issue implementation. Queue macro dependency hygiene and corrected proc-command runtime fixtures separately. |
| **Blockers** | None for this local baseline. Linux/jemalloc, older Redis hosts, inherited/archive proc-command fixture runtime, optional module integration and full application suites remain explicit coverage gaps. CI remains deferred. |
| **Decisions** | No MSRV, package release identity or broad host-support commitment selected. Runtime evidence is macOS arm64/libc. F01 is an issue-independent local chunk; no pushes, PRs or publication. The user's newer issue-visibility instructions govern future tracked-issue changes. |

Details: [baseline, findings and reproduction](docs/BASELINE.md),
[runtime evidence](docs/evidence/f01-runtime.json),
[archive consumers](docs/evidence/f01-package-results.md),
[reference consumers](docs/evidence/f01-reference-results.md).

## M01 — Workspace and dependency foundations: complete

| Status | Report |
|---|---|
| **Done** | Centralized manifests/dependencies, grouped support crates, tracked the lockfile, updated strum, repaired scripts/CI metadata, removed broken legacy tooling and added package notices/docs. The same 12 pure tests pass before/after; all six API-floor selections compile; all 27 debug/release examples build. Clippy passes; four archives inspected. Local branch validated and merged with a merge commit. |
| **Next** | F01: reproducible runtime/package/consumer baseline, bounded server startup, fresh archive consumers, four reference-module migration inventory and explicit support decisions. Deeper dependency migrations have separate gates in the meta report. |
| **Blockers** | None for M01. Rustdoc's 32 warnings and the API-8 unused-field warning match the original baseline. Runtime/consumer validation remains outstanding, not passing evidence. |
| **Decisions** | Local branches before pushing supersede the earlier draft-PR-first process. Preserve package/API/feature identities and developer Rust 1.94; no MSRV or new support promise. Keep enum-primitive-derive 0.1.2 because newer versions conflict with existing public conversions. |

Details: [meta pass and validation](docs/META_PASS.md),
[development workflow](docs/DEVELOPMENT.md), [working instructions](AGENTS.md).
