# F15 — Evidence and next priorities

The hybrid fork has delivered concrete improvements against all three goals.
The strongest efficiency result is a small parsing workload; the strongest
ergonomics result is less configuration author code; the ownership work adds
specific compiler-enforced boundaries and corrected native accounting. This is
enough evidence to continue the fork, with explicit limits on the claims.

This checkpoint uses existing receipts at post-F14 master
`b3d205416578f3c0cfe15d1fba03df243d0e6045`. It changes documentation only.
No SDK implementation, test, benchmark or native execution occurs in F15.
Earlier receipts describe their own tested revisions, not one cumulative test
run of every historical contract against today's tree. Counts below must not
be added together as independent coverage.

## Scorecard against the goals

| Goal / delivered slice | Concrete improvement | Evidence and comparison boundary |
|---|---|---|
| Fix existing problems: F02 logging | Embedded NUL becomes visible `\0`; native logging uses a fixed `%s` format and a separate message argument. Public signatures stay. | Five new pure tests and eight selected native executions, including legal IO callbacks and exact text. No historical invalid path was run. [F02](F02_LOGGING.md) / [receipt](evidence/f02-logging.json). |
| Fix existing problems: F07/F09 strings | Views borrow their owner; consuming `NextArg::next_str` is removed. Private non-null ownership, explicit unsafe acquisition/adoption and decoder, producer-context detachment, optional scan values/current user, valid mutable callback origins and balanced direct producer/transfer paths. | F07: 18 compiler controls and 32 selected native executions. F09: 28 compiler controls, 68 unique host/profile cases plus 16 affected-fixture rechecks. These establish selected contracts, not all native producers or aliases. [F07](F07_STRING_VIEWS.md), [F09](F09_STRING_OWNERSHIP.md). |
| Fix existing problems: F11/F13 configuration | Getter retains zero/one owned replies and lends the pointer through native copying; selected load/error/unload cleanup releases it. Every registration kind returns `Result`; missing APIs/native rejection become observable and rejected callback data is dropped. | F11: six new pure state probes and four native ownership cells. F13: four new pure production-helper probes (unavailable API, rejected/unsubmitted callback drop and stable accepted owner), 10 compiler controls plus 10 archived controls, 56 native cells. Accepted userdata metadata reclamation is still deferred. [F11](F11_CONFIGURATION_OWNERSHIP.md), [F13](F13_FALLIBLE_CONFIGURATION.md). |
| Rust-like API / less boilerplate: F03/F05 | Registration macros use the facade's `linkme`; authors need two SDK dependencies rather than those two plus direct `linkme`. `ArgCursor` offers slice-based borrowing, bytes/text choice and exact arity/tail handling. | All 11 registration attributes checked through actual archive consumers/link tests; conventional names still required. Cursor author example removes one statement, with the larger gain in borrowing and copies. [F03](F03_MACROS.md), [F05](F05_ARGUMENTS.md). |
| Rust-like API / less boilerplate: F14 | Typed `config::{integer,boolean,string,enumeration}` factories, explicit integer range, named optional flags/callbacks and consuming fallible registration. Existing free functions, tuple DSL and custom storage remain. | Six stream-filter calls: **62→39** nonempty lines. Four JMESPath calls: **60→41**. F13 baseline, identical rustfmt 1.94/edition 2021 extraction, including equivalent error handling and callback bodies. Isolated helpers compile/run; originals unchanged. [F14](F14_CONFIGURATION_BUILDERS.md) / [receipt](evidence/f14-configuration-builders.json). |
| Efficiency: F05 measured micro-workload | Borrow expression/option bytes instead of copying expression, option and uppercase text. | **3→0 Rust allocation requests per parse**, requested bytes **11/42/4106→0** for expression lengths 1/32/4096; 360 paired parses, two hosts/two profiles. Window excludes decoding, reply construction and native C allocation. No throughput/full-module/total-memory claim. [F05 receipt](evidence/f05-arguments.json). |
| Mechanical memory/type boundaries: F07/F09/F11/F13/F14 | Owner-bound views/private construction; one pointer without cached Context; explicit native transfer/drop; selected cached/rejected-resource accounting; static typed builder storage, callback lifetime and consuming registration. | F09 owner **16-byte baseline layout→8 bytes**, with 8-byte `Option`, on tested arm64. Compiler rejects selected lifetime, private/raw-ingress, wrong-kind and reuse errors. F14's 26 controls also pass against the actual archived SDK. Layout, compiler and API-call/drop counters are different evidence, not a universal safety certificate. |

F04 supplied meaningful proc-command/derived-reply runtime fixtures: eight
selected example executions and eight fresh archive proc flows, with metadata,
key discovery, nested/flattened replies, integer endpoints and ordinary error
recovery. It improves validation rather than claiming a new production API or
performance gain. See [F04](F04_PROC_RUNTIME.md).

F06, F08, F10 and F12 were source-only specifications. Their principal proposals
were subsequently implemented in F07, F09, F11 and F13/F14 respectively; the
specification itself was not compiler/runtime evidence. F12's **57→39** lines
and `i64` spelling were an uncompiled proposal against F11. The implemented F14
comparison above supersedes that figure, using the fallible F13 baseline.

## What authors can use now

Real migrated stream-filter call, with its existing `Result<(), String>` boundary:

```rust
config::boolean("use-iterator", &USE_ITERATOR, use_iterator)
    .register(ctx)
    .map_err(|error| error.to_string())?;
```

The builder hides positional default options and `Some(Box::new(...))` syntax.
It delegates to the tested registration functions, including F11's string load
admission. Callback boxing still allocates; no allocation reduction is claimed
for builders. Names/default text are borrowed until registration, storage stays
static, and custom storage/domain parsing/callback bodies remain supported.

Compatibility is deliberate: raw constructors/decoder and `inner` access, the
consuming text method, scan callback signatures, nullable current user and
registration result signatures require Rust-source migration. Compile examples
and isolated migrations are evidence of a path, not compatibility with arbitrary
downstream code. Strict text/numeric/stored-format and selected successful
command/default behavior were preserved; no universal command/persistence/
replication compatibility promise follows. Correcting the extra string retain
can permit native append where the old accounting incorrectly refused it.

## Evidence strength and outstanding limits

| Evidence class | What it establishes here | Important limit |
|---|---|---|
| Source / mechanical | Ownership/control-flow/layout comparisons and selected native source contracts | Source review is not runtime or complete soundness proof. Raw pointers, contexts and retained native aliases still carry obligations. |
| Pure / compiler | Selected resource/state probes and specific diagnostics, positive controls and public archived APIs | These checks do not initialize Redis APIs or establish native callback/thread legality. |
| Actual native / references | Corrected fixtures on Redis 7.2.8/8.10.2, debug/release, selected macOS arm64/libc cells; isolated author flows | No Linux/jemalloc/static-bindgen/older-server/support policy. Full application, Search/JSON, RESP3/Cluster, persistence/replication coverage remains separate. F01's selected restart flows are historical baseline evidence. |
| Latest F14 integration | 30 pure tests; 18 docs (four ignored); 35 examples both profiles; Clippy/fmt; 64 paired native cells = 32 pairs; four isolated builds/eight flows; four fresh archives/three consumers/six builds | Archive runtime was skipped; owned SDK/reference fixtures provide native evidence. Pure missing-API handling is inherited from F13, with no real missing-API host cell. |
| Measurement | F05 scoped Rust requests/bytes; F09 layout; F14 consistently formatted call-site lines | None is a whole-project speedup, heap/leak sampler, fewer builder callback allocations or proof that every raw/FFI path is safe. |

Still open: broad Context/GIL/thread/phase/alias/reentry/panic/key/raw-module
contracts; complete accepted userdata/closure/static-resource reclamation;
worker-thread configuration callbacks and type/timer/blocked-client unload
refusals; legal native no-current-user coverage; 8.10 template-specific
TMPL_LP/TMPL_ARRAY hash encodings. Active defrag needs a capable allocator/host;
7.4/8.0-specific ACL cells need their selected API floor. Borrow checks do not
make object pointers and byte buffers equally stable across native phases.

Failed/intermediate harness records remain distinct from corrected passing
receipts. In particular, F13's `runtime-corrected/report.json` and F14's
`runtime-corrected/report.json` are authoritative; F13's `runtime-final.log`
retains an earlier harness failure. Native flag behavior was preserved, not
inferred from an initial fixture assumption. See each chunk's compact record
for exact artifact hashes, prerequisites and retained failures.

## Next priority: scope binary call-result conversion

Recommend **F16 source-only scoping of `CallReply` string → `RedisValue`
conversion**, before another implementation. This is an existing F01/F09 gap,
affects ordinary module authors beyond configuration, and connects byte
preservation, Rust API clarity and avoidable copies within one small boundary.
It is not a claim that binary replies are wholly unsupported: `StringCallReply`
already has `as_bytes`, and non-UTF-8 values already fall back to `StringBuffer`.
Current `src/redisvalue.rs` chooses `SimpleString` for valid UTF-8, using an owned
text conversion first. Upstream PR 93 discusses the bulk-versus-simple distinction.
Neither the wire policy nor any copy saving is decided/measured by F15.
The downstream SimpleString emitter also uses `CString::new(s).unwrap()`;
its rejection of interior NUL is a source-observed boundary to cover in the
scope, with no native execution or reproduction claimed.

F16 should refresh overlap, inventory only direct conversions and affected SDK/
reference callers, inspect retained native reply contracts, and specify:

- A byte-preserving default conversion versus an opt-in/new API; explicit
  strict text extraction and the source/wire migration consequences, including
  the direct value/key sites in `src/redisvalue.rs:214–218` and `236–239`.
  The key conversion already has a byte fallback; the surrounding unhashable-key
  panic policy must be distinguished and retained unless a later scoped decision
  changes it. Separate status/error/verbatim/
  RESP3/blocking reply families unless required by the selected conversion.
- Corrected-code gates for empty/ASCII/Unicode/NUL/non-UTF-8/CR/LF/CRLF byte
  values, top-level and selected nested replies, ownership after native reply
  disposal, and compiler checks for any new borrowed view. Pure probes must
  remain Redis-independent. Runtime tests must use only the corrected contract
  in legal isolated owned-host command phases, with exact byte assertions and
  subsequent ordinary commands; never replay a historical invalid wire path.
- Fresh archive/public consumer checks for changed exported APIs and one
  selected isolated reference flow if actually affected. Measure a defined
  conversion window only if claiming copies/allocations; do not promise a
  zero-copy design or full RESP2/RESP3 coverage in advance.

Dependencies: current F07/F09 ownership boundaries, F04/F01 owned-host/RESP2
tooling only as needed, exact PR-93 comparison and existing issue-visibility
rules before any later tracked implementation. No F16 work is implemented here.

Other known priorities stay explicit: LoadConfigs result/order policy was
intentionally preserved (the macro ignores its optional native return); that
deserves separate narrow scoping. Accepted metadata cleanup needs a proven
retirement/quiescence contract before freeing pointers. Broad native audits
need their own bounded entry points. CI remains held until Josh resumes it.
These do not become extra F15 tasks.

Decision for the next interval: proceed with that F16 source-only scope or
redirect focus. No MSRV, package identity, support matrix or release choice is
needed to finish this scorecard. No automatic CI/release/migration work follows.

## Remote state, credit and checkpoint

Fresh October 14 overlap snapshots contain four fork drafts, 45 upstream open
PRs and 55 open issues. Fork drafts [1](https://github.com/joshrotenberg/redismodule-rs/pull/1),
[2](https://github.com/joshrotenberg/redismodule-rs/pull/2),
[3](https://github.com/joshrotenberg/redismodule-rs/pull/3) and
[4](https://github.com/joshrotenberg/redismodule-rs/pull/4) retain F02/F07/F09/F11
for deferred CI/remote handoff. F02's empty kickoff failed four of eight jobs.
F07's early `target/f07/remote-final.json` captured four successes, three failures
and one still-running job. F08's later prose reports five successes/three
failures; F15's read-only GitHub check snapshot now independently confirms that
terminal result for the same F07 implementation head. No CI jobs were rerun.
Fork master was not pushed; earlier required review
baseline/kickoff uploads included local ancestors. F15 uploads no new history.
GitHub merges/releases/publication remain human-owned.

F02 adapts Josh Rotenberg's [upstream PR 476](https://github.com/RedisLabsModules/redismodule-rs/pull/476),
head `437bf22b933c31d04573943b692542032edd9538`, and leaves upstream issue 473
open. F09 credits jonnyomerredis's
[PR 487](https://github.com/RedisLabsModules/redismodule-rs/pull/487), historical
head `c7ca18e4a6fce2f2f32e390f39abef4d67e1c9c6`, and Omer Shadmi (oshadmi)'s
[PR 488](https://github.com/RedisLabsModules/redismodule-rs/pull/488), reviewed
head `01ff52181d9d2efd136b1ef38143ad05223adca6`. Their INFO work is credited,
not newly duplicated in F15. Leaves upstream issue 358 open; issue 471's separate
unvalidated allegation remains excluded. Next-scope overlap includes Mikkel
Snitker's [PR 93](https://github.com/RedisLabsModules/redismodule-rs/pull/93),
observed head `f1ec666ecc6891e11130df853e8b80c758df5230`; metadata/source
observations are not an accepted port or compatibility decision. Inherited BSD
notices remain intact.

F15 uses a dedicated local documentation branch, one new Due/Claude source
review, caller receipt/source/diff checks and the existing reviewed no-ff merge
process. No original module changes, test/benchmark reruns, new PR/push, native
reproduction, general audit or package/support commitment belongs to this chunk.
The reviewer approves; four nonblocking precision suggestions were checked and
applied by the caller (CI receipt timing, emitter boundary, direct key conversion
and probe wording). No second model review of these documentation refinements
or historical code is claimed. Request/result/answer, 206 trace records and the
summary validate; one failed Grep has an unknown metadata-only cause, with no
reported permission blocker. Review approval is separate from receipt checking.
The [compact scorecard evidence](evidence/f15-evidence-priorities.json) records
claim checks, input hashes, review and limitations; raw local receipts remain
under `target/f15/`.
