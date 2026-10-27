# Porting example: redis-event-stream-module

This walkthrough follows one real module from redis-module 2.x to 3.0.
[redis-event-stream-module](https://github.com/redis-developer/redis-event-stream-module)
mirrors keyspace notifications into Redis Streams, one stream per event name
such as `events:expired`, and writes lifecycle gap markers such as `loaded`,
`unloading` and `flushed` to a control stream. Its `src/` directory has eight
files and about 6,400 lines of Rust. `cargo test` in its workspace runs 255
tests, and the integration tests start real Redis servers. The port was done
for #89.

| | SDK 2 | SDK 3 |
|---|---|---|
| Module revision | [`0cb93ba`](https://github.com/redis-developer/redis-event-stream-module/tree/0cb93ba), the module's `main` | [`3b09c5a`](https://github.com/redis-developer/redis-event-stream-module/tree/3b09c5a9ec14a5dac7d23055fdca1ff91b5ca39b), the head of branch `codex/sdk3-port` |
| SDK | upstream `redis-module` tag `v2.1.3` | this repository's tag `v3.0.0-alpha.1` (`4d473c3`) |
| SDK features | `default-features = false`, `min-redis-compatibility-version-7-2` | the same |

The module needs Redis 7.2 or later, where post-notification jobs first
appear. The excerpts below are copied from the two revisions and trimmed with
`// ...`.

## How the port went

The port is seven commits on `codex/sdk3-port`:

| Commit | Subject |
|---|---|
| `a3d96c6` | build!: move to redis-module 3.0 |
| `6882a21` | refactor: use SDK wrappers for database selection, clock and test notifications |
| `6d13742` | refactor: subscribe to keyspace and server events through the SDK |
| `77beb95` | build: drop the direct linkme dependency |
| `e44a353` | docs: update SPEC and comments for redis-module 3.0 |
| `473cc18` | build: pin redis-module to the v3.0.0-alpha.1 tag |
| `3b09c5a` | docs: refresh comments for the redis-module 3.0 tag pin |

Building the 2.1.3 sources against 3.0 gave 31 errors, all predicted by the
[migration guide](MIGRATING-3.0.md): 16 uses of the private `ctx` field, 8
context-view parameter types and 7 newly `unsafe` constructors. `a3d96c6`
fixes them. `6882a21` and `6d13742` are optional: they replace raw calls that
3.0 wraps. The port's diff to `src/`, `examples/notifytest.rs` and `tests/`
adds 322 lines and removes 337.

## Improvements from the port

### Context views checked at compile time

Under 2.1.3, `init`, `deinit`, post-notification jobs and every helper took
`&Context`, the command handler's type, so they had the same methods as a
command, including client replies. 3.0 passes each callback the view that
matches what Redis allows there, as the guide's
[table](MIGRATING-3.0.md#events-and-locked-workers-have-separate-context-views)
lists. The change is to signatures, not bodies. `src/capture.rs` at `0cb93ba`:

```rust
pub(crate) fn process_pending_event(ctx: &Context, event: &PendingEvent) {
// ...
pub(crate) fn defer_pending_event(ctx: &Context, event: PendingEvent) {
    let status = ctx.add_post_notification_job(move |ctx| {
        guard_job(move || process_pending_event(ctx, &event));
    });
// ...
pub(crate) fn on_keyspace_event(ctx: &Context, event_type: NotifyEvent, event: &str, key: &[u8]) {
```

`src/capture.rs` at `3b09c5a`:

```rust
pub(crate) fn process_pending_event(ctx: &RuntimeContext, event: &PendingEvent) {
// ...
pub(crate) fn defer_pending_event(ctx: &EventContext, event: PendingEvent) {
    let status = ctx.add_post_notification_job(move |ctx| {
        guard_job(move || process_pending_event(ctx, &event));
    });
// ...
pub(crate) fn on_keyspace_event(
    ctx: &EventContext,
    event_type: NotifyEvent,
    event: &str,
    key: &[u8],
) {
```

The same split runs through the rest of the module:

| Functions | 2.1.3 | 3.0 |
|---|---|---|
| `init` and `setup_acl_category` (`src/lib.rs`) | `&Context` | `&RegistrationContext<'_>` |
| `deinit` (`src/lib.rs`) | `&Context` | `&EventContext` |
| Functions that schedule post-notification jobs: `on_keyspace_event`, `defer_pending_event`, `dispatch_pending_event`, `defer_ordered_fallback`, `drain_pending_markers` | `&Context` | `&EventContext` |
| Helpers that call, log, select a database or lock a guard, in `capture.rs`, `cluster.rs`, `commands.rs`, `dispatch.rs`, `markers.rs` and `stats.rs` | `&Context` | `&RuntimeContext` |
| The three command handlers (`src/commands.rs`) | `&Context` | `&Context`, unchanged |

What this buys a module author:

- `add_post_notification_job` is an `EventContext` method. A helper that
  takes `&RuntimeContext`, or the worker's lock guard, cannot schedule a job,
  so the signatures show which functions can.
- A job receives a fresh `&EventContext`. The write helpers it runs take
  `&RuntimeContext`, which has calls, database selection and logging but no
  client replies or blocking. Commands, `deinit` and the asynchronous worker's
  lock guard coerce to the same type, so one set of helpers serves every
  caller.
- `setup_acl_category` takes `&RegistrationContext<'_>`, so it can run only
  during the load.
- The worker's lock guard now dereferences to `RuntimeContext`, which is
  already a `RedisLockIndicator`, so the module's own
  `unsafe impl RedisLockIndicator` is gone.

The first build's 8 type errors pointed at this split: six inside
`add_post_notification_job` closures, where a helper still expected
`&Context`, and `init` and `deinit`, reported at the `redis_module!`
invocation.

### Safe wrappers in place of raw calls

2.1.3 had no wrapper for several calls the module needed, so the module called
raw bindings with the native context. 3.0 wraps them. None of these
replacements is required, and the port made all of them:

| 2.1.3 raw call | 3.0 wrapper | Files |
|---|---|---|
| `RedisModule_GetSelectedDb`, `RedisModule_SelectDb` | `get_selected_db()`, `select_db(0)`, `with_db(0, ..)` | `src/capture.rs`, `src/markers.rs`, `src/commands.rs` |
| `RedisModule_Milliseconds` | `redis_module::milliseconds()` | `src/capture.rs` |
| `RedisModule_SubscribeToKeyspaceEvents` and an `extern "C"` callback | `subscribe_to_keyspace_events` with `redis_event_handler!` | `src/lib.rs`, `src/capture.rs` |
| `RedisModule_NotifyKeyspaceEvent` with a `CString` name | `notify_keyspace_event_bytes` | `examples/notifytest.rs` |
| `RedisModule_SubscribeToServerEvent` for FLUSHDB, SWAPDB and CONFIG, with three `extern "C"` callbacks | `#[flush_info_event_handler]`, `#[swapdb_event_handler]`, `#[config_changed_event_handler]` | `src/lib.rs`, `src/capture.rs` |

Database selection in `cmd_streams`, `src/commands.rs` at `0cb93ba`:

```rust
    let registry = format!("{}{seg}#streams", PREFIX.value.lock(ctx).as_str());
    let orig_db = unsafe { raw::RedisModule_GetSelectedDb.unwrap()(ctx.ctx) };
    if unsafe { raw::RedisModule_SelectDb.unwrap()(ctx.ctx, 0) } != raw::REDISMODULE_OK as i32 {
        return Err(RedisError::Str("failed to select database 0"));
    }
    let out = streams_in_db0(ctx, &registry, mode);
    // Restore the caller's database before returning on any path.
    unsafe { raw::RedisModule_SelectDb.unwrap()(ctx.ctx, orig_db) };
    out
}
```

`src/commands.rs` at `3b09c5a`:

```rust
    let registry = format!("{}{seg}#streams", PREFIX.value.lock(ctx).as_str());
    // `with_db` selects db 0 for the body and restores the caller's database
    // on every path, including an unwind.
    ctx.with_db(0, |ctx| streams_in_db0(ctx, &registry, mode))
        .map_err(|_| RedisError::Str("failed to select database 0"))?
}
```

The 2.1.3 code restored the database on every return but not on a panic.
`with_db` also restores it on unwind, which matters now that a panic in a
command replies with an error instead of aborting the server. `cmd_prune` has
the same change.

The keyspace callback, `src/capture.rs` at `0cb93ba`:

```rust
pub(crate) extern "C" fn raw_keyspace_event(
    ctx: *mut raw::RedisModuleCtx,
    event_type: c_int,
    event: *const c_char,
    key: *mut raw::RedisModuleString,
) -> c_int {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let context = Context::new(ctx);
        let key_slice = RedisString::string_as_slice(key);
        let event_name = String::from_utf8_lossy(unsafe { CStr::from_ptr(event) }.to_bytes());
        on_keyspace_event(
            &context,
            NotifyEvent::from_bits_truncate(event_type),
            &event_name,
            key_slice,
        );
    }));
    // ...
    raw::Status::Ok as c_int
}
```

`src/capture.rs` at `3b09c5a`:

```rust
pub(crate) fn keyspace_event_handler(
    ctx: &EventContext,
    event_type: NotifyEvent,
    event: &str,
    key: &[u8],
) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        on_keyspace_event(ctx, event_type, event, key);
    }));
    // ...
}
```

The module wrote the 2.1.3 callback by hand for two reasons: the macro's
generated handler panicked across the FFI boundary on a non-UTF-8 event name
(upstream issue 472), and the `event_handlers` list intersected away `MISSED`
and `NEW`. `init` now subscribes with
`ctx.subscribe_to_keyspace_events(mask, redis_event_handler!(keyspace_event_handler))`.
That keeps the same mask, chosen at load, and the same lossy decoding of the
event name, lends the key as `&[u8]`, and contains panics. The SDK only logs a
contained panic, so the module keeps its own `catch_unwind` to count it in the
`handler_panics` INFO field. 3.0 also contains panics in post-notification
jobs, which 2.1.3 did not; the module's `guard_job` stays for the same
counting reason.

The flush and SWAPDB attributes copy the event payload out for the handler.
`FlushInfo.db` is `None` for `FLUSHALL`, which the module maps back to its
marker's `-1`, and `SwapDbInfo` carries both database numbers. The 2.1.3
callbacks cast and dereferenced the payload pointers themselves.

Across `src/` and `examples/notifytest.rs`, `unsafe` blocks, functions and
impls went from 39 to 24, raw call or read sites from 26 to 12, and
`extern "C"` callbacks from 5 to 1. The remaining callback is described under
[What stays raw](#what-stays-raw).

### Load failures that refuse cleanly

Three failed-load paths changed.

**An older server.** On a Redis 6.0.20 build, 2.1.3 panicked in its command
registration code (`Option::unwrap()` on `None`) and aborted with "failed to
initiate panic", exit status 134. 3.0 logged `<module> module load panicked:
RedisModule_GetCommand is not available in this Redis server`, and the server
exited with status 1. On 7.0 and 7.1, registration needs only APIs those
servers have, so `init` runs and its existing version gate returns
`Status::Err`; under 2.1.3 that gate was unreachable on any pre-7.2 server.
The 7.0 and 7.1 path comes from reading the SDK and the Redis 7.0.15 source;
no 7.0 binary was run.

**Numeric configuration defaults.** `maxlen`, `max-streams` and `retention-ms`
are registered with a range of 0 to `i64::MAX`. Under 2.1.3 a module argument
became the registered default without that check, so the module repeated it in
`set()`. `src/config.rs` at `0cb93ba`:

```rust
/// `eventstream.maxlen` config binding. Redis enforces the registered 0 to
/// i64::MAX range on CONFIG SET and redis.conf paths, but a module-arg value
/// becomes the registered default and bypasses that boundary check entirely
/// (verified against the wrapper at v2.1.3 and redis 7.2 module.c/config.c),
/// so `set()` re-validates: a negative value would silently disable trimming,
/// the module's only memory bound. Rejection aborts the load like any other
/// malformed module arg.
// ...
    fn set(&self, _ctx: &ConfigurationContext, val: i64) -> Result<(), RedisError> {
        if val < 0 {
            return Err(RedisError::String(format!(
                "maxlen must be 0 (trimming disabled) or positive, got {val}"
            )));
        }
```

3.0 checks the default against the registered range before registering the
configuration, so `maxlen -1` fails the load before `set()` runs, with the
SDK's message. `tests/capture.rs` at `3b09c5a`:

```rust
fn negative_maxlen_module_arg_aborts_load() {
    // redis-module 3.0 checks a module-arg default against the registered
    // 0..i64::MAX range before registering the config (SPEC.md section 7), so
    // the refusal is the wrapper's range error rather than the binding's own.
    let err = TestServer::try_start(&["maxlen", "-1"])
        .err()
        .expect("loadmodule with maxlen -1 must abort the server start");
    assert!(
        err.contains("Numeric configuration 'maxlen' default must be between 0 and"),
        // ...
    );
}
```

The setter is unchanged and stays as defense in depth.

**Invalid module arguments.** Under 2.1.3, a module argument that the module's
own `set()` rejected (the port tried `control-checkpoint-ms 99`,
`events @loaded` and `cluster-streams bogus`) still let `init` run with the
defaults and log `eventstream loaded: ...`. Redis then unloaded the module,
which ran `deinit` and logged `eventstream unloading: ...`, before the server
exited. 3.0 fails the load before `init` with `Redis rejected a module
configuration value; see the Redis log`. Both exit with status 1, and Redis's
own line naming the bad value is in the log either way.

For a module author, an older server gets a logged reason instead of a crash,
the version check in `init` is a real refusal on 7.0 and 7.1, the range
registered with a numeric configuration also holds for module arguments, and
`init` and `deinit` no longer run for a load that a configuration value has
already failed.

### A simpler build

`Cargo.toml` at `0cb93ba`:

```toml
[dependencies]
# crates.io releases of redis-module lag the repo badly; Redis's own modules
# (e.g. RedisJSON) consume the git v2.1.x tags instead. Match RedisJSON's pin
# shape, with the 7.2 compatibility floor from SPEC.md section 14.
redis-module = { git = "https://github.com/RedisLabsModules/redismodule-rs", tag = "v2.1.3", default-features = false, features = [
    "min-redis-compatibility-version-7-2",
] }
redis-module-macros = { git = "https://github.com/RedisLabsModules/redismodule-rs", tag = "v2.1.3" }
# The info_command_handler attribute expands to linkme::distributed_slice in
# this crate's namespace, so linkme must be a direct dependency (same major
# as the wrapper's).
linkme = "0.3"
lazy_static = "1"
```

`Cargo.toml` at `3b09c5a`:

```toml
[dependencies]
# redis-module 3.0 (the joshrotenberg/redismodule-rs fork) is not published to
# crates.io, so both crates are pinned to one git pre-release tag, with the 7.2
# compatibility floor from SPEC.md section 14. The macros crate comes from the
# same git and tag: its expansions name `redis_module` items, including the
# SDK's own re-export of linkme, so this crate needs no linkme dependency.
redis-module = { git = "https://github.com/joshrotenberg/redismodule-rs", tag = "v3.0.0-alpha.1", default-features = false, features = [
    "min-redis-compatibility-version-7-2",
] }
redis-module-macros = { git = "https://github.com/joshrotenberg/redismodule-rs", tag = "v3.0.0-alpha.1" }
lazy_static = "1"
```

- **No bindgen or libclang.** The bindings come pregenerated. `Cargo.lock`
  gained `redis-module`, `redis-module-macros` and
  `redis-module-macros-internals` 3.0.0 and `redis-module-common` 0.1.2, which
  replaces upstream's `common` 0.1.0, and lost 50 other packages, among them
  `bindgen`, `clang-sys`, `which`, `home`, `backtrace` and its tree,
  `serde_syn`, `enum-primitive-derive`, `num-traits` 0.1, `nix`, `regex`, and
  `syn` 0.11 and 1.x. The release dylib went from 2,426,240 to 1,036,016
  bytes. The Dockerfile comment no longer gives libclang as a requirement; the
  package stays installed until the image is rebuilt and checked without it.
- **No direct `linkme` dependency.** The attribute macros expand through the
  SDK's re-export of `linkme`.
- **A tag pin.** The port started on a commit of this repository and moved to
  the `v3.0.0-alpha.1` tag in its last commit, the same pin shape as 2.1.3's
  `v2.1.3`. Both crates name the same git source and tag.
- **A smaller dependency policy.** `deny.toml`'s `allow-git` names this
  repository. The `Apache-2.0 WITH LLVM-exception`, `0BSD` and `ISC`
  allowances, which only the bindgen tree needed, are gone, and so is the
  license clarification for upstream's `common` 0.1.0.

## Behavior

The module's own suite, run with `RUST_TEST_THREADS=4 cargo test
--no-fail-fast`, is the behavior check:

| SDK | Redis 8.10.2 | Redis 7.2.16 |
|---|---|---|
| 2.1.3 at `0cb93ba` | 255 passed, 0 failed | 255 passed, 0 failed |
| 3.0 with the tests unchanged | not run | 252 passed, 3 failed |
| 3.0 at `473cc18` | 255 passed, 0 failed | 255 passed, 0 failed |

`3b09c5a`, the branch head, changes only comments after `473cc18`, where these
runs were made.

With the tests unchanged, the same three tests also failed on Redis Stack
7.4.7, the harness default described under
[Reproducing the results](#reproducing-the-results). They are the first item
under [Intentional changes](#intentional-changes).

A wire comparison on 8.10.2 and 7.2.16 loaded both release builds with
`events *` and ran the same command script: SET, DEL and a GET miss,
`EVENTSTREAM.STATS`, `EVENTSTREAM.STREAMS VERBOSE`, `EVENTSTREAM.PRUNE`, CONFIG
GET, COMMAND INFO, ACL CAT, MODULE LIST, the control stream, two error paths,
two rejected CONFIG SET calls, INFO, and MODULE UNLOAD and LOAD. The replies
were identical apart from CONFIG GET ordering, and so were the module's log
lines.

### Behavior that matches

The tests named here pass unchanged under both revisions.

- **Notification capture and the subscription mask.** `tests/event_classes.rs`:
  `missed_class_captures_keymiss`, `new_class_captures_new_key`,
  `missed_and_new_not_delivered_by_default` and
  `star_at_load_subscribes_missed_and_new`.
- **Event names that are not UTF-8, or are empty.** `tests/module_events.rs`:
  `non_utf8_event_name_is_captured_not_a_crash` and
  `empty_event_name_increments_skipped_invalid`. They fire through the
  companion module `examples/notifytest.rs`, which now sends with
  `notify_keyspace_event_bytes`.
- **Panic containment.** The handler and job guards still count into
  `handler_panics`. `tests/module_events.rs`, `tests/event_classes.rs` and
  `tests/async_dispatch.rs` assert that it stays 0. No test forces a panic, so
  the suite does not exercise the unwind path itself. The SDK's own regressions
  cover it:
  [`test_runtime_keyspace_callback_preserves_bytes_and_database_and_catches_panics`](../tests/integration.rs)
  for a keyspace callback that panics, and
  [`test_database_scopes_restore_after_errors_and_panics`](../tests/integration.rs)
  for `with_db` restoring the database after an error or a panic.
- **Database selection.** `tests/capture.rs`
  `cross_db_event_lands_in_db0_with_origin_field`; `tests/commands.rs`
  `streams_reads_db0_from_any_client_db` and
  `prune_operates_on_db0_from_any_client_db`; `tests/auto_group.rs`
  `auto_group_ignores_same_destination_name_in_another_database`. The
  selection error keeps the module's text.
- **Server clock.** `tests/retention.rs` `retention_ms_trims_entries_by_age`,
  whose `MINID` threshold comes from `redis_module::milliseconds()`.
- **Flush and SWAPDB markers.** `tests/markers.rs`
  `flushall_writes_flushed_marker_db_all`,
  `flushdb_nonzero_writes_flushed_marker_with_db_and_streams_survive`,
  `swapdb_db0_writes_swapdb_marker_and_moves_streams` and
  `swapdb_without_db0_writes_no_marker`; `tests/replication.rs`
  `flush_marker_replicates_and_is_not_duplicated_on_promotion`.
- **Configuration changes.** `tests/observability.rs`
  `eviction_risk_tracks_maxmemory_policy`, driven by the config-change event.
- **Unload and in-place reload.** `tests/upgrade.rs` `in_place_unload_load_swap`
  and `async_dispatch_can_unload_and_reload_in_process`; `tests/markers.rs`
  `unloading_marker_on_module_unload`.
- **The paths that stay raw.** `tests/acl.rs` for the ACL category;
  `tests/markers.rs` `checkpoint_cadence_is_opt_in_bounded_and_load_time_only`
  and `checkpoint_cron_does_not_block_module_unload` for the conditional cron
  subscription; `tests/async_dispatch.rs` for the try-locking worker;
  `tests/cluster.rs`, such as `per_node_repins_after_slot_migration`, for the
  per-slot tags.
- **Load errors that keep their text.** `tests/capture.rs`
  `glob_stream_prefix_module_arg_aborts_load` and `tests/cluster.rs`
  `invalid_cluster_streams_value_aborts_load`. These loads now fail before
  `init`, as the next section describes, and Redis's log line still carries
  the module's message.

### Intentional changes

None of these is a regression; each follows from a 3.0 change that the
[guide](MIGRATING-3.0.md#changes-that-compile-unchanged) lists.

1. **Negative numeric module arguments get the SDK's message.** Three tests
   changed their expectation: `tests/capture.rs`
   `negative_maxlen_module_arg_aborts_load`, `tests/filters.rs`
   `max_streams_rejects_negative_module_arg` and `tests/retention.rs`
   `negative_retention_ms_module_arg_aborts_load`. Each expected the module's
   own message, such as `maxlen must be 0 (trimming disabled) or positive`,
   and now expects `Numeric configuration '<name>' default must be between 0
   and 9223372036854775807`. The load still fails; only the message changed,
   because 3.0 checks the registered range before the module's `set()` runs.
2. **Invalid module arguments fail before `init`.** 2.1.3 ran `init`, and then
   `deinit` as Redis unloaded the module, as described above. The suite's
   load-failure tests match only the error text or the failure, so they pass
   under both; the `init` and `deinit` difference was checked by hand on
   8.10.2.
3. **Boolean module arguments must be `yes` or `no`,** in any ASCII case. With
   `enabled maybe`, 2.1.3 loaded with capture disabled and 3.0 fails the load
   with `Boolean configuration 'enabled' must be 'yes' or 'no'`. With
   `enabled YES`, 2.1.3 loaded with capture disabled and 3.0 loads with it
   enabled. The suite loads only `enabled no` (`tests/markers.rs`
   `enabled_no_load_queues_loaded_then_disabled`), which behaves the same; the
   other values were checked by hand on 8.10.2.
4. **Bare `EVENTSTREAM.STREAMS` replies with bulk strings.** The command
   forwards the `SMEMBERS` reply from `Context::call`, which keeps the reply's
   type in 3.0: `*1 $10 events:set` where 2.1.3 sent `*1 +events:set`.
   `WITHSTATS` and `VERBOSE` build bulk strings and are unchanged. redis-rs
   decodes both forms to `String`, so no test sees the difference.
5. **Flush, SWAPDB and config subscriptions happen before `init`.** The
   attributes subscribe during the SDK's load sequence, not in `init` after
   the version and cluster gates, and a failed subscription fails the load,
   where the module logged a warning and continued. The SDK also reads the
   event payload without the module's null checks, and its config adapter
   expects UTF-8 configuration names. None of this is reachable on a supported
   server: a refused `init` fails the load and Redis drops the subscriptions,
   Redis always passes the payload, and configuration names are ASCII.
6. **A pre-7.2 server gets a logged load failure instead of SIGABRT.** On
   Redis 6.0.20, 2.1.3 exited with status 134 and 3.0 with status 1, as
   described above. CI has no pre-7.2 server: `tests/version_gate.rs`
   `supported_server_loads_without_version_refusal` covers the supported side,
   and the unit test `version_gate_refuses_below_7_2` in `src/lib.rs` covers
   the gate.
7. **The companion module's NUL message.** `examples/notifytest.rs` now replies
   `ERR keyspace event name contains a NUL byte` to an event name containing
   NUL, where it replied `event name must not contain NUL`. No test checks it.

## What stays raw

Twelve raw call or read sites remain. Each has a reason:

| Raw calls | Where | Why they stay raw | Issue |
|---|---|---|---|
| `ThreadSafeContextTryLock`, and with it the worker's whole thread-safe context lifecycle | `src/dispatch.rs` | No try-lock wrapper, and `ThreadSafeContext` exposes neither its pointer nor a guard for a lock taken elsewhere | #113 |
| `SubscribeToServerEvent` for the cron loop, with the last `extern "C"` callback | `src/lib.rs`, `src/markers.rs` | The module subscribes only when `control-checkpoint-ms` is positive; `#[cron_event_handler]` subscribes on every load, and there is no runtime server-event subscription | #114 |
| `AddACLCategory`, `GetCommand`, `SetCommandACLCategories` | `src/lib.rs` `setup_acl_category` | On Redis 7.2 and 7.3 the macro's per-command categories log a warning for each command; the module logs one notice, which `tests/acl.rs` uses to detect the server lane | #115 |
| `GetRandomHexChars` | `src/markers.rs` `initialize_generation` | No wrapper | none |
| `ClusterCanonicalKeyNameInSlot` | `src/cluster.rs` `select_owned_tag` | `cluster::key_for_slot` makes different names, and pinned tags must stay stable across the upgrade | none |

The worker try-locks so that it can see a stop request while `MODULE UNLOAD`
holds the Redis lock; a blocking lock would deadlock there. Its hand-written
guard now holds a `RuntimeContext` view and must drop it before unlocking,
because dropping a runtime view can end call-user scopes through the native
context. `src/dispatch.rs` at `3b09c5a`:

```rust
struct RawContextGuard {
    view: ManuallyDrop<RuntimeContext>,
    ctx: *mut raw::RedisModuleCtx,
}
// ...
impl Drop for RawContextGuard {
    fn drop(&mut self) {
        // SAFETY: dropped exactly once, here, while the lock is still held.
        unsafe { ManuallyDrop::drop(&mut self.view) };
        if let Some(unlock) = unsafe { raw::RedisModule_ThreadSafeContextUnlock } {
            unsafe { unlock(self.ctx) };
        }
    }
}
```

The module also reads three bindings without calling them, to check that an
API exists: `RedisModule_AddPostNotificationJob` in `init`,
`RedisModule_ThreadSafeContextTryLock` when the worker starts, and
`RedisModule_ClusterCanonicalKeyNameInSlot` for its Redis 7.2 fallback. The SDK
has no safe availability query (#117). At the 7.2 floor,
`add_post_notification_job` panics with the API name when the API is missing.

Separately, on Redis 6.0 to 7.4 every module built with the SDK logs a defrag
registration warning on load, whether or not it registers defrag code. The
port saw it on 7.2.16 and 7.4.7 under both SDKs; it is #116.

## Reproducing the results

The recipe needs Rust 1.88 or later, a Redis 8.10.2 server and CLI, and a
Redis 7.2 build, the module's minimum. The 2.1.3 revision also needs libclang,
for bindgen. The runs used Homebrew's Redis 8.10.2 and Redis 7.2.16 built from
source:

```bash
curl -LO https://download.redis.io/releases/redis-7.2.16.tar.gz
tar xzf redis-7.2.16.tar.gz
make -C redis-7.2.16
R72=$PWD/redis-7.2.16/src
R810=/opt/homebrew/bin   # or the directory of another Redis 8.10.2 build

git clone https://github.com/redis-developer/redis-event-stream-module
cd redis-event-stream-module

suite() {
  TEST_REDIS_SERVER_BIN="$1/redis-server" TEST_REDIS_CLI_BIN="$1/redis-cli" \
    RUST_TEST_THREADS=4 cargo test --no-fail-fast
}

git checkout 0cb93ba   # SDK 2: redis-module v2.1.3
suite "$R810"
suite "$R72"

git checkout 3b09c5a   # SDK 3: codex/sdk3-port, tag v3.0.0-alpha.1
suite "$R810"
suite "$R72"
```

Cargo prints one result line per test binary; for each of the four runs they
add up to 255 passed and 0 failed. The harness builds the module and the
notifytest companion itself with `cargo build --release`, so no separate build
step is needed. The runs used a separate `CARGO_TARGET_DIR` for each revision,
which avoids rebuilding after a checkout.

Set both binaries explicitly. The module's harness, redis-server-wrapper 0.5,
otherwise prefers a Homebrew `redis-stack-server` cask
(`/opt/homebrew/Caskroom/redis-stack-server/*/bin/redis-server`) over
`redis-server` on `PATH` and loads the Redis Stack modules beside it. On the
port's machine that was Redis Stack 7.4.7 with five extra modules: an early
2.1.3 run meant for 8.10.2 had used it, and also passed 255. Cluster formation
runs `redis-cli`, so `TEST_REDIS_CLI_BIN` matters as much as the server.

The module keeps edition 2021 and `rust-version = "1.88"`, and builds against
this edition 2024 SDK unchanged. The 1.88 floor now comes from the `redis` 1.3
client line and redis-server-wrapper 0.5; under 2.1.3, bindgen's dependency on
`home` 0.5.12 set it too. 3.0 itself needs Rust 1.87. On the port,
`cargo +1.88 check --all-targets --locked` passes.

## Other changes

This module met only part of 3.0. It hit none of the guide's other common
changes, such as raw imports from the crate root, `NextArg::next_str`,
`try_as_str` lifetimes, the key and hash return types, or `?` on other error
types. [docs/MIGRATING-3.0.md](MIGRATING-3.0.md) lists every change a 2.x
module may need, and the [changelog](../CHANGELOG.md) lists everything in the
release.
