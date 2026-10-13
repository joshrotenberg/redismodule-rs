# F03 — Public macro dependency hygiene

F03 removes the direct `linkme` requirement from module-author manifests.
The facade already depends on it; generated registration attributes now use a
doc-hidden facade re-export and linkme's native custom-crate hint. A shared
emitter keeps the attribute order consistent for all 11 registrations:
commands, six server events, INFO and three defragmentation phases. Ten repeated
handler parsers now use one helper; command metadata and callback signatures are
unchanged.

## Author-visible before / after

Before, a consumer using public registration macros needed:
```toml
[dependencies]
redis-module = { ... }
redis-module-macros = { ... }
linkme = "0.3"
```

After:
```toml
[dependencies]
redis-module = { ... }
redis-module-macros = { ... }
```

Those ellipses illustrate the dependency count, not runnable TOML. The actual
archive-consumer manifests and fresh locks are retained under
`target/f03/packages/consumers/`; they contain exact SDK versions and the
temporary candidate registry. They contain no direct linkme, path, Git or patch
dependency. Conventional SDK crate names are still required.

The underlying linkme dependency remains in the resolved graph. This is an
author dependency/boilerplate improvement, not a runtime allocation or speed
claim. The existing `linkme = "0.3"` constraint and Cargo.lock are unchanged:
source inspection of the primary linkme-impl 0.3.0 archive confirms the native
custom-crate hint existed at that declared lower bound. This is source/API
evidence, not a minimum-version build or Rust support commitment.

## Validation

The unmodified baseline produced exactly 11 E0433 missing-linkme errors in a
clean consumer. After the repair that consumer compiles; its 11 exact-one list
assertions pass. A control with direct linkme also passes.

Fresh package validation produces four actual Cargo archives and proves each
candidate package's checksum, source registry and extracted file bytes.
Three external consumers build in debug/release with Rust 1.94.0 and 1.98.1:
12 builds total. The registration consumer runs its one test with 11 exact-one
assertions in both profiles/compiler cells: four passing executions.
The proc consumer separately builds a real `redis_module!` entrypoint and
`RedisValue` derive into a cdylib, without a direct linkme dependency.

The linked-list assertions use Rust's ordinary test allocator. They do not
invoke callbacks or initialize Redis. Their executable is a Rust test harness,
not the module cdylib. No module was loaded into Redis in this chunk.
Meaningful proc-command host behavior remains F04.

The pinned locked workspace passes 26 Rust library/dependency/harness tests and
15 Python tests. All 28 examples build in debug/release at API floor 7.2 with
runtime bindgen. Formatting, workspace/all-target Clippy and whitespace checks
pass. Evidence is native macOS arm64; no new CI/platform/host support claim.

Reproduce:
```sh
LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib \
  python3 -B tools/package_baseline.py --output target/f03/new-packages \
  --toolchains 1.94.0 stable --skip-runtime
```
The output directory must be fresh. See the consumer README and development
instructions for pure tests/feature selection. A future `stable` compiler can
differ from the recorded 1.98.1.

## Process and remaining scope

Josh restored caller filesystem access after two read-only scheduled activations.
F03 uses a local issue-independent branch; no push, draft PR or publication.
F02's existing remote draft and its CI blockers are separate.

Due/Claude independently inspected SDK source and approved the implementation.
There were no failed native activities or source-read denials. Its full JSON
receipt is retained; request, result, answer and trace contracts are independently
validated before the merge. The caller applied its documentation suggestions and
added a metadata gate that rejects unexpected direct consumer dependencies,
including a renamed linkme dependency; 15 Python tests and the fresh archive run
validate those follow-ups. These follow-ups were caller-verified rather than a
second model review. Previous limited plan-review evidence remains in its
temporary checkpoint and is not substituted for this implementation review.

Dependency aliases, facade re-exports of the macro crate, the legacy optional INFO
branch's hard-coded macro-crate path, F04 runtime fixtures and CI modernization
remain separate. No MSRV, package identity, release or support policy was chosen.

The first package run passed; a follow-up with the direct-dependency metadata
gate hit OS error 28 (disk full). Only F03 compilation caches were removed;
archives, locks, metadata, logs and 13 selected binaries were retained. The
fresh recovered run passed all 12 builds and four registration tests. Reports
remain under target/f03/packages, packages-final and packages-recovered.
