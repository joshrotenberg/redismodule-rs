# Development handoff

State reconciled on 2026-10-22. Start here when resuming in another checkout or
account; Git and the issue backlog carry the technical state. Private working
notes are optional history, not the source of current instructions.

## Project and branches

This fork is being developed as SDK 3.0: retain the existing SDK foundation and
make correctness, safety and ergonomic improvements, including breaking changes.
Upstream acceptance is not a prerequisite. Version 3.0 is not published; use the
Git dependency recipe in [README.md](README.md).

| Track | Branch in [the fork](https://github.com/joshrotenberg/redismodule-rs) | Current functional checkpoint |
|---|---|---|
| Core SDK, priority 1 | `master` | `29d046c`, successful zero-deletion stream trims; earlier context/key/INFO lifetime fixes included |
| Paired examples and author documentation, priority 2 | SDK PRs target `master` | Existing guides are linked below; paired original/new example selection is still open in issue 63 |
| Wasm sidecar, priority 3 | `experiment/wasm-host` | `b787fef`, branch-scoped CI merged; includes SDK through `29d046c` |

Wasm remains an experiment on its own branch. Its [README](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/README.md),
[tracking](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/TRACKING.md)
and [CI guide](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/CI.md)
contain host capabilities, module-API gaps, historical benchmarks and validation.
The earlier long-form handoff remains available in [Git history](https://github.com/joshrotenberg/redismodule-rs/blob/b787fefe4082f0ec98730c2334bf7ef0083bb47c/HANDOFF.md).
Historical benchmark numbers are not fresh measurements or a current comparison
with Redis core.

Only `master` and `experiment/wasm-host` are active integration branches. Merged
PR branches, the old `wasm` remote and archived local refs are historical, not
unmerged implementation work. No repository/org relocation has been performed.

## Completed work and evidence

[CHANGES-FROM-UPSTREAM.md](CHANGES-FROM-UPSTREAM.md) indexes the fork's changes;
[CHANGELOG.md](CHANGELOG.md) records release changes and
[the migration guide](docs/MIGRATING-3.0.md) explains required code changes.
Completed work includes configuration validation/ownership, the test harness,
command builders, context capabilities, nested call-user scopes, binary/native
string ownership, typed timers, macro/dependency refreshes and compatibility
validation. The guides below contain the supported author-facing patterns.

Recent functional checkpoints, before this handoff documentation change:

| PR | Result / merge | Independently reviewed head | Passing PR CI |
|---|---|---|---|
| [80](https://github.com/joshrotenberg/redismodule-rs/pull/80) | INFO callback guidance, `a14b71b` | `611ce3c` | [39787134329](https://github.com/joshrotenberg/redismodule-rs/actions/runs/39787134329) |
| [81](https://github.com/joshrotenberg/redismodule-rs/pull/81) | Guidance indexes, `084cd23` | `57171d7` | [39787995906](https://github.com/joshrotenberg/redismodule-rs/actions/runs/39787995906) |
| [82](https://github.com/joshrotenberg/redismodule-rs/pull/82) | INFO snapshot lifetimes, `d0bba7d` | `51d7dde` | [39796004066](https://github.com/joshrotenberg/redismodule-rs/actions/runs/39796004066) |
| [83](https://github.com/joshrotenberg/redismodule-rs/pull/83) | Open key/context lifetimes, `01b37b5` | `203077d` | [39802306088](https://github.com/joshrotenberg/redismodule-rs/actions/runs/39802306088) |
| [84](https://github.com/joshrotenberg/redismodule-rs/pull/84) | Zero-count trims, `29d046c` | `a0c28fd` | [39806809017](https://github.com/joshrotenberg/redismodule-rs/actions/runs/39806809017) |
| [79](https://github.com/joshrotenberg/redismodule-rs/pull/79) | Wasm CI, `b787fef` | `cb65413` | [39809579615](https://github.com/joshrotenberg/redismodule-rs/actions/runs/39809579615) |

Every SDK row passed all 20 expected checks at its reviewed head. PRs 82 and 83
required real retries after GitHub left a successful job's check unfinished;
merges waited for complete passing results for every expected check name. The
squash trees were verified against their reviewed heads. The wasm PR passed its
required Linux check, then the actual [experiment push run](https://github.com/joshrotenberg/redismodule-rs/actions/runs/39811561168)
passed at `b787fef`; [issue 69](https://github.com/joshrotenberg/redismodule-rs/issues/69)
is closed. CI logs can expire; committed tests and validation scripts are the
reproducible checks.

The macOS stripping recheck is also complete in [issue 70](https://github.com/joshrotenberg/redismodule-rs/issues/70).
Retain the host's `strip = "none"` override: Rust 1.94 on ARM64 macOS 27.0.1
still produced stripped libraries rejected by dyld. Unstripped artifacts passed
native load/call/persistence and unload/reload probes. The issue records inputs,
artifact hashes and the paired manifest-removal control. This is not a result
for newer Rust, Intel macOS or Linux; recheck after a supported-toolchain change.

## Remaining work

The [open issues](https://github.com/joshrotenberg/redismodule-rs/issues?q=is%3Aissue+is%3Aopen)
are the live backlog. Do not duplicate completed survey items or infer that an
old local branch or historical TODO is active work. There is no outstanding
implementation patch carried only in the working tree at this checkpoint.

| Issue priority | Issue | Status / next step |
|---|---|---|
| 1 | [85: blocking-call capture ownership](https://github.com/joshrotenberg/redismodule-rs/issues/85) | Source-inspection candidate only; no defect established. Build compiler controls and trace native promise/callback ownership before choosing a fix. No implementation started. |
| 1 | [88: git pre-release](https://github.com/joshrotenberg/redismodule-rs/issues/88) | Choose tag/version policy and obtain explicit release authorization before tagging. No tag, release or publication is authorized by this handoff. |
| 1 | [89: downstream module port](https://github.com/joshrotenberg/redismodule-rs/issues/89) | Pin revisions and agree on the port repository/branch first; no PR to the other organization without its owner's agreement. Record migration-guide corrections, behavior differences, unsafe workarounds and harness needs. |
| 1 | [63: original/new paired module examples](https://github.com/joshrotenberg/redismodule-rs/issues/63) | Choose the example and location, using the downstream port as input; provide shared behavior tests and an annotated comparison. |
| 1 | [62: loaded-keys-in-RAM option](https://github.com/joshrotenberg/redismodule-rs/issues/62) | Explicit adoption and target-server support decision required. |
| 2 | [51: retained native-handle storage](https://github.com/joshrotenberg/redismodule-rs/issues/51) | Design parked; no public shape or blanket `Send` wrapper approved. Keep separate from focused lifetime fixes. |
| 2 | [60: server configuration reads](https://github.com/joshrotenberg/redismodule-rs/issues/60) | Ownership, availability and native-versus-call fallback proposal needed. |
| 2 | [90: safe-wrapper coverage](https://github.com/joshrotenberg/redismodule-rs/issues/90) | Produce a checked, version-aware API inventory; rank concrete downstream needs before proposing wrappers. Missing coverage alone does not justify adding an API. |
| 3 | [54: external-runtime context access](https://github.com/joshrotenberg/redismodule-rs/issues/54) | Design audit; no persistent public context or new host-access API approved. |
| 3 | [55: streaming replies](https://github.com/joshrotenberg/redismodule-rs/issues/55) | Measure a concrete cost and settle reply capability/partial-output behavior before implementation. |
| 3 | [68: wasm allocator/accounting](https://github.com/joshrotenberg/redismodule-rs/issues/68) | Reproduce observations before choosing policy; distinguish compiler peaks, lasting heap, guest memory and Redis accounting. |
| 3 | [86: sidecar versus Redis core](https://github.com/joshrotenberg/redismodule-rs/issues/86) | Historical exploratory comparison needs a fresh, version-pinned capability matrix and equivalent bounded workloads. No fresh measurements or architecture decision. |

Issue priorities reflect the latest backlog triage; the overall track order remains
core SDK, examples/documentation, then wasm. Closed design decisions from the
2026-10-22 review are also part of the handoff:

- [52](https://github.com/joshrotenberg/redismodule-rs/issues/52): keep passive timer handles and explicit cancellation; no cancellation-on-drop owner planned.
- [53](https://github.com/joshrotenberg/redismodule-rs/issues/53): keep payload inspection in caller-owned state and `timer_remaining` metadata-only.
- [56](https://github.com/joshrotenberg/redismodule-rs/issues/56): keep the small command builder; use `#[command]` for richer metadata.
- [59](https://github.com/joshrotenberg/redismodule-rs/issues/59): retain exactly one compatibility floor; the guide and compile controls satisfy the issue.
- [61](https://github.com/joshrotenberg/redismodule-rs/issues/61): keep INFO output-only with atomics or other `Sync` state; the audit and guide are complete.
- [65](https://github.com/joshrotenberg/redismodule-rs/issues/65): downstream harness assessment is folded into issue 89; no new crate selected.

Reopen closed designs only with a concrete consumer; do not treat their closure
as an implementation of the previously proposed API. Park undecided work and
continue independent actionable items rather than blocking the whole workflow.

## Working agreement

1. Read this document and current issue/PR state. Fetch the fork and inspect
   local changes before starting; preserve unrelated user files.
2. Start SDK branches from `fork/master`, not a possibly stale local `master`.
   Start wasm PRs from `fork/experiment/wasm-host`. Use `codex/<topic>` branches
   and open a draft PR before implementation.
3. Keep changes focused. SDK behavior changes update the changelog/upstream
   comparison and the migration guide when breaking. Preserve existing syntax
   alongside optional builders. Technical docs and issues carry ongoing work;
   update this handoff when the current state or decisions change.
4. Use independent native subagents to review the actual PR diff. Due and its
   reporting/A/B workflow are disabled. Workers must not recursively delegate.
   Fix findings, run appropriate checks, and have the resulting changes reviewed
   again until no blocking findings remain.
5. The owner authorized automatic merges for this workflow's SDK PRs targeting
   `master` and wasm PRs targeting `experiment/wasm-host` after clear independent
   review and all exact-head CI checks pass. Record the reviewed commit and CI
   evidence; merge that exact head. Do not bypass unfinished/failed checks or
   unresolved findings. Use conventional subjects without attribution trailers;
   squash commit messages omit PR numbers. This does not authorize unrelated PRs.
6. After an SDK merge, merge the **merged SDK changes** into the experiment,
   preserve open PR ancestry and adapt only what the SDK change requires.
   Review all four pinned lock snapshots, run the experiment gates, update its
   tracking notes and verify the actual experiment push CI. Use the normal
   merge path rather than rewriting published experiment history:

   ```bash
   git fetch fork
   git switch experiment/wasm-host
   git merge --no-ff fork/master
   # resolve required adaptations, review lock drift, validate, record notes
   git push fork experiment/wasm-host
   ```

Writes go to `fork` (`joshrotenberg/redismodule-rs`). The upstream remote `origin`
is read-only: no upstream pushes, PRs, issues or comments. Crate publication,
releases and upstream integration are not authorized. Do not import archived
refs or private historical helper policies into this workflow.

## Reproducing validation

Use the toolchain pinned by `rust-toolchain.toml` (1.94.0), with MSRV 1.87.0.
If Homebrew Rust precedes rustup, put `$HOME/.cargo/bin` first on `PATH`.
On the SDK branch:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --examples
RUST_TEST_THREADS=4 cargo test --workspace
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --features all-features-but-xor
```

Build examples before filtered integration tests; they load modules from the
selected target/profile directory. `TEST_REDIS_SERVER_BIN` selects a server
binary. The [SDK workflow](.github/workflows/ci.yml) is the authority for all
compatibility-floor/server/MSRV jobs, including the selected legacy-server probes.

On the experiment branch follow its [CI guide](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/CI.md)
and bounded `wasm/ci/check.sh`. The Linux job pins Node 24.22.1 and Redis 8.10.2,
requires JavaScript and verifies all four Rust lock snapshots. Respect existing
developer lockfiles; follow the guide's setup instead of overwriting them blindly.
Alternate-floor SDK checks exclude `redis-module-wasm` because its dependency
selects the default floor. Host validation uses the default-floor workspace.

Latest full local results on Rust 1.94 / Redis 8.10.2: 69 SDK units, 158 SDK
integrations, six harness helpers, nine macro tests and 100 SDK doctests. The
experiment additionally passed its allocator-fallback check, seven host units,
21 host integrations including JavaScript and four guest helper tests, with
strict formatting/lint/docs and guest builds. Local JavaScript used Node 26.12.0;
pinned Node 24.22.1 was validated separately by Linux CI.

## Receiving account / checkout

Clone/fetch the fork and choose the branch for the work. A conventional setup is:

```bash
git clone https://github.com/joshrotenberg/redismodule-rs
cd redismodule-rs
git remote rename origin fork
git remote add origin https://github.com/RedisLabsModules/redismodule-rs
git fetch fork
```

Account authentication, editor/session state and recurring automations are
outside Git. Confirm the source account's scheduled work is stopped before
starting another runner; do not create duplicate automation. The owner's stop
request and the actual automation status must be checked in the source session.
Recreate scheduling in the receiving account only when requested.

Private `local-docs/`, old helper scripts/memory, internal strategy documents,
raw logs and local archive refs must not be published. Add `/local-docs/` to
`.git/info/exclude` if a private bundle is copied into a fresh clone. Historical
helpers can contain obsolete policies; use this handoff and the current human
instructions instead. No private bundle is required to find the active backlog,
understand accepted decisions or run the committed SDK/wasm validation.

Useful author guides: [configuration](docs/CONFIGURATION.md),
[INFO handlers](docs/INFO-HANDLERS.md), [compatibility floors](docs/COMPATIBILITY-FLOORS.md),
[module testing](docs/TESTING-MODULES.md), [shipping](docs/SHIPPING-MODULES.md),
[upgrades](docs/UPGRADING-MODULES.md) and [notification writes](docs/KEYSPACE-NOTIFICATIONS.md).
