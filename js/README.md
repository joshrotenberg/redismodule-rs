# redis-module-js experiment

`redis-module-js` is a native Redis module experiment. It keeps QuickJS out of
the `redis-module` SDK crate and exposes a synchronous command plus ephemeral
reactive projections:

```text
JS.PROJECT <source-key> <destination-key> <function>
JS.WATCH <name> <source-key> <destination-key> <function>
JS.UNWATCH <name>
JS.INFO <name>
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

`JS.WATCH` registers without reading or projecting its source. Later source
mutations, deletion, expiry and eviction run the function against the latest
snapshot. Missing, non-string, non-UTF-8 or invalid JSON records a failure and
leaves the last good destination unchanged. Notifications before the
post-notification drain are coalesced. `JS.INFO` reports the keys, bounded
counters, pending state, last duration and a bounded last error; it never
reports source code.

Names are nonempty UTF-8 of at most 128 bytes and are scoped to the selected
database. A name must be explicitly unwatched before reuse. Definitions belong
to the ACL username that registered them, and only that username can inspect or
remove them. Redis looks up that username afresh for each run, so disabling or
deleting it fails closed. Recreating a username with the same bytes makes the new account the watch
owner; subsequent runs use that account's current enabled state and permissions
until the watch is removed. `JS.UNWATCH` itself performs
no `GET` or `HSET`, allowing an owner with `+js.unwatch` to remove a definition
after those permissions are revoked.

Registration preflights the owner's current `GET`/source and `HSET`/destination
permissions without touching either key. Every run checks those permissions,
the owner's current permission to run `JS.WATCH`, and performs the actual calls
under that owner. Both keys have read-source/write-destination metadata and
`COMMAND GETKEYS` coverage. Reactive watches require standalone Redis and
execute only on a writable primary outside loading, replay and shutdown.
Replicas receive replicated `HSET` effects and never rerun JavaScript.
`JS.PROJECT` keeps its existing behavior, including in cluster mode.

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

## Opt-in deadline diagnostics

The private `internal-phase-diagnostics` Cargo feature adds timeout observations
for [issue #246](https://github.com/joshrotenberg/redismodule-rs/issues/246).
It is off by default and has no runtime switch. Build the module and tests with
the same feature, profile and API floor:

```sh
cargo build -p redis-module-js --features internal-phase-diagnostics
cargo test -p redis-module-js --features internal-phase-diagnostics --test phase_diagnostics -- --test-threads=1
```

Only the existing final timeout decision emits a `js_phase` warning, after
context and runtime cleanup. The original 25 ms deadline, errors and writes
remain unchanged. Clocks, an allocation and interrupt counters consume that
same deadline; formatting and logging delay the reply afterward. Enabled
measurements can affect timing and allocation failures.

The body contains these 18 canonical decimal `name=value` fields in order:

```text
v op mask avail_us diag_us rt_us ctx_us vm_us run_us result_us cdrop_us rdrop_us guard_us total_us vm_err polls hit flags
```

`v=1`; `op=1` means function validation and `op=2` means projection. `mask`
marks nine timestamps: trace start, setup complete, runtime created, context
created, successful function call, VM result ready, context dropped, runtime
dropped and final timeout guard selected. Validation and failed calls omit
timestamp 4. `vm_err` records whether the VM result was an error; `polls` is a
saturating interrupt observation count and `hit` records an observed true
deadline comparison. Neither is a threshold or a cause attribution.

Durations are wall microseconds, calculated independently before truncation:

| Field | Timestamp endpoints |
| --- | --- |
| `avail_us` | trace start to original effective deadline, saturating at zero |
| `diag_us` | 0 to 1 |
| `rt_us` | 1 to 2 |
| `ctx_us` | 2 to 3, including runtime configuration |
| `vm_us` | 3 to 5, including wrapped-source formatting and `context.with` entry |
| `run_us` | 3 to 4 |
| `result_us` | 4 to 5 |
| `cdrop_us` | 5 to 6 |
| `rdrop_us` | 6 to 7 |
| `guard_us` | 7 to 8 |
| `total_us` | 0 to 8, before formatting and logging |

A missing endpoint renders zero; use `mask` to distinguish it from an actual
zero duration. `vm_us` remains available without timestamp 4. `flags` bits 0,
1 and 2 mean duration saturation, a nonmonotonic timestamp pair and counter
overflow. Durations and counters fit `u32`; the ASCII body is bounded to 384
bytes, excluding newline, NUL and Redis's prefix. It contains no keys, owner,
source, credentials or error text. These observations cannot distinguish CPU
execution from descheduling or establish the cause of earlier timeouts.

With the feature enabled, a failed watch-count registration adds its index,
capped client elapsed time and the latest validated `op=1` record to its test
error. The reader takes at most the last 8192 bytes of the owned server log and
returns only reserialized numeric fields or `unavailable`. The latest record
may belong to another invocation, and an older record may fall outside the
window. Existing reload and watch-count assertions remain in force.

## Order summary example

The runnable function is in [`examples/order-summary.js`](examples/order-summary.js):

```sh
redis-cli HSET order:1:summary retained yes
redis-cli -x JS.WATCH order-summary order:1 order:1:summary < js/examples/order-summary.js
redis-cli SET order:1 '{"customer":"Ada","items":[{"quantity":2,"price_cents":1250},{"quantity":1,"price_cents":500}]}'
redis-cli HGETALL order:1:summary
redis-cli SET order:1 'not JSON'
redis-cli HGETALL order:1:summary
redis-cli JS.INFO order-summary
redis-cli JS.UNWATCH order-summary
```

Register once, then each valid `SET` refreshes `customer`, `item_count` and
`total_cents` automatically while `retained` is preserved. The invalid update
increments failure diagnostics and keeps the previous hash. Projection uses
merge-`HSET`: unrelated fields and fields omitted by later output stay.
Deleting or expiring the source similarly preserves the last good hash and
records a failure.

## Resource and accounting limits

Each invocation gets a fresh QuickJS runtime and context, with no module loader,
I/O bridge, Redis handle, futures support or pending-job execution. Limits are:

- 64 KiB JavaScript source
- 1 MiB input JSON and 1 MiB serialized output JSON
- 1024 output fields
- 8 MiB QuickJS heap, 256 KiB QuickJS stack and a 25 ms invocation deadline

Reactive definitions additionally allow at most 64 watches across all
databases, 4 MiB of aggregate retained name/owner/key/source bytes, 1024-byte
keys and owners, and one active source per database. Any source/destination
overlap with another watch in that database is rejected, preventing chains and
cycles. A single queue bounded by the watch limit feeds one drain. Each drain
runs at most 64 distinct watches and shares a 25 ms budget; exhausted work is
cleared and diagnosed without continuation jobs. Source notifications generated
by a drain itself (including lazy expiry during `GET`) are suppressed because
the current snapshot read already accounts for them. Unload is refused while
Redis owns a queued or executing drain, even if its watches were unwatched;
unload can be retried after that drain finishes. Projection failure never rolls
back the source write.

This milestone deliberately uses QuickJS's bounded default libc allocator.
QuickJS allocations are therefore external to Redis `used_memory`. The heap,
stack and deadline are not a whole-process memory ceiling; Rust, Redis and
native library allocations are outside the QuickJS heap limit. Engine
interruption is cooperative, and the host checks the same deadline again after
output conversion, accounting and QuickJS cleanup before allowing `HSET`. The
`rust-alloc` rquickjs feature is not enabled because rquickjs documents that
`set_memory_limit` is a no-op with custom allocators. A fallible, budgeted Redis
allocator remains separate future work.

Watch definitions are module-local and ephemeral. They are not replicated,
persisted, reconstructed after RDB/AOF load, or recovered on failover;
unloading or retained-image reload starts empty. Only successful native `HSET`
effects replicate and persist. Persistent libraries, background execution,
general Redis host calls, production allocator policy and script-defined data
types remain out of scope.
