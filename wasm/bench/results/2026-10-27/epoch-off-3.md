2026-10-27 (local; the UTC time is below), Darwin arm64, Apple M3, macOS 26.7.1, Redis server v=8.10.2 sha=00000000:1 malloc=libc
wasmtime 36.0.19 from wasm/ci/locks, Rust 1.94.0, release build
epoch interruption off: wasm/src/host.rs with config.epoch_interruption(false), not committed
bench component built by run.sh; built the same way separately it is 41265 bytes, sha256 8ac3af3e736a873c7e6822eed7fbc659c80c3f2525fee76d3e756fed3c24c8d3
PORT=6700 wasm/bench/run.sh, 50 clients; runs interleaved off-1, on-1, off-2, on-2, off-3, on-3

variant=off run=3 date=2026-10-28T00:26:59Z
dylib sha256 4f8069d62843cb5b7e2a91f60b36c883e091520f818ad136f872288df1e32c6b
load before: 3.93 9.97 8.90

| function | Lua µs/call | wasm µs/call | wasm / Lua | Lua calls/s | wasm calls/s |
|---|---|---|---|---|---|
| incr_twice | 1.79 | 3.59 | 2.01 | 147384 | 135226 |
| nested | 2.05 | 1.81 | 0.88 | 144928 | 144718 |
| adler32 | 760.54 | 64.15 | 0.08 | 1302 | 14749 |
| median | 292.28 | 195.27 | 0.67 | 3365 | 4986 |
| lrange | 68.68 | 160.66 | 2.34 | 13860 | 6081 |
| median_flat | 290.48 | 140.07 | 0.48 | 3347 | 6942 |
| lrange_flat | 70.06 | 91.18 | 1.30 | 13550 | 10515 |
| primes | 503.40 | 18.02 | 0.04 | 1957 | 44643 |

load after: 3.91 9.00 8.60
