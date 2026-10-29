# Memory policy measurements, 2026-10-29

One complete controlled probe and one full lifetime run per variant per platform. The lifetime runs use 20 JavaScript load/delete cycles and 30 unload/reload cycles with a guest. Separate controlled cases do 30 empty reloads. These are memory observations on a shared developer machine; no throughput or latency conclusion is drawn.

Host source: `4546214080ff09cfb56eae062338020021a74e24`, Wasmtime 36.0.19, Rust 1.94.0, release builds. Both guests are freshly built once and shared byte-for-byte across all runs. The four snapshot lockfiles are used. Compilation uses `RAYON_NUM_THREADS=8`. macOS: Apple M3, 8 cores, 16 GB, macOS 26.7.1, Homebrew Redis 8.10.2 with libc malloc. Linux: arm64, Redis 8.10.2 with jemalloc 5.3.0, Docker limited to 4 CPUs, 6 GiB RAM and no additional swap.

Baseline is the committed allocator/configuration. System changes only the module allocator. Heap selects MallocMemory with four Config calls while retaining RedisAlloc. See [policy and reproduction](../../../MEMORY-POLICY.md).

## Admission and eviction

The growth case gives Redis 16 MiB headroom before growing to 49.2 MiB logical guest memory. Eviction cases preload 16 separate 1 MiB user keys. Compilation has 64 MiB headroom. Startup restores a saved JavaScript library with maxmemory 32 MiB. Write counts are sampled responses, not rates.

| platform | variant | Redis used after growth, MiB | growth SET | user keys after growth | user keys after compile | compile SET OK / OOM | startup SET OK / OOM |
|---|---|---|---|---|---|---|---|
| macos | baseline | 2.5 | OK | 16/16 | 0/16 | 2 / 24 | 0 / 25 |
| macos | system | 1.1 | OK | 16/16 | 16/16 | 24 / 0 | 24 / 0 |
| macos | heap | 51.4 | OOM | 0/16 | 0/16 | 1 / 26 | 0 / 25 |
| linux | baseline | 2.7 | OK | 16/16 | 0/16 | 5 / 53 | 0 / 56 |
| linux | system | 0.9 | OK | 16/16 | 16/16 | 57 / 0 | 56 / 0 |
| linux | heap | 58.2 | OOM | 0/16 | 0/16 | 4 / 57 | 0 / 61 |

## JavaScript load and release

The noeviction case has no preloaded user keys. Sampled maximum and Redis peak are distinct: the sampler can miss short peaks. Code is the compiled image, not only executable instructions. Purge is requested after deletion; it does not promise to release System-allocator memory. All sizes below are MiB.

| platform | variant | sampled max used | Redis peak | loaded used | logical linear | code image | deleted used | deleted RSS | post-purge used | post-purge RSS |
|---|---|---|---|---|---|---|---|---|---|---|
| macos | baseline | 691.2 | 780.0 | 43.0 | 7.3 | 90.2 | 31.1 | 729.5 | 31.2 | 729.5 |
| macos | system | 11.7 | 11.7 | 1.0 | 7.3 | 90.2 | 1.1 | 781.4 | 1.1 | 781.4 |
| macos | heap | 693.5 | 736.8 | 52.8 | 7.3 | 95.1 | 32.7 | 767.0 | 32.7 | 767.0 |
| linux | baseline | 818.0 | 833.2 | 46.4 | 7.3 | 90.1 | 33.5 | 695.6 | 33.5 | 78.8 |
| linux | system | 12.9 | 12.9 | 0.9 | 7.3 | 90.1 | 0.9 | 983.6 | 1.0 | 983.6 |
| linux | heap | 750.7 | 793.8 | 58.6 | 7.3 | 94.9 | 36.7 | 631.4 | 36.7 | 80.0 |

## Retention checkpoints

Used memory after deleting each library is not required to return to idle: compiler contexts are owned by the engine until unload. RSS includes allocator retention and mappings. A final unload removes the module INFO section; the binary remained mapped on both platforms. Flat counted heap cannot rule out leaks outside that accounting. All sizes are MiB.

| platform | variant | used after delete | used after 5 / 10 / 20 cycles | RSS after 20 cycles | final unload used / RSS | empty reload used before / after 30 | empty reload RSS before / after 30 |
|---|---|---|---|---|---|---|---|
| macos | baseline | 37.4 | 53.8 / 59.9 / 57.5 | 1102.1 | 1.0 / 42.6 | 1.0 / 1.0 | 8.2 / 8.7 |
| macos | system | 1.1 | 1.1 / 1.1 / 1.1 | 1088.0 | 1.0 / 43.3 | 1.0 / 1.0 | 8.2 / 8.8 |
| macos | heap | 39.0 | 62.6 / 57.3 / 57.1 | 1078.1 | 1.0 / 44.9 | 1.0 / 1.0 | 8.1 / 8.6 |
| linux | baseline | 34.8 | 55.7 / 56.0 / 56.4 | 624.2 | 0.9 / 76.9 | 0.9 / 0.9 | 15.4 / 15.8 |
| linux | system | 1.0 | 1.0 / 1.0 / 1.0 | 1421.6 | 0.9 / 62.6 | 0.9 / 0.9 | 15.3 / 15.7 |
| linux | heap | 32.5 | 56.1 / 60.1 / 56.8 | 591.8 | 0.9 / 82.7 | 0.9 / 0.9 | 15.4 / 15.7 |

The hello guest reached 57.2 MiB after the 16 MiB then 40 MiB growth sequence. The over-limit 80 MiB request failed and dropped its instance; the next call used a new instance. All variants returned to zero live instances and zero reported code after deleting the JavaScript library. These observations reconcile H8 as a retained high-water mark with explicit release, and H16 as bounded first-use/allocator retention in these runs, without a general leak-free claim.

## Inputs

Probe SHA-256: `26ff2d87a4fc62463c260c55fe7ef3a4a8302e6910dec57bf0aaa88d313764f8`.

Git archive SHA-256: `24bbdea368f378ec653c3581570fa86625ccad8aa7fb30864cfa32962fe362a5`.

Linux image: `sha256:062e38ee718bc92743457582ce90a19975a03773f30e3126ad82d5f26cad9b9e`.

| platform | variant | module SHA-256 |
|---|---|---|
| macos | baseline | `4b374ec23a85c4ab86090cfc6a3164160718109446e5c6ef9dbbdc5cc075b1bb` |
| macos | system | `4306abdca78ccf89e0fd60b6a383cfd38cae2775fe352743fdb93c9dfa577d3c` |
| macos | heap | `48a7f91e589264690957ee62efc513c6f2e109eb1dbb64518d02dbb425e4225b` |
| linux | baseline | `b36b975acb4f9aec48dca015f55c34bf51595e44ea403dce6f19796112fcdae2` |
| linux | system | `8221aefe42cdf202eac9171f99c3e0650bd625182f4c48c88d7f7f27e6e98eb1` |
| linux | heap | `1b3f1720ccc16a3c2ad880e1ba05a58c139148cbcb82e7d499c0a3dbd55baa85` |

Component SHA-256:

- `hello.wasm`: `9fc8e520cf34e6351f4b4a7c61121fd423445e2978354ce642001196ba69358a`
- `javascript.wasm`: `249071e58b125bfbe9cd1f26b6c49bb58d1f3f4f4cb0d3044fda212704896ce2`

An initial Linux run stopped at a five-second synchronous-load timeout in the harness. It was excluded. The complete runs use a 120-second bound for every compilation path and five seconds for ordinary commands. Missing or failed measurements are not recorded as zero. Raw JSON samples, logs and experiment binaries stay outside Git; these tables are the published measurement artifact.
