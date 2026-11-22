//! The operating point (E3 1.6): Newton, then gmin stepping until a confirmed wall, then source
//! stepping, then the resumed gmin ladder, then pseudo-transient. The answer says which aid
//! succeeded; when the list is exhausted it is a located diagnostic with the last partial
//! solution. Iteration counts are switch points between the aids, not failure caps.

use crate::newton::{DiagKind, Extras, NewtonFail, SimDiag, Solver};
use crate::stamp::{EvalCx, Unk};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpAid {
    Newton,
    GminStepping,
    SourceStepping,
    PseudoTransient,
}

/// How a gmin ladder in the aid list runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GminLadder {
    Full,
    Yield,
    Resume,
}

/// One entry of the operating point's aid list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpTry {
    Newton,
    Gmin(GminLadder),
    Source,
    PseudoTransient,
}

/// The default list: a ladder that yields at a confirmed wall, source stepping, the same ladder
/// resumed, pseudo-transient.
///
/// Yielding cannot make a previously converging operating point fail: the resumed tail is the
/// full ladder. What can change is which aid wins when two would. With one operating point the solution is
/// the same; with several, the circuit takes the branch of the aid that gets there, as for a seed.
pub const OP_AIDS: [OpTry; 5] = [OpTry::Newton, OpTry::Gmin(GminLadder::Yield), OpTry::Source, OpTry::Gmin(GminLadder::Resume), OpTry::PseudoTransient];

/// One stage of an operating point's homotopy, for a probe or a report.
#[derive(Clone, Debug, PartialEq)]
pub struct OpStage {
    /// The aid the stage belongs to (`Newton` for the first attempt and for a warm start).
    pub aid: OpAid,
    /// gmin stepping: the shunt in siemens; source and pseudo-transient stepping: the source
    /// scale; plain Newton: 1.
    pub param: f64,
    /// Newton iterations the stage took.
    pub iterations: u64,
    /// Largest `|dx[u]|` of the first Newton iteration, or 0 when no iteration ran. At a fold,
    /// this move per unit of ln(gs) grows without bound over the last successes before the wall;
    /// in a stretch Newton cannot cross it stays flat.
    pub first_move: f64,
    /// Index into `Solver::unknowns` of that first iteration's largest move, or 0 when none.
    /// Identifies the unknown behind the fold growth or flat stretch in `first_move`.
    pub first_unknown: u32,
    pub ok: bool,
    /// `x` as the stage started.
    pub start_x: Vec<f64>,
    /// The banks' iteration state (`Bank::iter_state`) as the stage started: the points its
    /// first Newton step is limited from.
    pub start_state: Vec<f64>,
}

/// The conductance that holds a node at its initial condition or nodeset.
pub const G_HOLD: f64 = 1e4;

/// A solved operating point and the unknown layout it was solved in: the start of a later solve of
/// a circuit close to this one with the same layout (an ensemble member from its nominal, a bench
/// restarted on an unchanged topology from its last run). A seed of another layout is not used.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpSeed {
    pub names: Vec<String>,
    pub x: Vec<f64>,
}

impl OpSeed {
    /// The solver's present solution.
    pub fn of(s: &Solver) -> OpSeed {
        OpSeed { names: s.unknowns.iter().skip(1).map(|u| u.name.clone()).collect(), x: s.x[1..].to_vec() }
    }
}

fn stage(s: &mut Solver, aid: OpAid, param: f64, cx: &EvalCx, init: bool, ex: &Extras, max_iter: u32) -> Result<u32, NewtonFail> {
    if s.op_trace.is_none() {
        return s.newton(cx, init, None, ex, max_iter);
    }
    let start_x = s.x.clone();
    let mut start_state = Vec::new();
    for b in &s.banks {
        b.iter_state(&mut start_state);
    }
    let before = s.stats.iterations;
    let result = s.newton(cx, init, None, ex, max_iter);
    let iterations = s.stats.iterations - before;
    let (first_move, first_unknown) = s.first_move;
    let ok = result.is_ok();
    s.op_trace.as_mut().unwrap().push(OpStage { aid, param, iterations, first_move, first_unknown, ok, start_x, start_state });
    result
}

/// Solves the operating point into `s.x`. With `hold_ics`, node initial conditions are held
/// (the start of a transient without `uic`).
pub fn operating_point(s: &mut Solver, hold_ics: bool) -> Result<OpAid, SimDiag> {
    operating_point_from(s, hold_ics, None)
}

/// `operating_point`, first trying plain Newton from `seed` when it has this solver's unknown
/// layout (as a DC sweep continues from its last point): a circuit a few percent from the seed's
/// converges there in a few iterations. On a failure, or a seed of another layout, the aids run as
/// without one, from their own start. A circuit with several operating points takes the one the
/// seed's Newton reaches, normally the seed's own (an ensemble member takes its nominal's branch).
pub fn operating_point_from(s: &mut Solver, hold_ics: bool, seed: Option<&OpSeed>) -> Result<OpAid, SimDiag> {
    operating_point_aids(s, hold_ics, seed, &OP_AIDS)
}

/// `operating_point_from` with an explicit aid list, tried in order after the optional warm start.
/// The first converging aid wins; a resumed ladder answers `OpAid::GminStepping`.
pub fn operating_point_aids(s: &mut Solver, hold_ics: bool, seed: Option<&OpSeed>, aids: &[OpTry]) -> Result<OpAid, SimDiag> {
    let r = match seed {
        Some(seed) if warm(s, hold_ics, seed) => Ok(OpAid::Newton),
        _ => solve(s, hold_ics, aids),
    };
    if r.is_ok() {
        let cx = s.base_cx();
        s.load(&cx);
        for b in s.banks.iter_mut() {
            b.accept(&s.x, &cx);
        }
    }
    r
}

/// Plain Newton from `seed`, node initial conditions held; true when it converged. False at once
/// (`s.x` untouched) for a seed of another unknown layout. O(unknowns) besides the Newton.
fn warm(s: &mut Solver, hold_ics: bool, seed: &OpSeed) -> bool {
    let same = seed.names.len() + 1 == s.unknowns.len() && seed.names.iter().zip(&s.unknowns[1..]).all(|(a, b)| *a == b.name);
    if !same {
        return false;
    }
    s.x[0] = 0.0;
    s.x[1..].copy_from_slice(&seed.x);
    let ics: Vec<(Unk, f64, f64)> = if hold_ics { s.ics.iter().map(|&(u, v)| (u, v, G_HOLD)).collect() } else { Vec::new() };
    for &(u, v, _) in &ics {
        s.x[u as usize] = v;
    }
    let cx = s.base_cx();
    // the limiters would measure the first step from the devices' last points (on a fresh solver
    // zero: pnjlim takes a 1.8 V LED to 0.18 V); the seed becomes their last point
    s.reseat(&cx);
    let itl1 = s.opts.itl1;
    stage(s, OpAid::Newton, 1.0, &cx, false, &Extras { hold: &ics, ..Default::default() }, itl1).is_ok()
}

struct AidCx<'a> {
    cx: &'a EvalCx,
    itl1: u32,
    ics: &'a [(Unk, f64, f64)],
    both: &'a [(Unk, f64, f64)],
    x0: &'a [f64],
    held: Extras<'a>,
}

struct GminState {
    gs: f64,
    factor: f64,
    saved: Vec<f64>,
    floor: f64,
    /// Failed targets not yet passed, paired with the shunt each stage started from.
    walls: Vec<(f64, f64)>,
}

enum GminResult {
    Converged,
    Failed,
    Yielded,
}

fn solve(s: &mut Solver, hold_ics: bool, aids: &[OpTry]) -> Result<OpAid, SimDiag> {
    let cx = s.base_cx();
    let itl1 = s.opts.itl1;
    let ics: Vec<(Unk, f64, f64)> = if hold_ics { s.ics.iter().map(|&(u, v)| (u, v, G_HOLD)).collect() } else { Vec::new() };
    let mut both = ics.clone();
    both.extend(s.nodesets.iter().map(|&(u, v)| (u, v, G_HOLD)));
    let x0 = {
        let mut x = vec![0.0; s.n + 1];
        for &(u, v, _) in &both {
            x[u as usize] = v;
        }
        x
    };
    let held = Extras { hold: &ics, ..Default::default() };
    let ctx = AidCx { cx: &cx, itl1, ics: &ics, both: &both, x0: &x0, held };
    let mut last = NewtonFail::NoConvergence { noncon: None };
    let mut yielded = None;
    let mut ran = Vec::new();
    for &aid in aids {
        let (converged, answer, name) = match aid {
            OpTry::Newton => (plain_newton(s, &ctx, &mut last)?, OpAid::Newton, "Newton"),
            OpTry::Gmin(mode) => {
                if mode == GminLadder::Resume && yielded.is_none() {
                    continue;
                }
                let name = if mode == GminLadder::Resume { "gmin stepping (resumed)" } else { "gmin stepping" };
                (gmin(s, &ctx, mode, &mut yielded, &mut last), OpAid::GminStepping, name)
            }
            OpTry::Source => (source(s, &ctx, &mut last), OpAid::SourceStepping, "source stepping"),
            OpTry::PseudoTransient => (pseudo_transient(s, &ctx, &mut last), OpAid::PseudoTransient, "pseudo-transient"),
        };
        if converged {
            return Ok(answer);
        }
        ran.push(name);
    }
    let mut d = s.diag_of(last, DiagKind::NoConvergence, "operating point");
    if let Some((last, before)) = ran.split_last() {
        let names = if before.is_empty() { (*last).to_owned() } else { format!("{} and {last}", before.join(", ")) };
        d.message = format!("{} after {names}", d.message);
    }
    Err(d)
}

/// Newton from the nodesets and initial conditions, or zero.
fn plain_newton(s: &mut Solver, ctx: &AidCx, last: &mut NewtonFail) -> Result<bool, SimDiag> {
    let AidCx { cx, itl1, ics, both, x0, held } = *ctx;
    s.x.copy_from_slice(x0);
    s.reseat(cx);
    let first = if both.len() > ics.len() {
        stage(s, OpAid::Newton, 1.0, cx, true, &Extras { hold: both, ..Default::default() }, itl1)
            .and_then(|_| stage(s, OpAid::Newton, 1.0, cx, false, &held, itl1))
    } else {
        stage(s, OpAid::Newton, 1.0, cx, true, &held, itl1)
    };
    match first {
        Ok(_) => Ok(true),
        Err(NewtonFail::Singular(u)) => Err(s.diag_of(NewtonFail::Singular(u), DiagKind::Singular, "operating point")),
        Err(e) => {
            *last = e;
            Ok(false)
        }
    }
}

/// A shunt on every node, reduced by an adaptive factor down to gmin.
fn gmin(s: &mut Solver, ctx: &AidCx, mode: GminLadder, yielded: &mut Option<GminState>, last: &mut NewtonFail) -> bool {
    let AidCx { cx, itl1, ics, x0, .. } = *ctx;
    let mut state = if mode == GminLadder::Resume {
        let Some(state) = yielded.take() else { return false };
        s.x.copy_from_slice(&state.saved);
        s.reseat(cx);
        state
    } else {
        *yielded = None;
        s.x.copy_from_slice(x0);
        s.reseat(cx);
        let gs = 1e-2;
        if stage(s, OpAid::GminStepping, gs, cx, true, &Extras { gshunt: gs, hold: ics, ..Default::default() }, itl1).is_err() {
            return false;
        }
        // With gmin 0 the stepping ends where a shunt no longer moves any current that matters.
        GminState { gs, factor: 10.0, saved: s.x.clone(), floor: s.opts.gmin.max(1e-3 * s.opts.abstol), walls: Vec::new() }
    };
    match gmin_tail(s, ctx, &mut state, mode == GminLadder::Yield, last) {
        GminResult::Converged => true,
        GminResult::Failed => false,
        GminResult::Yielded => {
            *yielded = Some(state);
            false
        }
    }
}

fn gmin_tail(s: &mut Solver, ctx: &AidCx, state: &mut GminState, yield_at_wall: bool, last: &mut NewtonFail) -> GminResult {
    let AidCx { cx, itl1, ics, held, .. } = *ctx;
    loop {
        let next = state.gs / state.factor;
        if next < state.floor {
            return match stage(s, OpAid::GminStepping, 0.0, cx, false, &held, itl1) {
                Ok(_) => GminResult::Converged,
                Err(e) => {
                    *last = e;
                    GminResult::Failed
                }
            };
        }
        match stage(s, OpAid::GminStepping, next, cx, false, &Extras { gshunt: next, hold: ics, ..Default::default() }, itl1) {
            Ok(_) => {
                state.gs = next;
                state.saved.copy_from_slice(&s.x);
                state.factor = (state.factor * state.factor).min(10.0);
                if yield_at_wall {
                    state.walls.retain(|&(target, _)| target < next);
                }
            }
            Err(e) => {
                *last = e;
                s.x.copy_from_slice(&state.saved);
                s.reseat(cx);
                state.factor = state.factor.sqrt();
                if state.factor < 1.0001 {
                    return GminResult::Failed;
                }
                if yield_at_wall {
                    // A failed target no deeper than an earlier failure, reached from closer:
                    // the wall is confirmed. The saved state is the full ladder's next stage.
                    if state.walls.iter().any(|&(target, start)| next >= target && start > state.gs) {
                        return GminResult::Yielded;
                    }
                    state.walls.push((next, state.gs));
                }
            }
        }
    }
}

/// Every independent source scaled from 0 to 1.
fn source(s: &mut Solver, ctx: &AidCx, last: &mut NewtonFail) -> bool {
    let AidCx { cx, itl1, held, .. } = *ctx;
    s.x.iter_mut().for_each(|v| *v = 0.0);
    s.reseat(cx);
    let mut scx = cx.clone();
    scx.src_scale = 0.0;
    if stage(s, OpAid::SourceStepping, scx.src_scale, &scx, true, &held, itl1).is_ok() {
        let mut scale = 0.0f64;
        let mut step = 0.1f64;
        let mut saved = s.x.clone();
        loop {
            let next = (scale + step).min(1.0);
            scx.src_scale = next;
            match stage(s, OpAid::SourceStepping, next, &scx, false, &held, itl1) {
                Ok(_) => {
                    scale = next;
                    saved.copy_from_slice(&s.x);
                    if scale >= 1.0 {
                        return true;
                    }
                    step = (step * 2.0).min(0.25);
                }
                Err(e) => {
                    *last = e;
                    s.x.copy_from_slice(&saved);
                    s.reseat(&scx);
                    step *= 0.25;
                    if step < 1e-7 {
                        break;
                    }
                }
            }
        }
    }
    false
}

/// A capacitor on every node anchors each solve to the last one while the sources ramp up;
/// the anchor then relaxes until nothing moves.
fn pseudo_transient(s: &mut Solver, ctx: &AidCx, last: &mut NewtonFail) -> bool {
    let AidCx { cx, itl1, ics, held, .. } = *ctx;
    s.x.iter_mut().for_each(|v| *v = 0.0);
    s.reseat(cx);
    let mut prev = s.x.clone();
    let mut gpt = 1e-2f64;
    let mut scale = 0.0f64;
    let mut scx = cx.clone();
    let mut fails = 0;
    while fails < 60 {
        let target = (scale + 0.05).min(1.0);
        scx.src_scale = target;
        match stage(s, OpAid::PseudoTransient, target, &scx, scale == 0.0, &Extras { pt: Some((gpt, &prev)), hold: ics, ..Default::default() }, itl1) {
            Ok(_) => {
                scale = target;
                prev.copy_from_slice(&s.x);
                if scale >= 1.0 {
                    gpt *= 0.25;
                    if gpt < s.opts.gmin {
                        if stage(s, OpAid::PseudoTransient, 1.0, cx, false, &held, itl1).is_ok() {
                            return true;
                        }
                        break;
                    }
                }
            }
            Err(e) => {
                *last = e;
                s.x.copy_from_slice(&prev);
                s.reseat(&scx);
                gpt *= 4.0;
                fails += 1;
            }
        }
    }
    false
}
