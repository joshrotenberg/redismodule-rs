# Fork status

Updated 2026-10-14. Direction: improve the original SDK with justified Rust API
changes. Work is local; automatic publication remains disabled. Package names
and versions are inherited, not a new fork release.

**CI follow-up:** the user deferred the broader CI pass. Workflow work remains
provisional; F02 draft jobs ran with failures, so remote CI is not certified.
Continue local SDK validation independently. Revisit actual server/platform
coverage, package-consumer jobs, failure handling and retained evidence when
the CI workstream resumes.

## F16 — Call-result conversion scope: reviewed locally

| Status | Report |
|---|---|
| **Done** | Source-only specification recommends valid UTF-8 BulkString and invalid StringBuffer, reusing one owned copy and retaining key variants. Inventoried 218 tracked Rust files/four references; 37 caller document/source checks, 12 source hashes and ten reference excerpts checked. Due/Claude approves; five nonblocking refinements applied. Native source distinguishes 7.2 direct status output from 8.10 CR/LF mapping. No implementation, compilation, SDK tests/benchmarks/native execution. |
| **Next** | F17: narrow conversion repair, version/enterprise text consumers and config-get forwarding/selected corrected fixtures. Before code, refresh overlap and create/push post-F16 baseline/empty kickoff plus scoped draft listing upstream234 and PR93 credit. Baseline uploads local F11 merge/F12–F16 ancestors under issue-visibility history authority; never push fork master. |
| **Blockers** | None for local F16. Future implementation remains gated on draft visibility and corrected-code compiler/native/archive/reference validation. CI/drafts1–4, accepted metadata and broad native contracts remain deferred. The source proposal has no runtime pass or measured allocation saving. |
| **Decisions** | Prefer valid-text bulk plus existing binary fallback over always-byte values or opt-in-only repair; preserve strict extraction and key Eq/Hash/Ord. Wrapped status becomes bulk deliberately; explicit module status stays. Existing JMESPath text patterns need no selected domain rewrite. Original modules unchanged; no new package/MSRV/support/release commitment. |

Details: [F16 scope, options and validation gates](docs/F16_CALL_REPLY_CONVERSION.md),
[compact evidence](docs/evidence/f16-call-reply-conversion.json).

## F15 — Evidence and priorities: reviewed locally

| Status | Report |
|---|---|
| **Done** | Scorecard against all three goals records corrected logging/ownership/registration, Rust-like author APIs and measured parser/layout/line-count gains. Existing receipts independently checked: 54 claim checks/59 input hashes; 122 protected SDK files and four original modules' heads/status/135 Rust hashes unchanged. One new Due/Claude source review approves; four nonblocking precision suggestions applied. No SDK tests, builds, benchmarks or native executions rerun. |
| **Next** | Recommend F16 source-only scope of CallReply string-to-RedisValue byte/wire conversion, an existing F01/F09 gap with upstream PR93 overlap. Inventory the direct value/key conversion sites and emitter, choose API/migration options and corrected-code validation gates before implementation. Later human focus supersedes this recommendation; do not start F16 in F15. |
| **Blockers** | None for the local scorecard. CI/drafts 1–4 stay deferred; accepted metadata and broader native Context/GIL/thread/alias/panic contracts remain unresolved. Native no-user/template-hash/special-unload/worker-thread and platform gaps remain explicit. F15's read-only CI snapshot confirms F07 five successes/three failures; its earlier retained snapshot was four/three/one pending. No CI rerun or repair occurred. |
| **Decisions** | Continue the hybrid fork on evidence: parser fixture 3→0 Rust requests, arm64 string owner 16→8 bytes, selected builder calls 62→39 and 60→41 lines. These are distinct scoped measurements, with no whole-project throughput or safety claim. Source-only next scope is the default unless redirected; CI/MSRV/package/support/release choices remain held. No push/PR/publication or original module migration. |

Details: [F15 scorecard and priorities](docs/F15_EVIDENCE_PRIORITIES.md),
[compact evidence](docs/evidence/f15-evidence-priorities.json).

## F14 — Typed configuration builders: validated locally

| Status | Report |
|---|---|
| **Done** | Additive configuration::config integer/boolean/string/enumeration factories, explicit integer range, named flags/callbacks and consuming Result registration delegate to existing functions. Actual selected calls shrink 62→39 lines (stream-filter) and 60→41 (JMESPath), retaining error/callback bodies. 30 pure tests, 26 compiler plus 26 archived-SDK controls, 18 doc tests, all 35 examples both profiles, Clippy/fmt, 64 paired native backend/host/profile/mode cells, four isolated reference builds/eight flows and fresh archive/consumer checks pass. Independent Due/Claude source review approves; caller verifies contracts/effects separately. |
| **Next** | F15 source-only evidence/priorities checkpoint: summarize F02–F14 gains against the three project goals, distinguish implemented/tested/proposed work, list known gaps and recommend the next bounded SDK slice. Use existing evidence; no automatic new feature/CI/general native audit or unchanged test/review replay. |
| **Blockers** | None for local F14. CI/drafts 1–4 and accepted userdata cleanup remain deferred. Source-line savings are scoped call-site measurements; no allocation/throughput/support/safety certificate. Pure missing-API coverage remains inherited from F13, no invented native host cell. |
| **Decisions** | Final factory name integer, not the historical i64 proposal; static storage, borrowed name/default, existing Fn callback and string load admission retained. Free APIs/tuple grammar/successful protocols remain. Original modules unchanged; no push/PR/publication or MSRV/package/support choice. |

Details: [F14 API, measured examples and validation](docs/F14_CONFIGURATION_BUILDERS.md),
[compact evidence](docs/evidence/f14-configuration-builders.json).

## F13 — Fallible scalar registration: validated locally

| Status | Report |
|---|---|
| **Done** | Numeric/bool/enum registration returns Result with unavailable-API/native-error handling, immediate errno capture and rejected callback-data cleanup. Macro failures log/return ERR; accepted metadata stays stable. 29 pure tests (four new ownership probes), 10 compiler controls plus 10 against actual archived SDK, all 34 examples both profiles and Clippy/fmt pass. 56 native host/profile/mode cells, six isolated reference builds/12 flows, four archives/three consumers/six builds pass. Core and focused Due/Claude source reviews approve; caller checks contracts/effects separately. |
| **Next** | F14 additive typed configuration builders from F12: explicit integer range, named flags/callbacks, consuming Result registration delegating to the existing functions. Preserve tuple DSL/custom traits and validate compiler/native/author-code evidence. Start on a fresh local branch after overlap refresh; no second chunk in this activation. |
| **Blockers** | None for local F13. CI remains deferred and drafts 1–4 stay open. Accepted userdata/closure/static-resource reclamation and broad native contracts remain separate. Missing-API native host untested; pure shared-helper coverage is explicit. Failed/partial harness attempts are retained and corrected. |
| **Decisions** | Deliberate Rust Result source break and observable registration rejection; successful command/default/parser/LoadConfigs behavior unchanged. Prefer consistent integer/boolean/string/enumeration factory names next. F13 stays local with no push/PR/publication; original modules unchanged, no new MSRV/package/support commitment. |

Details: [F13 implementation and validation](docs/F13_FALLIBLE_CONFIGURATION.md),
[compact evidence](docs/evidence/f13-fallible-configuration.json).

## F12 — Configuration registration ergonomics: reviewed locally

| Status | Report |
|---|---|
| **Done** | Source-only specification from 214 tracked Rust files/four references, current SDK/native registration contracts and fresh 4-fork/45-upstream-PR/55-issue overlap snapshots. Due/Claude approves; caller checked contracts, hashes and findings. Proposed direct-call comparison is 57 to 39 consistently formatted nonempty lines across six stream-filter calls, including five new error conversions; APIs remain uncompiled. |
| **Next** | F13: only uniform fallible numeric/bool/enum registration, rejected-private-data cleanup, macro error propagation and selected isolated direct-caller migrations. Read F12 gates, refresh overlaps and apply the appropriate local/issue visibility process before implementation. Builders follow in a later bounded chunk. |
| **Blockers** | None for F12 scoping. No proposed API/compiler/native runtime validation occurred; those are F13/future gates. CI remains deferred, drafts 1–4 stay open; accepted userdata cleanup and broader native safety remain outside the claim. |
| **Decisions** | Preserve tuple DSL, ConfigurationValue traits, successful command/default/parser/LoadConfigs behavior. Registration failures intentionally become visible. Scalar Result is a source-contract change; additive typed builders come later, spelling provisional. Original references unchanged; F12 stays local with no push/PR/publication or MSRV/package/support commitments. |

Details: [F12 registration proposal](docs/F12_CONFIGURATION_API.md),
[compact evidence](docs/evidence/f12-configuration-api.json).

## F11 — String-configuration reply ownership: validated locally

| Status | Report |
|---|---|
| **Done** | Each string adapter retains zero/one reply owners and returns a borrowed native pointer. Macro load/error/unload coordination spans direct init registrations, tracks native acceptance, preserves canceled unload and releases final cache after deinit. String registration returns Result with native errno; unsafe raw lifecycle recipe and owner-thread TLS/Rc state avoid Send/Sync transport. Two independent Due/Claude reviews approve. 25 pure Rust tests (six meaningful new state probes), seven compiler controls, 17 docs and 22 Python tests pass; all 33 examples build both profiles, Clippy/fmt pass. Four native ownership host/profile cells, four existing configuration cells, four isolated reference builds/eight flows, four fresh archives/six consumers/12 builds pass. Archive production bytes match sources; originals unchanged. |
| **Next** | Select F12 focus; recommendation is one source-only configuration API ergonomics specification with actual reference patterns, before/after examples and validation gates. Keep implementation separate; do not duplicate F11 or automatically resume CI. |
| **Blockers** | None for local F11. CI/remote handoff remains deferred and fork PR 4 stays draft alongside 1/2/3. Full userdata/closure/static backing reclamation remains separate. Worker-thread config callbacks, special native unload refusals and broader platform/GIL/alias/panic contracts are not certified. |
| **Decisions** | Preserve ConfigurationValue traits, text/default/command and LoadConfigs result/order policy. Direct string callers propagate Err; raw callbacks explicitly manage legal load/unload. Cached owner accounting is tested, not all memory/throughput/safety. Leaves upstream 358 open; 471 excluded. Baseline upload includes F09 local merge/F10 docs; fork master/GitHub merges/releases remain human-owned. No MSRV/package/server support choice. |

Details: [F11 implementation and migration](docs/F11_CONFIGURATION_OWNERSHIP.md),
[compact evidence](docs/evidence/f11-configuration-ownership.json),
[fork draft PR 4](https://github.com/joshrotenberg/redismodule-rs/pull/4).

## F10 — String-configuration getter specification: reviewed locally

| Status | Report |
|---|---|
| **Done** | Source-checked borrowed getter versus temporary setter/error transfer on retained 7.2.8 and pinned 8.10.2. Inventoried 210 tracked Rust files/four references and refreshed 55 issues/45 upstream PRs/three fork drafts. Specified one owned reply per string adapter with narrow load-error/success/canceled-unload cleanup. Due/Claude initial needs_changes and focused approve; caller verified/corrected load-order assumptions and acceptance gates. Result/answer/trace contracts and input hashes checked; no SDK implementation, compilation or native execution. |
| **Next** | F11: implement only the reviewed getter owner/lifecycle/registration contract after fresh overlap checks and required dedicated baseline/empty kickoff/scoped draft. Corrected-code state/compiler/native/isolated migration/archive validation; carry this chunk forward if unfinished. |
| **Blockers** | None for F10 scoping. F11 cannot begin code until draft visibility exists and must justify narrow registry access/native lifecycle phases. Full userdata/closure allocation reclamation is deferred; no complete memory/safety claim. Two outside-cwd reviewer reads denied; caller verified retained reference excerpts without expanding permission. CI remains deferred, drafts 1/2/3 remain open. |
| **Decisions** | Preserve custom ConfigurationValue traits, strict text/default/command behavior; string registration gains Result and unmanaged raw entry needs explicit unsafe lifecycle recipe. Reply cache bounded to zero/one snapshots per registration, closure releases its cached owner; stable private data/registry pointer retirement kept explicit. Original references unchanged; local docs-only no-ff merge, no F10 push/PR/publication. Leaves upstream 358/471 open; F11's partial issue scope must be listed before implementation. No new MSRV/package/support commitments. |

Details: [F10 repair specification](docs/F10_CONFIGURATION_GETTER.md),
[compact evidence](docs/evidence/f10-configuration-getter.json).

## F09 — Independent native string ownership: validated locally

| Status | Report |
|---|---|
| **Done** | Private NonNull RedisString owner with no cached context; explicit unsafe acquisition/adoption, actual producer AM detachment, independent binary creation, unsafe raw decoder/adapters, balanced direct producer/scan reference handling, current-user Option and consuming transfer/defrag boundary. Two Due/Claude source reviews approve. 28 compiler controls, 28 pure/harness tests, 14 docs, 22 Python tests, 31 examples in both profiles, 68 unique native host/profile cases plus 16 final affected fixture rechecks pass. Eight isolated reference builds/eight selected flows and four fresh archives/six consumers/12 builds pass on 1.94.0/1.98.1. Production archive bytes independently match current sources; Clippy/fmt pass. |
| **Next** | F10 scoping only: investigate the source-verified string-configuration getter return/release contract, inventory overlaps and affected uses, and write corrected-code validation criteria before implementation. |
| **Blockers** | None for local F09. Native no-user cell and 8.10 template hash modes remain explicitly untested. Existing configuration getter release leak is preserved as a separate follow-up; its behavior tests do not certify ownership balance. Archive disk-full failure was resolved by reclaiming generated cache; failed/partial attempts are retained. Remote CI remains deferred and PR 3 stays draft. |
| **Decisions** | Justified source breaks and native append policy correction; command handler shapes, selected protocols/UTF-8/numeric/stored-format policy remain. One pointer/8-byte owner on tested arm64, no throughput/global safety claim. Leaves upstream issue 358 open; exact PR 487/488 attribution retained. Baseline upload includes F07 merge/F08 history; fork master/GitHub merges/releases remain untouched. Original references unchanged; no MSRV/package/support commitment. |

Details: [F09 implementation, migrations and limits](docs/F09_STRING_OWNERSHIP.md),
[compact evidence](docs/evidence/f09-string-ownership.json),
[fork draft PR 3](https://github.com/joshrotenberg/redismodule-rs/pull/3).

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
