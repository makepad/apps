//! The assembled system and Newton's method (E3 1.2, 1.5).
//!
//! Each iteration evaluates every bank at `x`, forms `J = G + a0 C` and `r = f + a0 q + hist`,
//! solves `J dx = -r` and steps. It has converged when every unknown moved less than
//! `reltol * max(|x_new|, |x_old|) + vntol` (`abstol` for currents) and no device reported that
//! its current disagreed with its last linearisation or that it limited: the first test alone
//! accepts a stalled iteration.

use crate::analysis::op::OpStage;
use crate::blocks::BlockPlan;
use crate::circuit::{Options, SimCircuit};
use crate::sparse::{Layout, Lu};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk, UnkKind, UnknownInfo};
use crate::tiers::TierPlan;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagKind {
    /// The circuit could not be set up (a missing model, a bad value).
    Setup,
    /// The matrix is singular at an unknown.
    Singular,
    /// No operating point after every aid.
    NoConvergence,
    /// The time step fell below its floor.
    TimestepTooSmall,
    /// The run was stopped by its control.
    Stopped,
}

/// A located diagnostic: names, never row numbers.
#[derive(Clone, Debug)]
pub struct SimDiag {
    pub kind: DiagKind,
    pub message: String,
    /// The nets and devices involved, most relevant first.
    pub at: Vec<String>,
    pub time: Option<f64>,
    /// The last partial solution, by unknown, for inspection.
    pub partial: Option<Vec<f64>>,
}

impl SimDiag {
    pub fn setup(msg: String) -> SimDiag {
        SimDiag { kind: DiagKind::Setup, message: msg, at: Vec::new(), time: None, partial: None }
    }
}

impl std::fmt::Display for SimDiag {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)?;
        if !self.at.is_empty() {
            write!(f, " (at {})", self.at.join(", "))?;
        }
        if let Some(t) = self.time {
            write!(f, " at t = {t:e} s")?;
        }
        Ok(())
    }
}

/// The integrator's contribution to one Newton solve.
#[derive(Clone, Copy)]
pub struct Integ<'a> {
    pub a0: f64,
    pub hist: &'a [f64],
}

/// Extra terms the operating-point aids add.
#[derive(Clone, Copy, Default)]
pub struct Extras<'a> {
    /// Gmin stepping: a conductance from every voltage node to ground.
    pub gshunt: f64,
    /// Held nodes (unknown, value, conductance): initial conditions and nodesets.
    pub hold: &'a [(Unk, f64, f64)],
    /// Pseudo-transient: a conductance `C/h` to the previous solution on every voltage node.
    pub pt: Option<(f64, &'a [f64])>,
}

#[derive(Clone, Copy, Debug)]
pub enum NewtonFail {
    Singular(Unk),
    NoConvergence { noncon: Option<(usize, u32)> },
    NonFinite(Unk),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub loads: u64,
    pub iterations: u64,
    /// Newton solves stopped early after proving an exact iteration cycle.
    pub cycles: u64,
    pub solves: u64,
    pub steps: u64,
    pub rejected: u64,
    /// `rejected` by cause: Newton did not converge; the step was cut to a threshold crossing;
    /// the provisional first step after a restart was too long; the truncation error was too
    /// large.
    pub rejected_newton: u64,
    pub rejected_crossing: u64,
    pub rejected_provisional: u64,
    pub rejected_error: u64,
    /// Steps accepted at the representable step although their truncation error was above tolerance.
    pub floor_steps: u64,
    /// Of those, the ones accepted at a time resolution (`Stepper::set_res`) whose estimate does
    /// not reach back to an unresolved edge's start: such an estimate measures the edge's jump,
    /// which its edge point carries exactly. What happened within the rest is below the resolution
    /// (`Accepted::coarse`).
    pub coarse_steps: u64,
    /// First steps after a pin write that Newton solved from the solution last converged under the
    /// same pins (`Stepper::seed`, a warm start), the Newton iterations those took, and the warm
    /// starts that failed and fell back to the predictor.
    pub warm_starts: u64,
    pub warm_iterations: u64,
    pub warm_misses: u64,
}

/// A time step the transient took only through the gmin ladder, its last resort: Newton failed
/// at the step floor after the Gear 2 retry, and the step was solved again under a conductance
/// from every node to ground, reduced stage by stage (`Stepper`). A run that needed one is
/// visible by its record: the run went on where it used to stop with `TimestepTooSmall`.
#[derive(Clone, Debug, PartialEq)]
pub struct Rescue {
    /// The failed step's start, seconds.
    pub t: f64,
    /// Where Newton failed, as a stop would have reported it (`SimDiag::at`): the device that did
    /// not converge first, then the unknowns with the largest residuals.
    pub at: Vec<String>,
    /// The shunt the step settled at, siemens: 0 when the last stage solved it without one, else a
    /// stage at or below the model's gmin.
    pub gmin: f64,
    /// Ladder stages solved, the exact one included.
    pub stages: u32,
}

/// Why a step was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reject {
    Newton,
    Crossing,
    Provisional,
    Error,
}

/// One rejected step of a profiled run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rejection {
    /// The step's start and its length.
    pub t: f64,
    pub h: f64,
    pub why: Reject,
    /// Steps accepted since the last restart (a corner, a crossing, a value change) before it.
    pub since_restart: u32,
}

/// One provisional point of a profiled run, recorded once confirmed or final at a corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CornerStep {
    /// The restart time the provisional step started from.
    pub t: f64,
    /// Provisional rejections of this point plus one.
    pub attempts: u64,
    /// The first provisional rejection's model z; NaN if absent or that retry fell back.
    pub z_first: f64,
    /// The confirming step's provisional error ratio; NaN if final without confirmation.
    pub p_accepted: f64,
    /// Confirming step length / provisional step length; NaN without confirmation.
    pub k: f64,
    /// The confirming step was judged by the filtered estimate (`STIFF_CARRY`).
    pub stiff: bool,
}

/// Why a Newton iteration did not end the solve (`Profile::open_by`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpenBy {
    /// The junction initialisation of a first iteration.
    InitJunctions,
    /// An instance (bank, instance) reported itself not converged: limited, a current off its line,
    /// a branch flipped (the load's last such instance).
    Instance(u32, u32),
    /// The unknown whose update was the largest past its tolerance, where Fast's single pass was
    /// not asked. Where it was, a moved unknown keeps nothing open by itself: after the first
    /// iteration every live signal has moved since the last point, which Newton starts from.
    Unknown(u32),
    /// Fast's single pass was asked and this nonlinear bank refused to settle (`Bank::settle`),
    /// the first that did.
    Settle(u32),
    /// Nothing moved, but a nonlinear circuit's first iteration never ends a solve.
    FirstIteration,
}

/// Where a profiled run's time goes (`Options::profile`): nanoseconds in device evaluation
/// (`load`), full factorisations, refactorisations and triangular solves, and every rejection.
/// The rest is split into assembly, updates, step control, prediction, history and the sinks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub load_ns: u64,
    pub factor_ns: u64,
    pub refactor_ns: u64,
    pub solve_ns: u64,
    /// Clearing the whole-board load's conductance, capacitance, current and charge arrays.
    pub clear_ns: u64,
    /// Combining the Jacobian and right-hand side, including shunts, holds and pseudo-time.
    pub combine_ns: u64,
    /// Applying Newton's update and testing convergence over the unknowns.
    pub update_ns: u64,
    /// Step setup, integration coefficients, prediction and seeding, outside loads and Newton.
    pub predict_ns: u64,
    /// Threshold scans and the crossing decisions before the truncation estimate.
    pub crossing_ns: u64,
    /// Charge at the solution and the charge and solution derivatives.
    pub charge_ns: u64,
    /// Raw and filtered truncation estimates (with the charge caps of the rows they reach) and
    /// their step choices, outside triangular solves.
    pub estimate_ns: u64,
    /// Accepting points and maintaining their history, outside the caller's sinks.
    pub accept_ns: u64,
    /// Calls to the accepted-point sink.
    pub emit_ns: u64,
    /// Attempt bookkeeping, Newton-failure retries and rescue control, outside loads and Newton.
    pub control_ns: u64,
    /// Comparing and copying the last matrix for the profile's `same_matrix` count.
    pub profile_overhead_ns: u64,
    pub factors: u64,
    pub refactors: u64,
    /// Refactors whose matrix was bit for bit the one factored before it.
    pub same_matrix: u64,
    /// Refactor columns skipped (their inputs unchanged) and recomputed, so far (`Lu::column_counts`).
    pub columns_skipped: u64,
    pub columns_recomputed: u64,
    /// Distinct columns a dirty-column refactor was handed (`Lu::refactor_dirty`: changed in the
    /// matrix), before the ones that read them, and those refactors that followed a move of `a0`
    /// (which moves every capacitive slot); and each bank's evaluation time in tiered loads.
    pub columns_dirty: u64,
    pub a0_moves: u64,
    /// What kept Newton open for another iteration, counted per iteration that did not end it.
    pub open_by: std::collections::BTreeMap<OpenBy, u64>,
    /// Fast's single pass: solves it accepted, and by bank the asks it refused (`Bank::settle`).
    pub settled: u64,
    pub settle_refused: Vec<u64>,
    pub bank_ns: Vec<u64>,
    /// Steps tried and their Newton iterations by kind: a step on its way (0), a step after a
    /// restart, the provisional one or its confirming one (1), a step cut to a crossing (2).
    pub kind_steps: [u64; 3],
    pub kind_iterations: [u64; 3],
    pub rejections: Vec<Rejection>,
    pub corners: Vec<CornerStep>,
    /// The last factored matrix, for `same_matrix`.
    pub last_matrix: Vec<f64>,
    /// Where the work goes (`analysis::attrib`), when the caller sets it up.
    pub attrib: Option<Box<crate::analysis::attrib::Attribution>>,
}

impl Stats {
    pub(crate) fn reject(&mut self, why: Reject) {
        self.rejected += 1;
        match why {
            Reject::Newton => self.rejected_newton += 1,
            Reject::Crossing => self.rejected_crossing += 1,
            Reject::Provisional => self.rejected_provisional += 1,
            Reject::Error => self.rejected_error += 1,
        }
    }
}

fn ns(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[derive(Clone)]
pub struct Solver {
    pub unknowns: Vec<UnknownInfo>,
    pub n: usize,
    /// The device banks. Values change through `set_param`, `retune` and `pin_write`, which keep
    /// the three-tier load's folded stamps current.
    pub banks: Vec<Box<dyn Bank>>,
    pub layout: Layout,
    lu: Lu<f64>,
    pub g: Vec<f64>,
    pub c: Vec<f64>,
    jac: Vec<f64>,
    charge_error_v: Vec<f64>,
    pub f: Vec<f64>,
    pub q: Vec<f64>,
    /// After a solve: dx by unknown (index 0 is 0).
    pub dx: Vec<f64>,
    /// Value index of each voltage unknown's diagonal (0 for currents).
    pub diag: Vec<u32>,
    /// Unknown row and column of each value index (0 for the trash).
    pub slot_row: Vec<u32>,
    pub slot_col: Vec<u32>,
    pub x: Vec<f64>,
    pub opts: Options,
    /// Circuit temperature, kelvin.
    pub temp: f64,
    pub nonlinear: bool,
    pub stats: Stats,
    /// The steps the transient took only through its gmin ladder, in time order.
    pub rescues: Vec<Rescue>,
    /// Opt-in operating-point stage trace; a probe enables it with `Some(Vec::new())`.
    pub op_trace: Option<Vec<OpStage>>,
    /// Largest first-iteration move and its unknown in the last Newton call, for `OpStage`.
    pub(crate) first_move: (f64, u32),
    /// `Some` while `Options::profile` is on.
    pub profile: Option<Box<Profile>>,
    /// Load by block, when `Options::border` cuts the circuit into more than one block.
    pub blocks: Option<Box<BlockPlan>>,
    /// The three-tier load's plan (`tiers`), built at the first transient load with
    /// `Options::tiers` and dropped by a value edit on a folded instance.
    pub tiers: Option<Box<TierPlan>>,
    /// Whether the last load was a tiered one (its matrix changes only where the plan says).
    tiered: bool,
    /// The columns the next refactor recomputes (0-based), reused.
    dirty: Vec<u32>,
    /// Newton's absolute tolerance by unknown: `vntol` for a voltage, `abstol` for a current.
    abs_tol: Vec<f64>,
    /// The node initial conditions and nodesets, by unknown.
    pub ics: Vec<(Unk, f64)>,
    pub nodesets: Vec<(Unk, f64)>,
    /// The values set since it was built (`set_param`): (device, key, value), the newest per
    /// device and key. A solver built anew from the circuit takes them from here.
    pub edits: Vec<(String, String, f64)>,
    /// The circuit's own value under each of `edits`' keys, from before its first edit there
    /// (`Bank::param`), in `edits`' order: None where the bank does not say, or for a part another
    /// device absorbed. An edit back to it is no edit to what the circuit is (`states::key`).
    pub edits_own: Vec<Option<f64>>,
    fresh: bool,
    // Reused Brent checkpoint and comparison scratch, first touched after eight iterations.
    cycle_x: Vec<f64>,
    cycle_history: Vec<f64>,
    iter_history: Vec<f64>,
    cycle_factors: u64,
}

pub const KELVIN: f64 = 273.15;

impl Solver {
    pub fn new(ckt: &SimCircuit) -> Result<Solver, SimDiag> {
        let mut banks = crate::dev::build(ckt).map_err(SimDiag::setup)?;
        let mut topo = Topology::from_nodes(&ckt.nodes);
        for b in banks.iter_mut() {
            b.setup(&mut topo).map_err(SimDiag::setup)?;
        }
        let n = topo.unknowns.len() - 1;
        // a diagonal for every voltage unknown: gmin stepping and pseudo-transient need it
        let mut diag_req = vec![0u32; n + 1];
        for u in 1..=n {
            if topo.unknowns[u].kind == UnkKind::Voltage {
                diag_req[u] = topo.slot(u as Unk, u as Unk);
            }
        }
        let entries: Vec<(u32, u32)> = topo.entries.iter().map(|&(r, c)| (r - 1, c - 1)).collect();
        let (layout, map) = Layout::build(n, &entries, ckt.opts.force_dense);
        let remap = |s: &mut u32| {
            if *s != 0 {
                *s = map[*s as usize - 1];
            }
        };
        for b in banks.iter_mut() {
            b.slots().iter_mut().for_each(remap);
        }
        diag_req.iter_mut().for_each(remap);
        let (rows, cols) = layout.coords();
        let slot_row = rows.iter().map(|&r| if r == u32::MAX { 0 } else { r + 1 }).collect();
        let slot_col = cols.iter().map(|&c| if c == u32::MAX { 0 } else { c + 1 }).collect();
        let len = layout.len();
        let temp = ckt.temp_c + KELVIN;
        for b in banks.iter_mut() {
            b.temperature(temp, ckt.opts.tnom + KELVIN);
        }
        let nonlinear = banks.iter().any(|b| b.nonlinear() && b.len() > 0);
        let mut ics = Vec::new();
        let mut nodesets = Vec::new();
        for (i, nd) in ckt.nodes.iter().enumerate().skip(1) {
            if let Some(v) = nd.ic {
                ics.push((i as Unk, v));
            }
            if let Some(v) = nd.nodeset {
                nodesets.push((i as Unk, v));
            }
        }
        let border: Vec<Unk> = ckt.opts.border.iter().filter_map(|name| topo.unknowns.iter().position(|u| u.name == *name)).filter(|&u| u != 0).map(|u| u as Unk).collect();
        let blocks = if border.is_empty() {
            None
        } else {
            let names: Vec<String> = topo.unknowns.iter().map(|u| u.name.clone()).collect();
            BlockPlan::build(&banks, n, len, &names, border).map(Box::new)
        };
        let abs_tol = topo.unknowns.iter().map(|u| if u.kind == UnkKind::Voltage { ckt.opts.vntol } else { ckt.opts.abstol }).collect();
        Ok(Solver {
            edits: Vec::new(),
            edits_own: Vec::new(),
            blocks,
            tiers: None,
            tiered: false,
            dirty: Vec::new(),
            abs_tol,
            unknowns: topo.unknowns,
            n,
            banks,
            lu: {
                let mut lu = layout.lu();
                lu.set_jit(ckt.opts.plan_jit.clone());
                lu
            },
            layout,
            g: vec![0.0; len],
            c: vec![0.0; len],
            jac: vec![0.0; len],
            charge_error_v: vec![0.0; n + 1],
            f: vec![0.0; n + 1],
            q: vec![0.0; n + 1],
            dx: vec![0.0; n + 1],
            diag: diag_req,
            slot_row,
            slot_col,
            x: vec![0.0; n + 1],
            opts: ckt.opts.clone(),
            temp,
            nonlinear,
            stats: Stats::default(),
            rescues: Vec::new(),
            op_trace: None,
            first_move: (0.0, 0),
            profile: ckt.opts.profile.then(Box::default),
            ics,
            nodesets,
            fresh: true,
            cycle_x: Vec::new(),
            cycle_history: Vec::new(),
            iter_history: Vec::new(),
            cycle_factors: 0,
        })
    }

    /// The unknown of that name (a net, or `name#branch` / `name#internal`).
    pub fn unknown(&self, name: &str) -> Option<Unk> {
        self.unknowns.iter().position(|u| u.name == name).map(|i| i as Unk)
    }

    /// The bank and instance of the device of that name.
    pub fn find_device(&self, name: &str) -> Option<(usize, usize)> {
        self.banks.iter().enumerate().find_map(|(b, bank)| bank.find(name).map(|i| (b, i)))
    }

    /// Changes a device value between solves (sweeps, live mode), kept in `edits`; false if
    /// unknown. A part another device absorbed (a timing cell's resistor) is set on that device
    /// (`Bank::set_absorbed`).
    pub fn set_param(&mut self, name: &str, key: &str, v: f64) -> bool {
        let found = match self.find_device(name) {
            Some((b, i)) => {
                let own = self.banks[b].param(i, key);
                Some((b, i, own, self.banks[b].set_param(i, key, v)))
            }
            None => (0..self.banks.len()).find_map(|b| self.banks[b].set_absorbed(name, key, v).map(|i| (b, i, None, true))),
        };
        let Some((b, i, own, set)) = found else { return false };
        self.edited(b, i);
        self.banks.iter_mut().for_each(|bank| bank.value_edited());
        if set {
            match self.edits.iter_mut().find(|(d, k, _)| d == name && k == key) {
                Some(e) => e.2 = v,
                None => {
                    self.edits.push((name.into(), key.into(), v));
                    self.edits_own.push(own);
                }
            }
        }
        set
    }

    /// Retunes a named independent source between solves; `None` if it is not found or not a source.
    pub fn retune(&mut self, name: &str, t: f64, source: crate::circuit::Source) -> Option<crate::circuit::Retuned> {
        let (b, i) = self.find_device(name)?;
        let retuned = self.banks[b].retune(i, t, source);
        self.edited(b, i);
        self.banks.iter_mut().for_each(|bank| bank.value_edited());
        retuned
    }

    /// Feeds a named boundary source between solves; false if absent or not a boundary.
    /// Only replaces the points it reads: no value edit or integration restart.
    /// A running stepper re-reads its pending event with `Stepper::breaks_changed` after a feed.
    pub fn feed(&mut self, name: &str, table: std::sync::Arc<crate::dev::boundary::Table>, delay: f64) -> bool {
        match self.find_device(name) {
            Some((b, i)) => self.banks[b].feed(i, table, delay),
            None => false,
        }
    }

    /// Writes a GPIO by its bank and instance handle between solves.
    pub fn pin_write(&mut self, dev: (usize, usize), t: f64, mode: crate::circuit::PinMode) -> Option<crate::circuit::Retuned> {
        let bank = self.banks.get_mut(dev.0)?;
        if dev.1 >= bank.len() {
            return None;
        }
        let written = bank.pin_write(dev.1, t, mode);
        self.edited(dev.0, dev.1);
        written
    }

    /// A value of instance `i` of bank `b` may have changed: a plan that folded it, or that would
    /// fold it now, is dropped, and the next transient load builds it again from the new values.
    fn edited(&mut self, b: usize, i: usize) {
        let folded = self.tiers.as_ref().is_some_and(|plan| plan.iterate[b].binary_search(&(i as u32)).is_err());
        if folded || self.banks[b].tier(i).folded() {
            self.tiers = None;
        }
    }

    /// Adds a GPIO voltage watch by its bank and instance handle between solves.
    pub fn pin_watch(&mut self, dev: (usize, usize), id: u32, level: f64, edge: crate::circuit::PinEdge) -> bool {
        let Some(bank) = self.banks.get_mut(dev.0) else { return false };
        dev.1 < bank.len() && bank.pin_watch(dev.1, id, level, edge)
    }

    /// Removes all GPIO watches, retaining any hits already recorded.
    pub fn clear_watches(&mut self) {
        for bank in &mut self.banks {
            bank.clear_watches();
        }
    }

    /// Appends and drains all pending GPIO hits in time order.
    pub fn pin_hits(&mut self, out: &mut Vec<(u32, f64)>) {
        let start = out.len();
        for bank in &mut self.banks {
            bank.pin_hits(out);
        }
        out[start..].sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
    }

    pub fn base_cx(&self) -> EvalCx {
        EvalCx {
            t: 0.0,
            tran: false,
            init_junctions: false,
            src_scale: 1.0,
            limit: true,
            gmin: self.opts.gmin,
            edge_hold: false,
            bypass: false,
            settle: false,
            reltol: self.opts.reltol,
            abstol: self.opts.abstol,
            vntol: self.opts.vntol,
            chgtol: self.opts.chgtol,
            temp: self.temp,
        }
    }

    /// Evaluates the banks at `self.x` into `g`, `c`, `f` and `q`: every instance, or in a
    /// transient (`Options::tiers`) the iterating ones on top of the folded rest (`tiers`), which
    /// writes the same arrays. Returns the non-converged count and the last such (bank, instance).
    pub fn load(&mut self, cx: &EvalCx) -> (u32, Option<(usize, u32)>) {
        self.stats.loads += 1;
        let timed;
        // a transient load folds what does not iterate (`tiers`): every other load is the whole one
        self.tiered = self.opts.tiers && cx.tran && cx.src_scale == 1.0;
        if !self.tiered {
            if let Some(plan) = self.tiers.as_deref_mut() {
                plan.synced = false;
            }
        }
        let (total, at) = if self.tiered {
            if self.tiers.is_none() {
                self.tiers = Some(Box::new(TierPlan::build(&mut self.banks, self.n, self.g.len(), &self.slot_row, &self.slot_col, cx)));
            }
            let plan = self.tiers.as_deref_mut().expect("built above");
            timed = self.profile.is_some().then(Instant::now);
            plan.load(&mut self.banks, &self.x, cx, &mut self.g, &mut self.c, &mut self.f, &mut self.q, self.profile.as_mut().map(|p| &mut p.bank_ns))
        } else if let Some(plan) = self.blocks.as_mut() {
            timed = self.profile.is_some().then(Instant::now);
            plan.load(&mut self.banks, &self.x, cx, None, &mut self.g, &mut self.c, &mut self.f, &mut self.q)
        } else {
            let clearing = self.profile.is_some().then(Instant::now);
            self.g.iter_mut().for_each(|v| *v = 0.0);
            self.c.iter_mut().for_each(|v| *v = 0.0);
            self.f.iter_mut().for_each(|v| *v = 0.0);
            self.q.iter_mut().for_each(|v| *v = 0.0);
            if let (Some(since), Some(p)) = (clearing, self.profile.as_mut()) {
                p.clear_ns += ns(since);
            }
            timed = self.profile.is_some().then(Instant::now);
            let (mut sum, mut last) = (0, None);
            for (bi, b) in self.banks.iter_mut().enumerate() {
                let mut st = Stamps { g: &mut self.g, c: &mut self.c, f: &mut self.f, q: &mut self.q, noncon: 0, noncon_at: 0 };
                b.eval(&self.x, cx, &mut st);
                if st.noncon > 0 {
                    sum += st.noncon;
                    last = Some((bi, st.noncon_at));
                }
            }
            (sum, last)
        };
        if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
            p.load_ns += ns(since);
            if let Some(a) = p.attrib.as_mut() {
                let start = Instant::now();
                a.on_load(&self.x, &self.g, &self.c, &self.f, &self.q);
                a.overhead_ns += ns(start);
            }
        }
        (total, at)
    }

    /// Makes `self.x` every device's last point: one load with limiting off, no solve. The iterate
    /// of a limited Newton is `x` together with the devices' last points (the limiters measure the
    /// next step from them), so a stage that restarts from a saved or zeroed `x` restores both.
    pub fn reseat(&mut self, cx: &EvalCx) {
        self.load(&EvalCx { limit: false, init_junctions: false, ..cx.clone() });
    }

    /// One factor-and-solve of the assembled system; `dx` holds the step.
    fn solve_step(&mut self, integ: Option<Integ>, ex: &Extras) -> Result<(), NewtonFail> {
        let timed = self.profile.is_some().then(Instant::now);
        let n = self.n;
        // After a tiered load without the operating point's extras, the matrix changes only on the
        // iterating instances' slots, and on the folded capacitances' when `a0` moved: those are
        // written, and the columns of those whose value moved are refactored. Otherwise every slot
        // is, and the refactor compares.
        let plain = ex.gshunt == 0.0 && ex.hold.is_empty() && ex.pt.is_none();
        let planned = self.tiered && plain && integ.is_some();
        if !planned {
            // this matrix's folded slots will not hold one `a0`'s values
            if let Some(plan) = self.tiers.as_deref_mut() {
                plan.jac_a0 = f64::NAN;
            }
        }
        let plan = if planned { self.tiers.as_deref_mut() } else { None };
        let (mut partial, mut a0_moved) = (false, false);
        match (integ, plan) {
            (Some(ig), Some(plan)) => {
                let a0 = ig.a0;
                self.dirty.clear();
                if plan.jac_a0.is_nan() {
                    for s in 0..self.jac.len() {
                        self.jac[s] = self.g[s] + a0 * self.c[s];
                    }
                } else {
                    // `jac` holds what the LU loaded last: a slot whose value moved, bit for bit,
                    // makes its column dirty, and no other column changed
                    let jac = &mut self.jac;
                    let (g, c, dirty) = (&self.g, &self.c, &mut self.dirty);
                    let mut write = |slots: &[u32], cols: &[u32]| {
                        for (&s, &col) in slots.iter().zip(cols) {
                            let s = s as usize;
                            let v = g[s] + a0 * c[s];
                            if v.to_bits() != jac[s].to_bits() {
                                jac[s] = v;
                                dirty.push(col);
                            }
                        }
                    };
                    if plan.jac_a0.to_bits() != a0.to_bits() {
                        write(&plan.a0_slots, &plan.a0_slot_cols);
                        a0_moved = true;
                    }
                    write(&plan.nl_slots, &plan.nl_slot_cols);
                    partial = true;
                }
                plan.jac_a0 = a0;
                for i in 0..=n {
                    self.dx[i] = -(self.f[i] + a0 * self.q[i] + ig.hist[i]);
                }
            }
            (Some(ig), None) => {
                for s in 0..self.jac.len() {
                    self.jac[s] = self.g[s] + ig.a0 * self.c[s];
                }
                for i in 0..=n {
                    self.dx[i] = -(self.f[i] + ig.a0 * self.q[i] + ig.hist[i]);
                }
            }
            (None, _) => {
                self.jac.copy_from_slice(&self.g);
                for i in 0..=n {
                    self.dx[i] = -self.f[i];
                }
            }
        }
        if ex.gshunt != 0.0 {
            for u in 1..=n {
                let d = self.diag[u] as usize;
                if d != 0 {
                    self.jac[d] += ex.gshunt;
                    self.dx[u] -= ex.gshunt * self.x[u];
                }
            }
        }
        for &(u, v, gh) in ex.hold {
            let d = self.diag[u as usize] as usize;
            self.jac[d] += gh;
            self.dx[u as usize] -= gh * (self.x[u as usize] - v);
        }
        if let Some((gpt, prev)) = ex.pt {
            for u in 1..=n {
                let d = self.diag[u] as usize;
                if d != 0 {
                    self.jac[d] += gpt;
                    self.dx[u] -= gpt * (self.x[u] - prev[u]);
                }
            }
        }
        if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
            p.combine_ns += ns(since);
            let overhead = Instant::now();
            if partial {
                // the tiered path compared every slot it wrote: no dirty column is a bit-equal
                // matrix, counted without walking the whole matrix
                if !self.fresh && self.dirty.is_empty() {
                    p.same_matrix += 1;
                }
                p.last_matrix.clear();
            } else {
                if !self.fresh && p.last_matrix.len() == self.jac.len() && p.last_matrix.iter().zip(&self.jac).all(|(a, b)| a.to_bits() == b.to_bits()) {
                    p.same_matrix += 1;
                }
                p.last_matrix.clone_from(&self.jac);
            }
            p.profile_overhead_ns += ns(overhead);
        }
        let timed = self.profile.is_some().then(Instant::now);
        let full = self.fresh;
        let r = if full {
            self.lu.factor(&self.jac)
        } else if partial {
            if let Some(p) = self.profile.as_mut() {
                // a column with several moved slots is listed once per slot; the refactor's order
                // does not matter
                self.dirty.sort_unstable();
                self.dirty.dedup();
                p.columns_dirty += self.dirty.len() as u64;
                p.a0_moves += a0_moved as u64;
            }
            self.lu.refactor_dirty(&self.jac, &self.dirty)
        } else {
            self.lu.refactor(&self.jac)
        };
        if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
            if full {
                p.factor_ns += ns(since);
                p.factors += 1;
            } else {
                p.refactor_ns += ns(since);
                p.refactors += 1;
            }
            (p.columns_skipped, p.columns_recomputed) = self.lu.column_counts();
        }
        if let Err(s) = r {
            self.fresh = true;
            return Err(NewtonFail::Singular(s.col + 1));
        }
        self.fresh = false;
        let timed = self.profile.is_some().then(Instant::now);
        self.lu.solve(&mut self.dx[1..]);
        if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
            p.solve_ns += ns(since);
        }
        self.dx[0] = 0.0;
        self.stats.solves += 1;
        Ok(())
    }

    /// Whether `x` moved from `x0` by more than Newton's own test allows at any unknown.
    /// A run at rest does not move, so this is a cheap test before `static_converged`.
    pub fn moved_from(&self, x0: &[f64]) -> bool {
        x0.len() != self.x.len() || (1..=self.n).any(|u| {
            let (x, old) = (self.x[u], x0[u]);
            !((x - old).abs() <= self.opts.reltol * x.abs().max(old.abs()) + self.abs_tol[u])
        })
    }

    /// Whether `x` solves the static system at `cx` (dynamic terms dropped, a step of unbounded
    /// length) by Newton's own test: one load and one solve, every unknown's update within
    /// `reltol * max(|x_new|, |x_old|) + vntol` (`abstol` for currents), and no device reporting
    /// non-convergence. `x` is left as it was; devices are reseated at `x` and the next solve
    /// factors afresh.
    pub fn static_converged(&mut self, cx: &EvalCx) -> bool {
        let (noncon, _) = self.load(cx);
        let mut converged = false;
        if self.solve_step(None, &Extras::default()).is_ok() {
            let mut moved = false;
            for u in 1..=self.n {
                let d = self.dx[u];
                let xo = self.x[u];
                let xn = xo + d;
                if !xn.is_finite() {
                    moved = true;
                    break;
                }
                if d.abs() > self.opts.reltol * xn.abs().max(xo.abs()) + self.abs_tol[u] {
                    moved = true;
                }
            }
            converged = !moved && noncon == 0;
        }
        self.reseat(cx);
        self.fresh = true;
        converged
    }

    /// Newton from `self.x`. `init` starts junctions at their critical voltage on the first
    /// iteration. Returns the iteration count.
    ///
    /// An exact cycle stops with `NoConvergence`: after the first iteration, the next iteration
    /// is a function of `x`, the banks' iteration history and the LU pivots. The values and
    /// history are compared bit for bit, and an unchanged count of kept pivot choices
    /// (`Lu::kept_pivots`) proves the pivots have not changed. Thus this stop never shortens a
    /// converging solve. Brent checkpoints
    /// start at eight iterations, so a Newton converging in under eight pays nothing for them.
    pub fn newton(&mut self, cx: &EvalCx, init: bool, integ: Option<Integ>, ex: &Extras, max_iter: u32) -> Result<u32, NewtonFail> {
        self.first_move = (0.0, 0);
        // a step boundary: Fast takes compiled LU plans here (Exact keeps the interpreted ones)
        if cx.bypass {
            self.lu.adopt();
        }
        let mut cx = cx.clone();
        // never under an operating point's aids: a shunted or held stage is not the circuit's point
        let plain = ex.gshunt == 0.0 && ex.hold.is_empty() && ex.pt.is_none();
        let single_pass = plain && cx.bypass && cx.tran && self.opts.bypass && self.opts.single_pass > 0;
        cx.settle = single_pass && self.opts.single_pass > 1;
        let mut last_noncon = None;
        for iter in 0..max_iter {
            cx.init_junctions = init && iter == 0;
            let (noncon, at) = self.load(&cx);
            if at.is_some() {
                last_noncon = at;
            }
            self.solve_step(integ, ex)?;
            self.stats.iterations += 1;
            let timed = self.profile.is_some().then(Instant::now);
            let mut moved = false;
            let profiling = self.profile.is_some();
            let mut worst = (0.0, 0);
            for u in 1..=self.n {
                let d = self.dx[u];
                if iter == 0 && (self.first_move.1 == 0 || d.abs() > self.first_move.0) {
                    self.first_move = (d.abs(), u as Unk);
                }
                let xo = self.x[u];
                let xn = xo + d;
                if !xn.is_finite() {
                    if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
                        p.update_ns += ns(since);
                    }
                    return Err(NewtonFail::NonFinite(u as Unk));
                }
                let tol = self.opts.reltol * xn.abs().max(xo.abs()) + self.abs_tol[u];
                if d.abs() > tol {
                    moved = true;
                    if profiling && d.abs() > worst.0 * tol {
                        worst = (d.abs() / tol, u);
                    }
                }
                self.x[u] = xn;
            }
            let converged = !moved && noncon == 0 && !cx.init_junctions && (iter > 0 || !self.nonlinear);
            if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
                p.update_ns += ns(since);
            }
            if converged {
                return Ok(iter + 1);
            }
            // Fast's single pass: the next load would leave every nonlinear device's part of the
            // solved system as it is (`Bank::settle`), so its iteration would only confirm `x`.
            // The load's flag judged the line made at the previous point against this load's
            // point; `settle` judges the line just solved at the point it returns, and refuses
            // whatever raised the flag (a limiter that would act, a B branch step, a value not
            // finite, a device without `settle`), so the flag does not gate it.
            let mut refused = None;
            if single_pass && !cx.init_junctions {
                match self.settles(&cx) {
                    Ok(()) => return Ok(iter + 1),
                    Err(b) => refused = Some(b),
                }
            }
            if let Some(p) = self.profile.as_mut() {
                let why = match (cx.init_junctions, at) {
                    (true, _) => OpenBy::InitJunctions,
                    (false, Some((b, i))) if noncon > 0 => OpenBy::Instance(b as u32, i),
                    _ => match refused {
                        Some(b) => OpenBy::Settle(b as u32),
                        None if moved => OpenBy::Unknown(worst.1 as u32),
                        None => OpenBy::FirstIteration,
                    },
                };
                *p.open_by.entry(why).or_default() += 1;
            }
            if iter >= 7 {
                // Iteration eight creates the first checkpoint of this call. Compare before
                // replacing it, and only gather history when every unknown matches exactly.
                if iter > 7 && self.x[1..=self.n].iter().zip(&self.cycle_x).all(|(x, saved)| x.to_bits() == saved.to_bits()) {
                    self.iter_history.clear();
                    for bank in &self.banks {
                        bank.iter_state(&mut self.iter_history);
                    }
                    if self.iter_history.len() == self.cycle_history.len()
                        && self.iter_history.iter().zip(&self.cycle_history).all(|(x, saved)| x.to_bits() == saved.to_bits())
                        && self.lu.kept_pivots() == self.cycle_factors
                    {
                        self.stats.cycles += 1;
                        return Err(NewtonFail::NoConvergence { noncon: last_noncon });
                    }
                }
                if (iter + 1).is_power_of_two() {
                    self.cycle_x.clear();
                    self.cycle_x.extend_from_slice(&self.x[1..=self.n]);
                    self.cycle_history.clear();
                    for bank in &self.banks {
                        bank.iter_state(&mut self.cycle_history);
                    }
                    self.cycle_factors = self.lu.kept_pivots();
                }
            }
        }
        Err(NewtonFail::NoConvergence { noncon: last_noncon })
    }

    /// Asks every nonlinear bank whether it settles at `self.x` (`Bank::settle`), stopping at
    /// the first that does not: that bank is the error.
    fn settles(&mut self, cx: &EvalCx) -> Result<(), usize> {
        let timed = self.profile.is_some().then(Instant::now);
        let x = &self.x;
        let refused = self.banks.iter_mut().position(|b| b.nonlinear() && !b.settle(x, cx));
        if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
            // device evaluation, as a load's
            p.load_ns += ns(since);
            match refused {
                Some(b) => {
                    p.settle_refused.resize(self.banks.len(), 0);
                    p.settle_refused[b] += 1;
                }
                None => p.settled += 1,
            }
        }
        refused.map_or(Ok(()), Err)
    }

    /// The charges at the accepted solution: `q + C dx` from the last load and solve. The
    /// linearised update keeps the discrete charge balance exact (E3 1.2).
    pub fn charge_at_solution(&self, out: &mut [f64]) {
        out.copy_from_slice(&self.q);
        for s in self.c_slots() {
            let cv = self.c[s];
            if cv != 0.0 {
                out[self.slot_row[s] as usize] += cv * self.dx[self.slot_col[s] as usize];
            }
        }
        out[0] = 0.0;
    }

    /// Ascending, every slot `c` may hold a nonzero on after the last load: after a tiered load
    /// the plan's (`TierPlan::c_slots`), otherwise all.
    fn c_slots(&self) -> impl Iterator<Item = usize> + '_ {
        let plan = self.tiers.as_deref().filter(|_| self.tiered);
        let (list, all) = match plan {
            Some(plan) => (&plan.c_slots[..], 0..0),
            None => (&[][..], 1..self.c.len()),
        };
        list.iter().map(|&s| s as usize).chain(all)
    }

    /// Applies the stiff error filter `C (G + a0 C)^-1 a0` to a charge error. `a0 e` is
    /// its current defect; the solve turns that into an error of `x`, and `C` turns it back
    /// into charge, so the row tolerances are unchanged. Valid only while the LU holds this
    /// step's last Newton iteration matrix; it is reused without refactoring.
    pub fn filter_charge_error(&mut self, a0: f64, e: &mut [f64]) {
        let timed = self.profile.is_some().then(Instant::now);
        let v = &mut self.charge_error_v;
        debug_assert_eq!(v.len(), e.len());
        for (v, &error) in v.iter_mut().zip(e.iter()) {
            *v = a0 * error;
        }
        v[0] = 0.0;
        if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
            p.estimate_ns += ns(since);
        }
        let timed = self.profile.is_some().then(Instant::now);
        self.lu.solve(&mut v[1..]);
        if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
            p.solve_ns += ns(since);
        }
        self.stats.solves += 1;
        let timed = self.profile.is_some().then(Instant::now);
        e.fill(0.0);
        let v = &self.charge_error_v;
        for s in self.c_slots() {
            let cv = self.c[s];
            if cv != 0.0 {
                e[self.slot_row[s] as usize] += cv * v[self.slot_col[s] as usize];
            }
        }
        e[0] = 0.0;
        if let (Some(since), Some(p)) = (timed, self.profile.as_mut()) {
            p.estimate_ns += ns(since);
        }
    }

    /// Forces a full factor at the next solve (after a value change that may move pivots).
    pub fn refresh(&mut self) {
        self.fresh = true;
    }

    pub fn lu_counts(&self) -> (u64, u64) {
        self.lu.counts()
    }

    /// The sparse factor's size and work: (entries of L and U, multiply-subtracts a refactor and a
    /// solve do); None for the dense LU or before the first factor.
    pub fn lu_work(&self) -> Option<(usize, usize, usize)> {
        self.lu.work()
    }

    /// What the LU's compiled plan in use cost and made, if any (`Options::plan_jit`).
    pub fn lu_compiled(&self) -> Option<crate::sparse::compiled::CompileStats> {
        self.lu.compiled_stats()
    }

    /// (full factors, those that changed the pivot sequence) so far: what a compiled factor
    /// plan would be rebuilt on.
    pub fn lu_pivots(&self) -> (u64, u64) {
        (self.lu.counts().0, self.lu.pivot_changes())
    }

    /// Names a Newton failure in design terms.
    pub fn diag_of(&self, fail: NewtonFail, kind: DiagKind, what: &str) -> SimDiag {
        let mut at = Vec::new();
        let message = match fail {
            NewtonFail::Singular(u) => {
                at.push(self.unknowns[u as usize].name.clone());
                return SimDiag {
                    kind: DiagKind::Singular,
                    message: format!("{what}: the matrix is singular at {} (a node with no DC path to ground, or a loop of voltage sources and inductors)", self.unknowns[u as usize].name),
                    at,
                    time: None,
                    partial: Some(self.x.clone()),
                };
            }
            NewtonFail::NonFinite(u) => {
                at.push(self.unknowns[u as usize].name.clone());
                format!("{what}: the solution overflowed at {}", self.unknowns[u as usize].name)
            }
            NewtonFail::NoConvergence { noncon } => {
                let mut reason = None;
                if let Some((b, i)) = noncon {
                    at.push(self.banks[b].inst_name(i as usize).to_string());
                    reason = self.banks[b].noncon_reason(i as usize);
                }
                // the rows with the largest residual at the last iterate
                let mut rows: Vec<(f64, usize)> = (1..=self.n).map(|u| (self.f[u].abs(), u)).collect();
                rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                for &(_, u) in rows.iter().take(3) {
                    at.push(self.unknowns[u].name.clone());
                }
                match reason {
                    Some(reason) => format!("{what}: Newton did not converge ({reason})"),
                    None => format!("{what}: Newton did not converge"),
                }
            }
        };
        SimDiag { kind, message, at, time: None, partial: Some(self.x.clone()) }
    }
}
