//! Transient analysis: variable-step trapezoidal / Gear / Euler with breakpoints (E3 1.7).
//!
//! `Stepper` is the one time loop; `tran` runs it to a stop time, live mode runs it forever.
//!
//! - After every restart (the start, a breakpoint, a switching instant, a live value change) the
//!   history is cut to that point and the next step is backward Euler: a corner is never
//!   integrated at order 2, which is what makes trapezoidal ring.
//! - That first step is provisional: it is confirmed by the error estimate of the step after it,
//!   and if rejected both are retaken with a shorter step from a two-step backward Euler error
//!   model when valid, otherwise the usual square-root reduction with its one-eighth floor.
//!   It is at most a tenth of the way to the next corner, so a short ramp takes several steps.
//! - The confirming step is twice as long unless a corner cuts it. Its raw estimate reads backward
//!   Euler's own error, a quarter of it the provisional step's; through the step's iteration
//!   matrix (`C (G + a0 C)^-1 a0`) the same estimate reads a mode much faster than the step damped,
//!   and `STIFF_CARRY` times that bounds the provisional step's error too. When that bound is the
//!   smaller (the fastest mode is past z = 3) both steps are judged by the filtered estimate, and
//!   the steps after stay backward Euler while their raw estimate is above tolerance: trapezoidal
//!   would carry the undamped remainder of that mode as a sawtooth. Below that crossover every
//!   decision is the raw estimate's.
//! - The step ends exactly on every breakpoint, and is cut to the instant a threshold is crossed.
//! - A Newton failure cuts the step by 8 at order 1. Below `1e-9 * max_step` the stepper retries
//!   once with Gear 2 and gmin raised 10x, then solves that floor step with the gmin ladder
//!   (`Stepper::ladder`, recorded as a `Rescue`), and only then reports the time and the device.
//! - Truncation-error control never shortens a step below a few ulp of t
//!   (`4 eps max(|t|, max_step)`); a step at that floor is accepted and counted in
//!   `Stats::floor_steps`, so a tight tolerance costs steps, never the run.
//! - A live run's time resolution `res` (`Stepper::set_res`; 0 is Exact, bit for bit) leaves what
//!   is shorter than it unresolved: truncation-error control never shortens a step below it (on
//!   an own bound longer than the sample period, `Stepper::set_own_bound`, below it to the period;
//!   a step cut below it, by a corner, a crossing or a failed solve, doubles back to it), the first
//!   step after a restart is at least `res` (or the whole way to the next corner), and the banks
//!   report no corner that only ends such a feature (a GPIO edge's ramp end). Corners and the
//!   advance's ends are still landed on exactly. That first step is a jump no shorter step eases
//!   until the devices' own charges hold it (an LED switching), so its Newton solve gets the
//!   operating point's iterations (`itl1`), not a transient step's. A threshold is crossed where
//!   the history's quadratic extrapolation puts it, or a solved step's linear interpolation: the
//!   step is cut there before it is solved, or once after, and a cut step that still crosses (a
//!   hair early) is taken as the crossing, so an edge costs the cut step and the step on to the
//!   grid (and a trial step when the extrapolation missed it). A watched quantity (what a co-simulated process reads at
//!   its thresholds) keeps Exact's crossing instants: a first step that moves one, or crosses a
//!   threshold while moving one, is cut to the end of the driver's edge the resolution reports no
//!   corner for (a GPIO write's ramp, a known event: the edge in one step) or else halved until it
//!   does neither, then confirmed as a provisional step is; a watched crossing is cut to until it
//!   is found, and a watch moved but not yet crossed is stepped with Exact's floor until the next
//!   restart. Once a watched crossing is found the first step after it is halved only for
//!   crossing a watch, not for moving one on (nothing for a watch that rests).
//!   The values an unresolved first step after a jump (a value change, a corner) reaches hold
//!   from just after the jump: an edge point at the next float after it carries them, so a mean
//!   (a trapezoid) sees a held level and a scope a vertical edge, not a ramp over the step. Where
//!   the jump ran over a source edge the resolution hides (a GPIO ramp), the edge point also
//!   carries that edge's end (`Accepted::edge_end`), and nothing else changes: the whole circuit
//!   leaves the edge unresolved, as it leaves any feature shorter than `res` (a firmware's PWM
//!   stays a few points an edge). A block in rounds that lands the jump, where the plan found a
//!   fast charge held, takes its first step to that end and doubles on from there
//!   (`Bank::jump_step`), as a PWL jump's ramp is stepped.
//! - A crossing at which a device commits a discrete position (a switch pair's or a timing cell's
//!   flip, `Bank::commits`) is a jump of the circuit at that instant for the record: the
//!   estimates whose oldest point is the crossing point measure the flip, so a floor step they
//!   fail is over a jump, not coarse. It keeps a crossing's stepping (its short backward Euler
//!   steps) and gets no edge point.

use crate::circuit::Method;
use crate::integ::{self, History, LteTol};
use crate::newton::{CornerStep, DiagKind, Extras, Integ, NewtonFail, Reject, Rejection, Rescue, Solver};
use std::time::Instant;
use crate::stamp::UnkKind;

// End a contiguous section; `next` starts the next one after updating the counter.
macro_rules! profile_ns {
    ($s:ident, $timed:ident, $field:ident) => {
        if let (Some(since), Some(p)) = ($timed, $s.profile.as_mut()) {
            p.$field += u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX);
        }
    };
    ($s:ident, $timed:ident, $field:ident, next) => {
        profile_ns!($s, $timed, $field);
        $timed = $s.profile.is_some().then(Instant::now);
    };
}

/// The provisional step's backward Euler error over the filtered estimate of its doubled
/// confirming step, at most: `sup_z (1/(1+z) - e^-z) * 3 (1+z)(1+2z)^2 / (8 z^2)` = 1.742 (at
/// z = 5, 1.5 as z grows), rounded up. A constant of backward Euler, not a tolerance (ESIM-Q5).
const STIFF_CARRY: f64 = 7.0 / 4.0;

/// Under a resolution, the longest of the two backward Euler steps that open the history after a
/// switching instant, as a share of the step bound. Measured on drum16's board at a 48 kHz sim rate
/// (its six metal oscillators restart the history about 7 600 times a second), against Exact: an
/// eighth keeps the metal's pitch within 0.3 cents and the line's highs within 1.7 dB (full steps:
/// 3.7 cents flat and 5.2 dB dull at 16 kHz; a quarter: 0.5 cents and 2.7 dB), and at 96 kHz within
/// 0.1 cents and 0.3 dB.
const EDGE_STEP: f64 = 0.125;

/// The per-row current full scale the charge cap is measured against (ESIM-Q9): the largest |qdot|
/// accepted over the whole run or, with a window, over the last one to two half-windows (two
/// buckets of half the window each, rolled when an accepted point passes the current one's end).
#[derive(Clone)]
struct RatePeak {
    cur: Vec<f64>,
    prev: Vec<f64>,
    /// Half the window, and the end of the current bucket (both infinite without a window).
    half: f64,
    end: f64,
}

impl RatePeak {
    fn new(n: usize) -> RatePeak {
        RatePeak { cur: vec![0.0; n], prev: vec![0.0; n], half: f64::INFINITY, end: f64::INFINITY }
    }

    /// Buckets of `span / 2` from `t` on; a span that is not finite and positive keeps no window.
    fn window(&mut self, span: f64, t: f64) {
        (self.half, self.end) = if span.is_finite() && span > 0.0 { (span / 2.0, t + span / 2.0) } else { (f64::INFINITY, f64::INFINITY) };
    }

    /// An accepted point's rates at time `t`.
    fn record(&mut self, t: f64, qdot: &[f64]) {
        if t >= self.end {
            // the current bucket becomes the previous one, or both are older than the window
            let behind = ((t - self.end) / self.half).floor();
            if behind >= 1.0 {
                self.prev.fill(0.0);
            } else {
                std::mem::swap(&mut self.prev, &mut self.cur);
            }
            self.cur.fill(0.0);
            self.end += (behind + 1.0) * self.half;
        }
        for (peak, &rate) in self.cur.iter_mut().zip(qdot) {
            *peak = peak.max(rate.abs());
        }
    }

    fn at(&self, i: usize) -> f64 {
        self.cur[i].max(self.prev[i])
    }
}

/// Row `i`'s cap on its charge-level error allowance after the corner pair: the charge whose
/// discharge through the row's own conductance (tau = C_ii / G_ii) is the tolerated current error,
/// against the row's current full scale (ESIM-Q9); none without both. Read only for the rows the
/// estimate reaches.
fn charge_cap<'a>(s: &'a Solver, peak: &'a RatePeak, qdot: &'a [f64], qdot0: &'a [f64], current_row: &'a [bool], tol: LteTol) -> impl Fn(usize) -> f64 + 'a {
    move |i| {
        let d = s.diag[i] as usize;
        let (c, g) = if i == 0 || d == 0 { (0.0, 0.0) } else { (s.c[d], s.g[d]) };
        if c > 0.0 && g > 0.0 {
            let abs_rate = if current_row[i] { tol.vntol } else { tol.abstol };
            let rate = peak.at(i).max(qdot[i].abs()).max(qdot0[i].abs());
            (c / g) * (tol.reltol * rate + abs_rate)
        } else {
            f64::INFINITY
        }
    }
}

/// The stepper's history restarts by cause. Each cuts the history, and the step after it starts
/// at most a tenth of the way to the next corner and climbs back from there. A step that lands
/// on the end it was given (an attached process's next instant) is none: nothing changed there,
/// and a caller that changes a value restarts the history itself (`Stepper::restart`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Restarts {
    /// A step landed on a corner: a source's breakpoint or a device's (a GPIO edge's ramp end).
    pub corner: u64,
    /// A step was cut to a threshold crossing.
    pub crossing: u64,
    /// A value changed between steps (`Stepper::restart`): a pin write, an edit, a retune.
    pub change: u64,
    /// The gmin ladder rescued a step.
    pub rescue: u64,
    /// Of the `crossing` restarts, the ones whose step was cut to the crossing before its solve,
    /// where the history's extrapolation put it (`Stepper::step`), with no trial step.
    pub crossing_ahead: u64,
    /// Steps cut ahead to a crossing their solve did not reach: accepted points, not restarts.
    pub ahead_short: u64,
    /// Of the `corner` restarts, landings solely on imported switching instants.
    pub corner_edge: u64,
}

impl Restarts {
    pub fn total(&self) -> u64 {
        self.corner + self.crossing + self.change + self.rescue
    }
}

/// A run in rounds counts each block's restarts.
impl std::ops::AddAssign for Restarts {
    fn add_assign(&mut self, o: Restarts) {
        let Restarts { corner, crossing, change, rescue, crossing_ahead, ahead_short, corner_edge } = o;
        self.corner += corner;
        self.crossing += crossing;
        self.change += change;
        self.rescue += rescue;
        self.crossing_ahead += crossing_ahead;
        self.ahead_short += ahead_short;
        self.corner_edge += corner_edge;
    }
}

/// The discrete positions the banks' `accept` has committed so far (`Bank::commits`).
fn bank_commits(s: &Solver) -> u64 {
    s.banks.iter().map(|b| b.commits()).sum()
}

/// Whether every breakpoint at `t` is an imported switching instant.
pub(crate) fn edge_only(s: &Solver, t: f64) -> bool {
    s.banks.iter().any(|b| b.edge_break_at(t)) && !s.banks.iter().any(|b| b.plain_break_at(t))
}

#[derive(Clone)]
pub struct Stepper {
    pub method: Method,
    pub max_order: u8,
    pub max_step: f64,
    /// The longest step the devices allow (`Bank::max_step`); `set_max_step` never exceeds it.
    device_max: f64,
    /// The sample period the steps land on, seconds (`set_grid`); 0 for none.
    grid: f64,
    pub hist: History,
    h: f64,
    /// `hist.pts[0]` is a provisional first step after a restart.
    prov: bool,
    /// The step size the provisional point was taken with, and whether the step bound or the grid
    /// set it.
    prov_h: f64,
    prov_capped: bool,
    /// A stiff mode was stepped over: stay backward Euler while the raw estimate is above 1.
    hold_order1: bool,
    next_bp: f64,
    hist_vec: Vec<f64>,
    q_acc: Vec<f64>,
    qdot: Vec<f64>,
    /// Per-row current full scale the charge cap is measured against.
    rate_peak: RatePeak,
    /// Scratch per-row charge caps for the current step.
    charge_error: Vec<f64>,
    /// The integrator's derivative of `x` at the newest accepted point, and its history term.
    xdot: Vec<f64>,
    x_prev: Vec<f64>,
    /// The history's quadratic extrapolation to a step's end, for a crossing ahead.
    x_ahead: Vec<f64>,
    /// The step being solved was cut ahead to a crossing (`Restarts::crossing_ahead`), from the
    /// step it would have been.
    ahead: Option<f64>,
    /// The next attempt is not cut ahead: it retries a cut ahead that fell short.
    no_ahead: bool,
    current_row: Vec<bool>,
    /// A rejection left the banks at the rejected attempt's iterate; the next attempt reseats them
    /// at the newest accepted point first.
    needs_reseat: bool,
    retried: bool,
    gmin_scale: f64,
    /// History restarts so far, by cause.
    pub restarts: Restarts,
    /// Steps accepted since the last restart.
    since_restart: u32,
    /// The time resolution (`set_res`); 0 is Exact.
    res: f64,
    /// A threshold crossing is being resolved under `res`: Exact's floor until the next restart.
    resolving: bool,
    /// The restart whose first step was cut to a driver's edge end (`Bank::hidden_break`): its
    /// attempts are not halved toward the watch they move.
    known_edge: Option<f64>,
    /// The latest restart was a watched crossing that was resolved: its first step is not halved
    /// for moving a watch, only for crossing one, and the watch's settling after it is resolved
    /// from Exact's first step while it moves (`resolving`), not to the next restart.
    watch_crossed: bool,
    /// The latest restart was a jump (a value change, a corner), not an advance's end, a crossing
    /// or a rescue.
    jumped: bool,
    /// The latest restart was a crossing whose accepted point committed a discrete position
    /// (`Bank::commits`: a switch pair's or a timing cell's flip). It is a jump of the circuit at
    /// that instant for the record: the estimates that reach back to it measure the flip, not
    /// detail the step left out (`jump_at`). Its stepping stays a crossing's, with no edge point.
    committed: bool,
    /// Whether the newest accepted point's `accept` committed a discrete position.
    last_commit: bool,
    /// The latest restart was a switching instant (a crossing): under a resolution its backward
    /// Euler steps are kept short ([`EDGE_STEP`]).
    edged: bool,
    /// The step [`EDGE_STEP`] is a share of; 0 for the bound (`set_own_bound`).
    edge_bound: f64,
    /// Under a resolution, how short truncation-error control may make a step when that is shorter
    /// than the resolution; 0 for the resolution (`set_own_bound`).
    error_floor: f64,
    /// The latest two crossings of this solver's own thresholds, the older first (`crossing_spacing`).
    crossings: [f64; 2],
    /// The restart instant whose edge point goes out with its unresolved first step's point.
    edge_at: Option<f64>,
    /// The end of the source edge shorter than the resolution that the first step after the latest
    /// jump runs over with no corner (`Bank::hidden_break`, a GPIO ramp's); infinite for none. The
    /// jump's edge point carries it (`Accepted::edge_end`).
    edge_end: f64,
    /// The latest unresolved edge's instant (its first step's start), or a committed crossing's:
    /// an estimate that reaches back to it measures that step's jump, not what the step it tests
    /// left out.
    jump_at: f64,
    /// A warm start's Newton start (`seed`), and whether the first step after the restart takes it.
    seed: Vec<f64>,
    warm: bool,
    /// The next attempt is a step cut to a crossing (the profile's step kinds).
    cutting: bool,
    /// The cause of the restart whose climb the steps are in (`attrib::CAUSES`; 0 outside one):
    /// the climb ends at an accepted step of half the bound or more, or one that grew less than
    /// 1.5 times from the climb's last.
    window: u8,
    window_h: f64,
    /// The attempt a profile's attribution is measuring: its role, its cause, the iterations and
    /// loads before it, its start and the observer's time then.
    attempt: Option<(u8, u8, u64, u64, Instant, u64)>,
}

/// An emitted point that is not a coarse step's.
fn plain(t: f64, xdot: &[f64], event: bool) -> Accepted<'_> {
    Accepted { t, xdot, event, coarse: false, chosen: false, edge_end: f64::INFINITY }
}

/// An accepted point as `Emit` hands it over (the solver's `x` is set to it).
pub struct Accepted<'a> {
    pub t: f64,
    /// The integrator's own derivative of `x` there (`History`'s `xdot`): what a capacitor's
    /// current is computed from.
    pub xdot: &'a [f64],
    /// The step landed exactly on a source's breakpoint (an edge) or a switching instant.
    pub event: bool,
    /// The step that ends here was accepted at the time resolution with its error above tolerance,
    /// an error not an unresolved edge's jump (`Stats::coarse_steps`): what happened within it is
    /// not resolved.
    pub coarse: bool,
    /// The step that ends here is free: its length is above the truncation floor in force and was
    /// not set by the step bound or the grid, so error control was free to shorten it and its chord
    /// is the run's (`listen`'s read takes it as resolving what it passes over). False for a step
    /// at the bound or cut by the grid (a sample), one at or below the floor (every step of a run
    /// whose resolution is a period or more, outside a resolved crossing), and an edge point.
    pub chosen: bool,
    /// An edge point's: the end of the source edge shorter than the resolution that its jump ran
    /// over with no corner (a GPIO ramp's, `Bank::hidden_break`), which a block that lands the jump
    /// takes its first step to (`Bank::jump_step`); infinite for none and for every other point.
    pub edge_end: f64,
}

/// One accepted point, handed to the caller in time order.
pub type Emit<'a> = dyn FnMut(&Solver, &Accepted) + 'a;

/// The worst row's charges and tolerance inputs for the provisional retry model.
#[derive(Clone, Copy)]
struct RetryRow<'a> {
    /// Charge at the corner.
    q0: f64,
    /// Charge at the provisional point.
    q1: f64,
    /// Charge at the confirming step's point.
    q2: f64,
    /// The row's `current_row` flag, as `integ::row_tolerance` takes it.
    current_row: bool,
    /// Tolerances used to measure this row's truncation error.
    tol: &'a LteTol,
}

/// Model retry length and z, or None to keep the usual retry. For a mode of time constant tau,
/// z = prov_h / tau and k = h / prov_h: backward Euler leaves a/(1+z), then
/// a/((1+z)(1+kz)). Its provisional ratio is P = a u w / (1+k), with u = z/(1+z)
/// and w = kz/(1+kz); the filter gives rho = 1/(1+kz). The retry has u w = m = 0.81 u w / P,
/// solving k z2^2 (1-m) - m (1+k) z2 - m = 0. As z tends to zero this gives today's
/// z2 = 0.9 z / sqrt(P); the model retry has no floor and is capped by that same reduction.
/// A second pass predicts the worst row's charges at the retry's own two points from the same
/// model and its tolerance there; only a smaller tolerance scales P up by the ratio and shortens
/// the retry, otherwise it is the first pass's bit for bit. Further passes repeat that while the
/// tolerance at the retry's points still falls by more than 1 %: a row charging from rest has a
/// rate term that shrinks as h^2 like the error itself, so one pass leaves the retry about as far
/// over as before (ESIM-Q12). Each pass only shortens the retry, which stays under the first pass's;
/// the tolerance is bounded below by the charge floor, so the passes end.
fn provisional_retry(prov_h: f64, h: f64, p: f64, raw: f64, filtered: f64, row: RetryRow) -> Option<(f64, f64)> {
    if raw == 0.0 {
        return None;
    }
    let rho = filtered.abs() / raw.abs();
    if rho >= 1.0 || !rho.is_finite() {
        return None;
    }
    let k = h / prov_h;
    let z = (1.0 / rho - 1.0) / k;
    if !z.is_finite() || z <= 0.0 {
        return None;
    }
    let kz = k * z;
    let uw = (z / (1.0 + z)) * (kz / (1.0 + kz));
    let m = 0.81 * uw / p;
    let b = m * (1.0 + k);
    let z2 = (b + (b * b + 4.0 * k * m * (1.0 - m)).sqrt()) / (2.0 * k * (1.0 - m));
    let retry = prov_h * z2 / z;
    if !retry.is_finite() || retry <= 0.0 {
        return None;
    }
    let retry1 = retry.min(prov_h * 0.9 / p.sqrt());
    if !(retry1.is_finite() && retry1 > 0.0) {
        return None;
    }

    // Predict the tolerance at the retry's own points from the same model: the line plus
    // backward Euler's signed mode contributions, -a u and -a (1 - g), respectively.
    // Only a smaller tolerance moves the retry, and then only shorter; otherwise the retry
    // stays bit for bit the same as the first pass.
    let a = raw * (1.0 + k) / (k * k * uw); // The mode's size on this row, signed.
    let u = z / (1.0 + z);
    let sh = row.q1 - row.q0 + a * u; // The line's rise over prov_h.
    let tolerance_at = |retry: f64| {
        let z1 = z * retry / prov_h;
        let u1 = z1 / (1.0 + z1);
        let g1 = 1.0 / ((1.0 + z1) * (1.0 + k * z1));
        let q1r = row.q0 + sh * retry / prov_h - a * u1;
        let q2r = row.q0 + sh * (1.0 + k) * retry / prov_h - a * (1.0 - g1);
        integ::row_tolerance(q2r, q1r, (q2r - q1r) / (k * retry), (q1r - row.q0) / retry, k * retry, row.current_row, row.tol)
    };
    // the retry for P scaled by the tolerance's fall
    let retry_for = |p_eff: f64| {
        let m2 = 0.81 * uw / p_eff;
        let b2 = m2 * (1.0 + k);
        let z2 = (b2 + (b2 * b2 + 4.0 * k * m2 * (1.0 - m2)).sqrt()) / (2.0 * k * (1.0 - m2));
        prov_h * z2 / z
    };
    let t1 = tolerance_at(retry1);
    let t0 = integ::row_tolerance(
        row.q2, row.q1, (row.q2 - row.q1) / (k * prov_h), (row.q1 - row.q0) / prov_h, k * prov_h, row.current_row, row.tol,
    );
    if !(t1 < t0) {
        return Some((retry1, z));
    }
    let retry2 = retry_for(p * t0 / t1);
    if !(retry2.is_finite() && retry2 > 0.0) {
        return Some((retry1, z));
    }
    let mut retry = retry2.min(retry1);
    // repeat while the tolerance at the retry's own points still falls
    let mut t_prev = t1;
    loop {
        let t = tolerance_at(retry);
        if !(t < 0.99 * t_prev) {
            break;
        }
        let next = retry_for(p * t0 / t).min(retry1);
        if !(next > 0.0 && next < retry) {
            break;
        }
        retry = next;
        t_prev = t;
    }
    Some((retry, z))
}

impl Stepper {
    /// Starts at t = 0: from the initial conditions (`uic`) or from the operating point with
    /// node initial conditions held.
    pub fn start(s: &mut Solver, uic: bool, max_step: f64) -> Result<Stepper, crate::newton::SimDiag> {
        Stepper::start_from(s, uic, max_step, None)
    }

    /// `start`, the operating point first tried from `seed` (`operating_point_from`).
    pub fn start_from(s: &mut Solver, uic: bool, max_step: f64, seed: Option<&super::op::OpSeed>) -> Result<Stepper, crate::newton::SimDiag> {
        if let Some((name, why)) = s.banks.iter().find_map(|b| b.tran_unsupported().map(|(i, why)| (b.inst_name(i).to_string(), why))) {
            return Err(crate::newton::SimDiag { kind: DiagKind::Setup, message: why, at: vec![name], time: Some(0.0), partial: None });
        }
        if uic {
            s.x.iter_mut().for_each(|v| *v = 0.0);
            for &(u, v) in &s.ics.clone() {
                s.x[u as usize] = v;
            }
            for b in s.banks.iter() {
                b.initial(&mut s.x);
            }
        } else {
            super::op::operating_point_from(s, true, seed)?;
        }
        Ok(Stepper::resume(s, 0.0, max_step))
    }

    /// Starts at time `t` from the state already in `s.x` (live mode after a topology change: the
    /// carried voltages and currents, new unknowns at whatever the caller set). The first step is
    /// backward Euler from that point, so an inconsistent new node settles in it.
    pub fn resume(s: &mut Solver, t: f64, max_step: f64) -> Stepper {
        let mut cx = s.base_cx();
        cx.tran = true;
        cx.t = t;
        s.load(&cx);
        for b in s.banks.iter_mut() {
            b.accept(&s.x, &cx);
        }
        let n1 = s.n + 1;
        let mut hist = History::new(8);
        hist.push(t, &s.x, &s.q, &vec![0.0; n1], &vec![0.0; n1]);
        let current_row = s.unknowns.iter().map(|u| u.kind == UnkKind::Current).collect();
        // a bank that reads its own past (a delay line) bounds the step
        let max_step = s.banks.iter().map(|b| b.max_step()).fold(max_step, f64::min);
        let mut st = Stepper {
            method: s.opts.method,
            max_order: s.opts.max_order,
            device_max: max_step,
            grid: 0.0,
            max_step,
            hist,
            h: 0.0,
            prov: false,
            prov_h: 0.0,
            prov_capped: false,
            hold_order1: false,
            next_bp: f64::INFINITY,
            hist_vec: vec![0.0; n1],
            q_acc: vec![0.0; n1],
            qdot: vec![0.0; n1],
            rate_peak: RatePeak::new(n1),
            charge_error: vec![0.0; n1],
            xdot: vec![0.0; n1],
            x_prev: vec![0.0; n1],
            x_ahead: vec![0.0; n1],
            ahead: None,
            no_ahead: false,
            current_row,
            needs_reseat: false,
            retried: false,
            gmin_scale: 1.0,
            restarts: Restarts::default(),
            since_restart: 0,
            res: 0.0,
            resolving: false,
            known_edge: None,
            watch_crossed: false,
            jumped: false,
            committed: false,
            last_commit: false,
            edged: false,
            edge_bound: 0.0,
            error_floor: 0.0,
            crossings: [f64::NEG_INFINITY; 2],
            edge_at: None,
            edge_end: f64::INFINITY,
            jump_at: f64::NEG_INFINITY,
            seed: Vec::new(),
            warm: false,
            cutting: false,
            window: 0,
            window_h: 0.0,
            attempt: None,
        };
        st.next_bp = st.find_bp(s, t);
        st.h = 1e-3 * max_step.min(st.next_bp - t);
        st
    }

    /// Sets the largest step (live mode's bandwidth setting); takes effect from the next step.
    pub fn set_max_step(&mut self, max_step: f64) {
        self.max_step = max_step.min(self.device_max);
        self.h = self.h.min(self.max_step);
    }

    /// Puts the steps on a grid of `period` seconds from t = 0 (anything but a positive finite
    /// period is none): a step at the bound (`max_step`, at most the grid) that would pass the
    /// next sample ends on it, and the step after it takes up its length again. A live run
    /// stepping at its rate then steps on its samples, and on the knots of its noise sources,
    /// which draw once a period.
    pub fn set_grid(&mut self, period: f64) {
        self.grid = if period > 0.0 && period.is_finite() { period } else { 0.0 };
    }

    /// The period the steps land on (`set_grid`, `set_own_bound`); 0 for none.
    pub fn grid(&self) -> f64 {
        self.grid
    }

    /// Steps of up to `bound` on the grid of sample period `period`; a bound longer than the
    /// period is a whole number of periods, its own grid: a block that follows its own pace, not
    /// the sample rate's. There, under a resolution, truncation-error control still shortens a step
    /// to the period, and a switching instant's backward Euler steps are [`EDGE_STEP`] of the
    /// period, as on the period. A bound of the period or less is the plain setting.
    ///
    /// On its own bound the steps cross the knots of the run's noise sources (one a period): the
    /// banks are told the bound as their cell (`Bank::set_cell`), and a noise source reads as at
    /// that spacing, the same noise band-limited at the same density, its knots on the grid.
    /// Where the bound changes, a noise source's value at that instant changes with it (the means
    /// of other cells): a step of noise, not a corner.
    pub fn set_own_bound(&mut self, s: &mut Solver, bound: f64, period: f64) {
        let own = bound > period;
        self.set_max_step(bound);
        self.set_grid(if own { bound } else { period });
        self.edge_bound = if own { period } else { 0.0 };
        self.error_floor = if own { period } else { 0.0 };
        let cell = if own { bound } else { 0.0 };
        for b in s.banks.iter_mut() {
            b.set_cell(cell);
        }
    }

    /// Whether it steps on its own bound (`set_own_bound` with a bound longer than the period).
    pub fn on_own_bound(&self) -> bool {
        self.error_floor > 0.0
    }

    /// The spacing of this solver's own threshold crossings as seen at `t`: the time between its
    /// latest two, or since the latest when that is longer (infinite before two).
    pub fn crossing_spacing(&self, t: f64) -> f64 {
        let [older, newest] = self.crossings;
        (newest - older).max(t - newest)
    }

    pub fn t(&self) -> f64 {
        self.hist.newest().t
    }

    /// A live run's current full scale forgets what is older than `span` (the record's length),
    /// so one inrush does not loosen the charge cap for the rest of a session. Without a window
    /// (a batch run) the full scale is the whole run's.
    pub fn set_rate_window(&mut self, span: f64) {
        let t = self.t();
        self.rate_peak.window(span, t);
    }

    /// Whether the present point is settled: no bank's stimulus changes after `t` and `x` solves
    /// the static system there (`Solver::static_converged`), with the same evaluation context
    /// a step at `t` would use.
    /// The test runs on a copy of the solver because loading and solving change the devices'
    /// iteration history and the factorization, and the run's next step must be independent
    /// of whether or when anyone checked.
    pub fn steady(&self, s: &Solver) -> bool {
        let t = self.t();
        if s.banks.iter().any(|b| b.varies_after(t)) {
            return false;
        }
        let mut cx = s.base_cx();
        cx.tran = true;
        cx.t = t;
        cx.gmin *= self.gmin_scale;
        s.clone().static_converged(&cx)
    }

    fn find_bp(&self, s: &Solver, t: f64) -> f64 {
        s.banks.iter().map(|b| b.next_break(t)).fold(f64::INFINITY, f64::min)
    }

    /// A few ulp of time: truncation-error control can still advance `t0` at this step size.
    fn exact_floor(&self, t0: f64) -> f64 {
        4.0 * f64::EPSILON * t0.abs().max(self.max_step)
    }

    /// The step truncation-error control never goes below: Exact's, raised to the resolution
    /// except while a crossing is resolved.
    fn truncation_floor(&self, t0: f64) -> f64 {
        let exact = self.exact_floor(t0);
        let res = if self.error_floor > 0.0 { self.res.min(self.error_floor) } else { self.res };
        if self.resolving { exact } else { exact.max(res.min(self.max_step)) }
    }

    /// The first step after a restart at `t`, from the step `h` before it: a tenth of the way to
    /// the next corner (never the advance's end, which is not one), so a ramp shorter than the
    /// step before it is never crossed in one step, whose backward Euler error no estimate after
    /// the corner sees; under a resolution at least `res`, or the whole way to the corner.
    fn first_step(&self, h: f64, t: f64) -> f64 {
        let gap = self.next_bp - t;
        h.min(0.1 * gap).max(self.res.min(gap)).min(self.max_step)
    }

    /// Under a resolution, the end of a source edge shorter than it that the first step from a jump
    /// at `t` runs over with no corner (`Bank::hidden_break`, a GPIO ramp's); infinite for none.
    /// The stepping is the same either way: the jump's edge point only carries it on.
    fn hidden_edge(&self, s: &Solver, t: f64) -> f64 {
        if !(self.res.min(self.max_step) > self.exact_floor(t)) {
            return f64::INFINITY;
        }
        let end = s.banks.iter().map(|b| b.hidden_break(t)).fold(f64::INFINITY, f64::min);
        if end < t + self.h { end } else { f64::INFINITY }
    }

    /// Sets the time resolution, seconds (anything but a positive value is 0, Exact): from the
    /// next step on, what is shorter than it is not resolved (see the module notes). The banks are
    /// told and the next corner is read again; the history is not cut.
    pub fn set_res(&mut self, s: &mut Solver, res: f64) {
        let res = if res > 0.0 { res } else { 0.0 };
        if res == self.res {
            return;
        }
        self.res = res;
        for b in s.banks.iter_mut() {
            b.set_res(res);
        }
        self.next_bp = self.find_bp(s, self.t());
    }

    pub fn res(&self) -> f64 {
        self.res
    }

    /// Cuts the history at the newest point: the next steps are backward Euler. Live mode calls
    /// this after a value change. Under a resolution they run at the full step, not a crossing's
    /// short one (`EDGE_STEP`): they damp the stiff modes the change excites. Short ones left
    /// them ringing under the trapezoidal steps after them, and a supply's current one sample
    /// later was 12 times further off. The cost is a lag of about h^2 / (2 tau) on every RC
    /// charge per change, a pitch flat in proportion to change rate times frequency (drum16 with
    /// a key held: 0.06 cents).
    pub fn restart(&mut self, s: &Solver) {
        // `step` only returns once its newest point is confirmed
        debug_assert!(!self.prov);
        let t = self.t();
        self.hist.restart();
        self.hold_order1 = false;
        self.restarts.change += 1;
        self.begin_window(3);
        self.since_restart = 0;
        self.resolving = false;
        self.known_edge = None;
        self.watch_crossed = false;
        self.jumped = true;
        self.edged = false;
        self.committed = false;
        self.edge_at = None;
        self.warm = false;
        self.next_bp = self.find_bp(s, t);
        self.h = self.first_step(self.h, t);
        self.edge_end = self.hidden_edge(s, t);
    }

    /// Offers `x` as Newton's start for the first step after the restart just made: a solution
    /// converged before under the same pin pattern (a warm start). Only a time resolution takes it,
    /// where that step is an edge's whole jump; a Newton failure from it falls back to the
    /// predictor at the same step. One copy.
    pub fn seed(&mut self, x: &[f64]) {
        self.seed.clear();
        self.seed.extend_from_slice(x);
        self.warm = true;
    }

    /// The newest accepted point's solution.
    pub fn newest_x(&self) -> &[f64] {
        &self.hist.newest().x
    }

    /// A source's breakpoints changed without changing anything before the next one.
    /// Re-reads the next breakpoint without cutting the history.
    pub fn breaks_changed(&mut self, s: &Solver) {
        self.next_bp = self.find_bp(s, self.t());
    }

    /// Takes steps until at least one point is accepted (or `t_end` is reached), calling `emit`
    /// for each accepted point with the solver's `x` set to it.
    pub fn step(&mut self, s: &mut Solver, t_end: f64, emit: &mut Emit) -> Result<(), crate::newton::SimDiag> {
        let mut timed = s.profile.is_some().then(Instant::now);
        let tol = LteTol { reltol: s.opts.reltol, abstol: s.opts.abstol, vntol: s.opts.vntol, chgtol: s.opts.chgtol, trtol: s.opts.trtol };
        let h_min = 1e-9 * self.max_step;
        // The step a failed step is retried at: two instants closer than it are one.
        let sliver = 64.0 * h_min;
        // A call returns only after the provisional point is confirmed or final; keep its
        // optional profile across rollbacks, without recording unfinished attempts.
        let mut corner: Option<CornerStep> = None;
        loop {
            let t0 = self.t();
            if t0 >= t_end {
                profile_ns!(s, timed, predict_ns);
                return Ok(());
            }
            // A corner closer than a sliver is crossed, not landed on: a failed step's retries and the
            // gmin ladder are all clamped to the gap, and a step that short can fail at every one (its
            // a0 C swamps the conductances, and a node held only through capacitors pivots as
            // singular). At the stepper's resolution the two instants are one, the step landed.
            while self.next_bp - t0 < sliver && self.next_bp < t_end {
                let next = self.find_bp(s, self.next_bp);
                if next <= self.next_bp {
                    break;
                }
                self.next_bp = next;
            }
            let floor = self.truncation_floor(t0);
            if self.needs_reseat {
                // The limiters would measure the next attempt's first step from the rejected
                // iterate: restart them, with x, at the newest accepted point (after a rollback,
                // the confirmed one). One load per rejection.
                s.x.copy_from_slice(&self.hist.newest().x);
                let mut cx = s.base_cx();
                cx.tran = true;
                cx.t = t0;
                cx.gmin *= self.gmin_scale;
                profile_ns!(s, timed, predict_ns);
                s.reseat(&cx);
                timed = s.profile.is_some().then(Instant::now);
                self.needs_reseat = false;
            }
            let bp = self.next_bp.min(t_end);
            let gap = bp - t0;
            let mut h = self.h.min(self.max_step);
            // under a resolution the backward Euler steps that open the history after a switching
            // instant are short: each lags a charge by about h^2 / (2 tau), and with an edge
            // anywhere in the circuit restarting all of it, full steps' lags add up to a delay every
            // period of every oscillator (a pitch flat in proportion to its frequency) and damp the
            // highs everywhere
            if self.edged && self.hist.pts.len() <= 2 && !self.hold_order1 && self.res.min(self.max_step) > self.exact_floor(t0) {
                h = h.min(EDGE_STEP * if self.edge_bound > 0.0 { self.edge_bound } else { self.max_step });
            }
            let h_free = h;
            // a step that would pass the next sample ends on it: a run stepping at its rate steps on
            // its grid, while shorter steps (resolving detail) cost nothing extra. It ends on the
            // sample's own instant (`k * grid`), never on `t0 + h`: that sum and the product round
            // apart by |t|, and steps that landed a rounding off their samples (on drum16 from
            // t = 128 s, every one) no longer met the other blocks' samples in the merge (twice the
            // rows) and came a rounding short of each round's end, which was then taken in two
            // halves. From a sample (within the millionth of one the grid's index takes as on it) a
            // step of the bound, the bound being a whole sample, ends on the next by its index: the
            // difference of two products is not compared with the bound. The grid only shortens a
            // step, never lengthens one: such a step keeps the bound's length and only its end is
            // the sample's, since a step a rounding past the bound (and so past Fast's floor) that
            // failed its estimate would be retried at the bound, and snap past it again.
            let mut snapped = false;
            let mut sample = None;
            if self.grid > 0.0 && h >= self.max_step {
                let mut k = (t0 / self.grid).floor() + 1.0;
                if k * self.grid - t0 < 1e-6 * self.grid {
                    k += 1.0;
                }
                let next = k * self.grid;
                if next - t0 < h {
                    h = next - t0;
                    snapped = true;
                    sample = Some((next, h));
                } else if self.max_step >= self.grid && ((t0 / self.grid).round() * self.grid - t0).abs() < 1e-6 * self.grid {
                    snapped = true;
                    sample = Some((next, h));
                }
            }
            // A corner a rounding past the step's end is that end: the step and the gap each come a
            // rounding from the grid, and halving there put a point half a step before every round's
            // end in every block (7 halvings in 10 on drum16's board in rounds). `rounded_from` keeps
            // the step's length before the rounding took it there.
            let mut rounded_from = None;
            if h >= gap - self.exact_floor(t0) {
                if h < gap {
                    rounded_from = Some(h);
                }
                h = gap;
            } else if gap - h < 0.25 * h {
                // a corner just past the step's end: within the free step it is taken whole (a step
                // the grid cut, halved, would end on the same sample and halve again), else in two
                h = if snapped && gap <= h_free { gap } else { 0.5 * gap };
            }
            // Under a resolution a threshold the history's extrapolation puts inside this step is cut
            // to at once (aimed a hair past it, as a trial step's crossing is): an edge costs the
            // cut step and the step on, not a trial step besides. A watched quantity's crossing is
            // resolved below as Exact resolves it, whatever cut it.
            self.ahead = None;
            let no_ahead = std::mem::take(&mut self.no_ahead);
            if !no_ahead && !self.cutting && !self.prov && !self.resolving && self.hist.pts.len() >= 3 && h > sliver && self.res.min(self.max_step) > self.exact_floor(t0) {
                integ::predict(t0 + h, &self.hist, 2, &mut self.x_ahead);
                let x_old = &self.hist.newest().x;
                let cross = s.banks.iter().filter_map(|b| b.crossing(x_old, &self.x_ahead)).fold(f64::INFINITY, f64::min);
                if cross.is_finite() && cross > 1e-3 && cross < 0.999 {
                    self.ahead = Some(self.h);
                    h = (h * cross * (1.0 + 5e-4)).max(sliver);
                    self.cutting = true;
                }
            }
            let lands = h == gap;
            // the sample's own instant while the step is still the one cut to it
            let on_sample = sample.filter(|&(_, cut)| cut == h);
            let t1 = if lands { bp } else { on_sample.map_or(t0 + h, |(next, _)| next) };
            let past = self.hist.pts.len();
            let method = if self.retried { Method::Gear } else { self.method };
            let max_order = if self.retried { 2 } else { self.max_order };
            let order = if self.hold_order1 { 1 } else { integ::order_for(method, max_order, past) };
            let a0 = integ::coefficients(method, order, t1, &self.hist, &mut self.hist_vec);
            // Newton's start. Under a time resolution (Fast) it is the newest point: once an edge
            // is over, the line through the two newest points carries a switching junction on past
            // where it stopped, and a node driven by noise moves at random from step to step (on
            // drum16 the newest point took 9 % fewer iterations whole and 12 to 19 % fewer in the
            // open blocks in rounds, its pitch and bands against Exact as before). Exact starts from
            // the line through the two newest points, whatever the order: a quadratic through a fast
            // transition carries a junction deep into forward bias (a transistor coming out of
            // saturation), and Newton walks a forward junction back by about a thermal voltage an
            // iteration (on drum16 the quadratic start cost Fast 9 % more loads and two and a half
            // times the failed solves)
            let start = if self.res.min(self.max_step) > self.exact_floor(t0) { 0 } else { order.min(1) };
            integ::predict(t1, &self.hist, start, &mut s.x);
            s.x[0] = 0.0;
            let mut cx = s.base_cx();
            cx.tran = true;
            cx.t = t1;
            cx.gmin *= self.gmin_scale;
            cx.edge_hold = true;
            cx.bypass = self.res.min(self.max_step) > self.exact_floor(t0);
            for b in s.banks.iter_mut() {
                b.step_from(&self.hist.newest().x);
            }
            // under a resolution the first step after a restart is a jump no shorter step eases
            // until the devices' own charges hold it: it gets the operating point's iterations
            let jump = past == 1 && self.res.min(self.max_step) > self.exact_floor(t0);
            let itl = if jump { s.opts.itl1 } else { s.opts.itl4 };
            // a warm start: under a resolution the first step after a pin write starts Newton from
            // the solution last converged under the same pins (`seed`), so its junctions need not
            // climb from the old pattern's voltages; a failure falls back to the predictor at the
            // same step, with the limiters back at the newest point
            let kind = if std::mem::take(&mut self.cutting) { 2 } else if past == 1 || self.prov { 1 } else { 0 };
            let iterations_before = s.stats.iterations;
            if let Some(a) = s.profile.as_mut().and_then(|p| p.attrib.as_mut()) {
                let role: u8 = if kind == 2 { 1 } else if past == 1 { 2 } else if self.prov { 3 } else if self.window != 0 { 4 } else { 0 };
                a.class = u8::from(role != 0 || self.window != 0);
                a.ladder((!lands && h == h_free && kind != 2).then(|| ((self.max_step / h).log2().ceil().max(0.0) as u32, order as u8)));
                self.attempt = Some((role, self.window, s.stats.iterations, s.stats.loads, Instant::now(), a.overhead_ns));
            }
            let warm = std::mem::take(&mut self.warm) && past == 1 && self.res.min(self.max_step) > self.exact_floor(t0) && self.seed.len() == s.x.len();
            let mut warmed = None;
            if warm {
                s.x.copy_from_slice(&self.seed);
                s.x[0] = 0.0;
                profile_ns!(s, timed, predict_ns);
                s.reseat(&cx);
                let res = s.newton(&cx, false, Some(Integ { a0, hist: &self.hist_vec }), &Extras::default(), itl);
                timed = s.profile.is_some().then(Instant::now);
                match res {
                    Ok(iterations) => {
                        s.stats.warm_starts += 1;
                        s.stats.warm_iterations += u64::from(iterations);
                        warmed = Some(Ok(iterations));
                    }
                    Err(_) => {
                        s.stats.warm_misses += 1;
                        s.x.copy_from_slice(&self.hist.newest().x);
                        let mut at_newest = cx.clone();
                        at_newest.t = t0;
                        profile_ns!(s, timed, predict_ns);
                        s.reseat(&at_newest);
                        timed = s.profile.is_some().then(Instant::now);
                        integ::predict(t1, &self.hist, start, &mut s.x);
                        s.x[0] = 0.0;
                    }
                }
            }
            profile_ns!(s, timed, predict_ns);
            let res = match warmed {
                Some(res) => res,
                None => s.newton(&cx, false, Some(Integ { a0, hist: &self.hist_vec }), &Extras::default(), itl),
            };
            timed = s.profile.is_some().then(Instant::now);
            s.stats.steps += 1;
            if let Some(p) = s.profile.as_mut() {
                p.kind_steps[kind] += 1;
                p.kind_iterations[kind] += s.stats.iterations - iterations_before;
            }
            if let Err(fail) = res {
                if let (Some(a), Some(att)) = (s.profile.as_mut().and_then(|p| p.attrib.as_mut()), self.attempt) {
                    a.newton_failed(match fail { NewtonFail::NoConvergence { noncon } => noncon, _ => None }, usize::from(att.0));
                }
                self.reject(s, Reject::Newton, t0, h);
                if self.prov {
                    // the confirming step failed: the provisional step is retried shorter, as a
                    // failed step is. At the failed length it can succeed and its confirming step
                    // fail the same way for good, and under a resolution both are as long as one
                    // sample from the restart on.
                    self.rollback(h / 8.0);
                } else {
                    self.h = h / 8.0;
                }
                if self.h < h_min {
                    if !self.retried {
                        self.retried = true;
                        self.gmin_scale = 10.0;
                        self.h = 64.0 * h_min;
                        self.hist.restart();
                        self.edged = false;
                        self.prov = false;
                        self.hold_order1 = false;
                        profile_ns!(s, timed, control_ns, next);
                        continue;
                    }
                    let mut d = s.diag_of(fail, DiagKind::TimestepTooSmall, "time step too small");
                    profile_ns!(s, timed, control_ns);
                    if self.ladder(s, t0, h_min.min(bp - t0), d.at.clone(), emit) {
                        return Ok(());
                    }
                    timed = s.profile.is_some().then(Instant::now);
                    d.message = format!("time step too small at t = {t0:e} s (retried with Gear 2 and gmin x10, then the gmin ladder)");
                    d.time = Some(t0);
                    profile_ns!(s, timed, control_ns);
                    return Err(d);
                }
                profile_ns!(s, timed, control_ns, next);
                continue;
            }
            profile_ns!(s, timed, control_ns, next);
            s.charge_at_solution(&mut self.q_acc);
            for i in 0..self.qdot.len() {
                self.qdot[i] = a0 * self.q_acc[i] + self.hist_vec[i];
            }
            profile_ns!(s, timed, charge_ns, next);
            // threshold crossings: cut the step to the crossing
            let x_old = &self.hist.newest().x;
            let cross = s.banks.iter().filter_map(|b| b.crossing(x_old, &s.x)).fold(f64::INFINITY, f64::min);
            // A crossing is never cut to below a sliver of the bound (the length a failed step is
            // retried at); in a step that short it is taken as found. A control that jumped between
            // solves (a key's press source set live) crosses at the same share of any step from the
            // point before the jump, so cutting to it would shrink the step until it had no length.
            let crosses = cross.is_finite() && cross > 1e-3 && cross < 0.999 && h > sliver;
            // Under a resolution what a co-simulated process reads at its thresholds (a watched
            // quantity) crosses them where Exact has it cross: a first step that moves one at all,
            // or crosses a threshold while moving one, is cut to a driver's edge end or halved (to
            // before the crossing) until it does neither, then confirmed as a provisional step is,
            // and a crossing that moves one is cut with Exact's floor until the next restart. Any
            // other threshold is not searched
            // for below the resolution: its time is the one the step's linear interpolation gives,
            // the step is cut there once, and a cut step that still crosses (a hair early) is taken
            // as the crossing.
            if let (Some(from), false) = (self.ahead, cross.is_finite()) {
                // a cut ahead that fell short of its crossing can end a rounding before the band the
                // crossing enters, and a step on from there fails its solve: the step it was cut from
                // is tried instead, and finds the crossing as any step does
                self.reject(s, Reject::Crossing, t0, h);
                self.restarts.ahead_short += 1;
                self.h = from;
                self.no_ahead = true;
                profile_ns!(s, timed, crossing_ns, next);
                continue;
            }
            let in_effect = self.res.min(self.max_step) > self.exact_floor(t0);
            let watched = in_effect && (past == 1 || crosses) && s.banks.iter().any(|b| b.watch_moved(x_old, &s.x));
            let known = self.known_edge == Some(t0);
            if watched && past == 1 && !known && (crosses || !(self.resolving || self.watch_crossed)) {
                self.reject(s, Reject::Crossing, t0, h);
                self.resolving = true;
                // A driver's edge the resolution reports no end for (a GPIO write's ramp) is a known
                // event: the first step is cut to the edge's end instead of halved toward the watch
                // it moves. That one step holds the whole edge; a watch it crosses is cut to as any
                // watched crossing is, and one it has not crossed yet is resolved on to its crossing.
                let edge = s.banks.iter().map(|b| b.hidden_break(t0)).fold(f64::INFINITY, f64::min);
                if edge - t0 > sliver && edge < t1 - sliver {
                    self.known_edge = Some(t0);
                    self.h = edge - t0;
                } else {
                    self.h = if crosses { (0.5 * h * cross).max(sliver) } else { 0.5 * h };
                }
                self.cutting = true;
                profile_ns!(s, timed, crossing_ns, next);
                continue;
            }
            if crosses && !(in_effect && !watched && !self.resolving && kind == 2) {
                self.reject(s, Reject::Crossing, t0, h);
                if watched {
                    self.resolving = true;
                }
                // aim a hair past it (inside the 0.1 % the acceptance below allows), so the cut step
                // contains the crossing instead of ending a rounding short of it
                self.h = (h * cross * (1.0 + 5e-4)).max(sliver);
                self.cutting = true;
                profile_ns!(s, timed, crossing_ns, next);
                continue;
            }
            // a threshold met at the step's start (a cut that aimed at it ended a rounding short):
            // the event is at t0, so restart there as a crossing does, and never accept a whole step
            // past it; a step that already starts at a restart takes the crossing as found
            if cross.is_finite() && cross <= 1e-3 && past > 1 && !self.prov {
                self.reject(s, Reject::Crossing, t0, h);
                self.hist.restart();
                self.hold_order1 = false;
                self.edged = true;
                self.committed = self.last_commit;
                self.restarts.crossing += 1;
                self.crossings = [self.crossings[1], t0];
                self.begin_window(2);
                self.since_restart = 0;
                self.watch_crossed = self.resolving;
                self.resolving = false;
                self.jumped = false;
                self.edge_at = None;
                self.next_bp = self.find_bp(s, t0);
                self.h = self.first_step(h, t0);
                if self.watch_crossed {
                    self.resolving = true;
                    self.h = h.min(0.1 * (self.next_bp - t0)).min(self.max_step);
                }
                profile_ns!(s, timed, crossing_ns, next);
                continue;
            }
            let crossed = cross.is_finite();
            // a resolved crossing's watch has settled once a step leaves every watch where it was:
            // from there on the resolution holds again
            let settled = self.watch_crossed && self.resolving && !crossed && !s.banks.iter().any(|b| b.watch_moved(x_old, &s.x));
            profile_ns!(s, timed, crossing_ns, next);
            // truncation error, as the charge error itself where the filter may read it
            let held = self.hold_order1 && !self.prov && order == 1;
            // after the corner pair a row's charge allowance is capped by its own current scale
            let capped = !self.prov;
            // a step at the floor is accepted whatever its ratio, and any ratio above 1 marks it
            // and keeps the next step's length alike: its scan stops at the first row over. A floor
            // step a rounding took to its corner is at the floor: retried at the floor, it would take
            // the corner again, and fail again.
            let at_floor = h <= floor || (lands && rounded_from.is_some_and(|h0: f64| h0 <= floor));
            let first_over = !self.prov && !held && at_floor;
            let lte = if (self.prov || held) && order == 1 {
                let cap = capped.then(|| charge_cap(s, &self.rate_peak, &self.qdot, &self.hist.pts[0].qdot, &self.current_row, tol));
                integ::lte_charge_error(method, order, t1, &self.q_acc, &self.hist, &mut self.charge_error)
                    .map(|()| integ::charge_error_ratio(&self.charge_error, t1, &self.q_acc, &self.qdot, &self.hist, &self.current_row, &tol, cap))
            } else {
                let cap = capped.then(|| charge_cap(s, &self.rate_peak, &self.qdot, &self.hist.pts[0].qdot, &self.current_row, tol));
                integ::lte_ratio(method, order, t1, &self.q_acc, &self.qdot, &self.hist, &self.current_row, &tol, cap, first_over)
            };
            let k1 = (order + 1) as f64;
            // The filtered estimate, on the doubled confirming step and on a held step whose raw
            // estimate is not small: (its ratio, the raw and the filtered error on the raw worst row).
            let doubled = self.prov && order == 1 && h == 2.0 * self.prov_h;
            let mut filtered = None;
            if let Some((r, i)) = lte {
                if doubled || (held && r > 0.2) {
                    let raw = self.charge_error[i];
                    profile_ns!(s, timed, estimate_ns);
                    s.filter_charge_error(a0, &mut self.charge_error);
                    timed = s.profile.is_some().then(Instant::now);
                    let cap = capped.then(|| charge_cap(s, &self.rate_peak, &self.qdot, &self.hist.pts[0].qdot, &self.current_row, tol));
                    let (rf, _) = integ::charge_error_ratio(&self.charge_error, t1, &self.q_acc, &self.qdot, &self.hist, &self.current_row, &tol, cap);
                    filtered = Some((rf, raw, self.charge_error[i]));
                }
            }
            // the provisional first step was backward Euler like this one: its own error follows
            // from the same divided difference, scaled by the square of its step; on the doubled
            // confirming step STIFF_CARRY times the filtered ratio bounds it too, and the smaller
            // bound decides (the filtered one only past the crossover, a mode faster than z = 3)
            let p_raw = match lte {
                Some((r, _)) if self.prov => r * (self.prov_h / h).powi(2),
                _ => 0.0,
            };
            let stiff = doubled && filtered.is_some_and(|(rf, ..)| STIFF_CARRY * rf < p_raw);
            let r_prov = match filtered {
                Some((rf, ..)) if stiff => STIFF_CARRY * rf,
                _ => p_raw,
            };
            // the step's own test and growth: the filtered ratio when it decided, or on a held step
            let own = match (lte, filtered) {
                (Some((_, i)), Some((rf, ..))) if stiff || held => Some((rf, i)),
                _ => lte,
            };
            if r_prov > 1.0 && self.prov_h > floor {
                self.reject(s, Reject::Provisional, t0, h);
                let model = if order == 1 {
                    lte.and_then(|(_, i)| {
                        if self.hist.pts.len() < 2 {
                            return None;
                        }
                        let q0 = self.hist.pts[1].q[i];
                        let q1 = self.hist.pts[0].q[i];
                        let q2 = self.q_acc[i];
                        let (raw, f) = match filtered {
                            Some((_, raw, f)) => (raw, f),
                            None => {
                                let raw = self.charge_error[i];
                                profile_ns!(s, timed, estimate_ns);
                                s.filter_charge_error(a0, &mut self.charge_error);
                                timed = s.profile.is_some().then(Instant::now);
                                (raw, self.charge_error[i])
                            }
                        };
                        provisional_retry(self.prov_h, h, p_raw, raw, f, RetryRow { q0, q1, q2, current_row: self.current_row[i], tol: &tol })
                    })
                } else {
                    None
                };
                if let Some(corner) = corner.as_mut() {
                    if corner.attempts == 1 {
                        corner.z_first = model.map_or(f64::NAN, |(_, z)| z);
                    }
                    corner.attempts += 1;
                }
                let hp = self.prov_h * (0.9 / r_prov.sqrt()).max(0.125);
                self.rollback(model.map_or(hp, |(retry, _)| retry).max(floor));
                profile_ns!(s, timed, estimate_ns, next);
                continue;
            }
            // the estimate's oldest point is an unresolved edge's start: it measures the jump, which
            // the edge point carries exactly, so a floor step it fails is not detail left out
            let spans_jump = self.hist.pts.get(order).is_some_and(|p| p.t <= self.jump_at);
            let mut coarse = false;
            // the step bound or the grid set this step's length, not error control: a sample
            let capped_step = snapped || h >= self.max_step;
            let grow = match own {
                Some((r, _)) if r > 1.0 && !at_floor => {
                    self.reject(s, Reject::Error, t0, h);
                    let hn = h * (0.9 * r.powf(-1.0 / k1)).max(0.125);
                    self.h = hn.max(floor);
                    profile_ns!(s, timed, estimate_ns, next);
                    continue;
                }
                Some((r, _)) if r > 1.0 => {
                    s.stats.floor_steps += 1;
                    coarse = floor > self.exact_floor(t0) && !spans_jump;
                    1.0
                }
                Some((r, _)) => {
                    if r > 0.0 {
                        (0.9 * r.powf(-1.0 / k1)).min(2.0)
                    } else {
                        2.0
                    }
                }
                None => 2.0,
            };
            profile_ns!(s, timed, estimate_ns, next);
            // accepted: a stiff confirming step starts the hold, a held step keeps it while its raw
            // estimate is above tolerance
            self.hold_order1 = stiff || (held && lte.is_some_and(|(r, _)| r > 1.0));
            let first_after_restart = past == 1;
            if first_after_restart && self.jumped && !self.resolving && self.res.min(self.max_step) > self.exact_floor(t0) {
                self.edge_at = Some(t0);
                self.jump_at = t0;
            }
            // a committed crossing is a jump for the record only: no edge point, the same stepping
            if first_after_restart && self.committed && !self.resolving && self.res.min(self.max_step) > self.exact_floor(t0) {
                self.jump_at = t0;
            }
            if first_after_restart && s.profile.is_some() && corner.is_none() {
                corner = Some(CornerStep { t: t0, attempts: 1, z_first: f64::NAN, p_accepted: f64::NAN, k: f64::NAN, stiff: false });
            }
            let mut prov_coarse = false;
            // the provisional step's length was free
            let prov_chosen = self.prov_h > floor && !self.prov_capped;
            if self.prov {
                // the provisional point is confirmed by this step's estimate or its step floor; an
                // unresolved edge's first step (its edge point pending) carries a jump, not detail
                if r_prov > 1.0 {
                    s.stats.floor_steps += 1;
                    prov_coarse = !spans_jump && floor > self.exact_floor(t0);
                }
                if let (Some(profile), Some(mut corner)) = (s.profile.as_mut(), corner.take()) {
                    corner.p_accepted = r_prov;
                    corner.k = h / self.prov_h;
                    corner.stiff = stiff;
                    profile.corners.push(corner);
                }
                let p = &self.hist.pts[0];
                self.x_prev.copy_from_slice(&p.x);
                let tp = p.t;
                std::mem::swap(&mut s.x, &mut self.x_prev);
                let mut cx_prov = cx.clone();
                cx_prov.t = tp;
                for b in s.banks.iter_mut() {
                    b.accept(&s.x, &cx_prov);
                }
                self.rate_peak.record(tp, &self.hist.pts[0].qdot);
                if let Some(at) = self.edge_at.take().map(f64::next_up).filter(|&at| at < tp) {
                    profile_ns!(s, timed, accept_ns, next);
                    emit(s, &Accepted { edge_end: self.edge_end, ..plain(at, &self.hist.pts[0].xdot, false) });
                    profile_ns!(s, timed, emit_ns, next);
                }
                if prov_coarse {
                    s.stats.coarse_steps += 1;
                }
                profile_ns!(s, timed, accept_ns, next);
                emit(s, &Accepted { t: tp, xdot: &self.hist.pts[0].xdot, event: false, coarse: prov_coarse, chosen: prov_chosen, edge_end: f64::INFINITY });
                profile_ns!(s, timed, emit_ns, next);
                std::mem::swap(&mut s.x, &mut self.x_prev);
                self.prov = false;
            }
            profile_ns!(s, timed, accept_ns, next);
            // the same coefficients applied to x: the integrator's own derivative of x
            integ::rate_of(method, order, t1, &self.hist, &s.x, &mut self.xdot, |p| (p.x.as_slice(), p.xdot.as_slice()));
            self.xdot[0] = 0.0;
            profile_ns!(s, timed, charge_ns, next);
            if let Some(a) = s.profile.as_mut().and_then(|p| p.attrib.as_mut()) {
                a.accepted(&self.hist.newest().x, &s.x, s.opts.reltol, s.opts.vntol);
            }
            self.finish(s, 0);
            if self.window != 0 && (h >= 0.5 * self.max_step || (self.window_h > 0.0 && h < 1.5 * self.window_h)) {
                self.window = 0;
            }
            self.window_h = h;
            if settled {
                self.resolving = false;
            }
            // the step's vectors go into the history as they are: from here on its newest point holds them
            self.hist.push_taking(t1, &s.x, &mut self.q_acc, &mut self.qdot, &mut self.xdot);
            self.since_restart = self.since_restart.saturating_add(1);
            let event = (lands && t1 == self.next_bp) || crossed;
            if first_after_restart && !lands && !crossed {
                self.prov = true;
                self.prov_h = h;
                self.prov_capped = snapped || h >= self.max_step;
            } else {
                if let (Some(profile), Some(corner)) = (s.profile.as_mut(), corner.take()) {
                    // The first point landed on a corner or the advance's end: no confirming test.
                    profile.corners.push(corner);
                }
                let commits = bank_commits(s);
                for b in s.banks.iter_mut() {
                    b.accept(&s.x, &cx);
                }
                self.last_commit = bank_commits(s) != commits;
                // An accepted point can add a corner ahead (a delay line's) or move one (a timing
                // cell's flip, predicted again from what the step solved): every bank's next corner
                // as it now stands, not a moved one's old instant, which is no corner to land and
                // restart on. A step that landed on its corner keeps it: below, that tells the
                // corner from the advance's end.
                if !(lands && t1 == self.next_bp) {
                    self.next_bp = self.find_bp(s, t1);
                }
                self.rate_peak.record(t1, &self.hist.pts[0].qdot);
                if let Some(at) = self.edge_at.take().map(f64::next_up).filter(|&at| at < t1) {
                    profile_ns!(s, timed, accept_ns, next);
                    emit(s, &Accepted { edge_end: self.edge_end, ..plain(at, &self.hist.pts[0].xdot, false) });
                    profile_ns!(s, timed, emit_ns, next);
                }
                if coarse {
                    s.stats.coarse_steps += 1;
                }
                profile_ns!(s, timed, accept_ns, next);
                emit(s, &Accepted { t: t1, xdot: &self.hist.pts[0].xdot, event, coarse, chosen: !at_floor && !capped_step, edge_end: f64::INFINITY });
                profile_ns!(s, timed, emit_ns, next);
            }
            profile_ns!(s, timed, accept_ns, next);
            self.h = h * grow;
            if snapped && grow >= 1.0 {
                // the grid cut the step, not its error: the next one takes up the free length again
                self.h = self.h.max(h_free);
            }
            if floor > self.exact_floor(t0) {
                // under a resolution error control never shortens a step below it: an accepted step
                // takes at least the floor next, and one below it (cut by a corner, a crossing or a
                // failed solve) doubles toward it
                self.h = self.h.max((2.0 * h).min(floor));
            }
            profile_ns!(s, timed, estimate_ns, next);
            if self.retried && !first_after_restart {
                // the hard spot is passed: back to the bench's method
                self.retried = false;
                self.gmin_scale = 1.0;
            }
            if lands || crossed {
                if self.prov {
                    // a provisional point that lands on a corner is final
                    let commits = bank_commits(s);
                    for b in s.banks.iter_mut() {
                        b.accept(&s.x, &cx);
                    }
                    self.last_commit = bank_commits(s) != commits;
                    self.rate_peak.record(t1, &self.hist.pts[0].qdot);
                    if let Some(at) = self.edge_at.take().map(f64::next_up).filter(|&at| at < t1) {
                        profile_ns!(s, timed, accept_ns, next);
                        emit(s, &Accepted { edge_end: self.edge_end, ..plain(at, &self.hist.pts[0].xdot, false) });
                        profile_ns!(s, timed, emit_ns, next);
                    }
                    profile_ns!(s, timed, accept_ns, next);
                    emit(s, &Accepted { chosen: !at_floor && !capped_step, ..plain(t1, &self.hist.pts[0].xdot, event) });
                    profile_ns!(s, timed, emit_ns, next);
                    self.prov = false;
                }
                let t = t1;
                if lands && !crossed && t1 != self.next_bp {
                    // the advance's end, not a corner: the history, the order and the step go on,
                    // and the next step is the one the landing cut short
                    if h < h_free {
                        self.h = self.h.max(h_free);
                    }
                    profile_ns!(s, timed, accept_ns);
                    return Ok(());
                }
                self.hist.restart();
                self.hold_order1 = false;
                // a landing on the next corner is one (as `event` says), even where the end is too
                self.jumped = lands && t1 == self.next_bp;
                let edge = self.jumped && edge_only(s, t1);
                self.jumped &= !edge;
                self.edged = !self.jumped;
                self.committed = !(self.jumped || edge) && self.last_commit;
                // a resolved watched crossing's instant is found: what follows it is not resolved
                // for moving the watch on
                self.watch_crossed = !(self.jumped || edge) && self.resolving;
                if self.jumped || edge {
                    self.restarts.corner += 1;
                    self.restarts.corner_edge += u64::from(edge);
                    self.begin_window(1);
                } else {
                    self.restarts.crossing += 1;
                    self.restarts.crossing_ahead += u64::from(self.ahead.is_some());
                    self.crossings = [self.crossings[1], t];
                    self.begin_window(2);
                }
                self.since_restart = 0;
                self.resolving = false;
                self.edge_at = None;
                self.next_bp = self.find_bp(s, t);
                self.h = self.first_step(h, t);
                if self.jumped && self.res > 0.0 {
                    // a jump fed in at the corner (a landed import's, A-K1) is taken as the whole
                    // circuit takes its source's: no longer than its exporter's first step from it,
                    // the steps after grown from there as after any short step
                    self.h = self.h.min(s.banks.iter().map(|b| b.jump_step(t)).fold(f64::INFINITY, f64::min));
                }
                self.edge_end = if self.jumped { self.hidden_edge(s, t) } else { f64::INFINITY };
                if self.watch_crossed {
                    // the watch settles on from the crossing as Exact steps it, while it moves
                    self.resolving = true;
                    self.h = h.min(0.1 * (self.next_bp - t)).min(self.max_step);
                }
            }
            profile_ns!(s, timed, accept_ns, next);
            if !self.prov {
                return Ok(());
            }
        }
    }

    /// The last resort when Newton fails at the step floor after the Gear 2 retry: the operating
    /// point's gmin ladder (`op.rs`) on this step's own system. From the newest accepted point, a
    /// backward Euler step of the floor length `h` (`1e-9 * max_step`, or less to the next corner
    /// or the advance's end) is solved under a conductance from every
    /// node to ground, from 1e-2 S down by an adaptive factor to the model's gmin, then without
    /// it. The companions `C/h` of so short a step hold every charge, so only what no charge holds
    /// moves: an algebraic node whose carried value is far from its solution, such as an LED's
    /// anode a live edit left on one connection while it conducted. Newton from there falls about
    /// `n Vt` per iteration and no shorter step helps, since nothing on that node depends on `h`.
    ///
    /// The solution is accepted as a corner (the history restarts there) and the run goes on,
    /// with a `Rescue` in `Solver::rescues`. When the exact stage still fails, the last stage is
    /// accepted if its shunt is at or below gmin (what the model's own junctions carry). Returns
    /// false, with nothing accepted, when the ladder does not get there. A run that converges
    /// without it never comes here: its steps are what they were.
    fn ladder(&mut self, s: &mut Solver, t0: f64, h: f64, at: Vec<String>, emit: &mut Emit) -> bool {
        let mut timed = s.profile.is_some().then(Instant::now);
        if !(h > 0.0) {
            profile_ns!(s, timed, predict_ns);
            return false;
        }
        self.hist.restart();
        self.edged = false;
        self.prov = false;
        self.hold_order1 = false;
        let t1 = t0 + h;
        let a0 = integ::coefficients(Method::Euler, 1, t1, &self.hist, &mut self.hist_vec);
        s.x.copy_from_slice(&self.hist.newest().x);
        let mut cx = s.base_cx();
        cx.tran = true;
        cx.t = t1;
        profile_ns!(s, timed, predict_ns);
        s.reseat(&cx);
        timed = s.profile.is_some().then(Instant::now);
        let integ = Some(Integ { a0, hist: &self.hist_vec });
        let itl = s.opts.itl1;
        let gmin = s.opts.gmin;
        let floor = gmin.max(1e-3 * s.opts.abstol);
        let shunt = |gshunt: f64| Extras { gshunt, ..Default::default() };
        let mut gs = 1e-2;
        profile_ns!(s, timed, predict_ns);
        let failed = s.newton(&cx, false, integ, &shunt(gs), itl).is_err();
        timed = s.profile.is_some().then(Instant::now);
        if failed {
            self.needs_reseat = true;
            profile_ns!(s, timed, control_ns);
            return false;
        }
        let mut stages = 1;
        let mut saved = s.x.clone();
        let mut factor = 10.0_f64;
        let settled = loop {
            let next = gs / factor;
            if next < floor {
                profile_ns!(s, timed, control_ns);
                let solved = s.newton(&cx, false, integ, &Extras::default(), itl).is_ok();
                timed = s.profile.is_some().then(Instant::now);
                if solved {
                    break Some(0.0);
                }
                break None;
            }
            profile_ns!(s, timed, control_ns);
            let solved = s.newton(&cx, false, integ, &shunt(next), itl).is_ok();
            timed = s.profile.is_some().then(Instant::now);
            if solved {
                gs = next;
                stages += 1;
                saved.copy_from_slice(&s.x);
                factor = (factor * factor).min(10.0);
            } else {
                s.x.copy_from_slice(&saved);
                profile_ns!(s, timed, control_ns);
                s.reseat(&cx);
                timed = s.profile.is_some().then(Instant::now);
                factor = factor.sqrt();
                if factor < 1.0001 {
                    break None;
                }
            }
        };
        let settled = match settled {
            Some(g) => {
                stages += u32::from(g == 0.0);
                g
            }
            // the exact stage, or a deeper one, failed: a last stage at or below gmin stands
            None if gs <= gmin => {
                s.x.copy_from_slice(&saved);
                profile_ns!(s, timed, control_ns);
                s.reseat(&cx);
                let failed = s.newton(&cx, false, integ, &shunt(gs), itl).is_err();
                timed = s.profile.is_some().then(Instant::now);
                if failed {
                    self.needs_reseat = true;
                    profile_ns!(s, timed, control_ns);
                    return false;
                }
                gs
            }
            None => {
                self.needs_reseat = true;
                profile_ns!(s, timed, control_ns);
                return false;
            }
        };
        s.stats.steps += 1;
        profile_ns!(s, timed, control_ns, next);
        s.charge_at_solution(&mut self.q_acc);
        for i in 0..self.qdot.len() {
            self.qdot[i] = a0 * self.q_acc[i] + self.hist_vec[i];
        }
        integ::rate_of(Method::Euler, 1, t1, &self.hist, &s.x, &mut self.xdot, |p| (p.x.as_slice(), p.xdot.as_slice()));
        self.xdot[0] = 0.0;
        profile_ns!(s, timed, charge_ns, next);
        self.hist.push(t1, &s.x, &self.q_acc, &self.qdot, &self.xdot);
        let commits = bank_commits(s);
        for b in s.banks.iter_mut() {
            b.accept(&s.x, &cx);
        }
        self.last_commit = bank_commits(s) != commits;
        self.rate_peak.record(t1, &self.qdot);
        profile_ns!(s, timed, accept_ns, next);
        emit(s, &plain(t1, &self.xdot, false));
        profile_ns!(s, timed, emit_ns, next);
        s.rescues.push(Rescue { t: t0, at, gmin: settled, stages });
        // a corner: the next step starts a fresh history from the rescued point
        self.hist.restart();
        self.edged = false;
        self.committed = false;
        self.restarts.rescue += 1;
        self.begin_window(4);
        self.since_restart = 0;
        self.resolving = false;
        self.watch_crossed = false;
        self.jumped = false;
        self.edge_at = None;
        self.next_bp = self.find_bp(s, t1);
        self.h = self.first_step(2.0 * h, t1);
        self.retried = false;
        self.gmin_scale = 1.0;
        self.needs_reseat = false;
        profile_ns!(s, timed, accept_ns);
        true
    }

    fn reject(&mut self, s: &mut Solver, why: Reject, t: f64, h: f64) {
        self.finish(s, match why {
            Reject::Newton => 1,
            Reject::Crossing => 2,
            Reject::Provisional => 3,
            Reject::Error => 4,
        });
        self.needs_reseat = true;
        s.stats.reject(why);
        if let Some(p) = s.profile.as_mut() {
            p.rejections.push(Rejection { t, h, why, since_restart: self.since_restart });
        }
    }

    fn begin_window(&mut self, cause: u8) {
        self.window = cause;
        self.window_h = 0.0;
    }

    /// The measured attempt ended with `outcome` (`attrib::OUTCOMES`).
    fn finish(&mut self, s: &mut Solver, outcome: usize) {
        if let (Some((role, cause, iterations, loads, start, overhead)), Some(a)) = (self.attempt.take(), s.profile.as_mut().and_then(|p| p.attrib.as_mut())) {
            let ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX).saturating_sub(a.overhead_ns - overhead);
            a.attempt(usize::from(role), usize::from(cause), outcome, s.stats.iterations - iterations, s.stats.loads - loads, ns);
        }
    }

    /// Drops the provisional point and retries it with step `h`.
    fn rollback(&mut self, h: f64) {
        self.hist.pts.pop_front();
        self.prov = false;
        self.edge_at = None;
        self.hold_order1 = false;
        self.h = h;
    }

    /// Advances to `t_stop` (or the stop flag), emitting every accepted point.
    pub fn run_to(&mut self, s: &mut Solver, t_stop: f64, emit: &mut Emit, stop: Option<&std::sync::atomic::AtomicBool>) -> Result<(), crate::newton::SimDiag> {
        while self.t() < t_stop {
            if let Some(f) = stop {
                if f.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(crate::newton::SimDiag { kind: DiagKind::Stopped, message: "stopped".into(), at: Vec::new(), time: Some(self.t()), partial: None });
                }
            }
            self.step(s, t_stop, emit)?;
        }
        Ok(())
    }
}
