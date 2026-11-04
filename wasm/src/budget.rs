//! Admission for mapped resources, independently of Redis heap and RSS.
//! A limit and its use share one lock, including background image admission.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use anyhow::{Context as _, bail};
use redis_module::RedisError;
use redis_module::configuration::{ConfigurationContext, ConfigurationValue};

pub(crate) const DEFAULT_BUDGET: i64 = 1 << 30;
pub(crate) static GUEST: Budget = Budget::new("wasm.guest-memory-budget", DEFAULT_BUDGET as usize);
pub(crate) static CODE: Budget = Budget::new("wasm.code-budget", DEFAULT_BUDGET as usize);

/// Field order retains every owner until the resource's full destructor has
/// returned. In particular, Store data alone dies before Store resources.
pub(crate) struct Retained<T, L> {
    pub(crate) resource: T,
    pub(crate) _owners: L,
}

struct State {
    limit: usize,
    used: usize,
}

pub(crate) struct Budget {
    name: &'static str,
    state: Mutex<State>,
}

impl Budget {
    const fn new(name: &'static str, limit: usize) -> Self {
        Self {
            name,
            state: Mutex::new(State { limit, used: 0 }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_limit(&self, limit: i64) -> Result<(), RedisError> {
        let limit = usize::try_from(limit).map_err(|_| {
            RedisError::Str("ERR a memory budget must be a representable nonnegative byte count")
        })?;
        // Lowering never evicts a valid instance or image. Future positive
        // admission must fit the new limit, even when current use exceeds it.
        self.state().limit = limit;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_test(limit: usize) -> &'static Self {
        // Independent budget for concurrent tests, never a production reset.
        Box::leak(Box::new(Self::new("test-budget", limit)))
    }

    #[cfg(test)]
    pub(crate) fn limit_for_test(&self, limit: i64) {
        self.set_limit(limit).unwrap();
    }

    pub(crate) fn lease(&'static self) -> Lease {
        Lease(Arc::new(Charge {
            budget: self,
            bytes: AtomicUsize::new(0),
        }))
    }

    /// Never block a crash-report INFO while this same thread owns the lock.
    /// -1 denotes an unavailable snapshot, rather than falsely reporting zero.
    pub(crate) fn snapshot(&self) -> (i64, i64) {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return (-1, -1),
        };
        let count = |n| i64::try_from(n).unwrap_or(i64::MAX);
        (count(state.limit), count(state.used))
    }
}

impl ConfigurationValue<i64> for Budget {
    fn get(&self, _: &ConfigurationContext) -> i64 {
        i64::try_from(self.state().limit).expect("configuration limits fit i64")
    }

    fn set(&self, _: &ConfigurationContext, value: i64) -> Result<(), RedisError> {
        self.set_limit(value)
    }
}

/// One resource's growing charge. Clones retain the same reservation, not a
/// second charge. The final owner must drop only AFTER its actual resource.
#[derive(Clone)]
pub(crate) struct Lease(Arc<Charge>);

struct Charge {
    budget: &'static Budget,
    bytes: AtomicUsize,
}

impl Lease {
    pub(crate) fn bytes(&self) -> usize {
        self.0.bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn grow(&self, delta: usize) -> anyhow::Result<()> {
        if delta == 0 {
            return Ok(());
        }
        // No logging, allocation, Redis or Wasmtime/user callback under lock.
        let refused = {
            let mut state = self.0.budget.state();
            let own = self.bytes().checked_add(delta);
            let total = state.used.checked_add(delta);
            match (own, total) {
                (Some(own), Some(total)) if total <= state.limit => {
                    state.used = total;
                    self.0.bytes.store(own, Ordering::Relaxed);
                    None
                }
                _ => Some((state.used, state.limit)),
            }
        };
        if let Some((used, limit)) = refused {
            bail!(
                "{} refuses {delta} additional bytes (used {used}, limit {limit})",
                self.0.budget.name
            );
        }
        Ok(())
    }

    /// Roll back one previously accepted growth, before any resource exists
    /// for that delta. Normal destruction releases via the final Arc owner.
    pub(crate) fn rollback(&self, delta: usize) -> anyhow::Result<()> {
        let mut state = self.0.budget.state();
        let own = self.bytes().checked_sub(delta);
        let total = state.used.checked_sub(delta);
        if let (Some(own), Some(total)) = (own, total) {
            state.used = total;
            self.0.bytes.store(own, Ordering::Relaxed);
            return Ok(());
        }
        drop(state);
        bail!("memory reservation rollback exceeds its charge")
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        let mut state = self.budget.state();
        state.used = state
            .used
            .checked_sub(self.bytes.load(Ordering::Relaxed))
            .expect("live resource charge is part of aggregate use");
    }
}

/// The complete file mapping is rounded conservatively to host pages. This is
/// a mapped-image charge, not executable text size or resident physical pages.
pub(crate) fn rounded_image_bytes(bytes: usize, page: usize) -> anyhow::Result<usize> {
    if bytes == 0 || page == 0 {
        bail!("compiled image and host page size must be nonzero");
    }
    bytes
        .checked_add(page - 1)
        .and_then(|n| (n / page).checked_mul(page))
        .context("compiled image page rounding overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn budget(limit: usize) -> &'static Budget {
        Budget::for_test(limit)
    }

    #[test]
    fn boundaries_lowering_and_final_owner_preserve_existing_resources() {
        let budget = budget(10);
        let first = budget.lease();
        first.grow(6).unwrap();
        let retained = first.clone();
        let second = budget.lease();
        second.grow(4).unwrap();
        assert!(second.grow(1).is_err());
        assert_eq!(budget.snapshot(), (10, 10));
        budget.set_limit(3).unwrap();
        assert!(first.grow(1).is_err());
        first.grow(0).unwrap();
        drop(first);
        assert_eq!(budget.snapshot(), (3, 10));
        second.rollback(4).unwrap();
        drop(retained);
        assert_eq!(budget.snapshot(), (3, 0));
        second.grow(3).unwrap();
        assert!(second.rollback(4).is_err());
        assert_eq!(budget.snapshot(), (3, 3));
        drop(second);
        assert_eq!(budget.snapshot(), (3, 0));
        assert!(budget.set_limit(-1).is_err());
        budget.set_limit(0).unwrap();
        assert!(budget.lease().grow(1).is_err());
    }

    #[test]
    fn overflow_and_rounding_are_refused_without_changing_use() {
        let budget = budget(usize::MAX);
        let lease = budget.lease();
        lease.grow(usize::MAX).unwrap();
        assert!(lease.grow(1).is_err());
        assert_eq!(lease.bytes(), usize::MAX);
        drop(lease);
        assert_eq!(budget.state().used, 0);
        assert_eq!(rounded_image_bytes(4096, 4096).unwrap(), 4096);
        assert_eq!(rounded_image_bytes(4097, 4096).unwrap(), 8192);
        assert_eq!(rounded_image_bytes(1, 16384).unwrap(), 16384);
        assert!(rounded_image_bytes(usize::MAX, 4096).is_err());
        assert!(rounded_image_bytes(0, 4096).is_err());
        assert!(rounded_image_bytes(1, 0).is_err());
    }

    #[test]
    fn setters_reservations_and_release_share_one_linearization_point() {
        for _ in 0..32 {
            let budget = budget(1);
            let barrier = Arc::new(Barrier::new(2));
            let other = Arc::clone(&barrier);
            let worker = std::thread::spawn(move || {
                other.wait();
                let lease = budget.lease();
                let admitted = lease.grow(1).is_ok();
                (lease, admitted)
            });
            barrier.wait();
            budget.set_limit(0).unwrap();
            let (lease, admitted) = worker.join().unwrap();
            assert_eq!(budget.snapshot(), (0, i64::from(admitted)));
            assert!(budget.lease().grow(1).is_err());
            drop(lease);
            assert_eq!(budget.snapshot(), (0, 0));
        }
    }

    #[test]
    fn poison_recovery_and_crash_snapshot_never_erase_charges() {
        let budget = budget(10);
        let lease = budget.lease();
        lease.grow(4).unwrap();
        {
            let _guard = budget.state();
            assert_eq!(budget.snapshot(), (-1, -1));
        }
        let _ = std::thread::spawn(move || {
            let _guard = budget.state();
            panic!("poison only, no partial update");
        })
        .join();
        budget.set_limit(2).unwrap();
        assert_eq!(budget.snapshot(), (2, 4));
        assert!(lease.grow(1).is_err());
        drop(lease);
        assert_eq!(budget.snapshot(), (2, 0));
    }
}
