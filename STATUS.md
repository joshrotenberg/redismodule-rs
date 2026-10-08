# Fork status

Updated 2026-10-08. Direction: improve the original SDK with justified Rust API
changes. Work is local; automatic publication remains disabled. Package names
and versions are inherited, not a new fork release.

**CI follow-up:** the user deferred the broader CI pass. Existing workflow edits
remain local and provisional; GitHub jobs have not been run to validate them.
Continue the local baseline independently. Revisit actual server/platform
coverage, package-consumer jobs, failure handling and retained evidence when
the CI workstream resumes.

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
