//! A thread-local observer for cooperative cancellation polls.

use std::{cell::Cell, marker::PhantomData, rc::Rc};

thread_local! {
    static OBSERVER: Cell<Option<fn()>> = const { Cell::new(None) };
}

/// Restores the previous observer on the thread that installed it.
#[must_use]
pub struct ObserverGuard {
    previous: Option<fn()>,
    thread: PhantomData<Rc<()>>,
}

impl Drop for ObserverGuard {
    fn drop(&mut self) {
        OBSERVER.with(|observer| observer.set(self.previous));
    }
}

/// Installs a callback to run once before each cancellation poll on this thread.
pub fn set_poll_observer(f: fn()) -> ObserverGuard {
    ObserverGuard {
        previous: OBSERVER.with(|observer| observer.replace(Some(f))),
        thread: PhantomData,
    }
}

#[inline]
pub(crate) fn observe_poll() {
    if let Some(f) = OBSERVER.with(Cell::get) {
        f();
    }
}
// Modified for Makepad by fab/brep/sync/tool (fab/brep/SYNC.md): patch 0001.
