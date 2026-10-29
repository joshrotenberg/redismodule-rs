# Changes from upstream

This repository is a fork of
[RedisLabsModules/redismodule-rs](https://github.com/RedisLabsModules/redismodule-rs).
This file lists what it changes relative to upstream: the upstream issues and
pull requests each change relates to, the fixes for problems upstream has no
issue or pull request for, the additions, and the changes that break
compatibility. Commit hashes link to this repository's master, where each change
is one commit.

For what a module has to change, see [docs/MIGRATING-3.0.md](docs/MIGRATING-3.0.md).
For the same changes by version, see [CHANGELOG.md](CHANGELOG.md).

## Redis OSS 8.x header declarations

`src/include/redismodule.h` is upstream's header at `1c84d3d` plus what Redis
OSS 8.10.2's `src/redismodule.h` declares and upstream's lacks, in OSS's text
and at OSS's positions, so a later refresh from either header stays a small
diff (#106). The committed bindings include the additions; the SDK adds no
wrappers for them (#90). Each API gets its `REDISMODULE_GET_API` line in
`RedisModule_InitAPI`, so a server without it leaves the `raw` pointer `None`
and the module still loads, and no compatibility floor changes. The first
release is the first examined `module.c` that registers the API or uses the
name:

- 8.8.3, absent from 8.6.7: `SubscribeToKeyspaceEventsWithSubkeys`,
  `UnsubscribeFromKeyspaceEventsWithSubkeys` and
  `NotifyKeyspaceEventWithSubkeys`, with the callback type
  `RedisModuleNotificationWithSubkeysFunc`; `REDISMODULE_KEYTYPE_ARRAY`; and
  the delivery flags `REDISMODULE_NOTIFY_FLAG_NONE` and
  `REDISMODULE_NOTIFY_FLAG_SUBKEYS_REQUIRED`.
- 8.10.2, absent from 8.8.3: `AddPostNotificationJobForKey`, with
  `RedisModulePostNotifyJobPerKeyFunc`; `GetClusterNodeSlotRanges`;
  `RedisModulePostNotifyJobFunc`; the cluster topology change event
  (`REDISMODULE_EVENT_CLUSTER_TOPOLOGY_CHANGE`,
  `RedisModuleEvent_ClusterTopologyChange`,
  `RedisModuleClusterTopologyChangeInfo`,
  `REDISMODULE_CLUSTER_TOPOLOGY_CHANGE_INFO_VERSION` and the four
  `REDISMODULE_CLUSTER_TOPOLOGY_CHANGE_FLAG_*` reasons); and
  `REDISMODULE_SUBEVENT_CLUSTER_SLOT_MIGRATION_MIGRATE_MODULE_PROPAGATE_END`.
- Not checked: `REDISMODULE_NOTIFY_ARRAY` and the four
  `REDISMODULE_NOTIFY_SUBKEY*` flags. No examined `module.c` uses those
  names; 8.8.3's is the first to use the server's own subkey flags.

The `_NEXT` sentinels for notification flags, events and slot migration
sub-events take OSS's values, and the topology change event's is added.
They have no binding: `build.rs` allowlists variables matching
`(REDIS|Redis).*`, which a name starting with an underscore cannot match.

`RedisModule_GetUserUsername` now takes the context first, as `module.c` of
Redis 8.8.3 and 8.10.2 defines it and OSS's header declares it. Upstream's
header declares it with only the user, from the same unstable snapshot, so a
call through upstream's binding passes the user where Redis reads the context
and reads the user from an unset argument. The SDK does not call it. No
upstream issue or pull request reports it.

The header still differs from OSS 8.10.2's:

- It keeps upstream's Redis Enterprise declarations: the `Big*` entries,
  `IsKeyInRam`, `SetOpenFilesReservation`, `SwapPrefetchKey` and its flags,
  `REDISMODULE_OPTIONS_REQUIRE_LOADED_KEYS_IN_RAM` and
  `REDISMODULE_NOTIFY_TRIMMED`.
- It includes upstream's persistence-event payload
  `RedisModulePersistenceInfoV1` and its `RedisModulePersistenceInfo` alias,
  `REDISMODULE_PERSISTENCEINFO_VERSION` and
  `REDISMODULE_PERSISTENCE_FLAG_SYNCER` from [upstream PR 490][p490]. OSS
  8.10.2's header has none of these declarations. They add no API pointers.
- Value 8 has two key type names: upstream's `REDISMODULE_KEYTYPE_GCRA`, from
  a Redis unstable snapshot, and OSS's `REDISMODULE_KEYTYPE_ARRAY`. OSS
  returns a GCRA type only when built with `ENABLE_GCRA`. `KeyType::Array`
  now uses `REDISMODULE_KEYTYPE_ARRAY` instead of a constant of its own.
- `REDISMODULE_NOTIFY_RATE_LIMIT` keeps upstream's `1<<18`. OSS defines it as
  `1<<24`, only with `ENABLE_GCRA`, and leaves bit 18 unused; the header takes
  neither that nor the `ENABLE_GCRA` value of `_REDISMODULE_NOTIFY_NEXT`.
- `REDISMODULE_NOTIFY_ALL` keeps upstream's value. OSS adds
  `REDISMODULE_NOTIFY_ARRAY` to it; taking that would add array events to
  `NotifyEvent::ALL`, and before Redis 8.8 every subscription with it would
  log the bit as unsupported.
- `RedisModulePostNotificationJobFunc` stays upstream's typedef, which
  `AddPostNotificationJob` still names; OSS makes it an alias of
  `RedisModulePostNotifyJobFunc`, the same function type.
- The five long-double declarations stay commented out, and
  `RedisModule_InitAPI` stays separate from `RedisModule_Init`, as upstream
  has them.

## Defrag registration warnings

Registration logs a warning only for defrag code the server cannot run: a
`#[defrag_function]` before Redis 6.2, which added
`RedisModule_RegisterDefragFunc`, and a `#[defrag_start_function]` or
`#[defrag_end_function]` before Redis 8.0, which added
`RedisModule_RegisterDefragCallbacks`. Upstream checks each API before it checks
whether the module registered any such code, so every module logs "Skip register
defrag function" on Redis 6.0 and "Skip register defrag callbacks" on Redis 6.2
to 7.4 and on Valkey 8.1 and 9.1, which have no
`RedisModule_RegisterDefragCallbacks`. On Redis 6.0 it also returns after the
first warning, so a module with start or end functions gets no warning about
them. The #89 port of redis-event-stream-module, which has no defrag code,
logged the callbacks warning on Redis 7.2.16 and Redis Stack 7.4.7. An
integration test checks the server log for a module with no defrag code and one
with all three kinds; before the change it failed on Redis 6.0.20 and 7.2.16. No
upstream issue or pull request reports it.

## Redis 6 replies

`RedisValue::Bool`, `RedisValue::BigNumber` and `RedisValue::VerbatimString`
replies and `Context::get_current_user` use module APIs that Redis 7.0 added.
Upstream unwraps them, so they panic on Redis 6; in the fork the panic guard
turned that into `ERR command panicked`, and a bool inside an array or a set
left the client waiting for the rest of the reply. They now reply the way
Redis 7 replies to a RESP2 client: a bool is the integer 1 or 0 and a big
number a bulk string. Verbatim text keeps its type in the `txt` format, which
Redis 6 can send, and is a bulk string in any other format. `get_current_user`
reads the name with `RedisModule_GetClientUserNameById` on Redis 6.2 and
returns `None` on Redis 6.0.

A set reply on Redis 6, which has no `RedisModule_ReplyWithSet`, is an array.
Upstream, like the fork before this change, announced twice as many elements as
it sent, so the client waited for elements that never came.

An integration test that runs in the Redis 6.0.20 CI job checks each reply over
RESP2 and RESP3. No upstream issue or pull request reports these problems;
[upstream issue 378][i378] and [upstream issue 404][i404] ask for tests and CI
on Redis 6 in general.

## Array key type

`KeyType` maps value 8 to `KeyType::Array`. Redis 8.8 and later return 8 from
`RM_KeyType` for array keys, the type `ARSET` creates, and declare it as
`REDISMODULE_KEYTYPE_ARRAY`; GCRA is compiled only with `ENABLE_GCRA`.
Upstream's header names 8 only `REDISMODULE_KEYTYPE_GCRA`, from a Redis
unstable snapshot; the fork's header also declares `REDISMODULE_KEYTYPE_ARRAY`,
as the Redis OSS 8.x header declarations section above describes. Upstream's
`KeyType` has no variant for 8, and its `From<c_int>` unwraps `from_i32`, so
`key_type` on an array key panics there. Before this change the fork reported
such keys as a `KeyType::Gcra` it had added. An integration test checks every
built-in key type the server has, arrays included where the server provides
`ARSET`; on Redis 8.10.2 it reported `Gcra` for an array key before the change.
No upstream issue or pull request reports it.

## Call replies are not Send

`CallReply` and `ErrorCallReply`, and through them `ErrorReply` and
`CallResult`, are no longer `Send`. Upstream PR 349, which added the cron server
event, added both impls without a use or a soundness argument, and nothing in
the SDK, its examples or the wasm host uses them. Freeing a reply reads the
context that made it in `autoMemoryFreed`: on Redis 6.0 to 7.0 every reply
carries it, and on 7.2 and later error replies created before the command runs
and replies under AutoMemory do. On 6.0 and 6.2, `RM_Replicate` and `RM_Call`
with `!` write the field it reads on the owning thread, so safe code that
dropped a reply in a scoped thread while its command replicated raced on it.
With ThreadSanitizer Redis 6.0.20 a probe reported a data race between
`moduleReplicateMultiIfNeeded` and `autoMemoryFreed`, and a control that dropped
the reply on the owning thread reported none. From 7.0 only AutoMemory writes
the field, and `auto_memory` is already unsafe. Compile-fail controls reject
moving each type to another thread. Upstream has the same impls; no upstream
issue or pull request reports the race.

## Detached context across reload

`redis_module!` frees and clears `MODULE_CONTEXT` when an unload commits or a
load fails, and stores a fresh detached context on every load. Previously the
macro ignored `set_context` refusing a second context, so when the module's
image stayed mapped a reload kept the first load's context, whose `RedisModule`
`moduleUnload` had freed. On AddressSanitizer Redis 8.10.2 a detached worker's
`RM_Call` after a reload was a heap-use-after-free. The SDK logger kept the same
stale context, because `log::set_logger` succeeds once per process;
`logging::setup()` now adopts the current context when the SDK logger is
already installed, and the logger leaves a context the SDK frees.
`DetachedContext::lock` re-checks the context after taking the lock, which Redis
takes without reading the context, and panics if an unload released it
meanwhile. Event loop callbacks that Redis runs while no context is stored are
skipped. A native regression unloads and reloads three times and fails one
load, with the logger and a detached worker, and checks that the image stayed
mapped; without the unload release it reports a replaced stale context, and
without the logger change the reload fails. Upstream has the same code; no
upstream issue or pull request reports it.

## Unload with pending blocking calls

The `redis_module!` unload hook now refuses `MODULE UNLOAD` while an unblock
handler installed with `set_unblock_handler` has neither run nor been
aborted. Redis does not count an async `RM_Call`'s temporary client in
`module->blocked_clients`, so it accepted the unload, then
`moduleCallCommandUnblockedHandler` read the freed module struct and called the
handler when the call unblocked: a heap-use-after-free under AddressSanitizer
on Redis 8.10.2. The SDK counts installed handlers, releasing each after its
callback or a successful abort, and checks the count before user deinit.
Blocking calls fail while the module loads or unloads: Redis frees a failed
load without calling the unload hook, and asserts that the forced unload after
a rejected `MODULE LOADEX` configuration succeeds. Native regressions refuse
an unload with a pending, panicking or worker-installed handler and allow it
once the handler runs, after an abort, after a failed abort, with no handler and
after a reload; `init` and deinit blocking calls are refused, and the forced
unload after a rejected `LOADEX` succeeds.
Upstream has the same blocking-call code, from upstream PR 333; no upstream
issue or pull request reports it.

## Call reply context lifetimes

`call_ext` and immediately resolved `call_blocking` replies now borrow their
context; previously they were `CallResult<'static>`. Values returned from
`with_call_user` and `with_db` closures may borrow the view, which only widens
what compiles. Redis 6.0 to 7.0 store the calling context in every reply and
read it in `RM_FreeCallReply`; 7.2 and later store it in error replies created
before the command runs and, under AutoMemory, in every reply, which AutoMemory
also frees when the callback returns. With AddressSanitizer, a reply kept past
a command was a stack-use-after-return on 7.0.15, as was a kept pre-execution
error reply on 8.10.2, and a reply kept past a worker guard on 7.0.15 was a
heap-use-after-free. With AutoMemory, a kept reply crashed release builds of
7.0.15 and 8.10.2 when dropped.
`RuntimeContext::auto_memory` is now `unsafe`, because AutoMemory also frees a
blocking call's promise reply and a `FutureHandler` kept past its callback
crashed Redis 8.10.2 in `abort_and_dispose`. Compile-fail controls reject the
escapes and a safe AutoMemory call; native regressions copy reply bytes in
command and worker contexts with AutoMemory off and on, and dispose a blocking
call's handle before an AutoMemory callback returns. Upstream has the same
signatures; upstream PR 233 noted that AutoMemory may not work with the SDK's
RAII ownership, and no upstream issue or pull request addresses call replies.

## Blocking-call unblock handlers

`FutureCallReply::set_unblock_handler` requires `Send + 'static` handlers.
Redis keeps the boxed handler on the promise until the command unblocks or is
aborted, and `FutureHandler::dispose` releases only the promise reply's
reference, so a handler borrowing locals could run after their scope ended.
A handler installed from a module-owned worker ran on the main thread without
a `Send` bound. `abort_and_dispose` released an aborted handler with
`std::alloc::dealloc`, which for a zero-sized handler freed a dangling pointer
and aborted the server; it now uses `Box::from_raw`. Compile-fail controls
cover dispose, abort, `mem::forget` and a non-`Send` capture. Native
regressions abort empty and capturing handlers, checking that neither runs and
each capture drops once, and abort a kept handle after its handler ran.
`FutureHandler` is now exported, so a kept handle can be named. Upstream has
the same code, from upstream PR 333; no upstream issue or pull request reports
it. Unloading a module with a pending blocking call and replies kept past
AutoMemory collection are separate.

## Stream trim results

`RedisKeyWritable::trim_stream_by_id` accepts zero as a successful deletion
count; only a negative native result is an error. Exact and approximate native
regressions compare deletion counts and retained entries with `XTRIM MINID`,
including no-op and empty-stream trims. Missing keys and wrong types still
return errors; API availability and the public method signature are unchanged.

## Open key context lifetimes

Safe key opens now return `RedisKey<'ctx>` and `RedisKeyWritable<'ctx>` tied to
`RuntimeContext`; an owning `ScanKeyCursor<'ctx>` preserves that borrow. Native
key close uses the opening context even without AutoMemory, so previously a
worker could unlock and free that context before dropping its key. Compiler
regressions reject guard escape and early unlock across all four open methods,
DMA/stream views and owning key scans. Native worker coverage exercises reads,
writes, binary DMA data and scanning with AutoMemory disabled and enabled.
Unsafe raw constructors retain their context, lifetime and lock obligations;
no general native-handle storage API is introduced.

## Server INFO snapshot lifetime

`RuntimeContext::server_info` returns a snapshot tied to its runtime borrow.
Previously safe code could keep the snapshot after a worker guard unlocked
and freed its native context; snapshot cleanup then used that stale context.
Compile controls now reject both scope escape and early guard drop. Native
tests cover valid worker use with AutoMemory off and on, and copying fields
into owned Rust values before unlocking. General native-handle storage is separate.

## INFO handler state access

The [INFO handler guide](docs/INFO-HANDLERS.md) traces ordinary, nested and crash
callbacks across tagged Redis releases. `InfoContext` remains an output-only
view without a Redis lock proof or runtime operations. Compile-fail controls
preserve that boundary; a native-tested atomic snapshot example supports
ordinary INFO while skipping custom output for crash reports. This does not
promise an async-signal-safe SDK crash-report path or introduce a new capability.

## Compatibility-floor workspace controls

A [workspace guide](docs/COMPATIBILITY-FLOORS.md) and isolated compile fixture
exercise two module consumers using the same SDK. They verify separate and
aligned floor selections. The SDK build script now rejects conflicting defaults,
combined floors and missing floors before C compilation or wrapper expansion. CI runs the controls on the pinned toolchain
and MSRV. This documents the current contract without changing defaults or native
availability checks; feature redesign is separate.

## Macro implementation dependencies

Both macro crates use Syn 2. The public crate replaces `serde_syn` with a private
parser for the existing metadata grammar; command and reply generation retain
their existing names and behavior. Compatibility tests cover typed and quoted
metadata, options, recursive command help, key specs and integer rules. Invalid
metadata still fails compilation; diagnostic wording may differ.

## Base

- This fork branches from upstream master at [a808377] (2026-09-27, the merge of
  [upstream PR 486][p486]).
- Upstream commits since then that this fork does not have:
  - [cca5360](https://github.com/RedisLabsModules/redismodule-rs/commit/cca5360)
    (2026-10-16, [upstream PR 488][p488]): fixes `ServerInfo::field` leaking its
    string. This fork has the same fix in [813cfa0], and `field` returns `None`
    for a field name containing a NUL byte, as upstream's does, since
    [9ea85f4].
  - [4ab88e7](https://github.com/RedisLabsModules/redismodule-rs/commit/4ab88e7)
    (2026-10-20, [upstream PR 489][p489]): adds
    `REDISMODULE_OPTIONS_REQUIRE_LOADED_KEYS_IN_RAM` to `redismodule.h`. This
    fork's header and committed bindings include the define, and
    `ModuleOptions::REQUIRE_LOADED_KEYS_IN_RAM` exposes
    it. Redis OSS 6.0 to 8.10 stores the option and ignores it.
  - [1c84d3d](https://github.com/RedisLabsModules/redismodule-rs/commit/1c84d3d2f4597421f196f1110f80bfe48be178c9)
    (2026-10-26, [upstream PR 490][p490]): adds the persistence-event payload
    and syncer flag. Included in the header and committed bindings; see the
    header differences above. The upstream review checked all 42 open pull
    requests on 2026-10-29; none had been updated since the previous review
    at 2026-10-22T20:45:00Z. The older pull request assessment stays in #101.

## Upstream issues this fork fixes

"Partly" means the change covers part of what the issue asks; the note says
what is left.

| upstream issue | title | change | status |
|---|---|---|---|
| [7][i7] | Parameters shouldn't be owned by command | `ArgCursor` parses arguments by borrowing them ([ce0c36d]). Commands still receive `Vec<RedisString>`. | partly |
| [89][i89] | `cargo test` panics on crates with dependency on `redis_module` | The `system-allocator-fallback` feature lets a module's tests allocate outside Redis ([a6841fb]). | fixed |
| [134][i134] | Support for non-utf8/binary-safe data | `RedisString::create` keeps NUL bytes ([7333dba]); call replies holding NUL, CR or LF stay bulk strings instead of becoming simple strings ([b29cd45]); hash fields can be any bytes ([dd25376]). `RedisKeyWritable::write` still takes `&str`. | partly |
| [150][i150] | Issues building examples modules | A default build uses committed bindings and needs no bindgen or libclang ([c1dc312]). It still compiles a C stub with `cc`. | partly |
| [205][i205] | Functions suppressing not_unsafe_ptr_arg_deref clippy lint are unsound | Functions that take raw pointers are `unsafe` or private; no `not_unsafe_ptr_arg_deref` allows remain ([ce0c36d], [813cfa0], [24cd5ff], [3de40d8]). | fixed |
| [234][i234] | Incorrect Return Type from `Context::call()` | Call replies keep simple versus bulk strings ([b29cd45], [41238c3]). | fixed |
| [246][i246] | How to use binary/bytes/stringbuffer type | Hash fields and values as bytes ([dd25376]). `RedisKeyWritable::write` still takes `&str`; `as_string_dma` writes bytes. | partly |
| [251][i251] | key.delete always returns okay - whether or not the key exists | `delete` and `unlink` return whether a value was removed ([0196506]). | fixed |
| [287][i287] | Replace `unwrap` with `expect` and proper error message | Module API calls go through macros that name a missing API instead of unwrapping `None`; no `RedisModule_X.unwrap()` calls remain ([68b33ab]). | fixed |
| [332][i332] | Consider changing command flags in bitflags | `redis_module!` checks command flags at compile time ([1c57c65]). They are still a string. | partly |
| [351][i351] | Separate detached and thread safe context to be a different context | `Context` can no longer be built from any pointer by safe code, and `RedisGILGuard` can no longer hand out two mutable references ([24cd5ff]). Command, registration, event, and worker views now separate client and module capabilities through a shared runtime view. Worker guards borrow their owner; native resource lifetimes remain separate work. | partly |
| [358][i358] | Refactor RedisString API | String views borrow from their owner, and strings no longer keep or free through the context they were created with ([ce0c36d], [813cfa0]). | fixed |
| [365][i365] | Redis requires the length of native type names to be exactly 9 characters? | A type name or encoding version Redis would reject fails to compile ([2444ae1]). | fixed |
| [416][i416] | Work around rust-analzyer | Doc comments inside the `redis_module!` matcher are plain comments ([113216b]). | fixed |
| [473][i473] | Logging panics across the FFI boundary on messages containing interior NUL bytes | Log messages go through a fixed `%s` format, with NUL written as `\0` ([3700a31]). | fixed |

Related: [upstream issue 471][i471] (Unsound usages of unsafe implementation
about c_void) is about enum configuration registration. This fork does not change
the `c_void` use the issue describes; configuration registration now returns
Redis's errors and frees the private data Redis rejects ([c3432c5]).

## Upstream pull requests this fork includes, adapts or supersedes

| upstream PR | title | state upstream | in this fork |
|---|---|---|---|
| [93][p93] | Changed parse_call_reply to return a BulkString. | open | Adapted: string call replies keep their type ([b29cd45], [41238c3]). |
| [247][p247] | adds bytes-accepting versions of key methods | open | The hash part, as `hash_set_by_string` and `hash_del_by_string` ([dd25376]). |
| [269][p269], [277][p277] | clean some code warnings; revert get-value lifetime changes | closed; both merged | 269 tied the reference `RedisKeyWritable::get_value` returns to the key; 277 reverted that so a caller could keep the reference next to the key. This fork ties it to the key again, through `&mut self` ([72eaf1a]); see the breaking changes. |
| [368][p368] | fix: version_from_info from info cmd && change test start_redis_server_with_module add module args | open | The empty-capture panic is fixed ([b29cd45]) and the parser no longer uses a regex ([86b8113]). |
| [375][p375] | Update dependencies and refactor the FFI calls | open | Covered differently: module API calls go through lookup macros that name a missing API ([68b33ab]), and dependencies were removed or updated ([86b8113], [0e1d8f2]). |
| [386][p386] | Derive hash and comparison traits for RedisAlloc | open | Ported ([b0135e0]). |
| [397][p397] | Add API to get dbsize | open | Ported ([b0135e0]). |
| [417][p417] | README: update Redis Modules API link | open | Included, with the other redirected links ([113216b]). |
| [420][p420] | Breaking Change Proposal: Replace `isize` with `i32` or `u32` as representations for enums. | open | Done as `#[repr(i32)]`, with non-panicking conversions ([5dbf237]). |
| [427][p427] | Replace doc /// comment into regular comment // inside macro. | open | Included ([113216b]). |
| [467][p467] | implement std::error::Error for RedisError and cleanup | open | Reworked: `RedisError` implements `Error`, with explicit `From` conversions ([eaaf9b2]). |
| [476][p476] | fix: make logging total on messages with interior NUL bytes | closed | Adapted, and the message no longer passes as a format string ([3700a31]). |
| [477][p477] | Add example and support for shutdown server event | open | Ported ([b0135e0]). |
| [483][p483] | [MOD-17680] Honour layouts requiring more than 16-byte alignment | open | The same bug is fixed here ([e81393d]). |
| [487][p487], [488][p488] | Fix ServerInfo::field leak with a null context.; Fix ServerInfo field string leaks with and without auto-memory | closed; 488 merged | The same fix ([813cfa0]), and 488's `None` for a field name with a NUL byte ([9ea85f4]); see Base. |
| [489][p489] | Add `REDISMODULE_OPTIONS_REQUIRE_LOADED_KEYS_IN_RAM` to redismodule.h | closed; merged | Included in the header and bindings, and exposed as `ModuleOptions::REQUIRE_LOADED_KEYS_IN_RAM`; see Base. |
| [490][p490] | update redismodule.h with RedisModulePersistenceInfo | closed; merged | Included in the header and bindings: the persistence-event payload, version and syncer flag; see Base. |
| [85][p85], [348][p348], [360][p360], [361][p361] | dependency updates | open | Superseded: `enum-primitive-derive` removed ([5dbf237]), `nix` and `strum_macros` removed ([86b8113]), bindgen at 0.73 ([0e1d8f2]). |

## Fixes with no upstream issue or pull request

Upstream's issues and pull requests, open and closed, were searched for each of
these in October 2026.

| change | problem |
|---|---|
| [b6371e9] | The `proc_macro_commands` example declared a key spec on a command with no keys; Redis 8.10 crashes on that spec. |
| [3700a31] | Log messages were passed to `RedisModule_Log` as the format string, so `%` in logged text was undefined behavior. (The NUL panic is upstream issue 473.) |
| [1d7aab1] | Modules using the registration attributes needed their own `linkme` dependency. |
| [e7d39ce] | `call_blocking` was compiled out under the 8.0 floor. |
| [813cfa0] | Strings freed themselves through the context they were created with, which may be gone; `ListPop` and the keys cursor leaked strings (the server info leak is upstream PRs 487 and 488); stream iterator strings were freed while Redis's automatic memory still held them. |
| [d2f790a] | String configuration getters leaked one string per `CONFIG GET`. |
| [c3432c5] | A rejected configuration registration was ignored and leaked its private data. |
| [c478791] | A panic in any callback unwound into Redis and aborted the server. |
| [a83e6d3] | Dropping a stream iterator deleted the current entry instead of stopping. |
| [762ab77] | A value rejected by `LoadConfigs` at load time was ignored; `init` ran with defaults and Redis then unloaded the module. |
| [7333dba] | Simple string replies with CR or LF ended early, and error replies turned non-ASCII text into invalid UTF-8. |
| [752b2a7] | Keys of the wrong type got a custom error instead of `WRONGTYPE`. |
| [d742fec] | Cron event handlers received 1 instead of the server's `hz`. |
| [332c68a] | `RedisKey::to_raw_parts` closed the key it handed over. |
| [e074cdb] | Verbatim call replies read their format through a null pointer. |
| [dd25376] | `hash_set` and `hash_del` reported replacing or deleting a field as an error. |
| [41238c3] | A failed call read as success (`CallReply::Unknown`), a dry run reported success on Redis before 7.2, and `reply_with_call_reply` left a RESP2 client waiting. |
| [3cb2121] | The ACL key check and unknown-user errors had no error code. |
| [2444ae1] | `RedisType` kept its registered type in a public `RefCell` while being `Sync`. |
| [5dbf237] | Converting an unlisted key type, reply type or status from `c_int` panicked. |
| [3de40d8] | `logging::setup_for_context` replaced the logger's context even when it failed, and `DefragContext::defrag_alloc` passed a zero-size layout to the allocator. |
| [86b8113] | Version parsing panicked on a number that overflows. |
| [955e3d4] | Under a floor below 7.2, a module whose commands come from `#[command]` did not load on Redis 7.0: registration required `RedisModule_SetCommandACLCategories`, which Redis 7.2 added (and which registration has used since [upstream PR 432][p432]), even for commands without ACL categories. Under the 7.0 floor, a command that failed to register was also ignored. |
| [dabad65] | The `configurations` block of `redis_module!` compiled only with all four groups and `module_args_as_configuration` listed, and a memory configuration given as a module argument with a unit, such as `8mb`, failed the load. |
| [9ea85f4] | `Context::server_info` panicked on a section name with a NUL byte (upstream still does); see the 488 row for `field`. |

## Additions

`ThreadSafeContext::try_lock` and `DetachedContext::try_lock` wrap
`RedisModule_ThreadSafeContextTryLock`, returning the same borrowed guards as
`lock` or `None` when the native lock attempt fails (#113). Guards keep the
existing call-user cleanup and unlock behavior; the detached path also checks
that unload or reload did not replace its context before acquisition. This
lets a worker check its stop flag rather than block on the Redis lock while
module unload joins it. The API exists in Redis 6.0.20 and every supported
floor. `examples/try_lock.rs` and the integration test cover contention,
repeated successful acquisition, explicit stop, unload and reload; the oldest
server CI job runs that test too.

`redis_module!` owns accepted configuration private data per load, releasing
observer captures and the final cached string getter reply after caught load
failures or successful deinit. Refused or panicking deinit retains this data;
caller-owned static storage is never destroyed. Callback access closes before
cleanup, and captured destructor panics are isolated after teardown commits.
Native configuration frames block unload until deferred completion. A
main-thread timer provides a fallback when a regular post-notification job is
unavailable or rejected; a worker in that situation conservatively blocks
unload for its load generation. Custom loaders retain the previous accepted
allocation behavior. This does not add general native-handle storage or change
public `RedisString` thread bounds. See the
[configuration lifecycle notes](docs/CONFIGURATION.md#registration-ownership-and-unload).

Timers use opaque `TimerHandle<T>` values with explicit cancellation and
metadata-only queries. **Breaking:** creation returns a result and requires
`Send + 'static` callbacks and payloads; untyped `stop_timer<T>` and
payload-borrowing `get_timer_info<T>` are replaced by `cancel_timer(&handle)`
and `timer_remaining(&handle)`. Cancellation recovers the payload at most once
and releases captured callback state. Dropping a handle leaves the timer
scheduled. Creation requires an active module bound through `redis_module!`
and fails during initialization or unload. SDK lifecycle hooks guard unload
against pending, firing or newly registered timers. See the
[migration guide](docs/MIGRATING-3.0.md#timers-use-typed-handles) and
[timer example](examples/timer.rs). Cancellation-on-drop and payload inspection
are separate work.

Repeated `CallOptionsBuilder::resp` selections now use the last requested
protocol. Previously, RESP2 did not clear earlier selections and a stale RESP3
flag overrode Auto. Other call flags and API signatures remain unchanged; the
same behavior applies when building blocking options. Native regression tests
check array/map reply kinds under both RESP2 and RESP3 clients.

Direct client blocking now validates the invocation before calling Redis.
Scripts, transactions, denied module calls and reply/timeout callback reentry
return an error rather than exposing an unusable handle or reaching native
assertions. **Breaking:** `Context::block_client()` returns a result; add `?`
before starting a worker. The reply-callback helper shares the checks, including
a fallback for Redis 6.0's older flags. See the
[migration guide](docs/MIGRATING-3.0.md#direct-client-blocking-returns-a-result)
and [blocked-client example](examples/block.rs).

Shared `RedisGILGuard<T>` storage now requires `T: Send`, closing access to
non-`Send` aliases outside Redis's lock. Owned non-`Sync` values remain
supported. String configurations can use `RedisGILGuard<Vec<u8>>` or
`Mutex<Vec<u8>>` for binary values, converting native strings only within
configuration callbacks. **Breaking:** shared native-string guards must migrate
to owned text/bytes or a separately audited retention design. `RedisString`
remains non-`Send` and non-`Sync`. The private call-user registry is thread-local;
native ownership fixtures retain their handles on the command thread. See the
[migration guide](docs/MIGRATING-3.0.md#shared-redis-guards-require-send-values).

Module initialization now constructs its NUL-terminated name buffer in constant
evaluation. The old unchecked 64-byte copy could overrun the buffer or leave
it without a terminator. **Breaking:** `name:` must be a constant `&str` without
NUL bytes; long and UTF-8 names are preserved. See the
[migration guide](docs/MIGRATING-3.0.md#module-names-are-constant-strings) and
[module-name example](examples/module_name.rs).

Configuration observers now require `Send` captures: a worker's locked
`CONFIG SET` invokes them on the worker thread. The Redis lock serializes
invocation, so `Sync` is not required, but non-`Send` captures shared outside
Redis were unsafe. **Breaking:** add `+ Send` to explicit callback trait
objects and replace shared `Rc` captures with thread-safe state. See the
[migration guide](docs/MIGRATING-3.0.md#configuration-observers-require-send)
and [observer example](examples/configuration_observers.rs).

| change | addition |
|---|---|
| [Notification writes](docs/KEYSPACE-NOTIFICATIONS.md) | A runnable Redis 7.2+ notification-to-stream example demonstrates deferred writes, owned callback data, database scopes, role/loading gates, propagation and teardown. Rejected post-notification jobs now drop their captures instead of leaking. |
| [Database event payloads](examples/server_event_payloads.rs) | `flush_info_event_handler` receives the flush phase and copied `FlushInfo` (`db: None` for all databases); `swapdb_event_handler` receives copied `SwapDbInfo` with both IDs after a successful swap. The phase-only flush handler remains supported. |
| [Scoped call users](examples/context_acl.rs) | `RuntimeContext::scoped_call_user` returns the root-exported `CallUserScope`. These replace `authenticate_user` and `ContextUserScope`, naming the scoped user for ACL-checked calls. Nested guards restore the latest still-active scope, including on unwind and out-of-order drops. `with_call_user` provides the closure form, preserving the context view. Forgotten scopes are cleaned up at view exit before worker unlock. Redis 8.8+ preserves a preexisting native override. |
| [Event and worker views](src/context/views.rs) | `EventContext` carries module-owned callback operations; `WorkerContext` is borrowed from locked guards; both expose `RuntimeContext` for shared calls, keys, databases, logging, and scoped ACL-call users. Only client `Context` has replies, client blocking, key-position discovery and verbatim replication. Blocked-client worker replies retain their explicit reply API. Module-less blocking calls return an error before Redis's assertion. |
| [Registration context](examples/registration.rs) | A borrowed `RegistrationContext` separates load-time command, type, configuration, and option setup from runtime `Context`. The macro supplies it to init hooks; runtime code cannot safely promote a context to registration. It forwards shared runtime and module event operations without client reply capability. |
| [Runtime keyspace subscriptions](examples/runtime_events.rs) | `Context::subscribe_to_keyspace_events` accepts a runtime mask and a `redis_event_handler!(handler)` adapter. It shares supported-flag filtering, borrowed keys, lossy event decoding, and panic protection with declarative handlers. Subscriptions require the module-owned `EventContext`, also exposed by load and command contexts; worker guards cannot register them. Custom native constructors require module ownership to preserve unload cleanup (see the migration guide). |
| [Optional command builders](examples/command_builder.rs) | Fluent command definitions with typed flags, named key positions, and required/optional ACL categories, alongside the legacy tuple syntax. The forms share base registration, validation, and panic-safe callbacks. |
| [Error and notification helpers](examples/events.rs) | `ErrorReply::code` and `ErrorCallReply::code` borrow the error prefix as bytes; `Context::notify_keyspace_event_bytes` sends non-UTF-8 event names and rejects NUL bytes. The existing text helper returns `Status::Err` on a NUL-containing name instead of panicking. |
| [Database selection helpers](examples/database.rs) | `Context::get_selected_db`, `select_db`, and `with_db`, which restores the original database after synchronous scoped access, including returned errors and unwinding. |
| [ce0c36d] | `ArgCursor`, a borrowing argument parser (upstream issue 7). |
| [b0135e0] | `Context::dbsize`, the shutdown server event, `RedisAlloc` trait derives (upstream PRs 397, 477, 386). |
| [a6841fb] | The `system-allocator-fallback` feature (upstream issue 89). |
| [5c4d6b3] | ACL command and channel checks, and ACL log entries. |
| [41238c3] | `Context::reply_with_call_reply`, `StringCallReply::is_simple_string`, and the call options `dry_run`, `replicate_to_replicas_only` and `replicate_to_aof_only`. |
| [4300c6a] | `Context::yield_now`, for long-running commands. |
| [bd4e81d] | `RedisValue::InsertionOrderedMap`. |
| [54cde09] | Memory accounting: `Context::used_memory_ratio`, `RedisString::malloc_size`, and `alloc::malloc_size`, `malloc_usable_size` and `redis_alloc_size`. |
| [0d63e82] | `Context::block_client_on_reply`, which blocks a client with reply and timeout callbacks on the main thread. |
| [a3b0f3d] | The `event_loop` module: file descriptor callbacks and one-shot callbacks on the main thread. |
| [dd25376] | `hash_set_by_string` and `hash_del_by_string`. |
| [eaaf9b2] | `RedisError::from_error`. |
| [c478791] | `panic::catch_panic`, for callbacks a module registers itself. |
| [762ab77] | `configuration::load_configurations`, for configurations registered outside `redis_module!`. |
| [813cfa0] | `RedisString::from_bytes`, `from_owned_ptr`, `retain_from_ptr` and `as_ptr`. |
| [3de40d8] | `InfoContext::get_raw`. |
| [09acfb4] | docs/MIGRATING-3.0.md. |
| [dad8637] | `RedisValue::Error(String)`, an error reply with an owned message, which can be an array element or a map value. |
| [a5eb60d] | `RedisValueKey::SimpleString`, a map key or set element replied as a simple string. |
| [d100d13] | `Context::get_command_keys` (`RedisModule_GetCommandKeysWithFlags`), `cluster::key_slot`, which computes a key's hash slot without the server, and `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq` and `Hash` for `ContextFlags`. |
| [dabad65] | `configuration::get_memory_default_config_value`. |

The [configuration notes](docs/CONFIGURATION.md) document registration, load
precedence, SDK argument parsing, validation, UTF-8 and binary storage, locked
reads, observers and rollback. Integration tests verify explicit Redis load
settings overriding SDK argument defaults and callbacks observing intermediate
values and rollback during a rejected multi-setting command.

The [module upgrade guide](docs/UPGRADING-MODULES.md) explains unload/load
eligibility, configuration replay, explicit per-load state initialization,
callback cleanup, platform loader behavior, and failure recovery. The
[lifecycle example](examples/lifecycle.rs) and integration tests cover a failed
load followed by recovery, immutable configuration, transient-state reset,
and native-type unload refusal even without keys. Documentation builds deny
warnings in CI after correcting existing API links.

`module_version!()` derives the module version from the caller's Cargo package
version at compile time, using `encode_module_version` and the optional
`major * 10000 + minor * 100 + patch` convention. The encoder rejects
prereleases, malformed strings, minor/patch values above 99, and `i32` overflow;
valid build metadata is ignored. Explicit integer versions remain supported.
The [hello example](examples/hello.rs) uses it in `redis_module!`.

`configuration::ParsedStringConfig<T>` validates UTF-8 string values before
publishing their raw text and parsed representation together. It integrates
with existing string configuration registration and exposes matching reads
under a Redis lock proof without cloning the parsed value.

`redis_module::milliseconds()` reads fresh Unix milliseconds through the
server module API without a context or Redis lock. It supports module workers
after API initialization at every SDK compatibility floor.

`cluster::key_for_slot` generates a tagged ASCII key for any valid hash slot,
without a server or the native canonical-name API. Its exact key names are
stable across SDK and server versions; an exhaustive compatibility fixture
pins the mapping for all 16,384 slots.

The integration harness retries confirmed TCP address-in-use failures at
`listen()` as well as `bind()`, requiring the matching socket and fatal port
diagnostics. Other startup failures propagate immediately.

## Changes that break compatibility with upstream

A module written against upstream needs changes for these; the
[migration guide](docs/MIGRATING-3.0.md) says what to write instead.

| change | breaking change |
|---|---|
| [Scoped call users](docs/MIGRATING-3.0.md#scoped-call-users) | Rename `authenticate_user` to `scoped_call_user`; the returned guard is `CallUserScope`, available at the crate root. Inner scopes now restore the outer user; `with_call_user` provides a closure form. |
| [3de40d8] | `raw` is no longer re-exported at the crate root, except `Status`, `KeyType`, `NotifyEvent`, `Version` and `ModuleOptions`. |
| [3de40d8], [ce0c36d], [813cfa0], [24cd5ff] | Functions that take raw pointers are `unsafe`: `Context::new`, `RuntimeContext::new` and `dummy`; `RedisString::new`, `from_redis_module_string`, `from_ptr` and `string_as_slice`; `decode_args`; `RedisKey::open` and `open_with_flags`; `RedisKeyWritable::open`, `open_with_flags` and `open_with_redis_string`; `InfoContext::new`; `RedisBuffer::new`; `RedisModuleIO::new`; `logging::setup_for_context` and `log_io_error`; `DefragContext::defrag_dealloc`; and in `raw` the RDB `save_*` and `load_*` helpers, `is_io_error`, `redis_log`, `call_reply_verbatim_string` and `register_info_function`. The other pointer-taking `raw` helpers and `key::verify_type` are no longer public. `Context.ctx`, `InfoContext.ctx` and `RedisString.inner` are private. |
| [ce0c36d] | `RedisString::try_as_str` borrows from the string, and `NextArg::next_str` is removed. Keyspace event handlers get the event name and key only for the callback. |
| [813cfa0] | `Context::get_current_user` returns `Option<RedisString>`; `ScanKeyCursor` callbacks get the value as `Option<&RedisString>`; `KeysCursor::scan` takes `&mut F`. |
| [0196506] | `RedisKeyWritable::delete` and `unlink` return `Result<bool, RedisError>`. |
| [dd25376] | `hash_set` and `hash_del` return `Result<usize, RedisError>`. |
| [72eaf1a] | `RedisKeyWritable::get_value` and `as_string_dma` take `&mut self`, so the reference they return borrows the key. Code that keeps that reference in the same struct as the key, the pattern upstream PR 277 kept the unbound lifetime for, no longer compiles. |
| [b29cd45], [41238c3] | `Context::call` returns `BulkString` for bulk replies, and an error for a call that fails without the `E` flag. |
| [bd4e81d], [dad8637] | `RedisValue` has new variants, `InsertionOrderedMap` and `Error`. |
| [a5eb60d] | `RedisValueKey` has a new variant, `SimpleString`. |
| [eaaf9b2] | `RedisError` converts with `From` only from a list of standard library errors. |
| [c3432c5] | The `register_*_configuration` functions return `Result<(), RedisError>`. |
| [24cd5ff] | `RedisGILGuard<T>` is `Send` only when `T` is; `ConfigurationContext` is neither `Send` nor `Sync`, and `ConfigurationValue` no longer has `Send` as a supertrait; `redis_command!` and `redis_event_handler!` take `&Context`. |
| [2444ae1] | `RedisType::raw_type` is a method; `create_data_type` takes `&Context`; invalid type names fail to compile. |
| [1c57c65] | Command flags in `redis_module!` must be a constant expression. |
| [5dbf237] | `KeyType` and `ReplyType` are `#[non_exhaustive]` with new variants; the raw enums are `#[repr(i32)]` without `FromPrimitive`. |
| [3de40d8] | `raw::export_shared_api` and `Context::export_shared_api` return `Status`. |
| [c1dc312] | `bindgen-runtime` is no longer a default feature. |
| [0e1d8f2] | The opaque handle types in `raw` no longer derive `Copy` and `Clone`. |

These compile unchanged but behave differently:

| change | behavior change |
|---|---|
| [752b2a7], [dd25376] | Keys of the wrong type reply `WRONGTYPE` from `get_value`, the hash getters, `hash_set` and `hash_del`. |
| [3cb2121] | A denied ACL key check replies `NOPERM ...`; an unknown or disabled user replies `ERR the user does not exist or is disabled`. |
| [b29cd45], [41238c3] | `Context::call` returns `SimpleString` only for simple string replies, so a forwarded bulk reply reaches the client as a bulk string; a call that fails without the `E` flag returns an error. |
| [7333dba] | Simple string and error replies replace CR, LF and NUL with spaces. |
| [c478791] | A command that panics gets an error reply instead of aborting the server; other callbacks log the panic. |
| [24cd5ff] | Locking a `RedisGILGuard` that is already locked panics. |
| [5dbf237] | `Status::from(c_int)` maps every value other than `REDISMODULE_OK` to `Err`. |
| [332c68a] | `RedisKey::to_raw_parts` leaves the key open for the caller. |
| [a83e6d3] | Dropping a stream iterator stops it instead of deleting the current entry. |
| [c3432c5], [762ab77] | `redis_module!` fails the load when a configuration registration fails or `LoadConfigs` rejects a value. |
| [5bf61ff] | Numeric defaults must be within the declared inclusive bounds, including module arguments after memory-unit conversion. Boolean module arguments accept only case-insensitive `yes` or `no`; invalid values fail the load. Configuration names are looked up only in the name positions of name/value pairs, so a value matching another configuration's name does not override it. |
| [efad730] | `redis_module!` reuses an existing custom ACL category instead of failing the load. Redis keeps categories after module unload, so a module can reload with its category and existing user grants intact. Only `EBUSY` is accepted; invalid names and other failures still fail the load. |
| [86b8113] | The crash report's `backtrace` field is `std::backtrace::Backtrace` output. |
| [a5eb60d] | A call reply's simple-string map key or set element converts to `RedisValueKey::SimpleString` instead of `String`, so a forwarded reply keeps it a simple string. |
| [9258efb] | An error in a call reply, also inside an array or map, converts to `RedisValue::Error` instead of `StringBuffer`, so a forwarded reply keeps it an error. |
| [955e3d4] | Under the 7.0 floor, a `#[command]` command that fails to register fails the module load, as under the other floors. |

## Build, tooling and documentation

- [Module testing patterns](docs/TESTING-MODULES.md) reuse existing event
  modules for a companion-input recipe and provide an independent parser/fuzz
  fixture. The pinned libFuzzer setup checks both system allocation and explicit
  SDK test fallback, plus refusal without fallback. Normal SDK dependencies and
  production allocator guards are unchanged.

- A [native module shipping guide](docs/SHIPPING-MODULES.md) records build
  inputs, platform limits, archive checksums and a disposable-server load check
  of the extracted artifact. It links the module upgrade/recovery guide.

- Strict documentation checks select every SDK workspace package, including
  public and internal macro crates, at each CI API floor. Public macro docs use
  qualified SDK type text and accessible links without private implementation
  references or warning suppressions; generated code is unchanged.

- The integration harness uses Redis client 1.2.2 as a development dependency,
  with optional features disabled. An exact pin preserves the Rust 1.87 minimum:
  client releases from 1.2.3 require Rust 1.88. Response checks preserve binary
  versus simple strings, nested server errors and pipeline alignment; partial or
  malformed replies close their connection. RESP2/RESP3 and oldest-runtime
  controls run against native Redis 6.0 alongside the existing server matrix.
  SDK runtime dependencies and public APIs are unchanged.

- Internal API-wrapper and mixed command-list parsers use Syn 2, preserving
  existing declarations, generated registration paths and compatibility-floor
  signatures. The public macro crate uses the SDK-owned parser described above
  for its existing metadata grammar.

- Edition 2024, with `rust-version = "1.87"` declared and tested in CI ([cfebad5], [2daa0ea]).
- CI pins checksum-verified Redis 7.2, 7.4 and 8.0 through 8.10 releases and Valkey 8.1/9.1, caches server binaries per platform, concrete runner image and build inputs, validates the selected product/version, and uploads integration directories and server build logs on failure. Valkey uses the default API floor.
- CI runs on pushes to master and builds every API floor; the initial server matrix covered Redis 7.2, 7.4 and 8.0 ([e7d39ce], [2daa0ea]). The test matrix builds the default way, with the committed bindings, on the toolchain `rust-toolchain.toml` pins, and one job checks the committed bindings against bindgen ([848824a]). A job runs cargo-deny, with `Unicode-3.0` allowed ([1353e52]).
- The Docker build, which ran the `Makefile` and `sbin/` that [upstream PR 389][p389] deleted, and the `deps/readies` submodule are removed ([9b0f605]). The release workflows run only in the upstream repository ([cb78fec]). Dependabot no longer ignores bindgen 0.56 and also updates the workflow actions ([686f84c]). The build and test scripts use `--workspace` ([9e5ff10]). The crates' `repository` field names this repository ([6d21760]).
- The migration guide shows the `system-allocator-fallback` dev-dependency in the git form ([8ddccf1]), and the README says that README.upstream.md is upstream's README unchanged ([e4c5d1c]). Notes and tests corrected after a review ([ee02544]).
- Committed bindings, with bindgen optional and at 0.73 ([c1dc312], [0e1d8f2]).
- Six fewer dependencies: `nix`, `strum_macros`, `regex`, `backtrace`, `enum-primitive-derive`, `num-traits` ([86b8113], [5dbf237]).
- Version 3.0.0 for `redis-module`, `redis-module-macros` and `redis-module-macros-internals`, and 0.1.2 for `redis-module-common` ([af6611a]).
- A regression test for the string ownership leaks ([1cf82af]), and a test harness that prints the server log when a test fails or panics ([4e8efc7]).
- The [integration harness](tests/utils.rs) accepts module arguments, returns
  startup diagnostics, polls with a deadline, starts replicas, and restarts
  after saving while preserving the data directory and launch settings. It
  uses OS-selected ports, isolated directories, bounded socket probes before
  protocol traffic, and child PID verification. Fresh launches retry confirmed
  port collisions up to five times with separate directories and logs;
  restarts keep their original port. TEST_REDIS_SERVER_BIN selects the Redis
  executable.
- Logging docs that say how NUL bytes are written ([4e8efc7]), and the changelog for 3.0.0 ([1eb962a]).

[a808377]: https://github.com/RedisLabsModules/redismodule-rs/commit/a808377

[0196506]: https://github.com/joshrotenberg/redismodule-rs/commit/0196506
[09acfb4]: https://github.com/joshrotenberg/redismodule-rs/commit/09acfb4
[0d63e82]: https://github.com/joshrotenberg/redismodule-rs/commit/0d63e82
[0e1d8f2]: https://github.com/joshrotenberg/redismodule-rs/commit/0e1d8f2
[113216b]: https://github.com/joshrotenberg/redismodule-rs/commit/113216b
[1353e52]: https://github.com/joshrotenberg/redismodule-rs/commit/1353e52
[1c57c65]: https://github.com/joshrotenberg/redismodule-rs/commit/1c57c65
[1cf82af]: https://github.com/joshrotenberg/redismodule-rs/commit/1cf82af
[1d7aab1]: https://github.com/joshrotenberg/redismodule-rs/commit/1d7aab1
[1eb962a]: https://github.com/joshrotenberg/redismodule-rs/commit/1eb962a
[2444ae1]: https://github.com/joshrotenberg/redismodule-rs/commit/2444ae1
[24cd5ff]: https://github.com/joshrotenberg/redismodule-rs/commit/24cd5ff
[2daa0ea]: https://github.com/joshrotenberg/redismodule-rs/commit/2daa0ea
[332c68a]: https://github.com/joshrotenberg/redismodule-rs/commit/332c68a
[3700a31]: https://github.com/joshrotenberg/redismodule-rs/commit/3700a31
[3cb2121]: https://github.com/joshrotenberg/redismodule-rs/commit/3cb2121
[3de40d8]: https://github.com/joshrotenberg/redismodule-rs/commit/3de40d8
[41238c3]: https://github.com/joshrotenberg/redismodule-rs/commit/41238c3
[4300c6a]: https://github.com/joshrotenberg/redismodule-rs/commit/4300c6a
[4e8efc7]: https://github.com/joshrotenberg/redismodule-rs/commit/4e8efc7
[54cde09]: https://github.com/joshrotenberg/redismodule-rs/commit/54cde09
[5bf61ff]: https://github.com/joshrotenberg/redismodule-rs/commit/5bf61ff
[5c4d6b3]: https://github.com/joshrotenberg/redismodule-rs/commit/5c4d6b3
[5dbf237]: https://github.com/joshrotenberg/redismodule-rs/commit/5dbf237
[686f84c]: https://github.com/joshrotenberg/redismodule-rs/commit/686f84c
[68b33ab]: https://github.com/joshrotenberg/redismodule-rs/commit/68b33ab
[6d21760]: https://github.com/joshrotenberg/redismodule-rs/commit/6d21760
[72eaf1a]: https://github.com/joshrotenberg/redismodule-rs/commit/72eaf1a
[7333dba]: https://github.com/joshrotenberg/redismodule-rs/commit/7333dba
[752b2a7]: https://github.com/joshrotenberg/redismodule-rs/commit/752b2a7
[762ab77]: https://github.com/joshrotenberg/redismodule-rs/commit/762ab77
[813cfa0]: https://github.com/joshrotenberg/redismodule-rs/commit/813cfa0
[848824a]: https://github.com/joshrotenberg/redismodule-rs/commit/848824a
[86b8113]: https://github.com/joshrotenberg/redismodule-rs/commit/86b8113
[8ddccf1]: https://github.com/joshrotenberg/redismodule-rs/commit/8ddccf1
[9258efb]: https://github.com/joshrotenberg/redismodule-rs/commit/9258efb
[955e3d4]: https://github.com/joshrotenberg/redismodule-rs/commit/955e3d4
[9b0f605]: https://github.com/joshrotenberg/redismodule-rs/commit/9b0f605
[9e5ff10]: https://github.com/joshrotenberg/redismodule-rs/commit/9e5ff10
[9ea85f4]: https://github.com/joshrotenberg/redismodule-rs/commit/9ea85f4
[a3b0f3d]: https://github.com/joshrotenberg/redismodule-rs/commit/a3b0f3d
[a5eb60d]: https://github.com/joshrotenberg/redismodule-rs/commit/a5eb60d
[a6841fb]: https://github.com/joshrotenberg/redismodule-rs/commit/a6841fb
[a83e6d3]: https://github.com/joshrotenberg/redismodule-rs/commit/a83e6d3
[af6611a]: https://github.com/joshrotenberg/redismodule-rs/commit/af6611a
[b0135e0]: https://github.com/joshrotenberg/redismodule-rs/commit/b0135e0
[b29cd45]: https://github.com/joshrotenberg/redismodule-rs/commit/b29cd45
[b6371e9]: https://github.com/joshrotenberg/redismodule-rs/commit/b6371e9
[bd4e81d]: https://github.com/joshrotenberg/redismodule-rs/commit/bd4e81d
[c1dc312]: https://github.com/joshrotenberg/redismodule-rs/commit/c1dc312
[c3432c5]: https://github.com/joshrotenberg/redismodule-rs/commit/c3432c5
[c478791]: https://github.com/joshrotenberg/redismodule-rs/commit/c478791
[cb78fec]: https://github.com/joshrotenberg/redismodule-rs/commit/cb78fec
[ce0c36d]: https://github.com/joshrotenberg/redismodule-rs/commit/ce0c36d
[cfebad5]: https://github.com/joshrotenberg/redismodule-rs/commit/cfebad5
[d100d13]: https://github.com/joshrotenberg/redismodule-rs/commit/d100d13
[d2f790a]: https://github.com/joshrotenberg/redismodule-rs/commit/d2f790a
[d742fec]: https://github.com/joshrotenberg/redismodule-rs/commit/d742fec
[dabad65]: https://github.com/joshrotenberg/redismodule-rs/commit/dabad65
[dad8637]: https://github.com/joshrotenberg/redismodule-rs/commit/dad8637
[dd25376]: https://github.com/joshrotenberg/redismodule-rs/commit/dd25376
[e074cdb]: https://github.com/joshrotenberg/redismodule-rs/commit/e074cdb
[e4c5d1c]: https://github.com/joshrotenberg/redismodule-rs/commit/e4c5d1c
[e7d39ce]: https://github.com/joshrotenberg/redismodule-rs/commit/e7d39ce
[e81393d]: https://github.com/joshrotenberg/redismodule-rs/commit/e81393d
[eaaf9b2]: https://github.com/joshrotenberg/redismodule-rs/commit/eaaf9b2
[ee02544]: https://github.com/joshrotenberg/redismodule-rs/commit/ee02544
[efad730]: https://github.com/joshrotenberg/redismodule-rs/commit/efad730
[i7]: https://github.com/RedisLabsModules/redismodule-rs/issues/7
[i89]: https://github.com/RedisLabsModules/redismodule-rs/issues/89
[i134]: https://github.com/RedisLabsModules/redismodule-rs/issues/134
[i150]: https://github.com/RedisLabsModules/redismodule-rs/issues/150
[i205]: https://github.com/RedisLabsModules/redismodule-rs/issues/205
[i234]: https://github.com/RedisLabsModules/redismodule-rs/issues/234
[i246]: https://github.com/RedisLabsModules/redismodule-rs/issues/246
[i251]: https://github.com/RedisLabsModules/redismodule-rs/issues/251
[i287]: https://github.com/RedisLabsModules/redismodule-rs/issues/287
[i332]: https://github.com/RedisLabsModules/redismodule-rs/issues/332
[i351]: https://github.com/RedisLabsModules/redismodule-rs/issues/351
[i358]: https://github.com/RedisLabsModules/redismodule-rs/issues/358
[i365]: https://github.com/RedisLabsModules/redismodule-rs/issues/365
[i378]: https://github.com/RedisLabsModules/redismodule-rs/issues/378
[i404]: https://github.com/RedisLabsModules/redismodule-rs/issues/404
[i416]: https://github.com/RedisLabsModules/redismodule-rs/issues/416
[i471]: https://github.com/RedisLabsModules/redismodule-rs/issues/471
[i473]: https://github.com/RedisLabsModules/redismodule-rs/issues/473
[p85]: https://github.com/RedisLabsModules/redismodule-rs/pull/85
[p93]: https://github.com/RedisLabsModules/redismodule-rs/pull/93
[p247]: https://github.com/RedisLabsModules/redismodule-rs/pull/247
[p269]: https://github.com/RedisLabsModules/redismodule-rs/pull/269
[p277]: https://github.com/RedisLabsModules/redismodule-rs/pull/277
[p348]: https://github.com/RedisLabsModules/redismodule-rs/pull/348
[p360]: https://github.com/RedisLabsModules/redismodule-rs/pull/360
[p361]: https://github.com/RedisLabsModules/redismodule-rs/pull/361
[p368]: https://github.com/RedisLabsModules/redismodule-rs/pull/368
[p375]: https://github.com/RedisLabsModules/redismodule-rs/pull/375
[p386]: https://github.com/RedisLabsModules/redismodule-rs/pull/386
[p389]: https://github.com/RedisLabsModules/redismodule-rs/pull/389
[p397]: https://github.com/RedisLabsModules/redismodule-rs/pull/397
[p417]: https://github.com/RedisLabsModules/redismodule-rs/pull/417
[p420]: https://github.com/RedisLabsModules/redismodule-rs/pull/420
[p427]: https://github.com/RedisLabsModules/redismodule-rs/pull/427
[p432]: https://github.com/RedisLabsModules/redismodule-rs/pull/432
[p467]: https://github.com/RedisLabsModules/redismodule-rs/pull/467
[p476]: https://github.com/RedisLabsModules/redismodule-rs/pull/476
[p477]: https://github.com/RedisLabsModules/redismodule-rs/pull/477
[p483]: https://github.com/RedisLabsModules/redismodule-rs/pull/483
[p486]: https://github.com/RedisLabsModules/redismodule-rs/pull/486
[p487]: https://github.com/RedisLabsModules/redismodule-rs/pull/487
[p488]: https://github.com/RedisLabsModules/redismodule-rs/pull/488
[p489]: https://github.com/RedisLabsModules/redismodule-rs/pull/489
[p490]: https://redirect.github.com/RedisLabsModules/redismodule-rs/pull/490
