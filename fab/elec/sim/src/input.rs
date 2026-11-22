//! Streamed inputs (Q11, Q12): a device reads a source at simulated instants (an IMU part reads a
//! motion track a Mechanics world appends to), the source covers up to some simulated time, its
//! horizon, and the live run holds before anything in it could read past that. Nothing here is
//! one source's or one device's: a live microphone holds the same way.
//!
//! - The rule, checked by `LiveSim` at each loop top of an advance (whole stepping) and at each
//!   round start (rounds), after the due commands, and by `run_live` before it steps on: the run
//!   holds while an open source's horizon is short of the reach plus the source's period. The
//!   reach is where the next step may end (`upto`: the device's next instant, at most one step
//!   cap ahead; in rounds, the round's end) plus the attached process's `CoSim::input_span`, how
//!   far past the analog its run reads (an analog reader reads at the step's points: 0).
//! - So every read lands at or before the horizon, the run is held only at a loop top whose
//!   state is already fixed, and its points do not depend on when an input arrived: nothing is
//!   cut, capped or paused inside a step or a device's run.
//! - After a step the rule granted, the run stands where its next calls at that instant (a
//!   publication's `CoSim::state`, a host input) read within the source. A call made before the
//!   first grant at an instant is not covered. So `LiveCmd::Inputs` goes before the input that
//!   attaches the source to its reader, and that input applies where a granted step put the run:
//!   past the first instant the run asked the source to cover (`InputSource::ask`; the app's
//!   motion link waits for that), or with the source already reaching the instant plus the
//!   reader's span (a test that fills it first).
//! - A source that closes (its world stopped) holds the run no more; its readers keep its last
//!   value from there, and the run says so once.
//! - `want` is the simulated time (seconds, an `f64`'s bits) the run asks every source to cover:
//!   the reach, the longest source period and the lead. Paced, the lead is what the run steps
//!   between two publications and one slice; unpaced, [`UNPACED_LEAD_PERIODS`] source periods.
//!   A source's writer reads it, or the run hands it to each open source (`InputSource::ask`);
//!   nothing waits on it. A lead short of what Mechanics needs shows as holds in steady state
//!   (the readout's `input`).
//!
//! At a loop top each open source is polled once. A ring-fed source also copies the samples
//! that arrived since the last check. With no inputs there is no poll.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// While unpaced, how many of the longest source period `want` asks past the reach: an estimate,
/// to be raised if an unthrottled source still holds the run in steady state.
pub const UNPACED_LEAD_PERIODS: f64 = 8.0;

/// A stream a device reads at simulated instants.
pub trait InputSource: Send + Sync {
    /// The simulated time (seconds) of the source's last sample: a read at or before it is valid.
    /// None once the source is closed, when it feeds no more and holds no run.
    fn horizon(&self) -> Option<f64>;
    /// The horizon for this step, which needs samples through `required`. At the loop top
    /// a source fed by a ring can drain what has already arrived before the run decides to hold.
    /// Like `ask`, it runs on the worker and must not wait.
    fn poll_horizon(&self, _required: f64) -> Option<f64> { self.horizon() }
    /// The source's sample period, seconds: how far short of its horizon the run's reach stays.
    /// It absorbs the rounding between the source's instants and the run's.
    fn period(&self) -> f64;
    /// What the run's note calls it ("the motion track of U5").
    fn name(&self) -> String;
    /// The run asks the open source to cover `want`, simulated seconds (the value it stores in
    /// `Inputs::want`), at each check: a source whose writer goes only as far as it is asked
    /// raises its allowance here. On the run's thread, at a loop top: it must not wait. The
    /// default does nothing.
    fn ask(&self, _want: f64) {}
}

/// The run's streamed inputs (`LiveCmd::Inputs`): the sources it holds for, and where it says how
/// far it wants them to reach.
#[derive(Clone)]
pub struct Inputs {
    pub sources: Vec<Arc<dyn InputSource>>,
    pub want: Arc<AtomicU64>,
}

impl fmt::Debug for Inputs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Inputs({} sources)", self.sources.len())
    }
}

impl PartialEq for Inputs {
    fn eq(&self, other: &Inputs) -> bool {
        Arc::ptr_eq(&self.want, &other.want) && self.sources.len() == other.sources.len()
            && self.sources.iter().zip(&other.sources).all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

/// The live run's inputs and which closed sources it has said so of.
pub(crate) struct InputSet {
    sources: Vec<(Arc<dyn InputSource>, bool)>,
    want: Arc<AtomicU64>,
}

impl InputSet {
    pub(crate) fn new(inputs: Inputs) -> InputSet {
        InputSet { sources: inputs.sources.into_iter().map(|s| (s, false)).collect(), want: inputs.want }
    }

    /// Whether an open source is short of `reach` plus its period, the run standing at `t`. Asks
    /// for `reach`, the longest open period and the lead (`paced_lead`, else
    /// `UNPACED_LEAD_PERIODS` of that period).
    pub(crate) fn starved(&mut self, reach: f64, paced_lead: Option<f64>, t: f64) -> bool {
        let mut starved = false;
        let mut period = 0.0f64;
        for (source, noted) in &mut self.sources {
            if *noted { continue; }
            let p = source.period();
            let required = reach + p;
            match source.poll_horizon(required) {
                Some(horizon) => {
                    period = period.max(p);
                    starved |= horizon < required;
                }
                None => {
                    *noted = true;
                    eprintln!("elec sim: {} closed at {t:.6} s; the run no longer holds for it", source.name());
                }
            }
        }
        let lead = paced_lead.unwrap_or(UNPACED_LEAD_PERIODS * period);
        let want = reach + period + lead;
        self.want.store(want.to_bits(), Ordering::Release);
        // a closed source was noted above, and stays closed
        for (source, _) in self.sources.iter().filter(|(_, noted)| !noted) {
            source.ask(want);
        }
        starved
    }
}
