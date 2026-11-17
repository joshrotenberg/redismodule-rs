# Module users and client identities

`RuntimeContext::create_module_user` creates an unlinked Redis ACL user owned by
this module's current load. Keep its `ModuleUserHandle` in module state. Cloning
or dropping the handle does not free a native user. Use
`Context::remove_module_user(&handle)` from an ordinary SDK command to invalidate
every clone and disconnect its clients. Explicitly revoke installed identities
and remove all created users before unloading. Unload refuses live SDK ownership
before user deinitialization and preserves existing handles and identities.
Successful empty-registry unload performs no native client or user cleanup.
Handles cannot resolve after removal or through a different module or load generation.

Creation and identity attachment require an active `redis_module!` module.
They return an error during initialization and unload. This avoids retaining
native resources after Redis rejects a load. Custom loaders do not acquire this
lifecycle binding through a public safe API.

Created users start disabled. Apply individual directives with
`set_module_user_acl`, or a complete transactional update with
`set_module_user_acl_string`. The safe wrapper requires Redis 7.4 or later,
verified with the actual native runtime version before setter FFI. The native
API exists on 7.0, but older implementations synchronously destroy Pub/Sub
clients while traversing a shared client list; lawful callbacks can invalidate
that iteration even when a different user is updated. Missing or unverifiable
runtime versions fail closed. There is no fallback to a batch of directives.
Invalid rules preserve the previous permissions. A lexical preflight rejects
unterminated quotes and invalid quote boundaries that the native setter can
otherwise report as successful empty updates. Valid input is passed unchanged. Errors omit ACL text and parser diagnostics
because both can contain passwords or hashes. Changing a user to `off` prevents
new attachments; use explicit removal or client revocation to disconnect
existing clients.

`Context::authenticate_client_with_user` and
`Context::authenticate_client_with_acl_user` attach an identity to the current
client. These are privileged operations: they do not verify a password.
Authorize the caller before exposing either operation in a module command.
Attachment requires a real client present in Redis's client-ID index. Internal
clients, including Lua's client, are refused before transferring subscriptions.
The ACL form requires a current SDK-owned client identity installed by an
ordinary admitted command. A native-auth installation alone cannot prove that
Redis still owns its callback: a suspended foreign notifier can overwrite that
callback while leaving a historical SDK record. Such records never authorize
a borrowed ACL account attachment.
An untracked connection, foreign native callback or pending registration is
refused before native account lookup or identity change. The caller may first
explicitly attach a chosen created user with `authenticate_client_with_user`,
then hand off to the ACL account. The SDK never inserts a hidden bootstrap user.
This precondition ensures the old native callback is the known SDK trampoline:
a foreign callback could otherwise delete an incoming borrowed ACL account
before native attachment finishes. Per-client pending admission also refuses
nested attachment from a lawful raw prior callback, including attempts with a
different created user. Pending registrations never count as installed.
A delivered native overwrite consumes the old SDK installation, so that
connection must establish command-side ownership again. Every native-auth
installation is conservatively classified as historical for ACL admission,
including after a successful ACL handoff.

The ACL form then resolves the name at attachment time and rejects missing or
disabled accounts. It never retains a lookup-derived native user handle. A native
rejection returns an error without replacing the previous subscription.

Both forms install native cleanup even when the consumer callback is `None`.
An optional `ClientChangeCallback` runs once on reauthentication (including the
same user), disconnect or explicit revocation. Capture owned `Send`
state. Redis provides a `ClientId` and no context to this callback. It runs with
the Redis lock held, so it must not reacquire that lock. Captures remain `Send`;
there is no promise of delivery on the registration thread. When a notification
occurs during an SDK native operation, the SDK defers consumer code until the
outer native operation returns. This includes the common native call boundary
used by `call`, `call_ext`, blocking-call variants and `yield_now`. Notifications outside such
operations deliver immediately. Some native ACL changes schedule client
closure for a later event-loop turn, so their notification can arrive after
the setter and any scoped caller context have ended. Consumers must own all
required cleanup state and cannot assume a context remains available.
Cookies remain valid through native callback
return and consumer execution. Client-affecting mutations from cleanup are
refused, including through a borrowed scoped command context. The SDK
removes subscription ownership before calling consumer code and holds no
registry lock across native calls or callbacks. Created-user native leases also
refuse reentrant removal or ACL mutation from lawful raw server-event hooks
while attachment or a setter still borrows that user. These leases end before
deferred consumers run. Callback execution and callback
destruction on every preflight/refusal path are contained without adding panic payloads to SDK logs; the configured
process panic hook still runs. As elsewhere in Rust, a second panic during
unwinding can abort the process.

Removal, complete ACL-string updates, attachment and revocation are `Context`
methods, but the Rust type alone does not authorize them. They require this exact
SDK command-dispatch invocation on an indexed connection, Redis's actual main
thread, no active SDK native operation and no executing auth consumer. Commands
registered `allow-busy` are refused because they can run during another module's
Yield. Unknown callback metadata and callbacks shared with any `allow-busy`
registration are refused. The actual invocation name must also match an SDK
registration for that callback, including the subcommand name. A raw alias
reusing a known callback address, or an unrecorded renamed invocation, does not
inherit that provenance. Names compare ASCII case-insensitively. Event, worker,
blocked-reply, timeout, internal-call
and scoped native-callback reentry paths do not acquire this capability.
Native `LOADING` and `ASYNC_LOADING` flags are also refused: loading-time Yield
can process events without the usual busy-command exclusion. A missing native
context-flags API fails closed. These checks precede registry invalidation and
client-affecting native FFI.
Thread detection supports Linux and macOS; client operations are refused elsewhere.
Creation and single-directive updates need a real runtime lock; their native
paths do not close clients or invoke user-change callbacks.

`Context::deauthenticate_and_close_client` additionally requires the target's
current-load SDK subscription. Missing, foreign, pending or already-closing
ownership is refused without changing its identity. When a retained installation
has a conclusively absent native client ID, an admitted command instead
reconciles its notification and releases its cookie. An indexed client or an
unknown lookup result never permits this retirement.
Closing admission lasts through native return, including native failure.
A client ID alone does not grant revocation of an arbitrary connection. A
retained historical installation grants administrative close authority for
that connection even if a foreign module later replaced its native identity.
It does not certify the connection's current identity or callback owner.

These checks cover legal raw callbacks inside SDK-guarded calls and Yield.
Unsafe raw FFI that bypasses those scopes or forges invocation provenance does
not establish this safety boundary. No global cross-module phase protocol or
unchecked safe helper is provided.

Unload refuses new auth work before user deinitialization. A refused deinit
restores the active lifecycle. An unload with any created user, live or pending
subscription, deferred consumer, executing cleanup or in-flight auth ownership
is refused before deinit. Drain explicitly from an admitted command: revoke
all identities with `Context::drain_client_identities`, then remove created
handles. The drain enumerates the SDK registry, including installations without
consumer callbacks and installations whose created user was already removed.
Keep user handles until removal succeeds. If a foreign notifier erased an SDK
callback, its cookie and capture remain retained while the native ID is indexed.
Native closure can finish after drain returns a pending error; run drain again
from a later ordinary command. Once disappearance is verified, this reconciliation
delivers cleanup once outside registry locks. It does not promise delivery at
the time of the lost native notification. Retry unload after cleanup completes. No native user free runs from a Rust destructor.
Dropping the last handle while a user is still registered loses the caller's
removal capability and keeps unload refused, so retain handles until removal.
The example's `auth_users.drain` command demonstrates explicit cleanup.

`RuntimeContext::client_certificate` copies an allocated native PEM string to
owned bytes and releases it using its actual producer context, including with
AutoMemory enabled. `None` is ambiguous: missing client, non-TLS transport or no
peer certificate. `Context::redact_client_command_argument` redacts a command
argument from MONITOR and SLOWLOG. Position zero and out-of-range positions fail.
Redact secret arguments before processing them. The example's `acl` and `rule`
commands require redaction. `rule` requires Redis 7.0 and `acl` requires 7.4,
although the underlying single-directive API is available on 6.0. Its fixed `defaults` helper contains no
secret arguments and works on the older runtime.

User creation, single-directive ACL updates, attachment, revocation and peer
certificates require Redis 6.0 APIs. Redaction requires Redis 7.0. Safe full ACL
strings require verified Redis 7.4 or later, beyond the native API's 7.0 floor. Missing APIs return capability errors at every SDK
compile-time floor; the default floor is unchanged.

For the administrative example, an explicit handoff is:

```text
auth_users.create service
auth_users.defaults service
auth_users.attach service none
auth_users.attach_acl existing-account track
```

`defaults` grants broad permissions for the example. Applications must choose the
created identity's permissions and authorize the caller before the handoff.
A rejected ACL handoff preserves the selected created identity. Run
`auth_users.drain` before `MODULE UNLOAD auth_users`.

See [auth_users.rs](../examples/auth_users.rs). The example's commands are an
administrative demonstration and do not implement application authorization.

## Synchronous authentication callbacks

Declare `auth: authenticate,` in `redis_module!`, after `init`, `deinit` and
`info` when present, before `commands`. The handler is a function path:

```rust,ignore
fn authenticate(ctx: &AuthContext<'_>, request: AuthRequest<'_>) -> AuthDecision {
    if request.username() != b"service" {
        return AuthDecision::NotHandled;
    }
    if request.password().as_bytes() != b"example-only-password" {
        return AuthDecision::Deny(PublicAuthError::default());
    }
    let user = { USERS.lock(ctx).as_ref().cloned() };
    match user {
        Some(user) => AuthDecision::Accept(AuthTarget::module_user(user)),
        None => AuthDecision::Deny(PublicAuthError::new("Identity is not configured")),
    }
}
```

The password comparison above is a stub. Applications supply their own
verification. An infrastructure or configuration failure must deny the attempt.
Only explicit `NotHandled` continues to another module hook and then Redis ACL
password authentication. Redis tries hooks in newest-registration-first order.
The declared hook requires Redis 7.2 and its companion APIs at runtime; a missing
API fails module load by name at every compile-time floor. The SDK default floor
is unchanged. Registration happens after successful init and has no dynamic
closure owner or runtime unregister operation.

`AuthContext` supplies only `client_id` and a `RedisLockIndicator` implementation
for module-owned `RedisGILGuard` state. It cannot become an ordinary `Context`
or `RuntimeContext`, reply, call commands, yield, block clients or mutate users.
Create/configure users explicitly after load in an administrative command, then
keep their owned handles in module state for the auth handler. The callback
never creates an implicit bootstrap identity. Before unload, stop accepting new
module identities and explicitly drain installed clients and created users from
ordinary commands. A live auth callback itself prevents unload even before its
first attachment. Empty-registry unload performs no client-affecting cleanup.

The native callback boundary is distinct from command mutation admission. It
checks the active load, actual main thread, indexed client and native loading
flags before calling the handler. Nested SDK native operations, executing auth
callbacks/consumers and LOADING/ASYNC_LOADING states are conservatively denied.
This includes auth reached during an SDK `Yield`; it never weakens the public
command provenance rules. Unsafe native calls must obey their native contracts.

`Accept` requests a target; successful native attachment establishes success.
A removed, disabled or foreign handle, missing ACL account or other attachment
failure produces a fixed handled denial. `AuthTarget::acl_user` retains the
command-installed identity precondition described above. It cannot authenticate a
fresh untracked client through trusted ACL-name mapping. Use a created target
or deliberate Redis ACL password fallback for that connection. Optional
`AuthTarget::on_changed` cleanup follows `ClientChangeCallback` ownership and
execution rules. Every installation has native SDK cleanup even without it.

Redis writes both AUTH and HELLO replies, including HELLO protocol negotiation,
metadata and SETNAME. The handler returns a decision and writes no reply. A
failed reauthentication preserves the prior session identity according to
Redis's native behavior; it is not an implicit deauthenticate-and-close request.

Credentials are binary slices borrowed from Redis for the invocation. `Password`
has no cloning, formatting or owning native-string conversion. `as_bytes` is
explicit; any copy or logging performed by the consumer remains its own action.
There is no owned-password or asynchronous completion API in this delivery.

Default denial uses Redis's WRONGPASS response. `PublicAuthError::new` accepts
explicit static public text; CR, LF and NUL become spaces, and the error code
remains WRONGPASS. Custom errors transfer one manual native string reference to
Redis, which frees it after writing AUTH/HELLO's response. A handled denial adds
one ACL auth-log attempt using the supplied username and fixed `module-auth`
object only after a private fixed `CONFIG GET acllog-max-len` read positively
verifies retention. The read uses RESP2 and the no-writes call flag, owns its
reply through strict parsing, and contains no credentials. Native command filters
can intercept it; a read lease bounds nested AUTH refusal without another read.
Zero retention, an unavailable API, a read error or an unknown reply skips audit
silently while authentication remains denied. This avoids native implementations
that duplicate the audit object before returning early with retention disabled. Passwords, provider errors, ACL text and panic payloads are never added
to SDK errors, audit objects or logs. `NotHandled` adds no module-denial audit;
the eventual handler owns its result. Native ACL log aggregation and retention
settings still apply.

An auth panic produces fixed failure text and a fixed SDK diagnostic. The SDK
does not use the generic payload-logging panic adapter here. Rust's configured
process panic hook runs before containment, so an application hook or its own
panic/log messages can still disclose credentials. Containment requires
`panic = "unwind"`; a second panic while unwinding can abort the process.
Native valid AUTH and HELLO AUTH paths redact their credential arguments from
MONITOR and SLOWLOG before invoking hooks. This is not a privacy guarantee for
malformed forms that Redis rejects before dispatch.

The [auth_callbacks example](../examples/auth_callbacks.rs) uses fixed test
credentials and bounded permissions:

```text
auth_callbacks.prepare
AUTH stub stub-password
GET auth:key
```

Run setup and `auth_callbacks.drain` from a separate authorized administrative
connection; the selected stub identity cannot execute them. Then run
`MODULE UNLOAD auth_callbacks`. Authentication-specific blocking/completion is a
separate API and is not provided here.
