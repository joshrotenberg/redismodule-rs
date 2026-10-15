# F16 — Call-result string conversion scope

Recommend a narrow F17 repair: default call-result conversion should produce
`BulkString(String)` for valid UTF-8 and `StringBuffer(Vec<u8>)` otherwise,
reusing one owned byte copy. Preserve the current text/byte key variants and
explicit text extraction. This fixes the implicit wire boundary with fewer
downstream migrations than turning every value into a byte-vector variant.
It is a reviewed proposal, not an implemented or compiled contract.

Baseline: local master `1a5e437cfa7058a97bc163444e157d4ccdf1d007`, after F15.
F16 is source-only; no SDK implementation, compilation, benchmarks, tests or
native execution. All original module directories remain unchanged.

## Existing contract and the precise gap

| Source | Current behavior / consequence |
|---|---|
| `src/context/mod.rs:432–435` | `Context::call` converts successful native replies through `RedisValue::from(&reply)`; native errors remain `Err`. `call_ext` can return a typed `CallResult` without that conversion. |
| `src/context/call_reply.rs:19–33` | `as_bytes` borrows native bytes; `to_string` copies and strictly validates UTF-8. Binary access already exists. Views must not outlive the reply/native phase. No new borrowing or native-owner API is needed. |
| `src/redisvalue.rs:236–239` | Valid UTF-8 becomes `SimpleString`, including text containing NUL/CR/LF. Invalid UTF-8 already becomes `StringBuffer`, but the first owned UTF-8 attempt is discarded before copying again. |
| `src/context/mod.rs:523–547` | Explicit SimpleString emission uses `CString::new(...).unwrap()` and a null-terminated native API; BulkString/StringBuffer use pointer plus length. NUL rejection here is source-observed, not reproduced. |
| `src/redisvalue.rs:209–225` | String map/set keys become `RedisValueKey::String` or byte `BulkString`; both emit as bulk via `reply_with_key`. Eager `map_or` builds a second byte copy even when text succeeds. Preserve variant identity, derived Eq/Hash/Ord and supported integer/bool keys. |
| `src/redisvalue.rs:241–263, 284–310` | Nested values use the same conversion. Unhashable keys retain their existing failure/panic policy. Borrowed CallResult errors become byte values, unlike Context::call's Err route; this separate policy stays. |
| `src/configuration.rs:589–607` | `module_config_get` forwards native CONFIG GET through the same conversion. No consumer policy rewrite is needed; text values will emit as bulk, matching native CONFIG GET's string framing. Include a selected regression check. |

Both retained native versions classify simple and bulk call replies as
`REDISMODULE_REPLY_STRING`; the public typed tag does not preserve that wire
distinction. `callReplyGetString` explicitly returns borrowed, length-delimited,
non-NUL-terminated data. Copy before disposing the root reply. Native
`ReplyWithStringBuffer` emits bulk bytes on both examined hosts.

Native simple-string emission differs: Redis 7.2.8 writes status text directly;
pinned 8.10.2 uses `addReplyStatusSafe`, replacing CR/LF with spaces. Neither
establishes byte-preserving forwarding of arbitrary string data. The SDK's
CString step still precedes both. No old wire/panic path was run. This proposal
normalizes implicit string results to bulk rather than claiming to reconstruct
the original status/bulk type from the public call-reply tag.

Primary retained source locations: 7.2.8 `module.c:3075–3087,3317–3325,6007–6012`
and `call_reply.c:89–108,299–315`; 8.10.2 `module.c:3210–3220,3450–3458,6605–6611`,
`call_reply.c:69–88,279–297`, `networking.c:876–881`. The new 8.10 files were
fetched at commit `498ecd0d6d007db11ddb3aea9428552598a78622`, Git-blob verified,
and hashed. These are source observations, not runtime evidence or support policy.

## Options and selected proposal

| Option | Benefit | Cost / disposition |
|---|---|---|
| Keep default; add a byte-preserving alternate conversion | Opt-in downstream migration | Leaves ordinary Context::call forwarding on the reported boundary; every author must find the alternate API. Do not select. |
| Every implicit string becomes StringBuffer | One uniform byte variant; text is explicit | Breaks JMESPath's text-pattern parsers throughout several files and changes valid-text key identity if applied indiscriminately. A broader API redesign, unnecessary for this repair. |
| **Valid text → BulkString, invalid text → StringBuffer; retain key variants** | Existing byte support and text pattern compatibility; all implicit values emit length-delimited bytes; discard no failed UTF-8 buffer | Deliberate SimpleString→BulkString variant/wire change for valid text, including wrapped status replies. Select for F17. |

Proposed value arm, **not implemented or compiled**:

```rust
match String::from_utf8(reply.as_bytes().to_vec()) {
    Ok(text) => RedisValue::BulkString(text),
    Err(error) => RedisValue::StringBuffer(error.into_bytes()),
}
```

Use equivalent one-buffer classification for keys, mapping Ok to
`RedisValueKey::String` and Err to `RedisValueKey::BulkString`. A small private
production helper may classify an owned Vec so pure tests exercise the actual
code and prove buffer reuse. No new public enum variant, method, dependency,
unsafe block, pointer constructor or stored Context is needed. The owned copy
still allocates through the SDK allocator; no new native retain/adoption protocol
is required.
`String::from_utf8` consumes the Vec; `into_bytes` recovers it on failure.
This is a source-level copy argument, not a measured allocation/latency result.

Retain `StringCallReply::{as_bytes,to_string}` and existing
`TryFrom<RedisValue> for String`: strict UTF-8 still rejects invalid bytes.
Explicit `SimpleString`, `SimpleStringStatic`, `REDIS_OK`, manual status/error
helpers and unrelated reply families remain unchanged. An author deliberately
returning an explicit simple string keeps that separate contract; this repair
does not certify arbitrary manual SimpleString emission.

## Required direct migrations and before/after

SDK helpers must follow the changed successful text variant:

- `Context::version_from_info` currently accepts only SimpleString. Add BulkString
  while retaining its previous accepted text behavior and parser policy.
- `is_enterprise_internal` currently accepts SimpleString/BulkRedisString. Add
  BulkString, preserving the existing strict native-string branch and mismatch
  behavior. A pure parser check can cover synthetic `rlec_version:` text;
  an OSS host is not proof of Enterprise product coverage.
- Correct the selected `call` example/DUMP tests and source docs. DUMP's serialized
  bytes are not guaranteed to share the input value's UTF-8 classification.
  Compare exact bytes against native DUMP; do not infer its variant from input
  text, compression or checksum contents. F01 exclusions remain historical;
  update selection reasons only after corrected checks pass.

Author match migration (proposed):

```rust
// Before: matching only the implicit text variant.
RedisValue::SimpleString(text) => parse_text(&text),
// After: accept the new bulk text; retain compatibility with explicit text.
RedisValue::SimpleString(text) | RedisValue::BulkString(text) => parse_text(&text),
```

The four references inform this choice; the inventory covers 218 tracked Rust
files (83 SDK, 135 original references), hashes and selected literal searches.
It includes comments, client types and embedded Lua false positives; it is not
an exhaustive AST/callgraph audit or an exact native-call count.

| Reference | Selected actual use | Migration / validation consequence |
|---|---|---|
| JMESPath | `command.rs:83–194` GET/JSON.GET/MGET; `search.rs`, `streams.rs`, `timeseries.rs` parse SimpleString **or BulkString** | Selected successful-text patterns already accept the proposal. An all-StringBuffer alternative would need parser changes. Keep domain parsers unchanged; validate isolated ordinary GET/MGET query and stream text/JSON flow. Optional Search/JSON/TimeSeries host coverage remains separate. |
| Event stream | `commands.rs` SMEMBERS values use strict String conversion; `capture.rs:1340` CONFIG GET also converts to String; other selected calls consume typed replies/integers | Strict conversion already accepts BulkString. Check isolated registry/stat text and existing entry-format flow; no original migration imposed. |
| Stream filter | `native.rs:22` returns typed CallResult; `reply.rs:116–121` copies StringCallReply bytes directly | Existing typed byte path remains; use a selected isolated binary stream/filter flow. Its historical comment about wrapper `to_string().unwrap()` is not evidence of today's SDK, which has a byte fallback. Do not rewrite the original. |
| Carolina | Selected module search finds no direct Context call conversion; many other hits are Lua/client/explicit status replies | No selected caller migration; no full Carolina suite or unrelated client migration required. |

All isolated snapshots must carry F07/F09/F11/F13/F14 prerequisites where
applicable, retain fresh SDK graph/source/patch provenance, and preserve original
directories, domain behavior, defaults, command/replication flags and stored data.
Bulk status forwarding is intentionally observable: e.g. a wrapper returning
Context::call("SET", ...) changes from SimpleString("OK") to BulkString("OK").
Explicit module status replies still use their chosen variants. Document this
wire/result-kind change rather than promising complete protocol compatibility.

## Corrected-code gates for F17

| Gate | Required evidence; future work only |
|---|---|
| Pure production policy | Empty, ASCII, Unicode, NUL, CR, LF, CRLF, invalid UTF-8, and a longer mixed byte vector. Exact bytes and expected value/key variants; strict String success/error; valid-key Eq/Hash/Ord unchanged. Exercise a private production helper if introduced; record owned buffer identity/capacity through valid and invalid classification, with nonempty cases to make identity meaningful. No Redis APIs or fabricated native owners/Contexts. |
| Source/compiler/public API | Uninvoked metadata-only consumer functions for Context::call/call_ext, conversion, strict String and existing typed bytes. If showing a borrowed view recipe, paired positive/negative lifetime controls must fail for the intended diagnostic without constructing or executing native pointers. No new public borrowed API is proposed. |
| Native byte/wire behavior | Corrected isolated legal command fixtures on owned 7.2.8/8.10.2 hosts, debug/release. GET/ECHO, nested MGET/array and DUMP exact bytes; empty/Unicode/NUL/non-UTF8/CR/LF/CRLF, nil, ordinary error, wrapped SET status and explicit module status controls. Check actual bulk wire framing as well as decoded bytes, then another ordinary command on the same connection. Never route edge bytes through historical SimpleString emission for comparison. |
| Native ownership / direct keys | Convert while root is alive, dispose it, then use the owned value/key in the same legal command phase. Selected HGETALL/SMEMBERS typed RESP3 map/set replies via existing call_ext options to cover direct key conversion; keys/values compared without relying on hash iteration order. No unhashable-key/panic or broad RESP3/blocking audit; pure/source checks retain unsupported policy. |
| SDK helper regressions | Pure version parser tests for old explicit/new bulk text and invalid input; retain `tests/integration.rs:701`'s explicit SimpleString control and add BulkString sibling coverage. Forced `get_redis_version_rm_call` on actual hosts; selected Enterprise-marker parser and OSS result with limitations stated. Selected module_config_get forwarding checks exact text bytes/bulk framing without changing its registration/default/LoadConfigs policy. No fake Enterprise host or arbitrary raw contexts. |
| Archives/references | Fresh source-verified archives and external public consumers; independent other-provider source review of implementation. Selected isolated JMESPath, event-stream and stream-filter builds/flows above, with exact bytes and unchanged domain results. Archive build-only results remain separate from native runtime. |
| Efficiency claims | Optional bounded corrected-code conversion window, after native reply creation and before response/decoding. Count only Rust requests/bytes if claiming allocation improvement; include value and key, valid/invalid/nonempty/empty inputs. Any baseline must be pure/source or valid ordinary paths, never historical invalid wire execution. Without such evidence say only one-buffer source mechanics; no zero-copy, total-memory or throughput claim. |

The implementation scope is the two conversion arms, the smallest testable
private policy helper if useful, the two SDK text consumers, focused fixtures/
docs and selected isolated caller consequences. No `From<String>` policy change,
new variant names, native ownership redesign, error/status/verbatim/bignum policy,
promise lifecycle, general alias/GIL/thread/reentry/panic audit, accepted userdata
reclamation, LoadConfigs repair, CI, MSRV/package/support/release choice.

## Overlap, attribution and next process

Fresh snapshots: four fork drafts, 45 upstream PRs, 55 issues. Directly related
[issue 234](https://github.com/RedisLabsModules/redismodule-rs/issues/234) reports
Context::call string forwarding. F17 should list it as scoped and leave it open
for the owner; no closing action belongs here. Broad historical issues
[134](https://github.com/RedisLabsModules/redismodule-rs/issues/134) and
[246](https://github.com/RedisLabsModules/redismodule-rs/issues/246) are background
on binary APIs, not promises to implement their entire requests. Leaves those
and upstream 358/473 open; issue 471 is outside this scope.

Mikkel Snitker's [PR 93](https://github.com/RedisLabsModules/redismodule-rs/pull/93),
observed head `f1ec666ecc6891e11130df853e8b80c758df5230`, changes the old
parse_call_reply string arm from SimpleString to BulkString. Credit that policy
contribution and issue report; the contemporary typed conversion and retained
binary fallback need a new adaptation rather than applying the old patch blindly.
[PR 375](https://github.com/RedisLabsModules/redismodule-rs/pull/375), iddm/head
`68a0705b7f3d95a893eda19390e4dff51d3a4263`, overlaps files with broad FFI/status
changes but its selected diff does not implement this string-conversion repair.
Do not duplicate that refactor. Retain inherited BSD notices and F02/F09 credit
ledgers (PR 476/487/488); no contribution is newly ported in F16.

F16 stays local on `docs/f16-call-reply-scope` with one source review and caller
checks, then the validated no-ff merge. Before F17 code, refresh all overlaps,
reconcile `review/f17-baseline` at post-F16 and
`fix/f17-call-reply-bulk-strings`, push the required baseline/empty kickoff and
open a scoped draft listing upstream issue 234 and PR-93 attribution/dependency.
The required baseline uploads the local F11 merge and F12–F16 ancestor history;
record that consequence under the existing issue-visibility/history authority.
If visibility/history authority is blocked, stop implementation and retain the
blocker. Never push fork master or merge/release on GitHub. Existing drafts 1–4
and CI remain held; no unchanged reviews/tests reopened.

One Due/Claude source review approves; five nonblocking documentation/evidence
refinements were checked and applied by the caller (checkpoint, config-forwarding
gate, excerpt ledger, line anchors and retained explicit-text parser control).
Request/result/answer, all 177 trace records and offline summary independently
validate; no dropped/gap/failed/unmatched activities. Reviewed inputs stayed
unchanged through completion; no second review of these doc refinements is
claimed. The caller separately checks sources, hashes, excerpts and final diff.

Read [compact evidence](evidence/f16-call-reply-conversion.json) and
`target/f16/checkpoint.json` before the next interval. All corrected-code gates
above remain pending until implementation; source review is not a runtime pass.
