//! The native keys behind [`RedisKey`](super::RedisKey) and
//! [`RedisKeyWritable`](super::RedisKeyWritable), and the per-thread registry
//! that keeps them valid across keyspace changes.
//!
//! An open native key caches the value Redis looked up when it opened.
//! Anything that deletes, replaces or moves that value while the key stays
//! open leaves the key pointing at freed memory, and closing it then reads that
//! memory too: from Redis 6.2, `moduleCloseKey` reads `key->value->type`. So
//! before such an
//! operation the SDK closes every key it opened on the thread, while each value
//! is still valid, and a closed key re-opens, with the same name, mode, flags
//! and database, the next time its handle is used. Re-opening has an open's
//! effects: it can raise `keymiss` and `expired` notifications and touch the
//! key's statistics, LRU and LFU, as the flags allow. The operations are
//! [`keyspace_will_change`]'s callers: calls, `yield_now`, keyspace
//! notifications, keyspace scans, and string DMA, writes, deletes, pops and
//! trims through a handle.
//!
//! Handles built from raw parts are not registered: the SDK does not own them,
//! or cannot re-open them. They count keyspace changes instead and panic when
//! used after one.

use std::cell::Cell;
use std::fmt;
use std::mem::ManuallyDrop;
use std::os::raw::c_int;
use std::ptr::{self, NonNull};

use crate::{RedisError, RedisString, raw};

thread_local! {
    /// The most recently opened of this thread's open SDK keys, which link to
    /// each other. A `Cell` of a pointer has no destructor, which would keep
    /// glibc from unloading the module.
    static OPEN_KEYS: Cell<*const Entry> = const { Cell::new(ptr::null()) };
    /// Counts keyspace changes on this thread, for raw handles.
    static CHANGES: Cell<u64> = const { Cell::new(0) };
    /// How many key scan callbacks are running on this thread.
    static FROZEN: Cell<u32> = const { Cell::new(0) };
}

/// The error for a keyspace change inside a key scan callback.
pub(crate) const FROZEN_ERROR: &str =
    "ERR a key scan callback is running; the keyspace cannot change until it returns";

const STALE_RAW: &str =
    "a key handle built from raw parts was used after the keyspace changed; open the key again";

/// The change count of a raw handle that has not been used yet: it takes the
/// count it first sees.
const UNSEEN: u64 = u64::MAX;

/// Prepares this thread for an operation that can delete, replace or move key
/// values: closes the keys the SDK opened here and has not closed, while each
/// value is still valid. With `acting`, a key changing its own value, only the
/// thread's other keys with the same name and database close, since that is
/// the only value the operation can free; without, every key closes.
///
/// A key in the middle of its own native call stays open: Redis is still
/// using it, and closing it would free the native key under that call. Only a
/// callback Redis raises during the call can reach it, such as the `new`
/// keyspace event of a write that creates the key. Redis warns notification
/// subscribers not to write, without refusing it; a subscriber that changes
/// the key a call is creating leaves Redis itself on a freed value.
///
/// # Errors
///
/// Refuses while a key scan callback runs on this thread.
pub(crate) fn keyspace_will_change(acting: Option<&NativeKey>) -> Result<(), RedisError> {
    if FROZEN.get() != 0 {
        return Err(RedisError::Str(FROZEN_ERROR));
    }
    CHANGES.set(CHANGES.get().wrapping_add(1));
    let acting = acting.and_then(NativeKey::entry);
    each_entry(|entry| {
        let affected = match acting {
            Some(acting) => {
                !ptr::eq(entry, acting)
                    && entry.db == acting.db
                    && entry.key_hash == acting.key_hash
            }
            None => true,
        };
        if affected && entry.busy.get() == 0 {
            entry.close();
        }
    });
    Ok(())
}

/// Calls `f` with each of this thread's registered keys.
fn each_entry(mut f: impl FnMut(&Entry)) {
    let mut next = OPEN_KEYS.get();
    while !next.is_null() {
        // SAFETY: an entry unlinks itself before it is freed, closing one
        // raises no callback that could unlink another, and entries are only
        // reached on their own thread.
        let entry = unsafe { &*next };
        next = entry.next.get();
        f(entry);
    }
}

/// Closes, before a key opens or re-opens, the registered keys opened with
/// `ACCESS_EXPIRED` (Redis 8.0+): they may hold an expired value that a plain
/// open of the same key deletes.
fn close_expired_access(except: Option<&Entry>) {
    let flag = super::KeyFlags::ACCESS_EXPIRED.bits();
    each_entry(|entry| {
        if entry.flags & flag != 0
            && !except.is_some_and(|except| ptr::eq(entry, except))
            && entry.busy.get() == 0
        {
            entry.close();
        }
    });
}

/// FNV-1a over a key name: registered keys compare it, with the database, to
/// tell which keys a change to one key can affect. A collision only closes an
/// extra key.
fn key_hash(name: *mut raw::RedisModuleString) -> u64 {
    let mut length = 0;
    // SAFETY: name is a live Redis string; the bytes are read immediately.
    let bytes = unsafe {
        let data = crate::__required_api!(RedisModule_StringPtrLen)(name, &mut length);
        std::slice::from_raw_parts(data.cast::<u8>(), length)
    };
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Refuses keyspace changes on this thread until dropped, even when the code
/// it guards panics.
pub(crate) struct Frozen;

impl Frozen {
    pub(crate) fn begin() -> Self {
        FROZEN.set(FROZEN.get() + 1);
        Frozen
    }
}

impl Drop for Frozen {
    fn drop(&mut self) {
        FROZEN.set(FROZEN.get() - 1);
    }
}

/// A registered SDK key: what it needs to close and re-open, and its links.
struct Entry {
    ctx: *mut raw::RedisModuleCtx,
    /// The open native key; null while closed, or for a missing key opened
    /// for reading.
    inner: Cell<*mut raw::RedisModuleKey>,
    /// Closed by a keyspace change; the next use re-opens it.
    closed: Cell<bool>,
    /// Native calls of this key in progress, which a change does not close.
    busy: Cell<u32>,
    /// Counts closes, so a stream iterator can tell its native iterator ended.
    epoch: Cell<u64>,
    /// The key's name: taken when the key closes, or at open for a missing
    /// key opened for reading, which has no native key to ask.
    name: Cell<Option<RedisString>>,
    /// The `raw::KeyMode` bits.
    mode: c_int,
    flags: c_int,
    db: c_int,
    key_hash: u64,
    prev: Cell<*const Entry>,
    next: Cell<*const Entry>,
}

impl Entry {
    fn link(&self) {
        let head = OPEN_KEYS.get();
        self.next.set(head);
        if !head.is_null() {
            // SAFETY: linked entries are live; see keyspace_will_change.
            unsafe { &*head }.prev.set(self);
        }
        OPEN_KEYS.set(self);
    }

    fn unlink(&self) {
        let (prev, next) = (self.prev.get(), self.next.get());
        if prev.is_null() {
            OPEN_KEYS.set(next);
        } else {
            // SAFETY: linked entries are live.
            unsafe { &*prev }.next.set(next);
        }
        if !next.is_null() {
            // SAFETY: linked entries are live.
            unsafe { &*next }.prev.set(prev);
        }
        self.prev.set(ptr::null());
        self.next.set(ptr::null());
    }

    /// Closes the native key, keeping its name for re-opening.
    fn close(&self) {
        if self.closed.get() {
            return;
        }
        let inner = self.inner.replace(ptr::null_mut());
        if !inner.is_null() {
            let name = self.name.take().or_else(|| {
                let get_name =
                    unsafe { crate::__required_api!(RedisModule_GetKeyNameFromModuleKey) };
                // SAFETY: inner is open; it holds its name until it closes,
                // and retaining the name keeps it past that.
                let name = unsafe { get_name(inner) };
                NonNull::new(name.cast_mut())
                    .map(|name| unsafe { RedisString::retain_from_ptr(name) })
            });
            self.name.set(name);
            raw::close_key(inner);
        }
        self.closed.set(true);
        self.epoch.set(self.epoch.get().wrapping_add(1));
    }

    /// Re-opens a closed key in the database it was opened in.
    fn reopen(&self) {
        close_expired_access(Some(self));
        let name = self.name.take().expect("a closed key keeps its name");
        let selected = unsafe { crate::__required_api!(RedisModule_GetSelectedDb)(self.ctx) };
        let select = unsafe { crate::__required_api!(RedisModule_SelectDb) };
        if selected != self.db {
            // SAFETY: ctx is the key's opening context, on its thread.
            unsafe { select(self.ctx, self.db) };
        }
        let mode = raw::KeyMode::from_bits_truncate(self.mode);
        let inner = raw::open_key_with_flags(self.ctx, name.as_ptr(), mode, self.flags);
        if selected != self.db {
            unsafe { select(self.ctx, selected) };
        }
        self.name.set(Some(name));
        self.inner.set(inner);
        self.closed.set(false);
    }
}

/// Marks a registered key as mid-call until dropped.
struct Busy<'a>(&'a Entry);

impl<'a> Busy<'a> {
    fn begin(entry: &'a Entry) -> Self {
        entry.busy.set(entry.busy.get() + 1);
        Busy(entry)
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.set(self.0.busy.get() - 1);
    }
}

/// The native key behind a key handle.
pub(crate) struct NativeKey(State);

enum State {
    /// Opened by the SDK and registered on this thread. The entry is a leaked
    /// `Box`, freed by `Drop` or `into_raw`: the registry reaches it through
    /// raw pointers, which a live `Box` would not allow.
    Opened(NonNull<Entry>),
    /// Built from raw parts, with the change count it last saw.
    Raw {
        inner: *mut raw::RedisModuleKey,
        seen: Cell<u64>,
    },
}

impl NativeKey {
    /// Opens `name` with `mode` and `flags` and registers the key.
    ///
    /// # Safety
    ///
    /// `ctx` must be a context Redis passed to the module, and this thread
    /// must hold the Redis lock until the key is dropped. `name` must be a
    /// live Redis string.
    pub(crate) unsafe fn open(
        ctx: *mut raw::RedisModuleCtx,
        name: *mut raw::RedisModuleString,
        mode: raw::KeyMode,
        flags: c_int,
    ) -> Self {
        let mode_bits = mode.bits();
        close_expired_access(None);
        let key_hash = key_hash(name);
        let inner = raw::open_key_with_flags(ctx, name, mode, flags);
        let db = unsafe { crate::__required_api!(RedisModule_GetSelectedDb)(ctx) };
        // A missing key opened for reading has no native key to ask its name.
        let kept_name = if inner.is_null() {
            NonNull::new(name).map(|name| unsafe { RedisString::retain_from_ptr(name) })
        } else {
            None
        };
        let entry = NonNull::from(Box::leak(Box::new(Entry {
            ctx,
            inner: Cell::new(inner),
            closed: Cell::new(false),
            busy: Cell::new(0),
            epoch: Cell::new(0),
            name: Cell::new(kept_name),
            mode: mode_bits,
            flags,
            db,
            key_hash,
            prev: Cell::new(ptr::null()),
            next: Cell::new(ptr::null()),
        })));
        // SAFETY: the entry was just allocated, and is freed only by Drop or
        // into_raw, which unlink it first.
        unsafe { entry.as_ref() }.link();
        Self(State::Opened(entry))
    }

    /// A key built from raw parts, which takes the change count it first sees.
    pub(crate) const fn raw(inner: *mut raw::RedisModuleKey) -> Self {
        Self(State::Raw {
            inner,
            seen: Cell::new(UNSEEN),
        })
    }

    /// A key Redis lends to a callback, current as of now.
    pub(crate) fn borrowed(inner: *mut raw::RedisModuleKey) -> Self {
        Self(State::Raw {
            inner,
            seen: Cell::new(CHANGES.get()),
        })
    }

    fn entry(&self) -> Option<&Entry> {
        match &self.0 {
            // SAFETY: the entry lives until this key is dropped or handed over.
            State::Opened(entry) => Some(unsafe { entry.as_ref() }),
            State::Raw { .. } => None,
        }
    }

    /// The native key, re-opened first if a keyspace change closed it, for a
    /// native call that changes no value and raises no callback. Use
    /// [`changing`](Self::changing) for one that can.
    ///
    /// # Panics
    ///
    /// Panics for a key built from raw parts used after a keyspace change.
    pub(crate) fn current(&self) -> *mut raw::RedisModuleKey {
        if let Some(entry) = self.entry() {
            if entry.closed.get() {
                entry.reopen();
            }
            return entry.inner.get();
        }
        match &self.0 {
            State::Opened(_) => unreachable!(),
            // A missing key has nothing that can be stale.
            State::Raw { inner, .. } if inner.is_null() => ptr::null_mut(),
            State::Raw { inner, seen } => {
                let changes = CHANGES.get();
                if seen.get() == UNSEEN {
                    seen.set(changes);
                }
                assert!(seen.get() == changes, "{STALE_RAW}");
                *inner
            }
        }
    }

    /// Runs `f`, a native call that can delete, replace or move key values,
    /// with the native key, after closing the thread's other open keys. The
    /// key stays open if a callback Redis raises during `f` changes the
    /// keyspace.
    ///
    /// # Errors
    ///
    /// Refuses while a key scan callback runs on this thread.
    pub(crate) fn changing<R>(
        &self,
        f: impl FnOnce(*mut raw::RedisModuleKey) -> R,
    ) -> Result<R, RedisError> {
        let inner = self.current();
        keyspace_will_change(Some(self))?;
        if let State::Raw { seen, .. } = &self.0 {
            // The change is this key's own, so it stays current.
            seen.set(CHANGES.get());
        }
        let _busy = self.entry().map(Busy::begin);
        Ok(f(inner))
    }

    /// A token for an iterator started on the current native key; see
    /// [`unchanged_since`](Self::unchanged_since).
    pub(crate) fn token(&self) -> u64 {
        self.entry()
            .map_or_else(|| CHANGES.get(), |entry| entry.epoch.get())
    }

    /// The native key, if it has not closed or the keyspace changed since
    /// `token`; never re-opens it.
    pub(crate) fn unchanged_since(&self, token: u64) -> Option<*mut raw::RedisModuleKey> {
        match (&self.0, self.entry()) {
            (_, Some(entry)) => {
                (!entry.closed.get() && entry.epoch.get() == token).then(|| entry.inner.get())
            }
            (State::Raw { inner, .. }, None) => (CHANGES.get() == token).then_some(*inner),
            (State::Opened(_), None) => unreachable!(),
        }
    }

    /// Hands over the native key, open, re-opening it first if needed.
    pub(crate) fn into_raw(self) -> *mut raw::RedisModuleKey {
        let inner = self.current();
        let this = ManuallyDrop::new(self);
        if let State::Opened(entry) = this.0 {
            // SAFETY: the entry was leaked by open and is freed only here or
            // in Drop, which ManuallyDrop skips.
            let entry = unsafe { Box::from_raw(entry.as_ptr()) };
            entry.unlink();
            // The entry drops here, releasing a kept name, without closing.
        }
        inner
    }
}

impl Drop for NativeKey {
    fn drop(&mut self) {
        match &self.0 {
            State::Opened(entry) => {
                // SAFETY: the entry was leaked by open and is freed only here
                // or in into_raw.
                let entry = unsafe { Box::from_raw(entry.as_ptr()) };
                entry.unlink();
                let inner = entry.inner.get();
                if !entry.closed.get() && !inner.is_null() {
                    raw::close_key(inner);
                }
            }
            State::Raw { inner, .. } => {
                if !inner.is_null() {
                    raw::close_key(*inner);
                }
            }
        }
    }
}

impl fmt::Debug for NativeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.0, self.entry()) {
            (_, Some(entry)) => f
                .debug_struct("NativeKey")
                .field("inner", &entry.inner.get())
                .field("closed", &entry.closed.get())
                .finish(),
            (State::Raw { inner, .. }, None) => {
                f.debug_struct("NativeKey").field("raw", inner).finish()
            }
            (State::Opened(_), None) => unreachable!(),
        }
    }
}
