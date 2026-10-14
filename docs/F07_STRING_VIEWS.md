# F07: owner-bound string views

The reviewed [F06 boundary](F06_STRING_LIFETIMES.md) is implemented. Borrowed text
now follows the RedisString owner's Rust lifetime. The consuming borrowed-text
method is removed, and ownerless raw views require explicit unsafe calls.
Strict UTF-8 errors, remaining NextArg methods, numeric behavior and existing
command arity/replies are preserved. This is a deliberate Rust source break;
package versions are inherited, not a new release.

## Author migration

An owner-backed view has the ordinary Rust shape:

```rust
pub fn try_as_str(&self) -> Result<&str, RedisError> {
    str::from_utf8(self.as_slice())
        .map_err(|_| RedisError::Str("Couldn't parse as UTF-8 string"))
}
```

For a consuming parser, keep each native string owner explicitly:

```rust
let expression_owner = args.next_arg()?;
let expression = expression_owner.try_as_str()?;
```

For several immediate arguments, keep the input vector alive and borrow it:

```rust
let mut args = ArgCursor::new(&argv[1..]);
let expression = args.next_str()?;
let count = args.next_u64()?;
args.done()?;
```

The first recipe replaces the old consuming `NextArg::next_str` without a text
copy. The second is F05's cursor. Persistent data still needs explicit ownership;
the static-data example validates text before assigning `val.to_string()` to its
stored String. Do not substitute lossy `next_string()` when strict text is needed.
Custom NextArg implementations must remove the old trait method definition too.

`from_ptr` and `string_as_slice` retain their names, return types and zero-copy
behavior but are unsafe. Prefer owner-backed accessors. Their safety docs require
initialized APIs, a valid non-null native string, legal server/thread/callback
access and a live, stable read-only buffer for the chosen lifetime. The notification
adapter uses a narrow unsafe block, borrows only within its synchronous handler
and does not adopt/retain callback keys (including non-retainable LOADED keys).
A local lifetime anchor rejects handlers requiring a `'static` key slice. The
raw text helper still returns `Utf8Error`; the owner accessor keeps the SDK's
existing strict UTF-8 error text. The identity fixture checks the owner first,
so its raw-text conversion sees only validated bytes.

## Changes and evidence

| Area | Implementation / verified evidence |
|---|---|
| Rust lifetime relationship | `try_as_str` borrows through `as_slice(&self)`. Corrected compiler checks reject escaped views, early owner drops and mutation through that same owner. |
| Explicit raw boundary | Safe calls to both raw helpers fail with E0133; uninvoked positive controls compile. No invalid native pointer or freed-view test runs. |
| Consuming API | Removed only `NextArg::next_str`; stale calls fail E0599 and stale custom implementations E0407. Migrated custom implementation and strict owned-text controls compile. |
| SDK examples | INFO uses ArgCursor and continues accepting extra args. Static data binds the consumed owner and rejects invalid UTF-8 before assignment. All 30 examples build debug/release. |
| Native views | New isolated string_views module checks owner/raw pointer identity, empty/Unicode/NUL text, binary bytes, errors and later valid commands. |
| Callback boundary | A separately registered binary-safe handler copies bytes when retaining them after the callback. All-byte payloads never reach the older events example's UTF-8 formatter. |
| Archive consumer | Manual fixture adds strict owner/raw text and isolated callback checks; four fresh archives, six builds of three consumer types and two registration tests pass on Rust 1.94.0. |
| Real reference migrations | Isolated JMESPath binds six local owners in five handlers; its existing consuming format parser remains unchanged. Event-stream adds one justified unsafe callback byte view. Four library builds pass, plus eight selected runtime flows. |

The 18 metadata-only compiler cases include seven positive controls and eleven
expected rejections. The checker uses the actual Cargo-produced SDK rlib and
records its hash, exact snippets, compiler, exit and diagnostic codes. Rustdoc
separately passes ten view/NextArg cases and four cursor cases. Function bodies
in positive controls are never invoked; pure tests do not initialize Redis.

The 26 pure/harness tests, 22 Python tests, workspace/all-target Clippy with
warnings denied and formatting pass. Python negative controls reject a textual
identity flag, altered callback bytes, a dropped empty-key notification and
silent acceptance of an expected error.

Native validation uses owned loopback processes on macOS arm64/libc, Redis 7.2.8
and 8.10.2, debug/release:

- 32 focused integration executions: corrected views, INFO, static data,
  configuration, background static-data reply, F05 cursor/allocator regression
  and the existing ordinary keyspace-notification test.
- Eight archive runtime cells: manual owner/callback plus proc regression, both
  hosts/profiles. The archive runner originally covered manual release/proc both
  profiles on 7.2.8; the separate matrix supplies the five missing cells.
- Eight selected reference cells: JMESPath EVAL with local owners/formatting and
  ordinary error recovery; event-stream with explicit SET-event configuration
  and exact captured key/event/database assertions.

Server executable/library hashes, PID/version/allocator observations and logs
are retained. The reference producer copies current source into fresh snapshots,
checks the exact four SDK manifest paths, and captures live SDK inputs before and
after metadata and each locked build. New attempts do not inherit an old success.
Capture boundaries cannot detect an edit-and-revert between observations.
The existing broad reference runner is unchanged; this chunk uses a bounded
producer for its selected migrations. Original module source files remain unchanged.

## Initial check corrections

The first integration build used array values instead of slices with redis-rs
0.23 and failed compilation; corrected tests also compare INFO against the native
reply without adding a mandatory environment setting. All corrected builds pass.
The first diagnostic-checker extraction missed indented rustdoc comments; it
correctly refused an unrelated syntax error rather than accepting it as the
expected lifetime/API failure. Its corrected extraction passed the original
16 cases before the two callback controls were added. The new callback control
initially omitted the registration function's C-status return type (E0308);
corrected it. Pinned rustc reports the macro-local lifetime failure as E0716
rather than the initially predicted E0597. The final 18 cases pass with exact
diagnostic checks; no rejected function body is executed.

The first event-stream runtime producer assumed its default filter captured SET;
the module defaults to expired events. Its four successful JMESPath cells and
failed event-stream setup/log remain retained. Four fresh event-stream cells
configure the subscribed event explicitly and pass exact output assertions.
This was a caller fixture setup correction; no SDK/module repair was required.

## Visibility and remaining limits

[Draft PR 2](https://github.com/joshrotenberg/redismodule-rs/pull/2) existed before
implementation. Its empty kickoff is `9255606bf65112c130790dc83aab4c3b394316d1`,
against `review/f07-baseline` at `d59f80b1d5219e9664a28e58478f762a2dd76172`.
Uploading the required baseline included earlier local F03–F06 history; fork
master was not pushed. The partial relationship to upstream issue 358 is listed;
leaves upstream issue 358 open. GitHub merge and release remain human-owned.

Independent Due/Claude implementation review approves the core repair. Its
nonblocking findings prompted a callback lifetime anchor, nonempty-before-empty
callback assertions, documentation of the raw UTF-8 error type and explicit
reference result labels. A second focused review approves that delta; its four
evidence/wording suggestions are applied. The final adapter and fixtures were
rebuilt and the affected native/archive/reference checks rerun successfully.
Both workers only read/search source and evidence; caller-owned checks establish
the results. Both true exits are zero, answers/traces independently validate and
reviewed input hashes stayed unchanged. One core-review Grep activity is marked
failed; its cause is not exposed by the metadata-only trace. Neither reviewer
reported denied reads. No native permission scope was expanded.

The required empty-kickoff GitHub CI run failed four of eight jobs before F07
implementation was pushed. CI remains deferred; PR 2 stays draft for that
remote handoff. Local validation does not certify those runner jobs.
Complete retained outputs are under `target/f07/`; see
[compact results](evidence/f07-string-views.json).

The API enforces a Rust borrow relationship for correctly constructed/used native
strings. Public inner/raw constructors can violate the assumed native invariant;
context validity and native aliases remain separate. E0502 tests one Rust owner,
not alias uniqueness; safe_clone and native mutation/trim behavior are not
certified. No constructor, allocation provenance, general callback/threading,
Borrow<str> semantics, full application suite, persistence/replication, Cluster,
Linux/jemalloc or complete FFI-soundness claim follows. F05's parser allocation
regression still passes; no new throughput or total-memory savings are asserted.
MSRV, package identity, releases and broad support commitments remain undecided.
CI modernization remains deferred.
