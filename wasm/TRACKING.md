# redis-module-wasm: tracking

Work items for the wasm host, with what is done, what is next, and what is
outside this repository's control. Each item has an ID to cite in commits.

Status: `done`, `next` (being worked on or first in line), `todo`, `blocked`.

Last updated 2026-10-20.

## Where it stands

Milestone 1, the skeleton, works on Redis 8.10.2 with no server changes:

- `WASM.LOAD <library> <component> [REPLACE]`, `WASM.CALL <function> <numkeys>
  [key ...] [arg ...]`, `WASM.LIST`, `WASM.DELETE <library>`, `WASM.KILL`.
- A running function yields every 10 ms (wasmtime epochs and
  `RedisModule_Yield`): past `busy-reply-threshold` other clients get `-BUSY`,
  and `WASM.KILL` stops a function that has not written (`UNKILLABLE` if it has,
  `NOTBUSY` if none runs), as `FUNCTION KILL` does. The module unloads cleanly.
- Guests are components implementing `wit/redis.wit`. They import `call`
  (RedisModule_Call in script mode, as the caller's ACL user, errors as replies,
  effects replicated, RESP3) and `log`. No WASI imports.
- Reply types survive both ways, including simple versus bulk strings, maps,
  sets, nil, and errors inside arrays. A guest cannot run `WASM.*` commands.
- The host is about 400 lines. The example guest is 22 KB as a core module and
  23 KB as a component. The integration test runs in about 1.3 seconds.

Built on wasmtime 36 (the long-term support release; current wasmtime needs
Rust 1.95 or later and the workspace pins 1.94), `wit-component` 0.236 and
`wit-bindgen` 0.44.

### Numbers (release build, laptop, one run)

| | 1 client | 50 clients |
|---|---|---|
| `PING` | 11.7k/s | |
| Lua `FCALL incr_twice` (two INCRs) | 12.5k/s | 153k/s (about 6.5 µs per call) |
| `WASM.CALL incr_twice` (same work) | 9.7k/s | 37k/s (about 27 µs per call) |

- One client is bound by the round trip; the 50-client column is the server's
  cost. The 50-client wasm figure is with H1's epoch checks and write tracking
  (39k/s before them; Lua varied between 133k and 153k/s across runs, so the
  difference is within the noise). A fresh instance per call is the likely cause of the difference; not yet
  measured (H2).
- `WASM.LOAD` of the 23 KB component: 23 ms, compilation included, on the main
  thread.
- `used_memory` about 1.3 MB while RSS was 16 to 18 MB: guest memory and
  compiled code are outside the Redis allocator.
- The release dylib is 11 MB.

## Host work

### First, before anyone else runs it

| ID | status | item | done when |
|---|---|---|---|
| H1 | done | **Time limit and kill.** wasmtime epoch interruption; at each epoch the host calls `Context::yield_now(YieldMode::Clients, ..)`, so other clients get `-BUSY` past `busy-reply-threshold`. `WASM.KILL`, flagged `allow-busy`, stops a running guest that has not written, as `FUNCTION KILL` does. | A test runs a guest that loops forever, sees `-BUSY` from another client, kills it, and the server keeps serving. |
| H2 | next | **Per-call cost.** Measure where the 26 µs goes, then reuse instances per library or use wasmtime's pooling allocator. | The benchmark table is updated, with the cause named. |
| H3 | todo | **A fair benchmark.** Add a compute-heavy function next to the two-INCR one, in both wasm and Lua, and keep the script in the repository. | `wasm/bench/` reproduces both rows on one command. |
| H4 | todo | **A guest in another language.** JavaScript (componentize-js), Python (componentize-py) or Go (TinyGo), whichever produces components wasmtime 36 loads. | The integration test loads and calls a non-Rust guest. |
| H5 | todo | **Persistence and replication.** Save libraries in aux data (`aux_save`, `aux_load`) and replicate `WASM.LOAD` and `WASM.DELETE` verbatim. | Tests with a restart and with a replica see the same libraries. |

### Then

| ID | status | item | done when |
|---|---|---|---|
| H6 | todo | **Key specs for `WASM.CALL`.** A keynum key spec so a cluster routes it by its keys. `redis_module!`'s command table cannot express one; check the `#[command]` macro or `RedisModule_SetCommandInfo`. | `COMMAND GETKEYS WASM.CALL f 2 a b x` returns `a b`. |
| H7 | todo | **Compile off the main thread.** `WASM.LOAD` blocks the client and compiles on a thread. | The server answers other clients while a large component compiles. |
| H8 | todo | **Memory.** A memory limit per instance (wasmtime resource limiter), refusing writes when `used_memory_ratio` is at 1, and an `INFO` section with the host's memory. | Tests for the limit, the refusal and the `INFO` fields. |
| H9 | todo | **Script parity.** Function flags (`no-writes`, `allow-oom`, `allow-stale`, `no-cluster`, `allow-cross-slot-keys`), `WASM.CALL_RO`, a `call` that fails where `pcall` returns the error, and `acl-check-command` and `set-resp` imports. | Each has a test matching what Lua does. |
| H10 | todo | **Guest helper library.** A small Rust crate that builds the pre-order node list from typed values, so guests do not build it by hand. | The example guest uses it. |
| H11 | todo | **Bounds check cost.** Signal-based traps are off so wasmtime installs no signal handlers over Redis's crash handler; measure what explicit bounds checks cost. | A number for a memory-heavy guest with traps on and off. |
| H12 | todo | **Context in the store.** The store holds a raw pointer to the command's `Context`. Write down the safety argument or replace it with an SDK pattern (S3). | Reviewed and documented, or replaced. |
| H13 | todo | **Footprint.** Trim wasmtime features and measure the dylib and RSS. | Sizes recorded before and after. |
| H14 | todo | **CI.** A job that builds the guest, runs the host's tests and lints the guest. | Wherever the crate lives with CI. |

## SDK changes the spike found

These are in this repository's SDK and do not depend on the wasm host; each
can be an ordinary PR.

| ID | status | item |
|---|---|---|
| S1 | done here, not on master | `RedisValue::Error(String)`, an error reply with an owned message that can sit inside an array (the `feat!: add RedisValue::Error ...` commit on this branch). Open as its own PR. |
| S2 | todo | `RedisValueKey` has no simple string, so a map key sent as a simple string goes out as a bulk string. |
| S3 | todo | No lifetime-safe way to hand a `Context` to code that needs `'static` data, such as a runtime's store. |
| S4 | todo | Replies can only be built as a `RedisValue` tree; the low-level reply functions are crate-private since 3.0, so nothing streams a reply. |
| S5 | todo | The `min-redis-compatibility-*` floors are mutually exclusive, and Cargo unifies features across a workspace, so a second module crate in the workspace must use the root's floor. |
| S6 | worked around | On macOS a stripped release build of the host fails to load ("mis-aligned LINKEDIT string pool"); the workspace sets `strip = "none"` for it. Check with a newer toolchain. |
| S7 | todo | `ServerInfo::field` panics on a field name with a NUL byte; upstream returns `None`. |
| S8 | todo | `Cargo.toml`'s `repository` field still names upstream. |

## Module API gaps

Outside this repository: each needs a change in Redis's module API. Record the
evidence as the host meets them.

| ID | gap | effect on the host |
|---|---|---|
| G1 | No module API registers an engine for `FUNCTION` or `EVAL` (checked in Redis 8.0.0 and unstable `module.c`). | The host needs its own `WASM.*` commands; `FUNCTION LIST`, `FCALL` and `SCRIPT KILL` do not see it. |
| G2 | `RedisModule_CreateCommand` works only while the module loads. | A function loaded later cannot be a command of its own; every call goes through `WASM.CALL`. |
| G3 | No module API adds memory allocated outside the Redis allocator to `used_memory`. | `maxmemory` and `INFO memory` do not count guest memory or compiled code (confirmed: 1.3 MB used, 16 to 18 MB RSS). |

## Open questions

| ID | question |
|---|---|
| Q1 | Where the crate lives so others can use it: a private repository, a bundle, or a branch. |

## Log

- 2026-10-20: H1 done: epoch interruption with a 10 ms ticker thread,
  `yield_now` at each epoch, `WASM.KILL` with `NOTBUSY` and `UNKILLABLE`, write
  tracking from `COMMAND INFO` (cached, subcommands included), killable
  `WASM.LOAD`, a ticker that stops on unload. Tests: a looping guest gets
  `-BUSY` for other clients and is killed; one that wrote is `UNKILLABLE` and
  `SHUTDOWN NOSAVE` stops the server; five runs in a row passed.
- 2026-10-20: milestone 1 done (the `RedisValue::Error` and `redis-module-wasm`
  commits on this branch); numbers above; tracking document added.
