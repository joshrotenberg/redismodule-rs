2026-10-27 (local; the UTC time is below), Darwin arm64, Apple M3, macOS 26.7.1, Redis server v=8.10.2 sha=00000000:1 malloc=libc
wasmtime 36.0.19 from wasm/ci/locks, Rust 1.94.0, release build
epoch interruption on: wasm/src/host.rs as committed
bench component built by run.sh; built the same way separately it is 41265 bytes, sha256 8ac3af3e736a873c7e6822eed7fbc659c80c3f2525fee76d3e756fed3c24c8d3
PORT=6700 wasm/bench/run.sh, 50 clients; runs interleaved off-1, on-1, off-2, on-2, off-3, on-3

variant=on run=1 date=2026-10-28T00:23:47Z
dylib sha256 efec9d1fbd501e9e9f2d1f2863842a804e27b62bcff98de05b556a3cb9b2313a
load before: 10.27 15.79 10.28

| function | Lua µs/call | wasm µs/call | wasm / Lua | Lua calls/s | wasm calls/s |
|---|---|---|---|---|---|
| incr_twice | 1.78 | 3.71 | 2.08 | 147384 | 117994 |
| nested | 2.29 | 1.83 | 0.80 | 123762 | 143988 |
| adler32 | 755.94 | 64.97 | 0.09 | 1308 | 14493 |
| median | 290.84 | 201.96 | 0.69 | 3347 | 4762 |
| lrange | 68.31 | 165.17 | 2.42 | 13889 | 5910 |
| median_flat | 307.82 | 146.08 | 0.47 | 3169 | 6653 |
| lrange_flat | 68.70 | 91.55 | 1.33 | 13831 | 10477 |
| primes | 485.67 | 39.69 | 0.08 | 2031 | 22727 |

load after: 6.02 13.68 9.84
