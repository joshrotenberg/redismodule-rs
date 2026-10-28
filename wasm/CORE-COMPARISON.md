# The wasm host and the core integration branch

A version-pinned comparison of this module-hosted runtime with the public
`wasm-integration` branch of Redis, for #86. It records what each side does,
where the evidence comes from, and which gaps need a change to Redis core or
to the module API. It does not choose runtime APIs or add parity features.

Written 2026-10-27.

## Pins

- **This host**: `experiment/wasm-host` at `b10c51e`, wasmtime 36.0.19 and
  the other inputs in `wasm/ci/locks`, Rust 1.94.0. Runs used Homebrew Redis
  8.10.2 (`malloc=libc`) on macOS 26.7.1, Apple M3 (8 cores, 16 GB), with a
  release build of the module.
- **Core branch**: `wasm-integration` of
  [minchopaskal/redis](https://github.com/minchopaskal/redis/tree/0302b84e6255a04a8fe72b8d4fe4b27e93a96b01)
  at `0302b84e6255a04a8fe72b8d4fe4b27e93a96b01` (committed 2026-10-01, "auto
  load wasm extensions under extensions-dir folder (configurable)"). Its
  `src/version.h` says 8.9.241, and it vendors WAMR 2.4.5
  (`deps/wamr/core/version.h`). It was fetched with a shallow clone and the
  SHA checked. Nothing on it was built, run or executed: no make, no scripts,
  no tests. Every core path below is at that SHA.

The core evidence is the branch's code, its `WASM_EXTENSIONS_DESIGN.md` and
`extensions/README.md`, and its tests, read but not run. Where its design
document and its code differ, the code is cited and the difference noted.

## Labels

Each claim carries one label:

- **measured**: run on this host. Unmarked measurements are from this
  comparison, 2026-10-27; "issue 68" marks the memory runs of the same day
  (see [TRACKING.md](TRACKING.md), Memory accounting); "earlier" marks a
  measurement recorded in TRACKING.md and not repeated here.
- **source**: read in code, with its path.
- **inferred**: follows from the code read, but not exercised.
- **untested**: neither settled by reading nor run.

No core claim is measured. In the matrix, M, S, I and U stand for the four.

Today's host runs: `wasm/bench/run.sh` with epoch interruption on and off
(below), the host's test suite (`cargo test -p redis-module-wasm`: 7 unit and
21 integration tests passed, the JavaScript guest included), and two probes
with the hello guest, one for the ACL log and one for reply protocols.

## Capability matrix

| Area | This host | Core branch (not run) | Gap |
|---|---|---|---|
| Guest interface and SDK model | Components implementing the WIT world `library`; typed reply nodes (S). Rust SDK; JavaScript through componentize-js; both guests pass the same tests (M) | Core modules with `redis` imports taking `i32` pointers; raw RESP3 in and out, with a host tokenizer; C, C++, Rust `no_std`, TinyGo and AssemblyScript SDKs; a blob value type (S) | By design. A blob-like type is a module data type, which this module could add itself |
| Loading, calling and management | `WASM.LOAD`, `WASM.CALL`, `WASM.CALL_RO`, `WASM.LIST`, `WASM.DELETE`, `WASM.DUMP`, `WASM.RESTORE`, `WASM.FLUSH`, `WASM.STATS`; compiles on a thread (M) | `FUNCTION LOAD` of `#!wasm`, `FCALL` and the `FUNCTION` management subcommands; a function can also be a command of its own; startup autoload (S). Loads on the main thread (S) | Using `FUNCTION` and `FCALL` needs an engine API (G1); commands per function need commands after load (G2) |
| Command and key metadata | Keynum key spec plus `getkeys-api`: `COMMAND GETKEYS`, ACL key patterns, `MOVED` and `CROSSSLOT` (M) | `FCALL`'s key spec; direct commands get a leading-keys spec and a legacy range (S); a cluster test exists (S) | None open for `WASM.CALL`; G8 is closed in the module. Direct commands need G2 |
| Script flags | Lua's five flags and `WASM.CALL_RO` (M). `MISCONF` and `NOREPLICAS` surface at the first write; may-replicate commands are named in a list (S) | None. Functions register with flags 0, so they are treated as writing; `FCALL_RO` refuses them (S) | Core can add flags in tree. G5 and G6 need module API changes |
| ACLs | Commands run as the caller's user; a denial fails the function and the ACL log says `module` (M); `acl-check-command` (M) | The script path checks ACLs and logs denials with the `lua` context (S, I); no check import; direct commands have their own ACL identity (S) | Neither side needs an API change |
| Yielding, `BUSY` and kill | Yields every 10 ms; `-BUSY`, `WASM.KILL`, `UNKILLABLE` and `WASM.STATS` as for Lua (M). Costs 0 to 5% on call and reply work, 2.3 times on a tight loop (M) | Runs to completion or to 100 million instructions, then traps; nothing calls `scriptInterrupt`, so no `-BUSY` and no kill (S, I) | Core can close it in tree; its design document says WAMR would need a patch. Nothing open here |
| Errors and traps | `call` and `pcall` as in Lua; a trap fails with `ERR function '<name>' failed: ...`; any failure drops the instance (M) | A trap or nonzero return becomes an error naming the function; `call` returns -1 or a staged reply; the instance stays after a trap (S, I) | By design on both sides |
| Persistence, AOF and replication | RDB aux data through restart, `DEBUG RELOAD`, full sync and AOF (M); loading runs no guest code (S); a rewrite without the RDB preamble drops libraries (earlier) | RDB and every AOF rewrite keep libraries (S); effects replicate; blobs are an RDB type and replicate as `RESTORE` (S); loading runs guest startup code (I); every server loading them needs `BUILD_WASM` (I) | G4 needs a module API change |
| Memory accounting | Issue 68 (M): with Redis's allocator the compiler's heap counts in `used_memory`; linear memory and compiled code do not; `wasm.max-memory` per instance | A 64 MiB pool allocated with `zmalloc` at startup holds the runtime's own data (S), counted from the start (I); linear memory is mapped outside it (S) and not counted (I); 8 MiB per library (S) | Counting outside memory needs G3 here; core can count it in tree |
| Fault handling | Signal-based traps off, explicit bounds checks; an out-of-bounds access traps and the server serves on (M); 18% on the sieve (earlier) | WAMR's hardware bounds checks: no software check on guest loads and stores, `SIGSEGV`/`SIGBUS` handlers chained to Redis's (S); `CONFIG SET crash-log-enabled` replaces them (S), after which such an access would crash the server (I, U) | G7 needs a module API change; core can coordinate its handlers in tree |
| Supported versions and deployment | Loads into stock Redis 7.2 and later; 8.10.2 today (M), 7.2 to 8.10 earlier | A Redis 8.9.241 branch built with `BUILD_WASM=yes` and cmake, on Linux or macOS (S) | By design |
| Toolchain and binary size | 12.0 MiB dylib; components of 37.7 KB (hello), 41.3 KB (bench) and 10.7 MiB (JavaScript) (M) | WAMR's classic interpreter linked into `redis-server`, size not measured; committed guests of 2.5 to 16 KB (S) | By design; relative speed untested |

## Areas

### Guest interface and SDK model

This host:

- A library is a component implementing the `library` world in
  [`wasm/wit/redis.wit`](wit/redis.wit): it exports `functions`, which lists
  names and flags, and `invoke(name, keys, args)`, which returns reply nodes
  in pre-order. It imports `call`, `pcall`, `call-flat`, `pcall-flat`,
  `acl-check-command`, `set-resp` and `log`. Replies keep their RESP types
  both ways. No WASI imports. (source)
- Bindings come from wit-bindgen; [`guest-sdk`](guest-sdk) wraps them in a
  `Value` type. JavaScript guests are built with componentize-js. The Rust and
  JavaScript guests pass the same integration tests. (measured)

Core branch:

- A guest is a core module that exports `memory`, the marker
  `redis_wasm_abi_version_0_1_0`, `_initialize` (then an optional `main`) or
  `_start`, and one `() -> i32` export per function
  (`src/function_wasm.c` lines 477 to 536). Its imports, in the `redis`
  namespace, take `i32` pointers into guest memory: `input_len`,
  `input_read`, `keys_count`, `call`, `reply_read`, `last_error_read`,
  `parse_resp`, `reply_raw`, `status_reply`, `error_reply`, `register_function`,
  `create_command`, `blob_register`, `blob_len`, `blob_read`, `blob_write`,
  `blob_write_expire` and `log` (lines 1043 to 1062). (source)
- Keys and arguments arrive as one little-endian packed buffer; a command's
  reply is staged as RESP3 bytes for the guest to copy out; the guest replies
  with RESP bytes that the host validates (lines 361 to 381 and 1214 to
  1228). (source)
- SDKs: C (`sdk/wasm/c/redis_wasm.h`), C++, Rust as an allocation-free
  `no_std` crate (`sdk/wasm/rust/redis-wasm`), TinyGo (`sdk/wasm/go`) and
  AssemblyScript (`sdk/wasm/js`). (source)
- Guests can keep opaque values in the keyspace, `OBJ_WASM`, owned by the
  SHA-256 of the module's bytes and a registered type name
  (`src/function_wasm.c` lines 831 to 1039, `src/server.h`). (source)

Gap: the two ABIs are design choices on each side, not limits of Redis's APIs.
A blob-like value type would be a module data type, which this module could
add itself; neither the request nor this comparison proposes one.

### Loading, calling and management commands

This host:

- The `WASM.*` commands in [`wasm/src/lib.rs`](src/lib.rs) mirror
  `FUNCTION` and `FCALL`. `WASM.LOAD` compiles on a thread behind a blocked
  client; the test that pings another client during a slow load passed. Each
  library keeps one instance between calls. (measured)

Core branch:

- `FUNCTION LOAD` takes `#!wasm name=<library>` followed by the binary; the
  Functions framework supplies `FCALL`, `FUNCTION LIST`, `DELETE`, `FLUSH`,
  `DUMP`, `RESTORE` and `STATS` (`src/function_wasm.c` lines 1262 to 1297
  register the engine). `FCALL_RO` refuses every WASM function (see Script
  flags). (source)
- `create_command` also publishes the function as a top-level command
  (`src/functions.c` lines 226 to 288 and 857 to 871), at most 256 names per
  process, with arity and key count fixed until restart. (source)
- At startup Redis loads every top-level `*.wasm` in `extension-dir`
  (default `./extensions`, `src/config.c` line 31) and each `loadextension`
  path, naming each library `wasm_<sha256>` (`src/functions.c` lines 29 to
  130). (source)
- `wasm_runtime_load`, instantiation and the guest's startup exports run
  inside `FUNCTION LOAD` on the main thread, bounded by 10 million
  instructions (`src/function_wasm.c` lines 1072 to 1162). The 500 ms
  `timeout` the framework passes is unused (line 1076). (source)
- `FUNCTION FLUSH ASYNC` runs synchronously while a WASM library is loaded,
  because WAMR teardown is not thread-safe with loading (`src/functions.c`
  lines 391 to 406). (source)

Gap: G1 (no module API registers an engine for `FUNCTION` or `EVAL`) and G2
(`RedisModule_CreateCommand` only while loading) keep this host on its own
commands; both need module API changes. Off-thread loading is the core's to
add if it needs it; an interpreter has no compile step, so it may not.

### Command and key metadata

This host:

- `WASM.CALL` and `WASM.CALL_RO` have a keynum key spec and `getkeys-api`
  (`wasm/src/lib.rs` lines 155 to 222). `COMMAND GETKEYS`, ACL key patterns,
  `MOVED` and `CROSSSLOT` follow the declared keys; the key and two-node
  cluster tests passed. (measured)
- Without `allow-cross-slot-keys`, a function's commands must use one slot
  (`wasm/src/flags.rs`). The host does not check that a function uses only its
  declared keys. (source)

Core branch:

- `FCALL` keeps its key spec (`src/commands/fcall.json`). A direct command
  gets a key spec for its first `numkeys` arguments, `RW`, `ACCESS` and
  `UPDATE`, plus a legacy range, and the flags `write`, `denyoom`, `noscript`,
  `skip_monitor` and script-runner (`src/functions.c` lines 256 to 284).
  (source)
- Commands a guest calls pass the script layer's cluster checks, one slot per
  call since no flag allows more (`src/script.c` lines 496 to 580). The branch
  has a test that a direct command is routed by its keys and refuses
  `CROSSSLOT` (`tests/unit/wasm-commands.tcl`); it was not run. (source)

Gap: nothing open for `FCALL`-style calls. G8, routing before Redis 8.4, is
closed in this module. Direct commands need G2.

### Script flags

This host:

- Functions declare `no-writes`, `allow-oom`, `allow-stale`, `no-cluster` and
  `allow-cross-slot-keys`, with Lua's checks, and `WASM.CALL_RO` runs only
  `no-writes` functions; the flag tests, a replica's included, passed.
  (measured)
- A module cannot ask for `MISCONF` or `NOREPLICAS` state, so a writing
  function starts and its first write fails (G5); `COMMAND INFO` hides
  `may_replicate`, so such commands are named in `wasm/src/busy.rs` (G6).
  (source)

Core branch:

- `register_function` passes flags 0 (`src/function_wasm.c` line 801), and
  the design document lists flags as deferred. `scriptPrepareForRun` treats a
  function without `no-writes` as writing: it is refused on a read-only
  replica, under `MISCONF` or `NOREPLICAS`, on a stale replica, over
  `maxmemory` and from `FCALL_RO` (`src/script.c` lines 192 to 275). Direct
  commands are `write` and `denyoom`. The `FCALL_RO` refusal has a test.
  (source)

Gap: the core can add flags in tree; the framework already handles them. G5
and G6 need module API changes for this host.

### ACLs

This host:

- Guest commands run through `RedisModule_Call` in script mode as the
  caller's user (`call_options` in [`wasm/src/host.rs`](src/host.rs)). With a
  user denied `SET`, `WASM.CALL run 0 SET k v` failed with `NOPERM ... function:
  run.` and `ACL LOG` recorded reason `command`, object `set`, context
  `module`. (measured)
- `acl-check-command` answers for the running user; the `WASM.*` commands are
  in `@scripting`. (measured, by the tests)

Core branch:

- Guest commands go through `scriptCall`, whose `scriptVerifyACL` logs a
  denial with `ACL_LOG_CTX_LUA` (`src/script.c` lines 404 to 418), so `ACL
  LOG` would show context `lua` for a WASM guest. (source, inferred)
- There is no import like `redis.acl_check_cmd`. Direct commands have their
  own ACL identity, in `@scripting`, `@write` and `@slow`, and extensions load
  before `ACLLoadUsersAtStartup` so ACL files can name them (`src/server.c`
  lines 8389 to 8398). (source)

Gap: none needs an API change.

### Yielding, `BUSY` and kill

This host:

- `Host::new` sets `config.epoch_interruption(true)`
  ([`wasm/src/host.rs`](src/host.rs) line 359). Each store gets a deadline one
  epoch ahead and a callback (lines 307 to 311 and 333); a thread advances the
  epoch every 10 ms (`TICK` and `Ticker` in [`wasm/src/busy.rs`](src/busy.rs)).
  At each deadline `on_epoch` calls `RedisModule_Yield`, so past
  `busy-reply-threshold` other clients get `-BUSY` and `WASM.KILL` can run.
  (source)
- The busy, kill and `UNKILLABLE` tests passed. (measured)
- The instrumentation's cost is measured below.

Core branch:

- A call runs `wasm_runtime_call_wasm_a` to completion with an instruction
  limit of 100 million (`src/function_wasm.c` lines 34, 538 to 551 and 1188);
  past it WAMR raises `instruction limit exceeded` (`deps/wamr/core/iwasm/
  interpreter/wasm_interp_classic.c` lines 1549 to 1557), and the call fails.
  The branch's `spin` test expects that error. (source)
- `scriptInterrupt` (`src/script.c` lines 141 to 169), which processes events
  and starts `-BUSY` replies, is called only from `src/script_lua.c` line 1601.
  The WASM engine never calls it. (source)
- So while a WASM function runs, other clients get no reply at all, and
  `FUNCTION KILL` and `SHUTDOWN NOSAVE` cannot run until it returns or hits
  the limit. Time spent in Redis commands does not count toward the limit, so
  it does not bound wall time. (inferred)
- The design document (section 5) says neither WAMR nor wasmer can resume a
  call after its budget runs out, and that Lua's behaviour would need a local
  patch to the interpreter loop.

Gap: the core can close this in tree. Nothing is open for this host. The
core's counterpart of the epoch cost is the per-instruction counter in the
interpreter loop, which was not measured.

### Errors and traps

This host:

- `call` fails the function on an error reply, which the client gets
  followed by `function: <name>.`; `pcall` hands the error to the guest. A
  trap or kill fails with `ERR function '<name>' failed: <reason>`, and any
  failure drops the library's instance, so the next call starts a new one.
  (measured, by the tests)
- Replies follow the client's protocol: in a probe, the `nested` reply's map
  arrived as `*2` to a RESP2 client and as `%1` after `HELLO 3`. (measured)

Core branch:

- A trap replies `WebAssembly function '<name>' trapped: <reason>` and is
  logged; a nonzero return replies `failed: <last error>` or `returned <n>`
  (`src/function_wasm.c` lines 1188 to 1208). (source)
- `call` returns -1 when the script layer refuses a command (ACL, OOM,
  cluster, read-only replica), with the reason in `last_error_read`; an error
  from a command that ran is inside the staged reply (lines 647 to 679). There
  is no built-in `redis.call` behaviour; the guest or its SDK decides.
  (source)
- After a trap the library's instance and execution environment stay; nothing
  in `wasmEngineCall` instantiates again (lines 1164 to 1233), so the next
  call sees the memory and globals the trapped call left. (source, inferred)
- A RESP3-only reply to a RESP2 client is refused, not converted (lines 1222
  to 1226), and there is a test for it. (source)

Gap: by design on both sides; either could change its own behaviour.

### Persistence, AOF and replication

This host:

- The RDB keeps libraries as aux data; restart, `DEBUG RELOAD`, a replica's
  full sync and an AOF restart bring them back, and `WASM.LOAD` and
  `WASM.DELETE` replicate as they are. (measured, by the tests)
- Loading an RDB runs no guest code; components compile on threads while the
  server serves (H17 in TRACKING.md, `wasm/src/code.rs`). (source)
- With `aof-use-rdb-preamble no`, a rewrite drops the libraries (G4).
  (measured earlier)
- The aux data needs a data type, so `MODULE UNLOAD` works only with
  `wasm.persist no`; that test passed. (measured)

Core branch:

- Libraries are function payloads in the RDB and in `FUNCTION DUMP`. Every
  AOF rewrite writes a `FUNCTION LOAD` per library (`src/aof.c` lines 2837 to
  2855), so a rewrite without the preamble keeps them; the branch tests that
  with `aof-use-rdb-preamble no`. Effects replicate as for Lua. (source)
- Blobs are `RDB_TYPE_WASM_BLOB` values (`src/rdb.c`), rewritten into the AOF
  as `RESTORE` (`src/aof.c` lines 2967 to 3007), and a `blob_write`
  propagates as `RESTORE ... REPLACE` (`src/function_wasm.c` lines 987 to
  1034). (source)
- Loading an RDB recreates each library through the engine, which
  instantiates it and runs its startup exports on the main thread before the
  server serves. (inferred)
- A server built without `BUILD_WASM` that receives a WASM library, by RDB,
  sync or restore, fails with `Engine 'WASM' not found` (`src/functions.c`
  line 1226); the design document says so for replicas. (inferred)

Gap: G4 needs a module API change, or Redis asking modules for AOF rewrite
commands outside keys. The data type rule for unloading is Redis's.

### Memory accounting

This host (issue 68, measured, see TRACKING.md):

- With `RedisAlloc`, the compiler's transient heap counts: 650 to 800 MiB in
  `used_memory` while the JavaScript component compiled, enough for other
  clients' writes to get `OOM`. 33 to 66 MiB stayed counted afterwards, mostly
  the compiler's context pool.
- Linear memory and compiled code are mapped outside the allocator, so
  `used_memory` and `maxmemory` count neither; `INFO wasm` reports them, and
  `wasm.max-memory` (64 MiB by default) bounds each instance.

Core branch:

- `wasmEngineInitEngine` allocates a 64 MiB pool with `zmalloc` and gives it
  to WAMR with `Alloc_With_Pool` (`src/function_wasm.c` lines 30 and 1262 to
  1282). WAMR's modules, instances and execution stacks come from it. The
  engine reports the pool's used bytes (lines 1235 to 1241), which `INFO`
  adds to `used_memory_vm_functions` (`functionsMemoryVM` in
  `src/functions.c` lines 1327 to 1342, `src/server.c` lines 6550 and 6595).
  (source)
- The pool is one allocation made at startup, so `used_memory` includes all
  64 MiB from the start, used or not, and guest work does not move it. A load
  whose runtime data does not fit fails. (inferred)
- Linear memory does not come from the pool. The branch builds WAMR without
  `WAMR_DISABLE_HW_BOUND_CHECK` (`deps/Makefile` lines 156 to 175), so on
  x86-64 and AArch64 `OS_ENABLE_HW_BOUND_CHECK` is set
  (`deps/wamr/core/shared/platform/linux/platform_internal.h` lines 83 to 90,
  and the darwin one, lines 70 to 77), and with `WASM_MEM_ALLOC_WITH_USAGE` at
  its default 0 (`deps/wamr/core/config.h` line 693)
  `wasm_allocate_linear_memory` maps 8 GiB per memory with `os_mmap` and makes
  only the current pages accessible
  (`deps/wamr/core/iwasm/common/wasm_memory.c` lines 1938 to 1995). (source)
- So linear memory, up to 8 MiB per library (128 pages,
  `src/function_wasm.c` lines 31 and 1116 to 1137), is in neither the pool nor
  `used_memory`, as here. The design document's statement that WAMR takes all
  of its allocations from the pool holds for the runtime's own data, not for
  linear memory under these build flags. (inferred)
- An interpreter compiles nothing, so there is no compiled code and no
  compiler transient. (source)

Gap: this host cannot add memory it did not allocate through Redis to
`used_memory` (G3, a module API change); issue 68's options are what the
module can do itself. The core can count linear memory in tree.

### Fault handling

This host:

- `config.signals_based_traps(false)` ([`wasm/src/host.rs`](src/host.rs) line
  357) gives guests explicit bounds checks, because `CONFIG SET
  crash-log-enabled` replaces a module's fault handlers on Linux (H11, G7).
  The out-of-bounds test traps and the server serves on. (measured) The checks
  cost 18% on the sieve. (measured earlier)

Core branch:

- With `OS_ENABLE_HW_BOUND_CHECK` (see Memory accounting) and unaligned access
  supported (`deps/wamr/core/config.h` lines 266 to 272), the classic
  interpreter does no software bounds check on guest loads and stores
  (`deps/wamr/core/iwasm/interpreter/wasm_interp_classic.c` lines 51 to 95). An
  access past the accessible pages faults, and WAMR's `SIGSEGV`/`SIGBUS`
  handler turns it into a trap. (source)
- WAMR installs that handler when the engine initializes, after Redis's
  crash handler (`initServer` calls `setupSignalHandlers` at `src/server.c`
  line 3025, which reaches `setupSigSegvHandler` through `src/debug.c` line
  2631, and `functionsInit` at line 3252), and passes faults outside wasm to
  the previous handler
  (`deps/wamr/core/shared/platform/common/posix/posix_thread.c` lines 597 to
  715). Its native stack guard uses the same signals. (source)
- `CONFIG SET crash-log-enabled` calls `setupSigSegvHandler` or
  `removeSigSegvHandlers` (`src/config.c` lines 2765 to 2771), which set
  `SIGSEGV` and `SIGBUS` without passing anything on (`src/debug.c` lines
  2641 to 2687). (source)
- After that, a guest's out-of-bounds load or store would reach Redis's crash
  handler or the default action instead of trapping, and the server would
  exit: the problem H11 found for wasmtime here. WAMR uses these POSIX
  handlers on macOS too (`deps/wamr/core/shared/platform/darwin/
  shared_platform.cmake` includes the POSIX sources), so unlike this host the
  problem would not be Linux only. (inferred) The branch's
  `bad_ptr` test only passes a bad pointer to a host import, which WAMR checks
  in software (`deps/wamr/core/iwasm/common/wasm_memory.c` lines 1003 to
  1048); no test makes a guest-side access. (untested)

Gap: G7 needs a module API change for this host. The core can coordinate its
own handlers in tree.

### Supported Redis versions and deployment

This host:

- A module for stock Redis 7.2 and later (not 7.0, S11). Today's runs used
  8.10.2; the full suite has also passed on 8.0.2 on Linux, and a smoke test
  on 7.2, 7.4, 8.0, 8.2 and 8.10 (measured earlier). CI runs Ubuntu with
  8.10.2 ([CI.md](CI.md)). Replicas need the module too.

Core branch:

- A Redis branch at 8.9.241 (`src/version.h`). WASM is built only with
  `BUILD_WASM=yes` (`src/Makefile` line 42, default `no`), which builds WAMR
  with cmake for darwin or linux on x86, x86-64 or AArch64 (`deps/Makefile`
  lines 141 to 175). Every server that loads WASM libraries needs that build.
  Sentinel rejects `loadextension` (`src/config.c` lines 594 to 605). (source)

Gap: by design.

### Toolchain and binary size

This host:

- Release dylib 12,633,112 bytes (12.0 MiB), unstripped (S6), with wasmtime
  36.0.19 and Cranelift. Components: hello 37,686 bytes, the benchmark guest
  41,265 bytes, the JavaScript guest 11,169,758 bytes (10.7 MiB, built by the
  test run). (measured)
- Rust guests need wasm-tools 1.236 or the `componentize` example;
  JavaScript guests need componentize-js 0.18.5 and jco 1.14.0. (source)

Core branch:

- WAMR 2.4.5 is built as a static library with only the classic
  interpreter, instruction metering, and no JIT, AOT, WASI, SIMD or reference
  types (`deps/Makefile` lines 156 to 175), linked into `redis-server`. Its
  size was not measured. (source)
- Committed guest binaries: GCRA 3,674 bytes, the C example 2,733, C++ 2,524,
  Rust 4,710, AssemblyScript 3,627 and TinyGo 16,057 (file sizes in the tree).
  Extensions build with Clang and a wasm32 linker (`extensions/README.md`).
  (source)
- How fast its interpreter runs these workloads against this host's compiled
  code was not measured; the design document calls the classic interpreter
  WAMR's slowest mode. (untested)

Gap: by design.

## Epoch interruption cost

Epoch interruption is what gives `BUSY`, `WASM.KILL` and `UNKILLABLE` their
Lua behaviour here (#86, 2026-10-27 comment). Its price was measured by
building the host twice.

The change for the second build, made locally and not committed: in
`Host::new` in [`wasm/src/host.rs`](src/host.rs), line 359,

```diff
-        config.epoch_interruption(true);
+        config.epoch_interruption(false);
```

Nothing else changed. The store still sets a deadline and a callback, which
nothing reads without the instrumentation, and the ticker thread still runs.
That build cannot yield, so a guest that spins holds the server and
`WASM.KILL` cannot stop it (inferred; its tests were not run).

Method: `wasm/bench/run.sh` unchanged, `PORT=6700`, 50 clients, on the
machine and versions in Pins, 2026-10-27. Runs were interleaved off, on, off,
on, off, on, after one discarded warm-up run. Each variant was rebuilt from
source between runs and gave the same dylib each time: sha256 `efec9d1f...`
with epochs, `4f8069d6...` without. Load averages at the start of runs were
3.7 to 12.5, so the machine was not idle. The per-run tables are in
`wasm/bench/results/2026-10-27/epoch-*.md`.

Server time per call from `INFO commandstats`, three runs each, in µs:

| function | epochs on | epochs off | on / off, means | Lua, all six runs |
|---|---|---|---|---|
| `incr_twice` | 3.54 to 3.71 | 3.44 to 3.60 | 1.02 | 1.77 to 1.92 |
| `nested` | 1.83 to 1.97 | 1.81 to 1.91 | 1.01 | 2.05 to 2.29 |
| `adler32` | 64.74 to 64.97 | 64.15 to 65.34 | 1.00 | 755.94 to 781.99 |
| `median` | 201.41 to 206.61 | 194.52 to 198.84 | 1.04 | 290.84 to 298.69 |
| `lrange` | 165.17 to 169.71 | 158.43 to 165.46 | 1.03 | 68.28 to 70.61 |
| `median_flat` | 144.87 to 147.84 | 138.34 to 140.07 | 1.05 | 289.47 to 307.82 |
| `lrange_flat` | 91.55 to 92.01 | 90.07 to 91.57 | 1.01 | 68.70 to 71.07 |
| `primes` | 38.96 to 41.77 | 17.08 to 18.06 | 2.27 | 485.67 to 511.48 |

What it shows (measured):

- `primes`, a sieve, runs 2.2 to 2.4 times as long with epochs; the checks are
  56% of its time.
- `median`, `median_flat` and `lrange` are 3 to 5% slower with epochs. The
  ranges do not overlap for `median` and `median_flat`, but the Lua column
  moved by 3 to 8% between runs, so this is near the noise.
- `incr_twice`, `nested`, `adler32` and `lrange_flat` show no difference
  beyond their spread: calls into Redis and replies cost the same.

Why (inferred, the generated code was not inspected): wasmtime adds a check of
the engine's epoch against the store's deadline at function entries and loop
back edges. The sieve's inner loop is a store, an add and a compare, so the
check is a large share of it, and its out-of-line call may also cost the loop
registers. `adler32`'s loop does two remainders per byte, which hide it.

The yields themselves are probably not in these numbers: every benchmark call
is under 1 ms against a 10 ms tick, so few calls reach a deadline (inferred). A
function that runs longer pays one `RedisModule_Yield` per tick on top (not
measured).

So in this host the price of Lua's `BUSY` and kill behaviour is 0 to 5% on
call and reply work and up to 2.3 times on a tight compute loop, on this
machine. The core branch pays for its instruction limit with a counter on
every interpreted instruction (source; the cost is not measured). Remedies,
such as other wasmtime interruption mechanisms, are not chosen here.

## Gaps by who can close them

Need a module API change, so a change to Redis core:

- G1: registering an engine with `FUNCTION`, so `FCALL`, `FUNCTION LIST`,
  `DUMP` and `KILL` see wasm libraries. The core branch has this as a built-in
  engine.
- G2: creating commands after load, for commands per function as the branch's
  `create_command` provides.
- G3: counting memory a module did not allocate through Redis.
- G4: AOF rewrite commands outside keys, which the branch's
  `rewriteFunctions` covers for its libraries.
- G5 and G6: `MISCONF` and `NOREPLICAS` state, and command flags such as
  `may_replicate`.
- G7: fault handlers that Redis keeps, for signal-based bounds checks.

The module can close itself, or already has:

- Routing before 8.4 (G8), yielding and kill, Lua's flags, `pcall` and the ACL
  check import, replies by client protocol: done.
- Memory: issue 68's options (which allocator counts the compiler, counting
  linear memory through wasmtime's `Config` or a `MemoryCreator`, budgets over
  `INFO wasm`).
- A value type for guest data, if wanted, as a module data type.
- Checking that a function uses only its declared keys.

The core branch can close in tree, for its own side: script flags, `BUSY` and
kill (with an interpreter change, by its own document), an ACL check import
and an ACL log context, keeping its fault handlers through `CONFIG SET
crash-log-enabled`, and counting linear memory.

## What a run of the core branch would add

If the owner approves building and running the branch, these would turn its
source and inferred claims into measurements:

- Build with `BUILD_WASM=yes` and run its three WASM test files
  (`tests/unit/functions-wasm.tcl`, `wasm-commands.tcl`, `gcra-wasm.tcl`).
- Port the benchmark functions to its ABI (C or `no_std` Rust) and run
  `run.sh`'s comparison with Lua: per-call cost (`incr_twice`, `nested`),
  replies (`lrange`, `median`) and compute (`adler32`, `primes`); and again
  with WAMR built without `WAMR_BUILD_INSTRUCTION_METERING`, its counterpart of
  the epoch measurement.
- Time a spinning function to the 100 million instruction limit and watch
  another client and `FUNCTION KILL` meanwhile.
- `INFO memory` at startup with and without `BUILD_WASM` (the pool), and while
  a guest grows to 8 MiB (linear memory).
- A guest out-of-bounds load before and after `CONFIG SET crash-log-enabled
  no` and `yes`.
- A trap followed by a call, to see the kept instance's state; `ACL LOG` after
  a denied guest command; RDB load time with a larger module; a replica built
  without `BUILD_WASM`; `FUNCTION FLUSH ASYNC` with a WASM library loaded.

## Not verified

- Anything on the core branch at run time, including that it builds.
- Relative speed of the two runtimes; the branch has no benchmark.
- That WAMR's 8 GiB mappings and the 64 MiB pool behave in `INFO` as read.
- The epoch cost on Linux, on x86-64, or under jemalloc; and why the sieve
  pays so much (no look at Cranelift's output).
- Today's runs covered Redis 8.10.2 on macOS only; other versions are from
  TRACKING.md.
