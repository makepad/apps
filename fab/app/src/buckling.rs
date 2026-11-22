//! A Force study's buckling: the factor on all its loads at which the part bows out sideways,
//! read from the steps the safety factor is read from (the coarser cells, the finest, and the
//! finest shifted half a cell), each by LOBPCG ([`fab_force::buckle::lobpcg`]) with the AI hub's
//! solve to 1e-2 as its preconditioner, the first step from the rigid rotations about the
//! fixtures and the others from the modes of the step before.
//!
//! It never holds the stresses back: it starts on its own job once they are up to date, and is
//! skipped when nothing in the part is in compression. Any edit stops it and marks it stale; the
//! next study's end starts it again, on the board with its progress.
//!
//! The factor said (FORCE A2 1.3): only when the cell sizes agree (the safety factor's
//! [`Verdict`]), and then the Richardson value of the aligned pair, `fine - (coarse - fine) / 3`
//! (the reading is high, second order: the unsafe side), never above the shifted grid's: within
//! 3 % on the bench's columns, strip and plate. Apart, the finest value as "about".

use fab_force::buckle::{lobpcg, Loose, Start};
use fab_force::hub::{self, Hub};
use fab_force::results::{carry, Verdict};
use fab_force::solve::{Level, Options};
use fab_force::{Outcome, V3};
use makepad_widgets::jobs::{JobHandle, JobReporter, JobSpec};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle};
use makepad_widgets::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

/// The preconditioner's tolerance (A2 3.2: a loose solve per vector, not a tight one).
const PRECONDITION: f64 = 1e-2;

/// How high a buckling factor reads at so many cells through its thinnest member (E0's worst
/// read of the bench columns at each count, A2 1.3): at least these cells, up to this share.
const READS_HIGH: [(f64, f64); 5] = [(8.0, 0.04), (6.0, 0.06), (4.0, 0.11), (3.0, 0.17), (2.0, 0.37)];

/// One step's solve the stage reads: its analysed level, its answer, the hub slot it was
/// solved in, its cells.
#[derive(Clone)]
pub struct Input {
    pub level: Arc<Level>,
    pub answer: Arc<Outcome>,
    pub keep: String,
    pub h: f64,
}

/// What it found: the factor at each step (infinite: it does not buckle), and its cost.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub coarse: Option<f64>,
    pub fine: f64,
    pub shifted: Option<f64>,
    /// Cells of the finest step (mm).
    pub h: f64,
    /// Every step settled (else the factor is LOBPCG's last, said so).
    pub settled: bool,
    /// Where a step's stresses come from a static solve stopped above its tolerance (at its
    /// iteration limit): the largest such residual. The factor is then approximate, said so.
    pub stresses_unsolved: Option<f64>,
    /// The preconditioner's solves, and their iterations over the static solves' (the cost in
    /// static solves), over all the steps; how long it took (s).
    pub solves: usize,
    pub static_solves: f64,
    pub seconds: f64,
    /// Per step: its LOBPCG steps, hub solves and cost in its own static solves (the first is
    /// the cold one).
    pub per_step: Vec<(usize, usize, f64)>,
}

impl Found {
    /// The cell sizes' verdict on the factor (as the safety factor's).
    pub fn verdict(&self) -> Verdict {
        Verdict::of(self.fine, self.coarse, self.shifted)
    }

    /// The factor said: the aligned pair's Richardson value when the cell sizes agree, never
    /// above the shifted grid's; None while they disagree.
    pub fn stated(&self) -> Option<f64> {
        match self.verdict() {
            Verdict::Converged { .. } => {
                let c = self.coarse.unwrap_or(self.fine);
                let v = if c > self.fine { self.fine - (c - self.fine) / 3.0 } else { self.fine };
                Some(self.shifted.map_or(v, |s| v.min(s)))
            }
            Verdict::Unchecked(v) => Some(v),
            Verdict::Apart { .. } => None,
        }
    }
}

/// Where the stage is.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    /// No study answered yet.
    #[default]
    Waiting,
    Running(String),
    /// An edit stopped it; the study's next end starts it again.
    Stale,
    /// Nothing in the part is in compression.
    NoCompression,
    Done(Found),
    Failed(String),
}

enum Note {
    Stage(String),
    NoCompression,
    Done(Found),
    Failed(String),
}

#[derive(Default)]
pub struct Stage {
    pub phase: Phase,
    /// A newer count than its ticket stops the job under way.
    live: Arc<AtomicU64>,
    ticket: u64,
    job: Option<TaskHandle<()>>,
    rx: Option<mpsc::Receiver<(u64, Note)>>,
    busy: Option<JobHandle>,
}

impl Stage {
    /// The study changed: the job under way stops, what it found is stale.
    pub fn stale(&mut self) {
        self.live.fetch_add(1, Ordering::Relaxed);
        self.job = None;
        self.rx = None;
        // superseded, not failed: the study's next end starts it again, so its job ends as
        // stopped by a cancel, which goes straight to the history and tells the person nothing
        if let Some(job) = self.busy.take() {
            job.cancelled();
        }
        if !matches!(self.phase, Phase::Waiting) {
            self.phase = Phase::Stale;
        }
    }

    /// The study ended: none yet for these steps, or a stale one.
    pub fn wants(&self) -> bool {
        matches!(self.phase, Phase::Waiting | Phase::Stale) && self.job.is_none()
    }

    /// Starts the stage on `steps` (the coarser step, the finest, the finest shifted; the
    /// coarser and the shifted when the study has them) of `part`, on `hub`.
    pub fn start(&mut self, cx: &mut Cx, hub: Arc<Hub>, part: &str, steps: Vec<Input>) {
        self.ticket = self.live.fetch_add(1, Ordering::Relaxed) + 1;
        let (ticket, live) = (self.ticket, self.live.clone());
        let job = JobSpec::new(format!("Buckling of {part}")).stage("starting").start();
        let report = job.reporter();
        self.busy = Some(job);
        self.phase = Phase::Running("starting".into());
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        match cx.task_pool().submit_named(Class::Background, "fab force buckling", move || {
            run(&hub, &steps, ticket, &live, &report, &tx);
            SignalToUI::set_ui_signal();
        }) {
            Ok(h) => self.job = Some(h),
            Err(e) => self.end(Phase::Failed(format!("could not start: {e:?}"))),
        }
    }

    /// Takes what the job sent; whether the phase changed.
    pub fn take(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        // whether the job ended, asked before its notes are read: all it sent is in by then
        let ended = self.job.as_mut().and_then(|h| h.try_take());
        let mut last = None;
        while let Ok((ticket, n)) = rx.try_recv() {
            if ticket == self.ticket {
                last = Some(n);
            }
        }
        let cancelled = self.busy.as_ref().is_some_and(|j| j.is_cancelled());
        // the job ended with nothing more to say: a panic, or a return without an answer
        if let (Some(ended), false, None | Some(Note::Stage(..))) = (&ended, cancelled, &last) {
            let why = match ended {
                Err(e) => format!("the job stopped: {e:?}"),
                Ok(()) => "the job ended without an answer".to_string(),
            };
            log!("Force buckling: {why}");
            self.end(Phase::Failed(why));
            return true;
        }
        match last {
            None if cancelled => {
                self.live.fetch_add(1, Ordering::Relaxed);
                self.end(Phase::Failed("cancelled".into()));
                true
            }
            None => false,
            Some(Note::Stage(s)) => {
                self.phase = Phase::Running(s);
                true
            }
            Some(Note::NoCompression) => {
                self.end(Phase::NoCompression);
                true
            }
            Some(Note::Done(found)) => {
                if !found.settled {
                    log!("Force buckling: the eigensolver stopped unsettled ({} solves)", found.solves);
                }
                if let Some(r) = found.stresses_unsolved {
                    log!("Force buckling: its stresses come from a solve stopped at residual {r:.0e}, above its tolerance: the factor is approximate");
                }
                self.end(Phase::Done(found));
                true
            }
            Some(Note::Failed(why)) => {
                self.end(Phase::Failed(why));
                true
            }
        }
    }

    fn end(&mut self, phase: Phase) {
        if let Some(job) = self.busy.take() {
            match &phase {
                Phase::Failed(why) if why == "cancelled" => job.cancelled(),
                Phase::Failed(why) => job.fail(why.clone(), false),
                _ => job.done(),
            }
        }
        self.job = None;
        self.rx = None;
        self.phase = phase;
    }

    /// The panel's row: what it found, or what it is doing; `thin`: the cells through the
    /// study's thin parts, where it found some.
    pub fn row(&self, thin: Option<f64>) -> String {
        match &self.phase {
            Phase::Waiting => "after the study".into(),
            Phase::Running(s) => format!("finding: {s}"),
            Phase::Stale => "stale: the part or its loads changed; it runs again after the study".into(),
            Phase::NoCompression => "nothing in compression: it does not buckle".into(),
            Phase::Failed(why) => format!("not found: {why}"),
            Phase::Done(f) if !f.fine.is_finite() => "does not buckle under these loads".into(),
            Phase::Done(f) => {
                let unsettled = match (f.settled, f.stresses_unsolved) {
                    (true, None) => String::new(),
                    (false, None) => ", unsettled".into(),
                    (settled, Some(r)) => format!(
                        ", approximate: {}its stresses' solve stopped at residual {r:.0e}",
                        if settled { "" } else { "unsettled, and " }
                    ),
                };
                let said = match (f.verdict(), f.stated()) {
                    (Verdict::Converged { .. }, Some(v)) => format!("{v:.2}x the loads (within 3 %{unsettled})"),
                    (Verdict::Unchecked(_), Some(v)) => format!("about {v:.2}x the loads (one cell size, unchecked{unsettled})"),
                    _ => {
                        let all: Vec<f64> = [Some(f.fine), f.coarse, f.shifted].into_iter().flatten().collect();
                        let lo = all.iter().copied().fold(f64::INFINITY, f64::min);
                        let hi = all.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                        let apart = if hi.is_finite() { format!("{:.0} %", 100.0 * (hi - lo) / lo) } else { "far".into() };
                        format!("about {:.2}x the loads (cell sizes {apart} apart{unsettled}): Refine narrows it", f.fine)
                    }
                };
                match thin.filter(|c| *c < fab_force::thin::RESOLVED_CELLS) {
                    Some(cells) => match READS_HIGH.iter().find(|r| cells >= r.0) {
                        Some(r) => format!("{said}; its thin parts ({cells:.1} cells) read up to {:.0} % high", 100.0 * r.1),
                        None => format!("{said}; its thin parts ({cells:.1} cells) read far too high"),
                    },
                    None => said,
                }
            }
        }
    }

    /// How it was found, for the panel's tip.
    pub fn how(&self) -> Option<String> {
        match &self.phase {
            Phase::Done(f) => Some(format!(
                "LOBPCG, {} hub solves to {PRECONDITION:.0e} ({:.1} static solves: {}), {:.1} s; factors {:.3} coarse, {:.3} fine, {:.3} shifted",
                f.solves,
                f.static_solves,
                f.per_step.iter().map(|(steps, solves, cost)| format!("{steps} steps, {solves} solves, {cost:.1}")).collect::<Vec<_>>().join("; "),
                f.seconds,
                f.coarse.unwrap_or(f64::NAN),
                f.fine,
                f.shifted.unwrap_or(f64::NAN)
            )),
            _ => None,
        }
    }
}

/// Some cell of the answer in compression: its least principal stress under zero, beyond
/// round-off of its largest stress.
fn compressed(answer: &Outcome) -> bool {
    let r = &answer.results;
    let floor = 1e-9 * r.max_vm.0.max(f64::MIN_POSITIVE);
    r.stress.iter().any(|s| {
        let p = fab_force::element::principal(&s.map(f64::from));
        p.iter().copied().fold(f64::INFINITY, f64::min) < -floor
    })
}

/// The stage on the pool: each step's modes, the first from the rotations, the others from
/// the step before, carried.
fn run(hub: &Hub, steps: &[Input], ticket: u64, live: &AtomicU64, report: &JobReporter, tx: &mpsc::Sender<(u64, Note)>) {
    let stale = || live.load(Ordering::Relaxed) != ticket || report.is_cancelled();
    let send = |n: Note| {
        let _ = tx.send((ticket, n));
        SignalToUI::set_ui_signal();
    };
    let t0 = std::time::Instant::now();
    let Some(fine_input) = steps.get(steps.len().min(2).saturating_sub(1)) else { return };
    if !compressed(&fine_input.answer) {
        return send(Note::NoCompression);
    }
    let opts = Options { tolerance: PRECONDITION, ..Options::default() };
    let names = ["coarser cells", "finest cells", "finest cells shifted"];
    let named: Vec<&str> = if steps.len() == 1 { vec![names[1]] } else { names[..steps.len()].to_vec() };
    let mut factors = Vec::new();
    let (mut solves, mut cost, mut settled) = (0, 0.0, true);
    let mut per_step = Vec::new();
    let mut carried: Option<(Vec<Vec<V3>>, &fab_force::grid::Grid)> = None;
    for (k, step) in steps.iter().enumerate() {
        let start = match carried.take() {
            Some((modes, from)) => Start::Modes(modes.iter().map(|m| carry(from, m, &step.level.grid)).collect()),
            None => Start::Rotations,
        };
        let static_its = step.answer.solution.iterations.max(1) as f64;
        let mut its = 0usize;
        let mut pre = |r: &[Vec<V3>]| {
            let out = hub::solve_u(hub, &step.level, r, &opts, hub::Class::Visible, &step.keep, &mut || stale())?;
            Ok(out.map(|(u, n)| {
                its += n;
                u
            }))
        };
        let of = steps.len() as u32;
        let mut told = |s: usize, mu: f64, res: f64| {
            let stage = format!("{} ({:.2} mm), step {s}: {:.3}x, residual {res:.0e}", named[k], step.h, if mu > 0.0 { 1.0 / mu } else { f64::INFINITY });
            // a step's share: steps taken against the few it usually takes
            let fraction = (k as f32 + s as f32 / (s as f32 + 3.0)) / of as f32;
            report.stage(stage.clone(), Some((k as u32 + 1, of)), Some(fraction));
            send(Note::Stage(stage));
            stale()
        };
        let modes = match lobpcg(&step.level, &step.answer.solution.u, start, &Loose::default(), &mut pre, &mut told) {
            Ok(Some(m)) => m,
            Ok(None) => return,
            Err(e) => return send(Note::Failed(e)),
        };
        if modes.grew {
            log!("Force buckling: {} grew its block to three (a cluster of modes)", named[k]);
        }
        solves += modes.solves;
        cost += its as f64 / static_its;
        per_step.push((modes.steps, modes.solves, its as f64 / static_its));
        settled &= modes.converged;
        factors.push(modes.factor(0));
        carried = Some((modes.vectors, &step.level.grid));
    }
    // the stresses each step's modes are of: their static solves' residuals against the study's
    // tolerance
    let tolerance = Options::default().tolerance;
    let stresses_unsolved = steps.iter().map(|s| &s.answer.solution).filter(|s| !s.solved(tolerance)).map(|s| s.residual).reduce(f64::max);
    let seconds = t0.elapsed().as_secs_f64();
    let found = match factors[..] {
        [fine] => Found { coarse: None, fine, shifted: None, h: steps[0].h, settled, stresses_unsolved, solves, static_solves: cost, seconds, per_step: per_step.clone() },
        [coarse, fine] => Found { coarse: Some(coarse), fine, shifted: None, h: steps[1].h, settled, stresses_unsolved, solves, static_solves: cost, seconds, per_step: per_step.clone() },
        [coarse, fine, shifted, ..] => Found { coarse: Some(coarse), fine, shifted: Some(shifted), h: steps[1].h, settled, stresses_unsolved, solves, static_solves: cost, seconds, per_step: per_step.clone() },
        [] => return,
    };
    send(Note::Done(found));
}
