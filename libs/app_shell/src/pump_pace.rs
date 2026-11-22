//! The pump's clock. The pump polls what other threads and engines hand
//! the UI; most of them also raise the app signal (which pumps at once),
//! some only leave their result to be polled. So the clock runs at 30 Hz
//! while pumps keep finding something to do, and once a second of pumps
//! found next to nothing (a few changes at most: a readout that ticks a
//! couple of times a second) it drops to a 2 Hz check: an idle Stage asks
//! for no frames and barely wakes. Anything from outside (input, a signal,
//! an action), or three busy checks in a row, bring 30 Hz back.

/// The pump's interval while something moves.
pub const FAST: f64 = 1.0 / 30.0;
/// The interval of the idle check.
pub const SLOW: f64 = 0.5;
/// The pumps one look at the activity spans (1 s at full rate).
const WINDOW: u32 = 30;
/// Busy pumps a window may have and still count as idle.
const IDLE_BUSY: u32 = 4;
/// Busy checks in a row that mean work again (1.5 s at the slow rate).
const BUSY_CHECKS: u32 = 3;

#[derive(Default)]
pub struct PumpPace {
    pumps: u32,
    busy: u32,
    slow: bool,
    /// Idle checks in a row that found work.
    slow_busy: u32,
}

impl PumpPace {
    /// After a pump: `busy` when it asked for a frame, sent an action or
    /// timed work runs. The new interval when the clock changes.
    pub fn pumped(&mut self, busy: bool) -> Option<f64> {
        if self.slow {
            self.slow_busy = if busy { self.slow_busy + 1 } else { 0 };
            return if self.slow_busy >= BUSY_CHECKS { self.woken() } else { None };
        }
        self.pumps += 1;
        self.busy += busy as u32;
        if self.pumps < WINDOW {
            return None;
        }
        let idle = self.busy <= IDLE_BUSY;
        self.pumps = 0;
        self.busy = 0;
        if idle {
            self.slow = true;
            self.slow_busy = 0;
            return Some(SLOW);
        }
        None
    }

    /// Something came from outside: the clock is fast again (the new
    /// interval when it was slow).
    pub fn woken(&mut self) -> Option<f64> {
        self.pumps = 0;
        self.busy = 0;
        if self.slow {
            self.slow = false;
            return Some(FAST);
        }
        None
    }

    #[cfg(test)]
    fn slow(&self) -> bool {
        self.slow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_of_quiet_pumps_slows_the_clock_and_a_wake_restores_it() {
        let mut pace = PumpPace::default();
        for _ in 0..WINDOW - 1 {
            assert_eq!(pace.pumped(false), None);
        }
        assert_eq!(pace.pumped(false), Some(SLOW), "a second of nothing: the idle check");
        assert!(pace.slow());
        assert_eq!(pace.pumped(false), None, "stays slow");
        assert_eq!(pace.woken(), Some(FAST), "input or a signal: full rate at once");
        assert_eq!(pace.woken(), None);
        assert!(!pace.slow());
    }

    #[test]
    fn a_label_ticking_once_a_second_still_lets_the_clock_slow() {
        let mut pace = PumpPace::default();
        for i in 0..WINDOW - 1 {
            assert_eq!(pace.pumped(i == 10), None);
        }
        assert_eq!(pace.pumped(false), Some(SLOW));
        // a readout changing at 1.5 Hz: two of three checks see it
        for i in 0..30 {
            assert_eq!(pace.pumped(i % 3 != 0), None);
        }
        assert!(pace.slow());
    }

    #[test]
    fn steady_work_keeps_full_rate_and_three_busy_checks_restore_it() {
        let mut pace = PumpPace::default();
        for _ in 0..3 * WINDOW {
            assert_eq!(pace.pumped(true), None, "busy every pump: stays fast");
        }
        for _ in 0..WINDOW - 1 {
            pace.pumped(false);
        }
        assert_eq!(pace.pumped(false), Some(SLOW));
        assert_eq!(pace.pumped(true), None, "one busy check: maybe a tick");
        assert_eq!(pace.pumped(true), None, "two: a readout at the check's rate");
        assert_eq!(pace.pumped(true), Some(FAST), "three in a row: work again");
    }
}
