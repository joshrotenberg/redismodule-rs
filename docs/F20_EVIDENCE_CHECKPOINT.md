# F20 — Evidence checkpoint after reply and configuration loading repairs

The hybrid fork now addresses both priorities identified in [F15](F15_EVIDENCE_PRIORITIES.md):
call-result byte/wire conversion (F16/F17) and checked configuration loading
(F18/F19). The initial repair/author-API sequence is complete locally. The next
useful lane is a narrow source-only specification of configuration metadata
retirement, before considering any frees; a new feature is not needed to fill
an interval.

This is a documentation checkpoint at post-F19 master
`e406ed913682f6b207c55586dae4ead49b62c1cb`. Existing receipts are checked, with
no SDK implementation, compilation, tests, benchmarks or native executions in
F20. Historical results belong to their recorded revisions; counts are not
additive independent coverage or one cumulative run against today's tree.

## Goal scorecard

| Josh's goal | Delivered result | Evidence boundary |
|---|---|---|
| Fix existing implementation problems | F02 literal-format/NUL logging; F07 owner-bound views; F09 explicit private string ownership and direct producer/scan consequences; F11 cached getter owners; F13 observable registration failure. | Earlier compiler/pure/selected native results remain in the [F15 scorecard](F15_EVIDENCE_PRIORITIES.md). Raw/native aliases and accepted configuration metadata still have separate obligations. |
| Fix existing problems: new F17 | Valid UTF-8 call strings become BulkString; invalid text remains StringBuffer. One copied Vec is consumed/recovered. Keys keep String versus byte BulkString identity, Eq/Hash/Ord, integer/bool and unsupported-key policy. | 35 pure tests, 10 checkout + 10 actual-archive compiler controls, four native host/profile cells, 16 integration executions, six isolated reference builds/12 flows. Exact bytes and actual bulk framing, including NUL/CRLF, were checked. [F17](F17_CALL_REPLY.md). |
| Fix existing problems: new F19 | Additive load_configurations returns Result for absent API/non-OK; macro/manual callers propagate at existing positions. Failed OnLoad skips later init and closes pending cached getter owners before native removal. | 38 pure tests, 10 checkout + 10 archive compiler controls; four main native cells plus four startup/four registration controls, four F11 ownership cells; selected zero/one snapshot accounting. [F19](F19_LOAD_CONFIGS.md). API release counts do not prove native refcounts or accepted metadata reclamation. |
| Rust-like API / less boilerplate | F03 facade macro dependency hygiene; F05 borrowed ArgCursor; F14 typed config::{integer,boolean,string,enumeration} builders. F19 replaces selected unchecked raw calls with a fallible helper and ordinary error propagation. | F14 measured formatted calls from the fallible F13 baseline: stream-filter six calls **62→39** nonempty lines; JMESPath four calls **60→41**, including equivalent error handling/callback bodies. F19 comparisons below are actual code, **not a new line-count measurement**. Callback boxing still allocates. |
| Measured efficiency | F05 parser micro-workload: **3→0 Rust allocation requests per parse**, requested bytes **11/42/4106→0** for expression lengths 1/32/4096, 360 paired parses/two hosts/two profiles. F09 owner layout: **16→8 bytes**, Option 8, selected arm64. | Parsing window excludes decoding/replies/native C allocation; layout is a representation measurement. Neither establishes whole-module throughput, heap/leak balance or support policy. See F15 and its retained receipts. |
| Mechanical memory/type boundaries | Owner-bound views/private ingress, selected transfers/drop probes, static typed builder storage/callback lifetime/consuming registration. F17 pure pointer/capacity observations show reuse of its owned buffer. F19 production pure adapter establishes availability/status/single-call policy. | F17 still copies native bytes once: no measured allocator-request reduction, zero-copy or throughput claim. Compiler/drop/source evidence differs from native legality and complete Context/GIL/thread/alias/reentry/panic safety. |

F04's proc-command fixtures are a validation improvement. F06/F08/F10/F12/F16/F18
were source-only specifications; subsequent implementations supply their own
execution evidence. F12's uncompiled 57→39 proposal is superseded by F14's
actual measurements above. No proposed retirement API is implemented by F20.

## Real author migrations and compatibility

Both isolated F19 reference patches start with this F17-prerequisite tail:

```rust
unsafe {
    if let Some(load_configs) = RedisModule_LoadConfigs {
        load_configs(ctx.ctx);
    }
}
Ok(())
```

Stream-filter now ends its existing `Result<(), String>` helper with:

```rust
redis_module::configuration::load_configurations(ctx).map_err(|error| error.to_string())?;
Ok(())
```

Its existing init boundary remains necessary:

```rust
if let Err(e) = config::register(ctx, args) {
    ctx.log_warning(&format!("sf: refusing to load: {e}"));
    return Status::Err;
}
```

JMESPath's already migrated `Result<(), redis_module::RedisError>` helper uses:

```rust
redis_module::configuration::load_configurations(ctx)?;
Ok(())
```

Its retained init handling is:

```rust
if let Err(error) = config::register_configs(ctx) {
    ctx.log_warning(&error.to_string());
    return Status::Err;
}
```

These are actual snapshot code/patches in
`target/f19/references/{streamfilter,jmespath}/`, relative to their F17 snapshots.
Only raw-load imports/tails changed in F19; domain parser/default/custom
storage/callback bodies stayed. Original JMESPath's older void helper is not the
F17 comparison baseline. Event-stream's tuple/custom storage bodies remain;
Carolina has no selected load migration. All original module directories stay
read-only; migration is demonstrated in isolated snapshots, not imposed.

The selected SDK example `examples/configuration_builders.rs` similarly replaces
its ignored raw call (`raw::RedisModule_LoadConfigs.unwrap()(ctx.ctx)`) with:

```rust
if let Err(error) = redis_module::configuration::load_configurations(ctx) {
    ctx.log_warning(&error.to_string());
    return Status::Err;
}
```

This improves observability: absent API and native failure become explicit
errors, with detailed native diagnostics in server logs. There is no errno,
retry, rollback, deduplication, default cache or stored Context. Registration
failure remains distinct and earlier. The macro's load position stays after
registration and before config helper commands, info/server events, user init
and scope commit. Keyspace notification handlers are registered earlier. Manual helpers
stay at their prior positions inside init. Successful defaults/order/command/
parser/text/numeric/stored-format policy stays; 7.2/8.10 repeated-default policies
are preserved, not repaired. Failed OnLoad does not call user deinit. Forced
post-load unload still calls deinit under a native assertion: no refusing-deinit
assertion path was executed. An absent configurations block generates no call
(source/compiler evidence); a present empty block still issues the call and reports missing API when the
API is absent, rather than skipping. Real host absence is untested; the production pure adapter tests it.

F17's direct SDK consumer migration is equally explicit. `version_from_info`
previously accepted only SimpleString; the current match is:

```rust
if let RedisValue::SimpleString(info_str) | RedisValue::BulkString(info_str) = info {
```

The explicit SimpleString version control remains; bulk text is now accepted by
version/Enterprise-marker parsing. Strict String extraction still rejects invalid
UTF-8. The deliberately changed implicit result includes wrapped native SET OK:
it emits bulk OK, while explicit SimpleString/Static/REDIS_OK choices stay.
Native string tags do not retain the original status/bulk distinction, so this
normalizes replies rather than reconstructing that distinction. Existing
StringCallReply::as_bytes and invalid-UTF8 fallback already worked. DUMP checks
use actual native bytes; config-get forwarding retains policy with selected
bulk/byte checks. Synthetic Enterprise markers plus actual OSS are not an
Enterprise host result. Earlier Rust-source breaks remain listed in F15; no
arbitrary downstream source/wire compatibility promise follows.

## Evidence strength and remaining limits

F17's source/pure buffer observations do not measure allocator requests. Its
native/reference receipts retain their as-run hashes before two test/comment
refinements; final pure/fmt/archive/compiler checks followed those refinements.
F19's production/fixtures/runners stayed unchanged after its source review;
only an evidence-wording correction followed. Retained failed compile/harness
attempts remain separate from corrected passing reports.

F19 additionally records 38 examples in both profiles, Clippy/fmt/18 passing doc
tests (four ignored), six reference builds/12 domain flows/eight rejection-reload
controls, four fresh archives/three consumers/six builds, and 92 packaged
Rust/native/manifest-origin byte matches. F17's corresponding archive match
covers 47 production/manifest-origin files, a different set. Archive consumer
runtime was skipped; owned SDK/reference fixtures provide selected native
evidence. Neither count certifies arbitrary downstream applications.

Selected native coverage is Redis 7.2.8/8.10.2, debug/release, macOS arm64/libc.
It is not Linux/jemalloc/static-bindgen/older-server/product/platform support.
Full app/optional Search/JSON/TimeSeries, Cluster/persistence/replication and
general RESP3 coverage remain separate. Known gaps include accepted userdata/
closure/static-resource retirement, broad Context/GIL/thread/phase/alias/reentry/
panic contracts, worker-thread configuration callbacks, type/timer/blocked-client
unload refusals, legal native no-current-user and 8.10 template-hash cells.
Active defrag requires a capable allocator/host. Pure probes/compiler bodies do
not initialize Redis APIs or fabricate Contexts. The existing F11 real FreeString
forwarder counts issued getter owners/API release calls, not process heap or
native refcounts; raw custom lifecycle recipe coverage is uninvoked compiler
metadata, not a new raw-entrypoint host fixture.

## Recommended next bounded lane

Recommend **F21 source-only configuration accepted-metadata retirement scope**,
limited to the already deferred registration userdata/captured callbacks and
F11 registry-pointer retirement. It follows the existing configuration work and
addresses a known ownership gap. It is not authority to free pointers now, nor
a general Context/FFI audit. Broader static-resource families stay separate.

Dependencies are the current F11/F13/F19 state machine, actual selected native
registration/removal/unload source contracts and refreshed issue/PR overlap.
Inventory who retains/calls each pointer, distinguish failed registration from
accepted registration and cached getter owners, and specify a defensible last
use/quiescence point before any reclaim. Account for pending failed load,
committed/canceled/successful unload and reload. A native-removal interval is not
a callable callback window. If native evidence cannot establish retirement,
record the blocker and retain stable metadata; do not replace it with a guessed
free or blanket Send/Sync/phase claim.

A later implementation would require a separately reviewed narrow contract,
required tracked-issue visibility if mapped, pure ownership/state/drop probes,
uninvoked public compiler controls, legal isolated host failure/unload/reload
and retained-callback accounting, fresh archives/consumers and independent review.
No old invalid path, fake context, crash/leak sampler or speculative native
callback is an acceptance test. F21 should define these gates, not run a prototype.
No new human decision is required for this checkpoint. Josh can redirect the
next source-only scope; CI remains held unless explicitly resumed. No metadata
implementation, CI, release or support decision follows automatically.

## Local handoff and credit

F20 refresh finds five fork drafts, 45 upstream open PRs and 55 open issues;
no active F20/Due worker or overlapping F20 branch/worktree was observed.
Fork drafts 1–5 remain open for deferred CI/remote handoff. The historical F15
read-only CI snapshot confirms F07 five successes/three failures, superseding
its early four/three/one-pending snapshot. No CI rerun/poll is needed here.
Fork master is not pushed; F17's required baseline/kickoff uploaded F11 merge
and F12–F16 ancestors under prior issue-visibility/history authorization.
F18/F19/F20 upload no new history. GitHub merges/releases remain human-owned.

Preserve BSD notices and exact upstream adaptation credits: Josh Rotenberg,
PR476 `437bf22b933c31d04573943b692542032edd9538`; jonnyomerredis,
PR487 `c7ca18e4a6fce2f2f32e390f39abef4d67e1c9c6`; oshadmi,
PR488 historical reviewed `01ff52181d9d2efd136b1ef38143ad05223adca6`;
MikkelSnitker, PR93 `f1ec666ecc6891e11130df853e8b80c758df5230`.
Configuration ancestry PR229 by MeirShpilraien at
`566ad273797871acc6f50b5345a0c1cfa16fc34b` is narrow adaptation context,
not a full accepted port. PR375's broad FFI/status work remains excluded.
Leaves upstream issues 234/134/246/358/473 open; issue471 is excluded and
issue378 remains broader Redis6/CI background. No package/MSRV/server/platform/
support/release or original-module migration decision is made.

One new Due/Claude source review approves, true exit 0. Request/result/answer,
172 trace records and offline summary validate; 36 activities complete with no
failures/gaps/drops. Reviewed inputs stayed unchanged through completion. The
caller checked/applied two nonblocking wording clarifications: API absence is
conditional for an empty block, and keyspace handlers precede configuration
loading while info/server events follow. Final document hashes distinguish these
refinements from the frozen review inputs. No second model or SDK execution
review is claimed; source approval does not certify receipt hashes/runtime.

The [compact evidence](evidence/f20-evidence-checkpoint.json) and local
`target/f20/` retain caller claim/hash/original-state checks and the new Due
source review. Review approval and trace completeness are separate from the
caller's receipt verification. This checkpoint uses a local documentation branch
and reviewed no-ff merge; no SDK execution, push, PR or publication.
