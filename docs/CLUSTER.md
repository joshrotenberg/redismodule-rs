# Cluster mode

## Keys outside the declared slot

In cluster mode on Redis 7.4 and later, Redis looks up each key in the slot of
the keys the running command declared, not in the key's own slot. While a
client runs a command, `getKeySlot` returns the slot Redis computed from the
command's declared keys (`db.c:476` in 8.10.2, `db.c:210` in 7.4.11). Only a
build with `DEBUG_ASSERTIONS` checks that the key hashes to that slot. Redis
7.2 has no such shortcut.

So every key a module reaches while a command runs must hash to the slot of
that command's declared keys. For any other key:

- a read misses, even when the key exists;
- a write stores the key in the declared keys' slot, where clients do not find
  it: `GET` returns nil, `DEL` returns 0, and `CLUSTER COUNTKEYSINSLOT` counts
  the key in the declared slot.

The lookup is the same through `open_key`, `open_key_writable` and `call`. It
also applies to a keyspace notification callback that runs during a client's
command, including Redis's own commands such as `SET`. A command that declares
no keys has no slot, and Redis hashes each key it reaches.

To stay within the slot:

- Declare every key the command takes, so that Redis rejects arguments in
  different slots with `CROSSSLOT` before the command runs.
- Name keys the module creates with a hash tag taken from a declared key, so
  they hash to its slot. `cluster::key_slot` computes a name's slot, and
  `cluster::key_for_slot` builds a name for a slot.
- In a notification callback, open only the notified key or keys that hash to
  its slot.

On a cluster with more than one node, `call` also refuses a key that another
node serves, with "Attempted to access a non local key in a cluster node" (the
check at `module.c:7117` in 8.10.2). The reproduction below ran on one node,
where every slot is local.

### Reproduction

One-node cluster, `CLUSTER ADDSLOTSRANGE 0 16383`, and a test module whose
commands declare their first argument (first key 1, last key 1, step 1) and
reach the key named by their second argument. The keys' slots: `a` 15495,
`b` 3300, `c` 7365, `d` 11298.

| Step | 7.2.16 | 7.4.11 and 8.10.2 |
| --- | --- | --- |
| `SET b orig`, then a command declaring `a` reads `b` with `open_key` | `orig` | nil |
| The same read with `call("GET", b)` | `orig` | nil |
| A command declaring `a` writes `c` with `open_key_writable`, then `GET c` | the value | nil |
| A command declaring `a` writes `d` with `call("SET", d, ...)`, then `GET d` | the value | nil |
| `CLUSTER COUNTKEYSINSLOT 15495` (`a`'s slot) | 0 | 2 |
| `CLUSTER COUNTKEYSINSLOT 7365` (`c`'s slot) | 1 | 0 |
| `CLUSTER GETKEYSINSLOT 15495 10` | empty | `c`, `d` |
| `DEL c` | 1 | 0 |
| A command declaring `a` reads `c` | the value | the value |
| A string notification callback for `SET trigger 1` reads `b` with `open_key` and `call` | `orig` | nil |
| A command declaring no keys reads `b` | `orig` | `orig` |
| A command declaring `a` writes `{a}x`, then `GET {a}x` | the value | the value |
