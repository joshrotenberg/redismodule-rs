# Handoff

How to pick up work on this fork: the `redis-module` SDK on `master`, and the
wasm host on the `experiment/wasm-host` branch. State as of 2026-10-20.

## What is where

| | where | state |
|---|---|---|
| SDK, 3.0.0 (unpublished) | `master` of [joshrotenberg/redismodule-rs](https://github.com/joshrotenberg/redismodule-rs) | `5bf61ff`, configuration validation merged after all PR checks passed |
| wasm host, crate `redis-module-wasm` | `wasm/` on branch `experiment/wasm-host` | `master` plus the wasm commits; no PR, kept as a branch |
| What the fork changes from upstream | [CHANGES-FROM-UPSTREAM.md](CHANGES-FROM-UPSTREAM.md) | current through `5bf61ff` |
| Changes by version, migration | [CHANGELOG.md](CHANGELOG.md), [docs/MIGRATING-3.0.md](docs/MIGRATING-3.0.md) | current |
| wasm host work list | [wasm/TRACKING.md](wasm/TRACKING.md) (this branch) | H, S, G, Q items and a dated log |
| wasm host usage | [wasm/README.md](wasm/README.md) (this branch) | current |

The fork branches from upstream master at `a808377`. Upstream has two commits
since: `cca5360` (the `ServerInfo::field` leak fix; the fork has its own fix) and
`4ab88e7` (upstream PR 489, a new `REDISMODULE_OPTIONS_REQUIRE_LOADED_KEYS_IN_RAM`
define in `redismodule.h`, not in the fork's header or committed bindings).
CHANGES-FROM-UPSTREAM.md's Base section has the details.

The branches of merged fork PRs are still on the fork; only `master` and
`experiment/wasm-host` are live.

## Setup

```bash
git clone https://github.com/joshrotenberg/redismodule-rs
cd redismodule-rs
git remote rename origin fork
git remote add origin https://github.com/RedisLabsModules/redismodule-rs
git fetch fork && git fetch origin
```

- Remotes: `fork` is this repository, where all pushes go. `origin` is
  upstream and is read-only for this work: no pushes, pull requests, issues or
  comments there, and no crates.io publishing.
- Toolchain: `rust-toolchain.toml` pins 1.94.0; the MSRV is 1.87. If another
  `rustc` (for example Homebrew's) comes first on the `PATH`, run cargo with
  `PATH=$HOME/.cargo/bin:$PATH` so rustup's pinned toolchain is used.
- Redis: the integration tests start `redis-server` from the `PATH` (run
  locally with 8.10.2). CI builds Redis 7.2, 7.4 and 8.0 from source. For other
  versions, the `redis:<version>` Docker images work; with colima on macOS only
  `$HOME` is mounted, so copy a built module into a container with `docker cp`.
- wasm branch only: `rustup target add wasm32-unknown-unknown`. The JavaScript
  guest needs `node` and `npm` (its test is skipped without `node`; the first
  run does `npm ci` in `wasm/guests/js`). `wasm-tools` is optional.
- Issues are disabled on the fork. SDK work is tracked in draft PRs; wasm work
  in `wasm/TRACKING.md`.
- The fork's Actions allow only GitHub-owned actions,
  `actions-rust-lang/setup-rust-toolchain` and `swatinem/rust-cache`. The
  release workflows run only in the upstream repository.

## Conventions

- Conventional-commit prefixes on the branch (`fix/<slug>`), the PR title and
  each commit subject.
- Commit messages, PR bodies and docs are technical: what changed, why in code
  terms, which gates ran. No em dashes, and no AI attribution lines or
  trailers.
- `#N` in a fork commit message or PR body links to fork PR N, so upstream
  items are written out as "upstream PR N" or "upstream issue N". Squash commit
  messages carry no PR numbers at all.
- Every SDK change updates CHANGELOG.md and CHANGES-FROM-UPSTREAM.md, and
  docs/MIGRATING-3.0.md when it breaks code. Check any upstream issue or PR
  number with `gh api` (type, title, state) before citing it.

## Making an SDK change

1. Branch off `fork/master`: `git switch -c <type>/<slug> fork/master`.
2. Push an empty `chore: start <topic> [skip ci]` commit and open a draft PR
   against `master` whose body has Plan, Changes, Verification and Out of
   scope sections.
3. Implement, with tests in `tests/integration.rs` (helpers in `tests/utils.rs`)
   and an example under `examples/` when the change adds API.
4. Run the gates on the pinned toolchain:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo clippy --workspace --all-targets --no-default-features --features min-redis-compatibility-version-7-2 -- -D warnings
   cargo test --workspace --no-fail-fast
   ```

   The integration tests load the example modules from `./target`, and
   `cargo test --test integration` alone does not rebuild them: run the full
   `cargo test --workspace`.
5. Append a Results table of the gates to the PR body, mark it ready, and have
   it reviewed.
6. Squash-merge once every check passes, with the PR title as the subject and
   the commit messages (minus the empty start commit) as the body.
7. Rebase `experiment/wasm-host` onto the new master (below).

## SDK backlog

From a survey of
[redis-developer/redis-event-stream-module](https://github.com/redis-developer/redis-event-stream-module)
(commit `f0ebe1f`), a module built on this SDK, in the order to take them:

Module-argument configuration fixes merged in `5bf61ff`: numeric bounds are
enforced after memory-unit conversion, booleans accept case-insensitive
`yes`/`no`, and arguments are read as pairs.

1. ACL categories: the module reports that `RedisModule_AddACLCategory` fails on
   a second `MODULE LOAD` in the same process, which fails the load. Test a
   load, unload and reload of an example that declares `acl_categories`;
   distinguish an existing category from other registration errors.
2. Test harness: pass module arguments, assert a load failure with the server
   log, a `wait_until` helper, start a replica, restart on the same data.
3. `Context::select_db` and `get_selected_db`, with a guard that restores the
   database.
4. An accessor for the error code of a call's error reply, and
   `notify_keyspace_event` with a byte event name.

Then: subscribing to keyspace events after load, the server-event payloads for
flush and `SWAPDB`, a guide on writing to the keyspace from a notification, and
a wider CI matrix.

Also open:

- From the wasm host (wasm/TRACKING.md): S3, no lifetime-safe way to hand a
  `Context` to code that needs `'static` data; S4, replies can only be built as
  a `RedisValue` tree, so nothing streams one; S5, the
  `min-redis-compatibility-*` floors are mutually exclusive and Cargo unifies
  features across the workspace.
- Whether to take upstream PR 489's header define (regenerate the bindings).
- Upstream issue 351, splitting the context types: partly done (see
  CHANGES-FROM-UPSTREAM.md).
- `syn` 1 in both macro crates and the `redis` 0.23 dev-dependency are old.

## The wasm host

`wasm/` is a module that runs WebAssembly components as functions, with the
shape of Redis functions: `WASM.LOAD`, `WASM.CALL`, `WASM.CALL_RO`,
`WASM.LIST`, `WASM.DELETE`, `WASM.KILL`, `WASM.DUMP`, `WASM.RESTORE`,
`WASM.FLUSH` and `WASM.STATS`. Function flags and the checks before a function
starts follow the ones Lua functions have. Guests are in Rust
(`wasm/guests/hello`, on the `wasm/guest-sdk` helper crate) and JavaScript
(`wasm/guests/js`); the same integration tests run against both. Built on
wasmtime 36, the long-term support release whose MSRV fits the pinned 1.94
(later releases need 1.95).

State (wasm/TRACKING.md has each item and the log):

- Done: H1 to H12 and H15 to H19.
- Open: H13, the footprint: the compiler's temporary allocations count in
  `used_memory` with Redis's allocator (peak 1.05 GB loading the JavaScript
  library) and not with the system allocator. Which allocator to use is an
  open decision. H14, CI: nothing runs on this branch, since CI runs on master
  pushes and PRs only.
- Module API gaps G1 to G8 are recorded with what each costs the host.
- Run: the whole suite on macOS with Redis 8.10.2 and on Linux arm64 with
  8.0.2 (JavaScript tests skipped on that Linux run without Node); a smoke
  test of every feature and a two-node cluster on 7.2.16,
  7.4.11, 8.0.6, 8.2.10 and 8.10.2. Redis 7.0 does not load the module, since
  its commands declare `@scripting`. The one-slot rule has not been run on 7.2
  since it moved to `cluster::key_slot`.

Gates on this branch: the four above, except that the Redis 7.2 Clippy check
excludes the wasm crate. Its SDK dependency enables the default 6.0 floor;
combining it with another floor in the same build fails (S5). The full
workspace Clippy and test runs use the default floor. Then check the guests:

```bash
cargo clippy --workspace --exclude redis-module-wasm --all-targets --no-default-features --features min-redis-compatibility-version-7-2 -- -D warnings
for m in wasm/guest-sdk wasm/guests/hello wasm/bench/guest; do cargo fmt --manifest-path $m/Cargo.toml -- --check; done
cargo clippy --manifest-path wasm/guest-sdk/Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path wasm/guests/hello/Cargo.toml --target wasm32-unknown-unknown -- -D warnings
cargo clippy --manifest-path wasm/bench/guest/Cargo.toml --target wasm32-unknown-unknown -- -D warnings
cargo test --manifest-path wasm/guest-sdk/Cargo.toml
```

`cargo test -p redis-module-wasm` builds the module and the example guests
itself before starting servers. Plain `cargo clippy` and `cargo test` at the
root skip the wasm crate; use `--workspace`.

Keeping the branch current: after each merge to master,

```bash
git switch experiment/wasm-host
git fetch fork
git rebase fork/master
# adapt wasm/ to any SDK change, run the gates, update wasm/TRACKING.md
git push --force-with-lease fork experiment/wasm-host
```

Refer to the branch's commits by subject in notes, since a rebase changes
their hashes.

Linux runs: build in the `rust:1.94-trixie` image (run
`rustup target add wasm32-unknown-unknown` inside it, so the target matches the
pinned toolchain), then `docker cp` the module into `redis:<version>`
containers. The benchmark against the same functions in Lua is in
`wasm/bench/`.

## Untracked files

A `local-docs/` directory next to the checkout, if one is copied over, holds
working notes. It is not part of the repository and stays uncommitted; add it
to `.git/info/exclude`.
