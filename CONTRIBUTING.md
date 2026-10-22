# Contributing

This fork develops version 3.0 of `redis-module`. It keeps the upstream
foundation and makes correctness, safety and ergonomic changes, including
breaking ones; upstream acceptance is not a prerequisite. Version 3.0 is not
published, so modules use the Git dependency shown in the [README](README.md).

The [issue backlog](https://github.com/joshrotenberg/redismodule-rs/issues) is
the work list. `priority:1` is the core SDK, `priority:2` examples and author
documentation, `priority:3` the wasm experiment. An issue labelled
`needs-design` needs a decision before implementation, and closed design issues
record the decisions taken. Git history, pull requests and
[CHANGES-FROM-UPSTREAM.md](CHANGES-FROM-UPSTREAM.md) record completed work.

## Branches and remotes

- `master` is the SDK. `experiment/wasm-host` is `master` plus the `wasm/` host
  experiment, documented in its
  [README](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/README.md)
  and [tracking notes](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/TRACKING.md).
  Wasm code does not go on `master`. Other branches are merged PR branches.
- Push only to this repository. Upstream,
  [RedisLabsModules/redismodule-rs](https://github.com/RedisLabsModules/redismodule-rs),
  is read-only for this work: no pushes, pull requests, issues or comments.
- Publishing to crates.io, tags and releases need the owner's explicit
  authorization.
- Before starting another runner on the backlog, confirm that the previous
  account's or agent's scheduled work is stopped; do not create duplicate
  automation.

A conventional setup names this repository `fork` and upstream `origin`:

```bash
git clone https://github.com/joshrotenberg/redismodule-rs
cd redismodule-rs
git remote rename origin fork
git remote add origin https://github.com/RedisLabsModules/redismodule-rs
```

## Making a change

1. Branch from the latest `fork/master`, or `fork/experiment/wasm-host` for wasm
   work, as `codex/<topic>`.
2. Push an empty `chore: start <topic> [skip ci]` commit and open a draft PR
   whose body has Plan, Changes, Verification and Out of scope sections.
3. Keep the change focused. Add tests to `tests/integration.rs` (helpers are in
   `tests/utils.rs`) and an example under `examples/` for new API. Keep existing
   syntax working alongside optional builders.
4. Every SDK behavior change updates CHANGELOG.md and CHANGES-FROM-UPSTREAM.md,
   and docs/MIGRATING-3.0.md when it breaks code. Check an upstream issue or PR
   with `gh api` before citing it.
5. Run the validation below and record the results in the PR body.
6. Have an independent reviewer review the actual PR diff. Fix findings, rerun
   the affected checks and have the result reviewed again until no blocking
   findings remain.
7. Squash-merge the exact head once review is clear and every CI check passed
   on it. The owner has authorized this for this workflow's SDK PRs to `master`
   and wasm PRs to `experiment/wasm-host`; it covers no other PRs, and
   unfinished or failed checks are never bypassed.

Commit subjects and PR titles are conventional commits. Messages, PR bodies and
docs are technical: what changed, why in code terms and which checks ran, with
no em dashes and no attribution trailers. Squash messages carry no PR numbers.
`#N` links to this repository's items, so write upstream ones as "upstream
issue N" or "upstream PR N".

## Syncing the wasm experiment

After each SDK merge, merge `master` into the experiment rather than rebasing,
which keeps its published history and any open PR's ancestry intact:

```bash
git fetch fork
git switch experiment/wasm-host
git merge --no-ff fork/master
```

Adapt `wasm/` only where the SDK change requires it, review the four pinned
lock snapshots in `wasm/ci/locks/`, and run `wasm/ci/check.sh` as its
[CI guide](https://github.com/joshrotenberg/redismodule-rs/blob/experiment/wasm-host/wasm/CI.md)
describes, without overwriting existing local lockfiles. Record the sync in
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
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --features all-features-but-xor
```

- Code that needs the Redis 7.2 API floor, such as blocking calls, builds and
  runs only with `--no-default-features --features
  min-redis-compatibility-version-7-2`; repeat the build and tests with it.
- Build the examples before filtered integration tests, which load modules from
  the build profile's directory.
- On `experiment/wasm-host`, alternate-floor checks add
  `--exclude redis-module-wasm`.
- Stable rustdoc does not check `compile_fail` error codes;
  `RUSTC_BOOTSTRAP=1 cargo test --doc` does.
- The [CI workflow](.github/workflows/ci.yml) is the authority for the server,
  floor and MSRV matrix.
- `examples/module-testing` and `examples/compatibility-floors` are separate
  workspaces; [docs/TESTING-MODULES.md](docs/TESTING-MODULES.md) and
  [docs/COMPATIBILITY-FLOORS.md](docs/COMPATIBILITY-FLOORS.md) describe their
  checks.

## Private notes

Working notes such as a `local-docs/` directory, account state and scheduled
automation stay outside Git. Add `/local-docs/` to `.git/info/exclude` if one is
copied into a checkout. None of it is needed to find the backlog or run the
validation.
