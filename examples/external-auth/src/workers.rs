use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

const LIMIT: usize = 4;
struct State {
    reserved: usize,
    handles: Vec<JoinHandle<()>>,
}
struct Inner {
    state: Mutex<State>,
    fault: AtomicBool,
    cancelled: Arc<AtomicBool>,
}
impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| {
            self.fault.store(true, Ordering::Release);
            error.into_inner()
        })
    }
}
/// Bounds pending starters and all retained thread handles together.
/// Workers never remove their own handle or assert their own stack quiescence.
#[derive(Clone)]
pub struct Workers(Arc<Inner>);
impl Default for Workers {
    fn default() -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                reserved: 0,
                handles: Vec::with_capacity(LIMIT),
            }),
            fault: AtomicBool::new(false),
            cancelled: Arc::new(AtomicBool::new(false)),
        }))
    }
}
impl Workers {
    /// Reserves before credentials are copied or a deferred starter is created.
    pub fn reserve(&self) -> Option<Permit> {
        let mut state = self.0.lock();
        if self.0.fault.load(Ordering::Acquire)
            || self.0.cancelled.load(Ordering::Acquire)
            || state.reserved + state.handles.len() >= LIMIT
        {
            return None;
        }
        state.reserved += 1;
        Some(Permit {
            inner: self.0.clone(),
        })
    }
    /// Stops admission and marks all worker inputs cancelled. No wait or join.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
    /// Joins only finished handles. The caller must first establish an ordinary
    /// admitted administrative command boundary. No mutex is held while joining.
    pub fn reap_finished(&self) {
        let finished = {
            let mut state = self.0.lock();
            let mut finished = Vec::with_capacity(LIMIT);
            let mut index = 0;
            while index < state.handles.len() {
                if state.handles[index].is_finished() {
                    finished.push(state.handles.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            finished
        };
        for handle in finished {
            if handle.join().is_err() {
                self.0.fault.store(true, Ordering::Release);
            }
        }
    }
    /// Pending reservations and retained handles, including finished handles.
    pub fn retained(&self) -> usize {
        let state = self.0.lock();
        state.reserved + state.handles.len()
    }
    /// True only after ordinary-command joining, with no poisoned/unknown state.
    pub fn quiescent(&self) -> bool {
        self.retained() == 0 && !self.0.fault.load(Ordering::Acquire)
    }
}
/// Owned reservation retained with an unstarted SDK deferred closure.
pub struct Permit {
    inner: Arc<Inner>,
}
impl Permit {
    /// Spawns a bounded application thread. The fallible spawn either retains
    /// its handle before returning or drops its captures without starting work.
    pub fn start(self, work: impl FnOnce(Arc<AtomicBool>) + Send + 'static) {
        let cancelled = self.inner.cancelled.clone();
        let result = std::thread::Builder::new()
            .name("external-auth".into())
            .spawn(move || work(cancelled));
        if let Ok(handle) = result {
            self.inner.lock().handles.push(handle);
        }
        // Drop transfers the charged reservation to the stored handle on
        // success, or releases it after spawn drops the unused captures.
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.inner.lock();
        if let Some(next) = state.reserved.checked_sub(1) {
            state.reserved = next;
        } else {
            self.inner.fault.store(true, Ordering::Release);
        }
    }
}
