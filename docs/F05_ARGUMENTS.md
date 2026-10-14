# F05: borrowed argument parsing

`ArgCursor` adds a small, borrowing alternative to the consuming `NextArg`
interface. It works on the existing `Vec<RedisString>` callback contract; it
does not replace command registration or change native argument decoding.
The input slice is explicit, so callers choose whether to exclude the command
name or parse an option tail. No dependency or support commitment changes.

## API and author experience

Before, from the owned comparison in `examples/args.rs`:

```rust
let mut args = argv.into_iter().skip(1);
let expression = args.next_string()?;
let option = args.next_string()?.to_ascii_uppercase();
if option != "COUNT" {
    return Err(RedisError::Str("ERR expected COUNT"));
}
let count = args.next_u64()?;
args.done()?;
```

After, from the borrowed comparison in the same file:

```rust
let mut args = ArgCursor::new(&argv[1..]);
let expression = args.next_str()?;
if !args.next_bytes()?.eq_ignore_ascii_case(b"COUNT") {
    return Err(RedisError::Str("ERR expected COUNT"));
}
let count = args.next_u64()?;
args.done()?;
```

The original vector remains available. `next_arg()` returns `&RedisString`,
which existing key APIs can use directly; `next_str()` and `next_bytes()` return
references tied to the input slice. Returned references can coexist while the
cursor advances and remain valid after the cursor leaves scope, within the
owner's lifetime. Standard `Iterator`, `ExactSizeIterator` and `FusedIterator`
contracts apply. `remaining()` exposes the unconsumed slice; `done()` checks
arity without consuming extras. The example removes one temporary variable and
one statement; the main benefit is explicit borrowing rather than a dramatic
reduction in line count.

Text parsing rejects invalid UTF-8. Authors can choose raw bytes for binary
arguments. That is a deliberate choice compared with `next_string()`'s lossy
owned conversion; adoption is opt-in. Numeric methods call the same native
parsers as `NextArg`. In particular, `next_u64()` retains the existing zero
through `i64::MAX` range; extending it is a separate API decision.

## Why this slice

Read-only inspection of the four reference modules found distinct needs:

| Module and source | Observation | Appropriate application |
|---|---|---|
| Carolina `crates/klog-module/src/lib.rs`, `option_name` / `parse_fetch` | Converts option names to owned text and then uppercase; numeric values use `NextArg`. | Borrow option bytes and compare ASCII case without materializing uppercase text. Keys and retained request metadata still need suitable ownership. |
| JMESPath `src/command.rs`, `parse_format_options` / `jmespath_mquery` | Collects option tails, copies text and uppercases keywords; expressions are copied before immediate compilation. | Borrow a tail and parse it without an extra argument collection or option-text copies. Cache ownership requirements still apply. |
| Stream filter `src/commands/xreadgroup.rs`, `parse` | Already borrows bytes while parsing with manual indexing, then owns a request for blocking. | A cursor can simplify advancing/arity checks; the retained request's copies remain necessary. No measured allocation gain is claimed here. |
| Event stream `src/commands.rs`, `cmd_streams` | Already borrows text for a single optional case-insensitive mode. | Little allocation benefit for this command; the cursor mainly becomes useful as optional grammar grows. |

Original module working directories were not edited or migrated. Exact inspected
file hashes, Git heads and dirty flags are in `target/f05/references.json`.
These observations informed a synthetic SDK fixture, not a claimed performance
measurement of the full modules.

## Measured and tested

The `args` example wraps `RedisAlloc` with an allocation counter. The measured
window starts inside the callback, after command decoding, and ends before
building the reply. It counts Rust allocation requests and requested bytes;
it does not count native C allocations, deallocations, retained native memory,
wire traffic, total command memory, throughput or latency.

| ASCII expression bytes | Owned parser requests / bytes | Borrowed parser requests / bytes |
|---:|---:|---:|
| 1 | 3 / 11 | 0 / 0 |
| 32 | 3 / 42 | 0 / 0 |
| 4096 | 3 / 4106 | 0 / 0 |

Each length is checked with three COUNT spellings and ten repetitions: 90
paired parses per host/profile cell, 360 pairs in total. Both parsers return the
same length/count for these valid UTF-8 inputs. Rust 1.94.0, native macOS arm64,
Redis 7.2.8 and 8.10.2, debug and release: all four cells pass. Empty/Unicode/NUL
text and invalid-UTF-8 bytes are covered by behavior tests, not the comparison
table. The module has no background Rust workers; counts apply to this fixture
and compiler, not every optimization or caller.

Eight focused integration-test executions pass (behavior and allocation tests
in each cell). Behavior checks cover reference/pointer identity, binary reply
preservation, references used after the cursor's scope, iterator exhaustion,
non-consuming exact-arity checks, signed endpoints, the inherited unsigned
range, float parsing, ordinary arity/parse errors and later successful commands
on the same connection. Owned loopback server PID/version/memory evidence and
artifact hashes are retained in `target/f05/runtime-final/report.json`.

Four documentation cases pass: one valid parser and three compile failures.
Independent `rustc --emit=metadata` checks of those exact snippets verify
E0515 (escaping text), E0505 (dropping its owner) and E0502 (safe Rust mutation
while bytes are borrowed), rather than accepting an unrelated compile error.
Sources, diagnostics and linked SDK artifact hash are in
`target/f05/lifetimes/report.json`. No historical invalid native path was run.

The 26 existing pure/harness tests, workspace/all-target Clippy with warnings
denied, formatting and package-file listing pass. The listing includes the new
API source and example; fresh archive-consumer builds were not rerun for this
dependency-neutral addition. Only the new example was built debug/release.
Existing all-target checks cover compilation, not every example's runtime.

The initial behavior test asserted the client's formatted error as an unsplit
string and failed; it now checks the native error detail. Clippy also prompted
using a lexical cursor scope instead of explicit `drop`. Initial evidence is
retained separately; corrected checks and fresh runtime results pass.

Independent Due/Claude source review approves the implementation. Exit 0;
request/result/answer/all 146 trace records/summary independently validate,
with no dropped records, gaps, failed or unmatched activities. Reviewed source
hashes stayed unchanged. The reviewer ran no tests and did not recompute hashes;
the caller verified those separately. The allocation log now prints observed
reply fields, addressing its evidence suggestion; final debug/release builds,
all eight runtime executions, Clippy and formatting pass. A caller text edit
initially missed the formatted loop during that reporting change; the compiler
rejected the missing index, an explicit patch corrected it, and the failed log
is retained separately. No second model review was needed for that reporting
change. Legacy lifetime repair and optional double-ended iteration are followups.

Compact results: [F05 evidence](evidence/f05-arguments.json). Complete local
review contracts, provenance, receipt and trace are in `target/f05/review/`.
Observed measurement lines are in `target/f05/measurements-final.json`.
Rebuildable earlier package build directories were removed to recover disk
space; source archives/locks/logs and selected F04 module binaries were retained.
The exact relocation/removal record is `target/f05/disk-cleanup.json`.

## Limits and next decision

This API introduces no new unsafe block, native retain or native pointer
constructor. Its Rust lifetime relationship is checked, but the underlying
SDK still exposes raw pointers and native aliases. Existing `NextArg::next_str`
and several `RedisString` raw/text accessors remain separate lifetime/ownership
follow-ups; this chunk does not certify their contracts or complete FFI
soundness. Callbacks still allocate the decoded argument vector and retain
native strings. Keeping the input vector alive can retain those strings longer
than consuming it early. Copies for persistent or blocked requests remain
appropriate. No full-module speedup or total-memory reduction is asserted.

After this slice, choose a narrow ownership/lifetime follow-up or evidence-based
adoption example. Broader CI remains deferred. No MSRV, package identity,
release, Redis support, module migration or publication decision is selected.
This issue-independent chunk stays local under the branch/no-ff merge process.
