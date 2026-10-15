# F23 — Stream iterator cleanup and example ordering

Implemented locally from F22 checkpoint `532e84262799389b93eebed5bfe6ea2961936b57`.
This is the first repair in the finite F23–F28 batch. No SDK push, PR, CI rerun or
support-policy change belongs to this chunk.

## Changed behavior

`StreamIterator` resolves Start, Stop, NextID and NextField before acquiring a
native iterator. Missing APIs return descriptive errors. Successful construction
keeps the three functions needed for iteration and teardown; no optional API
unwrap remains after acquisition. A failed Start constructs no owner and cannot
stop a previously active iterator. Drop calls Stop once, never Delete.

Public key factories, `Iterator<Item = StreamRecord>`, ranges, reverse/exclusive
flags and materialized owned fields remain. F09's actual-opening-context field
adoption remains unchanged. A Rust key borrow does not settle native aliases,
context/GIL/phase obligations or arbitrary outside mutations. Stop's failure
cannot be returned from Drop; the legal native handle must remain valid through
teardown. No retry or new panic policy was added.

Both selected native pins permit readonly Stop. Former readonly Delete would
reject the operation; writable adopted handles could delete an entry. Key close
can reclaim a residual iterator while the live stream value remains, followed by
a native assertion. This repair is not evidence of universal former leaks or
ordinary readonly deletion.

The real `examples/stream.rs` command now ends its iterator **and read handle**
before opening a writer and trimming. Nonempty input still reads two records,
returns the first ID and trims below the second (or the existing MAX/MAX sentinel).
Missing/wrong-type keys still return WRONGTYPE; an existing empty stream returns
Null and leaves XLEN zero.

```rust
let (element, id_to_keep) = {
    // Open/read/type-check/iterate, then return the selected values.
    // The iterator and read handle leave scope here.
    /* ... */
};
let Some(element) = element else {
    return Ok(RedisValue::Null);
};
let stream = ctx.open_key_writable(&stream_key);
stream.trim_stream_by_id(id_to_keep, false)?;
```

This is an abbreviated excerpt of implemented code, not a standalone example.
It makes the nonempty order Stop → Close(read) → Open(write) → Trim explicit.

### Empty-stream finding during corrected validation

F22's intended Null control exposed an additional source assumption. Even after
fixing read/write ordering, the current `trim_stream_by_id` treats a native zero
count as an error (`src/key.rs`). The first corrected run therefore failed at the
existing-empty command with “Failed trimming the stream”; its owned host shut down
normally. No former live-trim path was run. The narrow example repair skips that
unnecessary trim by returning Null after the read scope. This makes the intended
empty result reachable; it is a deliberate example behavior correction, not a
claim that the former command already returned Null. The broader zero-count trim
helper contract remains unchanged and outside this batch's selected work.

## Verified evidence

All commands used the pinned Rust 1.94 compiler, locked dependencies and selected
`min-redis-compatibility-version-7-2,bindgen-runtime` feature combination.

| Gate | Actual result and limit |
|---|---|
| Pure tests | 39 pass (33 SDK + 6 dependency checks). One new production-resolver test covers all four missing slots and successful selection using integers, without Redis API initialization, native allocator or fabricated contexts. |
| Compiler | Six checkout and six actual-archive metadata controls pass. Existing Item/range/borrow signatures compile; consuming, dropping or outliving the borrowed key is rejected. The rejected `to_raw_parts` control is never executed. |
| Builds/lints/docs | All 39 examples build debug/release. The corrected empty branch was rebuilt in both profiles. Formatting and Clippy with warnings denied pass; 18 doc tests pass, four remain ignored. |
| Native fixture | Four owned host/profile cells: retained 7.2.8 and 8.10.2, debug/release, 17 case groups each. Zero/one/all/early-result return, same-handle restart, rejected second Start leaving first usable, exact binary fields/values, range/reverse/exclusive, missing/wrong-type errors and existing-empty traversal pass. |
| Cleanup/nonmutation | Legal real-API wrappers observe one Stop for every accepted fixture traversal, before Close, with no Delete or active-close. Contents remain byte-identical through readonly and exclusively adopted writable handles. This counts issued operations, not native heap/refcounts. |
| Actual example | Corrected STREAM_POP missing/wrong-type, empty/one/many, exact remaining IDs and later PING on the same connection pass. Read-before-write ordering is source-scope evidence plus actual output; the real example's APIs are not instrumented. |
| Field ownership | The existing F09 fixture retains exact stream field/value bytes after iterator/key/command return in AM0/AM1 modes, then clears them. Fresh views are derived in the later command. No unchanged full F09 replay. |
| Reload | Corrected stream fixture unloads and loads again legally, then traverses/restarts successfully. No assertion/refusing-deinit path is executed. |
| Archives | Four fresh archives, three external consumers, six profile builds pass. Six stream compiler controls use the archive-produced SDK rlib. All 93 packaged Rust/native/manifest-origin files match current sources. Packaging precedes final status/evidence prose; no whole-archive identity with later documentation is claimed. |
| Originals | All four reference heads/status and 135 tracked Rust hashes remain unchanged; no imposed migration or isolated-reference rebuild is needed for this direct SDK/example scope. |

Raw receipts: `target/f23/{pure.log,checks.json,compiler/report.json,archive-compiler-final/report.json,
runtime-final/report.json,packages-final/report.json,archive-sources-final.json,final-validation.json}`.
The earlier `runtime/` failure and `runtime-disposition.json` are retained separately.
Native source origins remain F22's pinned source evidence; F23 checks their hashes
and retains fresh owned host identity, binary/module hashes and process exits.
No throughput, allocation, zero-copy, broad memory/FFI safety or new platform
support claim follows from these checks. Existing accepted metadata and broader
native contracts remain parked. See compact evidence for independent review and
final source hashes.

## Independent review and limits

One monitored Due/Claude source review approves, with no blocking findings.
Caller separately verified the JSON answer/result/trace contracts, source hashes
and actual effects. Five nonblocking findings were reconciled: explicitly record
the empty-result behavior change and correct F22's contradictory preservation
wording; mark the diagnostic command `write` because a WRITE handle signals
modification on close even with unchanged contents; document its sticky
active-close sentinel and fatal unexpected mutex poisoning; retain optional
exclusive EDOM range coverage as a gap. The final fixture flag/comment changes
received fresh fixture builds, four native cells and archive/compiler checks.
SDK production code and the actual stream handler did not change after review.
No second unchanged model review was purchased.

Unexercised cells include exclusive EDOM Start rejection, second Start on the
WRITE-mode handle, and an iterator held across this fixture's command return.
The selected existing F09 check carries owned fields across return, not a live
iterator. No blanket native phase/alias/Context/GIL/panic guarantee follows.
Fixture locks are not held across native calls. An unexpected fixture panic or
poisoned lock is outside the corrected legal runs; it is not a panic-safe SDK
boundary. The write-labelled diagnostic command may signal native modification
on handle close even though XRANGE proves unchanged stream contents.

## Scope, attribution and next action

Fresh metadata for 45 open upstream PRs, 55 issues and five fork drafts matches
F22's overlap snapshot. No narrow tracked stream-cleanup issue was found; work
uses the local issue-independent process. PR229 by MeirShpilraien at
`566ad273797871acc6f50b5345a0c1cfa16fc34b` remains broad API/stream ancestry context,
not a newly accepted port. Existing BSD notices and prior F02/F09/F17 credits stay.
Existing drafts 1–5, CI and upstream issue status are unchanged.

Next activation is F24 **source-only raw-key handoff specification**, including
actual context, exclusive closing owner, AutoMemory/native phase/NULL recipes and
preservation of both ManuallyDrop scan adapters. F25 implementation waits for that
reviewed contract and upstream205 draft visibility. No F24 implementation or new
backlog is started in this activation.
