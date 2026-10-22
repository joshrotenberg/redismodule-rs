# INFO handler state access

`InfoContext` is output for one callback. It supports the INFO builder; it is
not a runtime context and does not implement `RedisLockIndicator`. Keep this
restriction when sharing helpers with command, event or worker callbacks.

The same handler runs for ordinary INFO requests and for crash reports. The
`for_crash_report` argument distinguishes these paths. Preserve it when
forwarding to another handler, including handlers registered with
`#[info_command_handler]` or the `redis_module!` macro's `info:` field.

## Ordinary INFO snapshots

The [builder example](../examples/info_handler_builder.rs) keeps an independent
`AtomicU64` counter. A command increments it, and the INFO handler reads it after
returning early for crash reports. `Ordering::Relaxed` is enough for this single
telemetry value: it does not publish other data or guarantee a consistent view
of multiple fields. Values may wrap at the integer limit.

```text
info_handler_builder.record
INFO info_handler_builder
info_handler_builder.snapshot
```

After one successful `record`, the requested INFO section contains
`info_handler_builder_recorded:1`. `snapshot` demonstrates a second ordinary
path: a command's `RuntimeContext::server_info` calls native `GetServerInfo`,
which invokes the handler synchronously before the command returns. The
handler still receives only `InfoContext`.

For state that is already protected by the Redis lock, publish a separate Rust
snapshot while a command, event or locked worker has access. Keep that snapshot
independently synchronized. Do not store a borrowed native handle in it or
assume that an atomic counter makes adjacent non-atomic fields safe. Ordinary
INFO snapshots may use Rust synchronization, but avoid holding a mutex across a
Redis call that can invoke INFO and attempt to take the same mutex again.

`RedisGILGuard::lock(info_ctx)` does not compile, even after testing
`!for_crash_report`. Do not add an unsafe `RedisLockIndicator` implementation or
construct a dummy runtime context to get around this boundary. A boolean alone
cannot establish the required lock, thread and borrow lifetime guarantees.
`RedisModuleInfoCtx` and `RedisModuleCtx` are distinct native types; casting
between them is invalid.

## Crash reports

The example returns before reading the counter or building its sections when
`for_crash_report` is true. Use this pattern for handlers intended only for
ordinary INFO. A crash may interrupt a state mutation or an existing lock
scope, including on a worker thread. Taking the Redis lock or a Rust mutex can
deadlock; accessing data through a fabricated lock proof can race or violate
Rust's aliasing rules. A nonblocking mutex attempt alone is not a portable
async-signal-safety argument either.

This early return only limits what the custom handler does. The SDK's default
handler captures and formats a backtrace before calling custom crash handlers,
and its builder and error paths can allocate or log. Neither this example nor
the SDK promises an async-signal-safe crash-report path. Successful ordinary
INFO tests are not evidence that interrupted execution or worker crashes are
safe. A handler that deliberately emits crash diagnostics needs a separate,
platform-specific signal, thread, ownership and reentrancy analysis.

## Native source audit

The following tagged Redis sources were inspected. These are source
observations, not a new API contract for all Redis derivatives or later tags.
Each row covers normal INFO dispatch, module callback dispatch and crash
collection; the version set spans all six SDK API floors and the Redis release
lines in SDK CI.

| Redis tag | Ordinary path and callback dispatch | Crash path |
|---|---|---|
| 6.0.20 | [INFO](https://github.com/redis/redis/blob/6.0.20/src/server.c#L4734), [dispatcher](https://github.com/redis/redis/blob/6.0.20/src/module.c#L6208) | [crash collection](https://github.com/redis/redis/blob/6.0.20/src/debug.c#L1681) |
| 6.2.21 | [INFO](https://github.com/redis/redis/blob/6.2.21/src/server.c#L5404), [dispatcher](https://github.com/redis/redis/blob/6.2.21/src/module.c#L7091) | [crash collection](https://github.com/redis/redis/blob/6.2.21/src/debug.c#L1624) |
| 7.0.15 | [INFO](https://github.com/redis/redis/blob/7.0.15/src/server.c#L6065), [dispatcher](https://github.com/redis/redis/blob/7.0.15/src/module.c#L9469) | [crash collection](https://github.com/redis/redis/blob/7.0.15/src/debug.c#L1796) |
| 7.2.16 | [INFO](https://github.com/redis/redis/blob/7.2.16/src/server.c#L6272), [dispatcher](https://github.com/redis/redis/blob/7.2.16/src/module.c#L10342) | [crash collection](https://github.com/redis/redis/blob/7.2.16/src/debug.c#L1900) |
| 7.4.11 | [INFO](https://github.com/redis/redis/blob/7.4.11/src/server.c#L6155), [dispatcher](https://github.com/redis/redis/blob/7.4.11/src/module.c#L10405) | [crash collection](https://github.com/redis/redis/blob/7.4.11/src/debug.c#L2048) |
| 8.0.6 | [INFO](https://github.com/redis/redis/blob/8.0.6/src/server.c#L6459), [dispatcher](https://github.com/redis/redis/blob/8.0.6/src/module.c#L10547) | [crash collection](https://github.com/redis/redis/blob/8.0.6/src/debug.c#L2142) |
| 8.2.10 | [INFO](https://github.com/redis/redis/blob/8.2.10/src/server.c#L6556), [dispatcher](https://github.com/redis/redis/blob/8.2.10/src/module.c#L10625) | [crash collection](https://github.com/redis/redis/blob/8.2.10/src/debug.c#L2154) |
| 8.4.7 | [INFO](https://github.com/redis/redis/blob/8.4.7/src/server.c#L6706), [dispatcher](https://github.com/redis/redis/blob/8.4.7/src/module.c#L10886) | [crash collection](https://github.com/redis/redis/blob/8.4.7/src/debug.c#L2191) |
| 8.6.7 | [INFO](https://github.com/redis/redis/blob/8.6.7/src/server.c#L6908), [dispatcher](https://github.com/redis/redis/blob/8.6.7/src/module.c#L11148) | [crash collection](https://github.com/redis/redis/blob/8.6.7/src/debug.c#L2204) |
| 8.8.3 | [INFO](https://github.com/redis/redis/blob/8.8.3/src/server.c#L7081), [dispatcher](https://github.com/redis/redis/blob/8.8.3/src/module.c#L11333) | [crash collection](https://github.com/redis/redis/blob/8.8.3/src/debug.c#L2252) |
| 8.10.2 | [INFO](https://github.com/redis/redis/blob/8.10.2/src/server.c#L7159), [dispatcher](https://github.com/redis/redis/blob/8.10.2/src/module.c#L11587) | [crash collection](https://github.com/redis/redis/blob/8.10.2/src/debug.c#L2320) |

Across these tags:

- `infoCommand` reaches INFO generation; when module sections are requested,
  generation calls `modulesCollectInfo` with a false crash-report flag.
- `RM_GetServerInfo` also invokes INFO generation. A callback can therefore be
  nested inside a module operation, rather than originating only from a
  top-level client INFO command.
- `modulesCollectInfo` constructs a local `RedisModuleInfoCtx`, invokes the
  registered function synchronously, and uses the resulting output before
  returning. It does not acquire the module GIL around that invocation.
  The output context must not escape the callback.
- Redis 6.0 collects module INFO directly from `sigsegvHandler`. The later tags
  use `logModulesInfo` from `printCrashReport`, reached from signal and
  assertion/panic paths. They pass a true crash-report flag without acquiring
  the module GIL for the callback. The debug implementation also explicitly
  handles a crashing thread that differs from the main thread. Its crash-report
  synchronization is not a module lock capability.

The current SDK restriction remains appropriate for the undifferentiated
`InfoContext`. A possible capability limited to ordinary callbacks still needs
its own API design and proof, including nested invocation and lifetime rules;
that decision remains tracked in [issue #61](https://github.com/joshrotenberg/redismodule-rs/issues/61).

## Validation and limits

`InfoContext` rustdoc has compile-fail controls for guarded-state access and
promotion to `RuntimeContext`, plus a compiling atomic snapshot example.
`test_info_handler_atomic_snapshot` exercises the builder against native Redis,
including accepted and rejected command arguments, a requested INFO section,
`INFO modules`, and nested native `GetServerInfo`. Existing builder/dictionary
checks remain in `test_helper_info`. These tests exercise ordinary callbacks;
they do not trigger a crash or certify signal safety.
