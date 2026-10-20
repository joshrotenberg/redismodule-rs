//! A library's compiled code. Loading an RDB fills it in on a thread, so the
//! server serves while large components compile; a call to a library still
//! compiling waits for it. Its client blocks, or, where a client cannot block
//! (inside `MULTI` or a script), the call waits in place.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use redis_module::ReplyingBlockedClient;

use crate::host::{CallState, LibraryPre};

/// A call of a function, held while its library compiles.
pub(crate) struct WaitingCall {
    pub(crate) function: String,
    pub(crate) keys: Vec<Vec<u8>>,
    pub(crate) args: Vec<Vec<u8>>,
    pub(crate) read_only: bool,
}

type Waiter = (ReplyingBlockedClient<WaitingCall>, WaitingCall);

enum State {
    /// Compiling; these blocked calls run when it is done.
    Compiling(Vec<Waiter>),
    Ready(LibraryPre<CallState>),
    Failed(String),
}

/// What a library's code is now.
pub(crate) enum Now {
    Ready(LibraryPre<CallState>),
    Compiling,
    Failed(String),
}

pub(crate) struct Code {
    state: Mutex<State>,
    done: Condvar,
}

impl Code {
    pub(crate) fn ready(pre: LibraryPre<CallState>) -> Self {
        Self::new(State::Ready(pre))
    }

    pub(crate) fn compiling() -> Self {
        Self::new(State::Compiling(Vec::new()))
    }

    fn new(state: State) -> Self {
        Self {
            state: Mutex::new(state),
            done: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn now(&self) -> Now {
        match &*self.state() {
            State::Compiling(_) => Now::Compiling,
            State::Ready(pre) => Now::Ready(pre.clone()),
            State::Failed(message) => Now::Failed(message.clone()),
        }
    }

    /// Records the compiled code, or why compiling failed, and runs the calls
    /// that waited for it.
    pub(crate) fn finish(&self, result: anyhow::Result<LibraryPre<CallState>>) {
        let next = match result {
            Ok(pre) => State::Ready(pre),
            Err(e) => State::Failed(format!("{e:#}")),
        };
        let waiters = match std::mem::replace(&mut *self.state(), next) {
            State::Compiling(waiters) => waiters,
            _ => Vec::new(),
        };
        self.done.notify_all();
        for (blocked, call) in waiters {
            blocked.unblock(call);
        }
    }

    /// Runs `call` once the code is no longer compiling: holds its blocked
    /// client until then, or unblocks it now.
    pub(crate) fn wait(&self, blocked: ReplyingBlockedClient<WaitingCall>, call: WaitingCall) {
        let mut state = self.state();
        if let State::Compiling(waiters) = &mut *state {
            waiters.push((blocked, call));
            return;
        }
        drop(state);
        blocked.unblock(call);
    }

    /// Waits on this thread until the code is no longer compiling. Compiling
    /// needs nothing from the main thread, so the main thread may wait.
    pub(crate) fn wait_here(&self) {
        let mut state = self.state();
        while matches!(*state, State::Compiling(_)) {
            state = self
                .done
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}
