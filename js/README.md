# redis-module-js experiment

`redis-module-js` is a native Redis module experiment. It keeps QuickJS out of
the `redis-module` SDK crate and exposes one synchronous command:

```text
JS.PROJECT <source-key> <destination-key> <function>
```

The source key must be a Redis string containing a UTF-8 JSON object. The
UTF-8 JavaScript function expression receives an owned parsed snapshot and must
return a nonempty flat object whose enumerable string-keyed fields all have
string values. The module validates the complete result, then merges its fields
with one `HSET`; unrelated destination hash fields remain unchanged. The reply
is the integer returned by `HSET`. Source and destination must be different.

`GET` and `HSET` run as the caller with Redis command and key ACL checks. Both
keys are declared for routing and `COMMAND GETKEYS`. The successful `HSET`
effect is what reaches replicas and AOF, so JavaScript is not re-executed during
replay. Any input, script, result, resource or execution failure happens before
the write and leaves the destination unchanged. A destination of the wrong type
returns Redis's normal `HSET` error.

## Build and load

Use the repository toolchain (Rust 1.94.0; package MSRV 1.87):

```sh
cargo build -p redis-module-js
cargo test -p redis-module-js
redis-server --loadmodule target/debug/libredis_module_js.so
```

On macOS the module suffix is `.dylib`. The first milestone supports Redis 7.2
and current Redis.

Run the native tests against the `redis-server` on `PATH`, or select another
server binary explicitly:

```sh
cargo build -p redis-module-js --release
cargo test -p redis-module-js --release

cargo build -p redis-module-js --no-default-features --features min-redis-compatibility-version-7-2
TEST_REDIS_SERVER_BIN=/path/to/redis-7.2/src/redis-server \
  cargo test -p redis-module-js --no-default-features --features min-redis-compatibility-version-7-2
```

Build the cdylib with the same profile and compatibility features immediately
before its native tests. `cargo test` does not itself produce the loadable
module artifact. The native functional cases serialize their Redis instances
because concurrent process startup can consume the unchanged 25 ms projection
budget on shared CI runners. The host-only accounting tests run separately.

## Order summary example

The runnable function is in [`examples/order-summary.js`](examples/order-summary.js):

```sh
redis-cli SET order:1 '{"customer":"Ada","items":[{"quantity":2,"price_cents":1250},{"quantity":1,"price_cents":500}]}'
redis-cli HSET order:1:summary retained yes
redis-cli -x JS.PROJECT order:1 order:1:summary < js/examples/order-summary.js
redis-cli HGETALL order:1:summary
```

The result contains `customer`, `item_count` and `total_cents`, while `retained`
is preserved.

## Resource and accounting limits

Each invocation gets a fresh QuickJS runtime and context, with no module loader,
I/O bridge, Redis handle, futures support or pending-job execution. Limits are:

- 64 KiB JavaScript source
- 1 MiB input JSON and 1 MiB serialized output JSON
- 1024 output fields
- 8 MiB QuickJS heap, 256 KiB QuickJS stack and a 25 ms invocation deadline

This milestone deliberately uses QuickJS's bounded default libc allocator.
QuickJS allocations are therefore external to Redis `used_memory`. The heap,
stack and deadline are not a whole-process memory ceiling; Rust, Redis and
native library allocations are outside the QuickJS heap limit. Engine
interruption is cooperative, and the host checks the same deadline again after
output conversion, accounting and QuickJS cleanup before allowing `HSET`. The
`rust-alloc` rquickjs feature is not enabled because rquickjs documents that
`set_memory_limit` is a no-op with custom allocators. A fallible, budgeted Redis
allocator remains separate future work.

The next experiment is a reactive adapter that invokes the same pure snapshot
to projection boundary from keyspace or stream triggers. Persistent libraries,
background execution, general Redis host calls, production allocator policy
and script-defined data types are intentionally out of scope here.
