2026-10-27 (local; the UTC time is below), Darwin arm64, Apple M3, macOS 26.7.1, Redis server v=8.10.2 sha=00000000:1 malloc=libc
wasmtime 36.0.19 from wasm/ci/locks, Rust 1.94.0, release build
epoch interruption on: wasm/src/host.rs as committed
bench component built by run.sh; built the same way separately it is 41265 bytes, sha256 8ac3af3e736a873c7e6822eed7fbc659c80c3f2525fee76d3e756fed3c24c8d3
PORT=6700 wasm/bench/run.sh, 50 clients; runs interleaved off-1, on-1, off-2, on-2, off-3, on-3

variant=on run=3 date=2026-10-28T00:28:02Z
dylib sha256 efec9d1fbd501e9e9f2d1f2863842a804e27b62bcff98de05b556a3cb9b2313a
load before: 3.70 8.79 8.53

| function | Lua µs/call | wasm µs/call | wasm / Lua | Lua calls/s | wasm calls/s |
|---|---|---|---|---|---|
| incr_twice | 1.78 | 3.54 | 1.99 | 146413 | 133690 |
| nested | 2.07 | 1.97 | 0.95 | 140746 | 131320 |
| adler32 | 769.36 | 64.74 | 0.08 | 1286 | 14609 |
| median | 295.49 | 201.41 | 0.68 | 3292 | 4772 |
| lrange | 68.79 | 166.06 | 2.41 | 13803 | 5884 |
| median_flat | 302.14 | 144.87 | 0.48 | 3226 | 6718 |
| lrange_flat | 71.07 | 91.58 | 1.29 | 13369 | 10449 |
| primes | 491.12 | 38.96 | 0.08 | 2005 | 23256 |

load after: 6.48 9.04 8.67
