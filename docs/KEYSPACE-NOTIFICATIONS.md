# Writing from keyspace notifications

A notification runs while Redis is changing a key. Inspect it, copy the data
you need, and queue a post-notification job for writes. Redis runs the job when
writes are safe, in the same execution unit. It is not a background task.
[`RedisModule_AddPostNotificationJob`](https://redis.io/docs/latest/develop/reference/modules/modules-api-ref/#redismodule_addpostnotificationjob)
is available from Redis 7.2.

The [keyspace_to_stream example](../examples/keyspace_to_stream.rs) captures
STRING, STREAM and LOADED notifications for keys starting with `watched:`.
It records the source database, event name and binary key in `watched:events`
in database 0. That reserved output name is excluded in every database.
The example supports standalone primaries and replicas and refuses to load
in Redis Cluster.

## Run the example

Build with an explicit 7.2 compatibility floor:

```sh
cargo build --no-default-features --features min-redis-compatibility-version-7-2 --example keyspace_to_stream
redis-server --loadmodule ./target/debug/examples/libkeyspace_to_stream.so
```

Use `.dylib` instead of `.so` on macOS. In another terminal:

```sh
redis-cli -n 3 SET watched:one value
redis-cli -n 0 XRANGE watched:events - +
redis-cli notify_stream.stats
```

The entry contains `db 3 event set key watched:one`. The stats are queued jobs,
successful appends, errors and skipped role/loading/shutdown events. They are
module-local counters, not persisted statistics.

You do not need to enable `notify-keyspace-events`. That setting controls
Pub/Sub delivery; module subscribers receive their requested notification
classes independently. Redis invokes module callbacks before checking the
Pub/Sub mask in [notify.c](https://github.com/redis/redis/blob/7.2/src/notify.c).

## Own the input and defer the write

The SDK supplies `&EventContext`, an event name and borrowed key bytes. Store
the source database number, clone the event name, and copy the key into a
`Vec<u8>` before returning. The queued `FnOnce` must own its data. Do not retain
the event context, an open Redis key, or a reference into callback input.
Binary key names need no UTF-8 conversion.

The example follows this sequence:

1. Reject the output key and keys outside the watched prefix.
2. Check that the server is primary, is not loading, and teardown has not begun.
3. Copy the database, event and key into a closure.
4. Submit with `ctx.add_post_notification_job` and check the returned status.
5. In the job, check the server state again and append through `with_db(0, ...)`.

The job's context initially selects the database where it was queued.
`with_db` temporarily selects the stream database and restores the job's
selection after return or unwind. Resources opened in that database must be
closed inside the scope.

The example subscribes to STREAM events too, so its own `XADD` produces another
notification under `watched:`. Filtering `watched:events` **before queuing** is
what stops that feedback loop. Redis does not impose a recursion limit for
jobs that schedule more jobs. Choose output keys and filters together.

## Propagate the derived write once

The append uses:

```rust
let options = CallOptionsBuilder::new()
    .replicate()
    .errors_as_replies()
    .verify_oom()
    .build();
```

These request propagation of the derived `XADD` to replicas and AOF, an error
reply on failure, and Redis's OOM check. Ordinary client source writes and
their derived appends are propagated by the primary. A replica's handler skips
work even if `replica-read-only` is disabled; it receives the already generated
stream commands instead of appending a second entry. RDB/AOF loading is skipped
too, so loading saved data does not regenerate the projection.

Propagation is also affected by the surrounding module call. Redis can suppress
nested propagation when an outer module used a non-replicating call. Test the
actual outer command path; `.replicate()` does not override all enclosing
propagation decisions. The flags and this interaction are described in the
[module call API](https://redis.io/docs/latest/develop/reference/modules/modules-api-ref/#redismodule_call).

Selecting database 0 is a standalone design choice. It is not a cluster routing
policy: cluster modules need a separate design for slots, local ownership and
output keys. This example rejects cluster mode rather than applying its
database layout there.

## Handle submission and write failures separately

With a 7.2+ feature floor, `add_post_notification_job` returns `Status`. An older
floor returns `APIResult<Status>` because the native function may be missing.
The example checks API availability during initialization; a compile-time floor
alone does not upgrade the running Redis server.

Redis rejects job submission during loading or on a readonly replica. The SDK
drops the closure and its captures immediately on rejection. After successful
submission, Redis owns the job and the SDK releases captured values as the
closure runs and its native private data is cleaned up.

Successful submission does not guarantee a successful append. A wrong-type
destination or OOM can fail the `XADD`; the example logs that error and counts
it. The original source command has already changed its key and still succeeds.
Atomic execution with that command does not mean rollback on an append error.
This is a small best-effort projection, not a complete or lossless audit log.
Only the listed notification classes and prefix are covered. The example
does not implement a stream retention policy.

## Finish callbacks before unload

The example sets a stopping flag at the beginning of `deinit`, and checks it in
both the notification handler and the job. Set that gate before adding any
teardown writes: they can emit notifications while the module is still
subscribed. Initialization resets the flag, including on platforms where an
unload does not reset Rust statics.

Post-notification jobs run at the end of the enclosing execution unit rather
than at an arbitrary later background time. The example has no workers, timers
or retained callbacks. A larger module must also finish or
cancel its own outstanding work before its code is unloaded; a stopping flag
alone does not make retained callback pointers safe.

The tests exercise ordinary unload followed by further writes and an in-place
reload. They also verify binary keys, database selection, feedback exclusion,
replication without duplicates, writable-replica gating, RDB reload, append
errors, and captured-data cleanup when Redis rejects a job:

```sh
cargo build --no-default-features --features min-redis-compatibility-version-7-2 --examples
cargo test --no-default-features --features min-redis-compatibility-version-7-2 --test integration notification_
```
