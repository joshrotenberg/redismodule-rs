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
The ACL form requires a current, actually installed SDK-owned client identity.
An untracked connection, foreign native callback or pending registration is
refused before native account lookup or identity change. The caller may first
explicitly attach a chosen created user with `authenticate_client_with_user`,
then hand off to the ACL account. The SDK never inserts a hidden bootstrap user.
This precondition ensures the old native callback is the known SDK trampoline:
a foreign callback could otherwise delete an incoming borrowed ACL account
before native attachment finishes. Per-client pending admission also refuses
nested attachment from a lawful raw prior callback, including attempts with a
different created user. Pending registrations never count as installed.
A lawful native overwrite consumes the old
SDK installation, so that connection must establish ownership again.

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
actually installed current-load SDK subscription. Missing, foreign, stale,
pending or already-closing ownership is refused without changing its identity.
Closing admission lasts through native return, including native failure.
A client ID alone does not grant revocation of an arbitrary connection.

These checks cover legal raw callbacks inside SDK-guarded calls and Yield.
Unsafe raw FFI that bypasses those scopes or forges invocation provenance does
not establish this safety boundary. No global cross-module phase protocol or
unchecked safe helper is provided.

Unload refuses new auth work before user deinitialization. A refused deinit
restores the active lifecycle. An unload with any created user, live or pending
subscription, deferred consumer, executing cleanup or in-flight auth ownership
is refused before deinit. Drain explicitly from an admitted command: revoke
owned client IDs, then remove created handles. Retry unload after pending
notifications have completed. No native user free runs from a Rust destructor.
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
Native AUTH/HELLO callback registration and asynchronous authentication completion
are separate APIs and are not provided here.
