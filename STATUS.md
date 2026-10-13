# Fork status

Updated 2026-10-13. Direction: improve the original SDK with justified Rust API
changes. Work is local; automatic publication remains disabled. Package names
and versions are inherited, not a new fork release.

**CI follow-up:** the user deferred the broader CI pass. Workflow work remains
provisional; F02 draft jobs ran with failures, so remote CI is not certified.
Continue local SDK validation independently. Revisit actual server/platform
coverage, package-consumer jobs, failure handling and retained evidence when
the CI workstream resumes.

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
