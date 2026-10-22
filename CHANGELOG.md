# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [3.0.0] - unreleased

The release date and comparison link are added when 3.0.0 ships. Entries marked **Breaking** need a change in modules; [docs/MIGRATING-3.0.md](docs/MIGRATING-3.0.md) describes each one.

### Added

- Binary string configuration adapters for `RedisGILGuard<Vec<u8>>` and
  `Mutex<Vec<u8>>`, preserving empty values, NUL and invalid UTF-8. Native
  strings are created only during getters and are not retained as storage.

- [Configuration notes](docs/CONFIGURATION.md) explain registration and load
  precedence, argument syntax, UTF-8 versus binary storage, validation,
  observers and rollback. Regression tests cover explicit Redis settings over
  SDK argument defaults and callback observations during rejected updates.

- A [module upgrade guide](docs/UPGRADING-MODULES.md) covering unload/load
  constraints, configuration replay, transient state, cleanup and failure
  recovery, with a tested `lifecycle` example. API documentation links are
  corrected and CI documentation builds now deny warnings.

- `module_version!()` derives the integer module version from the calling
  crate's Cargo package version at compile time. `encode_module_version` is
  the underlying const conversion using `major * 10000 + minor * 100 + patch`.
  Malformed versions, prereleases, minor/patch values above 99, and integer
  overflow are rejected; valid build metadata is ignored. Explicit module
  version values remain supported.

- `configuration::ParsedStringConfig<T>` validates string configurations before
  replacing their raw text and parsed value together. Read matching values
  under a Redis lock proof without requiring `T: Clone`. Existing string
  registration, defaults, module arguments and change callbacks are supported.

- `redis_module::milliseconds()` exposes the server module API's fresh Unix
  wall-clock time without a context, including from module workers after API
  initialization. It is available at every compatibility floor.

- `cluster::key_for_slot` generates an ASCII key for any valid cluster slot
  without a server API. Exact key names are stable across SDK and server
  versions, with all 16,384 names pinned by a compatibility fixture. A prefix
  without `{` and any suffix preserve its slot.

- CI covers pinned Redis 7.2, 7.4 and 8.0 through 8.10 releases, plus Valkey
  8.1 and 9.1 at the default API floor. Checksum-verified server builds are
  cached, selected explicitly by path, and validated on cache hits. Failed
  matrix jobs retain server build and integration logs for seven days.
  Version tests distinguish Valkey product versions from Redis compatibility
  versions without weakening Redis API-availability checks.

- A [notification-write guide](docs/KEYSPACE-NOTIFICATIONS.md) and
  `keyspace_to_stream` example covering owned callback data, deferred writes,
  feedback prevention, database selection, replication, loading and unload.

- `flush_info_event_handler` receives `FlushSubevent` and copied `FlushInfo`
  with the affected database (or all databases).
  `swapdb_event_handler` receives both database IDs in `SwapDbInfo` after a
  successful swap. Both use `EventContext` and isolate panics per callback.
  The phase-only `flush_event_handler` remains available alongside them.

- `with_call_user` runs a closure with a user for ACL-checked calls while
  preserving runtime, client, event, worker, or registration capabilities.

- `RuntimeContext` for shared locked operations and scoped ACL-call users,
  `EventContext` for module-owned
  callbacks, and `WorkerContext` for locked workers. Client command handlers keep
  `Context`; event and worker views cannot reply to or block clients.
- Borrowed `RegistrationContext` for module initialization, with access to
  runtime helpers and the Redis lock proof used by configuration guards.
- `Context::subscribe_to_keyspace_events` for runtime-selected notification
  masks, with `redis_event_handler!(handler)` providing the shared panic-safe
  callback adapter. Existing `redis_module!` event handlers remain supported.
  Module ownership is carried by the event view, also exposed by load and
  command contexts. Worker guards cannot subscribe; custom native callbacks use
  `EventContext::new` with a proven module association.
- Optional command builders in `redis_module!`: `redis_command!(name, handler)`
  returns a `CommandBuilder` with typed `CommandFlags`, `KeyRange`, and ACL
  settings. Builders can be mixed with existing command tuples.
- `ErrorReply::code` and `ErrorCallReply::code`, which borrow the error prefix
  without allocating or decoding UTF-8, and `Context::notify_keyspace_event_bytes`,
  which sends event names containing arbitrary bytes except NUL.
- `Context::get_selected_db`, `select_db`, and `with_db`, which restores the
  original database after a closure returns or unwinds.
- `ArgCursor`, which parses command arguments by borrowing them.
- `Context::get_command_keys`, the positions of a command's keys (`RedisModule_GetCommandKeysWithFlags`), and `cluster::key_slot`, a key's hash slot computed without the server. `ContextFlags` derives `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq` and `Hash`.
- `Context::yield_now` and `YieldMode`, for long-running commands (`RedisModule_Yield`).
- `Context::acl_check_command_permission`, `Context::acl_check_channel_permission` with `AclChannelPermissions`, and `Context::acl_add_log_entry` with `AclLogReason`.
- Memory accounting: `Context::used_memory_ratio`, `RedisString::malloc_size`, and `alloc::malloc_size`, `malloc_usable_size` and `redis_alloc_size`.
- `configuration::get_memory_default_config_value`, which reads a memory configuration from the module arguments with Redis's units.
- `Context::block_client_on_reply` and `ReplyingBlockedClient`, which block a client with reply and timeout callbacks on the main thread and private data.
- The `event_loop` module: `add` and `remove` (both `unsafe`), `add_one_shot`, and `EventLoopMask` (`RedisModule_EventLoopAdd`, `EventLoopDel` and `EventLoopAddOneShot`).
- `Context::reply_with_call_reply`, `StringCallReply::is_simple_string`, and the call options `dry_run`, `replicate_to_replicas_only` and `replicate_to_aof_only`.
- `RedisValue::InsertionOrderedMap`, which replies with entries in insertion order.
- `RedisValueKey::SimpleString`, a map key or set element replied as a simple string; a call reply's simple-string key converts to it.
- `RedisValue::Error(String)`, an error reply with an owned message, which can be an array element or a map value.
- `RedisString::from_bytes`, `from_owned_ptr`, `retain_from_ptr` and `as_ptr`.
- `RedisKeyWritable::hash_set_by_string` and `hash_del_by_string`, for fields that hold any bytes.
- `RedisKey::hash_get_by_string` and `RedisKeyWritable::hash_get_by_string`: `HGET` with the field passed as an existing `RedisString`, avoiding the per-call `CString` and the transient field object Redis creates for C-string fields (upstream PR 485).
- `Context::dbsize` (upstream PR 397), the shutdown server event (upstream PR 477), and `PartialEq`, `Eq`, `PartialOrd`, `Ord` and `Hash` on `RedisAlloc` (upstream PR 386).
- `configuration::load_configurations`, for modules that register configurations outside `redis_module!`.
- The `system-allocator-fallback` feature, which lets a module's `cargo test` allocate without a Redis server.
- `panic::catch_panic`, which the crate uses at every callback it hands to Redis.
- `RedisError::from_error` and `InfoContext::get_raw`.
- New variants `KeyType::Gcra`, `ReplyType::Attribute` and `ReplyType::Promise`.
- `KeyFlags::ACCESS_TRIMMED` (upstream PR 468), and the swap-prefetch and `RedisModule_SetOpenFilesReservation` declarations in the vendored header (upstream PRs 478 and 486).
- docs/MIGRATING-3.0.md.
- The fork's integration harness supports module arguments, fallible startup
  with server logs, bounded polling, replica startup, and graceful restarts on
  saved data. Test servers use OS-selected ports and isolated directories,
  with bounded retries for confirmed port collisions on fresh launches;
  TEST_REDIS_SERVER_BIN selects another Redis binary.

### Changed

- Internal API wrappers and mixed command-list registration macros now parse
  Rust syntax with Syn 2. Command syntax and API-floor-dependent signatures are
  unchanged. The public macro crate retains its existing metadata parser.

- **Breaking:** timers return `Result<TimerHandle<T>, RedisError>` and require
  `Send + 'static` callbacks and payloads. `cancel_timer(&handle)` replaces
  untyped `stop_timer<T>` and recovers the payload at most once;
  `timer_remaining(&handle)` replaces payload-borrowing `get_timer_info<T>`.
  Dropping a handle leaves its timer running. Creation requires an active module
  bound through `redis_module!`; initialization and unload reject new timers.
  SDK lifecycle hooks protect pending and firing timers during unload. See the
  [timer migration notes](docs/MIGRATING-3.0.md#timers-use-typed-handles).

- **Breaking:** `Context::block_client` returns `Result<BlockedClient,
  RedisError>`. It rejects denied invocations before native blocking, including
  scripts, transactions, module calls without blocking permission, and
  reply/timeout callback reentry. It shares validation with
  `block_client_on_reply`, handles Redis 6.0's older flags, and stores only
  non-null native handles. Examples check the result before starting workers.

- **Breaking:** shared `RedisGILGuard<T>` storage now requires `T: Send`.
  Serialization cannot protect non-`Send` aliases outside the guard.
  `T: Sync` is not required. Migrate shared native string configurations to
  `String` or `Vec<u8>` storage; `RedisString` remains non-`Send` and non-`Sync`.
  Scoped call-user bookkeeping is now thread-local, and the native ownership
  fixture keeps retained handles on their originating thread with explicit
  cleanup during Redis callbacks.

- **Breaking:** configuration `on_changed` closures must be `Send`, including
  those in `redis_module!`. A locked worker's `CONFIG SET` can invoke them on
  that worker; serialization does not make non-`Send` captures transferable.
  Function callbacks are unchanged, and `Sync` is not required. The
  `configuration_observers` example covers client and worker delivery.

- **Breaking:** `redis_module!` requires a constant `&str` module name without
  NUL bytes. Its terminated buffer is built at compile time, allowing long and
  UTF-8 names without allocation before native initialization. Runtime name
  expressions must become constants or compile-time inputs.

- **Breaking:** `authenticate_user` is now `RuntimeContext::scoped_call_user`,
  returning the root-exported `CallUserScope` (previously `ContextUserScope`).
  Nested guards now restore the latest still-active scope on drop, including
  during unwind or out-of-order drops. Forgotten guards are cleaned up when
  their runtime view ends, before worker unlock. Redis 8.8+ also preserves a
  preexisting native override. ACL checks remain opt-in through
  `CallOptionsBuilder::verify_acl`.

- **Breaking:** non-client callbacks use `EventContext`; database scan callbacks
  and shared helpers use `RuntimeContext`. Both worker guard types dereference
  to `WorkerContext` and borrow their owner. Init no longer exposes client APIs.
  `Context::new` now requires a module association and client target;
  `Context::dummy` moved to `RuntimeContext`. See the migration guide.
- Worker guard cleanup frees its temporary context while still locked and
  unlocks through the borrowed owner's live native context. Blocked-client
  replies use a private reply adapter without fabricating a Redis lock proof.
- Blocking calls from module-less workers return an error before entering Redis.
  Module-owned detached workers retain blocking calls and receive event views
  in their unblock handlers.
- **Breaking:** command registration, data-type registration, configuration
  registration/loading, and module options require `RegistrationContext`.
  `redis_module!` supplies it during load. Init functions that register items
  must update their context parameter; command handlers and both command
  declaration syntaxes are unchanged. Runtime contexts cannot safely acquire
  the load-time capability.
- **Breaking:** `redis_event_handler!(ctx, mask, handler)` requires an
  `EventContext` or a load/command context that exposes it. Worker guards cannot
  register subscriptions. Custom native event contexts require proven module
  ownership and the Redis lock through `EventContext::new`.
- `Context::notify_keyspace_event` returns `Status::Err` for NUL-containing
  event names instead of panicking.
- **Breaking:** functions that take raw pointers they dereference are `unsafe`: `Context::new`, `RuntimeContext::new`, `RuntimeContext::dummy`, `RedisString::new`, `RedisString::from_redis_module_string`, `RedisString::from_ptr`, `RedisString::string_as_slice`, `decode_args`, `RedisKey::open` and `open_with_flags`, `RedisKeyWritable::open`, `open_with_flags` and `open_with_redis_string`, `InfoContext::new`, `RedisBuffer::new`, `RedisModuleIO::new`, `logging::setup_for_context`, `logging::log_io_error`, `DefragContext::defrag_dealloc`, and in `raw` the RDB `save_*` and `load_*` helpers, `is_io_error`, `redis_log`, `call_reply_verbatim_string` and the doc-hidden `register_info_function`.
- **Breaking:** the `ctx` fields of `Context` and `InfoContext` and `RedisString`'s `inner` field are private; use `get_raw()` and `as_ptr()`.
- **Breaking:** `RedisString::try_as_str` borrows from the string, and `RedisString` no longer stores a context. `Context::get_current_user` returns `Option<RedisString>`. `ScanKeyCursor::scan` and `for_each` callbacks receive the value as `Option<&RedisString>`, and `KeysCursor::scan` takes `&mut F`.
- **Breaking:** keyspace event handlers registered with `redis_event_handler!` get the event name and key borrowed for the callback, so a handler that asks for `&'static str` or `&'static [u8]` no longer compiles.
- **Breaking:** `RedisKeyWritable::delete` and `unlink` return `Result<bool, RedisError>`, `true` when the key held a value.
- **Breaking:** `RedisKeyWritable::hash_set` and `hash_del` return `Result<usize, RedisError>`, the number of existing fields replaced or deleted, instead of a `Status`.
- **Breaking:** `RedisKeyWritable::get_value` and `as_string_dma` take `&mut self`.
- **Breaking:** `Context::call` returns `BulkString` for a bulk string reply and `SimpleString` only for a simple string reply, and a call that fails without the `E` flag returns an error naming the reason instead of `CallReply::Unknown` (adapted from upstream PR 93).
- **Breaking:** `RedisValue` has two new variants, `InsertionOrderedMap` and `Error`.
- **Breaking:** `RedisValueKey` has a new variant, `SimpleString`, and a call reply's simple-string map key or set element converts to it instead of `String`.
- **Breaking:** an error in a call reply, also inside an array or map, converts to `RedisValue::Error` instead of `StringBuffer`, so a forwarded reply keeps it as an error.
- **Breaking:** `RedisError` implements `std::error::Error`, which replaces the blanket `From<T: Error>` conversion with `From` for a list of standard library errors; other errors convert with `RedisError::from_error`.
- **Breaking:** `register_i64_configuration`, `register_string_configuration`, `register_bool_configuration` and `register_enum_configuration` return `Result<(), RedisError>`.
- **Breaking:** `register_i64_configuration` rejects defaults outside the declared inclusive bounds, including defaults supplied as module arguments. Boolean module arguments accept only `yes` or `no`, ignoring ASCII case; other values fail the load instead of becoming false.
- **Breaking:** locking `RedisGILGuard` twice panics. `ConfigurationContext` is neither `Send` nor `Sync`. `ConfigurationValue` no longer requires `Send`.
- **Breaking:** `RedisType::raw_type` is a method instead of a public `RefCell` field, `create_data_type` takes `&Context`, and an invalid type name or encoding version fails to compile.
- **Breaking:** command flags in `redis_module!` must be a constant expression and are checked at compile time. `redis_command!` and `redis_event_handler!` take `&Context`.
- **Breaking:** `KeyType` and `ReplyType` are `#[non_exhaustive]`, and `KeyType` has an `Unknown` variant. The raw enums are `#[repr(i32)]` and convert from `c_int` without panicking; `Status::from(c_int)` maps every value but `REDISMODULE_OK` to `Err`.
- **Breaking:** `raw::export_shared_api` and `Context::export_shared_api` return `Status`.
- **Breaking:** the opaque handle types in `raw` derive only `Debug`, not `Copy` and `Clone` (bindgen 0.73).
- **Breaking:** `bindgen-runtime` is no longer a default feature. The bindings are pregenerated in `src/redisraw/bindings.rs` for 64-bit targets, so a default build needs no libclang; `bindgen-runtime` and `bindgen-static` regenerate them with bindgen 0.73, and CI checks that the committed file is current.
- Error texts on the wire: wrong-type keys reply `WRONGTYPE`, a denied ACL key check replies `NOPERM`, and an unknown or disabled user replies `ERR the user does not exist or is disabled`.
- A panic in a command replies `ERR command panicked: <message>`, and a panic in other callbacks is logged, instead of aborting the server.
- A missing module API is reported by name (`RedisModule_X is not available in this Redis server`) instead of as an unwrap of `None`.
- `redis_module!` fails the module load when a configuration registration fails, when `LoadConfigs` rejects a value, or when a module built with `system-allocator-fallback` already allocated from the system allocator.
- Log messages are passed to Redis as the argument of a fixed `%s` format (adapted from upstream PR 476).
- The `backtrace` field of the crash report's `trace` section is `std::backtrace::Backtrace` output.
- `RedisKey::hash_get` takes `impl AsRef<[u8]>` (upstream PR 470).
- The crates use edition 2024 and declare `rust-version = "1.87"`, which CI tests. The pinned toolchain is 1.94.0 (upstream PR 479).

### Removed

- **Breaking:** `NextArg::next_str`; use `ArgCursor`, or `next_arg` and `try_as_str`.
- **Breaking:** the re-export of `raw` at the crate root, except `Status`, `KeyType`, `NotifyEvent`, `Version` and `ModuleOptions`.
- **Breaking:** the public pointer-taking `raw` helpers (replies, call replies, keys, hashes, lists, strings, info fields, replication) and `key::verify_type`; the safe API covers them.
- **Breaking:** the `FromPrimitive` methods on the raw enums.
- The dependencies `nix`, `strum_macros`, `regex`, `backtrace`, `enum-primitive-derive` and `num-traits`.

### Fixed

- `redis_module!` now owns accepted configuration private data per load and
  releases captured observers and final string getter replies after caught
  load failures or successful deinit. Refused or panicking deinit retains
  registrations. Unload is blocked while native configuration frames may
  retain pointers; worker invocations without a usable deferred completion
  mechanism conservatively block unload for that load generation. Caller
  statics remain caller-owned, and custom loaders retain the previous accepted
  allocation behavior. See the
  [configuration lifecycle notes](docs/CONFIGURATION.md#registration-ownership-and-unload).

- Timer cancellation now releases the complete callback allocation, including
  captured closure state. Typed handles prevent caller-selected payload casts,
  and timer queries no longer expose references that cancellation can invalidate.
  Creation validates API availability and delay before retaining timer state.

- Repeated `CallOptionsBuilder::resp` selections now replace the previous
  protocol, including switching back to RESP2 or Auto after RESP3. Other call
  options remain intact, and blocking call options use the same selection.

- Module initialization no longer copies an unchecked name into a 64-byte
  buffer. Names of 64 bytes lacked a terminator and longer names could overrun
  that buffer. Embedded NUL names now fail compilation instead of truncating.

- The integration harness retries confirmed TCP address-in-use failures at
  `listen()` as well as `bind()`, while preserving other startup failures.

- Rejected `add_post_notification_job` submissions now release the closure and
  captured data immediately. Redis does not take ownership when it rejects a
  job during loading or on a readonly replica.
- `RedisString` views could outlive their owner, and strings freed themselves through the context they were created with; several producers leaked strings or freed them twice (stream iterators, `ListPop`, server info, the keys cursor, the scan cursor).
- Reading through a `RedisKeyWritable` value or string buffer could alias a second one.
- `RedisGILGuard` could hand out two mutable references, and safe code could build a `Context` from any pointer.
- A forwarded call string reply holding NUL panicked, and one holding CR or LF corrupted the reply.
- `SimpleString` and `SimpleStringStatic` replies panicked on NUL, and CR or LF ended them early; they now become spaces.
- `RedisString::create` and `Context::create_string` panicked on a NUL byte, and error replies turned non-ASCII text into invalid UTF-8.
- `From<c_int>` for `KeyType`, `ReplyType` and `Status` panicked on values outside their lists, and version parsing panicked on a number that overflows.
- `RedisAlloc` ignored alignments above 16 bytes.
- String configuration getters leaked their replies.
- Integration tests no longer retry forever after Redis fails to start. Startup
  probes configure socket timeouts before protocol traffic and verify the child
  process identity.
- The `configurations` block of `redis_module!` compiled only with all four groups and `module_args_as_configuration` listed, and a memory configuration given as a module argument with a unit, such as `8mb`, failed the module load.
- Module arguments used as configuration are read in name/value pairs, so a value matching another configuration's name does not override it. Numeric bounds are checked after memory units are converted, and mixed-case boolean values are parsed correctly.
- The stream iterator deleted an entry when dropped instead of stopping.
- `RedisKey::to_raw_parts` closed the key it handed over.
- Cron event handlers received 1 instead of the server's `hz`.
- `ServerInfo::field` and `Context::server_info` panicked on a name holding a NUL byte; such a name now names nothing (for `field`, as upstream PR 488 does).
- Verbatim call replies read their format through a null pointer.
- `hash_set` and `hash_del` reported replacing or deleting a field as an error, and `delete` and `unlink` did not say whether a value was removed.
- `DefragContext::defrag_alloc` passed a zero-size layout to the allocator.
- Keyspace event names that are not UTF-8 no longer panic in the event macro (upstream PR 475).
- `call_blocking` is available under the 8.0 floor.
- Commands registered with `#[command]` load on Redis 7.0 when they declare no ACL categories; registration required Redis 7.2's `RedisModule_SetCommandACLCategories` for every command. Under the 7.0 floor, an error registering one of those commands was also ignored and the module loaded anyway; it now fails the load, as under the other floors.
- `redis_module!` reuses an existing custom ACL category when Redis reports `EBUSY`, so modules declaring `acl_categories` can unload and reload in the same server. Invalid names and other category registration failures still fail the load.

## [2.0.8](https://github.com/RedisLabsModules/redismodule-rs/compare/redis-module-v2.0.7...redis-module-v2.0.8) - 2026-05-04

### Fixed

- CI: Revamp the release pipeline, moving over to Trusted Publishing for all crates in the repository.
