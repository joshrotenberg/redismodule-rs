//! Which SDK command callbacks are registered `write` or `may-replicate`, for
//! the replication pause check (#220), and exclude `allow-busy` for auth entry.
//!
//! During a write pause Redis postpones a transaction that queued such a
//! command, so one running inside a transaction during a pause means the pause
//! started in that transaction, and Redis replicates the rest of it.
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::CommandCallback;

// Callback addresses, and whether every registration of each was flagged
// `write` or `may-replicate`, and every registration excludes `allow-busy`.
// Registration and lookups hold the Redis lock.
struct CallbackMetadata {
    address: usize,
    replicates: bool,
    ordinary: bool,
    names: Vec<Box<[u8]>>,
}
static CALLBACKS: Mutex<Vec<CallbackMetadata>> = Mutex::new(Vec::new());

fn callbacks() -> MutexGuard<'static, Vec<CallbackMetadata>> {
    CALLBACKS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// True if Redis treats a command with `flags` as one that can replicate.
/// Redis reads module command flags case-insensitively.
fn flags_replicate(flags: &str) -> bool {
    flags.split_ascii_whitespace().any(|flag| {
        flag.eq_ignore_ascii_case("write") || flag.eq_ignore_ascii_case("may-replicate")
    })
}

/// Records a callback Redis registered with `flags`. A callback registered
/// for several commands counts only if all of them can replicate.
pub(super) fn record(callback: CommandCallback, flags: &str, name: &[u8]) {
    let replicates = flags_replicate(flags);
    let ordinary = !flags
        .split_ascii_whitespace()
        .any(|f| f.eq_ignore_ascii_case("allow-busy"));
    let address = address(callback);
    let mut callbacks = callbacks();
    match callbacks.iter_mut().find(|known| known.address == address) {
        Some(known) => {
            known.replicates &= replicates;
            known.ordinary &= ordinary;
            known.names.push(name.into());
        }
        None => callbacks.push(CallbackMetadata {
            address,
            replicates,
            ordinary,
            names: vec![name.into()],
        }),
    }
}

/// The address of a command callback, as the running command records it.
pub(crate) fn address(callback: CommandCallback) -> usize {
    callback as usize
}

/// True if the callback at `address` is registered, and only for commands
/// flagged `write` or `may-replicate`. An unknown address is false, so the
/// pause check refuses.
pub(crate) fn replicates(address: usize) -> bool {
    callbacks()
        .iter()
        .find(|known| known.address == address)
        .is_some_and(|known| known.replicates)
}

/// Every registration must exclude allow-busy: such commands can run inside
/// a foreign module's Yield while that module still owns native teardown state.
/// Unknown/raw callbacks do not establish SDK ordinary-command provenance.
pub(crate) fn ordinary(address: usize, command: &[u8], subcommand: Option<&[u8]>) -> bool {
    callbacks()
        .iter()
        .find(|known| known.address == address)
        .is_some_and(|known| {
            known.ordinary
                && known.names.iter().any(|name| {
                    name.eq_ignore_ascii_case(command)
                        || name
                            .iter()
                            .position(|b| *b == b'|')
                            .is_some_and(|separator| {
                                name[..separator].eq_ignore_ascii_case(command)
                                    && subcommand.is_some_and(|sub| {
                                        name[separator + 1..].eq_ignore_ascii_case(sub)
                                    })
                            })
                })
        })
}

/// Forgets the callbacks when a load starts or fails, and when an unload goes
/// ahead.
pub(super) fn forget() {
    *callbacks() = Vec::new();
}

#[cfg(test)]
mod tests {
    use super::{address, flags_replicate, forget, ordinary, record, replicates};
    use crate::raw;

    extern "C" fn first(
        _: *mut raw::RedisModuleCtx,
        _: *mut *mut raw::RedisModuleString,
        _: i32,
    ) -> i32 {
        0
    }

    extern "C" fn second(
        _: *mut raw::RedisModuleCtx,
        _: *mut *mut raw::RedisModuleString,
        _: i32,
    ) -> i32 {
        1
    }

    #[test]
    fn write_and_may_replicate_flags_replicate() {
        assert!(flags_replicate("write deny-oom"));
        assert!(flags_replicate("fast may-replicate"));
        assert!(flags_replicate("WRITE"));
        assert!(flags_replicate("  May-Replicate  "));
        assert!(!flags_replicate("readonly fast"));
        assert!(!flags_replicate(""));
        assert!(!flags_replicate("writes may-replicates"));
    }

    #[test]
    fn a_callback_counts_only_if_every_registration_replicates() {
        forget();
        record(first, "write", b"first");
        assert!(ordinary(address(first), b"FIRST", None));
        record(second, "readonly", b"second");
        assert!(replicates(address(first)));
        assert!(!replicates(address(second)));
        record(first, "readonly", b"alias");
        assert!(!replicates(address(first)));
        record(second, "write", b"container|child");
        assert!(!replicates(address(second)));
        record(first, "ALLOW-BUSY write", b"busy");
        assert!(!ordinary(address(first), b"FIRST", None));
        record(first, "write", b"first");
        assert!(!ordinary(address(first), b"FIRST", None));
        assert!(ordinary(address(second), b"second", None));
        assert!(ordinary(address(second), b"CONTAINER", Some(b"CHILD")));
        assert!(!ordinary(address(second), b"raw-alias", None));
        assert!(!ordinary(address(second), b"container", Some(b"unknown")));
        forget();
        assert!(!ordinary(address(first), b"FIRST", None));
        assert!(!replicates(address(first)));
    }
}
