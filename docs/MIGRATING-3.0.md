# Migrating from redis-module 2.x to 3.0

3.0 closes ways that safe code could reach undefined behavior, returns results
that 2.x dropped, and reports errors the way Redis does. This guide lists what a
module built on 2.x changes: first what to set up, then the changes the compiler
reports, then the changes that compile unchanged but behave differently. The
[changelog](../CHANGELOG.md) lists everything in the release, including what is
new.

For replacing an already running module, see the separate
[module upgrade guide](UPGRADING-MODULES.md). For one real module's port,
side by side and with its test results, see the
[porting example](PORTING-EXAMPLE.md).

Most modules meet only a few of these. The common ones are the parameter types
of `init`, `deinit`, event and job callbacks and shared helpers, which now take
a context view (`RegistrationContext`, `EventContext` or `RuntimeContext`); the
private native context field; `Context::new` becoming `unsafe`; imports of raw
items from the crate root; `NextArg::next_str`; `try_as_str` lifetimes; the
return types of `delete`, `unlink`, `hash_set` and `hash_del`; and `?` on error
types without a `From` conversion.

## Before you start

- Rust 1.87 or later.
- Depend on `redis-module`, and on `redis-module-macros` if the module uses the
  attribute or derive macros, both from the same `git` and `tag` (or `rev`), as
  the [README](../README.md) shows. `redis-module-macros-internals` and
  `redis-module-common` come with them; do not list them. A dependency policy
  that allow-lists sources, such as cargo-deny's `allow-git`, needs this
  repository listed.
- The registration attributes (`#[command]`, the server event handler attributes
  such as `#[cron_event_handler]` and `#[flush_info_event_handler]`,
  `#[info_command_handler]` and the defrag attributes) expand through the SDK's
  own re-export of `linkme`. Remove the module's `linkme` dependency unless the
  module uses `linkme` directly.
- The crate no longer needs libclang. The bindings are pregenerated in
  `src/redisraw/bindings.rs`, and `bindgen-runtime` is no longer a default
  feature. A manifest that enables `bindgen-runtime` or `bindgen-static` can drop
  it, except for a 32-bit target: the pregenerated bindings are for 64-bit
  targets, and the build says so.
- The crate uses edition 2024. A module keeps its own edition: modules on
  edition 2021 and on edition 2024 both build with `redis_module!` and the
  proc macros.

## Native SDK dependency identity

The package declares `links = "redismodule"` (#169), so Cargo refuses a graph
with another package declaring the same native library identity. Use one SDK
revision throughout the module's dependency graph and update its Git dependency
packages together. An older SDK with no `links` key is not detected by this
check; inspect `cargo tree -d` before mixing dependencies inherited from 2.x.
The native API globals must not be linked from two SDK copies in one module.

## Macro metadata syntax

Both macro crates now use Syn 2 internally. The public macro crate parses its
existing metadata grammar directly, so modules do not need to rewrite
`#[command({ ... })]` or `#[RedisValueAttr{flatten: true}]`. Braced and typed
structs, quoted field/enum names, bare or explicit `Some`/`None`, trailing commas
and matching integer suffixes keep their existing meaning. Unknown or duplicate
fields remain errors; integer ranges and required fields are unchanged.
Generated helper names remain stable. Invalid-input diagnostic wording may
change because `serde_syn` is no longer involved.

## Changes the compiler reports

### Replication returns a status

`RuntimeContext::replicate` now returns `Status`: check `Status::Err` for a
refused command name or native replication failure. `Context::replicate_verbatim`
returns `Status` too (#220). Both return `Status::Err` without replicating
while Redis pauses writes, as `avoid_replication_traffic` reports, where Redis
would abort. Both are `#[must_use]`: a call used as a statement still compiles
but warns, and fails a build with `-D warnings`. Check the status, or discard
it with `let _ =`. Code that required a `()` return, such as a function
pointer or a unit-returning tail expression, must check or explicitly discard
the status. Flag commands that replicate `write` or `may-replicate`: during a
write pause Redis postpones them, and in the transaction that starts a pause
the SDK lets them replicate.

### Native data types implement `NativeType`

A native data type is a Rust type that implements
`native_types::NativeType`, declared as a `static` `RedisType<T>` (#154,
#155). The SDK generates the `extern "C"` callbacks Redis calls, runs each
inside a panic guard, and frees values with `Drop`. `RedisType::new` takes the
name and encoding version only.

```rust,ignore
// 2.x
static MY_TYPE: RedisType = RedisType::new(
    "mytype123",
    0,
    raw::RedisModuleTypeMethods {
        version: raw::REDISMODULE_TYPE_METHOD_VERSION as u64,
        rdb_load: Some(rdb_load),
        rdb_save: Some(rdb_save),
        free: Some(free),
        // ...
    },
);

// 3.0
impl NativeType for MyType {
    fn rdb_load(io: &mut RdbReader, _encver: i32) -> Result<Self, Error> {
        Ok(MyType { data: io.read_string()? })
    }
    fn rdb_save(&self, io: &mut RdbWriter) {
        io.write_string(&self.data);
    }
}
static MY_TYPE: RedisType<MyType> =
    RedisType::new("mytype123", 0).with_mem_usage();
```

- `rdb_load` and `rdb_save` are required. Redis calls them without checking,
  so a type that left them out crashed the server on its first save. `free`
  is the value's `Drop`.
- Load callbacks get an `io::RdbReader`, which only reads, and save callbacks
  an `io::RdbWriter`, which only writes: during a `RESTORE` the IO writes into
  the command's own argument, so a write from a load callback would corrupt
  it.
- Each optional callback is a trait with a `with_` method on `RedisType`:
  `MemUsage`, `CopyValue`, `FreeEffort`, `Unlink`, `DigestValue` (through
  `native_types::Digest`), `AofRewrite` (through `AofRewriter::emit`),
  `AuxData` and `Defrag`. A callback is registered only when the type asks for
  it, since Redis treats a missing callback differently from one that does
  nothing: without `CopyValue`, `COPY` replies that the type does not support
  it, and without `AofRewrite`, an AOF rewrite with `aof-use-rdb-preamble no`
  fails on Redis 8.10 and later and crashes its child process before that.
- A panic in `rdb_load` fails that value's load: `RESTORE` fails, and Redis
  exits when an RDB file or a replica's full sync does not load. In `copy` it
  fails the `COPY`, in `Drop` it is contained after the unwind drops the rest
  of the value, and in the others it falls back to a neutral result. A panic
  in `rdb_save`, `aux_save` or `aof_rewrite` is logged and ends the process,
  because the partly written value cannot be reported to Redis: a background
  save or rewrite's child process is killed, which fails only that save and
  lets the server remove its partial file, and the server itself aborts, as
  for `SAVE` or `DUMP`.
- `set_value` and the value accessors (`with_value` and `with_value_mut`, or
  the `unsafe` `get_value`; see
  [Key data borrows are scoped](#key-data-borrows-are-scoped)) take
  `&RedisType<T>` and take or lend `T`, so the turbofish is no longer needed
  and reading a value as another type does not compile. `T` must be
  `Send + 'static`: Redis can free values on a background thread, as
  `UNLINK`, `FLUSHALL ASYNC` and, since Redis 7.4, a
  plain `FLUSHALL` do. Hold shared data in an `Arc` rather than an `Rc`. A
  value that holds a `RedisString` needs a wrapper whose `unsafe impl Send`
  states why that is sound, as
  [string_ownership.rs](../examples/string_ownership.rs) shows.
- Both return an error for a `RedisType` that was never registered (not
  listed in `data_types`) instead of storing a value with a null type, which
  crashed Redis on the next `DEL`, `TYPE` or save. A value Redis refuses is
  dropped instead of leaked.
- `unsafe RedisType::from_raw_methods(name, version, methods)` keeps native
  callbacks for what the traits do not cover; they must treat each value as a
  `Box<T>`, and `T` need not implement `NativeType`.
- Redis 6.0 ignores `free_effort`, `unlink`, `copy` and `defrag`, which Redis
  6.2 added.
- `RedisModuleIO` adds `read_redis_string` and `write_redis_string`, which are
  binary safe.

See [data_type.rs](../examples/data_type.rs) and
[native_type.rs](../examples/native_type.rs).

### Key data borrows are scoped

Data borrowed from a key could be freed while still borrowed: the borrow tied
it to the key handle only, and `Context::call`, opening the key again or
another handle's string DMA could delete, replace or move the value (#162).
The safe APIs now copy, or lend the data to a closure that must be `Send`, so
it cannot hold the context, a key handle or a `RedisString`. While the
closure runs, the SDK also refuses calls, key opening and scans, other key
operations, `yield_now` and keyspace notifications on that thread, with an
`ERR key data is lent to a closure` error, or a panic where the API returns no
`Result`. The borrowing forms remain as `unsafe` functions whose safety
sections state the rules.

| 2.x and earlier 3.0 | 3.0 |
|---|---|
| `RedisKey::read() -> Result<Option<&[u8]>>` | `read() -> Result<Option<Vec<u8>>>`, or `unsafe read_borrowed()` |
| `RedisKey::get_value(&TYPE) -> Option<&T>` | `with_value(&TYPE, \|value\| ...)`, or `unsafe get_value` |
| `RedisKeyWritable::get_value(&TYPE) -> Option<&mut T>` | `with_value_mut(&TYPE, \|value\| ...)`, or `unsafe get_value` |
| `RedisKeyWritable::as_string_dma() -> StringDMA` | `with_string_dma(\|data\| ...)`, or `unsafe as_string_dma` |

```rust,ignore
// earlier
let mut key = ctx.open_key_writable(&name);
if let Some(value) = key.get_value(&MY_TYPE)? {
    value.count += 1;
}
key.as_string_dma()?.write(bytes)?;

// 3.0
let mut key = ctx.open_key_writable(&name);
key.with_value_mut(&MY_TYPE, |value| value.count += 1)?;
key.with_string_dma(|data| data.write(bytes).map(|_| ()))??;
```

The closures return a value (`Option<R>` for the value forms, since the key
may be empty) but cannot return a reference into the key. A closure that needs
a `RedisString`'s bytes borrows `as_slice()` before the call, since a
`&[u8]` is `Send` and the `RedisString` is not.

### Timers use typed handles

`EventContext::create_timer` now returns
`Result<TimerHandle<T>, RedisError>`. Command contexts reach it through the
event view. Creation is supported only while the module is active: move timer
setup out of `init` and into a runtime command or event after loading finishes.
Check the result and retain the typed handle when the module needs to cancel
or query the timer:

```rust,ignore
let timer: TimerHandle<String> = ctx.create_timer(
    Duration::from_secs(1),
    |ctx, value| ctx.log_notice(&value),
    String::from("ready"),
)?;
let remaining: Option<Duration> = ctx.timer_remaining(&timer)?;
let recovered: Option<String> = ctx.cancel_timer(&timer)?;
```

`cancel_timer(&handle)` replaces `stop_timer<T>(raw_id)`. It returns
`Some(data)` once after successful cancellation, `None` when the timer is
already firing, fired or cancelled, and an error for an API or lifecycle
failure. An error leaves the handle available for another query or attempt.
The SDK releases captured callback state and recovers the original payload
without casting an arbitrary native pointer to the caller's chosen type.

`timer_remaining(&handle)` replaces `get_timer_info<T>(raw_id)`. It returns
remaining time for a pending timer and `None` after firing begins or after
cancellation. It does not borrow the payload. If commands expose timer IDs,
keep an application-owned mapping from those display IDs to typed handles;
an integer is not sufficient to reconstruct a `TimerHandle<T>`.

Dropping a handle leaves its timer scheduled. This preserves fire-and-forget
usage: `ctx.create_timer(delay, callback, data)?;` checks creation without
requiring the handle to be retained. Explicit cancellation requires a live
module-owned event context and the Redis lock. A clone refers to the same
timer, so cancelling through one handle makes cancellation through another
return `None`.

Both callback captures and payloads must be `Send + 'static`; `Sync` is not
required. A timer may be registered from an event delivered on a locked worker
and later run on Redis's main thread. Copy borrowed arguments into owned
values and replace shared `Rc` captures with transferable ownership or suitable
thread-safe state. `RedisString` remains non-`Send`; retain owned `String` or
`Vec<u8>` data instead of a native string owner.

Creation validates the delay and required APIs before retaining state. Zero
delay schedules the next event-loop opportunity, positive submillisecond
delays round up to one millisecond, and durations that overflow the native
timer calculation return an error.

`redis_module!` provides the timer lifecycle binding. Creation returns an error
during module loading or initialization, during unload, or without this binding.
Redis can reject a load after the SDK's initialization hook succeeds, so timers
cannot safely retain native callbacks during initialization. Pending or firing
timers prevent unload, and a refused unload restores the active lifecycle.
Custom native loaders without this binding cannot create SDK timers; the
macro's hidden lifecycle hooks are not a stable custom-loader API. See the
[upgrade guide](UPGRADING-MODULES.md#sdk-timers-and-unload) for shutdown order.
Cancellation-on-drop and payload-inspection helpers are not part of this API.

### Key positions are checked

`Context::key_at_pos` returns `Result<(), RedisError>` instead of `()`. It
refuses a position that is not an argument position of the running command (1
to the argument count minus one), which Redis would accept and then read past
the arguments for. Propagate the result in a getkeys-api handler:

```rust,ignore
if ctx.is_keys_position_request() {
    for pos in (1..args.len()).step_by(2) {
        ctx.key_at_pos(pos as i32)?;
    }
    return Ok(RedisValue::NoReply);
}
```

The SDK checks positions in the command callbacks it generates
(`redis_module!`, `redis_command!` and `#[command]`); in a keys-position
request reached through another callback, `key_at_pos` returns an error. A
command callback registered another way can call
`unsafe fn key_at_pos_unchecked`, which keeps the old unchecked behavior.
Outside a keys-position request `key_at_pos` does nothing and returns `Ok`.

### Direct client blocking returns a result

`Context::block_client()` now returns `Result<BlockedClient, RedisError>`.
Check it before creating a worker, usually with `?`:

```rust,ignore
let blocked = ctx.block_client()?;
let worker = ThreadSafeContext::with_blocked_client(blocked);
// Send the reply through worker, then return RedisValue::NoReply.
```

The SDK rejects scripts, transactions, module calls without blocking permission,
missing clients, and attempts to block again inside reply or timeout callbacks.
Validation happens before native blocking, and a null native result cannot
become a handle. `block_client_on_reply` uses the same invocation checks and
keeps its existing result type. On Redis 6.0, where the newer denial flag is
absent, nested module-call clients are rejected through client lookup.

After successful blocking, return `RedisValue::NoReply`; returning a normal
result would produce a second reply. Worker errors must be sent through the
blocked client's thread-safe context. Dropping the handle without a reply
sends `ERR the blocked client was released without a reply`; see "Blocked
clients get one reply" under
[Changes that compile unchanged](#changes-that-compile-unchanged), and
[block.rs](../examples/block.rs).

### Clients blocked on keys

Use `Context::block_client_on_keys` for main-thread readiness callbacks rather
than putting a key-blocked native handle in a `BlockedClient`. Redis owns this
waiter and frees it after readiness, timeout or disconnect; no worker unblock
handle is returned. Supply owned `Send + 'static` data, a `KeyBlockingTimeout`,
a ready callback returning `Result<Option<RedisValue>, RedisError>` and a
timeout callback returning `RedisResult`. `Ok(None)` means keep waiting;
`Ok(Some(value))` or an error replies and unblocks. Callback `NoReply` becomes
an error because a completed wait must send one reply. To write a large reply
without building a value tree, a callback writes it through
`ctx.key_blocking_reply()` and returns `NoReply`. The writer counts the reply,
so the client still gets exactly one.

Return `RedisValue::NoReply` only for `KeyBlockingResult::Blocked`.
`KeyBlockingResult::Denied(data)` preserves the data for a non-blocking reply
inside a transaction, script or any module call, including one using `K`.
Only indexed network clients can wait. The new API requires an active module
loaded by `redis_module!` and a tracked SDK command callback; load/unload
callbacks and custom loaders without that lifecycle binding are refused.
Do not register another blocked handle for the same network client: all SDK
blocking entry points refuse that attempt and retain the first waiter.
Within an SDK-bound module, a raw command callback may invoke its handler
through the unsafe SDK `panic::run_command` trampoline.

`KeyBlockingTimeout::try_from(0_i64)` and `INFINITE` have no deadline; duration
conversions round zero and submillisecond values up to one millisecond.
Negative or excessive millisecond counts fail before native allocation.
Requested deletion wakeups are enabled on Redis 7.2+ and fall back to ordinary
readiness on older servers; the `Blocked` result reports the actual flag.
The ready callback must recheck key state. `blocked_client_ready_key` copies
Redis's borrowed name into a separate native owner, and `signal_key_as_ready`
signals the calling client's selected database. See
[key_blocking.rs](../examples/key_blocking.rs) for a complete example.

### Blocked-client contexts are not `Sync`

`ThreadSafeContext<BlockedClient>` is `Send` but no longer `Sync`. Its `reply`
writes to the client without the Redis lock, which Redis allows from one thread
at a time, so sharing one context between threads, for example through an
`Arc`, could corrupt the client's reply. Give the context to the one thread that
replies, or send the result to that thread. `ThreadSafeContext<DetachedFromClient>`
stays `Sync`.

### Open keys stay inside their opening context scope

All four `RuntimeContext::open_key*` methods now return handles borrowing the
opening context. Close keys before returning from a callback or releasing its
worker lock guard; `CloseKey` needs the live context even with AutoMemory off.
`ScanKeyCursor` owns a key and keeps the same context borrow. DMA and stream
views borrow a key and therefore also keep its opening context in scope.

Code that stores these handles must carry the context lifetime explicitly:
`RedisKey<'ctx>`, `RedisKeyWritable<'ctx>` and `ScanKeyCursor<'ctx>`. Function
parameters can usually use `RedisKey<'_>` or `RedisKeyWritable<'_>`. Helpers
returning keys must tie the result to their runtime input, for example:

```rust
use redis_module::{RuntimeContext, RedisString, key::RedisKey};

fn open<'ctx>(ctx: &'ctx RuntimeContext, name: &RedisString) -> RedisKey<'ctx> {
    ctx.open_key(name)
}
```

To retain data after a worker unlocks, copy it to `Vec<u8>` or another Rust-owned
value while locked. Closing a key early does not release the guard borrow of
other keys or views still in use. Native `RedisString` results keep their
existing thread and lock requirements; this change adds no storage API.
Unsafe raw constructors cannot infer lifetimes from pointers: callers must
still guarantee the opening context and Redis lock through the handle's drop.
Raw parts transfer the close obligation and do not extend native validity.

### Server INFO snapshots stay inside the context scope

`RuntimeContext::server_info` now borrows its producing context. Drop the
snapshot before returning from a callback or releasing a worker lock guard.
Its destructor needs the live native context even with AutoMemory disabled;
AutoMemory can also reclaim its allocation at callback cleanup. Code that kept
a snapshot after the context ended must copy the needed fields into owned
Rust data such as `String` or `Vec<u8>` inside the locked scope. A field's
`RedisString` owns its result independently of the snapshot, but still has
the existing native-string thread and lock requirements.

```rust,ignore
let version: String = {
    let guard = owner.lock();
    let info = guard.server_info("server");
    let value = info.field("redis_version").unwrap();
    value.try_as_str().unwrap().to_owned()
}; // snapshot and native string are dropped before guard unlocks
```

### Blocking-call unblock handlers own their captures

`FutureCallReply::set_unblock_handler` now requires a `Send + 'static`
handler. Redis keeps the handler until the blocked command unblocks or
`FutureHandler::abort_and_dispose` cancels it by returning `Status::Ok`;
`dispose`, dropping or forgetting the handle does not cancel it. The handler
runs on the main thread after the installing invocation returns, also when a
module-owned worker installed it. Move owned data into the handler instead of
borrowing locals:

```rust,ignore
let key = key_name.try_as_str()?.to_owned();
future
    .set_unblock_handler(move |_ctx, reply| { /* uses `key` and `reply` */ })
    .dispose(ctx);
```

A `RedisString` or `Rc` capture no longer compiles: copy the bytes into a
`String` or `Vec<u8>`, or share data through `Arc`. A `BlockedClient` capture,
as in the `call` example, compiles unchanged.

### Call replies stay inside their context scope

`RuntimeContext::call_ext` and the replies `call_blocking` resolves at once now
return `CallResult<'ctx>`, borrowing the context they were called through,
instead of `CallResult<'static>`. Redis reads that context when a reply is
freed: on 6.0 to 7.0 for every reply, and on 7.2 and later for error replies
created before the command runs and for every reply under AutoMemory, which
also frees the reply itself when the callback returns. Drop replies before
returning from a callback or releasing a worker lock guard, and copy what is
needed into Rust storage:

```rust,ignore
let bytes = {
    let guard = owner.lock();
    let reply = guard.call_ext("GET", &CallOptionsBuilder::new().build(), &["key"]);
    match reply {
        Ok(CallReply::String(value)) => Some(value.as_bytes().to_vec()),
        _ => None,
    }
}; // reply dropped before the guard unlocks
```

Functions that return a reply must tie it to their context input, for example
`fn read<'c>(ctx: &'c RuntimeContext) -> CallResult<'c>`. Closures passed to
`with_call_user` and `with_db` may now return values that borrow the view.
`Context::call` returns an owned `RedisValue` and is unchanged, as is the
`CallResult<'static>` an unblock handler receives: Redis parses that reply
without a context.

### Call replies are not `Send`

`CallReply` and `ErrorReply`, and so `CallResult`, no longer implement `Send`.
Freeing a reply can read the context that made it. On Redis 6.0 and 6.2,
replicating from the same command writes that context, so a reply moved into a
scoped thread and dropped there while the command replicated was a data race,
which ThreadSanitizer reports. Copy what another thread needs into owned Rust
data and drop the reply on the thread that made it:

```rust,ignore
let bytes = match &reply {
    Ok(CallReply::String(value)) => Some(value.as_bytes().to_vec()),
    _ => None,
};
drop(reply);
std::thread::spawn(move || process(bytes));
```

Errors convert to the owned `RedisError` with `RedisError::from`. The SDK has no
owned `Send` form of an arbitrary reply: `RedisValue` can hold a `RedisString`
and is not `Send` either, so copy into a type of your own. The same applies to
the `CallResult<'static>` an unblock handler receives; Redis parses that reply
without a context, but it now shares the type's rules. A reply also can no
longer be a timer or `block_client_on_reply` payload, a capture of a `Send`
callback such as an unblock handler, an event loop callback or a configuration
observer, or the value of a shared `RedisGILGuard`, which is `Sync` only for
`Send` values.

### `auto_memory` is unsafe

`RuntimeContext::auto_memory` is now an `unsafe fn`. The SDK releases its own
handles without AutoMemory, but AutoMemory also frees the promise reply of each
blocking call made through the context. Dispose or abort every `FutureHandler`
from such a call before the callback returns or the thread-safe context is
freed. AutoMemory stays enabled for the rest of the native context's life, so
this covers blocking calls made later through any view of the same context.
Drop call replies on the thread that holds the context. Then wrap the call with
a safety comment:

```rust,ignore
// SAFETY: every FutureHandler from this context is disposed before the
// callback returns, and its call replies are dropped on this thread.
unsafe { ctx.auto_memory() };
```

Without AutoMemory, a `FutureHandler` may still be kept and aborted later.

### Shared Redis guards require `Send` values

`RedisGILGuard<T>` is now `Send` and `Sync` only when `T: Send`. The Redis lock
serializes guarded access, but cannot protect an `Rc` alias used outside the
guard. Replace shared `Rc` state with transferable ownership or thread-safe
shared state. `T: Sync` is not required: owned `Cell` values remain supported.
Non-`Send` values can still be used in a guard confined to one thread.

`RedisString` remains non-`Send` and non-`Sync`. Replace shared
`RedisGILGuard<RedisString>` configuration storage with
`RedisGILGuard<String>` for UTF-8 or `RedisGILGuard<Vec<u8>>` for binary values:

```rust
use redis_module::RedisGILGuard;
static BYTES: RedisGILGuard<Vec<u8>> = RedisGILGuard::new(Vec::new());
// The existing string registration group accepts &BYTES.
```

Both byte guards and `Mutex<Vec<u8>>` implement
`ConfigurationValue<RedisString>`. They preserve empty values, NUL and invalid
UTF-8 by copying the accepted bytes. The getter creates its native string
while Redis is locked. Registration defaults and the macro's module-argument
conversion keep their existing UTF-8 syntax. Custom native-handle retention
still needs its own thread, alias and destruction guarantees; sharing a guard
does not provide those guarantees.

The generic guard implementation of `ConfigurationValue<T>` now requires
`T: Clone + Send`. The `ConfigurationValue` trait itself still requires only
`Sync` storage, preserving custom shared storage that is not `Send`.

### Configuration observers require `Send`

The optional `on_changed` closures passed to all four
`configuration::register_*_configuration` functions, including the
`redis_module!` configuration block, must be `Send`. A worker holding the Redis
lock can call `CONFIG SET` and invoke the observer on its own thread. The lock
serializes invocation but cannot make captures such as shared `Rc` values
safe to move between threads.

Function callbacks and closures that capture nothing are unchanged. Replace
shared `Rc` state with `Arc` and thread-safe storage, or use atomics or
Redis-locked storage. If the capture is private to the closure, owned `Cell`
or `RefCell` state is allowed: `Sync` is not required. Explicit trait object
types must change from `Box<dyn Fn(...)>` to `Box<dyn Fn(...) + Send>`.
`ConfigurationContext` remains confined to the setter's thread, and registered
`ConfigurationValue` storage still requires `Sync` rather than `Send`.

See the [configuration notes](CONFIGURATION.md#validate-before-storage-observe-afterward)
and [observer example](../examples/configuration_observers.rs).

### Module names are constant strings

`redis_module!` requires `name:` to be a constant `&str` expression without
NUL bytes. Ordinary string literals are unchanged. Move runtime-computed names
to a string constant, `concat!`, a const function, or compile-time environment
input, for example:

```rust
const MODULE_NAME: &str = concat!("prefix_", env!("CARGO_PKG_NAME"));
// In redis_module!: name: MODULE_NAME,
```

The macro creates the terminated C string in constant evaluation without a
fixed buffer size. The old unchecked 64-byte copy could write past the buffer
for longer names; an exactly 64-byte name also left no terminator. Embedded
NUL bytes previously truncated the name Redis received. These are now rejected
at compile time. Long NUL-free names, including UTF-8 names, are supported.
This concerns the module name; native data-type names still have their own
9-byte restriction.

### Load-time registration uses `RegistrationContext`

`CommandBuilder::register`, `commands::register_commands`,
`RedisType::create_data_type`, all four `configuration::register_*_configuration`
functions, and `configuration::load_configurations` now take
`&RegistrationContext<'_>`. `set_module_options` moved from `Context` to
`RegistrationContext`.

Change the parameter of an initialization hook that uses these APIs:

```rust,ignore
fn init(ctx: &RegistrationContext<'_>, args: &[RedisString]) -> Status {
    // Register commands, types, configurations, and module options here.
}
```

`redis_module!` creates the registration context after native initialization
succeeds and passes it through the load sequence. Both command declaration
syntaxes and ordinary `fn command(ctx: &Context, ...)` handlers are unchanged.
The registration wrapper dereferences to `EventContext`, then `RuntimeContext`.
Helpers shared with commands or workers should take `&RuntimeContext` (see below).
It also implements `RedisLockIndicator`, so configuration guards still accept it.
Runtime-only init hooks must change their `&Context` parameter to
`&RuntimeContext`, `&EventContext`, or `&RegistrationContext<'_>`. `deinit`
takes `&EventContext` (or `&RuntimeContext`, which it coerces to). A hook that
still takes `&Context` is a `mismatched types` error (E0308) reported at the
`redis_module!` invocation, with a note pointing at the hook: `expected
&Context, found &RegistrationContext<'_>` for `init`, `found &EventContext` for
`deinit`.

There is no safe conversion from runtime `Context` to `RegistrationContext`.
Move late command/type/configuration registration into initialization. Custom
native loaders may use unsafe `RegistrationContext::from_context` only during
their own active `RedisModule_OnLoad`, after successful initialization and
with a live native module association and the Redis lock held. It cannot be
used to promote a command or worker context outside load.

The wrapper borrows the load invocation and cannot be cloned or retained after
it. Module options are intentionally configured during initialization by this
SDK, even where Redis itself permits changing them later. Keyspace and server
event subscriptions remain available through the module-owned event view.

### Events and locked workers have separate context views

Ordinary command handlers and blocked-client reply/timeout handlers keep
`&Context`. Other callbacks use a narrower type:

| Where | Context | Capabilities |
|---|---|---|
| Module init | `&RegistrationContext<'_>` | Registration, event operations, shared runtime operations |
| Client command or blocked-client reply/timeout | `&Context` | Client replies/blocking/key positions, event operations, shared runtime operations |
| Keyspace/server/timer/post-notification/event-loop callback, module unload, blocking-call unblock handler | `&EventContext` | Module-owned timers/subscriptions/shared APIs, shared runtime operations |
| `ThreadSafeContext::lock` / `try_lock` or `DetachedContext::lock` / `try_lock` | Guard dereferencing to `WorkerContext` | Shared runtime operations; blocking calls only for module-owned detached workers |
| Database scan callback or helper shared across these scopes | `&RuntimeContext` | Calls, keys, database selection, logging, server info, explicit replication |
| Module INFO callback, including crash reports | `&InfoContext` | INFO output only; no Redis lock proof or runtime operations |

`add_post_notification_job` and the event subscriptions are `EventContext`
methods. A keyspace handler that defers writes keeps `&EventContext` down to the
function that schedules the job; the job receives a fresh `&EventContext`, and
the write helpers it calls can take `&RuntimeContext`.

Change callback parameters from `&Context` to the type in this table. A helper
that calls Redis without needing a client or module ownership should accept
`&RuntimeContext`; commands, events, init hooks, and worker guards all coerce to
it. Method calls on ordinary command contexts still work through dereferencing.
Qualified references to moved methods, such as `Context::get_flags`, must use
`RuntimeContext::get_flags` or a closure such as `|ctx| ctx.get_flags()`.
`RedisString::safe_clone` and `init_api` also accept `&RuntimeContext`.

```rust,ignore
fn read(ctx: &RuntimeContext, key: &str) -> RedisResult {
    ctx.call("GET", &[key])
}
fn on_timer(ctx: &EventContext, key: String) {
    let value = read(ctx, &key);
    // Store the result or notify a blocked client through its reply handle.
}
```

Event and worker views have no `reply*`, `block_client*`, key-position discovery,
`replicate_verbatim`, or current-client user queries, and cannot be converted
to `Context` by safe code. Use explicit `replicate(command, args)` outside a
command. `scoped_call_user` remains on `RuntimeContext`: it sets the context
user for ACL-checked calls, including on workers and events, and does not
authenticate a client connection. Dropping an inner scope restores the outer
user, including when work unwinds. Forgotten scopes are cleaned up when their
runtime view ends, before a worker unlocks. Worker guards cannot create timers
or subscribe to events. Configuration
and command/type registration still require `RegistrationContext`.

`yield_now` moved from `RuntimeContext` to `EventContext` (#156). Existing
command and event calls keep their syntax. A helper that yields must take
`&EventContext` or `&Context`, rather than the shared runtime view. Locked
workers (`ContextGuard`, `DetachedContextGuard` and `WorkerContext`) cannot
call it: Redis may release their lock inside a yield while borrowed keys or
guarded state remain live. Finish using that data and drop the lock guard
before allowing other threads to run; reacquire a fresh guard for later Redis
access. No replacement worker-yield API is provided. Main-thread callbacks
can still reenter while yielding, so release locks and borrowed key data that
those callbacks could invalidate before calling it.

A command or event callback can also run synchronously inside a worker's
`call`. Its `yield_now` returns an error before entering Redis. Main-thread
verification supports Linux and macOS; other platforms also return an error
rather than assuming the callback runs on the Redis main thread.

Both guard types borrow their owner and cannot outlive it. Their runtime views
are neither `Send` nor `Sync` and cannot outlive the lock guard. A
`ThreadSafeContext<BlockedClient>` retains its separate `reply` method, callable
without the Redis lock. It does not create a general context or lock proof.
`WorkerContext::call_blocking` returns an error for ordinary module-less worker
contexts before entering Redis, where a real blocking result requires a module
association. `MODULE_CONTEXT.lock()` retains that association and supports it.

Custom native integrations must choose their unsafe constructor deliberately:
`Context::new` (and its `from_module_context` alias) now requires both a live
module association and a client reply target or key-position request.
`EventContext::new` requires module ownership but no client; `RuntimeContext::new`
requires only a live context and the Redis lock. All require valid invocation and
lock scope throughout use. Replace `Context::dummy` with `RuntimeContext::dummy`
for APIs that explicitly accept a null context, such as logging.

Prefer `ThreadSafeContext::try_lock` or `MODULE_CONTEXT.try_lock()` when a
worker must check a stop flag instead of waiting for the Redis lock. They
return the existing guard on success and `None` on contention; the guard
handles view cleanup and unlocking. Stop and join workers before unload.

A view built over a lock the module took itself, for example after
`RedisModule_ThreadSafeContextTryLock`, must be dropped before the module
unlocks, because dropping a runtime view ends its call-user scopes through the
native context. Drop it explicitly, then unlock. In a guard type whose own
`Drop` unlocks, hold the view in an `Option<RuntimeContext>` and `take()` it
before the unlock, or in `ManuallyDrop` with an `unsafe` `ManuallyDrop::drop`
before the unlock, because fields drop only after the guard's `drop` body.

This split limits capabilities. Keys, server INFO snapshots and call replies
also borrow the context they came from, as the sections above describe. Release
other native resources obtained through raw APIs before the callback returns
or the worker guard is dropped.

### Post-notification jobs and module unload

Modules using `redis_module!` now refuse unload before user deinit while Redis
owns an accepted SDK post-notification job. Retry unload after the enclosing
execution unit finishes, even if the application cancelled its logical work.
Submission during module load or unload returns `Status::Err` and drops
captures immediately; move that scheduling into commands or events after
loading. The existing compatibility-dependent return types are unchanged.
Raw job submissions and custom native entrypoints still require their own
lifetime protection. See [the notification guide](KEYSPACE-NOTIFICATIONS.md).

### Scoped call users

Rename `ctx.authenticate_user(&username)` to `ctx.scoped_call_user(&username)`.
The returned guard is `CallUserScope`, replacing `ContextUserScope`, and can be
imported from `redis_module::CallUserScope`. Keep it alive while making calls
with `CallOptionsBuilder::verify_acl` enabled:

```rust,ignore
let scope: CallUserScope<'_> = ctx.scoped_call_user(&username)?;
let reply: CallResult = ctx.call_ext("GET", &acl_options, &["allowed:key"]);
drop(scope);
```

`acl_options` must enable `verify_acl`. The method selects the context's user
for these calls; it does not authenticate a client connection. Dropping the
guard restores the most recent SDK scope that is still active, including
on unwind. An inner scope now restores its outer user instead of clearing
the override. Guards can be dropped out of order; expired scopes are skipped.

The closure form keeps the context view's capabilities:

```rust,ignore
let reply: CallResult = ctx.with_call_user(&username, |ctx| {
    ctx.call_ext("GET", &acl_options, &["allowed:key"])
})?;
```

A lookup failure returns `Err` without running the closure or changing the
active user. Otherwise the helper returns `Ok(closure_result)`, so a closure
returning `Result` produces a nested `Result`. A panic also ends the helper's
scope. Explicit guards created inside the closure follow their own lifetimes.

After all SDK scopes end, Redis 8.8+ restores any prior native context override;
earlier versions clear it because they cannot query it. Avoid mixing scopes
with raw `SetContextUser` calls. Forgetting a guard keeps its user active until
its runtime view ends; worker guards clean up before unlocking. Do not delete
an ACL account while its scopes are active: Redis's native user handles borrow
the account.

Scoped users and the key/command/channel ACL helpers check the account's rules.
Native lookup and permission checks can still succeed for an existing account
marked `off`; these operations do not authenticate a client or guarantee that
the account is enabled. Check its current enabled state separately when
deferred work must stop after disabling its owner. This clarification preserves
the existing runtime behavior, return types and error strings.

### Raw items are under `redis_module::raw`

2.x re-exported all of `raw` at the crate root. 3.0 re-exports only `Status`,
`KeyType`, `NotifyEvent`, `Version` and `ModuleOptions` there.

```rust
// 2.x
use redis_module::{RedisModuleString, REDISMODULE_OK};

// 3.0
use redis_module::raw::{RedisModuleString, REDISMODULE_OK};
```

### `raw::RedisModule_GetUserUsername` takes the context (3.0.0-alpha.1 only)

No 2.x release declares it. 3.0.0-alpha.1 declared it with the user alone,
from a Redis unstable snapshot; Redis 8.8 and later define it with the
context first, so a call through the old declaration passed the user where
Redis reads the context and could not have worked.

```rust
// 3.0.0-alpha.1
let name = unsafe { raw::RedisModule_GetUserUsername.unwrap()(user) };

// 3.0
let name = unsafe { raw::RedisModule_GetUserUsername.unwrap()(ctx, user) };
```

### Raw calls with wrappers in 3.0

2.x modules called raw bindings where 2.x had no wrapper, or where its wrapper
fell short. None of these changes is required; the raw calls still work, with
the context pointer from `get_raw()` (see below). These now have a wrapper:

- `RedisModule_GetSelectedDb` and `RedisModule_SelectDb`:
  `RuntimeContext::get_selected_db`, `select_db` and `with_db`.
- `RedisModule_Milliseconds`: `redis_module::milliseconds()`.
- `RedisModule_SubscribeToKeyspaceEvents` with a mask chosen at runtime:
  `EventContext::subscribe_to_keyspace_events` with a `redis_event_handler!`
  handler. Event names are decoded lossily, and panics are caught and logged.
- `RedisModule_NotifyKeyspaceEvent` with an event name that is not UTF-8:
  `RuntimeContext::notify_keyspace_event_bytes`.
- Flush payloads and SWAPDB: `#[flush_info_event_handler]` and
  `#[swapdb_event_handler]`.
- An ACL category kept from an earlier load: `redis_module!`'s `acl_categories`,
  which accepts the `EBUSY` Redis returns for it.

Limits: the server event attributes subscribe on every load, before `init`, and
fail the load if a subscription fails, so a subscription that depends on
configuration stays raw. On Redis 7.2 and 7.3, which lack
`RedisModule_AddACLCategory`, a module's own ACL category cannot be applied to
its commands: a required one (`acl_categories`) fails the load, and an optional
one (`optional_acl_categories`) is dropped with a logged warning for each
command. A module that wants to skip the attempt, or log a single notice, keeps
the raw calls.

### Functions that take raw pointers are unsafe or private

A safe function must not accept a pointer it dereferences or passes to Redis,
so these are now `unsafe fn`, each with a `# Safety` section:

- `Context::new`, `EventContext::new`, `RuntimeContext::new` and `RuntimeContext::dummy`
- `RedisString::new`, `RedisString::from_redis_module_string`,
  `RedisString::from_ptr`, `RedisString::string_as_slice` and `decode_args`
- `RedisKey::open` and `open_with_flags`, and `RedisKeyWritable::open`,
  `open_with_flags` and `open_with_redis_string`
- `InfoContext::new`, `RedisBuffer::new`, `RedisModuleIO::new`
- `logging::setup_for_context`, `logging::log_io_error`,
  `DefragContext::defrag_dealloc`
- in `raw`: the RDB `save_*` and `load_*` helpers, `is_io_error`, `redis_log`,
  `call_reply_verbatim_string`, and the doc-hidden `register_info_function`

Inside a command or callback there is already a typed context; use its safe
methods instead:

```rust
// 2.x
let key = RedisKeyWritable::open(ctx.ctx, &name);

// 3.0
let key = ctx.open_key_writable(&name);
```

The other `raw` helpers that took pointers (replies, call replies, keys, hashes,
lists, strings, info fields, replication) are no longer public. The methods on
`Context`, `RedisKey`, `RedisKeyWritable`, the call reply types and
`InfoContext` cover them, and the bindings in `raw` remain for anything else.
`key::verify_type` is no longer public either.

Two patterns still need native bindings rather than a method equivalent:

- Piece-by-piece aggregate replies use `RedisModule_ReplyWithArray`,
  `RedisModule_ReplyWithMap`, `RedisModule_ReplyWithStringBuffer` or
  `RedisModule_ReplyWithString` with `ctx.get_raw()`. The module must emit
  exactly the announced element count and obey the native reply protocol.
  Prefer a complete `RedisValue` tree where practical; `InsertionOrderedMap`,
  `BulkRedisString` and `NullArray` cover repeated binary keys, native string
  replies and stream-style absent arrays without these raw calls.
- For an unwrapped key API, open with `ctx.open_key(&name)` or
  `ctx.open_key_writable(&name)` and borrow `key.as_ptr()`. Keep that SDK
  owner alive for every native call; its Drop still closes the handle. Native
  calls remain unsafe and must respect the open mode, the opening context and
  Redis lock, scoped key-data loans and native aliasing rules. Do not close the
  borrowed pointer, rebuild a second owner or keep it beyond that scope. A
  read-only open of a missing key can return a null pointer. `to_raw_parts`
  is only for intentionally transferring a read-key handle's ownership;
  after that transfer the caller must close it exactly once or reconstruct
  exactly one owner with `from_raw_parts`.

When adopting an owned string returned by a native API, a null producer context
is correct only when the string has no AutoMemory registration, such as a
producer context that never enabled AutoMemory. Pass the real producer context
otherwise: `RedisString::from_owned_ptr` detaches its registration with a native
`RetainString` plus `FreeString` pair, preserving one independent owner. Do not
use a null producer merely to save those calls. The compatibility constructor
`from_redis_module_string` follows the same rule.

The native context pointer is private on every context view and on
`InfoContext`. For `Context`, `EventContext`, `RegistrationContext` and
`WorkerContext` the compiler names the field on `RuntimeContext`, which they
dereference to. `get_raw()` returns the pointer; first check [Raw calls with
wrappers in 3.0](#raw-calls-with-wrappers-in-30).

`raw::export_shared_api` and `Context::export_shared_api` return the `Status`
Redis reports (`Err` when the name is taken), and the name must stay valid for
as long as the server runs.

### `RedisString` and argument parsing

`RedisString` no longer implements `Borrow<str>` (#164). Its replacement,
`Borrow<[u8]>`, matches the owned type's byte-slice hashing and binary order.
Equality and ordering compare the byte slices directly, avoiding native
comparison truncation for large prefix-length differences.
Change collection lookups from `set.contains("NX")` to
`set.contains("NX".as_bytes())`, and from `map.get(name)` to
`map.get(name.as_bytes())` when `name` is a `str`. Byte queries preserve NUL
and invalid UTF-8, without substituting a text placeholder.
Slices of `RedisString` no longer support text `join` implicitly; convert
each value to validated or explicitly lossy text before joining it.

```
use redis_module::RedisString;
use std::collections::HashMap;
fn find<'a, T>(values: &'a HashMap<RedisString, T>, name: &str) -> Option<&'a T> {
    values.get(name.as_bytes())
}
```

- `try_as_str` borrows from the `RedisString`, so the `&str` cannot outlive it.
- `NextArg::next_str` is removed: it returned a view into the string it had
  just dropped. Use `ArgCursor`, which borrows the arguments without copying,
  or `next_arg` followed by `try_as_str`.
- The `inner` field is private; use `as_ptr()`.
- `Context::get_current_user` returns `Option<RedisString>`.
- `ScanKeyCursor::scan` and `for_each` callbacks receive the value as
  `Option<&RedisString>`, and `KeysCursor::scan` takes the callback as `&mut F`.
- A keyspace event handler gets the event name and key borrowed for the
  callback, so a handler declared with `&'static str` or `&'static [u8]` no
  longer compiles; declare plain `&str` and `&[u8]` and copy what it keeps.

```rust
// 2.x
let mut args = args.into_iter().skip(1);
let key = args.next_arg()?;
let field = args.next_str()?;

// 3.0
let mut args = ArgCursor::new(&args[1..]);
let key = args.next_arg()?; // &RedisString
let field = args.next_str()?; // &str, borrowed from args
args.done()?;
```

### Keys

- `RedisKeyWritable::delete` and `unlink` return `Result<bool, RedisError>`:
  `true` when the key held a value.
- `hash_set` and `hash_del` return `Result<usize, RedisError>`, the number of
  existing fields replaced or deleted, instead of a `Status` that read a
  replaced field as an error. `hash_set_by_string` and `hash_del_by_string`
  take the field as a `RedisString`, so it can hold any bytes.
- Data borrowed from a key is scoped; see
  [Key data borrows are scoped](#key-data-borrows-are-scoped).

### Replies

`RedisValue` has three new variants: `InsertionOrderedMap`, which replies with
the entries in the order the module added them; `Error(String)`, an error
reply with an owned message, which can be an array element or a map value;
and `NullArray`, a RESP2 null array (`*-1`) or RESP3 null (`_`) (#180).
Use `NullArray` for stream-style absent arrays, including inside aggregates.
`Null` remains a RESP2 null bulk string (`$-1`), and `None` and `()` still
convert to `Null`. Exhaustive matches on `RedisValue` need three more arms.

`RedisValueKey` has a new variant, `SimpleString`, for a map key or set element
replied as a simple string. A map or set converted from a call reply holds it
for a simple-string key or element, where 2.x gave `String`; an exhaustive
`match` on `RedisValueKey` needs another arm.

An error in a call reply converts to `RedisValue::Error`, also inside an array
or map, where 2.x gave `StringBuffer` with the error's bytes. A module that
forwards the reply now replies with the error, as Redis did; one that read the
bytes from `StringBuffer` reads the message from `Error` instead.

### Expiration and crash INFO

`RedisKeyWritable::set_expire` rejects an overflowing absolute millisecond
value without changing the existing key or TTL (#166). Redis 6.2+ uses the
checked absolute setter. On Redis 6.0 the checked relative fallback cannot
atomically share its clock sample with the native setter's later reading, so
an extreme-boundary clock change remains subject to that old native API.

The default crash-info path no longer adds a Rust backtrace section. Module
INFO callbacks still receive the same crash-report flag. Continue skipping
ordinary shared-state access in those callbacks; omitting backtrace capture
is not a general async-signal-safety guarantee.

### Errors

SDK-generated failures now start with `ERR` or a specific Redis code (#166).
Update exact-message assertions and any code that treated the old first word,
such as `Couldn't`, as an error code. Explicit `RedisError::Str`/`String`
messages and errors forwarded from Redis keep their supplied text and code.

`RedisError` implements `std::error::Error`. That replaces 2.x's blanket
`impl<T: std::error::Error> From<T> for RedisError` with an explicit list of
`From` conversions: the standard library errors module code meets (`io::Error`,
`Utf8Error`, `ParseIntError`, `NulError`, `PoisonError<T>` and the others among
`RedisError`'s trait implementations). For any other error type, convert
explicitly:

```rust
let value = parse(text).map_err(RedisError::from_error)?;
```

`from_error` takes any `Display`, so `anyhow::Error` and `Box<dyn Error>` work.

### Configuration

- `register_enum_configuration` rejects `ConfigurationFlags::BITFLAGS`
  before native registration (#166). Remove this flag for an ordinary enum;
  combined flags are not represented by the typed enum setter. Module-argument
  option names now ignore ASCII case, matching Redis's parser.
- `register_i64_configuration`, `register_string_configuration`,
  `register_bool_configuration` and `register_enum_configuration` return
  `Result<(), RedisError>`. A module that registers configurations outside
  the `redis_module!` `configurations` block calls
  `configuration::load_configurations` after the last registration.
- `ConfigurationValue` no longer requires `Send`. `ConfigurationContext` is
  neither `Send` nor `Sync`.
- `RedisGILGuard<T>` is `Send` and `Sync` only when `T: Send`; see the guard
  storage migration above.

### Data types

- Native data types implement `NativeType`; see
  [Native data types implement `NativeType`](#native-data-types-implement-nativetype).
- `RedisType::raw_type` is a method returning `*mut RedisModuleType` instead of
  a public `RefCell` field.
- `RedisType::create_data_type` takes `&RegistrationContext<'_>`.
- A type name or encoding version that Redis would reject fails to compile
  instead of failing the module load. The name is 9 characters from
  `A-Z a-z 0-9 - _`.

### Command flags

The flags in `redis_module!`'s `commands` list must be a constant expression,
usually a string literal. They are checked when the module compiles, so a
misspelled flag is a compile error instead of a load failure. Flags that start
with `_` are accepted as they are. `redis_command!` and `redis_event_handler!`
take `&Context` instead of a raw pointer.

### Raw enums

- `KeyType` and `ReplyType` are `#[non_exhaustive]`, so a `match` on them
  needs a wildcard arm. `KeyType` gains `Array`, for Redis 8.8 arrays, and
  `Unknown` (for values a newer Redis may return), and `ReplyType` gains
  `Attribute` and `Promise`. Both
  convert from an `i32` with `From` instead of panicking.
- The raw enums are `#[repr(i32)]` and no longer have the `FromPrimitive`
  methods; the `enum-primitive-derive` and `num-traits` dependencies are gone.
- The opaque handle types in `raw` (`RedisModuleCtx`, `RedisModuleKey`,
  `RedisModuleString` and the others with no fields) no longer derive `Copy`
  and `Clone`. Modules hold pointers to them, never values.

## Changes that compile unchanged

### RDB text must be UTF-8

`RedisModuleIO::read_string` now returns an error when the stored bytes are not
valid UTF-8. Previously it replaced invalid bytes with U+FFFD, and a later save
could persist the altered value. Read arbitrary bytes with
`read_string_buffer` and choose an explicit decoding policy if the module's
format permits lossy text.

Converting native RESP3 maps and sets into `RedisValue` also returns an error
value for an aggregate containing a key or member outside `RedisValueKey`'s
supported scalar variants, instead of panicking. Forward the native call
reply directly when its full representation is required.

### Replication during a write pause

`replicate`, `replicate_verbatim` and calls with a replicate flag now refuse
while `avoid_replication_traffic` is true (#220). Redis 6.2 aborts on such
replication, and 7.0 and later abort only when they propagate the command,
with the AOF on or a replica or replication backlog. On Redis 6.0, and on 7.0
and later without AOF or replicas, the same replication used to return
`Status::Ok`, or run the called command, and now returns `Status::Err` or an
error. A command flagged `write` or `may-replicate` that its client queued in
the transaction that starts the pause still replicates.

A post-notification job's calls are refused during the pause too, with or
without a replicate flag (#231): `call`, `call_ext` and `call_blocking` from a
job return
`ERR post-notification jobs cannot call commands while Redis pauses replication`
without running the command. A job's call after a transaction that paused
writes itself made Redis 7.2 and later abort when they propagated the
transaction, and the SDK cannot tell that job from others. A job that read
data through a call during a pause gets the error now; read keys with
`open_key`, which runs no command, or copy the data before queuing the job.
During a pause a job's `yield_now` processes events and serves no clients,
even with `YieldMode::Clients`: a client command run during the yield, such
as a replica's `REPLCONF ACK`, would make Redis abort the same way.

Outside a transaction a call can no longer start a pause (#235): `call`,
`call_ext` and `call_blocking` return
`ERR calls cannot pause replication outside a transaction` for
`CLIENT PAUSE` in any mode and `FAILOVER` other than `FAILOVER ABORT`, and
`call_blocking` for `SHUTDOWN` without `NOW` or `ABORT`, without running the
command. Redis 6.2 and later abort when replication queued earlier in the same command, timer,
event or worker propagates during the pause, and from 7.0 that includes
deleting an expired key the module opened. A module that paused clients this
way gets the error now, also when nothing was queued and on Redis 6.0; send the
pause from a client, or call it from a command inside `MULTI`/`EXEC`.
`CLIENT UNPAUSE` and other `CLIENT` subcommands still run.

### Missing optional APIs on older Redis 6.0 servers

`ThreadSafeContext::try_lock` and `MODULE_CONTEXT.try_lock()` return `None`
when the native nonblocking lock API is missing, as well as when the lock is
contended (#167). They never wait for a blocking acquisition. A module that
requires nonblocking locking can check
`ctx.api_available("ThreadSafeContextTryLock")` before starting its worker.

Keyspace `event_handlers` and runtime subscriptions can load without native
flag enumeration. They pass the requested mask unchanged in that case:
`NotifyEvent::all()` means all flags known to this SDK, while the running
server only emits events it implements. Servers that provide flag enumeration
keep filtering unsupported bits. Native subscription support is still
required before `init` runs.

These compile without a change, so the compiler will not point at them. Check
the code that depends on them, and the tests and clients that match on reply
text.

- **Configuration wrapper commands need `CONFIG` permissions.** The commands
  named by `module_config_get` and `module_config_set` are now admin commands
  that scripts cannot call, in the `@admin` and `@dangerous` categories on
  Redis 7.2 and later, and they check the caller's permission for
  `CONFIG GET` or `CONFIG SET`. Users who could run them without `CONFIG`
  permission now get `NOPERM` (on Redis 7.0, `ERR acl verification failed`);
  grant `@admin`, or the wrapper and `+config|get` and `+config|set`, to users
  who should read or change the module's configurations. On Redis 7.0, where
  ACL categories do not cover module commands, allow the wrapper by name and
  `+config|get` and `+config|set`. A module calling its own wrapper through
  `ctx.call`, or calling `module_config_get` or `module_config_set` from a
  timer or thread-safe context, now gets an error, since there is no user to
  check; call `CONFIG` directly there.

- **Event loop callbacks and the module lifecycle.** With `redis_module!`,
  `event_loop::add_one_shot` returns an error while the module loads or
  unloads, so schedule one-shots from commands, events or workers rather than
  `init` or `deinit`, and `MODULE UNLOAD` is refused while a one-shot is still
  pending or a descriptor callback is running. A worker can get that error
  while an unload is attempted and then refused; retry it. `event_loop::add`
  returns an error during and after unload, and a successful unload stops
  watching every descriptor still watched after `deinit`, as a failed load
  does for those it added; the descriptors stay open. A callback that panics
  is no longer watched in any direction it handles; before, it stayed
  registered and was skipped on every event. Modules with their own
  `RedisModule_OnLoad` are unaffected and must still remove watches and let
  one-shots run before unloading.

- **`event_handlers` subscribe after `init`.** `redis_module!` now subscribes
  the `event_handlers` keyspace handlers once `init` has succeeded, as the last
  step of the load, so events raised during `init` no longer reach them. Redis
  before 7.2.4 keeps a failed load's keyspace subscriptions and has no API to
  remove them, which this avoids. Subscribe from `init` with
  `subscribe_to_keyspace_events` if a handler must see those events, at the
  cost of that subscription surviving a later load failure on those servers.
  Redis calls keyspace subscribers in subscription order, so handlers a module
  subscribes from `init` now run before its `event_handlers` entries.

- **Blocked clients get one reply.** Dropping a `BlockedClient`, or the
  `ThreadSafeContext` that holds it, without replying now sends
  `ERR the blocked client was released without a reply`; before, the client
  usually waited forever. A handle dropped by the command that blocked the
  client, on the command's own thread and before it returns, is the
  exception: the command's own reply, such as an error returned with `?` or
  the error for a panic, stays the only one, and a `NoReply` it returns
  becomes the error. A command that replied normally after blocking and let a
  worker drop the handle without replying, to hold the client until the work
  finished, now sends a second reply; return `NoReply` and reply through the
  worker instead. A second `ThreadSafeContext<BlockedClient>::reply` returns
  `Status::Err` and writes nothing; before, the client read it as the reply to
  its next command. `RedisValue::NoReply` through that context sends nothing
  and does not count, and `RedisError::WrongArity` is sent as
  `ERR wrong number of arguments`.
- **Nested `NoReply` and big number text.** A `RedisValue::NoReply` array
  element or map value is now replied as null; it used to write nothing, and
  the client then read the next command's reply in its place. A top-level
  `NoReply` still sends nothing. `RedisValue::BigNumber` text that is not an
  optional `-` followed by decimal digits is now the error
  `ERR big number reply is not a decimal integer` for every client. Before,
  `BigNumber("3.14".into())` reached a RESP2 client as the bulk string `3.14`,
  and text with CR or LF injected replies for RESP3 clients of Redis 7.0 and
  later. Inside an array or map the error takes the big number's place, and
  clients such as redis-rs then report the whole reply as an error. Send other
  numeric text as a bulk string or a double instead.
- **Detached context after unload.** `MODULE_CONTEXT` is released when
  `redis_module!` unloads the module or its load fails, and the next load stores
  a fresh one. Until then `MODULE_CONTEXT.lock()` panics instead of using the
  freed module's context, and event loop one-shots that Redis still runs are
  skipped (with `redis_module!` none can be pending at unload, and watches are
  removed); a worker that outlives a reload uses the new load's context. Stop
  workers, including ones that log, before unloading. Calling `logging::setup()`
  again, as on a reload, succeeds instead of returning an error.
- **Unload with pending blocking calls.** `MODULE UNLOAD` now fails while an
  unblock handler installed with `set_unblock_handler` has neither run nor been
  aborted. Previously the unload succeeded and Redis later called the handler
  through freed module state. Keep a handle to abort such calls, or let them
  unblock, before unloading. `call_blocking` also returns an error while the
  module loads or unloads, so start blocking calls from commands or events
  rather than `init` or deinit; see the
  [upgrade guide](UPGRADING-MODULES.md#blocking-calls-and-unload).
- **Repeated call protocol selections.** `CallOptionsBuilder::resp` now replaces
  its previous protocol selection. For example, `.resp(Resp3).resp(Resp2)`
  produces RESP2 call replies, and `.resp(Resp3).resp(Auto)` follows the client's
  protocol. Previously, the stale RESP3 flag took precedence. RESP2 remains the
  default and the method signatures are unchanged. This also applies to
  `build_blocking`. RESP3 and Auto module-call formats require Redis 7.0 or
  later; older Redis rejects those flags. Selecting RESP2 removes them even
  when an earlier selection requested RESP3 or Auto.
- **Keyspace subscription ownership.** Subscriptions require `EventContext`,
  also exposed by command and registration contexts. Redis removes the callback
  when its owning module unloads. A raw context obtained from
  `RedisModule_GetThreadSafeContext(NULL)` has no native module identity and must
  never be promoted to an event or command context. Worker subscriptions are
  rejected by the compiler; register them from init, commands, or event callbacks.
- **Call results.** `Context::call` returns `SimpleString` for a simple string
  reply and `BulkString` for a bulk string reply (`StringBuffer` when it is not
  UTF-8); 2.x returned `SimpleString` for both when the text was UTF-8, so a
  `match` on `RedisValue::SimpleString` may now miss. A bulk reply a command
  returns reaches the client as a bulk string, as Redis sent it.
  `StringCallReply::is_simple_string` tells the two apart in `call_ext`, and
  `Context::reply_with_call_reply` forwards a reply exactly.
- **Failed calls.** A call that fails without the `E` flag returns `Err` naming
  the reason, for example `ERR RedisModule_Call failed: unknown command`. In
  2.x, `call_ext` returned `CallReply::Unknown` and `Context::call` returned
  `Ok(RedisValue::StaticError("Error on method call"))`.
- **Panics.** A panic in a command replies `ERR command panicked: <message>`
  instead of aborting the server. A panic in a callback with no client (events,
  timers, INFO, configuration getters, defrag) is logged at warning level.
  A panic in a configuration setter fails the `CONFIG SET`; one in OnLoad
  fails the load. Native data type callbacks are guarded as
  [their section](#native-data-types-implement-nativetype) describes. This
  needs the default `panic = "unwind"`; callbacks a module registers itself
  through the raw API can use `panic::catch_panic`.
- **`RedisGILGuard`.** Locking a guard that is already locked panics instead of
  returning a second mutable reference.
- **Missing module APIs.** Calling an API the running Redis lacks returns or
  panics with `RedisModule_X is not available in this Redis server` instead of
  `called Option::unwrap() on a None value`.
- **Errors on the wire.** A key of the wrong type replies the standard
  `WRONGTYPE` error from `get_value`, `hash_set`, `hash_del` and the hash
  getters. A failed ACL key
  check replies `NOPERM User does not have permissions on the key`, and a
  failed native user lookup replies `ERR the user does not exist or is disabled`.
  An existing account marked `off` can still pass lookup and permission checks.
- **Load failures.** `redis_module!` fails the load when a configuration
  registration fails, where 2.x loaded without that configuration. It also fails
  the load, before `init` runs, when `LoadConfigs` rejects a value given at load
  time; 2.x ran `init` with the default values, and then Redis unloaded the
  module. 2.x then ran `deinit` as Redis unloaded the module; 3.0 runs neither
  hook.
- **Numeric configuration defaults.** `register_i64_configuration` rejects a
  default outside its inclusive `min` and `max`, before registering it with
  Redis. This applies to hard-coded defaults and values supplied through
  `module_args_as_configuration`; memory values are checked after unit
  conversion. Use a valid default even if `LOADEX CONFIG` will override it. From
  the `configurations` block, the load fails and the server log has `Numeric
  configuration '<name>' default must be between <min> and <max>`, before the
  module's `ConfigurationValue::set` runs. So a module argument outside the
  registered `min` and `max`, which `set` used to reject, now gets this message
  instead of the module's own, and tests that match the module's text need
  updating.
- **Older servers.** A module built for a compatibility floor and loaded on an
  older server fails the load with the missing API's name logged, instead of
  aborting the server: during registration an API the server lacks either
  returns an error naming it or panics inside the SDK's load guard. This holds
  with the default `panic = "unwind"`; with `panic = "abort"` the panic aborts
  the server, as in 2.x (see the Panics bullet above). `init` runs only if
  registration found every API it needed, so a version check in `init` is
  reached only on servers new enough for registration.
- **Module arguments as configuration.** Configuration names are read only
  from the name positions in name/value pairs, so a string value matching
  another configuration's name no longer overrides it. Boolean values must
  be `yes` or `no`, ignoring ASCII case. Other values fail the load before
  `init`, where 2.x silently treated them as false.
- **Status values.** `Status::from(c_int)` maps every value other than
  `REDISMODULE_OK` to `Err`.
- **Keys.** `RedisKey::to_raw_parts` leaves the key open for the caller to
  close, instead of closing it, and is `#[must_use]`. Dropping a stream iterator
  stops it instead of deleting the current entry.
- **Strings.** `RedisString::create` and `Context::create_string` keep NUL
  bytes instead of panicking. Simple string and error replies replace CR, LF
  and NUL with spaces byte by byte, so non-ASCII text arrives intact.
- **Unsigned arguments.** `NextArg::next_u64`, `ArgCursor::next_u64` and
  `RedisString::parse_unsigned_integer` accept 9223372036854775808 through
  18446744073709551615 (#222), which 2.x refused with
  `Couldn't parse as integer`. Code that converts the result with `as i64`
  now gets a negative number for them; use `i64::try_from`, or `next_i64` when
  the command takes a signed value. Other input gets the same errors as before.
- **Logging.** Messages go to Redis through a fixed `%s` format, so `%` in
  logged text is printed as is; an embedded NUL is written as `\0`.
- **Cron events.** Cron event handlers receive the server's `hz`; 2.x passed 1.
- **Crash reports.** Redis retains its native crash diagnostics, and module
  INFO callbacks still receive the crash-report flag. The SDK no longer
  captures or symbolizes a Rust backtrace (#166); this does not make arbitrary
  module INFO callbacks async-signal-safe.

## Testing a module with `cargo test`

`RedisAlloc` now uses non-aborting `TryAlloc` when the running server provides
it (Redis 7.0+). Fallible operations such as `Vec::try_reserve` can report
allocation failure, including growth of an existing buffer, without aborting
Redis. Older servers retain the aborting allocation path. Rust's infallible
allocation operations can still abort on failure. This is native allocation
failure handling, not enforcement of Redis `maxmemory` or a module budget.
Growth retains the inherited allocate/copy/free behavior; no native
`TryRealloc` or `TryCalloc` dependency is introduced.

A module's unit tests that allocate abort under `RedisAlloc`, because there is
no Redis allocator outside a server. The `system-allocator-fallback` feature
lets them use the system allocator. Enable it for tests only, from the same
`git` and `tag` (or `rev`) as the dependency, so Cargo builds one copy:

```toml
[dev-dependencies]
redis-module = { git = "https://github.com/joshrotenberg/redismodule-rs", tag = "v3.0.0-alpha.2", features = ["system-allocator-fallback"] }
```

`redis_module!` fails the module load if a module built with the feature has
already allocated from the system allocator when Redis loads it.

## Runtime server-event handlers

`server_events::ServerEventHandler` adds `Cron`, `FlushInfo`, `SwapDb` and
`ConfigChanged` variants for `EventContext::subscribe_to_server_event`, and is
now `#[non_exhaustive]`: a `match` on it needs a `_` arm, so later variants do
not break it. Existing variants, including the historical
`RuleChanged` spelling, remain available. Existing attribute handlers need no
changes and stay active alongside runtime subscriptions.
