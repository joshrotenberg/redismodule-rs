# Development

This is a local development fork. Existing package names and versions identify
the inherited package graph; they do not describe a newly published fork release.
The automatic publisher is disabled. Work proceeds on local branches, with
validation and merge commits before any later remote handoff.

## Workspace

| Path | Package / purpose |
|---|---|
| `Cargo.toml`, `src/` | Public `redis-module` facade and native build inputs |
| `crates/common/` | Shared metadata types (`redis-module-common`) |
| `crates/macros/` | Public command attributes and reply derives |
| `crates/macros-internals/` | Internal API-compatibility macros |
| `examples/` | The 27 standalone module examples |
| `tests/` | Pure dependency contracts and inherited Redis integration tests |

Shared metadata and dependency versions live in the root manifest. Member-specific
features remain in member manifests. Package names, the `common` dependency alias,
versions and the root facade layout are preserved. `Cargo.lock` records the
contributor/CI graph. A library consumer resolves its own graph; the lockfile is
not a substitute for fresh external consumer/package checks.

## Prerequisites

- Rust from `rust-toolchain.toml` (currently 1.94.0), including rustfmt and Clippy.
  This is the development compiler, not a declared minimum supported Rust version.
- A C compiler and libclang for the vendored Redis header and bindgen. On macOS,
  Xcode Command Line Tools provide these. On Debian/Ubuntu, install a C toolchain
  and `libclang-dev`.
- Redis is needed for module loading and integration tests, but not pure library
  tests. Runtime support is a separate validation project.

If bindgen cannot locate libclang, set `LIBCLANG_PATH` to its containing directory.
For the Command Line Tools installation used in this pass:

```sh
export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
```

## Local checks

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo test --locked --workspace --lib --test dependency_contracts
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets
cargo doc --locked --workspace --no-deps
```

The default facade features select the Redis 6.0 API floor and runtime libclang
loading. For a particular API floor, disable defaults and select exactly one:

```sh
cargo check --locked --workspace --all-targets --no-default-features \
  --features min-redis-compatibility-version-7-2,bindgen-runtime
```

The existing API-floor feature names are 6.0, 6.2, 7.0, 7.2, 7.4 and 8.0, written
with hyphens as above. They control compilation; they are not runtime support
certificates. `bindgen-runtime` and `bindgen-static` are mutually exclusive. When
disabling defaults, select a backend explicitly. Static libclang configuration
has not been validated by this pass.

The root scripts work from any caller directory and pass extra arguments to
Cargo. They retain their inherited Redis 8.0 API-floor default:

```sh
./build.sh
REDIS_MODULE_COMPATIBILITY=7-2 ./build.sh --release
```

`./test.sh` preserves the inherited all-target test scope, including actual Redis
integration tests. Those tests assume a server on `PATH` and example libraries
under the normal `target/{debug,release}` layout. Their server-startup loop and
custom-target handling still need the F01 harness work. Use the pure test command
above for this meta pass; a completed compile is not a runtime test result.

The local CI draft selects the pinned development toolchain and locked graph.
Its existing four API-floor configurations all select a Redis 7.2 host; job
labels now say so. These edits have not been pushed or verified on GitHub runners.
The user deferred the broader CI redesign and validation as a separate workstream.
F01 can establish the local runtime/package baseline independently; turning those
checks into reliable CI and confirming runner behavior remain later work.

## Dependency and layout policy

Use one dependency declaration in `[workspace.dependencies]`, then opt members
into it. Updating a constraint and refreshing a lockfile are separate review
decisions. For a major or pre-1.0 minor update, identify the used API and add
relevant checks; do not bump every dependency merely to remove version lag.

The M01 pass updates `strum_macros` with pure before/after string checks. A trial
of `enum-primitive-derive` 0.3 exposed conflicting public conversion impls;
0.2 has the same conflict, so its current version remains with new characterization
tests. Macro parsing (`syn`/`serde_syn`), the Redis test client, bindgen and the
allocator's `nix` use require separate migration or behavior review. Edition 2024,
resolver 3, a declared MSRV and replacement of `lazy_static` also remain separate.

The obsolete Docker/readies recipes were removed because they referenced missing
setup scripts, Makefiles and a nonexistent submodule checkout. This repository
currently offers the Cargo workflow above. Container validation will get one
working, documented path in F01 rather than inheriting those broken recipes.

Cargo references: [workspace inheritance](https://doc.rust-lang.org/cargo/reference/workspaces.html)
and [manifest versus lockfile](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html).
