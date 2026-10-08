# F01 archive consumer results

Recorded 2026-10-08 on macOS aarch64. The [machine-readable record](f01-package-consumers.json)
contains exact archive, manifest, lock, metadata, executable and log hashes.
All work remained local.

| Check | Result |
|---|---|
| Actual Cargo archives | Facade 2.0.8, public macros 2.0.8, internal macros 2.0.8, common 0.1.1 |
| Manual `redis_module!` consumer | Debug and release pass on Rust 1.94.0 and installed stable 1.98.1 |
| Public `#[command]` / derived-reply consumer | Debug and release pass on both compilers; runtime deferred |
| Build total | 8 successful builds; 26 successful bounded commands in the package run |
| Provenance / identity unit tests | 10 pass: 7 archive/index checks and 3 server-identity checks |
| Manual release runtime | Both compiler outputs pass on Redis 8.10.2/libc; the 1.94 output also passes on 7.2.8/libc |
| Runtime cases | Empty, ASCII, NUL/non-UTF-8/CRLF, and all 256 byte values round-trip exactly |

The runtime results above use the corrected identity-checking harness. Before
module commands, it verifies `INFO server` identifies the spawned PID and the
version reported by the exact Redis executable's `--version`. It verifies and
retains the `INFO memory` allocator, and hashes the server executable and loaded
module. The three `target/f01/packages/verified-runtime/` reports supersede the
earlier PING-only runtime entries. They reuse the original release binaries;
archives and build evidence were preserved.

## What the package check proves

`cargo package --locked --allow-dirty --no-verify` assembled four real archives.
A temporary sparse registry served those bytes on loopback. Fresh external
consumer manifests, Cargo homes and locks resolved the candidate packages from
that registry, while unrelated dependencies resolved from crates.io. The manual
consumer resolves the facade, common and internal macros; the proc consumer
additionally resolves the public macro archive.

There are no checkout dependency paths or source patches in these consumers.
Each candidate's metadata source/version, lock source/version/checksum, cached
archive digest and every extracted source-file digest must match the candidate.
A negative test confirms that same-version upstream registry metadata is rejected.
This matters because the inherited `2.0.8` / `0.1.1` labels alone cannot identify
the fork. Successful archive assembly is followed by actual consumer builds.

The public command attribute currently emits `linkme::distributed_slice` and
requires the author to declare `linkme = "0.3"` directly. An initial external
consumer build confirmed this missing-dependency error; the passing fixture
includes and documents that dependency. This remains an author-experience finding.

The run started at `febf8cb4dec487e0c72443b247bf1e51ea056d99` with F01 changes in
the working tree. That commit alone is not the tested package identity; use the
archive hashes in the JSON record. Raw archives, locks, metadata and logs remain
under ignored `target/f01/packages/`. The tracked record keeps their identities
without embedding all generated files.

## Reproduce locally

From the repository root, with Python 3.11+, the selected Rust toolchains,
Redis and libclang installed:

```sh
LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib \
  python3 tools/package_baseline.py \
  --output target/f01/packages-recheck \
  --toolchains 1.94.0 stable \
  --redis-server /opt/homebrew/bin/redis-server
python3 -m unittest discover -s tools -p test_package_baseline.py
rustfmt +1.94.0 --check \
  fixtures/package-consumers/manual/src/lib.rs \
  fixtures/package-consumers/proc/src/lib.rs
```

These executable/libclang paths match this macOS installation. Select another
fresh output directory for repeated runs; the tool refuses to overwrite evidence.
`stable` was 1.98.1 in this run and can change. For an exact compiler comparison,
select the installed 1.98.1 toolchain explicitly. A rerun records its own archive
and lock hashes; registry ports, the working tree and fresh dependency resolution
can change those hashes.

To exercise a different server, supply its executable with `--redis-server`.
The recorded 7.2.8 follow-up reused the existing 1.94 release library through
`runtime_manual()` rather than repeating builds. Runtime commands are ordinary
binary echo calls. Redis binds loopback, disables persistence for this fixture,
has a startup deadline and is terminated/reaped after the check. Cargo commands
have a 600-second default deadline.

## Limits

Both consumers select the Redis 7.2 API floor and `bindgen-runtime`, with facade
defaults disabled. The public proc-macro fixture was compiled but not loaded;
these checks do not establish its command-metadata runtime behavior. Manual echo
coverage does not establish persistence, replication, complete lifecycle handling
or FFI soundness. No Linux, jemalloc, other API-floor runtime, MSRV, CI-runner or
publication-readiness claim follows from this baseline.
