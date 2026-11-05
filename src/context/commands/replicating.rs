//! Which SDK command callbacks are registered `write` or `may-replicate`, for
//! the replication pause check (#220).
//!
//! During a write pause Redis postpones a transaction that queued such a
//! command, so one running inside a transaction during a pause means the pause
//! started in that transaction, and Redis replicates the rest of it.
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::CommandCallback;

// Callback addresses, and whether every registration of each was flagged
// `write` or `may-replicate`. Registration and lookups hold the Redis lock.
static CALLBACKS: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());

fn callbacks() -> MutexGuard<'static, Vec<(usize, bool)>> {
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
pub(super) fn record(callback: CommandCallback, flags: &str) {
    let replicates = flags_replicate(flags);
    let address = address(callback);
    let mut callbacks = callbacks();
    match callbacks.iter_mut().find(|(known, _)| *known == address) {
        Some((_, all)) => *all &= replicates,
        None => callbacks.push((address, replicates)),
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
        .find(|(known, _)| *known == address)
        .is_some_and(|(_, replicates)| *replicates)
}

/// Forgets the callbacks when a load starts or fails, and when an unload goes
/// ahead.
pub(super) fn forget() {
    *callbacks() = Vec::new();
}

#[cfg(test)]
mod tests {
    use super::{address, flags_replicate, forget, record, replicates};
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
        record(first, "write");
        record(second, "readonly");
        assert!(replicates(address(first)));
        assert!(!replicates(address(second)));
        record(first, "readonly");
        assert!(!replicates(address(first)));
        record(second, "write");
        assert!(!replicates(address(second)));
        forget();
        assert!(!replicates(address(first)));
    }
}
