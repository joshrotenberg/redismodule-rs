# redis-module-wasm

A Redis module that runs WebAssembly components as functions, the way
`FUNCTION LOAD` and `FCALL` run Lua. It is built on `redis-module` and
[wasmtime](https://wasmtime.dev) with the component model.

This is an early prototype: libraries live in memory only, and there is no time
limit on a running function yet.

## Commands

```text
WASM.LOAD <library> <component> [REPLACE]       load a library; replies with its functions
WASM.CALL <function> <numkeys> [key ...] [arg ...]
WASM.LIST                                       [[library, [function ...]] ...]
WASM.DELETE <library>
```

Function names are global across libraries, as with `FUNCTION`. Each call runs
in a fresh instance of its library, so calls share no guest state.

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

Then load it and call it (the module is `libredis_module_wasm.so` on Linux):

```bash
redis-server --loadmodule target/debug/libredis_module_wasm.dylib
redis-cli -x WASM.LOAD hello < hello.wasm
redis-cli WASM.CALL hello 0 redis
```

Guests get no WASI imports: no clocks, randomness, files or network.

## Tests

`cargo test -p redis-module-wasm` builds the example guest, encodes it as a
component with `wit-component`, starts a `redis-server` with the module, and
calls each function. It needs `redis-server` on the `PATH` and the
`wasm32-unknown-unknown` target.

## Not done yet

- A time limit, with `-BUSY` replies and a kill command.
- Function flags, read-only calls, and ACL and RESP helpers for guests.
- Saving libraries in RDB and AOF, and replicating `WASM.LOAD`.
- Memory limits per instance and an `INFO` section.
- Key specs on `WASM.CALL`, so a cluster can route it by its keys.
