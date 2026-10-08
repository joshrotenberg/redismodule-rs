# M01 — Workspace and dependency foundations

2026-10-08. Local modernization of upstream baseline
`a808377ffde496d2cb8bcf26ee76a52080da9d89`, before the broader F01 baseline and
SDK repairs. Branch: `chore/workspace-modernization`. No pushes, PRs, registry
publication or GitHub setting changes are part of this chunk.

## Changes

- Keep the public facade, native inputs, examples and tests at the root. Group
  the three support packages under `crates/common`, `crates/macros` and
  `crates/macros-internals`. Their existing Rust source is byte-for-byte unchanged.
- Declare all four members explicitly and record resolver 2, which was already
  implied by edition 2021. Centralize dependency versions/paths and common package
  metadata, retaining member-specific features, attribution, package names and
  independent common version. Repository metadata now identifies this fork.
- Track the development lockfile. Retain semver dependency ranges for library
  consumers; fresh consumer resolution remains a separate gate.
- Update `strum_macros` 0.24.3 → 0.28.0, with exact four-level string tests.
  The resulting lockfile changes only that package, `heck` 0.4.1 → 0.5.0, and
  removal of `rustversion` 1.0.23. Total locked packages, including four workspace
  members, go from 101 to 100. This is graph simplification, not a speed claim.
- Add pure numeric-conversion characterization for all five `Primitive` enums.
  A trial update of `enum-primitive-derive` 0.1.2 → 0.3.0 failed compilation because
  its new `TryFrom<i32>` implementations overlap the existing public `From<i32>`
  conversions. Version 0.2.2 has the same issue; the earlier 0.2.0 is yanked.
  Retain 0.1.2 and defer its conversion policy instead of changing the API here.
- Make build/test scripts independent of caller directory, enable `bindgen-runtime`
  explicitly, use `--workspace --locked`, and forward arguments. Preserve their
  Redis 8.0 API-floor default, with `REDIS_MODULE_COMPATIBILITY` as an override.
- Make CI follow the pinned development toolchain and locked graph. Preserve its
  existing feature/runtime combinations, label them accurately, remove unused
  Python installation, and remove the obsolete Dependabot bindgen-0.56 exclusion.
- Remove abandoned Docker/readies recipes referencing missing setup/Makefiles
  and a nonexistent Git submodule. The Cargo workflow is the documented route.
- Give support packages short READMEs and copies of the existing BSD license;
  archive inspection had shown the license files were absent. Add local process,
  development and status documentation and distinguish this fork from published
  upstream versions in the README.

## Validation actually performed

Host: macOS arm64, Rust/Cargo 1.94.0, Command Line Tools libclang. Dependency
downloads/build cache were isolated under ignored `target/cargo-home`. A source
snapshot of the original baseline was used for before/after checks; the original
reference checkout and the four module working directories were left unchanged.

| Check | Result |
|---|---|
| Pure tests on original baseline | Six inherited value-conversion tests plus six new dependency-contract tests pass |
| Pure tests on updated workspace | Same 12 tests pass; other workspace library targets contain zero unit tests |
| Format / shell syntax / Git whitespace | Pass |
| Workspace all-target compile | Pass separately for 6.0, 6.2, 7.0, 7.2, 7.4 and 8.0 API-floor features with runtime bindgen |
| Actual example libraries | All 27 named debug and all 27 named release libraries built; not loaded into Redis |
| Script invocation from another directory | `build.sh` succeeds from `/private/tmp` |
| Clippy | Pass on all workspace targets at API floor 7.2; no warnings in that run |
| Rustdoc | Builds; 32 warnings match the baseline warning messages/counts exactly |
| API floor 8.0 warning | Existing unused `BlockingCallOptions.options` warning also occurs on the original baseline |
| Package inspection | All four archives created with `--no-verify`; README/license/native inputs and normalized internal package names/versions checked |
| Independent review | Workspace membership, default package selection, source-preserving moves and normalized package manifests checked |

The `call` example is inherited with a `required-features` gate naming 7.2
specifically, so the 8.0 script does not select it. It was separately built under
7.2 for debug; the release set used 7.2. This gate is not a Redis 8 incompatibility
finding, but belongs in F01's feature/runtime inventory.

The new dependency tests cover ordinary defined/unknown integer conversions and
the four exact log strings without invoking Redis, logging functions or the Redis
allocator. They are not proof of generated-code equivalence for every integer
width, overflow behavior or the entire FFI surface.

No runtime integration, static-libclang build, Linux matrix, downstream archive
consumer or MSRV certification ran in this pass. In particular, `--no-verify`
archive creation does not prove consumers resolve these unpublished packages.
The test harness still needs startup deadlines and target-directory handling
before broad runtime execution. Those checks remain F01 work.

The [evidence summary](evidence/m01-workspace.json) records commands, relevant
input hashes and results. Detailed logs remain in ignored `target/meta-pass`;
they are local evidence, not a durable release artifact service.

## Decisions and follow-up

| Area | Decision / next gate |
|---|---|
| Work visibility | Local branch/check/merge now; map to issues/PRs when remote work is requested |
| Public API and feature behavior | Preserve existing contracts in this meta pass, including default features and facade self dev-dependency |
| Edition, resolver and compiler floor | Keep edition 2021, explicit resolver 2 and developer pin 1.94; select actual MSRV separately |
| `enum-primitive-derive` | Upgrade requires an intentional conversion-contract decision or replacement implementation |
| `syn` / `serde_syn` | Dedicated macro-parser migration; updating `syn` alone leaves the older parser dependency |
| Redis dev client | Dedicated response/client API migration and ordinary integration checks |
| `nix` | Its allocator failure-path use needs a careful implementation/behavior review |
| bindgen | Compare generated declarations and validate actual builds/loads before upgrading |
| `lazy_static`, backtrace, feature defaults | Separate source/support/behavior choices rather than manifest-only cleanup |
| Packaging and support | F01 proves fresh candidate archive consumers, migration requirements and actual runtime support |
| Rustdoc and feature-gated dead code | Recorded inherited debt; no new suppression or broad lint-policy change |

Cargo references: [workspace inheritance](https://doc.rust-lang.org/cargo/reference/workspaces.html),
[lockfile guidance](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html).
Primary registry metadata for [enum-primitive-derive](https://index.crates.io/en/um/enum-primitive-derive)
and [strum_macros](https://index.crates.io/st/ru/strum_macros) informed the version
review; compilation and pure tests determined the adopted change.
