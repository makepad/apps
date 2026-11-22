//! The bench's expectations judged on the live run (`LiveJudge`), point by point as the counted
//! run judges them (`run::bench_run`): a sink the run's host installs beside its views. What it
//! publishes (`Judged`) is what a person reads on the Bench editor and what the AI's
//! `elec.expect.*` read, so the two always see the same verdicts of the same run.
//!
//! Cost: on the run's thread O(expectations) per accepted point (`Expectations::step`), plus
//! O(pins) when a check reads a part's power; a publication (at most every `PUBLISH`, and when the
//! run rests) copies the states under a lock held for that copy only. A snapshot of the run saves
//! the judge's histories (`Sink::save`), so an edit behind the run's front goes on from exactly
//! where the judge stood there.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::expect::{CheckState, Expect, Expectations, Measure, Probe, Sample};
use crate::ValueIndex;
use fab_elec_core::diag::Diag;
use fab_elec_sim::live::ValuePlan;
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::{Sink, SinkState};

/// How often a running judge publishes its states, at most.
pub const PUBLISH: Duration = Duration::from_millis(50);

/// The verdicts of the judge installed last, as it last published them.
#[derive(Clone, Debug, Default)]
pub struct Judged {
    /// The judge these belong to: a judge installed later takes the slot over, and an earlier
    /// one publishes nothing more.
    pub judge: u64,
    /// The run judged: the host's count of runs from t = 0, kept when a circuit is replaced
    /// within the run (the judge then begins again).
    pub run: u64,
    /// The expectations judged, in the bench's order.
    pub expects: Vec<Expect>,
    /// Per expectation, its state (`Expectations::states`); pending until the run's points come.
    pub states: Vec<CheckState>,
    /// Why an expectation is never judged on this lowering (`Expectations::new`).
    pub diags: Vec<Diag>,
    /// The time of the last point judged.
    pub t: f64,
    /// Grows with every publication.
    pub version: u64,
}

impl Judged {
    /// The slot as a new judge takes it over: its expectations, all pending, before its first
    /// point. O(expectations).
    pub fn take_over(&mut self, judge: u64, run: u64, expectations: &Expectations, diags: Vec<Diag>) {
        self.judge = judge;
        self.run = run;
        self.expects = expectations.expects().to_vec();
        self.states = expectations.states().to_vec();
        self.diags = diags;
        self.t = 0.0;
        self.version += 1;
    }
}

/// The judge's state at a snapshot of the run.
#[derive(Clone)]
struct Saved {
    expectations: Expectations,
    t: f64,
}

/// A sink judging the bench's expectations on every accepted point of the live run, from the
/// values in the lowering's numbering (`Sink::values`). It begins again (every check pending)
/// when the run begins: a run from t = 0, or a circuit replaced within it.
pub struct LiveJudge {
    judge: u64,
    run: u64,
    expectations: Expectations,
    /// The lowering's net count: a point of another lowering (one still running while this judge
    /// went in for the next) is not judged.
    nets: usize,
    /// Each pin's net and each part's pins (`ValuePlan`), for a part's power: the sum of each
    /// connected pin's net voltage times its current, as `bench_run` takes it.
    pin_net: Vec<Option<u32>>,
    part_pins: Vec<(u32, u32)>,
    part_p: Vec<f64>,
    powers: bool,
    t: f64,
    shared: Arc<Mutex<Judged>>,
    published: Option<Instant>,
    unpublished: bool,
}

impl LiveJudge {
    /// A judge of `expects` on runs of a lowering (its value plan and numbering), publishing into
    /// `shared`, which it takes over now as judge `judge` of run `run`. O(expectations) and the
    /// plan's pin tables.
    pub fn new(judge: u64, run: u64, expects: Vec<Expect>, plan: &ValuePlan, index: &ValueIndex, shared: Arc<Mutex<Judged>>) -> LiveJudge {
        let (expectations, diags) = Expectations::new(expects, index);
        let reads_power = |probe: &Probe| matches!(probe, Probe::P(_));
        let powers = expectations.expects().iter().any(|e| reads_power(&e.probe) || matches!(&e.measure, Measure::Delay { to, .. } if reads_power(to)));
        shared.lock().unwrap().take_over(judge, run, &expectations, diags);
        LiveJudge {
            judge,
            run,
            expectations,
            nets: plan.nets.len(),
            pin_net: plan.pin_net.clone(),
            part_pins: plan.part_pins.clone(),
            part_p: vec![0.0; if powers { plan.part_pins.len() } else { 0 }],
            powers,
            t: 0.0,
            shared,
            published: None,
            unpublished: false,
        }
    }

    /// The states into the shared slot, unless a later judge has taken it. O(expectations).
    fn publish(&mut self) {
        self.published = Some(Instant::now());
        self.unpublished = false;
        let mut shared = self.shared.lock().unwrap();
        if shared.judge != self.judge {
            return;
        }
        shared.run = self.run;
        shared.states.clone_from_slice(self.expectations.states());
        shared.t = self.t;
        shared.version += 1;
    }

    fn rest(&mut self) {
        if self.unpublished {
            self.publish();
        }
    }
}

impl Sink for LiveJudge {
    fn begin(&mut self, _unknowns: &[UnknownInfo]) {
        self.expectations.reset();
        self.t = 0.0;
        self.publish();
    }

    /// A point without its pins' currents judges nothing: the host takes every pin while a judge
    /// is installed, and a current read as missing would fail its check. Nor does a point of
    /// another lowering.
    fn values(&mut self, at: f64, net_v: &[f64], pin_i: Option<&[f64]>) {
        let Some(pin_i) = pin_i.filter(|pins| pins.len() == self.pin_net.len() && net_v.len() == self.nets) else { return };
        if self.powers {
            for (power, &(first, count)) in self.part_p.iter_mut().zip(&self.part_pins) {
                *power = pin_i.iter().zip(&self.pin_net).skip(first as usize).take(count as usize)
                    .filter_map(|(&current, &net)| net.map(|net| net_v.get(net as usize).copied().unwrap_or(f64::NAN) * current))
                    .sum();
            }
        }
        self.expectations.step(&Sample { t: at, net_v, pin_i, part_p: &self.part_p });
        self.t = at;
        self.unpublished = true;
        if self.published.is_none_or(|at| at.elapsed() >= PUBLISH) {
            self.publish();
        }
    }

    fn idle(&mut self) {
        self.rest();
    }

    fn ended(&mut self, _at: f64) {
        self.rest();
    }

    fn save(&self) -> Option<Box<dyn SinkState>> {
        Some(Box::new(Saved { expectations: self.expectations.clone(), t: self.t }))
    }

    /// Back to where the judge stood at the snapshot; without one (it came after it), it begins
    /// again from here.
    fn rewind(&mut self, _at: f64, saved: Option<&dyn SinkState>, _unknowns: &[UnknownInfo]) {
        match saved.and_then(|saved| saved.downcast_ref::<Saved>()) {
            Some(saved) => {
                self.expectations = saved.expectations.clone();
                self.t = saved.t;
            }
            None => {
                self.expectations.reset();
                self.t = 0.0;
            }
        }
        self.publish();
    }
}
