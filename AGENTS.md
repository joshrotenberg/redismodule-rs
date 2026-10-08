# Working instructions

## Direction

Improve this fork of `RedisLabsModules/redismodule-rs` for external module
authors, with a path for Search and RedisJSON. Preserve useful macros, parsers,
reply conversions and direct key operations. Justified Rust API breaks are
allowed; command behavior, replication, stored formats and supported hosts are
separate contracts. The from-scratch SDK remains paused.

## Current local process

The user's 2026-10-08 instruction selects local work with good branching before
pushing. This supersedes the earlier draft-PR-before-implementation requirement
for this phase. Do not push, open PRs, change GitHub settings or publish unless
the user requests that transition. The automatic publisher is disabled remotely.

CI modernization is a separate deferred workstream. Existing workflow edits are
local and provisional; local command results do not validate GitHub runner jobs.
Wait for the user to resume that work or request remote verification.

1. Read `STATUS.md` and the relevant plan before starting.
2. Start each bounded chunk from clean local `master` on a descriptive branch.
3. Implement and validate the changed contract; preserve unrelated user work.
4. Record actual results, limitations and decisions in the same branch.
5. Commit and merge with `git merge --no-ff` once ready, delete the merged branch,
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
