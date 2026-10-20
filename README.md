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
upstream issues and pull requests it relates to. The upstream README is kept as
[README.upstream.md](README.upstream.md).

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
  floor and runs the tests against Redis 7.2, 7.4 and 8.0.

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

## Features

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

## Building and testing this repository

```bash
cargo build --examples
cargo test
```

The integration tests start `redis-server` processes, so it needs to be on the
`PATH`; they use ports from 6479. To test another floor:

```bash
cargo test --no-default-features --features min-redis-compatibility-version-7-2
```

`cargo test --test integration` alone does not rebuild the example modules it
loads; run `cargo build --examples` first, or plain `cargo test`.

API documentation: `cargo doc --open`. The pages on docs.rs are for the upstream
crate's published 2.x versions.

## License

BSD-3-Clause, as upstream. See [LICENSE](LICENSE).
