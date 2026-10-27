# Redis API floors in module workspaces

Select exactly one `min-redis-compatibility-version-*` feature for each SDK build.
The SDK defaults to 6.0. This is a compile-time API assumption, not a runtime
server-version probe or a guarantee that every operation in a module works on
that server. Native configuration, command metadata and other APIs can impose
additional requirements; load and test the actual module on its oldest server.

For example, `EventContext::add_post_notification_job` returns
`APIResult<Status>` with a 6.0 floor because the native API, added in Redis 7.2,
may be missing. At a 7.2 floor it returns `Status` and assumes the API is present.
Selecting 7.2 does not add that API to an older server. See the
[notification guide](KEYSPACE-NOTIFICATIONS.md) for checking availability during
initialization and for the native callback restrictions.

## Choose a floor at each module boundary

A workspace can share one SDK dependency without fixing the floor for every
member. In the workspace manifest, disable SDK defaults:

```toml
[workspace]
members = ["legacy", "modern"]
default-members = ["legacy"]
resolver = "3"

[workspace.dependencies]
redis-module = { git = "https://github.com/joshrotenberg/redismodule-rs", tag = "v3.0.0-alpha.1", default-features = false }
```

Both module manifests inherit that dependency and expose explicit floor choices:

```toml
[dependencies]
redis-module.workspace = true

[features]
default = ["redis-6"] # The modern module instead defaults to ["redis-7-2"].
redis-6 = ["redis-module/min-redis-compatibility-version-6-0"]
redis-7-2 = ["redis-module/min-redis-compatibility-version-7-2"]
```

All members share that one pin, a pre-release tag or `rev = "<commit>"`. The
[compile fixture](../examples/compatibility-floors/Cargo.toml) uses a local path
instead and requires no network SDK clone. Its two consumers are compile-only
libraries, not Redis-loadable modules; their shared source checks the concrete
return types above without calling a Redis API.

Build members with different floors in separate Cargo invocations:

```bash
cargo check -p floor-legacy
cargo check -p floor-modern
```

These commands use the fixture's package names; substitute the names of your
modules. Run them from the workspace root, or provide its `--manifest-path`.
Selecting both members at once combines their SDK features. It does not produce
independent SDK builds with different floors, and the SDK rejects the combination.
Resolver 3 does not isolate normal dependency features by workspace member.
See Cargo's [feature unification rules](https://doc.rust-lang.org/cargo/reference/resolver.html#features).

When both modules can support the same floor, select it for both in one build:

```bash
cargo check -p floor-legacy -p floor-modern --no-default-features \
  --features floor-legacy/redis-6,floor-modern/redis-6

cargo check -p floor-legacy -p floor-modern --no-default-features \
  --features floor-legacy/redis-7-2,floor-modern/redis-7-2
```

These explicit choices replace each selected module's default choice.
`default-members` only affects commands without package-selection options;
`--workspace` still selects all members. Do not use `--all-features` with this
SDK: it enables conflicting floors.

## Trace defaults and unexpected combinations

`--no-default-features` controls selected workspace packages. It does not undo
features enabled by dependency declarations. Every dependency edge to the SDK
must disable defaults when a module selects a non-default floor; a helper crate
that leaves SDK defaults enabled can reintroduce 6.0. A reusable helper can inherit
the workspace dependency without selecting a floor and let its consuming module
supply one, provided its code supports the selected API signatures. It still needs one floor if built on its own.
See Cargo's [dependency-default rules](https://doc.rust-lang.org/cargo/reference/features.html#the-default-feature).

The fixture's `floor-default-edge` package deliberately enables SDK defaults on a
separate dependency edge. Even with `--no-default-features`, selecting it alongside
`floor-modern` at 7.2 fails. Inspect feature provenance with:

```bash
cargo tree -p floor-modern -p floor-default-edge -e features -i redis-module
```

No floor, multiple floors and an accidental dependency default are rejected by
the SDK build script before C compilation, binding generation or API wrapper
expansion. Separate
invocations or an agreed common floor are the supported current recipes. This
guide does not decide whether a future SDK should change its feature design.

On `experiment/wasm-host`, the host selects the default SDK floor. Keep the host
out of an alternate-floor SDK validation build:

```bash
cargo clippy --workspace --exclude redis-module-wasm --all-targets \
  --no-default-features --features min-redis-compatibility-version-7-2 -- -D warnings
```

That command applies only on the experiment branch, where the host is a member.
The default-floor `--workspace` checks there include the host. New wasm work stays
on that branch.

## Reproduce the compile controls

From this repository's root, with its pinned Rust 1.94 toolchain and Python 3:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --manifest-path examples/compatibility-floors/Cargo.toml --all -- --check
python3 examples/compatibility-floors/check.py
```

The script runs eleven bounded checks: separate/default-member/default-edge builds, both common
floors, and five rejected combinations covering mixed members, whole-workspace
selection, a dependency default, all features and no floor. Rejections must carry
the SDK's floor-selection diagnostic; a network or unrelated compiler error does
not count as a passing negative control. Each command has a three-minute timeout
and retains its output under `examples/compatibility-floors/target/check-logs/`.
It uses no native handles or allocator fallback and starts no Redis server.

For an installed minimum-Rust toolchain, use `--toolchain 1.87.0`. CI runs these
controls separately from the SDK workspace on the pinned and minimum toolchains.
The fixture declares its own workspace and does not change normal SDK dependency
features, default members or runtime behavior.
