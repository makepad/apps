//! A component the product starts after the first frame (the DJ library and
//! its media and bake workers), with its own start/stop state, so closing
//! is bounded by what was actually started:
//!
//! - never started when the close begins: the close has nothing to wait for
//!   (it counts as stopped) and the component can never start afterwards;
//! - started: `close` hands it the stop signal, and the close waits for the
//!   component's own `stopped`.
//!
//! A start request that races the close (the pump that starts it runs after
//! the close was begun) is refused here, in one place, instead of at every
//! call site.

pub struct LazyComponent<T> {
    item: Option<T>,
    closed: bool,
}

impl<T> Default for LazyComponent<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> LazyComponent<T> {
    pub const fn new() -> Self {
        Self { item: None, closed: false }
    }

    /// Start the component once. Refused (`make` is not called) after the
    /// close began. Returns whether the component runs.
    pub fn start(&mut self, make: impl FnOnce() -> T) -> bool {
        if self.closed {
            return false;
        }
        if self.item.is_none() {
            self.item = Some(make());
        }
        true
    }

    pub fn as_ref(&self) -> Option<&T> {
        self.item.as_ref()
    }

    pub fn as_mut(&mut self) -> Option<&mut T> {
        self.item.as_mut()
    }

    pub fn is_none(&self) -> bool {
        self.item.is_none()
    }

    /// Begin the close: no start from now on, and a started component gets
    /// its stop signal.
    pub fn close(&mut self, stop: impl FnOnce(&mut T)) {
        self.closed = true;
        if let Some(item) = self.item.as_mut() {
            stop(item);
        }
    }

    /// Nothing is left to wait for: never started, or the component says so.
    pub fn stopped(&self, done: impl FnOnce(&T) -> bool) -> bool {
        self.item.as_ref().is_none_or(done)
    }
}

#[cfg(test)]
mod tests {
    use super::LazyComponent;
    use std::cell::Cell;

    /// A stand-in worker: stop() is the signal, `ended` what the pump sees.
    struct Worker {
        signalled: Cell<bool>,
        ended: Cell<bool>,
    }

    fn worker() -> Worker {
        Worker { signalled: Cell::new(false), ended: Cell::new(false) }
    }

    #[test]
    fn never_started_counts_as_stopped_at_close() {
        let mut component = LazyComponent::<Worker>::new();
        assert!(component.stopped(|w| w.ended.get()), "nothing started: nothing to wait for");
        component.close(|w| w.signalled.set(true));
        assert!(component.stopped(|w| w.ended.get()));
        assert!(component.is_none());
    }

    #[test]
    fn started_gets_the_stop_signal_and_is_stopped_when_it_says_so() {
        let mut component = LazyComponent::new();
        assert!(component.start(worker));
        assert!(!component.stopped(|w| w.ended.get()), "running");
        component.close(|w| w.signalled.set(true));
        assert!(component.as_ref().unwrap().signalled.get(), "the stop signal reached the worker");
        assert!(!component.stopped(|w| w.ended.get()), "signalled but not ended yet: still waiting");
        component.as_ref().unwrap().ended.set(true);
        assert!(component.stopped(|w| w.ended.get()));
    }

    #[test]
    fn stop_before_start_never_starts() {
        let mut component = LazyComponent::<Worker>::new();
        component.close(|_| {});
        let built = Cell::new(false);
        assert!(!component.start(|| {
            built.set(true);
            worker()
        }));
        assert!(!built.get(), "the component is never built after the close began");
        assert!(component.is_none());
        assert!(component.stopped(|w| w.ended.get()));
    }

    #[test]
    fn start_is_idempotent() {
        let mut component = LazyComponent::new();
        let built = Cell::new(0);
        for _ in 0..3 {
            component.start(|| {
                built.set(built.get() + 1);
                worker()
            });
        }
        assert_eq!(built.get(), 1);
    }
}
