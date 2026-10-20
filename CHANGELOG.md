# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [3.0.0] - unreleased

The release date and comparison link are added when 3.0.0 ships. [docs/MIGRATING-3.0.md](docs/MIGRATING-3.0.md) describes how to update a module from 2.x.

### Breaking changes

- Safe functions no longer accept raw pointers they dereference. `Context::new`, `Context::dummy`, `RedisString::new`, `RedisString::from_redis_module_string`, `RedisString::from_ptr`, `RedisString::string_as_slice`, `decode_args`, `RedisKey::open` and `open_with_flags`, `RedisKeyWritable::open`, `open_with_flags` and `open_with_redis_string`, `InfoContext::new`, `RedisBuffer::new`, `RedisModuleIO::new`, `logging::setup_for_context`, `logging::log_io_error`, `DefragContext::defrag_dealloc`, and the `raw` RDB helpers, `is_io_error`, `redis_log`, `call_reply_verbatim_string`, `notify_keyspace_event` and `export_shared_api` are `unsafe`. The other pointer-taking `raw` helpers and `key::verify_type` are no longer public.
- `raw` is no longer re-exported at the crate root, except `Status`, `KeyType`, `NotifyEvent`, `Version` and `ModuleOptions`.
- The `ctx` fields of `Context` and `InfoContext` are private; use `get_raw()`. `RedisString`'s `inner` field is private; use `as_ptr()`.
- `RedisString::try_as_str` borrows from the string, and `NextArg::next_str` is removed (use `ArgCursor`, or `next_arg` and `try_as_str`).
- `RedisString` no longer stores a context, and `Context::get_current_user` returns `Option<RedisString>`. `ScanKeyCursor::scan` callbacks receive the value as `Option<&RedisString>`; `KeysCursor::scan` takes `&mut F`.
- `RedisKeyWritable::delete` and `unlink` return `Result<bool, RedisError>`, `true` when the key held a value.
- `RedisKeyWritable::hash_set` and `hash_del` return `Result<usize, RedisError>`, the number of existing fields replaced or deleted, instead of a `Status`.
- `RedisKeyWritable::get_value` and `as_string_dma` take `&mut self`.
- `Context::call` returns `BulkString` for a bulk string reply and `SimpleString` only for a simple string reply. A call that fails without the `E` flag returns an error naming the reason instead of `CallReply::Unknown`.
- `RedisValue` has a new variant, `InsertionOrderedMap`.
- `RedisError` implements `std::error::Error`, which replaces the blanket `From<T: Error>` conversion with `From` for a list of standard library errors; other errors convert with `RedisError::from_error`.
- `register_i64_configuration`, `register_string_configuration`, `register_bool_configuration` and `register_enum_configuration` return `Result<(), RedisError>`.
- `RedisGILGuard<T>` is `Send` only when `T` is, and locking it twice panics. `ConfigurationContext` is neither `Send` nor `Sync`. `ConfigurationValue` no longer requires `Send`.
- `RedisType::raw_type` is a method instead of a public `RefCell` field, `create_data_type` takes `&Context`, and an invalid type name or encoding version fails to compile.
- Command flags in `redis_module!` must be a constant expression and are checked at compile time. `redis_command!` and `redis_event_handler!` take `&Context`.
- `KeyType` and `ReplyType` are `#[non_exhaustive]` with an `Unknown` variant; the raw enums are `#[repr(i32)]` without `FromPrimitive`, and `Status::from(c_int)` maps every value but `REDISMODULE_OK` to `Err`.
- `bindgen-runtime` is no longer a default feature: the bindings are pregenerated for 64-bit targets.
- The opaque handle types in `raw` derive only `Debug`, not `Copy` and `Clone` (bindgen 0.73).
- `raw::export_shared_api` and `Context::export_shared_api` return `Status`.
- Error texts on the wire: wrong-type keys reply `WRONGTYPE`, a denied ACL key check replies `NOPERM`, and an unknown or disabled user replies `ERR the user does not exist or is disabled`.

### Added

- `ArgCursor`, which parses command arguments by borrowing them.
- `Context::yield_now` and `YieldMode`, for long-running commands (`RedisModule_Yield`).
- `Context::acl_check_command_permission`, `Context::acl_check_channel_permission` with `AclChannelPermissions`, and `Context::acl_add_log_entry` with `AclLogReason`.
- Memory accounting: `Context::used_memory_ratio`, `RedisString::malloc_size`, and `alloc::malloc_size`, `malloc_usable_size` and `redis_alloc_size`.
- `Context::block_client_on_reply`, which blocks a client with reply and timeout callbacks on the main thread and private data.
- `event_loop::add` and `event_loop::remove` with `EventLoopMask` (`RedisModule_EventLoopAdd` and `EventLoopDel`).
- `Context::reply_with_call_reply`, `StringCallReply::is_simple_string`, and the call options `dry_run`, `replicate_to_replicas_only` and `replicate_to_aof_only`.
- `RedisValue::InsertionOrderedMap`, which replies with entries in insertion order.
- `RedisKeyWritable::hash_set_by_string` and `hash_del_by_string`, for fields that hold any bytes.
- `RedisKey::hash_get_by_string` and `RedisKeyWritable::hash_get_by_string`: `HGET` with the field passed as an existing `RedisString`, avoiding the per-call `CString` and the transient field object Redis creates for C-string fields (upstream PR 485).
- `Context::dbsize` (upstream PR 397) and the shutdown server event (upstream PR 477).
- `configuration::load_configurations`, for modules that register configurations outside `redis_module!`.
- The `system-allocator-fallback` feature, which lets a module's `cargo test` allocate without a Redis server.
- `panic::catch_panic`, which the crate uses at every callback it hands to Redis.
- `RedisError::from_error`, `InfoContext::get_raw`, and `PartialEq`, `Eq`, `PartialOrd`, `Ord` and `Hash` on `RedisAlloc`.
- `KeyFlags::ACCESS_TRIMMED` (upstream PR 468), and the swap-prefetch and `RedisModule_SetOpenFilesReservation` declarations in the vendored header (upstream PRs 478 and 486).
- docs/MIGRATING-3.0.md.

### Changed

- The bindings are pregenerated in `src/redisraw/bindings.rs`, so a default build needs no libclang. `bindgen-runtime` and `bindgen-static` regenerate them with bindgen 0.73, and CI checks that the committed file is current.
- The crates use edition 2024 and declare `rust-version = "1.87"`, which CI tests. The pinned toolchain is 1.94.0 (upstream PR 479).
- Removed dependencies: `nix`, `strum_macros`, `regex`, `backtrace`, `enum-primitive-derive` and `num-traits`.
- `RedisKey::hash_get` takes `impl AsRef<[u8]>` (upstream PR 470).
- A panic in a command replies `ERR command panicked: <message>`, and a panic in other callbacks is logged, instead of aborting the server.
- A missing module API is reported by name (`RedisModule_X is not available in this Redis server`) instead of as an unwrap of `None`.
- `redis_module!` fails the module load when a configuration registration fails, when `LoadConfigs` rejects a value, or when a module built with `system-allocator-fallback` already allocated from the system allocator.
- Log messages are passed to Redis as the argument of a fixed `%s` format.
- The crash report's `trace` field is `std::backtrace::Backtrace` output.

### Fixed

- `RedisString` views could outlive their owner, and strings freed themselves through the context they were created with; several producers leaked strings or freed them twice (stream iterators, `ListPop`, server info, the keys cursor, the scan cursor).
- Reading through a `RedisKeyWritable` value or string buffer could alias a second one.
- A forwarded call string reply holding NUL panicked, and one holding CR or LF corrupted the reply.
- `RedisAlloc` ignored alignments above 16 bytes.
- String configuration getters leaked their replies.
- The stream iterator deleted an entry when dropped instead of stopping.
- `RedisKey::to_raw_parts` closed the key it handed over.
- `RedisString::create` and `Context::create_string` panicked on a NUL byte, and error replies turned non-ASCII text into invalid UTF-8.
- Cron event handlers received 1 instead of the server's `hz`.
- Verbatim call replies read their format through a null pointer.
- `hash_set` and `hash_del` reported replacing or deleting a field as an error, and `delete` and `unlink` did not say whether a value was removed.
- `RedisGILGuard` could hand out two mutable references, and safe code could build a `Context` from any pointer.
- `reply_with_call_reply` left a RESP2 client waiting when Redis refused a RESP3 reply.
- `block_client_on_reply` blocked where Redis does not allow it (no client, `MULTI`, scripts, reply and timeout callbacks).
- `logging::setup_for_context` replaced the logger's context even when it failed, and `DefragContext::defrag_alloc` passed a zero-size layout to the allocator.
- Keyspace event names that are not UTF-8 no longer panic in the event macro (upstream PR 475).
- `call_blocking` is available under the 8.0 floor.

## [2.0.8](https://github.com/RedisLabsModules/redismodule-rs/compare/redis-module-v2.0.7...redis-module-v2.0.8) - 2026-05-04

### Fixed

- CI: Revamp the release pipeline, moving over to Trusted Publishing for all crates in the repository.
