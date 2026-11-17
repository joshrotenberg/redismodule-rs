# HTTP external authentication

An experimental SDK consumer with an isolated Cargo workspace. It pins SDK
`a2943e7870e00da01093759bc468978e85e01dc7` and its opt-in asynchronous auth
feature. It leaves #217 open. Caching, stable runtimes, real identity services,
deployment TLS and mTLS remain outside this example.

Only the accepted Linux x86_64 Redis 8.12-rc3 executable can prepare the SDK
runtime. Its source and build contract are in
[`accepted-runtime.json`](../../.github/experimental-auth/accepted-runtime.json).
An API floor selects SDK compile-time APIs; selecting 6.0 or 7.2 does not add
runtime support for those servers.

## Bootstrap

Use a disposable password-protected server and fake credentials. Keep an
authenticated native administrator connection open. Restrict these module
commands to administrators through Redis ACLs. The `ADMIN` command flag alone
does not grant or restrict command access.

The module loads **inactive**. Native password authentication is explicitly
allowed while inactive or configured. The following commands run on the
existing native administrator connection; `127.0.0.1:12345` must name the
test's own loopback verification stub.

```text
external_auth.configure 127.0.0.1:12345
external_auth.map alice reader
external_auth.map bob writer
external_auth.prepare
external_auth.activate
```

Configuration creates two module-owned users. `reader` has `+ping +get` and
`~http:*`; `writer` adds `+set`. Up to eight configured usernames map to those
profiles. No provider response creates a user or supplies an ACL rule. A partial
configuration failure retains created handles; run `external_auth.drain`
before configuring again.

Preparation is explicit ordinary-command work and may synchronously hash the
native executable. Missing preparation, incomplete configuration and unsupported
runtimes refuse activation before any worker or provider request starts.

Once **active**, unmapped usernames and all operational failures deny. Active
authentication has no automatic fallback. The existing native administrator
connection remains authenticated and can issue administrative commands. Fresh
native administrator authentication is denied while active unless that username
is configured. This example's mapped users have the limited profiles above.
Redis owns AUTH and HELLO responses, including HELLO protocol negotiation.

## Local verification protocol

The endpoint is a numeric loopback `SocketAddr` with a nonzero port. IPv4 and
`[::1]:port` forms work; hostnames, external addresses, URLs and custom paths
are rejected. Requests use HTTP POST `/verify`, `Content-Type: application/json`
and `Connection: close`:

```json
{"version":1,"username":"alice","password":"fake-allow"}
```

The only successful response is status 200 with exact content type
`application/json`, no content encoding, and a version 1 JSON object:

```json
{"version":1,"allowed":true,"username":"alice","role":"reader"}
```

Success must match both the requested configured username and its configured
profile. Unknown or duplicate fields, unknown roles, malformed JSON, non-200
status, redirects, compressed responses, oversized bodies and transport errors
deny with the same public WRONGPASS outcome. Provider errors are never formatted
or logged. Dependency logging is disabled at compile time for this isolated
dependency graph; native Redis authentication audit remains enabled.

Usernames contain 1 to 64 UTF-8 bytes without control characters. Passwords
contain 1 to 256 UTF-8 bytes. Binary credentials are explicitly unsupported and
denied before an asynchronous copy. The callback borrows credentials, reserves
a worker slot, then explicitly calls `copy_for_async`. The worker serializes a
separate bounded JSON copy and overwrites that buffer after sending. Neither
library, kernel nor allocator copies are guaranteed erased. Use fake secrets.

Requests are limited to 2048 JSON bytes, response headers to 4096 bytes and the
decoded response body to 1024 bytes. The HTTP client uses an 8192-byte input
buffer and a 4096-byte output buffer. These limits do not describe total
allocation or wire bytes. Proxies, environment proxy inheritance, redirects,
compression and idle connection pooling are disabled. A fixed-address resolver
performs no DNS or helper-thread creation.

The client configures a 100ms connect timeout, a 400ms per-read timeout, a 100ms
per-write timeout and a 500ms global transaction budget. Cancellation and the
application deadline are checked before and after verification. The SDK's 200ms
auth deadline may win first; a later provider result cannot authenticate that
expired client. Timeouts are I/O deadlines, not a wall-time guarantee for thread
return, scheduling, JSON parsing or cleanup. A partial blocking write can consume
multiple socket waits. `Agent.shutdown` interrupts connected socket I/O but a
connect in progress remains subject to its connect timeout.

## Drain and unload

`external_auth.status` returns `[phase, retained_slots, started_workers]`.
The started-worker diagnostic may persist if the native loader retains the
image across logical module unload and reload.
There are at most four slots across unstarted deferred reservations and retained
thread handles. Finished handles still consume slots until an administrator
calls `external_auth.reap`. This backpressure is intentional. No Redis context
or native string moves to a worker, and no application mutex covers network I/O.

`external_auth.drain` enters **draining**, denies new authentication, marks
application cancellation, shuts down the provider client and asks the SDK to
cancel blocked auth. It only joins handles whose `is_finished` predicate is
true. It returns `ERR external auth drain pending` while a reservation, running
thread or ownership fault remains. Repeat the command until it returns `OK`.
No join waits for an unfinished provider thread. Token completion alone never
proves that a worker stack has returned.

The command then reaps SDK cleanup, disconnects SDK-owned client identities and
removes the module users. It returns to **inactive**, explicitly allowing native
password fallback again. Reconfiguration creates a fresh client because client
shutdown is permanent across its clones. A poisoned registry or panicked worker
latches refusal. It requires server shutdown rather than forced native-accounting
repair. Unload refuses until application ownership is empty; the SDK separately
refuses pending tokens, native work or unknown ownership.

## Checks

Use Rust 1.94.0 and repeat compile/unit checks with Rust 1.87.0. Keep this target
directory separate from the SDK's target directory.

```sh
cargo fmt --manifest-path examples/external-auth/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path examples/external-auth/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path examples/external-auth/Cargo.toml --tests
RUSTDOCFLAGS="-D warnings" cargo doc --locked --manifest-path examples/external-auth/Cargo.toml --no-deps
cargo check --locked --manifest-path examples/external-auth/Cargo.toml --all-targets --no-default-features --features floor-7-2
```

Unit tests own ephemeral loopback listeners and use fake credentials. They cover
protocol binding, malformed and oversized responses, redirects, disabled proxy
inheritance, UTF-8 limits, read/global timeout, cancellation and retained-worker
accounting. Native CI reuses the existing pinned RC3 build/fixture jobs at both
API floors, then builds this separate consumer and explicitly runs `native.py`.
The script refuses positive execution without `EXTERNAL_AUTH_POSITIVE=1` and
the accepted server hash. It records native/module/lock/source identities,
retains loaded module bytes and waits for every owned child and stub thread.
Older-server refusal CI verifies zero started workers and provider requests.
