# redis-module-wasm

A Redis module that runs WebAssembly components as functions, the way
`FUNCTION LOAD` and `FCALL` run Lua. It is built on `redis-module` and
[wasmtime](https://wasmtime.dev) with the component model.

This is an early prototype.

## Commands

```text
WASM.LOAD <library> <component> [REPLACE]       load a library; replies with its functions
WASM.CALL <function> <numkeys> [key ...] [arg ...]
WASM.LIST                                       [[library, [function ...]] ...]
WASM.DELETE <library>
WASM.KILL                                       stop the running function, as FUNCTION KILL does
```

Function names are global across libraries, as with `FUNCTION`.

Each library keeps one instance between calls, so a guest's memory and globals
persist from one call to the next; creating an instance per call made a call
about eight times slower. A call that fails, for example one that traps or is
killed, drops the instance, and the next call starts a new one; replacing or
deleting the library drops it too. All callers of a library share its
instance, so a guest should not keep data from one call that another caller
must not see.

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

The libraries are aux data, which needs a data type, so Redis refuses
`MODULE UNLOAD` for this module. With `aof-use-rdb-preamble no`, a rewritten
AOF does not have the libraries; the default, `yes`, does.

## Long-running functions

A running function yields to Redis every 10 ms. Once it has run longer than
`busy-reply-threshold`, Redis answers other clients with `-BUSY a wasm function
is running. You can only call WASM.KILL or SHUTDOWN NOSAVE.`, as it does for a
long Lua script. `WASM.KILL` stops the function, which fails with `ERR function
'<name>' failed: killed by WASM.KILL`, unless it has already called a write
command: then `WASM.KILL` replies `UNKILLABLE`, so the function's writes are not
left half done, and `SHUTDOWN NOSAVE` is the way out. With no function running,
`WASM.KILL` replies `NOTBUSY`.

## Writing a library

A library is a component that implements the `library` world in
[`wit/redis.wit`](wit/redis.wit). It exports `functions`, which lists the
function names, and `invoke`, which runs one with its keys and arguments. It
imports:

- `call(command, args)`: runs a Redis command, like `redis.pcall` in a script.
  The call runs in script mode as the caller's ACL user, its effects are
  replicated, and an error comes back as a reply instead of failing the call.
- `log(level, message)`: writes to the server log.

Replies cross the boundary as a list of nodes in pre-order, since WIT has no
recursive types: `array(n)`, `set(n)` and `map(n)` are followed by their
elements. A reply from `call` keeps its type, so a function can pass it through
unchanged.

[`guests/hello`](guests/hello) is a library in Rust. From the repository root,
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

- Function flags, read-only calls, and ACL and RESP helpers for guests.
- Memory limits per instance and an `INFO` section.
- Key specs on `WASM.CALL`, so a cluster can route it by its keys.
