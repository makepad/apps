//! Topology building and the device interface (E3 1.3).
//!
//! Unknowns are numbered with ground at 0: circuit node `i` is unknown `i`, and the branch
//! currents and internal nodes the devices claim follow. Every value array (matrix values,
//! `f`, `q`) has its index 0 as a trash cell, so a stamp at ground needs no branch.
//!
//! A device bank claims its unknowns and matrix entries in `setup`, then the solver replaces each
//! claimed request id in `slots()` by its value index. `eval` writes, through those slots:
//! `g = df/dx`, `c = dq/dx`, and by row the currents `f` and charges `q`. The integrator turns
//! `dq/dt` into `a0 * q + history` in one place; no device integrates.

use crate::circuit::{NodeInfo, PinEdge, PinMode, Retuned, Source};
use crate::num::C64;
use std::collections::HashMap;

pub type Unk = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnkKind {
    /// A node voltage; its row is a current balance.
    Voltage,
    /// A branch current; its row is a voltage equation.
    Current,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnknownInfo {
    /// The net name, or `comp.path#branch` / `comp.path#internal`.
    pub name: String,
    pub kind: UnkKind,
}

pub struct Topology {
    pub unknowns: Vec<UnknownInfo>,
    /// Claimed (row, col) entries, both nonzero unknowns; request id = index + 1.
    pub entries: Vec<(u32, u32)>,
    branches: HashMap<String, Unk>,
}

impl Topology {
    pub fn from_nodes(nodes: &[NodeInfo]) -> Topology {
        Topology {
            unknowns: nodes.iter().map(|n| UnknownInfo { name: n.name.clone(), kind: UnkKind::Voltage }).collect(),
            entries: Vec::new(),
            branches: HashMap::new(),
        }
    }

    /// A new branch-current unknown owned by the device `owner`.
    pub fn branch(&mut self, owner: &str) -> Unk {
        let u = self.unknowns.len() as Unk;
        self.unknowns.push(UnknownInfo { name: format!("{owner}#branch"), kind: UnkKind::Current });
        self.branches.insert(owner.to_string(), u);
        u
    }

    /// The branch current of `owner` (a voltage source or an inductor), for F, H and K.
    pub fn branch_of(&self, owner: &str) -> Option<Unk> {
        self.branches.get(owner).copied()
    }

    /// A new internal node of `owner`.
    pub fn internal(&mut self, owner: &str, what: &str) -> Unk {
        let u = self.unknowns.len() as Unk;
        self.unknowns.push(UnknownInfo { name: format!("{owner}#{what}"), kind: UnkKind::Voltage });
        u
    }

    /// Claims the entry (r, c). Returns its request id, or 0 (the trash) at ground.
    pub fn slot(&mut self, r: Unk, c: Unk) -> u32 {
        if r == 0 || c == 0 {
            return 0;
        }
        self.entries.push((r, c));
        self.entries.len() as u32
    }

    /// The four entries of a two-terminal element between a and b: [aa, ab, ba, bb].
    pub fn slot4(&mut self, a: Unk, b: Unk) -> [u32; 4] {
        [self.slot(a, a), self.slot(a, b), self.slot(b, a), self.slot(b, b)]
    }
}

/// What a device evaluation writes.
pub struct Stamps<'a> {
    pub g: &'a mut [f64],
    pub c: &'a mut [f64],
    pub f: &'a mut [f64],
    pub q: &'a mut [f64],
    /// Instances whose current disagreed with their last linearisation, or that limited.
    pub noncon: u32,
    /// The last such instance (for the diagnostic).
    pub noncon_at: u32,
}

impl<'a> Stamps<'a> {
    #[inline(always)]
    pub fn not_converged(&mut self, inst: usize) {
        self.noncon += 1;
        self.noncon_at = inst as u32;
    }
    /// A conductance `g` between the two terminals of `s` = [aa, ab, ba, bb].
    #[inline(always)]
    pub fn g4(&mut self, s: &[u32], g: f64) {
        self.g[s[0] as usize] += g;
        self.g[s[1] as usize] -= g;
        self.g[s[2] as usize] -= g;
        self.g[s[3] as usize] += g;
    }
    /// A capacitance `c` between the two terminals of `s`.
    #[inline(always)]
    pub fn c4(&mut self, s: &[u32], c: f64) {
        self.c[s[0] as usize] += c;
        self.c[s[1] as usize] -= c;
        self.c[s[2] as usize] -= c;
        self.c[s[3] as usize] += c;
    }
    /// A current `i` leaving node a and entering node b (through the element from a to b).
    #[inline(always)]
    pub fn i2(&mut self, a: Unk, b: Unk, i: f64) {
        self.f[a as usize] += i;
        self.f[b as usize] -= i;
    }
    /// A charge `q` on node a, `-q` on node b.
    #[inline(always)]
    pub fn q2(&mut self, a: Unk, b: Unk, q: f64) {
        self.q[a as usize] += q;
        self.q[b as usize] -= q;
    }
}

/// The analysis state a device evaluation sees.
#[derive(Clone, Debug)]
pub struct EvalCx {
    /// Time (the DC value of sources is used when `tran` is false).
    pub t: f64,
    pub tran: bool,
    /// The first iteration of an operating point: junctions start at their critical voltage.
    pub init_junctions: bool,
    /// Source stepping: every independent source is scaled by this.
    pub src_scale: f64,
    /// Junction and gate limiting is on (off evaluates a device exactly at `x`: the Jacobian
    /// checks, a curve tracer).
    pub limit: bool,
    /// Conductance across every junction.
    pub gmin: f64,
    /// A transient step's Newton iterations, after `Bank::step_from` named the step's start: a
    /// switch whose control the iterate carries past the band it can enter holds it at that
    /// band's edge (`dev::sw`).
    pub edge_hold: bool,
    /// A transient step's Newton iterations under a time resolution in effect (`Stepper::set_res`,
    /// Fast): a bank whose `Options::bypass` is on may keep a device's last evaluation while its
    /// controlling voltages stay within a tenth of the tolerance. Never in Exact.
    pub bypass: bool,
    /// Fast's single pass may evaluate a device outside its bypass band at the solved point and
    /// accept the point when the device agrees with the line the solve used (`Bank::settle`,
    /// grade 2 of `Options::single_pass`). Only with `bypass`.
    pub settle: bool,
    pub reltol: f64,
    pub abstol: f64,
    pub vntol: f64,
    /// Newton's absolute tolerance on a charge (or a flux), as `abstol` is on a current.
    pub chgtol: f64,
    /// Circuit temperature, kelvin.
    pub temp: f64,
}

impl EvalCx {
    /// Fast's bypass band: `v` lies within a tenth of Newton's tolerance of `at`, the point of a
    /// kept evaluation (`abs` is `vntol` for a voltage, `abstol` for a current).
    #[inline]
    pub fn near(&self, v: f64, at: f64, abs: f64) -> bool {
        (v - at).abs() <= 0.1 * (self.reltol * v.abs().max(at.abs()) + abs)
    }
    /// Newton's device test: a value evaluated afresh agrees with the line a solve used for it
    /// (`abs` is `abstol` for a current, `chgtol` for a charge, `vntol` for a voltage). Never for
    /// a value that is not finite: an evaluation that overflowed agrees with nothing.
    #[inline]
    pub fn agrees(&self, fresh: f64, line: f64, abs: f64) -> bool {
        fresh.is_finite() && line.is_finite() && (fresh - line).abs() <= self.reltol * fresh.abs().max(line.abs()) + abs
    }
}

/// How often an instance's evaluation can change, which decides when a transient Newton iteration
/// evaluates it (the three-tier load, `crate::tiers`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Its stamps (`g`, `c`) are constants and its `f` and `q` are affine in `x` with them:
    /// `f(x) = G x + f(0)` and `q(x) = C x + q(0)` exactly but for rounding. Only a value edit
    /// (`set_param`, `retune`, `pin_write`) changes them. Its `eval` reads no other `EvalCx` field
    /// than `t`, `tran` and `src_scale`, and keeps no iteration history.
    Fixed,
    /// As `Fixed`, but `f(0)` and `q(0)` follow time (a source's waveform): read once per time point.
    Timed,
    /// Evaluated at every Newton iteration.
    Iterate,
}

impl Tier {
    /// Folded into the constant stamps (`Fixed`, `Timed`), not evaluated per iteration.
    pub fn folded(self) -> bool {
        matches!(self, Tier::Fixed | Tier::Timed)
    }
}

/// A bank's copy behind its box: every bank is `Clone`, so a run's state copies whole (a
/// snapshot) with the banks' own state, models and settings.
pub trait BankClone {
    fn clone_box(&self) -> Box<dyn Bank>;
}

impl<T: Bank + Clone + 'static> BankClone for T {
    fn clone_box(&self) -> Box<dyn Bank> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn Bank> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// One device type over all its instances (a struct of arrays, never a box per instance).
/// A value a device stands for in another lowering (`Bank::stands_for`): an unknown's, or a
/// device's state (`Bank::state`), by name.
#[derive(Clone, Debug, PartialEq)]
pub enum Given {
    Unknown(String, f64),
    State(String, Vec<f64>),
}

/// A run's values by name, for a hand-over (`Bank::adopt`): an unknown's value, a device's state.
pub trait RunState {
    fn value(&self, unknown: &str) -> Option<f64>;
    fn state_of(&self, device: &str) -> Option<Vec<f64>>;
}

pub trait Bank: Send + BankClone {
    fn kind(&self) -> &'static str;
    fn len(&self) -> usize;
    fn inst_name(&self, i: usize) -> &str;
    /// Appends the unknowns instance `i` reads from `x` or writes in `f` or `q`, controls
    /// and internal nodes included. Duplicates and ground are allowed.
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>);
    /// Appends instance `i`'s matrix value slots, after `slots` has been mapped by the layout.
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>);
    /// Terminals per instance, numbered as `live::Terminal` documents (R, C, L, switch, diode:
    /// 0 = a, 1 = b; V, I, E, F, G, H, B: 0 = p, 1 = n; Q: 0 = c, 1 = b, 2 = e, 3 = s;
    /// J: 0 = d, 1 = g, 2 = s; the regulator: 0 = vin, 1 = gnd, 2 = vout, 3 = en; GPIO: 0 = pad,
    /// 1 = vdd, 2 = vss; a line (T, O): 0 = 1+, 1 = 1-, 2 = 2+, 3 = 2-). Inductive coupling (K) has none.
    fn terminals(&self) -> usize {
        0
    }
    /// The current INTO each terminal of instance `inst` (positive flows from the node into the device),
    /// at the solution `x` with `xdot` its time derivative (all zeros at DC), evaluated exactly (no
    /// limiting, the device's own conduction plus the rate of its charges): writes `terminals()` values
    /// into `out[..terminals()]`. It must not change the bank's state (`&self`).
    fn terminal_currents(&self, _inst: usize, _x: &[f64], _xdot: &[f64], _cx: &EvalCx, _out: &mut [f64]) {}
    /// Claims unknowns and entries; `Err` is a located message (a missing control branch).
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String>;
    /// The claimed request ids, replaced in place by value indices after the layout is built.
    fn slots(&mut self) -> &mut [u32];
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps);
    /// Evaluates only the listed instances, in order, with their own convergence indices.
    /// The indices are ascending and unique; the full list is identical to `eval`.
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]);
    /// Appends the values the next `eval` reads from earlier evaluations (its iteration
    /// history), so that equal `x` and equal history give an identical next iteration;
    /// banks whose `eval` reads none append nothing.
    fn iter_state(&self, _out: &mut Vec<f64>) {}
    fn nonlinear(&self) -> bool {
        false
    }
    /// Instance `i`'s tier (`Tier`); a bank that does not know answers `Iterate`, which is always
    /// right. The answer may change only through a value edit on that instance.
    fn tier(&self, _i: usize) -> Tier {
        Tier::Iterate
    }
    /// A time step was accepted at `x`.
    fn accept(&mut self, _x: &[f64], _cx: &EvalCx) {}
    /// How many discrete positions `accept` has committed so far, a count that only grows: a
    /// switch pair's flip, a timing cell's. A crossing restart whose accepted point raised it is a
    /// committed crossing, a jump of the circuit at that instant for the record (the stepper's
    /// `jump_at`). Banks with no discrete state answer 0.
    fn commits(&self) -> u64 {
        0
    }
    /// A transient step solves next from `x`, the newest point (accepted, or the provisional
    /// point a confirming step starts from): the point `crossing` measures from. Its evaluations
    /// carry `EvalCx::edge_hold`.
    fn step_from(&mut self, _x: &[f64]) {}
    /// The next waveform corner or event strictly after `t`. An accepted point may add one ahead
    /// (a delay line's delayed corner): the stepper reads it again after every accepted point.
    fn next_break(&self, _t: f64) -> f64 {
        f64::INFINITY
    }
    /// Whether a boundary source has an imported switching instant at `t`.
    fn edge_break_at(&self, _t: f64) -> bool {
        false
    }
    /// From a jump a boundary source's fed values take at `t` (a value step to the next float, as
    /// its exporter lands on a corner), the step its exporter took next, or the end of the source
    /// edge it passed on with the jump (`Accepted::edge_end`, a GPIO ramp's): infinite where none.
    fn jump_step(&self, _t: f64) -> f64 {
        f64::INFINITY
    }
    /// Whether this bank has an ordinary breakpoint at `t`.
    fn plain_break_at(&self, t: f64) -> bool {
        self.next_break(t.next_down()) == t
    }
    /// Whether a stimulus of this bank changes after `t`; banks without time dependence answer false.
    fn varies_after(&self, t: f64) -> bool {
        self.next_break(t).is_finite()
    }
    /// A threshold crossed between two accepted-candidate solutions, as the fraction of the step
    /// (0, 1) at which it crossed: the step is cut there so an event's time error is bounded by
    /// the tolerance, not by the step.
    fn crossing(&self, _x_old: &[f64], _x_new: &[f64]) -> Option<f64> {
        None
    }
    /// Whether a watched quantity moved between two solutions (by more than a thousandth of its
    /// threshold): under a time resolution such a step is resolved as Exact resolves it.
    fn watch_moved(&self, _x_old: &[f64], _x_new: &[f64]) -> bool {
        false
    }
    /// Why instance `inst` did not converge in the last evaluation, when the bank knows more than
    /// its name (a behavioural source's expression); Newton's diagnostic adds it.
    fn noncon_reason(&self, _inst: usize) -> Option<String> {
        None
    }
    /// Initial values of the bank's own unknowns under `uic` (an inductor's initial current).
    fn initial(&self, _x: &mut [f64]) {}
    /// The AC excitation, by row.
    fn ac_rhs(&self, _rhs: &mut [C64]) {}
    /// Adds to the AC matrix `vals` (by value index, over `G + jwC` at the operating point) what
    /// `G + jwC` cannot say at angular frequency `w`: a delay's `exp(-jw td)`, a line's hyperbolic
    /// two-port. Banks without such terms add nothing.
    fn ac_stamp(&self, _w: f64, _vals: &mut [C64]) {}
    /// The longest time step this bank can follow (a delay line reads its own past, which is known
    /// only that far back); infinite by default.
    fn max_step(&self) -> f64 {
        f64::INFINITY
    }
    /// A live run's time resolution (`Stepper::set_res`, 0 is Exact): `next_break` need not report
    /// a corner that only ends a feature shorter than it (a GPIO edge's ramp end). Banks without
    /// such features ignore it.
    fn set_res(&mut self, _res: f64) {}
    /// The cell a run's steps cross without landing in it, seconds (`Stepper::set_own_bound`: a
    /// block on its own bound; 0 for none): a source whose values are closer reads at the cell
    /// (`Source::value_in`). Banks without such sources ignore it.
    fn set_cell(&mut self, _cell: f64) {}
    /// The next corner strictly after `t` that `next_break` leaves out under the time resolution
    /// (the end of a GPIO edge's ramp no longer than it); infinite for none, and always under Exact.
    fn hidden_break(&self, _t: f64) -> f64 {
        f64::INFINITY
    }
    /// A value somewhere in the circuit was edited (`Solver::set_param`, `Solver::retune`): drop
    /// any evaluation kept for reuse (Fast's bypass), so the next iteration evaluates afresh.
    fn value_edited(&mut self) {}
    /// Instance evaluations Fast's bypass skipped so far (a profile count).
    fn bypassed(&self) -> u64 {
        0
    }
    /// Fast's single pass (`Options::single_pass`), asked of a nonlinear bank after a transient
    /// Newton solve under `EvalCx::bypass`, at the solved `x`: would the next load leave this
    /// bank's part of the solved system as it is? True when every iterating instance lies inside
    /// the bypass band of the evaluation whose stamps were just solved (grade 1, exact: the next
    /// load would stamp the same bits and carry each value along the same line), or, with
    /// `EvalCx::settle`, an instance outside it evaluated afresh at `x` agrees with that line by
    /// Newton's device test (`EvalCx::agrees`; grade 2), its fresh evaluation then kept as a
    /// bypass would keep it. An instance whose limiter would act answers false unevaluated, and
    /// one that disagrees keeps its old evaluation, so the next load flags it as today. The band
    /// is measured from the kept evaluation, never from the last accepted point, and a value
    /// edit clears what is kept (`value_edited`). A bank that does not know answers false, so
    /// Newton takes its confirming iteration.
    fn settle(&mut self, _x: &[f64], _cx: &EvalCx) -> bool {
        false
    }
    /// The first instance that cannot run in a transient though it can at DC and in AC (a lossy
    /// line the transient has no exact form for), and why; `None` by default.
    fn tran_unsupported(&self) -> Option<(usize, String)> {
        None
    }
    /// Temperature-dependent derived values (kelvin).
    fn temperature(&mut self, _temp: f64, _tnom: f64) {}
    /// A value change between steps (live mode, sweeps); false if the key is unknown.
    fn set_param(&mut self, _inst: usize, _key: &str, _v: f64) -> bool {
        false
    }
    /// The value `set_param` sets under `key` (any of its names) on instance `inst`, as it stands;
    /// None for a key it does not set, or a bank that does not say.
    fn param(&self, _inst: usize, _key: &str) -> Option<f64> {
        None
    }
    /// A value change between steps of a part an instance absorbed (a timing cell's resistor or
    /// capacitor), by the part's name and its own key: the instance, or None when none absorbed
    /// it or the value cannot be the part's.
    fn set_absorbed(&mut self, _device: &str, _key: &str, _v: f64) -> Option<usize> {
        None
    }
    /// A device with positions (a switch): instance `inst` held closed (`Some(true)`), open
    /// (`Some(false)`) or in the position other than its present one (`None`), whatever its
    /// control, on a planner's copy of the circuit; false for a device without positions.
    fn hold_position(&mut self, _inst: usize, _closed: Option<bool>) -> bool {
        false
    }
    /// Retunes an independent source between steps; `None` if the device is not a source.
    fn retune(&mut self, _inst: usize, _t: f64, _source: Source) -> Option<Retuned> {
        None
    }
    /// A run fed in rounds hands a boundary source its exporter's points and coupling delay;
    /// false for a device that is not a boundary source. Changes no integration history.
    fn feed(&mut self, _inst: usize, _table: std::sync::Arc<crate::dev::boundary::Table>, _delay: f64) -> bool {
        false
    }
    /// Writes a GPIO mode between steps; `None` for an invalid mode or a device without a pin.
    fn pin_write(&mut self, _inst: usize, _t: f64, _mode: PinMode) -> Option<Retuned> {
        None
    }
    /// A pad's mode as it stands (`pin_write`), None for a device without a pin.
    fn pin_mode(&self, _inst: usize) -> Option<PinMode> {
        None
    }
    /// Watches a pad voltage relative to its low rail; false for an invalid watch or a device without a pin.
    fn pin_watch(&mut self, _inst: usize, _id: u32, _level: f64, _edge: PinEdge) -> bool {
        false
    }
    /// Removes all pin watches, retaining any hits already recorded.
    fn clear_watches(&mut self) {}
    /// Appends pending pin hits in time order and clears them, retaining the bank's storage.
    fn pin_hits(&mut self, _out: &mut Vec<(u32, f64)>) {}
    /// Device state that is not in `x` (a switch's position), by instance, for carrying over a
    /// topology change by name.
    fn state(&self, _inst: usize) -> Vec<f64> {
        Vec::new()
    }
    fn set_state(&mut self, _inst: usize, _s: &[f64]) {}
    /// What instance `inst` stands for at `t` in a lowering that keeps the parts it absorbed, for
    /// a hand-over to such a lowering (`analysis::handover`): those parts' unknowns' values and
    /// devices' states, by name (a timing cell: its input's voltage and its switch pair's
    /// positions). Gives nothing by default.
    fn stands_for(&self, _inst: usize, _t: f64, _out: &mut Vec<Given>) {}
    /// Takes, at `t`, what instance `inst` absorbs from a run of a lowering that kept those parts
    /// (`from`, by name); false when something it needs is not there (it keeps its own start).
    /// True by default: an instance that absorbs nothing needs nothing.
    fn adopt(&mut self, _inst: usize, _t: f64, _from: &dyn RunState) -> bool {
        true
    }
    fn find(&self, name: &str) -> Option<usize> {
        (0..self.len()).find(|&i| self.inst_name(i) == name)
    }
}
