# redis-module-wasm

A Redis module that runs WebAssembly components as functions, the way
`FUNCTION LOAD` and `FCALL` run Lua. It is built on `redis-module` and
[wasmtime](https://wasmtime.dev) with the component model.

This is an early prototype. It needs Redis 7.2 or later, and has been run on
7.2, 7.4, 8.0, 8.2 and 8.10.

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
RDB or AOF replaces the libraries: it compiles each saved component again and
runs no guest code, so a library's first call creates its instance. An RDB
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
or `MODULE LOADEX <path> CONFIG wasm.persist no`) leaves persistence off: the
RDB does not keep the libraries, loading an RDB leaves them alone, and
`MODULE UNLOAD` works. `WASM.LOAD` and `WASM.DELETE` still replicate.

## Long-running functions

A running function yields to Redis every 10 ms. Once it has run longer than
`busy-reply-threshold`, Redis answers other clients with `-BUSY a wasm function
is running. You can only call WASM.KILL or SHUTDOWN NOSAVE.`, as it does for a
long Lua script. `WASM.KILL` stops the function, which fails with `ERR function
'<name>' failed: killed by WASM.KILL`, unless it has already called a write
command: then `WASM.KILL` replies `UNKILLABLE`, so the function's writes are not
left half done, and `SHUTDOWN NOSAVE` is the way out. With no function running,
`WASM.KILL` replies `NOTBUSY`.

## Memory

`wasm.max-memory` (64 MiB by default; `CONFIG SET wasm.max-memory 128mb`)
limits the linear memory of each instance. A function that grows past it
fails, and its instance is dropped. Linear memory and compiled code are
allocated outside the Redis allocator, so `used_memory` and `maxmemory` do not
count them; `INFO wasm` reports them:

```text
wasm_libraries, wasm_functions        what is loaded
wasm_component_bytes                  the components' bytes, which the RDB keeps
wasm_code_bytes                       compiled code
wasm_instances                        live instances, kept or running
wasm_instance_memory_bytes            their linear memory
wasm_max_memory                       the limit
```

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

The component is 11 MB, and loading it compiles all of it: one to two seconds
in a release build of the module (11.7 s without parallel compilation). A Rust
library of the same functions is 23 KB and loads in milliseconds.

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

- Loading an RDB compiles each library before the server serves (H17).
