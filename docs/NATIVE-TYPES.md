# Native data types

The `native_types` rustdoc describes `NativeType`, its optional traits and
`RedisType`; the `data_type` and `native_type` examples use them. This guide
covers two server behaviors a native type can run into: read errors during an
RDB load, and an `aux_save` that requests a context and writes nothing.

## Read errors and `HANDLE_IO_ERRORS`

Without `ModuleOptions::HANDLE_IO_ERRORS`, a module read that runs out of data
during an RDB load, as in a truncated file, aborts Redis inside the read
(`moduleRDBLoadError`, `module.c:7648` in 8.10.2). `RdbReader` never gets to
return the error. The abort message names the module, the type and the key,
and `hide-user-data-from-log yes` does not hide the key.

With the option, the read sets the IO error flag and returns. The `RdbReader`
method returns an error, `rdb_load` fails, and Redis handles the failed load:

- At startup, Redis logs the failure and exits with status 1, without a crash
  report. Before exiting it runs its RDB check on the file (`rdbReportError`,
  `rdb.c:85` in 8.10.2), which prints the key on standard output.
  `hide-user-data-from-log` does not cover that output.
- On a replica loading from the replication socket (diskless load), Redis
  treats a short read as a connection error and continues (`rdb.c:92` in
  8.10.2).

Redis skips diskless load on a replica (`repl-diskless-load on-empty-db` or
`swapdb`) while a loaded module registers a data type without the option
(`replication.c:2261` in 8.10.2; 6.0.20, 6.2.24, 7.0.15, 7.2.16, 7.4.11,
8.0.6 and 8.2.10 have the same check). On 8.10.2, `repl-diskless-load flushdb`
loads from the socket regardless, and a short read there takes the abort path
above for a module without the option. The replica cases are from the source
and were not reproduced.

Set the option in `init` for a module that registers native types:

```rust
use redis_module::{ModuleOptions, RedisString, RegistrationContext, Status};

fn init(ctx: &RegistrationContext<'_>, _args: &[RedisString]) -> Status {
    ctx.set_module_options(ModuleOptions::HANDLE_IO_ERRORS);
    Status::Ok
}
```

- It covers every type the module registers, and `aux_load` as well as
  `rdb_load`.
- `RdbReader` checks the error after each read, so a type that reads only
  through it and passes errors on with `?` needs nothing else.
- A raw read through `RdbReader::as_ptr` must check for the error with
  `raw::is_io_error` (`RedisModule_IsIOError`) before using the result. With
  the option a failed read returns 0 or NULL instead of aborting.
- `set_module_options` replaces the stored mask, so combine options with `|`
  in one call.
- `INFO modules` lists the option as `options=[handle-io-errors]`.

### Reproduction

The `data_type` example (type `mytype123`, no options) on Redis 7.4.11 and
8.10.2, started with `--hide-user-data-from-log yes --rdbcompression no`:
`alloc.set user:secret-key 64`, `SAVE`, the RDB file cut 32 bytes into the
value, and a restart on that file. Redis aborts (exit status 134) and the
crash report contains:

```text
Guru Meditation: Error loading data from RDB (short read or EOF). Read performed by module 'alloc' about type 'mytype123' after reading '0' bytes of a value for key named: 'user:secret-key'. #module.c:7661
```

7.4.11 prints the same line with `#module.c:7064`.

A test module with the same kind of value and `HANDLE_IO_ERRORS`, same steps:
the SDK logs `native type rdb_load failed: Store error: ERR short read or OOM
loading DB`, Redis logs `Short read or OOM loading DB. Unrecoverable error,
aborting now.` and `Terminating server after rdb file reading failure.`, and
exits with status 1. The log file does not contain the key. The RDB check
prints `[additional info] Reading key 'user:secret-key'` on standard output.

## `aux_save` and `RedisModule_GetContextFromIO`

The SDK registers `AuxData::aux_save` as `aux_save2` as well as `aux_save`.
Redis 7.2 and later call `aux_save2`, so an `aux_save` that writes nothing
saves nothing for the type. If that call first gets a
context with `RedisModule_GetContextFromIO` through `RdbWriter::as_ptr`, Redis
returns before it frees the context: `rdbSaveSingleModuleAux` returns early
when nothing was written and skips `moduleFreeContext`. The early return is at
`rdb.c:1830` in 8.10.2, 1320 in 8.2.10, 1321 in 8.0.6, 1315 in 7.4.11 and 1250
in 7.2.16. Redis 7.0.15 and earlier have no `aux_save2` path and free the
context.

Besides the leaked allocation, the context holds an execution unit open:
creating it increments the execution nesting level (`module.c:975` in 8.10.2)
and only freeing it decrements the level. After a save in the server process,
such as `SAVE`, the level stays raised for the life of the process, and:

- with AOF on or a replica attached, the next write aborts Redis at
  `blocked.c:357 'server.also_propagate.numops == 0'` (8.10.2;
  `blocked.c:326` in 7.2.16): Redis queues the write's propagation and never
  sends it;
- the next `ThreadSafeContext::lock` or `try_lock` aborts Redis at
  `module.c:9180 'server.execution_nesting == 0'` (8.10.2; `module.c:8509` in
  7.2.16). The assertion has no condition; the lock was reproduced without
  AOF or replicas, and `try_lock` reaches it through
  `RM_ThreadSafeContextTryLock` (from the source).

Other effects follow from the source and were not reproduced:
`postExecutionUnitOperations` returns early while the level is raised
(`server.c:3908` in 8.10.2), so post-notification jobs queued after the save
do not run.

`FLUSHALL` with save points configured and `DEBUG RELOAD` also save in the
server process (`db.c:1254` and `debug.c:639` in 8.10.2; not reproduced).
`BGSAVE` runs `aux_save` in a child process, which exits, and the server is
not affected.

An `aux_save` that gets the context must write at least one value. `aux_save`
gets no context from the SDK, and the `RdbWriter` methods need none.

### Reproduction

A test type with `AuxTriggers::AFTER_RDB` whose `aux_save` calls
`RedisModule_GetContextFromIO(io.as_ptr())`, on Redis 7.2.16 and 8.10.2:

| Sequence | Result |
| --- | --- |
| `aux_save` writes nothing without the context; `SAVE`; `SET` | the write reaches the AOF |
| Gets the context and writes a value; `SAVE`; `SET` | the write reaches the AOF |
| Gets the context and writes nothing; `BGSAVE`; `SET` | the write reaches the AOF |
| Gets the context and writes nothing; `SAVE`; `SET`, AOF on | Redis aborts at `blocked.c:357` (7.2.16: 326) |
| The same with a replica attached and AOF off (8.10.2) | Redis aborts at `blocked.c:357` |
| The same with AOF off and no replica; `SET`, then a thread locks Redis | `SET` succeeds; the lock aborts Redis at `module.c:9180` (7.2.16: 8509) |

On Redis 6.2.24 the fourth row's write reaches the AOF, and a thread lock
after it succeeds.
