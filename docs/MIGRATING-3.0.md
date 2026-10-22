# Migrating from redis-module 2.x to 3.0

3.0 closes ways that safe code could reach undefined behavior, returns results
that 2.x dropped, and reports errors the way Redis does. This guide lists what a
module built on 2.x changes: first what to set up, then the changes the compiler
reports, then the changes that compile unchanged but behave differently. The
[changelog](../CHANGELOG.md) lists everything in the release, including what is
new.

For replacing an already running module, see the separate
[module upgrade guide](UPGRADING-MODULES.md).

Most modules meet only a few of these. The common ones are imports of raw items
from the crate root, `NextArg::next_str`, `try_as_str` lifetimes, the return
types of `delete`, `unlink`, `hash_set` and `hash_del`, and `?` on error types
without a `From` conversion.

## Before you start

- Rust 1.87 or later.
- Update `redis-module`, `redis-module-macros` and `redis-module-macros-internals`
  together to 3.0. `redis-module-common` is 0.1.2.
- The crate no longer needs libclang. The bindings are pregenerated in
  `src/redisraw/bindings.rs`, and `bindgen-runtime` is no longer a default
  feature. A manifest that enables `bindgen-runtime` or `bindgen-static` can drop
  it, except for a 32-bit target: the pregenerated bindings are for 64-bit
  targets, and the build says so.
- The crate uses edition 2024. A module keeps its own edition: modules on
  edition 2021 and on edition 2024 both build with `redis_module!` and the
  proc macros.

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
blocked client's thread-safe context. Dropping the handle unblocks the client,
but does not synthesize a reply. See [block.rs](../examples/block.rs).

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

### `auto_memory` is unsafe

`RuntimeContext::auto_memory` is now an `unsafe fn`. The SDK releases its own
handles without AutoMemory, but AutoMemory also frees the promise reply of each
blocking call made through the context. Dispose or abort every `FutureHandler`
from such a call before the callback returns or the thread-safe context is
freed, then wrap the call with a safety comment:

```rust,ignore
// SAFETY: this callback disposes every FutureHandler it creates before it returns.
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
`&RuntimeContext`, `&EventContext`, or `&RegistrationContext<'_>`.

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
| `ThreadSafeContext::lock` or `DetachedContext::lock` | Guard dereferencing to `WorkerContext` | Shared runtime operations; blocking calls only for module-owned detached workers |
| Database scan callback or helper shared across these scopes | `&RuntimeContext` | Calls, keys, database selection, logging, server info, explicit replication |
| Module INFO callback, including crash reports | `&InfoContext` | INFO output only; no Redis lock proof or runtime operations |

Change callback parameters from `&Context` to the type in this table. A helper
that calls Redis without needing a client or module ownership should accept
`&RuntimeContext`; commands, events, init hooks, and worker guards all coerce to
it. Method calls on ordinary command contexts still work through dereferencing.
Qualified references to moved methods, such as `Context::get_selected_db`, must
use `RuntimeContext::get_selected_db` or a closure such as `|ctx| ctx.get_selected_db()`.
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

This split limits capabilities. Keys, server INFO snapshots and call replies
also borrow the context they came from, as the sections above describe. Release
other native resources obtained through raw APIs before the callback returns
or the worker guard is dropped.

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

### Raw items are under `redis_module::raw`

2.x re-exported all of `raw` at the crate root. 3.0 re-exports only `Status`,
`KeyType`, `NotifyEvent`, `Version` and `ModuleOptions` there.

```rust
// 2.x
use redis_module::{RedisModuleString, REDISMODULE_OK};

// 3.0
use redis_module::raw::{RedisModuleString, REDISMODULE_OK};
```

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

The `ctx` fields of `Context` and `InfoContext` are private. `get_raw()` returns
the pointer.

`raw::export_shared_api` and `Context::export_shared_api` return the `Status`
Redis reports (`Err` when the name is taken), and the name must stay valid for
as long as the server runs.

### `RedisString` and argument parsing

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
- `RedisKeyWritable::get_value` and `as_string_dma` take `&mut self`, so the
  reference they return cannot outlive the key or alias another one. Bind the
  key with `let mut`.

```rust
// 3.0
let mut key = ctx.open_key_writable(&name);
if let Some(value) = key.get_value::<MyType>(&MY_TYPE)? {
    value.count += 1;
}
```

### Replies

`RedisValue` has two new variants: `InsertionOrderedMap`, which replies with
the entries in the order the module added them, and `Error(String)`, an error
reply with an owned message, which can be an array element or a map value. An
exhaustive `match` on `RedisValue` needs two more arms.

`RedisValueKey` has a new variant, `SimpleString`, for a map key or set element
replied as a simple string. A map or set converted from a call reply holds it
for a simple-string key or element, where 2.x gave `String`; an exhaustive
`match` on `RedisValueKey` needs another arm.

An error in a call reply converts to `RedisValue::Error`, also inside an array
or map, where 2.x gave `StringBuffer` with the error's bytes. A module that
forwards the reply now replies with the error, as Redis did; one that read the
bytes from `StringBuffer` reads the message from `Error` instead.

### Errors

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
  needs a wildcard arm. `KeyType` gains `Gcra` and `Unknown` (for values a newer
  Redis may return), and `ReplyType` gains `Attribute` and `Promise`. Both
  convert from an `i32` with `From` instead of panicking.
- The raw enums are `#[repr(i32)]` and no longer have the `FromPrimitive`
  methods; the `enum-primitive-derive` and `num-traits` dependencies are gone.
- The opaque handle types in `raw` (`RedisModuleCtx`, `RedisModuleKey`,
  `RedisModuleString` and the others with no fields) no longer derive `Copy`
  and `Clone`. Modules hold pointers to them, never values.

## Changes that compile unchanged

These compile without a change, so the compiler will not point at them. Check
the code that depends on them, and the tests and clients that match on reply
text.

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
  fails the load. This needs the default `panic = "unwind"`; callbacks a module
  registers itself, such as native type methods, can use `panic::catch_panic`.
- **`RedisGILGuard`.** Locking a guard that is already locked panics instead of
  returning a second mutable reference.
- **Missing module APIs.** Calling an API the running Redis lacks returns or
  panics with `RedisModule_X is not available in this Redis server` instead of
  `called Option::unwrap() on a None value`.
- **Errors on the wire.** A key of the wrong type replies the standard
  `WRONGTYPE` error from `get_value`, `hash_set`, `hash_del` and the hash
  getters. A failed ACL key
  check replies `NOPERM User does not have permissions on the key`, and an
  unknown or disabled user `ERR the user does not exist or is disabled`.
- **Load failures.** `redis_module!` fails the load when a configuration
  registration fails, where 2.x loaded without that configuration. It also
  fails the load, before `init` runs, when `LoadConfigs` rejects a value given
  at load time; 2.x ran `init` with the default values, and then Redis
  unloaded the module.
- **Numeric configuration defaults.** `register_i64_configuration` rejects a
  default outside its inclusive `min` and `max`, before registering it with
  Redis. This applies to hard-coded defaults and values supplied through
  `module_args_as_configuration`; memory values are checked after unit
  conversion. Use a valid default even if `LOADEX CONFIG` will override it.
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
- **Logging.** Messages go to Redis through a fixed `%s` format, so `%` in
  logged text is printed as is; an embedded NUL is written as `\0`.
- **Cron events.** Cron event handlers receive the server's `hz`; 2.x passed 1.
- **Crash reports.** The `backtrace` field of the crash report's `trace`
  section is `std::backtrace::Backtrace` output (one line, no column numbers)
  instead of the `backtrace` crate's numbered frames.

## Testing a module with `cargo test`

A module's unit tests that allocate abort under `RedisAlloc`, because there is
no Redis allocator outside a server. The `system-allocator-fallback` feature
lets them use the system allocator. Enable it for tests only, from the same
`git` and `rev` as the dependency, so Cargo builds one copy:

```toml
[dev-dependencies]
redis-module = { git = "https://github.com/joshrotenberg/redismodule-rs", rev = "<commit>", features = ["system-allocator-fallback"] }
```

`redis_module!` fails the module load if a module built with the feature has
already allocated from the system allocator when Redis loads it.
