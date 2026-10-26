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
describes, rather than replacing a developer's own lockfiles. If a `--locked`
step fails because the sync changed dependencies, regenerate the affected
lockfile with the pinned toolchain, review its diff, update its snapshot and
rerun `check.sh`. Record the sync in
`wasm/TRACKING.md`, push, and confirm the experiment push CI passes.

## Tracking upstream

The [upstream watch workflow](.github/workflows/upstream-watch.yml) runs
weekly. It compares upstream `master` with the review recorded in
`.github/upstream.json` and keeps the issue titled "Review upstream changes"
listing new upstream commits and open upstream pull requests updated since
that review. Its links to upstream pull requests go through
`redirect.github.com`, which adds no backlink to upstream; link upstream items
the same way, or name them as "upstream PR N", anywhere else in this
repository's issues and pull requests.

Reviewing it is ordinary backlog work: assess each item against the fork,
record relevant ones in CHANGES-FROM-UPSTREAM.md and file issues for work to
port. In the same pull request set `reviewed` to the upstream `master` commit
you reviewed and `reviewed_at` to the UTC time you checked the open pull
requests, as `YYYY-MM-DDTHH:MM:SSZ`; pull requests updated after that time are
listed again. The pull request's preview job should then print "Would close
issue N", and the merge closes it. To run the report now, use
`gh workflow run upstream-watch.yml`; to preview it locally, run
`python3 .github/scripts/upstream-watch.py --repo joshrotenberg/redismodule-rs --dry-run`.

## Pre-releases

3.0.0 reaches modules as pre-release tags on `master`; nothing is published to
crates.io.

- Tags are annotated and named `v3.0.0-alpha.N` while breaking changes still
  land, then `v3.0.0-beta.N` once none is planned. Upstream has no `v3` tags;
  its tags include `v2.x` and `redis-module-v2.x`.
- The crates keep version `3.0.0` until the final release, for two reasons:
  `module_version!` rejects pre-release versions, and the `hello`, `lifecycle`
  and `module_name` examples encode the SDK's own package version with it; and
  `redis-module`'s dependency on `redis-module-macros-internals` requires
  `version = "3.0.0"`, which a `3.0.0-alpha.N` package does not satisfy. The
  per-crate changelogs under `common/` and the two macro crates stay
  `unreleased` until then.
- Propose a pre-release when changes that git consumers should pin have landed.
  Preparing its pull request is ordinary work; creating and pushing the tag
  needs the owner's explicit authorization, as the branch rules above say. Get
  it before the pull request merges, because its snippets name a tag that does
  not exist until it is pushed.
- The pull request updates the tag in the dependency snippets of README.md,
  docs/MIGRATING-3.0.md and docs/COMPATIBILITY-FLOORS.md, and moves the
  CHANGELOG's Unreleased entries into a section named for the tag, with a
  compare link from the previous tag and the date the tag will be pushed. Before
  tagging, check that the section lists exactly the changes the tagged commit
  contains, and fix the date if the tag goes out on a later day.
- After it merges and every expected check passes on its squash commit, tag that
  commit on this repository's `master` and push the tag here (`fork` in the
  setup above). `ci.yml` does not run on tag pushes, which is why the checks on
  the squash commit are required:

  ```bash
  git fetch fork
  git tag -a v3.0.0-alpha.N -m "redis-module v3.0.0-alpha.N" <squash commit>
  git push fork v3.0.0-alpha.N
  ```

- Then build a throwaway module that depends on the tag exactly as the README
  snippet shows, to check that it resolves.
- A pushed tag is never moved or deleted; a fix ships in the next pre-release.
- Tagging is manual; the upstream release workflows do not run in this
  repository.
- The final release is the owner's decision, as is any crates.io publishing. It
  tags `v3.0.0` the same way, points the snippets at it, gives the CHANGELOG a
  `3.0.0` section, and dates the per-crate changelogs.

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
  floor and MSRV matrix. The repository's Actions allowlist permits only
  GitHub-owned actions, `actions-rust-lang/setup-rust-toolchain` and
  `swatinem/rust-cache`.
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
