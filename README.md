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

## Using it

3.0.0 is not published to crates.io. Depend on it from git, pinned to a commit:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
redis-module = { git = "https://github.com/joshrotenberg/redismodule-rs", rev = "<commit>" }
# Only for the attribute and derive macros: #[command], the event handler, INFO
# and defrag attributes, and #[derive(RedisValue)] and #[derive(InfoSection)].
redis-module-macros = { git = "https://github.com/joshrotenberg/redismodule-rs", rev = "<commit>" }
```

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

### Module initialization

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
| `min-redis-compatibility-version-6-0` (default), `-6-2`, `-7-0`, `-7-2`, `-7-4`, `-8-0` | The oldest Redis the module supports. Choose one. Module APIs newer than it are looked up when called, and fail with an error naming the API on an older server. |
| `bindgen-runtime`, `bindgen-static` | Generate the bindings with bindgen instead of using the committed ones. Both need libclang: `bindgen-runtime` loads it at build time, `bindgen-static` links it. |
| `system-allocator-fallback` | Lets a module's `cargo test` allocate outside a Redis server. Enable it in `[dev-dependencies]` only. |

## Bindings

The crate binds `src/include/redismodule.h`. The bindings are generated with
[bindgen](https://github.com/rust-lang/rust-bindgen) and committed as
`src/redisraw/bindings.rs`, so a default build needs no libclang. After
changing the header, build with the `bindgen-runtime` feature and copy the
generated `OUT_DIR/bindings.rs` over `src/redisraw/bindings.rs`; CI fails when
the two differ.

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
Retain the returned name if it identifies stored data; its exact bytes are not
a cross-version contract. The first valid call builds a shared 48 KiB lookup
table; subsequent calls allocate only the returned key. The `keys_pos` example
provides `key_for_slot <slot>` to try it from a client.

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
