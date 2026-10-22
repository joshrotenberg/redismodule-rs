# Module configurations

The SDK registers numeric, string, boolean and enum settings with Redis's
module configuration API, available in Redis 7.0 and later. Redis exposes
them through `CONFIG GET`, `CONFIG SET`, startup configuration, and
`MODULE LOADEX ... CONFIG`. An SDK compatibility floor does not add these
APIs to an older running server; propagate registration and loading errors.

See [configuration.rs](../examples/configuration.rs) for all four types,
[parsed_configuration.rs](../examples/parsed_configuration.rs) for validated
strings and observers, and [lifecycle.rs](../examples/lifecycle.rs) for an
immutable setting and repeated loads.

## Register, load, then initialize

Configuration storage is registered through `&'static` references. Choose
storage that fits the setting:

| Storage | Use |
|---|---|
| `AtomicI64`, `AtomicBool` | Simple numeric or boolean values. |
| `RedisGILGuard<T>` | Values read while holding a Redis lock proof, including strings and enums. |
| `Mutex<T>` | Supported cloneable storage; release its guard before operations that could reenter its configuration callbacks. |
| `ParsedStringConfig<T>` | UTF-8 text kept together with its validated parsed representation, without requiring `T: Clone` or `T: Sync`. |
| A custom `ConfigurationValue<T>` | Validation or storage behavior beyond the provided implementations. |

Inside `redis_module!`, list the groups you need; unused groups are optional.
For example, the lifecycle module declares:

```rust
configurations: [
    i64: [
        ["limit", &LIMIT, 4, 1, 64, ConfigurationFlags::DEFAULT, None],
    ],
    string: [
        ["prefix", &PREFIX, "items:", ConfigurationFlags::IMMUTABLE, None],
    ],
    module_args_as_configuration: true,
]
```

The fields for a numeric entry are its name, storage, default, inclusive
minimum and maximum, flags, and optional `on_changed` observer. String,
boolean and enum entries omit the numeric bounds. Names are normally exposed
as `<module>.<setting>`, such as `lifecycle.limit`.

The macro registers settings, calls `load_configurations`, and then calls
`init(&RegistrationContext, ...)`. Init can therefore read the selected values
before starting work. A registration or load error rejects the module before
init runs. Loading can call setters and observers before init, so they must
not depend on resources that init has yet to create.

For manual registration, call the `register_*_configuration` functions from
init using its `RegistrationContext`, check every result, then call
`load_configurations` once after the last registration. Use this route instead
of mixing manual registrations into a macro `configurations` block: the block
has already loaded its settings before init. The
[manual registration example](../examples/configuration_registration.rs)
shows the sequence.

## Defaults and load precedence

The value used to construct Rust storage is an initial seed, not the
configuration's registered default. For example, `AtomicI64::new(0)` may be
registered with default `4`. `ParsedStringConfig::new` validates its seed
immediately; registration supplies a separate fallback.

For each setting, the load value is selected as follows:

| Source | Effect |
|---|---|
| Default in the registration entry | Fallback when no override is supplied. |
| SDK module argument, with `module_args_as_configuration: true` | Replaces that registration default. |
| Explicit Redis startup configuration or `MODULE LOADEX ... CONFIG` | Overrides the selected registration default. |

For example:

```text
MODULE LOADEX /absolute/path/liblifecycle.so CONFIG lifecycle.limit 9 ARGS limit 6
```

The loaded limit is `9`, which init also sees. With only `ARGS limit 6`, it
is `6`; with neither override, it is `4`. `CONFIG` uses the registered Redis
name; SDK argument pairs use the unprefixed name. Use `.dylib` on macOS.

Keep all fallback values valid. The SDK parses recognized module arguments
and checks numeric registration bounds before Redis applies its explicit
overrides. For example, `ARGS limit 0` fails the lifecycle module's minimum
check even alongside `CONFIG lifecycle.limit 9`.

Redis applies the selected load values through configuration setters. Do not
assume every unused fallback will be passed through the setter first, or rely
on callback order or counts across server versions. A failed load can leave
earlier setters' application-side effects in place; it is not a transaction
over Rust statics. The module should reject the load and clean up work it
started, as described in the [upgrade guide](UPGRADING-MODULES.md).

## Module argument syntax

The SDK's option interprets arguments as consecutive `name value` pairs.
It compares names exactly and searches name positions only, so a string value
equal to another setting's name is still a value.

- Numeric arguments use signed decimal `i64` parsing, then inclusive bounds.
- With `ConfigurationFlags::MEMORY`, arguments accept a nonnegative integer
  with an optional case-insensitive unit: `b`, `k`, `kb`, `m`, `mb`, `g`, `gb`.
  `k`, `m`, `g` use powers of 1000; `kb`, `mb`, `gb` use powers of 1024.
  Conversion is checked for overflow and bounds apply to the resulting bytes.
- Boolean arguments accept `yes` or `no` in any ASCII case, including `NO`.
  `true`, `false`, `1` and `0` are not boolean argument spellings.
- Enum arguments match an option name exactly. String arguments must be UTF-8.

This is a lookup helper, not a complete argument-schema validator. The first
matching pair wins; unknown names and an unpaired final token are ignored by
the lookup. Validate the full argument list in your module if those cases
should fail. Modules with positional or more complex syntax should parse
their own arguments and register configurations manually. With the option
disabled or omitted, the macro does not derive defaults from module arguments;
init still receives the original arguments.

## Validate before storage; observe afterward

Redis checks native types, numeric bounds and flags such as `IMMUTABLE`.
Application validation belongs in `ConfigurationValue::set` or the parser of
`ParsedStringConfig`. Validate the candidate completely before replacing the
old value. Returning an error should leave the prior value unchanged.

`ParsedStringConfig<T>` rejects invalid UTF-8 and parser errors before changing
either its text or parsed value. It preserves accepted text exactly for
`CONFIG GET`, even if the parsed value normalizes it. `String` storage also
requires UTF-8; use `RedisString` storage when runtime string values need to
retain arbitrary bytes. The macro's string defaults and string argument
conversion remain UTF-8.

`on_changed` runs inside the setter trampoline, after storage succeeds. It
receives `&ConfigurationContext`, the setting name, and its registered
storage. It returns no result and cannot veto an update. It is also distinct
from Redis's native `applyfn` for grouped changes; the SDK registration helpers
do not expose that grouped apply callback.

Observers run on the thread executing the setter. A worker can hold the Redis
lock and call `CONFIG SET`, which invokes observers on that worker. Redis
serializes these invocations, but serialization alone does not make `Rc` or
other non-`Send` captures safe to access from a different thread. All four
registration helpers, including observers declared in `redis_module!`, require
`Send` closures. Ordinary function callbacks are unchanged. For shared state,
use `Arc` with appropriate synchronization, atomics, or Redis-locked storage.
Owned `Cell` or `RefCell` captures can be used because invocation is serialized;
the closure need not be `Sync`. Explicit `Box<dyn Fn(...)>` annotations must
include `+ Send`.

The [configuration observer example](../examples/configuration_observers.rs)
registers all four types with owned `Cell` counters, records the invoking
thread, and updates them from client and worker calls. It joins finished workers
before unloading, refusing unload while a worker still runs.

Keep parsing free of side effects, and make observers repeatable and
non-panicking. They can run during loading and rollback, and are not a
commit notification for an entire `CONFIG SET` command. In particular:

```text
CONFIG SET parsed_configuration.workers 8 parsed_configuration.prefix invalid
```

In the parsed example, `8` is accepted and observed, but the prefix is invalid.
Redis restores the previous stored values, invoking setters again; the worker
observer sees both the intermediate `8` and the restored `4`. Its observation
history is not rolled back. Do not use this callback as a guarantee that an
irreversible external action corresponds to a successful configuration command.

Redis attempts to restore backed-up values after a runtime multi-setting
failure, but restoration also depends on setters accepting the old values.
This is different from the partial effects of a failed initial load. See
[Redis's setter and rollback path](https://github.com/redis/redis/blob/7.2/src/config.c#L720-L864)
and the [native configuration callback contract](https://redis.io/docs/latest/develop/reference/modules/modules-api-ref/#redismodule_registerstringconfig).

Separate per-setting observers do not provide cross-field validation or a
transaction over external resources. When several fields must be validated
and published together, one composite value in a `ParsedStringConfig` can
provide a single configuration update boundary. More involved grouped apply
behavior needs a separate design.

## Read under the right lock and preserve settings across reloads

`ConfigurationContext` proves the Redis lock is held for configuration
callbacks. It can lock `RedisGILGuard` storage, but does not expose Redis calls,
replies or registration operations. Command, event and registration contexts,
and a locked worker's `WorkerContext`, also provide lock proofs for reads.

Use `config.with_value(ctx, |raw, parsed| ...)` to read a matching raw/parsed
pair. Return owned results; borrowed references cannot escape. Do not reenter
that same configuration while its read closure is borrowing it. Its parsed
type must be `Send` because locked workers may access it.

`IMMUTABLE` settings must be supplied at load time; later `CONFIG SET` cannot
change them. For reload, replay the desired values, including mutable settings
changed at runtime. Without replay, the new registration uses its new defaults.
Update the deployment's startup configuration separately for future restarts.
See [Upgrading a loaded module](UPGRADING-MODULES.md) for recovery and cleanup.

The integration suite checks precedence, invalid arguments, raw/parsed
consistency, callback observations during rollback, and reload behavior:

```sh
cargo build --examples
cargo test --test integration configuration_
```
