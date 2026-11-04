# redis-module-wasm

A Redis module that runs WebAssembly components as functions, the way
`FUNCTION LOAD` and `FCALL` run Lua. It is built on `redis-module` and
[wasmtime](https://wasmtime.dev) with the component model.

This is an early prototype. It needs Redis 7.2 or later, and has been run on
7.2, 7.4, 8.0, 8.2 and 8.10.

[MODULE-VS-CORE.md](MODULE-VS-CORE.md) sets out what running wasm as a module
can and cannot do, against building it into Redis as Lua is.

## Demo

[`demo/demo.sh`](demo/demo.sh) builds the module and the example guests and
walks through the commands on throwaway servers: loading a library, calls
with keys and into Redis, a read-only refusal, a guest trap the server
survives, `WASM.KILL` of a busy function, `WASM.STATS` and `INFO wasm`,
libraries kept through `DEBUG RELOAD` and a restart, a replica that gets
them, the JavaScript library when Node.js is installed, and a function that
cannot be killed once it has written. From the repository root:

```bash
wasm/demo/demo.sh
```

It needs `redis-server` and `redis-cli` on the `PATH` (or `REDIS_SERVER` and
`REDIS_CLI`), Rust with the `wasm32-unknown-unknown` target, and Node.js for
the JavaScript part. It seeds a missing `Cargo.lock` from
[`ci/locks`](ci/locks), takes free ports from 7400 up, works in
`target/wasm-demo` and stops its servers when it exits, so it can run again
at once. `DEMO_STEP=1` waits for Enter before each section, for presenting,
and `DEMO_PROFILE=release` builds the module with `--release`, which compiles
the JavaScript library faster. The first build compiles wasmtime and takes a
few minutes.

## Commands

```text
WASM.LOAD <library> <component> [REPLACE]       load a library; replies with its functions
WASM.CALL <function> <numkeys> [key ...] [arg ...]
WASM.CALL_RO <function> <numkeys> [key ...] [arg ...]   only functions with no-writes
WASM.LIST                                       [[library, [[function, [flag ...]] ...]] ...]
WASM.DELETE <library>
WASM.KILL                                       stop the running function, as FUNCTION KILL does
WASM.DUMP                                       every library, as a payload
WASM.RESTORE <payload> [FLUSH|APPEND|REPLACE]   load a payload's libraries, all or none
WASM.FLUSH                                      delete every library
WASM.STATS                                      the running function, and counts
```

Function names are global across libraries, as with `FUNCTION`. The commands
are in the `@scripting` ACL category, as `FUNCTION` and `FCALL` are.

`WASM.CALL` declares its keys as `FCALL` does: the `numkeys` keys after the
function name. `COMMAND GETKEYS`, ACL key patterns and cluster routing use
them, so in a cluster a call is redirected to the node that serves its keys,
and keys in different slots get `CROSSSLOT`. As in Lua, a function should use
only the keys it is given; the host does not check that yet. Each node has its
own libraries: in a cluster, load a library on every primary, as with
`FUNCTION LOAD`; replicas get it from their primary.

Each library keeps one instance between calls, so a guest's memory and globals
persist from one call to the next; creating an instance per call made a call
about eight times slower. A call that fails, for example one that traps or is
killed, drops the instance, and the next call starts a new one; replacing or
deleting the library drops it too. All callers of a library share its
instance, so a guest should not keep data from one call that another caller
must not see.

`WASM.DUMP`, `WASM.RESTORE` and `WASM.FLUSH` work as `FUNCTION DUMP`,
`FUNCTION RESTORE` and `FUNCTION FLUSH` do: a restore loads all the payload's
libraries or none, `APPEND` (the default) refuses a library that exists,
`REPLACE` replaces it, and `FLUSH` deletes every library first. A restore
lists each library's functions from its component again, so the payload holds
only names and components, with a checksum.

## Function flags

A function has the flags of a Lua function, and they mean what they mean for
Lua; the error replies have Lua's codes and say "function" where Lua's say
"script":

- `no-writes`: the function does not write. It may run from `WASM.CALL_RO`, on
  a read-only replica and over `maxmemory`, and a write command it calls fails,
  as does a `PUBLISH`, `SPUBLISH` or `PFCOUNT`.
- `allow-oom`: a function that writes may start over `maxmemory`, and the
  commands it calls are not checked against it.
- `allow-stale`: the function may run on a replica that has lost its primary
  while `replica-serve-stale-data` is `no`.
- `no-cluster`: the function does not run on a cluster node.
- `allow-cross-slot-keys`: on a cluster node, the commands the function calls
  may use keys in any slot the node serves. Without it, they must use one slot:
  the declared keys' slot, or the first one used.

A function without flags may write, so like a Lua function it does not run on
a read-only replica, from `WASM.CALL_RO`, or over `maxmemory`. Two of Lua's
checks happen later: Redis offers modules no way to ask whether writes are
refused for a disk error (`MISCONF`) or for too few replicas (`NOREPLICAS`), so
such a function starts, and its first write fails with that error.

## Loading

`WASM.LOAD` compiles the component on a thread while its client waits, so the
server keeps serving other clients; once it has compiled, the library is
instantiated and added on the main thread, and the client gets the reply.
Inside `MULTI`, a script or an AOF being loaded, where a client cannot block,
it compiles on the main thread. If two loads of the same library overlap, the
second to finish fails as if it had come second, unless it has `REPLACE`.

## Persistence and replication

The RDB keeps the libraries, as Redis does for `FUNCTION` libraries, so they
come back after a restart, `DEBUG RELOAD` or a replica's full sync. Loading an
RDB or AOF replaces the libraries and runs no guest code: the saved components
compile again on a thread while the server serves, a call to a library still
compiling waits for it, and a library's first call creates its instance. With
an 11 MB JavaScript library saved, a server was ready in 0.12 s instead of
1.6 s. An RDB
saved with no libraries has nothing from the module, and a server without the
module can load it.

`WASM.LOAD` and `WASM.DELETE` replicate as they are, and go to the AOF. On a
replica, a replicated `WASM.LOAD` compiles on a thread while the link to the
primary waits, so the replica keeps serving and the commands after it still
run in order. A function's writes replicate as its effects, as a Lua
function's do.

With `aof-use-rdb-preamble no`, a rewritten AOF does not have the libraries;
the default, `yes`, does.

The libraries are aux data, which needs a data type, and Redis does not unload
a module with one. `wasm.persist no` (set at load time: `--wasm.persist no`,
`MODULE LOAD <path> persist no`, or
`MODULE LOADEX <path> CONFIG wasm.persist no`) leaves persistence off: the
RDB does not keep the libraries, loading an RDB leaves them alone, and
`MODULE UNLOAD` works. `WASM.LOAD` and `WASM.DELETE` still replicate.

## Long-running functions

A running function yields to Redis every 10 ms. Once it has run longer than
`busy-reply-threshold`, Redis answers other clients with `-BUSY a wasm function
is running. You can only call WASM.KILL or SHUTDOWN NOSAVE.`, as it does for a
long Lua script. `WASM.KILL` stops the function, which fails with `ERR function
'<name>' failed: killed by WASM.KILL`, unless it has already called a write
command: then `WASM.KILL` replies `UNKILLABLE`, so the function's writes are not
left half done, and `SHUTDOWN NOSAVE` is the way out. On a primary with
replicas use `SHUTDOWN NOSAVE NOW`: without `NOW`, Redis waits for lagging
replicas and checks that wait's timeout only between commands, so it can wait
for good while a function or a Lua script yields. With no function running,
`WASM.KILL` replies `NOTBUSY`. `WASM.STATS`, like `FUNCTION STATS`, also
answers while a function runs: it names the running function and how long it
has run.

## The command's context

A guest's imports (`call`, `pcall`, `acl-check`, `log` and the rest) and the
yield every 10 ms all need the context of the `WASM.CALL` command running the
guest. The SDK hands a command its context only as a borrow for the length of
its callback, while wasmtime needs a store's data to be `'static`, so the store
holds a raw pointer to the context, and only while a guest runs:

- Each guest run, a call or an instantiation (when a library loads, and when a
  call has no kept instance to reuse), is an invocation that puts the
  command's context in the store and takes it out again when the run ends,
  however it ends: the function returns, it fails, the guest traps, or a host
  import panics. Between runs the store holds no context, including the
  instance a library keeps for its next call, which may come from another
  client and user.
- An import that ran outside an invocation would stop with a panic rather than
  use an old context; none can, since the guest runs synchronously inside one.
- A call that comes back into the host while a guest runs, through a command
  that runs `WASM.CALL` again, runs on another store (a new instance of the
  same library, whose kept instance is taken, or the other library's
  instance), leaving the outer run's context in place. A guest cannot run
  `WASM.*` commands itself, as a Lua script cannot run `FUNCTION` or `FCALL`.
- While a function yields, Redis runs only commands that allow it.
  `WASM.CALL` does not; another module's command that does and runs
  `WASM.CALL` is a call coming back into the host, as above.

The pointer and its safety argument are in `src/host.rs` ("The context
pointer"), next to the `invoke` function that sets it and the `Invocation`
that clears it.

## Memory

`wasm.max-memory` (64 MiB by default; `CONFIG SET wasm.max-memory 128mb`)
limits the linear memory of each instance. A function that grows past it
fails, and its instance is dropped. Linear memory and compiled code are
allocated outside the Redis allocator, so `used_memory` and `maxmemory` do not
count them; `INFO wasm` reports them:

```text
wasm_libraries, wasm_functions        what is loaded
wasm_component_bytes                  the components' bytes, which the RDB keeps
wasm_code_bytes                       full images of registered ready libraries
wasm_compiling                        libraries still compiling after an RDB load
wasm_instances                        live instances, kept or running
wasm_instance_memory_bytes            their linear memory
wasm_max_memory                       the limit
wasm_guest_memory_budget              aggregate guest-byte limit
wasm_guest_memory_used                live and pending guest-byte reservations
wasm_code_budget                      aggregate compiled-image limit
wasm_code_used                        live and pending page-rounded image reservations
```

`wasm.guest-memory-budget` and `wasm.code-budget` each default to `1gb`
(1,073,741,824 bytes). They accept Redis memory units at module load and in
`CONFIG SET`, for example `CONFIG SET wasm.guest-memory-budget 256mb`.
Zero is a hard cap on new positive reservations. Lowering a budget below its
current use preserves valid instances and images, including guest globals;
further positive admission is refused until ownership ends or the limit rises.
Limit changes and background compiler admission use the same lock.

The guest limiter reserves the requested additional committed wasm pages
before instantiation or growth, over every live store and every memory in
that store. The per-instance `wasm.max-memory` still applies. A denied growth
traps its call, dropping that call's instance as other traps do. Other valid
instances remain intact. A native growth failure rolls back its own delta;
failed initial allocation can retain a provisional charge until its entire
failed store is destroyed. Shared wasm memory is rejected by the pinned
engine's disabled threads feature, since Wasmtime's limiter excludes it.

Compiled-image admission counts the complete verified artifact file, including
metadata and data sections, rounded up to the OS page size. It occurs before
the host maps the artifact. Each independently deserialized mapping is charged
once; prepared calls, pre-instantiated code, kept stores and in-flight stores
share its owner. A deleted library can therefore have charged owners still
finishing. `code_bytes` describes registered libraries' exact image spans;
`code_used` also includes pending and still-live owners and page rounding.
Both resource leases outlive full store destruction, including Wasmtime's
internal code references. `INFO` uses `-1` when a budget snapshot cannot be
read during a crash report.

Replacement needs old-plus-new image and staging-instance headroom. A failed
`WASM.LOAD REPLACE` or `WASM.RESTORE` retains the old libraries; even RESTORE
FLUSH admits and stages all candidates before committing. Traps, failed or
cancelled loads, deletion, flush and successful unload release resources when
their final owners end. Refused unload retains their owners and the compiler
quarantine. There is no silent recycling of valid guest instances.

RDB restoration admits images through the same background compiler path. If a
saved image cannot fit, its source/function metadata remains listed, its code
slot becomes failed, and calls report the budget error. Other admitted
libraries remain usable. Increasing the budget and restoring or reloading the
saved libraries retries admission. Initial guest memory is admitted on the
first call. AOF and replication commands also use normal admission. Redis can
continue a command-only AOF or replication stream after a refused module
command: other key commands apply, but that library is absent or its previous
version remains. A replica can acknowledge later commands even when a library
load failed. Provision compatible node-local limits, inspect replay errors
and compare library readiness. After repairing limits, retry from saved
component source/dump, or reload/full-sync the RDB as appropriate. `PING`, link
status and replication acknowledgements alone do not establish library parity.

These counters distinguish logical guest commitment, mapped image extent and
Redis-accounted lasting heap. They do not measure resident physical pages or
RSS: demand paging, CoW, virtual reservations, allocator retention and file
cache can differ. Keep a separate server/process memory ceiling and the
compiler allowance below; Redis `maxmemory` does not cover these mappings.

The runtime's lasting heap, including metadata, stores and saved component
bytes, uses Redis's allocator and remains counted in `used_memory`.
Compilation runs in a System-allocated compiler process owned by a small
bounded launcher, with address-space and CPU limits and a wall timeout. Only
one compiler runs at a time; each operation owns these two processes. The
launcher waits for its own compiler child, since Redis's global child reaper
can otherwise consume an unrelated helper's exit status during BGSAVE or an
AOF rewrite. A private live challenge opens the compiler gate only after the
host arms a stable process-exit watch. It does not use Redis allocation for compiler
scratch. Host and helper share the exact Wasmtime build/configuration identity;
only complete, verified output from the owned helper can be deserialized.
There is no serialized-code import or compiler artifact cache API.

Install `redis-wasm-compiler` next to the module library. `cargo build -p
redis-module-wasm` builds both. Alternatively set the immutable
`wasm.compiler-path` to an absolute path to the trusted helper from the same
build. This executable is native code with the server's OS permissions; it
must come from your module deployment, not from a library caller. A missing,
incompatible, failed or timed-out helper fails the load without replacing an
existing library.

Compiler settings are immutable module configurations:

| Setting | Default | Meaning |
|---|---|---|
| `wasm.compiler-memory` | 2 GiB | Additional virtual address space above the fresh helper's trusted startup VM |
| `wasm.compiler-cpu-seconds` | 60 | Process CPU hard limit |
| `wasm.compiler-wall-seconds` | 120 | Pipe/compile wall deadline |
| `wasm.compiler-queue` | 8 | Waiting operations, in addition to the one active operation |
| `wasm.compiler-path` | empty | Empty selects the sibling executable; an override must be absolute |

Admission occurs before spooling source or starting an operation worker. A full
queue fails immediately. A restore or RDB batch owns one reservation and
compiles its components sequentially through the same path. Individual source
components are limited to 64 MiB and serialized artifacts to 512 MiB. These are
compiler bounds. The aggregate guest/image budgets above apply independently.

On macOS, the startup VM includes a large shared-cache reservation, so the hard
address-space ceiling is startup VM plus the configured allowance. The helper
disables NanoZone before startup and sets its limit before reading component
bytes or constructing the compiler. The reported ceiling is a virtual-memory
limit, not an RSS claim. `INFO wasm` adds `compiler_active`, `compiler_queued`,
`compiler_completed`, `compiler_failed`, `compiler_baseline_vm`,
`compiler_ceiling_vm`, `compiler_cpu_ms` and `compiler_uncertain`; failures also report the configured
resource limits and helper status in the server log. [TRACKING.md](TRACKING.md)
retains the earlier in-process measurements and policy assessment.

Cancellation is an explicit private message, rather than relying on pipe EOF:
Redis fork children can inherit open descriptors. Normal timeouts cancel and
reap the compiler through its launcher. If an external actor stops the entire
compiler process and kills its launcher, the stopped watchdog cannot run.
Unproven cleanup therefore retains the admission, refuses new compiler work
and refuses module unload. Existing libraries and the epoch ticker remain
available. A later admission or unload request retries the exit proof and
clears quarantine only after the processes are known to have exited.

As for a Lua function without flags, a function does not start while Redis is
over `maxmemory`, and the commands it calls follow `deny-oom`.

## Writing a library

A library is a component that implements the `library` world in
[`wit/redis.wit`](wit/redis.wit). It exports `functions`, which lists the
functions with their flags, and `invoke`, which runs one with its keys and
arguments. Guest code may not call Redis while its library loads. It imports:

- `call(command, args)`: runs a Redis command, like `redis.call`. An error
  reply fails the function, which replies with that error followed by
  `function: <name>.`, as Lua follows it with the script's name.
- `pcall(command, args)`: the same, like `redis.pcall`: an error reply comes
  back to the function as an `error` node.
- `call-flat` and `pcall-flat`: the same, with the reply flat: nodes whose
  strings are spans of one buffer. Moving a reply with many strings this way
  takes two allocations in the guest instead of one per string; for 1000 items
  it halved the time (see the benchmark).
- `acl-check-command(command, args)`: whether the user running the function may
  run the command, like `redis.acl_check_cmd`.
- `set-resp(version)`: the protocol of the replies `call` and `pcall` return,
  like `redis.setresp`. Each call starts with RESP3, so the function sees each
  reply's type; Lua starts with RESP2.
- `log(level, message)`: writes to the server log.

Commands run in script mode as the caller's ACL user, with their effects
replicated, as from a Lua function. A function that `call` fails, like one that
traps or is killed, drops its library's kept instance.

Replies cross the boundary as a list of nodes in pre-order, since WIT has no
recursive types: `array(n)`, `set(n)` and `map(n)` are followed by their
elements. A reply from `call` keeps its type, so a function can pass it through
unchanged.

In Rust, [`guest-sdk`](guest-sdk) (crate `redis-wasm-guest`) turns node
lists into `Value`s and back and wraps `call`, `pcall` and `log` to take and
return them, so a library replies with
`Value::Array(vec![first, second]).into_nodes()` rather than building nodes by
hand. A library's bindings share its types through wit-bindgen's `with` option
(see the example's `generate!`). Its unit tests run on the host:
`cargo test --manifest-path wasm/guest-sdk/Cargo.toml`.

[`guests/hello`](guests/hello) is a library in Rust, built on `guest-sdk`. From the repository root,
build the module and the guest, and turn the guest into a component with
[wasm-tools](https://github.com/bytecodealliance/wasm-tools) 1.236, the release
wasmtime 36 reads:

```bash
cargo build -p redis-module-wasm
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown --manifest-path wasm/guests/hello/Cargo.toml
wasm-tools component new wasm/guests/hello/target/wasm32-unknown-unknown/release/hello_guest.wasm -o hello.wasm
```

Without wasm-tools, the `componentize` example does the same:
`cargo run -p redis-module-wasm --example componentize -- <module.wasm> <component.wasm>`.

Then load it and call it (the module is `libredis_module_wasm.so` on Linux):

```bash
redis-server --loadmodule target/debug/libredis_module_wasm.dylib
redis-cli -x WASM.LOAD hello < hello.wasm
redis-cli WASM.CALL hello 0 redis
```

Guests get no WASI imports: no clocks, randomness, files or network.

### In JavaScript

[`guests/js`](guests/js) is the same library in JavaScript, built with
[componentize-js](https://github.com/bytecodealliance/ComponentizeJS), which
embeds a JavaScript engine in the component. With Node.js installed:

```bash
cd wasm/guests/js
npm ci
node build.mjs library.wasm
redis-cli -x WASM.LOAD js < library.wasm
```

`package.json` pins componentize-js 0.18.5 and jco 1.14.0, releases from the
same weeks as wasmtime 36: newer jco releases bring a WASI shim that
componentize-js 0.18 cannot use. `build.mjs` disables the WASI features the
library does not use, so the component imports only the host's `redis`
interface.

The component is 11 MB, and loading it compiles all of it. Earlier in-process
load timings in [TRACKING.md](TRACKING.md) predate the bounded helper and do
not establish its end-to-end cost. A Rust library of the same functions is
23 KB.

## Benchmark

[`bench/`](bench) compares the same functions in wasm and in Lua: calls into
Redis, replies, a checksum, a sort and a sieve. Run `wasm/bench/run.sh`; the
latest numbers are in [TRACKING.md](TRACKING.md).

## Tests

`cargo test -p redis-module-wasm` builds the example guests, starts a
`redis-server` with the module, and calls each function. It needs
`redis-server` on the `PATH` and the `wasm32-unknown-unknown` target. The
JavaScript guest's test runs when Node.js is installed and is skipped
otherwise; the first run installs its dependencies with `npm ci`.

Debug builds optimize Cranelift and the other compiler crates (see the
workspace `Cargo.toml`); unoptimized, loading the JavaScript component took
28 s.

## Not done yet

[TRACKING.md](TRACKING.md) has every open item, with IDs and what done means. The main ones:

- Choosing whether wasmtime's heap counts in `used_memory` (H13).


## Continuous integration

[The branch CI guide](CI.md) maps pinned host/guest inputs to checks, caches,
timeouts and retained diagnostics. JavaScript tests are mandatory in CI via
`WASM_TEST_REQUIRE_JS=1`; ordinary local test runs can skip them without Node.
The experiment workflow runs on pushes and PRs to `experiment/wasm-host`.
