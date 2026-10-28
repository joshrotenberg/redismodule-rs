2026-10-27 (local; the UTC time is below), Darwin arm64, Apple M3, macOS 26.7.1, Redis server v=8.10.2 sha=00000000:1 malloc=libc
wasmtime 36.0.19 from wasm/ci/locks, Rust 1.94.0, release build
epoch interruption off: wasm/src/host.rs with config.epoch_interruption(false), not committed
bench component built by run.sh; built the same way separately it is 41265 bytes, sha256 8ac3af3e736a873c7e6822eed7fbc659c80c3f2525fee76d3e756fed3c24c8d3
PORT=6700 wasm/bench/run.sh, 50 clients; runs interleaved off-1, on-1, off-2, on-2, off-3, on-3

variant=off run=2 date=2026-10-28T00:24:51Z
dylib sha256 4f8069d62843cb5b7e2a91f60b36c883e091520f818ad136f872288df1e32c6b
load before: 6.33 13.62 9.84

| function | Lua µs/call | wasm µs/call | wasm / Lua | Lua calls/s | wasm calls/s |
|---|---|---|---|---|---|
| incr_twice | 1.77 | 3.44 | 1.94 | 147601 | 129199 |
| nested | 2.08 | 1.89 | 0.91 | 142552 | 139082 |
| adler32 | 781.99 | 64.68 | 0.08 | 1265 | 14493 |
| median | 292.23 | 194.52 | 0.67 | 3332 | 4975 |
| lrange | 68.69 | 158.43 | 2.31 | 13812 | 6158 |
| median_flat | 289.47 | 140.02 | 0.48 | 3365 | 6940 |
| lrange_flat | 70.85 | 90.07 | 1.27 | 13378 | 10650 |
| primes | 509.73 | 17.08 | 0.03 | 1937 | 46296 |

load after: 3.92 11.62 9.35
