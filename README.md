# redismodule-rs

Rust bindings and higher-level APIs for writing Redis modules.

**Development fork of [RedisLabsModules/redismodule-rs](https://github.com/RedisLabsModules/redismodule-rs).**
Current changes are local and unpublished. Package names/versions still match
the inherited graph; [upstream registry documentation](https://docs.rs/redis-module)
does not describe these fork changes. See [development instructions](docs/DEVELOPMENT.md)
for the workspace, dependencies and checks, and [status](STATUS.md) for current work.

The goal is a Rust-friendly SDK for external authors, retaining useful existing
conveniences while improving correctness, ownership and testing. Existing
BSD-3-Clause notices and contributor history are retained; see [LICENSE](LICENSE).

# Running the example module

1. [Install Rust](https://www.rust-lang.org/tools/install); this checkout selects its development toolchain through `rust-toolchain.toml`.
2. Install a C compiler, libclang and [Redis](https://redis.io/download). See the [prerequisites](docs/DEVELOPMENT.md#prerequisites).
3. Run `cargo build --locked --example hello`
4. Start a redis server with the `hello` module
   * Linux: `redis-server --loadmodule ./target/debug/examples/libhello.so`
   * Mac: `redis-server --loadmodule ./target/debug/examples/libhello.dylib`
5. Open a Redis CLI, and run `HELLO.MUL 31 11`.

# Writing your own module

See the [examples](examples) directory for some sample modules.

This crate tries to provide high-level wrappers around the standard Redis Modules API, while preserving the API's basic concepts.
Therefore, following the [Redis Modules API](https://redis.io/topics/modules-intro) documentation will be mostly relevant here as well.

# Redis Modules based on this crate

The following are some modules that are built on this crate:

- [RedisJSON](https://github.com/RedisJSON/RedisJSON)
- [Rust API for RediSearch](https://github.com/RediSearch/redisearch-api-rs)
- [RedisStateMachine](https://github.com/RedisLabsModules/redis-state-machine)

Several member of the community have written their own modules based on this.
