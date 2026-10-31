//! Keep native job callbacks and their destructors inside the module lifetime.
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::RedisError;

const RUNNING: u8 = 0;
const LOADING: u8 = 1;
const UNLOADING: u8 = 2;
static PHASE: AtomicU8 = AtomicU8::new(RUNNING);
static PENDING: AtomicUsize = AtomicUsize::new(0);

struct PendingJob;

impl Drop for PendingJob {
    fn drop(&mut self) {
        PENDING.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct Job<F> {
    // Field order matters: retain the ticket through captured destructors,
    // including when one panics or attempts a reentrant unload.
    pub(super) callback: Option<F>,
    _pending: PendingJob,
}

impl<F> Job<F> {
    pub(super) fn new(callback: F) -> Option<Self> {
        // Submission and native lifecycle callbacks hold the Redis lock.
        if PHASE.load(Ordering::SeqCst) != RUNNING {
            return None;
        }
        PENDING.fetch_add(1, Ordering::SeqCst);
        Some(Self {
            callback: Some(callback),
            _pending: PendingJob,
        })
    }
}

/// Internal binding for `redis_module!`; not a custom-loader lifecycle API.
/// A failed load has no OnUnload, so it must never leave a native job behind.
#[doc(hidden)]
pub struct PostNotificationLoadGuard;

impl PostNotificationLoadGuard {
    pub fn begin() -> Self {
        PHASE.store(LOADING, Ordering::SeqCst);
        Self
    }
}

impl Drop for PostNotificationLoadGuard {
    fn drop(&mut self) {
        PHASE.store(RUNNING, Ordering::SeqCst);
    }
}

/// Internal binding for `redis_module!`; not a custom-loader lifecycle API.
#[doc(hidden)]
pub struct PostNotificationUnloadGuard;

impl PostNotificationUnloadGuard {
    pub fn begin() -> Result<Self, RedisError> {
        if PENDING.load(Ordering::SeqCst) != 0 {
            return Err(RedisError::Str(
                "ERR module has pending or executing post-notification jobs",
            ));
        }
        PHASE
            .compare_exchange(RUNNING, UNLOADING, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| RedisError::Str("ERR module lifecycle is already changing"))?;
        Ok(Self)
    }
}

impl Drop for PostNotificationUnloadGuard {
    fn drop(&mut self) {
        PHASE.store(RUNNING, Ordering::SeqCst);
    }
}
