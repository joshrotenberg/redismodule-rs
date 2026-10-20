# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [3.0.0] - unreleased

The release date and comparison link are added when 3.0.0 ships. Entries marked **Breaking** need a change in modules; [docs/MIGRATING-3.0.md](docs/MIGRATING-3.0.md) describes each one.

### Added

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

### Changed

- **Breaking:** functions that take raw pointers they dereference are `unsafe`: `Context::new`, `Context::dummy`, `RedisString::new`, `RedisString::from_redis_module_string`, `RedisString::from_ptr`, `RedisString::string_as_slice`, `decode_args`, `RedisKey::open` and `open_with_flags`, `RedisKeyWritable::open`, `open_with_flags` and `open_with_redis_string`, `InfoContext::new`, `RedisBuffer::new`, `RedisModuleIO::new`, `logging::setup_for_context`, `logging::log_io_error`, `DefragContext::defrag_dealloc`, and in `raw` the RDB `save_*` and `load_*` helpers, `is_io_error`, `redis_log`, `call_reply_verbatim_string` and the doc-hidden `register_info_function`.
- **Breaking:** the `ctx` fields of `Context` and `InfoContext` and `RedisString`'s `inner` field are private; use `get_raw()` and `as_ptr()`.
- **Breaking:** `RedisString::try_as_str` borrows from the string, and `RedisString` no longer stores a context. `Context::get_current_user` returns `Option<RedisString>`. `ScanKeyCursor::scan` and `for_each` callbacks receive the value as `Option<&RedisString>`, and `KeysCursor::scan` takes `&mut F`.
- **Breaking:** keyspace event handlers registered with `redis_event_handler!` get the event name and key borrowed for the callback, so a handler that asks for `&'static str` or `&'static [u8]` no longer compiles.
- **Breaking:** `RedisKeyWritable::delete` and `unlink` return `Result<bool, RedisError>`, `true` when the key held a value.
- **Breaking:** `RedisKeyWritable::hash_set` and `hash_del` return `Result<usize, RedisError>`, the number of existing fields replaced or deleted, instead of a `Status`.
- **Breaking:** `RedisKeyWritable::get_value` and `as_string_dma` take `&mut self`.
- **Breaking:** `Context::call` returns `BulkString` for a bulk string reply and `SimpleString` only for a simple string reply, and a call that fails without the `E` flag returns an error naming the reason instead of `CallReply::Unknown` (adapted from upstream PR 93).
- **Breaking:** `RedisValue` has two new variants, `InsertionOrderedMap` and `Error`.
- **Breaking:** `RedisValueKey` has a new variant, `SimpleString`, and a call reply's simple-string map key or set element converts to it instead of `String`.
- **Breaking:** `RedisError` implements `std::error::Error`, which replaces the blanket `From<T: Error>` conversion with `From` for a list of standard library errors; other errors convert with `RedisError::from_error`.
- **Breaking:** `register_i64_configuration`, `register_string_configuration`, `register_bool_configuration` and `register_enum_configuration` return `Result<(), RedisError>`.
- **Breaking:** `RedisGILGuard<T>` is `Send` only when `T` is, and locking it twice panics. `ConfigurationContext` is neither `Send` nor `Sync`. `ConfigurationValue` no longer requires `Send`.
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

- `RedisString` views could outlive their owner, and strings freed themselves through the context they were created with; several producers leaked strings or freed them twice (stream iterators, `ListPop`, server info, the keys cursor, the scan cursor).
- Reading through a `RedisKeyWritable` value or string buffer could alias a second one.
- `RedisGILGuard` could hand out two mutable references, and safe code could build a `Context` from any pointer.
- A forwarded call string reply holding NUL panicked, and one holding CR or LF corrupted the reply.
- `SimpleString` and `SimpleStringStatic` replies panicked on NUL, and CR or LF ended them early; they now become spaces.
- `RedisString::create` and `Context::create_string` panicked on a NUL byte, and error replies turned non-ASCII text into invalid UTF-8.
- `From<c_int>` for `KeyType`, `ReplyType` and `Status` panicked on values outside their lists, and version parsing panicked on a number that overflows.
- `RedisAlloc` ignored alignments above 16 bytes.
- String configuration getters leaked their replies.
- The `configurations` block of `redis_module!` compiled only with all four groups and `module_args_as_configuration` listed, and a memory configuration given as a module argument with a unit, such as `8mb`, failed the module load.
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

## [2.0.8](https://github.com/RedisLabsModules/redismodule-rs/compare/redis-module-v2.0.7...redis-module-v2.0.8) - 2026-05-04

### Fixed

- CI: Revamp the release pipeline, moving over to Trusted Publishing for all crates in the repository.
