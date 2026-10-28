2026-10-27 (local; the UTC time is below), Darwin arm64, Apple M3, macOS 26.7.1, Redis server v=8.10.2 sha=00000000:1 malloc=libc
wasmtime 36.0.19 from wasm/ci/locks, Rust 1.94.0, release build
epoch interruption off: wasm/src/host.rs with config.epoch_interruption(false), not committed
bench component built by run.sh; built the same way separately it is 41265 bytes, sha256 8ac3af3e736a873c7e6822eed7fbc659c80c3f2525fee76d3e756fed3c24c8d3
PORT=6700 wasm/bench/run.sh, 50 clients; runs interleaved off-1, on-1, off-2, on-2, off-3, on-3

variant=off run=1 date=2026-10-28T00:22:39Z
dylib sha256 4f8069d62843cb5b7e2a91f60b36c883e091520f818ad136f872288df1e32c6b
load before: 12.50 17.76 10.46

| function | Lua µs/call | wasm µs/call | wasm / Lua | Lua calls/s | wasm calls/s |
|---|---|---|---|---|---|
| incr_twice | 1.92 | 3.60 | 1.88 | 143164 | 130124 |
| nested | 2.18 | 1.91 | 0.88 | 138408 | 138122 |
| adler32 | 770.67 | 65.34 | 0.08 | 1283 | 14368 |
| median | 298.69 | 198.84 | 0.67 | 3266 | 4846 |
| lrange | 70.61 | 165.46 | 2.34 | 13360 | 5867 |
| median_flat | 307.31 | 138.34 | 0.45 | 3169 | 7018 |
| lrange_flat | 69.42 | 91.57 | 1.32 | 13680 | 10384 |
| primes | 511.48 | 18.06 | 0.04 | 1933 | 44643 |

load after: 11.01 16.23 10.33
