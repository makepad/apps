//! The rate the run reaches and what holds it there, from what `LiveValues` already carries (the
//! E2E transport's contract): no clock is read here.
//!
//! - Achieved: simulated seconds over wall seconds across about the last [`RATE_WINDOW`] wall
//!   seconds of publications, `sum(span) / sum(span / ratio)` with `span = t - since`. A
//!   publication's interval starts at the previous publication's time, except the first after a
//!   replacement (a new lowering), whose interval starts at the replacement: that one does not
//!   count, nor does a publication at rest (`ratio` 0) or one with an empty interval. A host that
//!   misses publications (it reads the newest each frame) still measures the intervals it sees.
//! - Limited by: paced while the run keeps up with a finite asked rate, else whichever of the
//!   solver, the scopes' acquisition and the emulator took the largest share of the stepping time
//!   ([`Split`], weighted by wall time). The host names [`Limit::Events`] over all of these while
//!   the device's event feed is full, and [`Limit::Input`] while the run holds for a streamed input.

use fab_elec_sim::live::LiveValues;
use std::collections::VecDeque;

/// The wall seconds of publications the achieved rate covers.
pub const RATE_WINDOW: f64 = 0.5;

/// A paced run keeps up when it reaches at least this share of the asked rate.
pub const KEEP_UP: f64 = 0.95;

/// What holds the run at the rate it reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// The asked rate: the run keeps up and waits for the clock.
    Paced,
    /// The analog solver's stepping.
    Solver,
    /// The scopes' acquisition (the sinks every accepted point goes to).
    Scope,
    /// The co-simulated device (the firmware emulator).
    Emulator,
    /// The device's event feed is full: the device holds its time until the host takes its events
    /// (none is dropped).
    Events,
    /// A streamed input (a motion track a Mechanics world appends to) does not yet cover what the
    /// run would read next: the run holds until its source catches up (`fab_elec_sim::input`).
    Input,
}

impl Limit {
    pub fn name(self) -> &'static str {
        match self {
            Limit::Paced => "paced",
            Limit::Solver => "solver",
            Limit::Scope => "scope",
            Limit::Emulator => "emulator",
            Limit::Events => "events",
            Limit::Input => "input",
        }
    }
}

/// Where the stepping time went over the window, as shares that add up to 1: the solver (the
/// rest), the scopes' sinks (`LiveValues::sink_share`) and the device (`LiveValues::co_share`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Split {
    pub solver: f32,
    pub scope: f32,
    pub emulator: f32,
}

impl Split {
    /// The part with the largest share; the solver on a tie.
    pub fn largest(&self) -> Limit {
        if self.scope > self.solver && self.scope >= self.emulator {
            Limit::Scope
        } else if self.emulator > self.solver && self.emulator > self.scope {
            Limit::Emulator
        } else {
            Limit::Solver
        }
    }
}

/// The newest publication intervals covering about [`RATE_WINDOW`] wall seconds.
#[derive(Clone, Debug, Default)]
pub struct RateMeter {
    /// (simulated span, wall seconds, the device's and the sinks' shares of the stepping time)
    seen: VecDeque<(f64, f64, f32, f32)>,
    generation: Option<u64>,
}

impl RateMeter {
    /// One publication's interval joins the window; the oldest leave while the rest still cover
    /// the window. The publication that ends the run's start (`LiveValues::started`) empties it.
    /// O(window) per publication, about 30.
    pub fn see(&mut self, lv: &LiveValues) {
        let replaced = self.generation.is_some_and(|g| g != lv.generation);
        self.generation = Some(lv.generation);
        // a run's start is no measure of it: the window starts over after it
        if lv.started {
            self.seen.clear();
            return;
        }
        let span = lv.t - lv.since;
        if replaced || !(lv.ratio.is_finite() && lv.ratio > 0.0 && span > 0.0) {
            return;
        }
        self.seen.push_back((span, span / lv.ratio, lv.co_share, lv.sink_share));
        let mut wall: f64 = self.seen.iter().map(|s| s.1).sum();
        while self.seen.len() > 1 && wall - self.seen[0].1 >= RATE_WINDOW {
            wall -= self.seen.pop_front().map_or(0.0, |s| s.1);
        }
    }

    /// The window covers its [`RATE_WINDOW`] of wall time: what it reaches is the run's, not its
    /// start's (the first steps after an operating point are short).
    pub fn full(&self) -> bool {
        self.seen.iter().map(|s| s.1).sum::<f64>() >= RATE_WINDOW
    }

    /// Simulated seconds per wall second over the window; None before an interval counted.
    pub fn achieved(&self) -> Option<f64> {
        let (span, wall) = self.seen.iter().fold((0.0, 0.0), |(s, w), x| (s + x.0, w + x.1));
        (wall > 0.0).then(|| span / wall)
    }

    /// The rate a run reads out: [`achieved`](Self::achieved) once the window is [`full`](Self::full),
    /// None before. A new run has no reading of its start, whose first steps are short.
    pub fn reached(&self) -> Option<f64> {
        if self.full() { self.achieved() } else { None }
    }

    /// Where the stepping time went over the window, each publication's shares weighted by its
    /// wall time; None before an interval counted.
    pub fn split(&self) -> Option<Split> {
        let (co, sink, wall) = self.seen.iter().fold((0.0, 0.0, 0.0), |(c, k, w), x| (c + f64::from(x.2) * x.1, k + f64::from(x.3) * x.1, w + x.1));
        if !(wall > 0.0) {
            return None;
        }
        let (emulator, scope) = ((co / wall) as f32, (sink / wall) as f32);
        Some(Split { solver: (1.0 - emulator - scope).max(0.0), scope, emulator })
    }

    /// What holds a running run at `achieved`, asked `rate` (paced only when `paced`: a finite
    /// rate and no firmware step, which runs unpaced); None before an interval counted.
    pub fn limited_by(&self, rate: f64, paced: bool) -> Option<Limit> {
        let achieved = self.achieved()?;
        if paced && rate.is_finite() && achieved >= KEEP_UP * rate {
            return Some(Limit::Paced);
        }
        Some(self.split().map_or(Limit::Solver, |split| split.largest()))
    }

    /// Forget the window (the run ended).
    pub fn clear(&mut self) {
        self.seen.clear();
        self.generation = None;
    }
}
