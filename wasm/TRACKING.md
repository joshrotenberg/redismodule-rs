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
  aux data needs a data type, `MODULE UNLOAD` refuses the module unless
  `wasm.persist no` leaves persistence off (H16).
- `WASM.CALL` declares its keys with a key spec like `FCALL`'s, so `COMMAND
  GETKEYS`, ACL key patterns and a cluster's `MOVED` and `CROSSSLOT` checks see
  them.
- Each instance's linear memory is limited by `wasm.max-memory` (64 MiB by
  default), and `INFO wasm` reports instances, their memory and compiled code.
- Runs unchanged on Redis 7.2, 7.4, 8.0, 8.2 and 8.10 (a smoke test of every
  feature plus a two-node cluster, Linux arm64, and the whole test suite on
  8.0.2 in Linux and 8.10.2 on macOS); not on 7.0, which has no ACL
  categories for module commands (S11, and the module's own `@scripting`). The `WASM.*`
  commands are in `@scripting`, as `FCALL` and `FUNCTION` are.
- Functions have Lua's flags, with Lua's checks before they start and on the
  commands they call, and `WASM.CALL_RO` runs those with `no-writes`. Guests
  have `call` and `pcall`, an ACL check and `set-resp`, as Lua scripts have.
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
- The host is about 790 lines of code at H5, more since, not counting
  comments and unit tests. The example guest is 37 KB as a core module. The
  twenty-one integration tests take about 10 seconds.

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
| `lrange`: the same LRANGE, count only | 58 to 59 | 139 | 2.4 |
| `median_flat`: `median` with `call-flat` | 253 to 254 | 126 | 0.50 |
| `lrange_flat`: `lrange` with `call-flat` | 56 | 77 | 1.4 |
| `primes`: sieve up to 10000 | 423 to 429 | 26 to 27 | 0.06 |

Rerun after H8 and H9 (flags, memory limit): the same numbers within noise.
`lrange` was added for H15 in that rerun.

Where `incr_twice`'s time goes, measured in one server with a throwaway native
module whose command makes the same two `INCR`s through `RedisModule_Call`
with the host's options (two runs, µs per call): a plain `INCR` 0.07, the
native command 0.94 to 0.97, Lua's `FCALL incr_twice` 1.31, wasm's 2.64 to
2.67; `nested`, with no calls, 1.31 in wasm and 1.48 in Lua. So
`RedisModule_Call` is not the gap, and a wasm call's fixed cost is close to
Lua's: what costs more is each call from the guest into Redis, about 0.67 µs
against about 0.45 µs for a native `RedisModule_Call`. The difference is the
component ABI and the host's per-call bookkeeping. Looking commands up in a
thread-local cache by the name as the guest spells it, without lowercasing it
or taking a mutex, then brought wasm's `incr_twice` to 2.50 to 2.52 µs.

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
| H6 | done | **Key specs for `WASM.CALL`.** Declared with the `#[command]` macro and a key spec like `FCALL`'s: `numkeys` at argument 2, the keys after it, flags `RW`, `access` and `update`; `no-mandatory-keys`, arity -3. `COMMAND GETKEYS`, ACL key patterns and cluster routing (`MOVED`, `CROSSSLOT`) see the keys. Redis before 8.4 routes a module command by the getkeys API or its first, last and step, not by its key spec, so the commands also have `getkeys-api` and answer key-position requests (G8). | `COMMAND GETKEYS WASM.CALL f 2 a b x` returns `a b`. |
| H7 | done | **Compile off the main thread.** `WASM.LOAD` blocks the client and compiles on a thread; the main thread instantiates and adds the library. Precompiled components (`Engine::precompile_component`) would skip the compile entirely, but loading one is `unsafe`: only for trusted input. | The server answers other clients while a large component compiles. |
| H8 | done | **Memory.** `wasm.max-memory` (default 64 MiB) limits each instance's linear memory through a wasmtime `ResourceLimiter`; a guest that grows past it traps and its call fails, which drops the instance. A function does not start while Redis is over `maxmemory`, and its calls use `RedisModule_Call`'s `M` flag. `INFO wasm` reports libraries, functions, component and compiled code bytes, live instances and their linear memory, and the limit. Not done: replacing a kept instance that grew large but stays under the limit; a failed call already drops it. | Tests for the limit, the replacement, the refusal and the `INFO` fields. |
| H9 | done | **Script parity.** Function flags (`no-writes`, `allow-oom`, `allow-stale`, `no-cluster`, `allow-cross-slot-keys`) in the WIT, `WASM.CALL_RO`, Lua's checks before a function starts, read-only calls that refuse write and may-replicate commands, one slot per call on a cluster node, no calls while a library loads, flags in `WASM.LIST` and the RDB (H9a); `call`, which fails the function on an error reply, next to `pcall`, and `acl-check-command` and `set-resp` imports (H9b). Differences left: G5, G6, and RESP3 as the default. | Each has a test matching what Lua does. |
| H10 | done | **Guest helper library.** `wasm/guest-sdk`, crate `redis-wasm-guest`: a `Value` type that converts to and from pre-order node lists and flat replies, `From` conversions, and `call`, `pcall` and `log` that take and return `Value`s. It generates bindings for a `guest-imports` world (the host's interface alone), and a library's own bindings share its types through wit-bindgen's `with`. | The example guest uses it. |
| H11 | done | **Bounds check cost.** Decided: keep signal-based traps off, so guests get explicit bounds checks. With H3's benchmark, turning them on made `primes` 18% faster (185 against 226 µs, old sieve loop) and `median` 6%. On Linux they are not safe in a module: wasmtime installs `SIGSEGV`/`SIGBUS`/`SIGILL`/`SIGFPE` handlers that chain to Redis's crash handler, but `CONFIG SET crash-log-enabled` (either value) reinstalls Redis's handlers or the defaults over them, and the next out-of-bounds guest access kills the server: silently after `no`, with a crash report after `yes` (G7). On macOS wasmtime takes faults through Mach exception ports, so the same steps are safe there. A crash outside wasm (`DEBUG SEGFAULT`) still produced Redis's crash report with wasmtime's handlers installed. | A decision, with a crash report checked with handlers on. |
| H12 | done | **Context in the store.** The store holds a raw pointer to the command's `Context`. The safety argument is written down in `host.rs` ("The context pointer"): set and cleared around each guest run, re-entrant calls use their own instance, a panic drops the instance with the stale pointer, and instances stay on the main thread. No SDK pattern replaces it yet (S3). | Reviewed and documented, or replaced. |
| H13 | todo | **Footprint.** The release dylib is 12.4 MB (macOS). Loading the JavaScript component (release build, macOS): `INFO wasm` accounts for 95 MB of compiled code and 7.7 MB of linear memory, and RSS reached 0.97 GB, then 1.35 GB after a delete and two more loads. The compiler allocates through the module's global allocator, Redis's, so `used_memory_peak` reached 1.05 GB during the load, and 57.7 MB stayed counted after `WASM.DELETE` (not growing on later loads: caches, not a leak). With `std::alloc::System` as the module's allocator, `used_memory` stayed near 1 MB (peak 25 MB, the argument itself) and RSS behaved the same. Decision needed: count wasmtime's heap in `used_memory`, so a load's transient 1 GB counts against `maxmemory`, or not, so 70 MB of lasting heap per JavaScript library is invisible to it. | Sizes recorded before and after. |
| H14 | todo | **CI.** A job that builds the guest, runs the host's tests and lints the guest. | Wherever the crate lives with CI. |
| H16 | done | **Unloading.** Since H5 the module registers a data type for its aux data, and Redis refuses `MODULE UNLOAD` for a module with one. `wasm.persist no`, an immutable config read in `init`, leaves the type unregistered and persistence off, so the module unloads. Each unload and reload grew RSS by about 0.5 MB (7.9 MB at start, 39.8 MB after 5 cycles, 54.8 MB after 30); not investigated. | `MODULE UNLOAD` works with the argument; a test covers both ways. |
| H17 | done | **Load time.** Loading an RDB compiled each library's component on the main thread before the server served: 1.56 to 1.67 s for the JavaScript library (release build), 15 ms of it reading. Now the libraries are added at once with their code compiling on a thread (`code.rs`): a call to a library still compiling blocks its client until the code is ready (or waits in place inside `MULTI` or a script), `INFO wasm` counts the libraries compiling, and the server was ready in 0.11 to 0.12 s. | Load time with the JavaScript library, before and after. |
| H18 | done | **Dump, restore, flush.** `WASM.DUMP`, `WASM.RESTORE <payload> [FLUSH\|APPEND\|REPLACE]` and `WASM.FLUSH`, as `FUNCTION DUMP`, `RESTORE` and `FLUSH` work. The payload (`dump.rs`) holds names and components with an FNV-1a checksum; a restore compiles on a thread, lists every library's functions, checks every conflict, then adds all or none. | Tests restore into another server under each policy, refuse a conflicting or damaged payload without changing anything, and replicate `WASM.FLUSH` and `WASM.RESTORE`. |
| H19 | done | **Stats.** `WASM.STATS`, `allow-busy` like `FUNCTION STATS`: the running function's name and duration, and the counts of libraries and functions (nil while a library loads and holds the host). | The busy test reads it while a function spins and after it is killed. |
| H15 | done | **Large replies into the guest.** `lrange` (the LRANGE of `median`, count only) took 139 µs in wasm against 58 µs in Lua; a profile showed the time spread over allocating and copying each string on both sides. `call-flat` and `pcall-flat` return the reply as nodes whose strings are spans of one buffer: `lrange_flat` takes 77 µs (1.4 times Lua) and `median_flat` 126 µs (0.50 of Lua's 253). The typed `call` and `pcall` stay for small replies. | `median`'s time split, and a lower number. |

## Repository

Found by an audit of fork master outside the crates' sources (2026-10-20).
These are ordinary fork PRs; none depends on the wasm host. All are merged;
the commit is master's.

| ID | status | item |
|---|---|---|
| R1 | done, 9b0f605 | Delete `Dockerfile`, `.dockerignore`, `build/docker/` and `.gitmodules`: they run the `readies` setup and `Makefile` that upstream deleted in October 2024, and cannot build. |
| R2 | done, 848824a | CI (`ci.yml`): drop `submodules: recursive` (no submodules) and the Python setup (no Python); run the matrix on the default features, so the tests use the committed bindings, and keep `bindgen-runtime`, clang and the bindings drift check in one job; use the pinned 1.94.0 toolchain or say why CI uses stable. |
| R3 | done, cb78fec | Release workflows: `release-pr.yml` is still enabled and would open a release-plz PR that rewrites the 3.0.0 changelog sections and bumps versions. Delete `release.yml` and `release-pr.yml`, or limit them to the upstream repository. |
| R4 | done, 686f84c | `dependabot.yml`: remove the ignore entry for bindgen 0.56; consider adding the `github-actions` ecosystem. |
| R5 | done, 1353e52 | `deny.toml`: nothing runs it since upstream's `Makefile` went. Add a cargo-deny CI job (its license list likely needs `Unicode-3.0` for `unicode-ident`) or delete it. |
| R6 | done, 8ddccf1 | `docs/MIGRATING-3.0.md`: the `system-allocator-fallback` dev-dependency example uses `version = "3"`, which does not resolve while 3.0 is unpublished; use the git and `rev` form. |
| R7 | done, e4c5d1c | `README.upstream.md` keeps upstream's dead `redis.io/topics/modules-intro` links: leave it verbatim, or say at its top that it is. |
| R8 | done, 9e5ff10 | `build.sh` and `test.sh`: `--all` is the old spelling of `--workspace` (cosmetic). |

The audit found nothing to change in `examples/`, `tests/`, `.gitignore`,
`rust-toolchain.toml`, the changelogs and the side crates' manifests. The
`repository` field is S8.

## SDK changes the spike found

These are in this repository's SDK and do not depend on the wasm host; each
can be an ordinary PR. A done item names its commit on master.

| ID | status | item |
|---|---|---|
| S1 | done, dad8637 and 9258efb | `RedisValue::Error(String)`, an error reply with an owned message that can sit inside an array; a call reply's error converts to it. |
| S2 | done, a5eb60d | `RedisValueKey` had no simple string, so a map key sent as a simple string went out as a bulk string. `RedisValueKey::SimpleString` keeps it. |
| S3 | todo | No lifetime-safe way to hand a `Context` to code that needs `'static` data, such as a runtime's store. |
| S4 | todo | Replies can only be built as a `RedisValue` tree; the low-level reply functions are crate-private since 3.0, so nothing streams a reply. |
| S5 | todo | The `min-redis-compatibility-*` floors are mutually exclusive, and Cargo unifies features across a workspace, so a second module crate in the workspace must use the root's floor. The full-workspace 7.2 Clippy check combines that floor with the host's default 6.0 floor and fails. Run alternate-floor SDK checks with `--exclude redis-module-wasm`; check the host in the default-floor workspace build. |
| S6 | worked around | On macOS a stripped release build of the host fails to load ("mis-aligned LINKEDIT string pool"); the workspace sets `strip = "none"` for it. Check with a newer toolchain. |
| S7 | done, 9ea85f4 | `ServerInfo::field` panicked on a field name with a NUL byte; upstream returns `None`, and so does it now. |
| S8 | done, 6d21760 | `Cargo.toml`'s `repository` field named upstream. |
| S10 | done, d100d13 | No safe wrappers for `RedisModule_GetCommandKeysWithFlags` and `RedisModule_ClusterKeySlot`, and `ContextFlags` was not `Copy`. `Context::get_command_keys` and `cluster::key_slot` (computed in Rust, so it works before Redis 7.4) replace the host's `raw` calls. |
| S11 | done, 955e3d4 | Registering commands from the `#[command]` macro required `RedisModule_SetCommandACLCategories` (Redis 7.2) even when no command declares ACL categories, so a module using the macro did not load on Redis 7.0. It is now needed only by a command that declares categories; this host's do (`@scripting`), so it still needs 7.2. |
| S9 | done, dabad65 | `redis_module!`'s `configurations` block compiled only with all four groups (`i64`, `string`, `bool`, `enum`) and `module_args_as_configuration` listed, and module arguments were parsed as plain integers, so a memory config given as `8mb` failed the load. The host now lists only its groups and takes module arguments as configuration. |

## Module API gaps

Outside this repository: each needs a change in Redis's module API. Record the
evidence as the host meets them.

| ID | gap | effect on the host |
|---|---|---|
| G1 | No module API registers an engine for `FUNCTION` or `EVAL` (checked in Redis 8.0.0 and unstable `module.c`). | The host needs its own `WASM.*` commands, which now cover `FUNCTION`'s (H9, H18); `FUNCTION LIST`, `FCALL`, `FUNCTION DUMP` and `SCRIPT KILL` do not see wasm libraries. |
| G2 | `RedisModule_CreateCommand` works only while the module loads. | A function loaded later cannot be a command of its own; every call goes through `WASM.CALL`. |
| G3 | No module API adds memory allocated outside the Redis allocator to `used_memory`. | `maxmemory` and `INFO memory` do not count guest memory or compiled code (confirmed: 1.3 MB used, 16 to 18 MB RSS). `INFO wasm` reports them instead: 7.7 MB of linear memory and 95 MB of compiled code for the JavaScript library. |
| G4 | Aux data reaches an AOF only inside its RDB preamble: no module API adds commands to a rewritten AOF without one (`aof_rewrite` is per key). Redis writes its own `FUNCTION` libraries into the rewrite (`rewriteFunctions` in `aof.c`). | With `aof-use-rdb-preamble no` (the default is `yes`), an AOF rewrite drops the libraries: `WASM.LIST` was empty after `BGREWRITEAOF` and a restart. |
| G5 | No module API says whether writes are refused for a disk error (`MISCONF`) or for too few good replicas (`NOREPLICAS`). | Lua refuses a function that may write before it starts; here it starts, and its first write command fails with that error (`RedisModule_Call`'s script mode checks each write). |
| G6 | `COMMAND INFO` hides the `may_replicate` flag ("Hidden on purpose" in `server.c`), and no module API returns a command's flags. | Lua's read-only functions refuse may-replicate commands. The host names them (`PUBLISH`, `SPUBLISH`, `PFCOUNT`; script mode refuses `EVAL`, `EVALSHA` and `FCALL`), so a module command with that flag is not refused. |
| G7 | No module API installs fault handlers that Redis keeps: `CONFIG SET crash-log-enabled` calls `setupSigSegvHandler` or `removeSigSegvHandlers`, which replace every `SIGSEGV`, `SIGBUS`, `SIGFPE` and `SIGILL` handler. | A module cannot use signal-based bounds checks on Linux (H11): explicit checks cost 18% on the sieve. Redis's crash handler would have to pass faults in a module's code to the module. |
| G8 | Before Redis 8.4, cluster routing (`getNodeByQuery`) finds a command's keys with `getKeysFromCommand`, which uses the module getkeys API, a getkeys function, or the legacy first, last and step, and not key specs. A numkeys key spec has no legacy range. | Fixed in the module: `WASM.CALL` and `WASM.CALL_RO` have `getkeys-api` and report the numkeys keys. Without it, Redis 7.2 to 8.2 ran a call on any node it reached (seen on 8.0.2 in Linux); 8.4 and later route from key specs. |

## Open questions

| ID | question |
|---|---|
| Q1 | Where the crate lives so others can use it. Answered: the fork's `experiment/wasm-host` branch, kept rebased on master. |

## Log

- 2026-10-20: rebased onto SDK master `bfacc11`, which adds optional command
  builders alongside legacy command tuples. No host adaptation needed. Rust
  1.94.0 and Redis 8.10.2: workspace and guest formatting, default workspace
  Clippy, separate SDK 7.2 Clippy, example builds, and guest Clippy passed. The
  full workspace suite passed, including 96 SDK integration tests, four
  standalone harness cases, seven host unit tests, and 21 host integration
  tests with JavaScript; the guest helper's four tests passed.
- 2026-10-20: rebased onto SDK master `1d918e7`, which adds borrowed call-error
  prefixes and byte notification names. No host adaptation needed. Rust 1.94.0
  and Redis 8.10.2: workspace and guest formatting, default workspace Clippy,
  separate SDK 7.2 Clippy, example builds, and guest Clippy passed. The full
  workspace suite passed, including 91 SDK integration tests, four standalone
  harness cases, seven host unit tests, and 21 host integration tests with
  JavaScript; the guest helper's four tests passed.
- 2026-10-20: rebased onto SDK master `e9425f8`, which adds direct and scoped
  database selection helpers. No host adaptation needed. Rust 1.94.0 and Redis
  8.10.2: workspace and guest formatting, default workspace Clippy, separate
  SDK 7.2 Clippy, example builds, and guest Clippy passed. The full workspace
  suite passed, including 87 SDK integration tests, four standalone harness
  cases, seven host unit tests, and 21 host integration tests with JavaScript;
  the guest helper's four tests passed.
- 2026-10-20: rebased onto SDK master `2f8e2a9`, which extends the integration
  harness with module arguments, bounded startup diagnostics, replicas,
  persistent restarts, and confirmed bind-collision retries. No host adaptation
  needed. Rust 1.94.0 and Redis 8.10.2: workspace and guest formatting, default
  workspace Clippy, separate SDK 7.2 Clippy, example builds, and guest Clippy
  passed. The full workspace suite passed, including 83 SDK integration tests,
  four standalone harness cases, seven host unit tests, and 21 host integration
  tests with JavaScript; the guest helper's four tests passed.
- 2026-10-20: rebased onto SDK master `efad730`, which reuses existing custom
  ACL categories while rejecting other registration errors. No host adaptation
  needed. Rust 1.94.0 and Redis 8.10.2: workspace and guest formatting, default
  workspace Clippy, separate SDK 7.2 Clippy, and guest Clippy passed. The full
  workspace suite passed, including 72 SDK integration tests and 21 host
  integration tests with JavaScript; the guest helper's four tests passed.
- 2026-10-20: rebased onto SDK master `5bf61ff`, which validates numeric
  configuration defaults, parses boolean arguments as case-insensitive
  `yes`/`no`, and reads module arguments in pairs. No host adaptation needed.
  On Rust 1.94.0 and Redis 8.10.2, workspace and guest formatting and Clippy
  passed, as did the full workspace suite (21 host integration tests,
  including JavaScript) and the guest helper's four tests. The full-workspace
  7.2 Clippy command exposed S5; the separate SDK 7.2 check with the host
  excluded passed. HANDOFF.md now documents that split.
- 2026-10-20: moved to the fork's `experiment/wasm-host` branch, rebased on
  master df36fea, and moved onto the SDK changes there: `cluster::key_slot`
  and `Context::get_command_keys` replace the `raw` calls (S10), so the
  one-slot rule no longer needs `RedisModule_ClusterKeySlot` and should hold
  on 7.2 too (not yet run there); a simple-string map key stays one (S2); the
  configurations list only their groups, and module arguments set them, with
  units (S9). R1 to R8 and S1, S2, S7 to S11 are merged on master.
- 2026-10-20: final smoke test of the night's build on Linux: every feature
  (calls, flags, `call`/`pcall`, flat replies, dump and restore, stats, kill,
  restart) and a cluster `MOVED` passed on Redis 7.2.16, 7.4.11, 8.0.6, 8.2.10
  and 8.10.2; 7.0.15 refused the module ("Redis version does not support ACL
  categories").
- 2026-10-20: the whole test suite ran on Linux (Redis 8.0.2 from Debian, in a
  colima VM; the JavaScript tests skipped without Node) and found G8: the
  cluster test failed because 8.0 did not route `WASM.CALL`. Fixed with
  `getkeys-api`; all twenty-one tests then passed there.
- 2026-10-20: H17 done: libraries from an RDB compile in the background (see
  H17). Test: after a restart with the JavaScript library saved, `INFO wasm`
  shows it compiling, a call waits and returns the right reply, and PINGs are
  answered meanwhile.
- 2026-10-20: H19 done: `WASM.STATS`.
- 2026-10-20: H18 done: `WASM.DUMP`, `WASM.RESTORE` and `WASM.FLUSH`. `Host::add`
  is now `stage` (instantiate, list functions) and a commit, so a restore can
  stage every library, check conflicts against the policy, and add all or
  none.
- 2026-10-20: H10 done: `wasm/guest-sdk` with four unit tests that run on the
  host, and the example guest rewritten on it (its replies are `Value`s; the
  same nineteen integration tests pass). The WIT gained the `guest-imports`
  world for it. The core module grew to 37 KB with the functions added since
  milestone 1 and the helper.
- 2026-10-20: H15 done: `call-flat` and `pcall-flat` in the WIT, the reply
  walk shared by both encodings (a `Sink` per encoding), `median_flat` and
  `lrange_flat` in the benchmark (the Lua library has the same functions under
  those names), and a `run_flat` fixture whose test checks that a flat reply
  rebuilt into nodes equals the typed reply for a list, a map, a status, a nil
  and an error. A profile (`sample`) of `lrange` under load had shown the time
  spread over per-string allocation and copying on the host, in wasmtime's
  lowering, and in the guest's allocator.
- 2026-10-20: H15 measured (see H15): the benchmark gained `lrange` in both
  libraries.
- 2026-10-20: H13 measured (see H13): the compiler's temporary allocations
  count in `used_memory` (peak 1.05 GB for one JavaScript load) with Redis's
  allocator, and do not with the system allocator. Left as a decision. The
  benchmark, rerun after H8 and H9, gave the same numbers as before: the flag
  checks cost nothing measurable.
- 2026-10-20: H11 done (keep signal-based traps off; see H11 and G7), with
  the experiment run on macOS and, in a colima VM, on Linux with Redis 8.10.
  The example guest gained `oob` and a test that an out-of-bounds access traps
  and drops the instance. The same Linux build passed a smoke test of every
  feature on Redis 7.2.16, 7.4.11, 8.0.6, 8.2.10 and 8.10.0, and failed to load
  on 7.0.15 (S11). The `WASM.*` commands joined `@scripting`; a test checks
  that a user without it is refused.
- 2026-10-20: H12 done: reviewed every path that sets, uses or clears the
  store's context pointer (calls, re-entrant calls, epoch yields, the reply
  callback that adds a library, the RDB fork, panics) and wrote the argument
  in `host.rs`. No code changed.
- 2026-10-20: H17 measured, not changed: `aux_load` logs how many libraries
  it loaded, their bytes and the time, and that showed compiling is all of it
  (see H17).
- 2026-10-20: H16 done: `wasm.persist`, an immutable `bool` config (default
  `yes`). `redis_module!` applies module configs before `init`, so `init`
  registers the `wasm-libs` type only when it is on; with it off, loading an
  RDB does not clear the libraries either. Test: with `--wasm.persist no`,
  `CONFIG SET` is refused, the module unloads and loads again with `MODULE
  LOADEX ... CONFIG wasm.persist no`, and with the default it does not unload.
- 2026-10-20: H9b done, so H9 is: `call` traps the guest with a `CallError`
  carrying the error reply (a `trappable` import), and the function replies
  with that error followed by `function: <name>.`; `pcall` returns it as a
  node, as `call` used to. `acl-check-command` uses the SDK's
  `acl_check_command_permission` for the current user, and `set-resp` picks
  between RESP2 and RESP3 call options (each call starts with RESP3). The
  example guest's functions moved to `pcall` and it gains `run`, `acl_check`
  and `resp_shape`; the JavaScript and benchmark guests use `call`, as the Lua
  benchmark uses `redis.call`. Test: `call` and `pcall` on an error reply and
  what each does to the kept instance, `acl_check` as two users and for an
  unknown command, and `set-resp` 3, 2 and 4.
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
