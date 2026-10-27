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
| `lrange` | The same `LRANGE`, returning only the count: the call and its reply, without the parsing and sorting. |
| `median_flat`, `lrange_flat` | `median` and `lrange` with the reply flat (`call-flat`): all the items in one buffer. In Lua they are the same functions again. |
| `primes` | Counts the primes up to 10000 with a sieve. Pure computation. |

Each function is written the same way in both languages, with the same
algorithm and loops of the same shape, so the comparison is between the
engines rather than the algorithms. How the Rust is written matters: the sieve
written with `(i * i..=n).step_by(i)` ran seven times slower than with the
`while` loops it has now.

Needs `redis-server`, `redis-cli` and `redis-benchmark` on the `PATH`, and the
`wasm32-unknown-unknown` target. The guest is turned into a component with the
`componentize` example (`cargo run -p redis-module-wasm --example componentize`).

## Memory

`memory.sh` measures memory rather than time: `INFO memory`, the process RSS
from `ps` and `INFO wasm` after each phase of loading, calling, growing,
deleting and reloading guests on throwaway servers (the comment at its top
lists the phases):

```bash
wasm/bench/memory.sh [module] [hello.wasm] [library.wasm]
```

Without arguments it builds the module and the Rust guest from this checkout
and looks for the JavaScript guest at `wasm/guests/js/library.wasm`; without
that file the JavaScript phases are skipped. `PORT`, `CYCLES` and `RELOADS`
change the first port tried and the checkpoints. The latest numbers are in
[TRACKING.md](../TRACKING.md) (H13).
