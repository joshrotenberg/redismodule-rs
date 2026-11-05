//! Keep native job callbacks and their destructors inside the module lifetime.
use std::cell::Cell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::RedisError;

const RUNNING: u8 = 0;
const LOADING: u8 = 1;
const UNLOADING: u8 = 2;
static PHASE: AtomicU8 = AtomicU8::new(RUNNING);
static PENDING: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// True while this thread runs an SDK post-notification job, for the
    /// replication pause check in calls. A `Cell<bool>` has no destructor,
    /// which would keep glibc from unloading the module.
    static JOB_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// True while this thread runs an SDK post-notification job.
pub(super) fn job_running() -> bool {
    JOB_RUNNING.get()
}

/// Marks this thread as running an SDK post-notification job until dropped,
/// restoring the previous mark.
pub(super) struct RunningJob {
    previous: bool,
}

impl RunningJob {
    #[must_use]
    pub(super) fn begin() -> Self {
        Self {
            previous: JOB_RUNNING.replace(true),
        }
    }
}

impl Drop for RunningJob {
    fn drop(&mut self) {
        JOB_RUNNING.set(self.previous);
    }
}

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

#[cfg(test)]
mod tests {
    use super::{RunningJob, job_running};

    #[test]
    fn running_job_marks_only_this_thread_until_dropped() {
        assert!(!job_running());
        {
            let _job = RunningJob::begin();
            assert!(job_running());
            {
                let _nested = RunningJob::begin();
                assert!(job_running());
            }
            assert!(job_running(), "a nested mark restores the outer one");
            let other = std::thread::spawn(job_running).join().unwrap();
            assert!(!other, "another thread runs no job");
        }
        assert!(!job_running());
    }
}
