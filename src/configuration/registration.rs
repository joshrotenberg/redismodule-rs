//! Owns a configuration's private data until Redis accepts its registration.
//!
//! Redis's Register*Config functions validate before storing the private data
//! pointer and call no callback during registration. A rejected registration
//! leaves the data with this guard, which drops it; an accepted one leaves it
//! with Redis for the life of the module.

use crate::{raw, RedisError};
use std::os::raw::c_int;
use std::ptr::NonNull;

pub(super) struct PendingRegistration<T> {
    data: NonNull<T>,
    accepted: bool,
    kind: &'static str,
}

impl<T> PendingRegistration<T> {
    pub(super) fn prepare<F>(
        api: Option<F>,
        data: T,
        kind: &'static str,
    ) -> Result<(F, Self), RedisError> {
        // Resolve availability before allocating/transferring the private Box.
        // An unavailable API also drops any callback captured in `data`.
        let api = api.ok_or_else(|| {
            RedisError::String(format!("{kind} configuration API is unavailable"))
        })?;
        Ok((
            api,
            Self {
                // Box guarantees a non-null allocation. Keep this raw pointer,
                // not a Box, so nothing moves the data after Redis stores it.
                data: NonNull::new(Box::into_raw(Box::new(data))).unwrap(),
                accepted: false,
                kind,
            },
        ))
    }

    pub(super) fn as_ptr(&self) -> *mut T {
        self.data.as_ptr()
    }

    /// Called immediately after the native call, before any caller cleanup.
    pub(super) fn finish(mut self, status: c_int) -> Result<(), RedisError> {
        if status != raw::REDISMODULE_OK as c_int {
            // Capture errno before dropping callback data can change it.
            let native_error = std::io::Error::last_os_error();
            return Err(RedisError::String(format!(
                "{} configuration registration failed: {native_error}",
                self.kind
            )));
        }
        // Redis keeps this pointer for the life of the module and has no free
        // callback for it, so the allocation stays.
        self.accepted = true;
        Ok(())
    }
}

impl<T> Drop for PendingRegistration<T> {
    fn drop(&mut self) {
        if !self.accepted {
            // Only this guard owns an unaccepted Box, and Drop runs once. Redis
            // rejects a registration before storing the pointer.
            unsafe { drop(Box::from_raw(self.data.as_ptr())) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct Probe(Rc<Cell<usize>>);
    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    fn captured_callback(drops: &Rc<Cell<usize>>) -> Box<dyn Fn()> {
        let probe = Probe(Rc::clone(drops));
        Box::new(move || {
            let _ = &probe;
        })
    }

    #[test]
    fn unavailable_api_drops_captured_callback_once() {
        let drops = Rc::new(Cell::new(0));
        let result = PendingRegistration::prepare::<()>(None, captured_callback(&drops), "Numeric");
        assert!(
            matches!(result, Err(RedisError::String(ref message)) if message == "Numeric configuration API is unavailable")
        );
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn rejected_registration_drops_captured_callback_once() {
        let drops = Rc::new(Cell::new(0));
        let (_, pending) =
            PendingRegistration::prepare(Some(()), captured_callback(&drops), "Boolean").unwrap();
        assert_eq!(drops.get(), 0);
        let error = pending.finish(raw::REDISMODULE_ERR as c_int).unwrap_err();
        assert!(error
            .to_string()
            .starts_with("Boolean configuration registration failed:"));
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn unsubmitted_registration_drops_captured_callback_once() {
        let drops = Rc::new(Cell::new(0));
        let (_, pending) =
            PendingRegistration::prepare(Some(()), captured_callback(&drops), "Enum").unwrap();
        drop(pending);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn accepted_data_keeps_its_stable_pointer_and_owner() {
        let drops = Rc::new(Cell::new(0));
        let (_, pending) =
            PendingRegistration::prepare(Some(()), captured_callback(&drops), "Enum").unwrap();
        let pointer = pending.as_ptr();
        let moved = pending;
        assert_eq!(pointer, moved.as_ptr());
        moved.finish(raw::REDISMODULE_OK as c_int).unwrap();
        assert_eq!(drops.get(), 0);
        // This allocation never reached Redis; free it so the test does not leak.
        unsafe { drop(Box::from_raw(pointer)) };
        assert_eq!(drops.get(), 1);
    }
}
