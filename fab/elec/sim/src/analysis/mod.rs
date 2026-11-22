//! The analyses and the one entry point, `run` (contract C9 as E3 8.3 states it).

pub mod ac;
pub mod attrib;
pub mod dc;
pub mod exact;
pub mod handover;
pub mod held;
pub mod logic;
pub mod op;
pub mod partition;
pub mod round;
pub mod states;
pub mod tran;

use crate::circuit::SimCircuit;
use crate::newton::{SimDiag, Solver, Stats};
use crate::num::C64;
use crate::stamp::UnknownInfo;
use std::any::Any;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub use ac::AcSweep;
pub use dc::DcSweep;
pub use exact::ExactReport;
pub use op::{operating_point_aids, GminLadder, OpAid, OpSeed, OpStage, OpTry, OP_AIDS};

#[derive(Clone, Debug, PartialEq)]
pub struct TranSpec {
    pub tstop: f64,
    /// The largest step; default `tstop / 50`.
    pub max_step: Option<f64>,
    /// Start from the initial conditions instead of the operating point.
    pub uic: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Analysis {
    Op,
    /// One sweep, or two nested (the first is the outer one).
    Dc(Vec<DcSweep>),
    Ac(AcSweep),
    Tran(TranSpec),
}

/// What a held live run waits for (`LiveValues::held`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldKind {
    /// The run's first plan is being made. Its clock and sound start when the planning call returns.
    Plan,
    /// A streamed input's source (a motion track a Mechanics world appends to) has not yet reached
    /// what the run would read next (`crate::input`).
    Input,
    /// A sink ([`Sink::holds`]): a recording a window ahead of what is heard.
    Other,
}

/// Where results go. `x` is indexed by unknown, ground at 0.
pub trait Sink {
    fn begin(&mut self, _unknowns: &[UnknownInfo]) {}
    /// An operating point (sweep 0), a DC sweep point (its value, and the outer sweep's), or an
    /// accepted time point.
    fn point(&mut self, _at: f64, _outer: Option<f64>, _x: &[f64]) {}
    /// A live run's point just given to `point`, in its lowering's numbering
    /// (`live::ValuePlan`): every net's voltage, and every component pin's current (into the pin)
    /// while the run takes them (a subscription or the traced pins; None otherwise). What the
    /// views read and a bench's checks judge.
    fn values(&mut self, _at: f64, _net_v: &[f64], _pin_i: Option<&[f64]>) {}
    /// Whether it reads `values`: a run whose sinks all say no copies no net values for them.
    /// Asked after each `point`.
    fn takes_values(&self) -> bool {
        true
    }
    /// The points told from here until `rows_end` come as one burst: a round's rows merged from its
    /// blocks, told together once the round is home. A sink that reads the wall clock at every
    /// point may read it once for them.
    fn rows_begin(&mut self) {}
    /// The burst `rows_begin` opened is over.
    fn rows_end(&mut self) {}
    /// The time point just given to `point` is an event: a source edge the step landed on exactly,
    /// or a switching instant (a trigger may lock to it).
    fn mark(&mut self, _at: f64) {}
    /// The step to the time point just given to `point` (from the one before it) is below the run's
    /// time resolution (`Accepted::coarse`): what happened within it is not resolved, and a view
    /// marks it so rather than drawing a confident line.
    fn below_res(&mut self, _at: f64) {}
    /// The step to the time point just given to `point` is free (`Accepted::chosen`): error control
    /// was free to shorten it, so a reader at the run's rate takes the periods it passes over as
    /// resolved. Not told for a step that is a sample, a cut below the resolution, or a piece of
    /// steps merged from several blocks.
    fn chosen(&mut self, _at: f64) {}
    /// The run's sample period: its steps land on the instants `n * period` from 0 (`Stepper::
    /// set_grid`), told after every `begin`. A sink that reads the run's motion at the run's rate
    /// reads it at these instants. Never told: the run has no grid.
    fn sample_period(&mut self, _period: f64) {}
    fn ac_point(&mut self, _f: f64, _x: &[C64]) {}
    /// The live run is paused (on `Pause`, at the end of a `Step`, and after every command applied
    /// while paused): no point follows until it runs again, so what the sink holds (a scope's partial
    /// frame) can be published now.
    fn idle(&mut self) {}
    /// The live run's start is over (its set-up and its first round plan, made on the worker since
    /// the last point): a sink that measures the run's speed measures it from here.
    fn started(&mut self) {}
    /// The live run is steady from `at` (the time of the last point): every value holds at `x`
    /// (indexed as in `point`) until a command, and the clock stops there. A sink may complete what
    /// it shows from the held values, marked as held. `idle` follows.
    fn steady(&mut self, _at: f64, _x: &[f64]) {}
    /// The live run reached the end of its take at `at` (`LiveSpec::until`, the time of the last
    /// point) and rests there: no point follows until an edit sends it back or the end moves
    /// later. What the sink holds of the take is whole now. `idle` follows.
    fn ended(&mut self, _at: f64) {}
    /// True while the sink holds output its consumer refused (a full channel): a run that waits
    /// for a command calls `idle` again, backing off, until this is false, so nothing it published
    /// last is stranded while the run rests.
    fn pending(&self) -> bool { false }
    /// A count that grows as the sink sends what was pending: a resting run that sees it move
    /// asks again soon instead of backing off further.
    fn progress(&self) -> u64 { 0 }
    /// Asked before the sink goes in over a take, at rest (`at_rest`) or still rendering, whose
    /// recording is on `shelf`'s slot `slot`: a recording sink that can play its take from that
    /// recording instead of recording one (no run again) takes it and says Ok; otherwise why not,
    /// as a few words for the person ("voice not recorded in this take (QUIET)").
    fn play_recorded(&mut self, _shelf: &crate::recording::TapeShelf, _slot: u64, _at_rest: bool) -> Result<(), String> {
        Err("this Listen records nothing".to_owned())
    }
    /// True for a sink that hears only a running take (an attached device's speaker, which no
    /// recording holds): it goes in over a take of finite length as it is, neither played from
    /// the take's recording nor with the take run again.
    fn live_only(&self) -> bool { false }
    /// True while the sink asks a live run to wait before it steps on (a recording a window
    /// ahead of what is heard): the run waits for a command, asking again every few milliseconds,
    /// and steps on once no sink asks. Called between steps; a sink may send what waited.
    fn holds(&mut self) -> bool { false }
    /// A paced run waited `wall` for its clock (it was ahead): no stepping, so a sink measuring
    /// the run's speed leaves it out and reads what the run can do, not the pace it was held to.
    fn clock_wait(&mut self, _wall: std::time::Duration) {}
    /// True while a listener hears what this sink is given as the run makes it (an open audio
    /// ring played live): a live run's worker then runs raised (`LiveSim::set_heard_priority`).
    /// O(1).
    fn listened(&self) -> bool { false }
    /// The unknowns this sink samples at the run's sample rate (indexed as in `point`), pushed
    /// onto `out`: an audio tap's voices and their references. While a sink names one, a live run
    /// in rounds steps the block that solves it, and every block that block reads, on the sample
    /// period, so no longer step spans
    /// what is heard (`LiveSpec::own_bound`). Asked as each round opens, before its points: a set
    /// of sinks installs what it has queued first, so what is heard follows the commands, not when
    /// points reach the sinks. The default hears none.
    fn heard(&mut self, _out: &mut Vec<u32>) {}
    /// Sound the attached process played and committed (`CoSim::take_audio`), in run time, given
    /// after the `run` that committed it, while the run goes on. A run that goes back gives the
    /// sound of the stretch it runs again once more.
    fn co_audio(&mut self, _audio: &crate::cosim::CoAudio) {}
    /// The sink's state as of the last point, kept with a live run's snapshot so that a rewind to
    /// that snapshot (`rewind`) goes on exactly as the sink went on from there. None (the default):
    /// the sink keeps no state that way, and a rewind begins it again.
    fn save(&self) -> Option<Box<dyn SinkState>> { None }
    /// A live run went back to `at` (an edit behind its front, `LiveCmd::Edit`): the points after
    /// `at` it was given are superseded, and the next point comes after `at`. `saved` is what
    /// `save` gave at that snapshot, None when it gave none or the sink came after it. The default
    /// begins the sink again on the run's `unknowns`.
    fn rewind(&mut self, _at: f64, _saved: Option<&dyn SinkState>, unknowns: &[UnknownInfo]) {
        self.begin(unknowns)
    }
}

/// A sink's state at a live run's snapshot ([`Sink::save`]): any `Clone + Send` value, handed back
/// as it was at a rewind to that snapshot ([`Sink::rewind`]), where the sink downcasts it.
pub trait SinkState: Any + Send {
    fn clone_state(&self) -> Box<dyn SinkState>;
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any + Send + Clone> SinkState for T {
    fn clone_state(&self) -> Box<dyn SinkState> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Clone for Box<dyn SinkState> {
    fn clone(&self) -> Self {
        (**self).clone_state()
    }
}

impl dyn SinkState {
    /// The state as the sink's own type, when it is that type.
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.as_any().downcast_ref()
    }
}

/// Control messages a run reads between steps.
#[derive(Clone, Default)]
pub struct Control {
    pub stop: Option<Arc<AtomicBool>>,
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub aid: Option<OpAid>,
    /// Global accuracy estimate for an Exact transient; `None` for other runs.
    pub exact: Option<ExactReport>,
    pub points: usize,
    pub stats: Stats,
    /// The steps a transient took only through its gmin ladder (`Rescue`): empty for a run that
    /// converged on its own.
    pub rescues: Vec<crate::newton::Rescue>,
    /// The cost split and rejections of an `Options::profile` run.
    pub profile: Option<Box<crate::newton::Profile>>,
    /// (full factors, refactors).
    pub lu: (u64, u64),
    pub unknowns: usize,
    pub dense: bool,
}

/// Runs one analysis of `c`: a pure function of its input plus the control.
pub fn run(c: &SimCircuit, a: &Analysis, sink: &mut dyn Sink, ctl: &Control) -> Result<Summary, SimDiag> {
    if let Analysis::Tran(spec) = a {
        if c.opts.exact {
            return exact::tran(c, spec, sink, ctl);
        }
    }
    let mut s = Solver::new(c)?;
    sink.begin(&s.unknowns);
    let mut aid = None;
    let points = match a {
        Analysis::Op => {
            aid = Some(op::operating_point(&mut s, false)?);
            sink.point(0.0, None, &s.x);
            1
        }
        Analysis::Dc(sw) => dc::dc(&mut s, sw, sink)?,
        Analysis::Ac(sw) => ac::ac(&mut s, sw, sink)?,
        Analysis::Tran(spec) => {
            let max_step = spec.max_step.unwrap_or(spec.tstop / 50.0);
            let mut st = tran::Stepper::start(&mut s, spec.uic, max_step)?;
            sink.point(0.0, None, &s.x);
            let mut n = 1;
            st.run_to(&mut s, spec.tstop, &mut |sv: &Solver, a: &tran::Accepted| {
                sink.point(a.t, None, &sv.x);
                if a.event {
                    sink.mark(a.t);
                }
                n += 1;
            }, ctl.stop.as_deref())?;
            n
        }
    };
    let rescues = std::mem::take(&mut s.rescues);
    Ok(Summary { aid, exact: None, points, stats: s.stats, rescues, profile: s.profile.take(), lu: s.lu_counts(), unknowns: s.n, dense: s.layout.is_dense() })
}

/// A sink that keeps everything (tests, the AI's probe tool, small runs).
#[derive(Default, Clone)]
pub struct Record {
    pub names: Vec<String>,
    pub at: Vec<f64>,
    pub outer: Vec<Option<f64>>,
    pub x: Vec<Vec<f64>>,
    pub freq: Vec<f64>,
    pub ac: Vec<Vec<C64>>,
}

impl Sink for Record {
    fn begin(&mut self, u: &[UnknownInfo]) {
        self.names = u.iter().map(|u| u.name.clone()).collect();
    }
    fn point(&mut self, at: f64, outer: Option<f64>, x: &[f64]) {
        self.at.push(at);
        self.outer.push(outer);
        self.x.push(x.to_vec());
    }
    fn ac_point(&mut self, f: f64, x: &[C64]) {
        self.freq.push(f);
        self.ac.push(x.to_vec());
    }
}

impl Record {
    pub fn index(&self, name: &str) -> usize {
        if name == "0" {
            return 0;
        }
        self.names.iter().position(|n| n == name).unwrap_or_else(|| panic!("no unknown named {name}"))
    }
    /// The trace of an unknown over the points.
    pub fn v(&self, name: &str) -> Vec<f64> {
        let i = self.index(name);
        self.x.iter().map(|x| x[i]).collect()
    }
    /// The last point's value.
    pub fn last(&self, name: &str) -> f64 {
        let i = self.index(name);
        self.x.last().map(|x| x[i]).unwrap_or(f64::NAN)
    }
    /// Linear interpolation of an unknown at `t`.
    pub fn at_time(&self, name: &str, t: f64) -> f64 {
        let i = self.index(name);
        let k = self.at.partition_point(|&a| a < t);
        if k == 0 {
            return self.x[0][i];
        }
        if k >= self.at.len() {
            return self.x[self.at.len() - 1][i];
        }
        let (t0, t1) = (self.at[k - 1], self.at[k]);
        let (v0, v1) = (self.x[k - 1][i], self.x[k][i]);
        if t1 == t0 {
            v1
        } else {
            v0 + (v1 - v0) * (t - t0) / (t1 - t0)
        }
    }
    pub fn ac_v(&self, name: &str) -> Vec<C64> {
        let i = self.index(name);
        self.ac.iter().map(|x| x[i]).collect()
    }
}
