# redis-module

A Rust toolkit for writing [Redis modules](https://redis.io/docs/latest/develop/reference/modules/):
safe wrappers over the Redis module API, the `redis_module!` macro that
registers a module's commands, data types and event handlers, and the raw
bindings for the rest.

This repository is a fork of
[RedisLabsModules/redismodule-rs](https://github.com/RedisLabsModules/redismodule-rs).
It fixes soundness and correctness bugs, wraps more of the module API, and
makes the breaking changes those needed, as version 3.0.0.
[CHANGES-FROM-UPSTREAM.md](CHANGES-FROM-UPSTREAM.md) lists every change and the
upstream issues and pull requests it relates to. The upstream README is kept,
unchanged, as [README.upstream.md](README.upstream.md).

For branches, the review and merge workflow and validation, see
[CONTRIBUTING.md](CONTRIBUTING.md). The open issues are the work list.

## Using it

3.0.0 is not published to crates.io. Depend on it from git, pinned to a
pre-release tag:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
redis-module = { git = "https://github.com/joshrotenberg/redismodule-rs", tag = "v3.0.0-alpha.1" }
# Only for the attribute and derive macros: #[command], the server event handler,
# INFO and defrag attributes, and #[derive(RedisValue)] and #[derive(InfoSection)].
redis-module-macros = { git = "https://github.com/joshrotenberg/redismodule-rs", tag = "v3.0.0-alpha.1" }
```

The tag is a pre-release; the crates inside it are version `3.0.0`. To build
against a later commit, use `rev = "<commit>"` instead of `tag`, the same in
both lines.

A module that used 2.x: [docs/MIGRATING-3.0.md](docs/MIGRATING-3.0.md) lists
what to change. [CHANGELOG.md](CHANGELOG.md) lists what changed in each version.

## Requirements

- Rust 1.87 or later. The repository pins 1.94.0 in `rust-toolchain.toml`.
- A C compiler: the build compiles a small C stub with `cc`.
- A 64-bit target for the default build, which uses the bindings committed in
  `src/redisraw/bindings.rs`. The `bindgen-runtime` and `bindgen-static`
  features generate them instead, for any target, and need libclang.
- Redis 6.0 or later, depending on the feature floor below. CI builds every
  floor and runs the tests against pinned Redis 7.2, 7.4 and 8.0 through
  8.10 releases, plus Valkey 8.1 and 9.1 at the default API floor.

## A first module

From a clone of this repository:

```bash
cargo build --example hello
redis-server --loadmodule ./target/debug/examples/libhello.dylib   # libhello.so on Linux
redis-cli hello.mul 31 11
```

`hello.mul` replies with its arguments and their product. Its source,
[examples/hello.rs](examples/hello.rs):

```rust
use redis_module::{Context, RedisError, RedisResult, RedisString, redis_module};

fn hello_mul(_: &Context, args: Vec<RedisString>) -> RedisResult {
    if args.len() < 2 {
        return Err(RedisError::WrongArity);
    }

    let nums = args
        .into_iter()
        .skip(1)
        .map(|s| s.parse_integer())
        .collect::<Result<Vec<i64>, RedisError>>()?;

    let product = nums.iter().product();

    let mut response = nums;
    response.push(product);

    Ok(response.into())
}

redis_module! {
    name: "hello",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    commands: [
        ["hello.mul", hello_mul, "", 0, 0, 0, ""],
    ],
}
```

The [examples](examples) directory has a module for most of the API: keys,
hashes, lists, streams, native data types, calls, blocked clients, threads,
timers, server and keyspace events, configuration, ACL checks, the event loop,
and INFO sections. The API follows the module API's concepts, so the
[module API reference](https://redis.io/docs/latest/develop/reference/modules/modules-api-ref/)
applies to it.

For writes triggered by keyspace notifications, see
[Writing from keyspace notifications](docs/KEYSPACE-NOTIFICATIONS.md) and the
[notification-to-stream example](examples/keyspace_to_stream.rs). It uses
Redis 7.2+ post-notification jobs, explicit replication, and role/loading gates.

For distributing a native module binary, see
[Shipping a native module](docs/SHIPPING-MODULES.md) for build inputs, platform
compatibility, checksums and an extracted-artifact load check.

For companion-module input checks and pure parser fuzzing outside Redis, see
[Testing module inputs and pure parsers](docs/TESTING-MODULES.md).

For shared state in INFO callbacks and the crash-report boundary, see
[INFO handler state access](docs/INFO-HANDLERS.md) and its atomic counter example.

For which module API functions have safe wrappers, which Redis version added
each, and the missing ones ranked by evidence of need, see
[Module API coverage](docs/API-COVERAGE.md).

## Features

### Optional command builders

Legacy command tuples remain supported. Builder expressions may appear in the
same `commands` list, with no flags, keys, or ACL categories by default:

Import `redis_command` from `redis_module`. Inside `redis_module!`:

```rust
commands: [
    ["database.select", select, "deny-script", 0, 0, 0, ""],
    redis_command!("database.selected", selected).read_only(),
    redis_command!("database.get_in", get_in)
        .read_only().deny_script().key(2),
],
```

Other settings, using `CommandFlags` and `KeyRange` from `redis_module`:

```rust
redis_command!("example.keys", handler)
    .flags(CommandFlags::READ_ONLY | CommandFlags::FAST)
    .keys(KeyRange::new(1).last(-1).step(2))
    .acl_categories("read")
    .optional_acl_categories("example")
```

`redis_command!(name, handler)` generates the same panic-safe C callback as a
legacy tuple and returns a `CommandBuilder`. Flags accumulate; key and ACL
settings replace their previous values. Keys are numbered from 1, with the
command at position 0. A negative last position counts from the end. A
single key uses `.key(position)`; a range uses named `.last(...)` and `.step(...)`
settings. Dynamic discovery uses `CommandFlags::GETKEYS_API` and the existing
`is_keys_position_request` / `key_at_pos` APIs.

Required ACL categories fail the load if unsupported or rejected. Optional
categories are tried alongside the required set; on failure, registration warns
and falls back to the required set. Optional categories are skipped on servers
without command ACL support. Builders reject empty or NUL-containing names,
unknown flag bits, invalid key ranges, and explicit empty optional categories
with load diagnostics. Legacy tuples retain their
native key-position semantics. Handlers still check
their own argument counts. The `#[command]` attribute remains available for
arity, detailed key specifications, summaries, and other rich metadata.

See [examples/database.rs](examples/database.rs) for mixed syntax and
[examples/command_builder.rs](examples/command_builder.rs) for more settings.

### Module version from Cargo

Use `version: redis_module::module_version!(),` in `redis_module!` to derive
the integer shown by `MODULE LIST` from the module crate's `CARGO_PKG_VERSION`.
For example, `1.23.45` becomes `12345` (`major * 10000 + minor * 100 + patch`).
The macro always validates at compile time: minor and patch must be below
100, the result must fit `i32`, and prereleases are rejected because this
encoding cannot distinguish them from releases. Valid SemVer build metadata
is ignored. `0.0.0` encodes as zero.

`encode_module_version("1.23.45")` provides the same conversion as a `const fn`;
invalid input panics, or fails to compile when used in a const expression.
Existing explicit `version:` values remain supported. This optional convention
does not change Redis server versions or native data-type encoding versions.
See [examples/hello.rs](examples/hello.rs).

### Module initialization

`name:` is a constant `&str` expression without NUL bytes. String literals,
named constants, `concat!` and `env!` work. The macro builds its terminated
C string at compile time, before the Redis allocator is available, and
supports names longer than 63 bytes without truncation. See
[examples/module_name.rs](examples/module_name.rs).

Use `RegistrationContext` in `init` hooks that register commands, native data
types, or configurations, or set module options. `redis_module!` supplies a
borrowed context for its load invocation. Runtime helpers remain available,
and ordinary command handlers continue taking `&Context`. A runtime context
cannot acquire the registration capability through safe code.

See [examples/registration.rs](examples/registration.rs) for manual registration
alongside legacy and builder command declarations.

### Runtime keyspace subscriptions

Keyspace subscriptions can also use a mask chosen from module arguments or a
command:

```rust,ignore
ctx.subscribe_to_keyspace_events(mask, redis_module::redis_event_handler!(on_event))?;
```

Use a module-owned context from initialization or a command. Ordinary
thread-safe context guards are rejected because Redis cannot associate their
subscriptions with a module for unload cleanup. The existing `event_handlers`
list still works. Each subscription is additive
and lasts until the module unloads. See
[examples/runtime_events.rs](examples/runtime_events.rs) for load-time and
command-time subscriptions.

### Feature flags

| feature | effect |
|---|---|
| `min-redis-compatibility-version-6-0` (default), `-6-2`, `-7-0`, `-7-2`, `-7-4`, `-8-0` | The minimum Redis API assumption for SDK wrappers. Choose exactly one; it does not probe the server or guarantee that every module operation is supported. See the workspace guide below. |
| `bindgen-runtime`, `bindgen-static` | Generate the bindings with bindgen instead of using the committed ones. Both need libclang: `bindgen-runtime` loads it at build time, `bindgen-static` links it. |
| `system-allocator-fallback` | Lets a module's `cargo test` allocate outside a Redis server. Enable it in `[dev-dependencies]` only. |

For separate or shared floor selections across several module packages, see
[Redis API floors in module workspaces](docs/COMPATIBILITY-FLOORS.md), including
compile controls for conflicting features and accidental dependency defaults.

## Bindings

The crate binds `src/include/redismodule.h`. The bindings are generated with
[bindgen](https://github.com/rust-lang/rust-bindgen) and committed as
`src/redisraw/bindings.rs`, so a default build needs no libclang. After
changing the header, build with the `bindgen-runtime` feature and copy the
generated `OUT_DIR/bindings.rs` over `src/redisraw/bindings.rs`; CI fails when
the two differ.

## Module configurations

The [configuration notes](docs/CONFIGURATION.md) explain registration, load
precedence, module arguments, validation, observers and rollback across all
configuration types.

For binary string values, use `RedisGILGuard<Vec<u8>>` or `Mutex<Vec<u8>>`.
Both preserve NUL and invalid UTF-8 without retaining a native string owner.
Shared Redis guards require `Send` values; `RedisString` remains thread-bound.

### Validated string configurations

`configuration::ParsedStringConfig<T>` stores accepted UTF-8 text together
with a parsed value. The parser can reject a load value or `CONFIG SET` before
either value changes. `CONFIG GET` preserves the accepted text exactly.

```rust
use std::sync::LazyLock;
use redis_module::{RedisError, configuration::ParsedStringConfig};

fn parse_limit(raw: &str) -> Result<u16, RedisError> {
    raw.parse().map_err(|_| RedisError::Str("expected a u16"))
}
static LIMIT: LazyLock<ParsedStringConfig<u16>> =
    LazyLock::new(|| ParsedStringConfig::new("8", parse_limit).unwrap());
```

Register `&*LIMIT` in the existing `string` configuration group with a default
and flags. Registration still uses the Redis 7.0+ module configuration API.
The constructor validates its initial text; Redis then validates and applies
the registration default or configured load value. The existing
`module_args_as_configuration: true` option validates module arguments too.

Read under a context or locked worker with
`LIMIT.with_value(ctx, |raw, parsed| (raw.to_owned(), *parsed))`.
The closure receives a matching pair and cannot return borrowed references.
Do not reenter the same configuration while the closure borrows it. `T` must
be `Send`, but need not be `Clone` or `Sync`. For validation without a parsed
value, return `Result<(), RedisError>` from the parser.

Parsers should have no side effects: loading and rollback can call them more
than once. Use the existing `on_changed` callback to observe accepted values;
it cannot veto storage. See [parsed_configuration.rs](examples/parsed_configuration.rs)
for registration, typed reads and observer usage. Cross-field validation and
transactional side effects require a custom configuration implementation.

## Blocking clients

For worker replies, `Context::block_client()` returns a result. Check it before
starting work, send the reply through the blocked client's `ThreadSafeContext`,
and return `RedisValue::NoReply`. Scripts, transactions and other denied
invocations fail before native blocking. See [block.rs](examples/block.rs).

## Timers

`ctx.create_timer(delay, callback, data)?` returns a typed `TimerHandle<T>`.
Callbacks and payloads require `Send + 'static`. Keep the handle to call
`ctx.cancel_timer(&handle)?` or query `ctx.timer_remaining(&handle)?`;
cancellation returns the payload at most once and queries expose only metadata.
Dropping a handle leaves the timer running. Creation requires an active module
bound through `redis_module!` and fails during initialization or unload. The
SDK protects pending and firing timers during unload. See
[timer.rs](examples/timer.rs) and the
[timer migration notes](docs/MIGRATING-3.0.md#timers-use-typed-handles).

## Server clock

`redis_module::milliseconds()` reads the current Unix time in milliseconds
through Redis's module API. It needs no context or Redis lock and can run on a
module worker after API initialization. This is fresh wall time; clock
adjustments can move it backward. Use `std::time::Instant` for elapsed time.
It is available at every SDK compatibility floor and panics with the native
API name if used before initialization.

## Cluster key names

`cluster::key_slot` computes a key's slot without calling Redis.
`cluster::key_for_slot` goes the other way and returns an ASCII key with a
nonempty hash tag, or `None` for a slot outside `0..cluster::SLOTS`:

```rust
use redis_module::cluster::{key_for_slot, key_slot};
let key = key_for_slot(1234).unwrap();
let events_key = format!("module:{key}:events");
assert_eq!(key_slot(events_key.as_bytes()), 1234);
```

A prefix must not contain `{`; a suffix can contain any bytes. This chooses a
name, without discovering slot ownership or routing commands. It works on all
server versions and does not return Redis's native canonical slot name.
For each valid slot, the exact returned key bytes are stable across SDK
versions and server versions. Modules can recompute stored key names after an
SDK upgrade. An exhaustive compatibility fixture pins all 16,384 names.
The first valid call builds a shared 48 KiB lookup table; subsequent calls
allocate only the returned key. The `keys_pos` example
provides `key_for_slot <slot>` to try it from a client.

## Upgrading a loaded module

The [module upgrade guide](docs/UPGRADING-MODULES.md) covers unload eligibility,
configuration replay, transient state, callback cleanup, platform loader
behavior, and recovery after a failed replacement load. The
[lifecycle example](examples/lifecycle.rs) demonstrates reloads with immutable
and mutable configuration. Modules registering native data types require a
server restart rather than `MODULE UNLOAD`.

## Building and testing this repository

```bash
cargo build --examples
cargo test
```

The integration tests start `redis-server` processes, so it needs to be on the
`PATH`; they use OS-selected loopback ports and isolated directories.
`TEST_REDIS_SERVER_BIN` selects another binary. To test another floor:

```bash
cargo test --no-default-features --features min-redis-compatibility-version-7-2
```

`cargo test --test integration` alone does not rebuild the example modules it
loads; run `cargo build --examples` first, or plain `cargo test`.

CI uses the releases and SHA-256 checksums in [.github/servers.json](.github/servers.json).
The [build helper](.github/scripts/build-server.py) verifies downloaded archives,
builds with two compiler jobs and the system allocator, and checks the product
and exact version of both fresh and cached binaries. The workflow passes that
binary through `TEST_REDIS_SERVER_BIN`, without installing into `/usr/local`.
To reproduce a server leg locally:

```bash
python3 .github/scripts/build-server.py redis-8.10.2 /tmp/sdk-server
TEST_REDIS_SERVER_BIN=/tmp/sdk-server/redis-server cargo test
```

CI also runs a focused direct-blocking regression on Redis 6.0.20, which lacks
the newer context flag for denied blocking. This job sets
`TEST_REDIS_SERVER_LEGACY_OPTIONS=1` to omit command-policy settings that old
servers do not recognize. It runs only
`test_direct_client_blocking_rejects_denied_invocations`; it does not establish
that the full integration suite works with Redis 6.0.

Use a separate destination for each server version. When updating a release,
copy its SHA-256 from [redis/redis-hashes](https://github.com/redis/redis-hashes)
or [valkey-io/valkey-hashes](https://github.com/valkey-io/valkey-hashes), update the
matrix, and run the helper. Binary caches include the runner platform,
architecture, concrete image version, release, manifest and build helper. Failed matrix jobs upload
server build output and the integration instance directories for seven days;
these contain the local test servers' logs and persistence files.

API documentation: `cargo doc --open`. The pages on docs.rs are for the upstream
crate's published 2.x versions.

## License

BSD-3-Clause, as upstream. See [LICENSE](LICENSE).
