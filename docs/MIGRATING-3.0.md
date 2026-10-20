# Migrating from redis-module 2.x to 3.0

3.0 closes ways that safe code could reach undefined behavior, returns results
that 2.x dropped, and reports errors the way Redis does. This guide lists what a
module built on 2.x changes: first what to set up, then the changes the compiler
reports, then the changes that compile unchanged but behave differently. The
[changelog](../CHANGELOG.md) lists everything in the release, including what is
new.

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

## Changes the compiler reports

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

- `Context::new` and `Context::dummy`
- `RedisString::new`, `RedisString::from_redis_module_string`,
  `RedisString::from_ptr`, `RedisString::string_as_slice` and `decode_args`
- `RedisKey::open` and `open_with_flags`, and `RedisKeyWritable::open`,
  `open_with_flags` and `open_with_redis_string`
- `InfoContext::new`, `RedisBuffer::new`, `RedisModuleIO::new`
- `logging::setup_for_context`, `logging::log_io_error`,
  `DefragContext::defrag_dealloc`
- in `raw`: the RDB `save_*` and `load_*` helpers, `is_io_error`, `redis_log`,
  `call_reply_verbatim_string`, and the doc-hidden `register_info_function`

Inside a command or callback there is usually a `&Context` already; use its safe
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
- `RedisGILGuard<T>` is `Send` only when `T` is.

### Data types

- `RedisType::raw_type` is a method returning `*mut RedisModuleType` instead of
  a public `RefCell` field.
- `RedisType::create_data_type` takes `&Context`.
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

- **Keyspace subscription ownership.** The three-argument
  `redis_event_handler!(ctx, mask, handler)` now rejects contexts without proven
  module ownership, logging a warning and returning a native error status from
  its enclosing function. SDK module-load, command, and keyspace callbacks
  provide the proof, so normal `redis_module!` event-handler lists are unchanged.
  `Context::new` does not establish ownership, even if its raw pointer happens
  to belong to a module; custom native callbacks must use the unsafe
  `Context::from_module_context` constructor only after verifying its stronger
  contract. Other SDK callback contexts that do not carry this proof are also
  rejected; move their subscriptions into initialization or a command.
  A context from `RedisModule_GetThreadSafeContext(NULL)` has no native module
  identity and must never be promoted this way. Register from a module-owned
  context instead, so Redis can remove the callback when the module unloads.
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
