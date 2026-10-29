# Wasm allocator and memory policy

Issue #68, assessed 2026-10-29. **Proposal pending the owner's decision.**
The owner accepted the source comparison and host interruption study from
PR 120 on this date; issue #86 is complete. This document concerns memory
policy only. It changes no allocator or host configuration.

## Evidence

The experiment's host source is pinned to `4546214`, wasmtime 36.0.19 and
Rust 1.94.0 with the four `wasm/ci/locks/` snapshots. The same freshly built
Rust and JavaScript components are used in every variant. macOS uses Redis
8.10.2 with libc malloc on an Apple M3; Linux uses the checksum-pinned Redis
8.10.2 source built with jemalloc 5.3.0 in a Linux arm64 container. The compile
pool has eight threads. Runs and build hashes are recorded in the results
section below. These measurements do not establish throughput or latency.

Three disposable builds test distinct questions:

- **Baseline:** the committed `RedisAlloc` host and mmap-backed linear memory.
- **System:** only the module's global allocator changes to `std::alloc::System`.
- **Heap:** keeps `RedisAlloc`, but sets `memory_guard_size(0)`,
  `memory_reservation(0)`, `memory_reservation_for_growth(0)` and
  `memory_init_cow(false)`, selecting Wasmtime's `MallocMemory`.

All three keep explicit bounds checks and epoch interruption enabled. The
variant patches are experiments, not changes to the deployed host. Each
variant has a distinct binary hash; rebuild the host package when sharing a
Cargo target directory between archived source trees, because their older
source timestamps can otherwise reuse an artifact from another tree.

The probes confirm two different failure modes:

1. Baseline guest linear memory is outside Redis accounting. A guest grew to
   49.2 MiB with only 16 MiB of Redis headroom and a subsequent `SET` succeeded.
2. Counting memory without admission can evict other clients' keys. A large
   JavaScript load with concurrent `SET`s under `allkeys-lru` evicted all 16
   preloaded 1 MiB keys.
   The heap variant made guest growth itself do the same, and the following
   write still received `OOM`. Redis had no evictable keys left to recover
   the guest's allocated memory.

The System build avoids charging compiler scratch to Redis's key eviction
budget in this probe, but hides lasting Rust heap and saved component bytes
as well. Its process memory still grows substantially. It is not a complete
accounting or memory-limit policy.

RDB restoration is part of the same problem: background compilation starts
without a `WASM.LOAD` admission check. The startup probe sets `maxmemory` to
32 MiB and records writes while the saved JavaScript library recompiles.
Readiness for `PING` is not proof that compilation has completed or that
function calls have memory headroom.

The configuration-only heap variant also increased the JavaScript compiled
image from about 90 MiB to 95 MiB on both platforms. Its execution-time cost
was not measured. On Linux, purging after deletion reduced baseline RSS from
695.6 MiB to 78.8 MiB while counted heap stayed at 33.5 MiB. Purging Redis's
jemalloc did not reclaim the System build's 983.6 MiB RSS. These observations
support separating allocator retention from live module ownership.

## Recommended policy

1. **Keep lasting runtime heap accounted.** Retain `RedisAlloc` for module
   metadata, stores and saved component bytes. Do not switch the whole module
   to System merely to remove compiler peaks from `used_memory`.
2. **Give compilation its own bounded resource domain.** Compiler scratch
   must not use unrelated Redis keys as its reclaimable memory budget. A
   bounded compiler process is the preferred next design to evaluate: it can
   enforce a memory ceiling and release its context pool independently.
   Its artifact format, trust boundary and failure/retry behavior require a
   separate reviewed implementation. A thread-local allocator switch alone
   is insufficient: compile allocations can outlive the thread and be freed
   elsewhere, and some become lasting metadata. Until isolation is built,
   large loads and restoration remain managed maintenance operations with
   measured headroom; component size is not a reliable compiler-memory bound.
3. **Budget all memory outside Redis's allocator.** Keep the current mapped
   linear memory and per-instance `wasm.max-memory` while adding a separate
   aggregate budget for live guest memories and compiled images. Reserve
   admission before growth or retaining a library. Pair this with a process
   memory limit and a distinct compiler allowance; `maxmemory` alone is not a
   process memory cap. `INFO wasm` counters describe live resources, not RSS.
4. **Do not adopt the configuration-only heap variant.** It demonstrates
   accounting feasibility, but couples guest growth to key eviction. Its
   amortized capacity and temporary old-plus-new allocations during growth
   can exceed logical `memory.size`. `RedisAlloc` ultimately uses the native
   allocation path that aborts Redis on allocation failure; Wasmtime's
   `try_reserve` cannot turn that into a recoverable guest error.
5. **Gate any counted-memory implementation.** A custom `MemoryCreator` over
   `TryAlloc` is a candidate after admission/budget rules are defined. It must
   satisfy alignment, base stability within reported capacity, zeroing,
   overflow and failure-atomic growth requirements. It must prevent guest
   growth from evicting unrelated keys, count allocator capacity/rounding,
   work at the 7.2 floor without `TryRealloc`, and fail an allocation cleanly.
   Compiled code still needs its own budget: this hook does not allocate code.
6. **Preserve retained guest state.** A large but valid instance remains live
   until the existing failure, replacement or deletion paths release it.
   Reject new work at budget boundaries rather than silently recycling an
   instance and resetting its guest globals. Any opt-in stateless recycling
   mode would be a separate semantic change.
7. **Do not select pooling as an accounting fix.** Wasmtime's pooling
   allocator can bound its mapped slab but does not use the custom memory
   creator or Redis allocation. It also does not bound compilation or code.

These are policy and implementation gates. This PR ships no compiler helper,
new budget configuration, allocator switch, or unsafe memory creator.

## Retention and release

H8's retained instance contains the guest's linear-memory high-water mark.
Wasm cannot shrink that memory. `INFO wasm` reports the logical bytes and the
existing per-instance limiter bounds them. The growth probe exercises release
after a failed over-limit call; deletion and flush also release instances and
compiled images. Heap capacity can be larger than the reported logical bytes.

H16 must separate first use from repeated empty reloads. The probes measure
30 reloads with a guest and 30 without one, plus 20 load/delete cycles. Flat
counted heap and a falling or plateauing RSS are consistent with compiler
context pools, allocator retention and process-lifetime worker pools. They
do not prove the absence of leaks outside Redis accounting. `MEMORY PURGE`
and RSS are diagnostic observations; neither defines live module ownership.

## Source contracts and remaining validation

The Wasmtime pin is tag `v36.0.19`, commit
`0d6b8ead4e94b0ce76f925611b831b56570aef94`. The relevant contracts are
[`MemoryCreator` and `LinearMemory`](https://github.com/bytecodealliance/wasmtime/blob/0d6b8ead4e94b0ce76f925611b831b56570aef94/crates/wasmtime/src/runtime/memory.rs),
[`MallocMemory`](https://github.com/bytecodealliance/wasmtime/blob/0d6b8ead4e94b0ce76f925611b831b56570aef94/crates/wasmtime/src/runtime/vm/memory/malloc.rs)
and [`Config::build_allocator`](https://github.com/bytecodealliance/wasmtime/blob/0d6b8ead4e94b0ce76f925611b831b56570aef94/crates/wasmtime/src/config.rs).
The custom creator is used by the on-demand allocator. Guards and reservations
must match the actual allocation; Linux copy-on-write initialization cannot
be used with an unguarded raw allocation. Explicit bounds checks remain on.

Before adopting a custom creator or compiler isolation, require:

- Successful growth preserves data and zeroes new bytes; every injected
  allocation failure preserves the old allocation and leaves Redis serving.
- Page alignment, capacities, overflow, movement and destruction are tested
  under both libc and jemalloc, including the 7.2 fallback without TryRealloc.
- No unrelated keys are evicted by guest growth or a rejected compilation;
  no false `maxmemory` coverage is claimed for mapped code.
- Aggregate limits hold across multiple libraries, failed growth, traps,
  replacement, delete/flush, replication, RDB restoration and unload/reload.
- Paired `bench/run.sh` runs measure moving-base/growth overhead, startup
  copies and end-to-end compilation before making performance claims.
- Compiler budget exhaustion and malformed/failed helper output leave a
  recoverable library state, with bounded startup and explicit diagnostics.

This assessment does not implement an unsafe creator or inject physical
allocator failure. Those remain acceptance tests for a later implementation.

## Results and reproduction

The [published tables](bench/results/2026-10-29/memory-policy.md) record all
six complete variant/platform combinations, lifetime checkpoints, input and
binary hashes, and the excluded harness-timeout run. The existing
[memory script](bench/memory.sh) supplies the lifetime phases;
[policy.py](bench/policy.py) adds controlled eviction and startup probes.

Start in a fresh checkout or worktree of this PR. Copy the four snapshot
lockfiles as `CI.md` describes; do not replace a developer's own lockfiles.
Use Rust 1.94.0, Redis 8.10.2 and Node.js. The measured JavaScript build used
Node v26.12.1 and npm 11.20.0, with the committed npm lockfile.

Build the shared components and macOS variants:

```bash
task_results=/absolute/new-memory-results
mkdir -p "$task_results"
cargo build --locked --release -p redis-module-wasm --example componentize
cargo build --locked --release --target wasm32-unknown-unknown \
  --manifest-path wasm/guests/hello/Cargo.toml
target/release/examples/componentize \
  wasm/guests/hello/target/wasm32-unknown-unknown/release/hello_guest.wasm \
  "$task_results/hello.wasm"
npm ci --prefix wasm/guests/js --no-audit --no-fund
npm run --prefix wasm/guests/js build
cp wasm/guests/js/library.wasm "$task_results/javascript.wasm"
python3 wasm/bench/memory-variants.py --output "$task_results/macos-build" \
  --target-dir target
for variant in baseline system heap; do
  RAYON_NUM_THREADS=8 python3 wasm/bench/policy.py \
    "$task_results/macos-build/$variant.dylib" "$task_results/hello.wasm" \
    "$task_results/javascript.wasm" --output "$task_results/macos-$variant.json"
  RAYON_NUM_THREADS=8 CYCLES='5 10 20' RELOADS='1 5 10 20 30' \
    wasm/bench/memory.sh "$task_results/macos-build/$variant.dylib" \
    "$task_results/hello.wasm" "$task_results/javascript.wasm" \
    > "$task_results/macos-$variant-memory.md"
done
```

The [variant builder](bench/memory-variants.py) archives the pinned source,
applies only the declared patch, checks all four lockfiles, rebuilds the host
package to avoid stale shared-cache artifacts, and records hashes. It never
modifies the checkout's runtime sources. Its output directory must be new.

For Linux/jemalloc, the [research image](bench/memory-linux/Dockerfile) pins
the Rust base image and checks the Redis release archive against the same
checksum as `.github/servers.json`. It is separate from normal SDK builds.
The archive input works even when a managed worktree's `.git` link cannot be
resolved inside the container:

```bash
git archive 4546214080ff09cfb56eae062338020021a74e24 > "$task_results/source.tar"
docker build -t wasm-memory-research wasm/bench/memory-linux
docker run --rm --cpus=4 --memory=6g --memory-swap=6g --pids-limit=256 \
  -v "$PWD:/source:ro" -v "$task_results:/results" \
  wasm-memory-research bash -c '
    set -euo pipefail
    python3 /source/wasm/bench/memory-variants.py \
      --source-archive /results/source.tar \
      --archive-sha256 24bbdea368f378ec653c3581570fa86625ccad8aa7fb30864cfa32962fe362a5 \
      --output /results/linux-build --target-dir /target
    for variant in baseline system heap; do
      RAYON_NUM_THREADS=8 python3 /source/wasm/bench/policy.py \
        /results/linux-build/$variant.so /results/hello.wasm \
        /results/javascript.wasm --output /results/linux-$variant.json
      RAYON_NUM_THREADS=8 CYCLES="5 10 20" RELOADS="1 5 10 20 30" \
        bash /source/wasm/bench/memory.sh /results/linux-build/$variant.so \
        /results/hello.wasm /results/javascript.wasm \
        > /results/linux-$variant-memory.md
    done
  '
```

Run platforms/variants sequentially for measurements. The container's 6 GiB
limit is an experimental resource bound, not a tested production setting or
an injected allocation failure. Keep raw JSON, server/build logs and binaries
outside Git; publish the sanitized measurement tables and input hashes.
