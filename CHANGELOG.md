# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Documentation of three server behaviors the SDK does not guard against
  (#225). [docs/CLUSTER.md](docs/CLUSTER.md): in cluster mode on Redis 7.4
  and later, keys that `open_key`, `open_key_writable` or `call` reach during
  a command that declares keys, from the command or a keyspace notification
  callback, must hash to the declared keys' slot, or reads miss and writes
  land in that slot. [docs/NATIVE-TYPES.md](docs/NATIVE-TYPES.md): without
  `ModuleOptions::HANDLE_IO_ERRORS`, a short read during an RDB load aborts
  Redis and the abort message names the key under
  `hide-user-data-from-log yes`; and on Redis 7.2 and later an `aux_save`
  that calls `RedisModule_GetContextFromIO` through `RdbWriter::as_ptr` and
  writes nothing leaves the context unreleased. After a `SAVE` that runs such
  a callback, Redis aborts on the next write while AOF is on or a replica is
  attached, and on the next thread-safe context lock. Rustdoc on
  `ModuleOptions::HANDLE_IO_ERRORS`, `set_module_options`,
  `NativeType::rdb_load`, `AuxData::aux_save`, `RdbReader`,
  `RdbReader::as_ptr`, `RdbWriter::as_ptr`, the `cluster` module, the
  `open_key` and `open_key_writable` methods, `call` and `call_ext` describes
  each behavior where it applies.

### Changed

- **Breaking:** `Context::replicate_verbatim` returns a `Status` instead of
  `()`, and `Status::Err` when it refuses to replicate during a write pause
  (#220). It and `RuntimeContext::replicate` are `#[must_use]`, so a call
  that ignores the status warns.

### Fixed

- `MODULE UNLOAD` no longer stops the server when an ACL rule names one of the
  module's commands (#221). From Redis 7.2 the unload reapplies every user's
  command rules, asserts that each one still applies, and aborts at `acl.c`
  `'res == C_OK'` for a rule naming a removed command; before 7.2 the unload
  succeeds and the next command that describes the user, such as `ACL LIST`,
  `ACL GETUSER` or `ACL SAVE`, stops the server. The unload hook of
  `redis_module!` now reads `ACL LIST` before anything else and refuses the
  unload, logging the user and the rule and whether it is in a selector, while
  a rule names a command the SDK registered for the module: `+cmd`, `-cmd`,
  `+cmd|sub` or `-cmd|sub`, in any case, at the top level or in a selector.
  First-argument rules such as `+cmd|arg` are refused too, although Redis
  before 7.2 does not fail on them. Rules on ACL categories, including
  categories the module added, do not block the unload. When `ACL LIST`
  cannot be read, for example because `ACL` is renamed, the hook logs a
  warning and the unload goes ahead unchecked: Redis asserts that the unload
  after a rejected `MODULE LOADEX` configuration succeeds. Commands registered
  with the native API directly are not checked.

- Replicating while Redis pauses writes no longer aborts the server (#220).
  `CLIENT PAUSE ... WRITE`, `FAILOVER` and `CLUSTER FAILOVER` pause
  replication, and Redis 6.2 and later abort at an assertion when a module
  replicates from code that still runs then: a read-only command, a timer, a
  server event or a worker. While `RedisModule_AvoidReplicaTraffic()` reports
  the pause, `RuntimeContext::replicate` and `Context::replicate_verbatim`
  return `Status::Err` without replicating, and `call_ext` and
  `call_blocking` with a replicate flag return an error without running the
  command, before they change anything. A command registered `write` or
  `may-replicate` that a client queued in the transaction that starts the
  pause still replicates, as Redis allows: Redis postpones a transaction that
  queued one during an earlier pause. Command registration records which
  callbacks are flagged that way. A command another command or a script
  called is refused, as is a read-only command in any transaction and a
  handler that is also registered for a command without either flag. The new
  `replicate` example replicates from each of these paths, and the `lists`
  example reports a refused replication.

- A call from a post-notification job no longer aborts the server after a
  transaction that pauses writes itself (#231). Redis lets a transaction such
  as `MULTI`, `CLIENT PAUSE ... WRITE`, `SET`, `EXEC` replicate its own
  writes, and runs its jobs after `EXEC` and before it propagates the
  transaction. Any command run outside `EXEC` ends that exemption, so a job's
  `RedisModule_Call`, even a read without the replicate flag, made Redis 7.2
  and later abort at the `propagateNow` assertion. While
  `RedisModule_AvoidReplicaTraffic()` reports a pause, `call`, `call_ext`,
  `call_blocking` and the SDK helpers built on them return
  `ERR post-notification jobs cannot call commands while Redis pauses replication`
  from a job, without running the command. The SDK cannot tell that
  transaction's jobs from other jobs, so it refuses every job call during a
  pause, reads included; a replicated call from a job, already refused for
  #220, now gets this error. Jobs can still read keys with `open_key`. A
  client command that Redis runs during a yield, such as a replica's
  `allow-busy` `REPLCONF ACK`, ends the exemption too, so during a pause
  `yield_now` from a job processes events and serves no clients, whichever
  `YieldMode` it asks for. The `keyspace_to_stream` example's job probe logs
  its call's error and has a mode that yields. The documentation of `call`
  and `call_ext` notes that on Redis 6.2 a read-only command whose call writes
  inside `MULTI` during an earlier write pause still aborts the server, and
  that of `yield_now` that replication queued before a yield cannot be
  refused once a pause starts during it.

- A module call that starts a replica pause no longer aborts the server
  (#235). Redis propagates what an execution unit queued after the unit
  ends, and sets the exemption for a pause only inside `EXEC`. A
  `CLIENT PAUSE` through `call` after `replicate_verbatim` or a replicated
  `call_ext`, or after opening an expired key, which queues a `DEL`, made
  Redis abort at the `propagateNow` assertion (`server.c:3751` on 8.10.2,
  `server.c:3254` on 7.2.16, and at `propagate`, `server.c:3609`, on 6.2.24,
  except for the expired key, which 6.2 propagates at once). `FAILOVER`
  through `call`, and `SHUTDOWN` through `call_blocking` while a replica
  lags, abort the same way. Outside a transaction, `call`, `call_ext`,
  `call_blocking` and the SDK helpers built on them now return
  `ERR calls cannot pause replication outside a transaction` without running
  `CLIENT PAUSE` in any mode or `FAILOVER` other than `FAILOVER ABORT`, and
  `call_blocking` returns it for `SHUTDOWN` without `NOW` or `ABORT`, matching
  names case-insensitively, from every context. A call that cannot block gets
  Redis's own error for that `SHUTDOWN` on 7.0 and later, and 6.2 shuts down
  without pausing. The SDK cannot see what is queued, so it refuses when
  nothing is queued too, and on Redis 6.0, which does not abort. Inside
  `EXEC` the call runs. The `replicate` example's `replicate.then_call` and
  the `call` example's `call.blocking_after_set` queue replication and then
  make a call.

- `RedisString::parse_unsigned_integer`, and with it `NextArg::next_u64` and
  `ArgCursor::next_u64`, accept the whole `u64` range (#222). They parsed with
  the signed parser, so 9223372036854775808 through 18446744073709551615 got
  `ERR Couldn't parse as integer`. The SDK now parses the string's bytes in
  Rust on every compatibility floor, with the signed parser's grammar: `0`, or
  a non-zero digit followed by digits, with no sign, spaces, `+`, leading
  zeros or NUL. Negative integers still get
  `ERR Couldn't parse negative number as unsigned integer`, and other input
  `ERR Couldn't parse as integer`. The `args` example replies the parsed values
  as big numbers instead of casting them to `i64`, and the `native_type`
  example's `note.aux` refuses values above `i64::MAX`, which `note.counts`
  cannot report as an integer.

## [3.0.0-alpha.2](https://github.com/joshrotenberg/redismodule-rs/compare/v3.0.0-alpha.1...v3.0.0-alpha.2) - 2026-11-04

### Added

- Incremental stream entries through `StreamIterator::next_entry` and
  `StreamEntry::next_field`, `remaining_fields` and `skip_fields` (#179,
  parts 1, 2 and 4). Skipping or dropping an entry uses native null outputs
  without creating field/value strings. Pending fields remain tracked by the
  iterator, so forgetting an entry and mixing incremental and legacy reads
  still advances at entry boundaries. The existing `Iterator` returns complete
  `StreamRecord`s. All public cursor advances honor scoped key-data loans;
  an unexpected field failure ends the cursor instead of reusing its state.
- Borrowing `RedisKey::as_ptr` and `RedisKeyWritable::as_ptr` accessors keep
  the SDK owner and its cleanup responsibility in place (#179). Native calls
  remain unsafe and subject to key lifetime, lock, mode and aliasing rules.
- `RedisString::parse_stream_id` forwards to native `StringToStreamID`, with
  an error if unavailable or refused (#179). The running server defines its
  numeric/range-ID grammar; command markers such as XREADGROUP's `>` remain
  separate. AutoMemory adoption is unchanged; part 3 of #179 remains open.

- Complete the module-author rustdoc for key reads, hash fields, list writes,
  expiry, native call replies and server events (#169). Cursor examples now
  compile against the current API, macro input syntax uses the current callback
  contract, and guide references follow the pinned dependency source. Both macro
  packages include the BSD license and fork maintainer metadata; the pre-release
  checklist also updates the shipping recipe's source pin.

- Integration tests load the `lists` and `load_unload` examples and run
  `export_shared_api`, `replicate_verbatim`, `reply_with_key` and
  `list_push_head`, which nothing ran before (#168). New `shared_api` and
  `shared_api_user` examples show one module calling a function another
  exports. The `lists` example now replicates its writes, which the key API
  does not propagate on its own, and rotates a list given to `LPOPRPUSH` twice
  through one key handle instead of two. `LPOPRPUSH` also replies with the
  element's bytes unchanged, where it replaced invalid UTF-8, and refuses
  extra arguments.

- `CommandBuilder` sets every field of the command info (#208):
  `.complexity()`, `.since()`, `.history()`, `.tip()`, `.key_spec()` and
  `.arg()` join `.summary()` and `.arity()` in the builder's one
  `RedisModule_SetCommandInfo` call, which Redis accepts once per command.
  Key specs take the `KeySpec`, `KeySpecFlags`, `BeginSearch` and `FindKeys`
  types `#[command]` uses; their flags reach `COMMAND INFO` and
  `COMMAND GETKEYSANDFLAGS` and decide what `%R~` and `%W~` ACL key patterns
  allow, which a key range's spec cannot. Registration checks the info before
  creating the command, refusing key specs Redis accepts but mishandles (a
  key step of 0 overruns its key buffer; an index of 0, or a limit with a last
  key other than -1, fails its assertions), NUL bytes, tips with spaces, and key
  specs on a container. Redis copies every string, and the builder frees them
  after the call, as `#[command]` now also does when the call fails. See
  `examples/subcommands.rs`.

- Command containers and native subcommands (#178).
  `CommandBuilder::container(name)` declares a command without a handler, and
  `.subcommand(redis_command!("child", handler))` registers each child as
  `name|child` through `RedisModule_CreateSubcommand`. A child keeps the flags,
  key range, required and optional ACL categories, panic guard and reply
  handling of a top-level command, and its handler receives the container and
  child names as its first two arguments. `.summary(text)` and `.arity(n)` set
  what `COMMAND DOCS` and `COMMAND INFO` report on any builder definition.
  Containers, summaries and arities need Redis 7.0 or later. Registration
  checks a container and all its children before creating anything, and
  refuses a container before 7.0, without subcommands, with keys, with ACL
  categories (Redis grants them to every subcommand), or with an arity that
  accepts a call without a subcommand. It refuses a child that is a container,
  has children, repeats another child's name ignoring case, has an empty name,
  or has an arity of 1, and a container or child name with a space, line
  break or any of `|@=,`, which Redis 7.2 and later refuse. See `examples/subcommands.rs`.
- Key-blocking callbacks can write their reply directly (#207).
  `Context::key_blocking_reply()` returns a `KeyBlockingReply` writer inside
  the `on_ready` and `on_timeout` callbacks of `block_client_on_keys`, so a
  large reply needs no `RedisValue` tree. `array`, `map` and `set` open an
  aggregate; `bulk`, `string`, `integer` and `value` write one element. The
  writer counts what the reply still owes, so the client still gets one
  reply. The first write claims the reply, a write after a complete reply is
  refused, and once anything is written the wait finishes and a returned value
  is logged and dropped. If the callback returns or panics while an aggregate
  still owes elements, the SDK writes each owed element as an error reply. A
  callback that writes nothing keeps the old behavior. The error Redis itself
  writes for `CLIENT UNBLOCK ... ERROR` of the client from inside its own
  callback is not counted.

- `Context::block_client_on_keys` transfers an owned, typed waiter to Redis
  (#177). Only indexed network clients in an active SDK-bound module may
  wait; scripts, transactions and all module calls return owned fallback data.
  Loading, unloading and unbound custom loaders are refused. Blocking an
  already-blocked network client is refused across the SDK blocking APIs.
  Ready callbacks may keep waiting or return a reply; timeout and disconnect
  cleanup release the data once. Callback/destructor panics are
  guarded, a validated timeout avoids native overflow, and denied blocking
  returns the data for a non-blocking fallback. Redis 7.2+ can wake on deletion;
  older servers report their ordinary-readiness fallback. `signal_key_as_ready`
  and `blocked_client_ready_key` expose signaling and an owned ready-key copy.

- The SDK package declares `links = "redismodule"`, allowing Cargo to reject
  another package declaring the same native library identity (#169).
  Legacy SDKs without a `links` declaration still require a dependency audit.
  The generated crate reference now introduces contexts, commands, keys,
  replies and floors; module guides cover master/local dependency overrides,
  post-notification unload guards and raw ownership transitions.

- **Breaking:** `RedisValue::NullArray` replies as a null array in RESP2
  (`*-1`) and null in RESP3 (`_`), including inside aggregates (#180).
  `Null` still sends a RESP2 null bulk string; `None` and `()` conversions
  keep using it. Exhaustive matches on `RedisValue` need a `NullArray` arm.

- The vendored header and committed bindings add
  `REDISMODULE_PERSISTENCE_FLAG_STDRDB` (`1<<1`), matching upstream PR 491
  (#150). It marks standard RDB saves restored one key at a time. Existing
  payload layout, flags, API pointers and compatibility floors are unchanged.

- `RuntimeContext::canonical_key_for_slot` returns an owned copy of Redis's
  exact native slot name on supporting servers (Redis 7.4+) (#146). Invalid
  slots return `None`; unavailable APIs, unexpected null names and invalid
  UTF-8 return errors at every floor. There is no automatic fallback. The
  separate `cluster::key_for_slot` keeps its existing stable names.

- `RuntimeContext::fill_random_bytes` and `fill_random_hex` fill caller-owned
  slices using Redis's generator under the Redis lock (#144). Hex output is
  lowercase ASCII, supports odd lengths and has no trailing NUL. Empty slices
  succeed after checking availability; missing bindings return errors without
  changing the buffer. Signatures are uniform at every compatibility floor.
  The `random_fill` example demonstrates per-load IDs and locked worker access.

- `RuntimeContext::api_available` safely queries the running server for a short
  native API name (#117). Every context view exposes it; unknown/missing names
  return false and interior NUL returns an error. Presence does not grant a
  context permission to call the API. The `api_availability` example demonstrates
  lookup and missing-API handling; `keyspace_to_stream` uses it at load time.

- `EventContext::subscribe_to_server_event` adds typed static function handlers
  at load time or during commands/events (#114). It preserves attribute handlers,
  deduplicates function addresses, contains each handler's panics and snapshots
  each delivery before invoking user code. Failed loads and successful unloads
  release runtime registrations; rejected unloads retain them. The
  `runtime_server_events` example demonstrates conditional subscription.

- **Breaking:** `ServerEventHandler` gains `Cron`, `FlushInfo`, `SwapDb` and
  `ConfigChanged` variants, which breaks an exhaustive `match` on it (#114). It
  is now `#[non_exhaustive]`, so a `match` needs a `_` arm and later variants
  are not breaking.

- `ThreadSafeContext::try_lock` and `DetachedContext::try_lock` return their
  existing borrowed guards on success and `None` when the Redis lock cannot
  be acquired immediately (#113). Workers can check a stop flag between
  attempts so module unload can join them while holding the Redis lock.
  Both methods support every compatibility floor; `examples/try_lock.rs`
  demonstrates acquisition, contention and worker teardown.
- The vendored header and committed bindings include upstream's
  `RedisModulePersistenceInfoV1`, its `RedisModulePersistenceInfo` alias,
  `REDISMODULE_PERSISTENCEINFO_VERSION` and
  `REDISMODULE_PERSISTENCE_FLAG_SYNCER` (#110). These persistence-event
  declarations are absent from Redis OSS 8.10.2's header; no API pointers,
  safe wrappers or compatibility floors change.
- `src/include/redismodule.h` and the committed bindings declare the module API
  that Redis OSS 8.10.2's header has and upstream's lacks (#106):
  `RedisModule_SubscribeToKeyspaceEventsWithSubkeys`,
  `RedisModule_UnsubscribeFromKeyspaceEventsWithSubkeys` and
  `RedisModule_NotifyKeyspaceEventWithSubkeys` (Redis 8.8),
  `RedisModule_AddPostNotificationJobForKey` and
  `RedisModule_GetClusterNodeSlotRanges` (Redis 8.10), with their callback
  types; `REDISMODULE_KEYTYPE_ARRAY` and the subkey delivery flags (Redis 8.8);
  the array and subkey notification flags; and the cluster topology change event
  with its data and flags, and the slot migration `MIGRATE_MODULE_PROPAGATE_END`
  sub-event (Redis 8.10). A server without one of the APIs leaves its `raw`
  pointer `None`, so no compatibility floor changes; the SDK adds no wrappers.
  The Redis Enterprise declarations stay. `REDISMODULE_NOTIFY_ALL` keeps
  upstream's value, without `REDISMODULE_NOTIFY_ARRAY`, so `NotifyEvent::ALL` is
  unchanged. [CHANGES-FROM-UPSTREAM.md](CHANGES-FROM-UPSTREAM.md) lists how the
  header differs from upstream's and from Redis OSS 8.10.2's.

- Documentation: [docs/PORTING-EXAMPLE.md](docs/PORTING-EXAMPLE.md) walks
  through porting a module from upstream 2.1.3 to 3.0 (#63), and the migration
  guide has the corrections that port turned up (#89). CHANGES-FROM-UPSTREAM.md
  assesses the remaining upstream pull requests and the upstream persistence
  flag update (#101, #148).

### Fixed

- A key-blocking ready callback that unblocks its own client with a nested
  `CLIENT UNBLOCK ... ERROR` no longer sends its value after Redis's error
  reply and reports the client served, which made Redis free the native
  waiter twice (#207). The SDK now checks that Redis still holds the client
  blocked before serving it.
- **Breaking:** callbacks that run while Redis is using a key's value can no
  longer change the keyspace (#205). In a `KeysCursor` callback, after which
  Redis closes the key it lent and reads its value, and in a callback Redis
  raises during an SDK key write, such as a `new` keyspace subscriber running
  before the native key holds the value it creates, calls, `yield_now` and
  notifications return `ERR`, key writes return `ERR` or `Status::Err`, and
  pops panic. A `KeysCursor` callback can still read the lent key's string
  value. Keys opened while a keyspace scan runs, or while a key scan scans a
  key opened with `ACCESS_EXPIRED`, open with `NOEXPIRE` (Redis 7.2+), so an
  open does not delete an expired key Redis is using; they close when the scan
  ends and re-open with their own flags.

- Key handles stay valid when something else changes their key (#197). An
  open native key caches the value Redis looked up, and a call, `yield_now`,
  a keyspace notification or scan, or string DMA, a write, delete, pop or trim
  through another handle could free that value: the handle then read and wrote
  freed memory, and from Redis 6.2 closing it did too. The SDK now registers
  every key it opens on its thread and closes them before such an operation,
  while the values are still valid: all of them for a call, `yield_now`, a
  notification or a scan, and the other handles to the same key for a change
  through one handle. A closed handle re-opens on its next use, with the same
  name, mode, flags and database, sees the key as a fresh open would, and has
  an open's effects, such as `keymiss` notifications. Each open costs one
  small allocation for its registry entry. `as_ptr` returns the current native
  key, valid until the next keyspace change.
  **Breaking:**
  - A `StreamIterator` panics on its next step, or its entry's next field,
    after a keyspace change, since it cannot re-open; start a new one.
  - A `ScanKeyCursor` callback cannot change the keyspace, because Redis keeps
    scanning the key after it: calls, `yield_now` and notifications return
    `ERR a key scan callback is running`, key writes return that error or
    `Status::Err`, and pops and keyspace scans panic.
  - Handles built with `RedisKey::from_raw_parts`, and the handles scan
    callbacks receive, panic when used after a keyspace change; one on a
    missing key does not.
  - `RedisKey` and `RedisKeyWritable` are no longer `UnwindSafe` or
    `RefUnwindSafe`.

- SDK-generated error replies consistently start with `ERR` or a specific
  Redis error code (#166), including argument parsing, key operations,
  configuration, registration and API-availability errors. Forwarded server
  errors and explicit module-provided messages keep their codes and text.
- `RedisKeyWritable::set_expire` rejects durations whose signed millisecond
  deadline overflows, preserving the old value and expiry (#166). Redis 6.2+
  receives the checked absolute deadline. Redis 6.0 keeps a checked relative
  fallback, whose later native clock reading still has an extreme-boundary
  clock-change limitation.
- Array, map and set call-reply `Display` separates every element (#166).
- **Breaking:** typed enum configurations reject `ConfigurationFlags::BITFLAGS`
  during registration, since their setter cannot represent combined flags
  (#166). Module-argument enum names now ignore ASCII case, as Redis does.
- The SDK's crash-info dispatcher no longer captures or symbolizes a Rust
  backtrace (#166). Module INFO callbacks still run with the original
  crash-report flag; their own crash-path restrictions continue to apply. A
  follow-up migration note for #166 removes the obsolete SDK-backtrace recipe.

- **Breaking:** safe code can no longer change a key while holding data
  borrowed from it (#162). `RedisKey::read` returns an owned `Vec<u8>`;
  `RedisKey::with_value`, `RedisKeyWritable::with_value_mut` and
  `RedisKeyWritable::with_string_dma` lend the value or the string buffer to a
  closure that must be `Send`, so it cannot hold the context, a key handle or
  a `RedisString`, and while it runs the SDK refuses calls, key opening and
  scans, other key operations, `yield_now` and keyspace notifications on that
  thread, so a context reached another way, such as from a thread local,
  cannot change the key either. The borrowing forms, `read_borrowed`,
  `get_value` and `as_string_dma`, are `unsafe`. A handle opened before the
  same key was deleted or replaced through another handle or a call re-opens
  its key since #197.

- Unusual input no longer panics or silently changes stored data (#165).
  Calls and replication reject NUL command names before creating native
  arguments. Converting a RESP3 map with an unsupported key, or a set with an
  unsupported member, produces an error value for that aggregate. Configuration
  change event names use replacement characters for invalid UTF-8 inside the
  panic guard, and caught panic payloads are leaked so their destructors cannot
  panic out of the guard. Built-in mutex configuration getters recover their
  stored values after poisoning instead of substituting defaults.

- **Breaking:** `RedisModuleIO::read_string` returns an error for invalid UTF-8
  instead of replacing bytes (#165). Read binary data with
  `read_string_buffer`.

- **Breaking:** `RuntimeContext::replicate` now returns Redis's `Status`, including
  `Status::Err` for a command name containing NUL or a native refusal, instead
  of discarding it (#165). Existing calls that ignore the result as a statement
  remain valid.

- Keyspace subscriptions and nonblocking context locks tolerate missing Redis
  6.2 capability APIs on older Redis 6.0 servers (#167). Without flag
  enumeration, keyspace handlers request their mask unchanged, including all
  SDK-known bits for `NotifyEvent::all()`. Both `try_lock` methods return
  `None` when the native nonblocking lock API is unavailable; they do not
  acquire a blocking lock instead.

- **Breaking:** `Context::key_at_pos` returns `Result<(), RedisError>` and
  refuses a position that is not an argument position of the running command
  (#161). Redis checks only that the position is positive and later reads the
  argument at it, so an off-by-one in a getkeys-api handler read past the
  arguments in `COMMAND GETKEYS` and cluster routing. The SDK's command
  callbacks record their context and argument count; from a callback
  registered without them, use the new `unsafe fn key_at_pos_unchecked`.
  Outside a keys-position request `key_at_pos` still does nothing.

- **Breaking:** native data types are typed Rust values with SDK-generated
  callbacks (#154, #155). A type implements `native_types::NativeType`
  (`rdb_load` and `rdb_save`; `free` is `Drop`) and is declared as a
  `static` `RedisType<T>`; `RedisType::new` takes only the name and encoding
  version. Optional callbacks are traits enabled with `with_` methods
  (`MemUsage`, `CopyValue`, `FreeEffort`, `Unlink`, `DigestValue`,
  `AofRewrite`, `AuxData`, `Defrag`). Every callback runs inside a panic guard;
  a panic in `rdb_save`, `aux_save` or `aof_rewrite` ends the process after
  logging rather than write an incomplete value (a background save's child is
  killed, failing only that save). `get_value` and `set_value` are typed
  by the `RedisType<T>`, require `T: Send + 'static` because Redis frees
  values on background threads, refuse a type that was never registered, and
  drop a value Redis refuses. Load callbacks get a read-only
  `io::RdbReader` and save callbacks a write-only `io::RdbWriter`, since a
  write while loading a `RESTORE` payload corrupts the command's argument.
  `unsafe RedisType::from_raw_methods` keeps native callbacks, and
  `RedisModuleIO` adds binary-safe `read_redis_string` and
  `write_redis_string`.

- The `module_config_get` and `module_config_set` commands that
  `redis_module!` registers require the same permissions as `CONFIG` (#160).
  They are registered with `CONFIG`'s flags (`admin deny-script allow-loading
  allow-stale`), so scripts cannot call them, and in the `@admin` and
  `@dangerous` ACL categories on Redis 7.2 and later, and they call `CONFIG`
  with ACL verification for the calling user. Before, any user allowed the
  wrapper, including one denied `@admin`, or a script could read and change
  the module's configurations through them. Callers with no user, such as a
  module's own `ctx.call` of a wrapper, now get an error.

- Event loop callbacks no longer outlive their module or loop forever after a
  panic (#159). With `redis_module!`, a successful unload stops watching every
  descriptor still watched after `deinit` and a failed load those it added,
  since Redis keeps them and calls them in the unloaded image. Redis cannot
  cancel a one-shot, so unload is refused while one is pending or a
  descriptor callback is running, and `add_one_shot` returns an error while
  the module loads or unloads; `add` returns one during and after unload.
  Modules with their own `RedisModule_OnLoad` keep the previous behavior. A
  callback that panics stops being watched in every direction it handles, with
  a warning, instead of being skipped through its poisoned lock on every event,
  and replaced or removed callbacks are dropped with the registry unlocked, so
  their destructors can add or remove descriptors. Destructors that run in a
  Redis callback, or in the unload or failed-load cleanup, are panic-guarded.

- A failed load no longer leaves native event subscriptions into the unloaded
  module on Redis before 7.2.4, which keeps them itself (#158). The
  `redis_module!` load removes the server-event subscriptions it made,
  attribute and runtime handlers alike, and subscribes the `event_handlers`
  keyspace handlers after `init` succeeds, since Redis has no API to remove
  keyspace subscriptions before 8.2; their APIs are checked before `init`.
  Events raised during `init` no longer reach those keyspace handlers.

- A plain blocked client released without a reply no longer waits forever
  (#157). Dropping a `BlockedClient`, or its `ThreadSafeContext`, without a
  reply sends `ERR the blocked client was released without a reply`. When the
  command that blocked the client drops it on the command's own thread before
  returning, the command's reply stands instead (an error it returns, or the
  panic error), and a `NoReply` it returns becomes that error. Commands
  registered without the SDK's command callbacks always get the error. A
  second `ThreadSafeContext<BlockedClient>::reply` returns `Status::Err`
  without writing, and a wrong-arity error through it is sent as
  `ERR wrong number of arguments` instead of crashing Redis.

- **Breaking:** `ThreadSafeContext<BlockedClient>` is no longer `Sync`, since
  its `reply` writes without the Redis lock (#157).
  `ThreadSafeContext<DetachedFromClient>` stays `Sync`.

- **Breaking:** `RedisString` implements `Borrow<[u8]>` instead of
  `Borrow<str>` (#164). Byte-slice collection lookups agree with its hash,
  equality and binary ordering, including invalid UTF-8. Use
  `map.get(name.as_bytes())` or `set.contains(name.as_bytes())` for text keys.
  Equality/order compare byte slices directly, including large length
  differences that the native comparator can truncate through an `int`.

- `RedisAlloc` prefers non-aborting `TryAlloc` when the running server
  provides it (Redis 7.0+) (#163). Fallible Rust allocation and inherited
  allocate/copy/free growth can return errors instead of aborting Redis;
  older servers keep the aborting allocator. Alignment above one word uses
  the existing header/padding path, including 16-byte layouts on Redis
  builds with a size prefix.

- `Context::reply` no longer writes replies that shift the client's later
  replies (#153). A `RedisValue::NoReply` array element or map value, which
  wrote nothing although the aggregate counted it, is replied as null.
  `RedisValue::BigNumber` text other than an optional `-` and decimal digits,
  which RESP3 writes without a length so CR or LF in it could inject replies,
  is replied as an error, for every client and at every compatibility floor.

- **Breaking:** `yield_now` is now an `EventContext` method, also available
  through command contexts, instead of a shared `RuntimeContext` method
  (#156). Locked workers cannot call it: Redis can release their lock during
  a yield while references derived from the guard remain live. Commands and
  events invoked synchronously by workers also refuse yielding. The native
  call requires OS main-thread verification on Linux/macOS; other platforms
  return an error.

- `EventContext::add_post_notification_job` returns `Status::Err` instead of
  panicking when its native API is missing at a 7.2+ compatibility floor (#117).
  Older floors keep their existing API error and all refused paths drop captures.

- Clarify that ACL permission checks and scoped call-user selection can succeed
  for an existing account marked `off` (#141). Deferred work must check enabled
  state separately when disabling its owner should stop that work. Runtime
  behavior, error strings, signatures and compatibility floors are unchanged.

- Modules using `redis_module!` refuse unload before user deinit while Redis
  owns an SDK post-notification job (#139). Ownership lasts through callback
  execution and captured-data cleanup, including cancellation and panics.
  Submissions during module load/unload return `Status::Err` and release
  captures immediately, preventing callbacks from surviving a failed load.

- On servers without `AddACLCategory`, `redis_module!` reports that limitation
  once per load and omits its declared custom names from optional command ACL
  categories (#115). Built-in categories and mandatory checks remain active;
  unrelated optional-category errors still warn. The policy also applies to
  legacy command definitions and builders registered by init.
- **Breaking:** `raw::RedisModule_GetUserUsername` takes the context as its
  first argument, as Redis 8.8 and later define it. The vendored header, like
  upstream's, declared it without one, from a Redis unstable snapshot, so a
  call through the binding passed the user where Redis reads the context. A
  call with the old signature no longer compiles; the SDK does not call it.
  No 2.x release declared it, so only code written against 3.0.0-alpha.1 or
  upstream's unreleased master is affected.
- On Redis 6, `RedisValue::Bool`, `RedisValue::BigNumber` and
  `RedisValue::VerbatimString` replies and `Context::get_current_user` no
  longer panic into `ERR command panicked` (#109). A bool replies with the
  integer 1 or 0 and a big number with a bulk string, as Redis 7 replies to a
  RESP2 client. Verbatim text keeps its type in the `txt` format and is a bulk
  string in any other format. `Context::get_current_user` reads the name with
  `RedisModule_GetClientUserNameById` on Redis 6.2, which finds only
  connected clients, and returns `None` on Redis 6.0.
- On Redis 6, a `RedisValue::Set` or `RedisValue::OrderedSet` reply announced
  twice as many array elements as it sent, so the client waited for elements
  that never came. It now announces as many as it sends.
- Loading a module no longer logs "Skip register defrag function" on Redis
  6.0, or "Skip register defrag callbacks" on Redis 6.2 to 7.4,
  when the module has no defrag code of that kind (#116). The warning now appears only
  for `#[defrag_function]`, `#[defrag_start_function]` or
  `#[defrag_end_function]` code the server cannot run. On Redis 6.0, a module
  with start or end functions now also gets the callbacks warning, which an
  early return used to skip.

## [3.0.0-alpha.1](https://github.com/joshrotenberg/redismodule-rs/compare/a808377...v3.0.0-alpha.1) - 2026-10-27

The first pre-release of 3.0.0, tagged for git consumers; the crates keep
version `3.0.0` (see [CONTRIBUTING.md](CONTRIBUTING.md#pre-releases)). The
comparison starts at `a808377`, the upstream commit this fork branches from.
Entries marked **Breaking** need a change in modules;
[docs/MIGRATING-3.0.md](docs/MIGRATING-3.0.md) describes each one.

### Added

- [Module API coverage](docs/API-COVERAGE.md) lists each module API function
  in the vendored header with the SDK's wrapper status, the Redis version that
  added it, its compatibility floor and the context a wrapper would fit, and
  ranks the gaps by evidence of need. `.github/scripts/api-coverage.py`
  generates the table and checks that it is current.

- `ModuleOptions::REQUIRE_LOADED_KEYS_IN_RAM`, from upstream's header: in
  BigRedis (Redis Flex) mode, a Redis Enterprise extension, it prevents
  direct-to-disk writes of keys loaded from RDB or by `RESTORE`. Redis OSS 6.0
  to 8.10 stores the option and ignores it. `ModuleOptions::all()` and
  complements such as `!ModuleOptions::HANDLE_IO_ERRORS` now include it, so
  name the options a module wants. `src/include/redismodule.h` now matches
  upstream's at `4ab88e7` (upstream PR 489), and the committed bindings include
  the define.

- `FutureHandler`, the handle `FutureCallReply::set_unblock_handler` returns,
  is exported at the crate root, so modules can name a kept handle and its
  `dispose` and `abort_and_dispose` documentation renders.

- [INFO handler state-access guidance](docs/INFO-HANDLERS.md) documents the
  output-only context, native callback paths and crash-report limitations.
  A tested atomic-counter example supports ordinary INFO and nested native
  server-info queries without adding a Redis lock capability.

- A [compatibility-floor workspace guide](docs/COMPATIBILITY-FLOORS.md) and
  isolated multi-consumer compile controls show separate-package and aligned
  builds, including rejection of conflicting floors and dependency defaults.
  SDK features and runtime availability behavior are unchanged.

- [Module testing patterns](docs/TESTING-MODULES.md) demonstrate companion
  event input and an isolated pure-parser fuzz target, with bounded native
  probes and validated libFuzzer/allocator-fallback controls. Parser checks run
  separately in CI; SDK allocator and runtime behavior are unchanged.

- A [module shipping guide](docs/SHIPPING-MODULES.md) covering native build
  inputs, architecture/libc limits, checksummed archives and a bounded load
  smoke test of the extracted artifact, with a pinned `hello` example recipe.

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

- CI covers pinned Redis 7.2, 7.4 and 8.0 through 8.10 releases.
  Checksum-verified server builds are cached, selected explicitly by path, and
  validated on cache hits. Failed matrix jobs retain server build and
  integration logs for seven days.

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
- New variants `KeyType::Array`, for the array keys of Redis 8.8 and later, and
  `ReplyType::Attribute` and `ReplyType::Promise`.
- `KeyFlags::ACCESS_TRIMMED` (upstream PR 468), and the swap-prefetch and `RedisModule_SetOpenFilesReservation` declarations in the vendored header (upstream PRs 478 and 486).
- docs/MIGRATING-3.0.md.
- The fork's integration harness supports module arguments, fallible startup
  with server logs, bounded polling, replica startup, and graceful restarts on
  saved data. Test servers use OS-selected ports and isolated directories,
  with bounded retries for confirmed port collisions on fresh launches;
  TEST_REDIS_SERVER_BIN selects another Redis binary.

### Changed

- The public macro crate completes the Syn 2 migration with a private metadata
  parser, removing its `serde_syn`/Syn 1 dependency without changing accepted
  attribute syntax or generated registration names. Compatibility tests run on
  the supported toolchains in CI; parser error wording may differ.

- Integration tests use the Redis 1.2.2 development client, pinned to preserve
  Rust 1.87 builds. The bounded harness retains binary reply distinctions,
  server-error propagation and pipeline alignment, and directly checks RESP3
  replies. Focused client regressions also run on Redis 6.0 in CI. SDK runtime
  dependencies and supported server versions are unchanged.

- Internal API wrappers and mixed command-list registration macros now parse
  Rust syntax with Syn 2. Command syntax and API-floor-dependent signatures are
  unchanged. Public attribute metadata uses the compatible SDK-owned parser.

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
- Error texts on the wire: wrong-type keys reply `WRONGTYPE`, a denied ACL key check replies `NOPERM`, and a failed native user lookup replies `ERR the user does not exist or is disabled` (documentation corrected in #141; runtime behavior is unchanged).
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

- **Breaking:** `CallReply`, `ErrorReply` and `CallResult` are no longer `Send`.
  Freeing a reply can read the context that made it, and on Redis 6.0 and 6.2 a
  reply dropped on another thread while its command replicated was a data race
  on that context. Copy what another thread needs into owned data, and convert
  errors with `RedisError::from`. See the
  [migration notes](docs/MIGRATING-3.0.md#call-replies-are-not-send).

- `MODULE_CONTEXT`, the detached module context, is freed and cleared when
  `redis_module!` unloads the module or its load fails, and the next load stores
  a fresh one. Previously a reload of a retained image kept the first load's
  context, whose module Redis had freed, so detached workers, event loop
  callbacks and the SDK logger used freed memory. `logging::setup()` can now be
  called again, as on a reload, and adopts the current context.
  `MODULE_CONTEXT.lock()` panics while no context is stored, also when an unload
  releases it while the call waits for the lock, and event loop callbacks that
  run while no context is stored are skipped.

- `MODULE UNLOAD` is refused while an unblock handler from `call_blocking` is
  pending. Redis does not count the call's temporary client as blocked, and
  when the call unblocks after an unload it reads the freed module state and
  calls the handler. Blocking calls also fail while `redis_module!` loads or
  unloads the module, since a handler left pending by `init` outlives a load
  that fails and makes the forced unload after a rejected `MODULE LOADEX`
  configuration fail an assertion. Keep a handle to abort a pending call, or
  let it finish, before unloading; see the
  [upgrade guide](docs/UPGRADING-MODULES.md#blocking-calls-and-unload).

- **Breaking:** call replies borrow the context that made them: `call_ext`
  and immediately resolved `call_blocking` replies are tied to the context
  borrow, as keys and server INFO snapshots are. Redis reads a reply's context
  when it is freed: on 6.0 to 7.0 for every reply, and on 7.2 and later for
  error replies created before the command runs and under AutoMemory, which
  also frees the reply when the callback returns. A reply kept past its
  context read freed memory or was freed twice. `with_call_user` and `with_db`
  closures can return values that borrow their view. See the
  [call reply migration notes](docs/MIGRATING-3.0.md#call-replies-stay-inside-their-context-scope).

- **Breaking:** `RuntimeContext::auto_memory` is `unsafe`. AutoMemory also
  frees the promise reply of each blocking call, so the caller must dispose or
  abort every `FutureHandler` from such a call before the callback returns, and
  drop call replies on the thread that holds the context. AutoMemory stays
  enabled for the rest of the native context's life.
  See the [AutoMemory migration notes](docs/MIGRATING-3.0.md#auto_memory-is-unsafe).

- **Breaking:** blocking-call unblock handlers require `Send + 'static`. Redis
  keeps a handler after `FutureHandler::dispose`, a drop or `mem::forget`, and
  runs it on the main thread, including handlers a module-owned worker
  installs. `abort_and_dispose` no longer frees a dangling pointer when it
  releases a zero-sized handler, and releases the reply before the handler.
  See the
  [unblock handler migration notes](docs/MIGRATING-3.0.md#blocking-call-unblock-handlers-own-their-captures).

- `RedisKeyWritable::trim_stream_by_id` returns `Ok(0)` when a successful
  exact or approximate trim removes no entries, including an empty stream.
  Missing keys, wrong types and unavailable native APIs still return errors.

- **Breaking:** read/write key handles and owning key scan cursors borrow their
  opening runtime context. Safe code can no longer close or use them after a
  callback ends or a worker guard unlocks. DMA and stream views retain that
  context through their key borrow; copy results into Rust-owned data first.

- **Breaking:** server INFO snapshots borrow their producing runtime context,
  preventing cleanup after a worker guard frees that native context or a
  callback's AutoMemory reclaims the snapshot. Copy fields into Rust-owned
  data before leaving the context or unlocking the worker.

- The SDK build script now rejects zero or multiple compatibility-floor features
  before C compilation and API wrapper expansion. Mixed floors previously could
  choose lower-version signatures and fail elsewhere with confusing type errors.
  Valid single-floor builds keep their existing behavior.

- Public macro documentation uses accessible type references and links. CI
  documents every SDK workspace package with warnings denied at each API floor,
  including both macro crates. Macro expansion and runtime behavior are unchanged.

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
- Modules using the registration attributes (`#[command]`, the server event
  handler attributes, `#[info_command_handler]` and the defrag attributes)
  needed their own `linkme` dependency; the attributes now expand through the
  SDK's re-export of it.

## [2.0.8](https://github.com/RedisLabsModules/redismodule-rs/compare/redis-module-v2.0.7...redis-module-v2.0.8) - 2026-05-04

### Fixed

- CI: Revamp the release pipeline, moving over to Trusted Publishing for all crates in the repository.
