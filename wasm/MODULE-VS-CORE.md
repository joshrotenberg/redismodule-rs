# Wasm in Redis: as a module or in core

This host runs WebAssembly functions in Redis as a module. The other way to
do it is an engine built into `redis-server`, as Lua is. This document sets
out what the module API let the host do, what it could not do, and what an
engine inside Redis gets from the server instead, so that the choice between
the two can be made from evidence.

## Sources

- **This host**: `experiment/wasm-host` and its code in [`src/`](src), its
  integration tests in [`tests/library.rs`](tests/library.rs) (named below by
  test function), [TRACKING.md](TRACKING.md) (items such as H11 and the
  module API gaps G1 to G8), [MEMORY-POLICY.md](MEMORY-POLICY.md) and the
  benchmark in [`bench/`](bench).
- **Redis**: the public source at tag
  [`8.10.2`](https://github.com/redis/redis/tree/8.10.2) (`498ecd0`), the
  release the tests and the [demo](demo/demo.sh) run against. Paths and line
  numbers below are at that tag. "Core" means the scripting framework Lua
  uses there: `src/functions.c` and `src/function_lua.c` for `FUNCTION` and
  `FCALL`, `src/eval.c` for `EVAL`, and `src/script.c` and `src/script_lua.c`,
  which both share.
- **Shutdown runs**: three runs on Redis 8.10.2 with a Lua script, made for
  this document (see [Long-running functions](#long-running-functions)).

No wasm engine inside Redis was built or run for this. Where the document
says what core gets, it means what Lua gets today through that framework; that
an in-tree wasm engine would get the same is an inference, marked as such.

## In short

The module API was enough to rebuild most of what Lua has: calls into Redis
in script mode as the caller's ACL user with their effects replicated, Lua's
five function flags and their checks, `-BUSY` and a kill command, declared
keys and cluster routing, libraries kept in the RDB and sent to replicas, and
`DUMP`, `RESTORE` and `FLUSH`. Each one is rebuilt under the host's own
`WASM.*` commands, and the [demo](demo/demo.sh) shows them working on a stock
Redis 8.10.2.

What a module cannot get is integration with what Redis already has for
scripts. `FCALL`, `FUNCTION`, `SCRIPT KILL` and the tools built on them do not
see wasm libraries; a function cannot be a command of its own; memory outside
the Redis allocator cannot be counted in `used_memory`; two checks Lua makes
before a script starts have no module API; and a rewritten AOF without its RDB
preamble loses the libraries. Core gets all of these from the framework.

Core pays with coupling: every Redis build would carry the runtime and its
toolchain, and the runtime's version would move with Redis releases. The
module loads into Redis 7.2 and later as it is, on its own release cycle, at
the cost of a second deployable binary and of being absent wherever it was
not loaded.

## What the module API made possible

### Calling into Redis

A guest's `call` and `pcall` run `RedisModule_Call` with the options that
`call_options` in [`src/host.rs`](src/host.rs) builds. The flags are
documented above `RM_Call` in `src/module.c` (lines 6751 to 6830):

| Flag | What Redis does with it | What the guest gets | Lua's counterpart |
|---|---|---|---|
| `S` | Script mode: refuses `deny-script` commands, and refuses writes with too few good replicas or when the server cannot persist | A guest cannot run `WASM.*`, which are all `deny-script` (`src/lib.rs`), or `SHUTDOWN` | The script layer's checks in `scriptCall` (`src/script.c` line 638) |
| `C` | Runs the command as the context's user, checking ACLs | Commands run as the caller; a denial fails the function | Scripts run as the caller |
| `E` | Returns errors as replies | `pcall` hands the error to the guest | `redis.pcall` |
| `!` | Propagates the command to replicas and the AOF | A function's writes replicate as their effects | Effects replication, `PROPAGATE_AOF \| PROPAGATE_REPL` (`src/script.c` line 296) |
| `W` | Refuses `write` commands | `no-writes` and `WASM.CALL_RO` | `no-writes` and `FCALL_RO` |
| `M` | Refuses `deny-oom` commands over `maxmemory` | A function without `allow-oom` | `allow-oom` |

The replies are RESP3 unless the guest asks for RESP2 with `set-resp`, so a
guest sees each reply's type. Tests: `call_pcall_acl_check_and_set_resp_work_as_in_lua`,
`function_flags_work_as_in_lua`, `a_replica_gets_the_libraries_and_their_changes`.

Two of Lua's checks have no module API:

- **G5.** Lua's `scriptPrepareForRun` (`src/script.c` line 192) refuses a
  script that may write before it starts when writes would fail for a disk
  error (`MISCONF`) or for too few replicas (`NOREPLICAS`), at lines 230 to
  255. No module API
  reports either state, so a wasm function that may write starts, and its
  first write fails in `S` mode with the same error.
- **G6.** A read-only Lua script also refuses commands flagged
  `may_replicate`. That flag is hidden from `COMMAND INFO` ("Hidden on
  purpose", `src/server.c` line 5369) and no module API returns a command's
  flags, so the host names the three it knows (`MAY_REPLICATE` in
  [`src/busy.rs`](src/busy.rs): `PUBLISH`, `SPUBLISH`, `PFCOUNT`). A module
  command with that flag is not refused.

### Commands, keys and ACL categories

`WASM.CALL` and `WASM.CALL_RO` declare their keys with a key spec like
`FCALL`'s, plus the `getkeys-api` that Redis before 8.4 needs to route a
module command in a cluster (H6, G8). `COMMAND GETKEYS`, ACL key patterns,
`MOVED` and `CROSSSLOT` all see the keys (`wasm_call_declares_its_keys`,
`a_cluster_routes_wasm_call_by_its_keys`). The commands are in the
`@scripting` ACL category through `RedisModule_SetCommandACLCategories`,
which needs Redis 7.2, so the host does not load on 7.0 (S11).

### The command's context

The SDK hands a command its `Context` only as a borrow for the length of the
callback, and wasmtime needs a store's data to be `'static`. The host keeps a
raw pointer to the context in the store, set by `invoke` and cleared by the
`Invocation` guard on every exit from a guest run: a return, an error, a trap
or a panic unwinding from an import. Between runs, the instance a library
keeps for its next call holds no context, so a later call by another user
cannot reach an earlier caller's context. A call that comes back into the
host while a guest runs, through another module's command that runs
`WASM.CALL`, runs on a different store. The argument is "The context pointer"
at the top of [`src/host.rs`](src/host.rs); the unit tests there check each
exit, and `each_call_runs_with_its_own_callers_context` has the kept instance
serve two users across a failure and a trap (H12, issue 189, PR 199).

Core has the same scope: an engine's `call` callback receives the run's
`scriptRunCtx` (the `engine` struct, `src/functions.h` line 37). What the
module adds is bridging the SDK's lifetimes, in Rust, to a runtime that
wants owned data, and a re-entry path through other modules' commands
(inferred).

### Persistence through aux data

The RDB keeps each library's name, component and function flags as module aux
data, saved before the keyspace ([`src/persist.rs`](src/persist.rs)). Loading
compiles the components again in the background and runs no guest code; with
the 11 MB JavaScript library saved, a server was ready in 0.11 to 0.12 s
instead of 1.56 to 1.67 s (H17). Tests: `libraries_survive_a_reload_and_a_restart`,
`libraries_survive_a_restart_with_the_aof`,
`a_restart_serves_while_saved_libraries_compile`.

Aux data costs three things that `FUNCTION` libraries do not pay:

- **Unloading.** Aux data belongs to a data type, so the host registers one
  that no key uses, and Redis refuses `MODULE UNLOAD` for a module that
  exports a data type (`src/module.c` line 13776). `wasm.persist no` trades
  persistence for unloading (H16, `the_module_unloads_when_persistence_is_off`).
- **AOF rewrites without the preamble (G4).** Aux data reaches an AOF only
  inside its RDB preamble. Redis writes its own `FUNCTION` libraries into a
  rewrite without one (`rewriteFunctions`, `src/aof.c` line 2837), and no
  module API adds commands outside keys, so with `aof-use-rdb-preamble no`
  a rewrite drops the wasm libraries. The default, `yes`, keeps them.
- **Servers without the module.** A server that reads an RDB with aux data
  from a module it has not loaded logs "no matching module" and exits
  (`src/rdb.c` lines 4873 to 4876). An RDB saved with no libraries has nothing
  from the module, and a server without it loads that one
  (`libraries_survive_a_reload_and_a_restart`).

### Replication

`WASM.LOAD`, `WASM.DELETE`, `WASM.FLUSH` and `WASM.RESTORE` replicate as they
are, and a function's writes replicate as their effects through `!`. A
replica compiles a replicated load on a thread while its link to the primary
waits, so it keeps serving reads (`a_replica_gets_the_libraries_and_their_changes`,
`a_replica_runs_functions_by_their_flags`).

Every replica needs the module and its compiler helper. A replica that
refuses a replicated load, for example over a memory budget, keeps applying
and acknowledging the rest of the stream, so `PING`, link status and `WAIT`
do not show whether the libraries match; compare `WASM.LIST` across nodes
(README, Memory; `admission_replica_refusal_retains_old_globals_and_fresh_full_sync_repairs`).

### Long-running functions

wasmtime's epoch interruption stops a running guest every 10 ms, and the
host calls `RedisModule_Yield` with `REDISMODULE_YIELD_FLAG_CLIENTS` and its
own busy reply ([`src/busy.rs`](src/busy.rs)). Past `busy-reply-threshold`,
Redis answers other clients with `-BUSY` and runs only commands flagged
`allow-busy` (`src/module.c` lines 1300 and 2460 to 2478). `WASM.KILL` and
`WASM.STATS` are `allow-busy`. `WASM.KILL` stops a function that has not
written, and replies `UNKILLABLE` to one that has, as `scriptKill` does
(`src/script.c` lines 361 to 389). Tests:
`a_busy_function_gets_busy_replies_and_can_be_killed`,
`a_function_that_wrote_cannot_be_killed`.

Lua reaches the same state from inside the interpreter: a count hook every
100,000 instructions (`src/script_lua.c` line 1670) calls `scriptInterrupt`
(`src/script.c` line 141), which processes events and honours `SCRIPT KILL`
and `FUNCTION KILL`. Those two commands act on the script layer's running
script, and reply `NOTBUSY` when there is none (`src/script.c` lines 362 to
364); a module command is not one, so they do not stop a wasm function. The
`-BUSY` reply names `WASM.KILL` instead.

The epoch checks have a cost. With the same build of `bench/run.sh` and
epoch interruption switched off, the sieve (`primes`) took 17.08 to 18.06 µs
a call against 38.96 to 41.77 µs with it, 2.27 times as long, and calls into
Redis and replies changed by 0 to 5% (TRACKING.md, Log, 2026-10-27; three
interleaved runs per build on an Apple M3, in `bench/results/2026-10-27/`).

One shutdown behavior is Redis's, shared with Lua. Without `NOW`, `SHUTDOWN`
waits for lagging replicas, up to `shutdown-timeout`, before it exits
(`prepareForShutdown`, `src/server.c` lines 5013 to 5026), and it checks that
deadline in `serverCron` (lines 1647 to 1650). `serverCron` does not run while
a script or a module command yields, and `whileBlockedCron` (line 1881) does
not check the deadline. The runs made for this document, on Redis 8.10.2 with
a replica stopped with `SIGSTOP` and an `EVAL` that wrote and then looped:

- `SHUTDOWN NOSAVE` had not returned after 25 s, and a second
  `SHUTDOWN NOSAVE NOW` sent after it did not stop the server either;
- `SHUTDOWN NOSAVE NOW` sent first stopped it at once;
- with the replica in sync, `SHUTDOWN NOSAVE` stopped it at once.

The demo's first run hung the same way on `SHUTDOWN NOSAVE` with its replica
attached, and now uses `SHUTDOWN NOSAVE NOW`. The way out of an `UNKILLABLE`
function on a primary with replicas is `SHUTDOWN NOSAVE NOW`, for Lua and
for the host alike.

### Compilation and memory

wasmtime compiles a component to native code with Cranelift before it runs.
Compiling the JavaScript component in the server process, with the Redis
allocator, put 650 to 800 MiB of transient heap in `used_memory`, enough for
other clients' writes to fail with `OOM` (TRACKING.md, Log, 2026-10-27; issue
68). The accepted policy keeps the runtime's lasting heap counted and moves
compilation out of the server's memory ([MEMORY-POLICY.md](MEMORY-POLICY.md)).

Since PR 192, `WASM.LOAD`, `WASM.RESTORE` and RDB loads compile in
`redis-wasm-compiler`, a helper process with its own allocator, an
address-space limit, a CPU limit and a wall timeout, one compile at a time
behind a bounded queue. Two things in it come from sharing a process with
Redis:

- Redis reaps any child process with `waitpid(-1)` in `checkChildrenDone`
  (`src/server.c` lines 1421 to 1425), so it could take the exit status of a
  helper the module started directly. A small launcher process owns the
  compiler and waits for it (`src/compiler_launcher.rs`).
- Redis forks for `BGSAVE` and AOF rewrites, and the children inherit open
  descriptors, so the end of a compile cannot be signalled by a pipe closing.
  The host sends an explicit cancel message and proves the processes exited
  before it releases their admission (README, Memory;
  `compiler_admission_survives_redis_background_forks`).

An in-tree engine that compiles would face the same peak; its child handling
is Redis's own code, which core can change (inferred). An interpreter, as Lua
is, has no compile step.

Linear memory and compiled code are mapped outside the Redis allocator, so
neither `used_memory` nor `maxmemory` counts them, and no module API adds
outside memory to `used_memory` (G3). The JavaScript library has 95 MB of
compiled code (`wasm_code_bytes` in the demo). The host limits each instance
with `wasm.max-memory` and, since PR 198, admits every live instance's
memory and every compiled image against `wasm.guest-memory-budget` and
`wasm.code-budget` (1 GiB each by default) before they grow or are mapped,
and reports both in `INFO wasm`.

Core reports an engine's memory through the engine's `get_used_memory`
callback (`src/functions.h`), summed in `src/functions.c` line 1090 and shown
as `used_memory_vm_functions` in `INFO memory` (`src/server.c` line 6559), and
could count linear memory in `used_memory` too. Counting is not enough on its
own: in issue 68's probes, a large load under `allkeys-lru` with concurrent
writes evicted all 16 preloaded 1 MiB keys, and a build that counted guest
memory let a guest's growth do the same (MEMORY-POLICY.md, Evidence). An
engine in core would need an admission budget like PR 198's as well
(inferred).

### Faults and signals

The host turns off wasmtime's signal-based traps, so guests get explicit
bounds checks. With them on, wasmtime's `SIGSEGV` and `SIGBUS` handlers chain
to Redis's, but `CONFIG SET crash-log-enabled` reinstalls Redis's handlers or
the defaults over them (`updateSighandlerEnabled`, `src/config.c` line 2778),
and on Linux the next out-of-bounds guest access then killed the server (H11,
G7). No module API installs handlers that Redis keeps. The explicit checks
cost 18% on the sieve as it was then written (H11). The demo's `oob` call
traps and the server serves on
(`an_out_of_bounds_access_traps_and_the_server_keeps_serving`). An engine in
core could coordinate its handlers with Redis's (inferred).

## What Lua gets from core that a module cannot

- **`FUNCTION` and `FCALL` (G1).** Engines register with
  `functionsRegisterEngine` (`src/functions.c` line 407), which only Lua
  calls (`src/function_lua.c` line 512) and which the module API does not
  expose. So `FCALL`, `FCALL_RO`, `FUNCTION LOAD`, `LIST`, `DUMP`, `RESTORE`,
  `FLUSH`, `STATS` and `KILL`, the RDB's functions section and
  `rewriteFunctions` cover Lua only. The host mirrors each of them as a
  `WASM.*` command (H9, H18, H19), and clients and tools must use those.
- **Commands per function (G2).** `RedisModule_CreateCommand` fails unless it
  is called while the module loads (`src/module.c` lines 1331 and 1332), so a
  library loaded later cannot add commands; every call goes through
  `WASM.CALL`.
- **The script cache.** `EVAL`, `EVALSHA` and `SCRIPT LOAD`, `EXISTS` and
  `FLUSH` cache scripts by their SHA1. Redis evicts the scripts `EVAL` added
  beyond 500 and keeps the ones `SCRIPT LOAD` added (`LRU_LIST_LENGTH` and
  `luaScriptsLRUAdd`, `src/eval.c` lines 534 to 545). A shebang naming any
  engine but `#!lua` is refused (`src/eval.c` lines 348 to 350), and no
  module API adds another. The host has named libraries only, as `FUNCTION`
  does.
- **`SCRIPT KILL` and `FUNCTION KILL`.** They do not reach a wasm function;
  see [Long-running functions](#long-running-functions).
- **Replication control.** Both replicate a script's effects. Lua can change
  where they go with `redis.set_repl` (`scriptSetRepl`, `src/script.c` line
  599); the host replicates every call's effects and offers no such import.
- **ACL integration.** Both run commands as the caller with per-command
  checks, and both have an ACL check for the guest (`redis.acl_check_cmd`,
  `acl-check-command`). `ACL LOG` records a denial inside a Lua script with
  context `lua` and one inside a wasm function with context `module`
  (`src/acl.c` lines 3181 and 3182, `src/script.c` line 411, `src/module.c`
  line 7095), so tooling that filters on the context sees them apart.
  Neither offers a per-function ACL rule; with commands per function an
  engine in core could, and G2 rules it out for the module.
- **Checks before a script starts (G5, G6)** and **counting outside memory
  (G3)**, above.
- **AOF rewrites without the preamble (G4)** and **fault handlers that
  survive `crash-log-enabled` changes (G7)**, above.

## Deployment

As a module:

- It loads into stock Redis with `--loadmodule` or `MODULE LOAD`, with no
  Redis rebuild. It has run on Redis 7.2, 7.4, 8.0, 8.2 and 8.10, and not on
  7.0 (TRACKING.md, Where it stands; S11). CI runs Ubuntu with Redis 8.10.2
  ([CI.md](CI.md)).
- A deployment is two files from one build: the module library and
  `redis-wasm-compiler` beside it, or at `wasm.compiler-path`. The helper's
  private protocol refuses a build or configuration mismatch.
- Every node that holds or serves libraries needs it: replicas, and every
  primary in a cluster, where a library is loaded on each primary as with
  `FUNCTION LOAD`.
- Its runtime version is its own: wasmtime 36, the long-term support release,
  built with Rust 1.94 (TRACKING.md). Upgrading it does not mean upgrading
  Redis, but with persistence on, `MODULE UNLOAD` is refused, so an upgrade
  is a restart.

In core:

- The engine is in every build and on every node, and needs no separate
  deployment, version matching or helper path.
- Its runtime and toolchain become Redis's: every build carries them, and
  their updates follow Redis releases (inferred).

## Safety boundaries

- **The same process.** In both, guest code runs on the server's main thread
  inside `redis-server`. The boundary is wasm's sandbox: no WASI imports (no
  files, network, clocks or randomness), bounds-checked linear memory, the
  per-instance limit and the aggregate budgets. A bug in the host or in
  wasmtime takes the server down, as a bug in the Lua engine would.
- **Native code with the server's rights.** The module and its helper are
  native code with the server's OS permissions, so they must come from the
  deployment, not from a library's author; `wasm.compiler-path` is immutable
  for that reason. Only compilation runs in a separate, limited process;
  instantiation and calls do not.
- **Signals.** G7, above: the host gives up guard-page bounds checks because
  it cannot keep its fault handlers.
- **Allocator.** The host's lasting heap uses the Redis allocator, which
  aborts the server on allocation failure, so wasmtime cannot turn such a
  failure into a guest error (MEMORY-POLICY.md, policy 4). Linear memory and
  code are separate mappings, bounded by the budgets.
- **Runaway guests.** A loop that has not written is stopped by `WASM.KILL`;
  one that has is `UNKILLABLE`, and `SHUTDOWN NOSAVE NOW` stops the server.
  That is Lua's model.

## Performance

From TRACKING.md, Numbers: `bench/run.sh`, two runs, Apple M4 Pro, Redis
8.10.2, a release build and 50 clients; server time per call from
`INFO commandstats`.

| Function | Lua µs per call | Wasm µs per call | Wasm / Lua |
|---|---|---|---|
| `incr_twice`: two `INCR`s | 1.28 to 1.37 | 2.45 to 2.47 | 1.8 to 1.9 |
| `nested`: a reply, no calls | 1.48 to 1.56 | 1.27 to 1.40 | 0.86 to 0.90 |
| `adler32`: checksum of a 16 KiB value | 569 to 574 | 61 | 0.11 |
| `median`: `LRANGE` 1000 integers and sort | 246 to 247 | 175 to 178 | 0.71 to 0.73 |
| `median_flat`: `median` with `call-flat` | 253 to 254 | 126 | 0.50 |
| `primes`: sieve up to 10000 | 423 to 429 | 26 to 27 | 0.06 |

What the numbers say about module against core:

- A call into Redis is the expensive part, and the module API is not the
  reason. In one server, two `INCR`s through `RedisModule_Call` from a native
  module command took 0.94 to 0.97 µs, Lua's `FCALL` 1.31 µs and the wasm
  function 2.64 to 2.67 µs. Each call from the guest into Redis cost about
  0.67 µs against about 0.45 µs for a native `RedisModule_Call`; the
  difference is the component ABI and the host's per-call bookkeeping
  (TRACKING.md, Numbers). An engine in core using the same runtime and ABI
  would pay most of it too (inferred).
- Compute runs 9 to 16 times faster than Lua (`adler32`, `primes`).
- Keeping one instance per library, instead of one per call, brought a call
  from 22 µs to 2.65 µs (H2).
- The epoch checks behind `-BUSY` and `WASM.KILL` cost 2.27 times on a tight
  loop and 0 to 5% elsewhere ([Long-running functions](#long-running-functions)).
  The explicit bounds checks cost 18% on the sieve (H11); that one is a
  module cost, since core could keep signal handlers.

The demo's JavaScript load is not a benchmark: through the debug build's
helper, on the Apple M3 that ran it, it took 16.8 s while the load average was
about 60 from other work, and the server answered 269 `PING`s meanwhile.

## Operability

- **Commands and stats.** `WASM.*` mirror `FUNCTION`; `WASM.STATS` mirrors
  `FUNCTION STATS` and answers while a function runs. `INFO wasm` has the
  libraries, instances, memory, budgets and compiler counters. Settings are
  module configurations under `wasm.*`, read and set with `CONFIG`; the
  persistence and compiler settings are immutable. `INFO commandstats`
  counts `wasm.call` as a whole, as it counts `fcall`.
- **What existing tools miss.** Anything built on `FUNCTION`, `FCALL`, `SCRIPT
  KILL` or `used_memory_vm_functions` does not see wasm libraries, and
  runbooks need `WASM.KILL` and `INFO wasm`.
- **Library parity.** Libraries are per node. Check them with `WASM.LIST` or
  `INFO wasm` on each node, since replication state alone does not show a
  refused load.
- **Logs.** The host logs through the server log; a guest's `log` import
  writes there too, tagged with the module's name.

## Summary

| Area | As a module (this host) | In core | Closed by |
|---|---|---|---|
| Calls into Redis | `RedisModule_Call` with `S`, `C`, `E`, `!`, `W`, `M`: Lua's behavior | The script layer, as for Lua | Done |
| Checks before start | `MISCONF` and `NOREPLICAS` surface at the first write; `may_replicate` named by hand | `scriptPrepareForRun` | Module API (G5, G6) |
| Function flags, `CALL_RO` | Lua's five flags and checks | Framework handles flags | Done |
| `FCALL` and `FUNCTION` | `WASM.*` mirrors | Native | Module API (G1) |
| Commands per function | Not possible | Possible | Module API (G2) |
| `EVAL` and the script cache | None | Native, `#!lua` only today | Module API (G1) |
| Busy and kill | `RedisModule_Yield`, `allow-busy`, `WASM.KILL` | `scriptInterrupt`, `SCRIPT KILL`, `FUNCTION KILL` | Done, under other names |
| Shutdown with lagging replicas while busy | Needs `SHUTDOWN NOSAVE NOW` | The same | Redis, if at all |
| Persistence | RDB aux data; unload refused unless `wasm.persist no` | The RDB's functions section | Done, with costs |
| AOF rewrite without preamble | Drops libraries | Keeps them | Module API (G4) |
| Replication | Verbatim loads, effects via `!`; parity not visible | `FUNCTION LOAD` replicated | Done |
| ACL | Caller's user; `ACL LOG` context `module` | Caller's user; context `lua` | Done |
| Memory accounting | Outside `used_memory`; budgets and `INFO wasm` | Engine memory in `INFO memory`; can count in tree | Module API (G3), plus admission either way |
| Compilation | Bounded helper process | Same peak; Redis's own child handling | Done |
| Faults | Explicit bounds checks, 18% on the sieve | Can coordinate handlers | Module API (G7) |
| Calls into Redis, cost | About 1.8 times Lua's server time | Same runtime and ABI would pay most of it (inferred) | Runtime and ABI choice |
| Deployment | Stock Redis 7.2+, two files per node, own release cycle | In every build, on Redis's release cycle | A choice |

## Open questions

1. **An engine API (G1).** Would Redis expose `functionsRegisterEngine`, or
   an equivalent, to modules? That one change moves `FCALL`, `FUNCTION`,
   `FUNCTION KILL`, the AOF rewrite (G4) and `used_memory_vm_functions` from
   core-only to available to a module, and most of the difference between
   the two options goes with it.
2. **Commands after load (G2).** Is a function that is a command of its own,
   with its own ACL rule and key spec, wanted? Only core or a new module API
   can provide it.
3. **Memory outside the allocator (G3).** Should guest memory and compiled
   code count in `used_memory`, given that counting without admission lets
   one load evict unrelated keys? What admission would core apply?
4. **Fault handlers (G7).** Is a module API that keeps a module's signal
   handlers across `crash-log-enabled` changes worth the 18% the explicit
   bounds checks cost on tight loops?
5. **The ABI.** Is the component model's cost per call into Redis, about
   0.67 µs against 0.45 µs native, acceptable, or would an engine in core
   choose a cheaper interface at the price of typed replies and the
   existing guest SDKs?
6. **Release coupling.** Would wasmtime's long-term support cadence and its
   Rust toolchain fit Redis's release and platform matrix if it shipped in
   core?
7. **Shutdown while busy.** Is it intended that `SHUTDOWN NOSAVE` during a
   busy script never reaches `shutdown-timeout` when a replica lags? It
   affects Lua as much as this host.
