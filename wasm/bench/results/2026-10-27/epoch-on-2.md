2026-10-27 (local; the UTC time is below), Darwin arm64, Apple M3, macOS 26.7.1, Redis server v=8.10.2 sha=00000000:1 malloc=libc
wasmtime 36.0.19 from wasm/ci/locks, Rust 1.94.0, release build
epoch interruption on: wasm/src/host.rs as committed
bench component built by run.sh; built the same way separately it is 41265 bytes, sha256 8ac3af3e736a873c7e6822eed7fbc659c80c3f2525fee76d3e756fed3c24c8d3
PORT=6700 wasm/bench/run.sh, 50 clients; runs interleaved off-1, on-1, off-2, on-2, off-3, on-3

variant=on run=2 date=2026-10-28T00:25:55Z
dylib sha256 efec9d1fbd501e9e9f2d1f2863842a804e27b62bcff98de05b556a3cb9b2313a
load before: 3.69 11.44 9.30

| function | Lua µs/call | wasm µs/call | wasm / Lua | Lua calls/s | wasm calls/s |
|---|---|---|---|---|---|
| incr_twice | 1.92 | 3.56 | 1.85 | 138600 | 134862 |
| nested | 2.06 | 1.86 | 0.90 | 143575 | 143266 |
| adler32 | 762.31 | 64.89 | 0.09 | 1298 | 14535 |
| median | 296.08 | 206.61 | 0.70 | 3290 | 4658 |
| lrange | 68.28 | 169.71 | 2.49 | 13899 | 5672 |
| median_flat | 291.22 | 147.84 | 0.51 | 3352 | 6577 |
| lrange_flat | 69.15 | 92.01 | 1.33 | 13746 | 10438 |
| primes | 494.71 | 41.77 | 0.08 | 1992 | 21834 |

load after: 3.99 10.19 8.96
