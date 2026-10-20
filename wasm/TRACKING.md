# redis-module-wasm: tracking

Work items for the wasm host, with what is done, what is next, and what is
outside this repository's control. Each item has an ID to cite in commits.

Status: `done`, `next` (being worked on or first in line), `todo`, `blocked`.

Last updated 2026-10-20.

## Where it stands

Milestone 1, the skeleton, works on Redis 8.10.2 with no server changes:

- `WASM.LOAD <library> <component> [REPLACE]`, `WASM.CALL <function> <numkeys>
  [key ...] [arg ...]`, `WASM.LIST`, `WASM.DELETE <library>`, `WASM.KILL`.
- A running function yields every 10 ms (wasmtime epochs and
  `RedisModule_Yield`): past `busy-reply-threshold` other clients get `-BUSY`,
  and `WASM.KILL` stops a function that has not written (`UNKILLABLE` if it has,
  `NOTBUSY` if none runs), as `FUNCTION KILL` does.
- Guests are components implementing `wit/redis.wit`. They import `call`
  (RedisModule_Call in script mode, as the caller's ACL user, errors as replies,
  effects replicated, RESP3) and `log`. No WASI imports.
- `WASM.LOAD` compiles on a thread behind a blocked client, so loading the
  11 MB JavaScript component no longer stops the server.
- Libraries persist and replicate: the RDB keeps them as aux data, so a
  restart, `DEBUG RELOAD` or a replica's full sync brings them back, and
  `WASM.LOAD` and `WASM.DELETE` go to replicas and the AOF as they are. Because
  aux data needs a data type, `MODULE UNLOAD` now refuses the module (H16).
- `WASM.CALL` declares its keys with a key spec like `FCALL`'s, so `COMMAND
  GETKEYS`, ACL key patterns and a cluster's `MOVED` and `CROSSSLOT` checks see
  them.
- Each instance's linear memory is limited by `wasm.max-memory` (64 MiB by
  default), and `INFO wasm` reports instances, their memory and compiled code.
- Functions have Lua's flags, with Lua's checks before they start and on the
  commands they call, and `WASM.CALL_RO` runs those with `no-writes`.
- Guests in Rust (`guests/hello`) and JavaScript (`guests/js`, componentize-js);
  the same integration tests pass against both.
- Reply types survive both ways, including simple versus bulk strings, maps,
  sets, nil, and errors inside arrays. A guest cannot run `WASM.*` commands.
- Each library keeps one instance between calls, so guest state persists
  across calls; a failed call, `REPLACE` or `DELETE` drops it.
- Against the same functions in Lua (see Numbers): a call into Redis costs
  about twice Lua's server time, a call with no Redis calls slightly less, and
  the checksum and the sieve run 9 and 16 times faster, and `median`, which
  moves 1000 strings into the guest, 1.4 times faster.
- The host is about 790 lines of code, not counting comments and unit tests.
  The example guest is 22 KB as a core module and 23 KB as a component. The
  fifteen integration tests take about 9 seconds.

Built on wasmtime 36 (the long-term support release; current wasmtime needs
Rust 1.95 or later and the workspace pins 1.94), `wit-component` 0.236 and
`wit-bindgen` 0.44.

### Numbers

`wasm/bench/run.sh` (see `wasm/bench/README.md`), two runs, Apple M4 Pro,
Redis 8.10.2, release build, 50 clients. Server time per call from
`INFO commandstats`:

| function | Lua µs/call | wasm µs/call | wasm / Lua |
|---|---|---|---|
| `incr_twice`: two INCRs | 1.28 to 1.37 | 2.45 to 2.47 | 1.8 to 1.9 |
| `nested`: a reply, no calls | 1.48 to 1.56 | 1.27 to 1.40 | 0.86 to 0.90 |
| `adler32`: checksum of a 16 KiB value | 569 to 574 | 61 | 0.11 |
| `median`: LRANGE 1000 integers, sort | 246 to 247 | 175 to 178 | 0.71 to 0.73 |
| `primes`: sieve up to 10000 | 423 to 429 | 26 to 27 | 0.06 |

The sieve's time grows linearly with n in both (wasm 4.3, 28 and 309 µs for n
of 1000, 10000 and 100000; Lua 34, 433 and 3863 µs).

#### Before H2

Server time per call from `INFO commandstats` (`usec_per_call`), with 50
`redis-benchmark` clients, release build, Redis 8.10.2 on a laptop; two runs
each, so ranges. Lua is `FCALL` of the same function.

| function | wasm, an instance per call (before H2) | wasm, kept instance (H2) | Lua |
|---|---|---|---|
| `incr_twice` (two INCRs) | 20.8 to 22.0 µs (35k to 38k/s) | 2.6 to 2.9 µs (85k to 126k/s) | 1.3 to 1.5 µs (99k to 157k/s) |
| `nested` (an array with a map, nil and an error; no calls) | 19.9 to 20.8 µs (38k/s) | 1.3 to 1.5 µs (103k to 155k/s) | 1.5 to 1.8 µs (92k to 153k/s) |

Where a call's time went before H2 (`incr_twice`, phase timers, 22 µs):
instantiating 7.9 µs, dropping the instance and store 7.8 µs, the guest 5.4 µs
of which 1.1 µs in the two RedisModule_Call calls, everything else under
0.5 µs. Variants measured back to back:

| variant | `incr_twice` | `nested` |
|---|---|---|
| an instance per call | 22.0 µs | 20.3 µs |
| an instance per call, pooling allocator, 64 KiB kept resident | 14.1 µs | 12.1 µs |
| a kept instance per library | 2.65 µs | 0.94 µs |

The pooling allocator's teardown is mostly resetting the guest's memory, which
starts at about 1.1 MiB (the Rust guest's default stack). The kept instance
avoids both instantiating and tearing down, and was adopted.

- `WASM.LOAD` of the 23 KB component: 23 ms, compilation included, on the main
  thread.
- `used_memory` about 1.3 MB while RSS was 16 to 18 MB: guest memory and
  compiled code are outside the Redis allocator.
- The release dylib is 11 MB.

## Host work

### First, before anyone else runs it

| ID | status | item | done when |
|---|---|---|---|
| H1 | done | **Time limit and kill.** wasmtime epoch interruption; at each epoch the host calls `Context::yield_now(YieldMode::Clients, ..)`, so other clients get `-BUSY` past `busy-reply-threshold`. `WASM.KILL`, flagged `allow-busy`, stops a running guest that has not written, as `FUNCTION KILL` does. | A test runs a guest that loops forever, sees `-BUSY` from another client, kills it, and the server keeps serving. |
| H2 | done | **Per-call cost.** Measure where the 26 µs goes, then reuse instances per library or use wasmtime's pooling allocator. | The benchmark table is updated, with the cause named. |
| H3 | done | **A fair benchmark.** Add a compute-heavy function next to the two-INCR one, in both wasm and Lua, and keep the script in the repository. | `wasm/bench/` reproduces both rows on one command. |
| H4 | done | **A guest in another language.** JavaScript (componentize-js), Python (componentize-py) or Go (TinyGo), whichever produces components wasmtime 36 loads. | The integration test loads and calls a non-Rust guest. |
| H5 | done | **Persistence and replication.** The RDB keeps each library's name, component and functions as aux data before the keyspace, under a data type no key uses; loading compiles each component again and runs no guest code. Loading an RDB or AOF clears the libraries first. `WASM.LOAD` and `WASM.DELETE` replicate verbatim; a replica's link blocks while a replicated load compiles. | Tests with a restart and with a replica see the same libraries. |

### Then

| ID | status | item | done when |
|---|---|---|---|
| H6 | done | **Key specs for `WASM.CALL`.** Declared with the `#[command]` macro and a key spec like `FCALL`'s: `numkeys` at argument 2, the keys after it, flags `RW`, `access` and `update`; `no-mandatory-keys`, arity -3. `COMMAND GETKEYS`, ACL key patterns and cluster routing (`MOVED`, `CROSSSLOT`) see the keys. | `COMMAND GETKEYS WASM.CALL f 2 a b x` returns `a b`. |
| H7 | done | **Compile off the main thread.** `WASM.LOAD` blocks the client and compiles on a thread; the main thread instantiates and adds the library. Precompiled components (`Engine::precompile_component`) would skip the compile entirely, but loading one is `unsafe`: only for trusted input. | The server answers other clients while a large component compiles. |
| H8 | done | **Memory.** `wasm.max-memory` (default 64 MiB) limits each instance's linear memory through a wasmtime `ResourceLimiter`; a guest that grows past it traps and its call fails, which drops the instance. A function does not start while Redis is over `maxmemory`, and its calls use `RedisModule_Call`'s `M` flag. `INFO wasm` reports libraries, functions, component and compiled code bytes, live instances and their linear memory, and the limit. Not done: replacing a kept instance that grew large but stays under the limit; a failed call already drops it. | Tests for the limit, the replacement, the refusal and the `INFO` fields. |
| H9 | next | **Script parity.** Done (H9a): function flags (`no-writes`, `allow-oom`, `allow-stale`, `no-cluster`, `allow-cross-slot-keys`) in the WIT, `WASM.CALL_RO`, Lua's checks before a function starts, read-only calls that refuse write and may-replicate commands, one slot per call on a cluster node, no calls while a library loads, flags in `WASM.LIST` and the RDB. Left (H9b): a `call` that fails the function where `pcall` returns the error, and `acl-check-command` and `set-resp` imports. | Each has a test matching what Lua does. |
| H10 | todo | **Guest helper library.** A small Rust crate that builds the pre-order node list from typed values, so guests do not build it by hand. | The example guest uses it. |
| H11 | todo | **Bounds check cost.** Signal-based traps are off so wasmtime installs no signal handlers over Redis's crash handler. Measured with H3's benchmark: turning them on made `primes` 18% faster (185 against 226 µs, with the old sieve loop), `median` 6%, `adler32` no different. Left: whether wasmtime's handlers and Redis's crash handler can coexist, and whether 18% is worth it. | A decision, with a crash report checked with handlers on. |
| H12 | todo | **Context in the store.** The store holds a raw pointer to the command's `Context`. Write down the safety argument or replace it with an SDK pattern (S3). | Reviewed and documented, or replaced. |
| H13 | todo | **Footprint.** Trim wasmtime features and measure the dylib and RSS. After loading the JavaScript component, RSS was 722 MB (serial compilation) and 959 MB (parallel) while `used_memory` stayed under 10 MB. `INFO wasm` (H8) accounts for 95 MB of compiled code and 7.7 MB of linear memory; find the rest, likely allocator memory kept after compiling. | Sizes recorded before and after. |
| H14 | todo | **CI.** A job that builds the guest, runs the host's tests and lints the guest. | Wherever the crate lives with CI. |
| H16 | todo | **Unloading.** Since H5 the module registers a data type for its aux data, and Redis refuses `MODULE UNLOAD` for a module with one ("the module exports one or more module-side data types, can't unload"). A module argument that leaves persistence off, and the type unregistered, would keep unloading for development. | `MODULE UNLOAD` works with the argument; a test covers both ways. |
| H17 | todo | **Load time.** Loading an RDB compiles each library's component on the main thread, one after another, before the server serves clients. With the JavaScript library in its RDB (5.4 MB), a release server took 2.6 to 3.9 s to start in three runs, against 0.07 s with an empty RDB. Compile them in parallel, or keep compiled code next to each component (`Component::serialize`; loading it is `unsafe`, and a replica's RDB comes from its primary). | Load time with the JavaScript library, before and after. |
| H15 | todo | **Large replies into the guest.** `median` is only 1.4 times faster than Lua although sorting 1000 integers is cheap: the 1000 strings of its `LRANGE` reply are copied into nodes, then into the guest's memory. Measure how much of its 177 µs that is, then copy less (for example, give the guest a handle to read the reply from). | `median`'s time split, and a lower number. |

## Repository

Found by an audit of fork master outside the crates' sources (2026-10-20).
These are ordinary fork PRs; none depends on the wasm host.

| ID | status | item |
|---|---|---|
| R1 | todo | Delete `Dockerfile`, `.dockerignore`, `build/docker/` and `.gitmodules`: they run the `readies` setup and `Makefile` that upstream deleted in October 2024, and cannot build. |
| R2 | todo | CI (`ci.yml`): drop `submodules: recursive` (no submodules) and the Python setup (no Python); run the matrix on the default features, so the tests use the committed bindings, and keep `bindgen-runtime`, clang and the bindings drift check in one job; use the pinned 1.94.0 toolchain or say why CI uses stable. |
| R3 | todo | Release workflows: `release-pr.yml` is still enabled and would open a release-plz PR that rewrites the 3.0.0 changelog sections and bumps versions. Delete `release.yml` and `release-pr.yml`, or limit them to the upstream repository. |
| R4 | todo | `dependabot.yml`: remove the ignore entry for bindgen 0.56; consider adding the `github-actions` ecosystem. |
| R5 | todo | `deny.toml`: nothing runs it since upstream's `Makefile` went. Add a cargo-deny CI job (its license list likely needs `Unicode-3.0` for `unicode-ident`) or delete it. |
| R6 | todo | `docs/MIGRATING-3.0.md`: the `system-allocator-fallback` dev-dependency example uses `version = "3"`, which does not resolve while 3.0 is unpublished; use the git and `rev` form. |
| R7 | todo | `README.upstream.md` keeps upstream's dead `redis.io/topics/modules-intro` links: leave it verbatim, or say at its top that it is. |
| R8 | todo | `build.sh` and `test.sh`: `--all` is the old spelling of `--workspace` (cosmetic). |

The audit found nothing to change in `examples/`, `tests/`, `.gitignore`,
`rust-toolchain.toml`, the changelogs and the side crates' manifests. The
`repository` field is S8.

## SDK changes the spike found

These are in this repository's SDK and do not depend on the wasm host; each
can be an ordinary PR.

| ID | status | item |
|---|---|---|
| S1 | done here, not on master | `RedisValue::Error(String)`, an error reply with an owned message that can sit inside an array (the `feat!: add RedisValue::Error ...` commit on this branch). Open as its own PR. |
| S2 | todo | `RedisValueKey` has no simple string, so a map key sent as a simple string goes out as a bulk string. |
| S3 | todo | No lifetime-safe way to hand a `Context` to code that needs `'static` data, such as a runtime's store. |
| S4 | todo | Replies can only be built as a `RedisValue` tree; the low-level reply functions are crate-private since 3.0, so nothing streams a reply. |
| S5 | todo | The `min-redis-compatibility-*` floors are mutually exclusive, and Cargo unifies features across a workspace, so a second module crate in the workspace must use the root's floor. |
| S6 | worked around | On macOS a stripped release build of the host fails to load ("mis-aligned LINKEDIT string pool"); the workspace sets `strip = "none"` for it. Check with a newer toolchain. |
| S7 | todo | `ServerInfo::field` panics on a field name with a NUL byte; upstream returns `None`. |
| S8 | todo | `Cargo.toml`'s `repository` field still names upstream. |
| S10 | todo | No safe wrappers for `RedisModule_GetCommandKeysWithFlags` and `RedisModule_ClusterKeySlot`; the host calls them through `raw`. `ContextFlags` is not `Copy`. |
| S9 | todo | `redis_module!`'s `configurations` block compiles only with all four groups (`i64`, `string`, `bool`, `enum`) and `module_args_as_configuration` listed, and module arguments are parsed as plain integers, so a memory config given as `8mb` fails the load. |

## Module API gaps

Outside this repository: each needs a change in Redis's module API. Record the
evidence as the host meets them.

| ID | gap | effect on the host |
|---|---|---|
| G1 | No module API registers an engine for `FUNCTION` or `EVAL` (checked in Redis 8.0.0 and unstable `module.c`). | The host needs its own `WASM.*` commands; `FUNCTION LIST`, `FCALL` and `SCRIPT KILL` do not see it. |
| G2 | `RedisModule_CreateCommand` works only while the module loads. | A function loaded later cannot be a command of its own; every call goes through `WASM.CALL`. |
| G3 | No module API adds memory allocated outside the Redis allocator to `used_memory`. | `maxmemory` and `INFO memory` do not count guest memory or compiled code (confirmed: 1.3 MB used, 16 to 18 MB RSS). `INFO wasm` reports them instead: 7.7 MB of linear memory and 95 MB of compiled code for the JavaScript library. |
| G4 | Aux data reaches an AOF only inside its RDB preamble: no module API adds commands to a rewritten AOF without one (`aof_rewrite` is per key). Redis writes its own `FUNCTION` libraries into the rewrite (`rewriteFunctions` in `aof.c`). | With `aof-use-rdb-preamble no` (the default is `yes`), an AOF rewrite drops the libraries: `WASM.LIST` was empty after `BGREWRITEAOF` and a restart. |
| G5 | No module API says whether writes are refused for a disk error (`MISCONF`) or for too few good replicas (`NOREPLICAS`). | Lua refuses a function that may write before it starts; here it starts, and its first write command fails with that error (`RedisModule_Call`'s script mode checks each write). |
| G6 | `COMMAND INFO` hides the `may_replicate` flag ("Hidden on purpose" in `server.c`), and no module API returns a command's flags. | Lua's read-only functions refuse may-replicate commands. The host names them (`PUBLISH`, `SPUBLISH`, `PFCOUNT`; script mode refuses `EVAL`, `EVALSHA` and `FCALL`), so a module command with that flag is not refused. |

## Open questions

| ID | question |
|---|---|
| Q1 | Where the crate lives so others can use it: a private repository, a bundle, or a branch. |

## Log

- 2026-10-20: H9a done: the WIT world has `function-flags` and `functions`
  returns each function's name and flags. `WASM.CALL` is now `may-replicate`
  rather than `write`, as `FCALL` is, and `WASM.CALL_RO` is `readonly`; both
  run Lua's `scriptPrepareForRun` checks (read from Redis's `script.c`) in its
  order. Calls run with `RedisModule_Call`'s `W` for a read-only call and `M`
  unless `allow-oom`; the host refuses may-replicate commands itself (G6), and
  on a cluster node keeps a call's keys in one slot through
  `RedisModule_GetCommandKeysWithFlags` (S10). The RDB format is version 1,
  with flags; version 0 still loads. The example guests declare flags; the
  Rust one gains `run_ro`, `set_oom`, `stale_ok`, `local_only`, `get_args` and
  `get_args_cross`. Tests: flags in `WASM.LIST` and after `DEBUG RELOAD`,
  `WASM.CALL_RO`, read-only refusals of `SET` and `PUBLISH`, the `OOM` refusal
  and `allow-oom`, a read-only replica, a stale replica with `allow-stale`,
  and on a cluster `no-cluster` and the one-slot rule with and without
  `allow-cross-slot-keys`. Not tested: refusing calls while a library loads,
  and loading a version 0 RDB. The test harness now builds the module itself:
  `cargo test` did not refresh its dylib, so a test could load a stale one.
- 2026-10-20: H8 done: a `ResourceLimiter` per store counts linear memory
  (wasmtime asks it when a memory is created and when it grows) and refuses
  growth past `wasm.max-memory`, an `i64` module config with the memory flag,
  so `CONFIG SET wasm.max-memory 4mb` works. Growth past it traps with "the
  instance would use N bytes of memory, more than wasm.max-memory (L)". The
  example guest has `grow`. Measured: the Rust guest's instance uses 1.2 MB of
  linear memory and 222 KB of compiled code; the JavaScript guest's 7.7 MB and
  95 MB. Test: `INFO wasm` before and after a load, a growth under and past a
  4 MB limit (the instance is dropped), the `OOM` refusal under `maxmemory`,
  and the counts after `WASM.DELETE`. Found S9.
- 2026-10-20: H6 done: `WASM.CALL` moved from the `redis_module!` table to
  the `#[command]` macro, which registers a keynum key spec through
  `RedisModule_SetCommandInfo`. Redis adds `movablekeys`; `COMMAND GETKEYS`
  with no keys returns an empty array, as for `FCALL`. Tests: `GETKEYS` and
  `GETKEYSANDFLAGS`, an ACL user limited to `~a*`, and a two-node cluster where
  a key on the other node gets `MOVED`, keys in two slots get `CROSSSLOT`, and
  a local key or no key runs. Both tests fail with the old registration, which
  declared no keys.
- 2026-10-20: H5 done: libraries are aux data in the RDB (data type
  `wasm-libs`, encoding version 0, saved before the keyspace). `aux_save2`
  saves nothing when there are no libraries, so such an RDB loads in a server
  without the module (tested). Loading clears the libraries when an RDB, AOF
  or replication load starts, then compiles each saved component and adds it
  without running guest code; its first call creates the instance.
  `WASM.LOAD` and `WASM.DELETE` replicate verbatim. A replicated load blocks
  the replica's link to its primary (`flags=Mb` in `CLIENT LIST`) while the
  replica keeps answering, so a delete right after a load arrives after it.
  Tests: `DEBUG RELOAD`, `SAVE` and a restart; an AOF restart before and after
  a rewrite; a replica whose own library the full sync replaces, then a load,
  a delete and a call on the primary. Found G4; added H16 and H17.
- 2026-10-20: H7 done: `WASM.LOAD` blocks its client (`block_client_on_reply`)
  and compiles on a thread; the library is instantiated and added in the reply
  callback, which checks the name again. Inside `MULTI` or a script it compiles
  inline. While the JavaScript component loaded (release build), the server
  answered 154 PINGs, slowest 14.7 ms, median 0.38 ms; before, one PING waited
  2.2 s. Tests: two overlapping loads of the same name (one succeeds, one is
  refused) with PINGs answered throughout, and a load inside `MULTI`.
- 2026-10-20: H4 done: `guests/js`, the example library in JavaScript, built
  with componentize-js 0.18.5 and jco 1.14.0 (pinned: jco 1.39 pulls a WASI shim
  componentize-js 0.18 cannot use). With stdio, random, clocks and HTTP
  disabled, the 11 MB component imports only the host's interface, so the host
  still has no WASI. The same functions and checks pass as for the Rust guest,
  including kill. Loading it compiled for 11.7 s on the main thread; parallel
  compilation, now on, makes that 1.4 s (H7 now next), and RSS reached 722 to
  959 MB (H13). Debug builds optimize the compiler crates (28 s to 3 s for the
  test). Repository audit added as R1 to R8.
- 2026-10-20: H3 done: `wasm/bench/` with a guest, the same functions in
  Lua, and `run.sh`, which checks both give the same results and prints server
  time per call. Computation is 9 to 16 times faster than Lua; calls into Redis
  cost about twice Lua's. The first sieve used `step_by` on an inclusive range
  and was seven times slower than with `while` loops, and its `i * i`
  overflowed `usize` on wasm32 past n of 65535; both fixed, results checked up
  to n of 1000000. A run with signal-based traps on gave H11 its first numbers.
  Added H15.
- 2026-10-20: H2 done: phase timers showed instantiating and dropping a
  fresh instance took about 16 of 22 µs per call. The pooling allocator cut a
  call to 14 µs; keeping one instance per library cut it to about 2.7 µs
  (`incr_twice`) and 1.3 µs (`nested`), against Lua's 1.3 to 1.5 µs. Adopted
  the kept instance: dropped after a failed call or on `REPLACE` and
  `DELETE`; a nested call of the same library (from another module's callback)
  gets its own instance; the store's context is set only during a call. Test:
  a counter in the guest carries over between calls, survives an error reply,
  and restarts after a kill and after `REPLACE`.
- 2026-10-20: H1 done: epoch interruption with a 10 ms ticker thread,
  `yield_now` at each epoch, `WASM.KILL` with `NOTBUSY` and `UNKILLABLE`, write
  tracking from `COMMAND INFO` (cached, subcommands included), killable
  `WASM.LOAD`, a ticker that stops on unload. Tests: a looping guest gets
  `-BUSY` for other clients and is killed; one that wrote is `UNKILLABLE` and
  `SHUTDOWN NOSAVE` stops the server; five runs in a row passed.
- 2026-10-20: milestone 1 done (the `RedisValue::Error` and `redis-module-wasm`
  commits on this branch); numbers above; tracking document added.
