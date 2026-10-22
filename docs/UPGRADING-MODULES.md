# Upgrading a loaded module

`MODULE UNLOAD` followed by `MODULE LOAD` can replace an unloadable module
without restarting Redis. There is a gap when its commands are unavailable,
and a failed replacement load does not restore the old module. Prepare a
known-good artifact and its configuration before unloading.

This guide concerns replacing a running module. For source changes when moving
to this SDK, see [Migrating to 3.0](MIGRATING-3.0.md).

## Check that the module can unload

Use the name from `MODULE LIST`, which can differ from the library filename.
Redis refuses unload for modules that register native data types, even when
there are no keys of those types. Deleting keys does not remove that restriction;
use a planned server restart with compatible persistence readers instead.

Other blockers include shared APIs in use by another module and blocked clients.
Current Redis also checks outstanding timers before the module's unload hook.
Finish or cancel that work through the module's normal controls first.
An unload hook may also refuse by returning `Status::Err`.
See [MODULE UNLOAD](https://redis.io/docs/latest/commands/module-unload/) and
the [Redis 8.2 unload implementation](https://github.com/redis/redis/blob/8.2/src/module.c#L11843-L11910).

Build and test the replacement against the intended server and SDK compatibility
floor. Use distinct, versioned artifact paths, and retain the previous file.
Do not overwrite a library file that a running process may still have mapped.
On macOS the extension is normally `.dylib`; on Linux it is `.so`.

## SDK timers and unload

Retain typed `TimerHandle<T>` values for timers that may need to be cancelled
before an upgrade. Dropping a handle does not stop its timer. Cancel pending
timers explicitly while the module is active, and then request `MODULE UNLOAD`.
`cancel_timer(&handle)` returns the payload once on success; `None` means firing
has begun or the timer has already finished or been cancelled.

Pending or firing SDK timers prevent unload. Modern Redis performs its native
timer check before calling `OnUnload`, so cancelling only in `deinit` cannot
clear that blocker. Redis 6.0 lacks that native check; the SDK's lifecycle
guard refuses unload while its timers remain live. See the
[Redis 8.10 timer guard](https://github.com/redis/redis/blob/8.10/src/module.c#L12940-L12957)
and [Redis 6.0 unload path](https://github.com/redis/redis/blob/6.0/src/module.c#L7172-L7200).

The `redis_module!` lifecycle rejects new timer registration while unload
is in progress. This prevents a deinit hook from creating a callback after
Redis's initial eligibility check. If unload is
refused or the deinit hook panics, the SDK timer lifecycle becomes active again;
application state changed by the hook still needs its own recovery.

Timer creation also returns an error during module loading or initialization.
Redis can reject a module after its `OnLoad` hook succeeds, so the SDK does not
retain native timer callbacks during initialization. Start timers from runtime
commands or events after loading finishes. Creation requires the lifecycle
binding supplied by `redis_module!`; custom native loaders without that binding
cannot create SDK timers. Hidden macro hooks are not a stable custom-loader
interface.

## Record configuration and pause module traffic

See the [configuration notes](CONFIGURATION.md) for load precedence, argument
syntax, validation and callback behavior.

Record `MODULE LIST`, the effective configuration, and the old artifact's path.
`MODULE LIST` reports the original load arguments; it does not capture later
`CONFIG SET` changes. Query the module's registered configuration, including
hidden settings by their exact names, or use its own configuration interface.

Replay the desired values on the replacement load. `CONFIG SET` values from
the previous registration are not automatically carried into the next one.
An `IMMUTABLE` configuration must be supplied at load time, through supported
module arguments or `MODULE LOADEX ... CONFIG`; it cannot be restored later
with `CONFIG SET`. Argument names and syntax belong to the module. The SDK's
`module_args_as_configuration: true` option accepts unprefixed name/value pairs.

Pause callers that depend on the module and drain its work before requesting
unload. Redis's checks do not account for every application-owned thread,
queue, retained Rust closure, or external callback.

## Replace, verify, or recover

For a module exposing the `lifecycle` example's argument syntax, a replacement
sequence is:

```text
MODULE LIST
CONFIG GET lifecycle.*
MODULE UNLOAD lifecycle
MODULE LOAD /opt/modules/lifecycle-1.2.0.so prefix live: limit 9
MODULE LIST
CONFIG GET lifecycle.*
lifecycle.state
```

Check every response. If unload fails, the old module remains registered, but
its hook may have changed application state before refusing; inspect that
state before resuming traffic. If load fails, inspect the server log, check
`MODULE LIST`, and explicitly reload the known-good artifact with the saved
values. Redis remains running, but the successfully unloaded module's commands
remain absent until a load succeeds. Do not treat the pair as an atomic swap
or assume a `MULTI`/`EXEC` wrapper provides rollback.

Verify the reported version, expected commands, configuration, and a
module-specific health operation before resuming traffic. `module_version!()`
can keep the integer module version in step with the crate version. A version
number alone does not establish persistence or command compatibility.

Update the deployment's startup configuration and artifact paths separately
so a future Redis restart loads the intended version with the intended values.
Deploy and verify each server in a replica or cluster topology separately;
loading a library on one server does not install its artifact on the others.

## Make the lifecycle repeatable

Do not rely on dynamic-library unload to reset Rust statics or run their
destructors. In particular, macOS can retain an image and reuse its process-local
state on a later load. Linux often releases the image, but that is not a
portable initialization contract. Reset transient counters, caches, stopping
flags and observer history explicitly on every successful initialization.
The SDK applies macro-registered configurations before calling
`init(&RegistrationContext, ...)`; reset application state without overwriting
those loaded values.

Using a different artifact path avoids replacing a mapped file, but it is not
a portable guarantee that the loader selected new code. Verify the actual
version and behavior; use a fresh Redis process when the platform cannot
reliably replace the image.

Custom ACL categories also survive a successful unload on servers that support
them. This SDK reuses a category only when Redis reports that it already
exists; other registration failures still reject the load. Check the resulting
command membership and user access after replacement, especially when category
membership changes.

For modules with asynchronous work, add `deinit(&EventContext) -> Status` to
`redis_module!`. Gate new work before teardown writes, since writes can reenter
notification callbacks. Finish or cancel all module-owned work and release
resources while their code and required Redis context are still valid. Do not
join a worker waiting for the Redis lock from an unload hook that holds that
lock; arrange quiescence before unload. Redis-owned subscription cleanup does
not make application-owned callbacks or threads safe to leave running.
The [notification guide](KEYSPACE-NOTIFICATIONS.md#finish-callbacks-before-unload)
shows the stopping gate for a module with no workers or timers.

Treat failed initialization separately: clean up application-owned work started
by `init` before returning `Status::Err`. Do not depend on `deinit` being called
after an `OnLoad` failure. Use local ownership for fallible setup and publish
long-lived state only after it succeeds. SDK timer creation is rejected during
initialization; application-owned threads, native handles and external callbacks
still need their own cleanup. See
[Redis's load-failure cleanup](https://github.com/redis/redis/blob/8.2/src/module.c#L11786-L11800).

## Exercise the sequence locally

The [lifecycle example](../examples/lifecycle.rs) needs Redis 7.0 or later.
It has an immutable `prefix`, a mutable `limit`, and a transient visit counter.
It has no native data types, threads, timers or retained callbacks.

```sh
cargo build --example lifecycle
# In one terminal, for an isolated development server:
redis-server --port 6389 --save '' --appendonly no --enable-module-command local
# In another terminal; use .dylib on macOS and your absolute artifact path:
redis-cli -p 6389 MODULE LOAD /absolute/path/target/debug/examples/liblifecycle.so prefix live: limit 6
redis-cli -p 6389 lifecycle.visit
redis-cli -p 6389 CONFIG SET lifecycle.limit 9
redis-cli -p 6389 MODULE UNLOAD lifecycle
redis-cli -p 6389 MODULE LOAD /absolute/path/target/debug/examples/liblifecycle.so prefix live: limit 9
redis-cli -p 6389 lifecycle.state
```

The final state is `live:`, `9`, `0`: configuration was replayed and the visit
counter was reset. Loading without arguments instead yields the registration
defaults `items:`, `4`, `0`. This is a reload demonstration of the same artifact,
not proof that two different binary versions or persistence formats interoperate.

The repository tests also reject a load with `limit 0`, verify that its commands
and configuration are absent, and recover by replaying valid values. An ordinary
Redis key survives the sequence. A separate test verifies native-type unload
refusal with an empty database. Run these checks with fresh examples:

```sh
cargo build --examples
cargo test --test integration test_module_lifecycle
cargo test --test integration test_module_with_native_type_refuses_unload
```
