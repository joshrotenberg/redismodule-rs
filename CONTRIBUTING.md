# Contributing

This fork develops version 3.0 of `redis-module`. It keeps the upstream
foundation and makes correctness, safety and ergonomic changes, including
breaking ones; upstream acceptance is not a prerequisite. Version 3.0 is not
published, so modules use the Git dependency shown in the [README](README.md).

## The work list

The [issue backlog](https://github.com/joshrotenberg/redismodule-rs/issues) is
the work list. Git history, pull requests and
[CHANGES-FROM-UPSTREAM.md](CHANGES-FROM-UPSTREAM.md) record completed work.

- `area:sdk`, `area:examples` and `area:wasm` name the track. `priority:1` to
  `priority:3` give the triaged order, which can differ from the track. Within
  a priority, take SDK work, then examples and documentation, then wasm.
- An issue labelled `needs-design` needs a decision before implementation. Do
  not treat a design question as a settled API. Park undecided work and continue
  with independent items in priority order, so a pending decision does not stall
  the rest.
- Closed design issues record the decisions taken. Reopen one only for a
  concrete consumer; closing a design does not implement its proposed API.

## Branches and remotes

- `master` is the SDK. `experiment/wasm-host` is `master` plus the `wasm/` host
  experiment, documented in its
  [README](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/README.md)
  and [tracking notes](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/TRACKING.md).
  Wasm code does not go on `master`. Other branches belong to pull requests;
  merged ones are historical, not pending work.
- Push only to this repository. Upstream,
  [RedisLabsModules/redismodule-rs](https://github.com/RedisLabsModules/redismodule-rs),
  is read-only for this work: no pushes, pull requests, issues or comments.
- Publishing to crates.io, tags and releases need the owner's explicit
  authorization.
- Before starting another runner on the backlog, confirm that the previous
  account's or agent's scheduled work is stopped; do not create duplicate
  automation. Create or recreate scheduled automation only when the owner asks
  for it.

A conventional setup names this repository `fork` and upstream `origin`:

```bash
git clone https://github.com/joshrotenberg/redismodule-rs
cd redismodule-rs
git remote rename origin fork
git remote add origin https://github.com/RedisLabsModules/redismodule-rs
```

## Making a change

1. Fetch `fork`, read the current issue and open PR state, and inspect local
   changes; preserve unrelated files.
2. Branch from the latest `fork/master`, or `fork/experiment/wasm-host` for wasm
   work, as `codex/<topic>`.
3. Push an empty `chore: start <topic> [skip ci]` commit and open a draft PR
   whose body has Plan, Changes, Verification and Out of scope sections.
4. Keep the change focused. Add tests to `tests/integration.rs` (helpers are in
   `tests/utils.rs`) and an example under `examples/` for new API. Keep existing
   syntax working alongside optional builders.
5. Every SDK behavior change updates CHANGELOG.md and CHANGES-FROM-UPSTREAM.md,
   and docs/MIGRATING-3.0.md when it breaks code. Check an upstream issue or PR
   with `gh api` before citing it.
6. Run the validation below and record the results in the PR body.
7. Have independent native subagents review the actual PR diff. Due and its
   reporting/A/B workflow are not used, and reviewers and workers do not
   delegate further. Fix findings, rerun the affected checks and have the result
   reviewed again until no blocking findings remain.
8. Record the reviewed head and the passing CI run in the PR. Squash-merge that
   exact head once review is clear and every expected check passed on it: the
   jobs in the [CI workflow](.github/workflows/ci.yml), or the wasm experiment
   check. The owner has authorized this for this workflow's SDK PRs to `master`
   and wasm PRs to `experiment/wasm-host`; it covers no other PRs, and
   unfinished, missing or failed checks are never bypassed.

Commit subjects and PR titles are conventional commits. Messages, PR bodies and
docs are technical: what changed, why in code terms and which checks ran, with
no em dashes and no attribution lines or trailers. Squash messages carry no PR
numbers. `#N` links to this repository's items, so write upstream ones as
"upstream issue N" or "upstream PR N".

## Syncing the wasm experiment

After each SDK merge, merge `master` into the experiment rather than rebasing,
which keeps its published history and any open PR's ancestry intact:

```bash
git fetch fork
git switch experiment/wasm-host
git merge --no-ff fork/master
```

Adapt `wasm/` only where the SDK change requires it. Run `wasm/ci/check.sh`
with the four snapshot lockfiles from `wasm/ci/locks/` in place, in a fresh
checkout or worktree as the
[CI guide](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/CI.md)
describes, rather than replacing a developer's own lockfiles. When a sync
changes dependencies, regenerate the affected lockfile with the pinned
toolchain, review its diff and update its snapshot. Record the sync in
`wasm/TRACKING.md`, push, and confirm the experiment push CI passes.

## Validation

Use the toolchain in `rust-toolchain.toml` (1.94.0); the MSRV is 1.87.0. If
another `rustc` comes first on `PATH`, put `$HOME/.cargo/bin` first. The
integration tests start `redis-server` from `PATH`, or the binary named by
`TEST_REDIS_SERVER_BIN`.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --no-default-features --features min-redis-compatibility-version-7-2 -- -D warnings
cargo build --examples
RUST_TEST_THREADS=4 cargo test --workspace --no-fail-fast
cargo test --features system-allocator-fallback --test system_allocator_fallback
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --features all-features-but-xor
```

- `call_blocking`, `BlockingCallOptions` and the `call` and
  `keyspace_to_stream` examples compile only at the Redis 7.2 or later API
  floors. Repeat the example build and tests with `--no-default-features
  --features min-redis-compatibility-version-7-2` against Redis 7.2 or later.
- Build the examples before filtered integration tests, which load modules from
  the build profile's directory.
- On `experiment/wasm-host`, alternate-floor checks add
  `--exclude redis-module-wasm`.
- Stable rustdoc does not check `compile_fail` error codes;
  `RUSTC_BOOTSTRAP=1 cargo test --doc` does.
- The [CI workflow](.github/workflows/ci.yml) is the authority for the server,
  floor and MSRV matrix. Its Actions allowlist permits only GitHub-owned
  actions, `actions-rust-lang/setup-rust-toolchain` and `swatinem/rust-cache`.
- `examples/module-testing` and `examples/compatibility-floors` are separate
  workspaces; [docs/TESTING-MODULES.md](docs/TESTING-MODULES.md) and
  [docs/COMPATIBILITY-FLOORS.md](docs/COMPATIBILITY-FLOORS.md) describe their
  checks.

## Private notes

Working notes such as a `local-docs/` directory, old helper scripts, raw logs,
archived refs, account state and scheduled automation stay outside Git. Add
`/local-docs/` to `.git/info/exclude` if one is copied into a checkout. They are
not a source of current rules and can hold obsolete policies; do not publish
them in commits, PRs or issues. None of it is needed to find the backlog or run
the validation.
