# F17 — Byte-preserving call-result conversion

Default conversion now returns `BulkString(String)` for valid UTF-8 and
`StringBuffer(Vec<u8>)` for invalid bytes. Both emit with length-delimited bulk
framing. This adapts Mikkel Snitker's upstream PR 93 policy to today's typed
reply conversion and existing binary fallback, addressing upstream issue 234.
Leaves upstream issue 234 open; the fork PR stays draft for CI/remote handoff.

## Implementation and author migration

The value and key arms copy native bytes once while the reply is alive. Their
private pure helpers consume that Vec with `String::from_utf8`; failure recovers
it with `into_bytes`. Key variants remain String for valid text and byte
BulkString otherwise, preserving Eq/Hash/Ord and explicit key lookups. Integer,
bool and unsupported-key policy are unchanged.

```rust
fn call_string_value(bytes: Vec<u8>) -> RedisValue {
    match String::from_utf8(bytes) {
        Ok(text) => RedisValue::BulkString(text),
        Err(error) => RedisValue::StringBuffer(error.into_bytes()),
    }
}
```

The pure buffer test observes pointer/capacity preservation for nonempty valid
and invalid values/keys. This supports buffer reuse, not measured allocator
requests, latency, throughput, native refcounts or complete memory safety.
One Rust-owned copy is still allocated through the SDK allocator. No new public
API, dependency, unsafe block, native retain/adoption protocol or stored Context.

Signatures, strict text extraction, existing typed `call_ext` byte access and
explicit module status/error variants remain. Implicit valid-text results change
from SimpleString to BulkString, including a forwarded native `SET` status OK.
An author matching only SimpleString must accept BulkString. Explicit
SimpleString/Static/REDIS_OK retain their old policy; arbitrary manual status
payloads are not certified by this repair. Native public string tags do not
retain the original bulk/status distinction, so this normalizes rather than
reconstructs it. No whole-protocol compatibility promise follows.

`version_from_info` accepts the new bulk text and the old explicit text.
`is_enterprise_internal` similarly accepts bulk text while retaining its native
string/strict branch. A private pure Enterprise-marker parser enables synthetic
text tests, not Enterprise product coverage. The first pure test exposed that
missing-version text yielded an empty capture list which was indexed; a local
capture-length guard now reaches the intended Err. The shared regex helper and
existing numeric parse/overflow policy remain separate. No historical invalid
native version or wire path was executed.

`module_config_get` retains its existing policy and gains selected bulk-framing
regressions. Its fixture uses binary RedisString creation in a custom text getter,
so embedded NUL does not pass through an unrelated CString producer. Existing
DUMP/example expectations now compare exact bytes with typed native DUMP and
accept text or byte bulk variants without assuming serialization's UTF-8 class
from the input value. F01 exclusions remain historical; their current reason
points to this separate corrected validation.

## Validation checkpoint

Caller-owned execution results below are separate from the approving independent
Due/Claude source review. The caller checked the actual receipts and effects:

- 35 pure tests pass: 29 library plus six dependency checks, including five new
  production-policy/buffer/key/parser tests. No Redis APIs or fake Contexts.
- Ten metadata-only compiler controls pass against checkout and ten against
  the actual archived SDK rlib. Typed bytes/strict text/key/owned conversion
  and explicit status remain usable; paired E0515/E0505 controls reject escaping
  a byte view or dropping its owner while borrowed. No function/native API runs.
- All 36 examples build debug/release at the selected 7.2 API floor, Rust 1.94;
  all-target Clippy with warnings denied, fmt and 18 doc tests pass (four ignored).
- Four native host/profile cells pass on Redis 7.2.8/8.10.2 debug/release. The
  corrected isolated module covers empty/ASCII/Unicode/NUL/CR/LF/CRLF/non-UTF8/
  long bytes, ECHO/GET/MGET/DUMP, actual bulk framing, later commands on the same
  connection, ordinary nil/error, wrapped versus explicit status, typed native
  map/set key conversion and owned copies used after root disposal in the same
  legal callback phase. No unsupported-key/panic or general RESP3 audit.
- Forced version RMCall and actual OSS detection pass, with absence of the
  mismatch-error log checked. Config forwarding matches native CONFIG GET bytes
  with bulk framing, including valid-text NUL/CRLF, using legal isolated storage.
- Sixteen corrected call/DUMP/binary/version/name integration executions pass
  across both hosts/profiles. The existing explicit SimpleString version control
  stays; the pure parser adds BulkString coverage.
- Four fresh archives, three external consumers/six profile builds pass, plus
  registration link tests. Forty-seven production/manifest-origin source files
  match current sources byte-for-byte. Task-local dependency/compiler caches are
  reused; registry/archive/consumer/lock graphs and source checks are fresh.
  Archive runtime is skipped; owned SDK/reference fixtures supply native evidence.
- Six isolated JMESPath/event-stream/stream-filter builds pass with fresh
  metadata-f17 and source hashes; 12 selected host/profile flows pass. JMESPath
  covers GET/MGET-backed query and XRANGE text, event-stream covers registry/stat
  text and default entry output, and stream-filter covers typed binary XRANGE
  with NUL/non-UTF8/CRLF. No original module migration or full application/optional
  Search/JSON/TimeSeries/replication/Cluster/platform claim.

Passing raw receipts: `target/f17/pure-final.log`, `compiler/report.json`,
`archive-compiler-final/report.json`, `runtime-debug/report.json`,
`runtime-release/report.json`, `integration/report.json`, `packages-final/report.json`,
`archive-source-match-final.json`, `references/report.json` and
`reference-runtime/report.json`. Compiler/runtime/report artifacts have their
own source and binary hashes. The initial missing-version pure failure and
configuration-fixture macro grouping compile failure remain retained separately.
No production macro repair is included; explicit empty groups suit the fixture's
existing tuple grammar. Owned rebuildable Rust incremental cache was reclaimed
for disk headroom, with the exact record in `target/f17/disk-cleanup.json`.

## Independent review and final caller checks

Due/Claude approves the reviewed implementation/fixtures, true process exit 0.
Request/result/answer, all 163 trace records and offline summary validate; 36
activities match with zero failures/drops/gaps. Reviewed inputs stayed unchanged
through completion. The source reviewer did not execute checks, inspect raw
runtime/archive receipts or verify remote state; those are caller checks.

Two nonblocking refinements were applied afterward: whole-buffer equality in
all four long value/key UTF-8 branches, and a comment explaining the strict-text
configuration loop's excluded invalid inputs. Production code stayed unchanged;
the runner's Python AST stayed unchanged. Final 35 pure tests and fmt pass. Fresh
archives/consumer builds and ten archived compiler controls were repeated for
final source bytes, with 47 production/manifest-origin files matched. Native and
reference receipts retain their as-run hashes preceding these test/comment-only
refinements; no unchanged native matrix or model review was replayed. See
`target/f17/review/followups.json` and `verification-final.json`.

No allocator request count, zero-copy, throughput or whole-FFI safety measurement
is claimed. Buffer pointer/capacity observations establish only the helper's
owned-buffer reuse. Config tests preserve strict text; synthetic Enterprise text
and an actual OSS result are not an Enterprise host test. Original reference
heads/status and 135 Rust hashes remain unchanged.

Next recommendation is F18, a source-only scope for the already deferred
`LoadConfigs` result/order/error-propagation contract. Investigate actual native
and macro/manual author paths, migration choices and corrected validation gates
before implementing. This is a later chunk; CI remains deferred.

## Scope, credit and remote process

Reviewed F16 specification is the scope; unrelated reply/error/status/verbatim/
bignum/promise/native-owner/context/GIL/thread/alias/reentry/panic, accepted
userdata reclamation, LoadConfigs, CI and package/MSRV/support/release policies
remain separate. Selected macOS arm64/libc evidence is not a support matrix.

Before code, fresh overlap found four fork drafts and 45 upstream PRs with no
competing fork issue-234 scope. Empty kickoff `7be99232c57a1a362a23b303562aa150fad3e583`
and baseline `ea0177207ae8ef27352169ee552725b2dfeac9f9` were pushed, then
[draft PR 5](https://github.com/joshrotenberg/redismodule-rs/pull/5) existed before
implementation. Baseline upload includes the local F11 merge and F12–F16 history
under issue-visibility authorization. Fork master is not pushed. GitHub merges,
releases and deferred CI remain human-owned; drafts 1–4 stay open.

Credit Mikkel Snitker's [PR 93](https://github.com/RedisLabsModules/redismodule-rs/pull/93),
refreshed head `f1ec666ecc6891e11130df853e8b80c758df5230`; its historical one-line
policy change is adapted, not blindly applied. Refreshed iddm PR 375 at
`68a0705b7f3d95a893eda19390e4dff51d3a4263` has adjacent broad FFI/status changes
and is excluded. Issues 134/246 are broader background; leaves them and 358/473
open, with issue 471 excluded. Inherited BSD and F02 PR476/F09 PR487/488 credit
remain in their contribution ledgers.
