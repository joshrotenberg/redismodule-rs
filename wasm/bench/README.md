# Benchmark: wasm functions against Lua functions

`run.sh` loads the same functions as a wasm library (`guest/`) and as a Lua
library (`functions.lua`) into a throwaway `redis-server`, checks that both
give the same results, and times each with `redis-benchmark`:

```bash
wasm/bench/run.sh
```

It prints a markdown table. "µs/call" is the server's time per call from
`INFO commandstats` (`usec_per_call`), so the network is left out; calls per
second are what `redis-benchmark` saw with 50 clients (`CLIENTS` changes that,
`PORT` the server's port, 6700 by default).

| function | what it does |
|---|---|
| `incr_twice` | Two `INCR`s of a key. Mostly the cost of a call into Redis. |
| `nested` | Returns an array holding a map, a nil and an error. No calls; the cost of a call and its reply. |
| `adler32` | `GET`s a 16 KiB value and computes its Adler-32 checksum one byte at a time. |
| `median` | `LRANGE`s a list of 1000 integers, parses and sorts them, and returns the lower median. |
| `primes` | Counts the primes up to 10000 with a sieve. Pure computation. |

Each function is written the same way in both languages, with the same
algorithm and loops of the same shape, so the comparison is between the
engines rather than the algorithms. How the Rust is written matters: the sieve
written with `(i * i..=n).step_by(i)` ran seven times slower than with the
`while` loops it has now.

Needs `redis-server`, `redis-cli` and `redis-benchmark` on the `PATH`, and the
`wasm32-unknown-unknown` target. The guest is turned into a component with the
`componentize` example (`cargo run -p redis-module-wasm --example componentize`).
