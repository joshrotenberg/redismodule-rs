//! Owner-thread state for string getter replies. No raw-pointer transport or
//! Send/Sync promise for callback closures, Rc or RefCell is introduced here.
use crate::{raw, Context, RedisError, RedisString};
use std::{cell::RefCell, marker::PhantomData, ptr::NonNull, rc::Rc};

pub(super) struct StringReply<T> {
    state: RefCell<ReplyState<T>>,
}
struct ReplyState<T> {
    published: Option<T>,
    closed: bool,
}
impl<T> StringReply<T> {
    pub(super) fn new() -> Self {
        Self {
            state: RefCell::new(ReplyState {
                published: None,
                closed: false,
            }),
        }
    }
    pub(super) fn is_closed(&self) -> bool {
        self.state.borrow().closed
    }
    pub(super) fn call_if_active<R>(&self, callback: impl FnOnce() -> R) -> Option<R> {
        if self.is_closed() {
            None
        } else {
            Some(callback())
        }
    }
    pub(super) fn read<R>(
        &self,
        get: impl FnOnce() -> T,
        pointer: impl FnOnce(&T) -> R,
    ) -> Option<R> {
        if self.is_closed() {
            return None;
        }
        // User code may synchronously complete another getter. No private slot
        // borrow is held here, and publish checks closure again afterward.
        self.publish(get(), pointer)
    }
    pub(super) fn publish<R>(&self, snapshot: T, pointer: impl FnOnce(&T) -> R) -> Option<R> {
        let (previous, result) = {
            let mut state = self.state.borrow_mut();
            if state.closed {
                // End the slot borrow before dropping a new native owner.
                drop(state);
                drop(snapshot);
                return None;
            }
            let previous = state.published.replace(snapshot);
            let result = pointer(state.published.as_ref().unwrap());
            (previous, result)
        };
        // Native free and any probe Drop run without a private mutable borrow.
        drop(previous);
        Some(result)
    }
    fn close(&self) {
        let previous = {
            let mut state = self.state.borrow_mut();
            state.closed = true;
            state.published.take()
        };
        drop(previous);
    }
}

struct Replies<T> {
    slots: Vec<Rc<StringReply<T>>>,
}
impl<T> Replies<T> {
    fn close(self) {
        for slot in self.slots {
            slot.close();
        }
    }
}
struct ModuleReplies {
    generation: u64,
    // Present only during OnLoad; cleared before successful callback return.
    loading_context: Option<NonNull<raw::RedisModuleCtx>>,
    replies: Replies<RedisString>,
}
struct OwnerThread {
    generation: u64,
    module: Option<ModuleReplies>,
}
thread_local! {
    static OWNER: RefCell<OwnerThread> = const { RefCell::new(OwnerThread { generation: 0, module: None }) };
}

/// Coordinates string configurations for a custom native module entry point.
///
/// The standard module macro manages this automatically. Raw OnLoad must begin
/// after native API initialization, keep the guard through registration/user
/// init, and commit immediately before returning OK. An error return drops the
/// pending guard and closes only its registered reply owners. Successful
/// OnUnload must call [`close_string_configurations`] after user deinit; canceled
/// unload must leave the committed state intact. Registered private data remains
/// stable through native removal; full userdata reclamation is not provided.
/// The guard cannot be moved to another thread. TLS destruction/image pinning is
/// platform dependent; successful close removes cached owners before native
/// unload, and does not rely on thread-exit cleanup. Unload/reload evidence is
/// limited to tested hosts/platforms, not a portability promise.
#[must_use]
pub struct StringConfigurationLoadScope {
    generation: u64,
    pending: bool,
    _owner_thread: PhantomData<Rc<()>>,
}
impl StringConfigurationLoadScope {
    /// Begin the load scope for this SDK image.
    ///
    /// # Safety
    /// Must run on the native owner thread in its actual OnLoad callback after
    /// successful native initialization. `ctx` must be that live callback's
    /// context; a fabricated Context or lock token is insufficient. The guard
    /// must be committed/dropped before returning from that same callback.
    /// Only one live module/load per SDK image is supported. All native config
    /// adapters and lifecycle operations must run on this same thread under the
    /// host's GIL. This does not certify other callbacks, panic or reentry paths.
    pub unsafe fn begin(ctx: &Context) -> Result<Self, RedisError> {
        let loading_context = NonNull::new(ctx.ctx)
            .ok_or(RedisError::Str("String configuration load context is null"))?;
        OWNER.with(|owner| {
            let mut owner = owner.borrow_mut();
            if owner.module.is_some() {
                return Err(RedisError::Str(
                    "String configuration scope is already active",
                ));
            }
            let generation = owner.generation.checked_add(1).ok_or(RedisError::Str(
                "String configuration scope generation exhausted",
            ))?;
            owner.generation = generation;
            owner.module = Some(ModuleReplies {
                generation,
                loading_context: Some(loading_context),
                replies: Replies { slots: Vec::new() },
            });
            Ok(Self {
                generation,
                pending: true,
                _owner_thread: PhantomData,
            })
        })
    }

    /// Keep reply owners active after a successful load.
    ///
    /// # Safety
    /// Call immediately before the same OnLoad returns OK, on the owner thread
    /// with its legal native API/GIL phase. No callback context is stored after
    /// this call. A later host rejection must use the real OnUnload cleanup.
    pub unsafe fn commit(mut self) -> Result<(), RedisError> {
        OWNER.with(|owner| {
            let mut owner = owner.borrow_mut();
            let module = owner
                .module
                .as_mut()
                .filter(|module| module.generation == self.generation)
                .ok_or(RedisError::Str(
                    "String configuration scope is no longer active",
                ))?;
            module.loading_context = None;
            self.pending = false;
            Ok(())
        })
    }
}
impl Drop for StringConfigurationLoadScope {
    fn drop(&mut self) {
        if self.pending {
            let module = OWNER.with(|owner| {
                let mut owner = owner.borrow_mut();
                if owner
                    .module
                    .as_ref()
                    .is_some_and(|m| m.generation == self.generation)
                {
                    owner.module.take()
                } else {
                    None
                }
            });
            if let Some(module) = module {
                module.replies.close();
            }
        }
    }
}

/// Release cached string getter owners following successful user deinit.
///
/// # Safety
/// Call only from this SDK image's real successful OnUnload on the same native
/// owner thread under the GIL, before native API/code removal. Do not call on
/// refusal, failed deinit or inside a live config callback. All prior borrowed
/// getter results must already have been consumed. Registered private data is
/// deliberately kept stable; no native config-removal/quiescence barrier or full
/// allocation reclamation is inferred. General context/thread safety is separate.
pub unsafe fn close_string_configurations() {
    let module = OWNER.with(|owner| owner.borrow_mut().module.take());
    if let Some(module) = module {
        module.replies.close();
    }
}

pub(super) fn admit(ctx: &Context) -> Result<u64, RedisError> {
    OWNER.with(|owner| {
        let owner = owner.borrow();
        owner
            .module
            .as_ref()
            .filter(|module| {
                module.loading_context.is_some() && module.loading_context == NonNull::new(ctx.ctx)
            })
            .map(|module| module.generation)
            .ok_or(RedisError::Str(
                "String configuration registration requires its active load scope",
            ))
    })
}
pub(super) fn track(
    generation: u64,
    reply: Rc<StringReply<RedisString>>,
) -> Result<(), RedisError> {
    let tracked = OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        if let Some(module) = owner
            .module
            .as_mut()
            .filter(|m| m.generation == generation && m.loading_context.is_some())
        {
            module.replies.slots.push(Rc::clone(&reply));
            true
        } else {
            false
        }
    });
    if tracked {
        Ok(())
    } else {
        // Defensive only: examined RegisterStringConfig does not invoke user
        // callbacks. Keep accepted private data stable, but never publish an
        // unmanaged native owner if that admission invariant was violated.
        reply.close();
        Err(RedisError::Str(
            "String configuration load scope changed during registration",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Probe {
        id: usize,
        drops: Rc<RefCell<Vec<usize>>>,
        slot: std::rc::Weak<StringReply<Probe>>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            // Reborrow during Drop proves cache mutation released its borrow.
            if let Some(slot) = self.slot.upgrade() {
                let _ = slot.is_closed();
            }
            self.drops.borrow_mut().push(self.id);
        }
    }
    fn probe(id: usize, drops: &Rc<RefCell<Vec<usize>>>, slot: &Rc<StringReply<Probe>>) -> Probe {
        Probe {
            id,
            drops: Rc::clone(drops),
            slot: Rc::downgrade(slot),
        }
    }
    #[test]
    fn replaces_one_owner_and_drops_outside_slot_borrow() {
        let slot = Rc::new(StringReply::new());
        let drops = Rc::new(RefCell::new(Vec::new()));
        assert!(slot.state.borrow().published.is_none());
        assert_eq!(slot.read(|| probe(1, &drops, &slot), |p| p.id), Some(1));
        assert!(drops.borrow().is_empty());
        assert_eq!(slot.read(|| probe(2, &drops, &slot), |p| p.id), Some(2));
        assert_eq!(*drops.borrow(), vec![1]);
        slot.close();
        assert!(slot.is_closed());
        assert_eq!(*drops.borrow(), vec![1, 2]);
        slot.close();
        assert_eq!(*drops.borrow(), vec![1, 2]);
    }
    #[test]
    fn closed_read_does_not_invoke_user_getter() {
        let slot = StringReply::<usize>::new();
        slot.close();
        let calls = Cell::new(0);
        assert_eq!(
            slot.read(
                || {
                    calls.set(calls.get() + 1);
                    3
                },
                |p| *p
            ),
            None
        );
        assert_eq!(calls.get(), 0);
        // The real adapter maps this None to a null callback pointer, without
        // invoking native APIs in this test.
    }
    #[test]
    fn closed_setter_gate_never_calls_user_code() {
        let slot = StringReply::<usize>::new();
        let calls = Cell::new(0);
        assert_eq!(
            slot.call_if_active(|| {
                calls.set(calls.get() + 1);
                1
            }),
            Some(1)
        );
        slot.close();
        assert_eq!(
            slot.call_if_active(|| {
                calls.set(calls.get() + 1);
                2
            }),
            None
        );
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn close_during_user_get_drops_new_snapshot_without_publication() {
        let slot = Rc::new(StringReply::new());
        let drops = Rc::new(RefCell::new(Vec::new()));
        assert_eq!(
            slot.read(
                || {
                    slot.close();
                    probe(1, &drops, &slot)
                },
                |p| p.id
            ),
            None
        );
        assert_eq!(*drops.borrow(), vec![1]);
        assert!(slot.state.borrow().published.is_none());
    }
    #[test]
    fn nested_getter_completes_before_outer_publication() {
        let slot = Rc::new(StringReply::new());
        let drops = Rc::new(RefCell::new(Vec::new()));
        let result = slot.read(
            || {
                assert_eq!(slot.read(|| probe(1, &drops, &slot), |p| p.id), Some(1));
                assert!(drops.borrow().is_empty()); // inner native-style consumption completed
                probe(2, &drops, &slot)
            },
            |p| p.id,
        );
        assert_eq!(result, Some(2));
        assert_eq!(*drops.borrow(), vec![1]);
        assert_eq!(slot.state.borrow().published.as_ref().unwrap().id, 2);
        slot.close();
        assert_eq!(*drops.borrow(), vec![1, 2]);
    }
    #[test]
    fn failed_scope_closes_only_owned_adapters() {
        let own = Rc::new(StringReply::new());
        let other = Rc::new(StringReply::new());
        own.publish(1, |p| *p);
        other.publish(2, |p| *p);
        Replies {
            slots: vec![Rc::clone(&own)],
        }
        .close();
        assert!(own.is_closed());
        assert!(!other.is_closed());
        assert_eq!(other.state.borrow().published, Some(2));
    }
}
