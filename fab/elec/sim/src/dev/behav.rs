//! B: behavioural sources, and the behavioural resistor, capacitor and inductor. Each expression compiles once, in `setup`,
//! to a tape over the unknowns it reads; each evaluation gives its value and exact partials
//! (forward-mode differentiation) into scratch the bank keeps, so it allocates nothing.
//!
//! - `I=expr` is a current from p through the source to n, linearised by its partials.
//! - `V=expr` claims a branch current and the row `v(p) - v(n) - expr = 0`.
//! - `R=expr` is the current `v(p, n) / expr` from p to n, its partials by the chain rule
//!   (`-i / r` times the resistance's own partials). A resistance of 0 is a value that is not
//!   finite.
//! - `C=expr` is the charge `expr * v(p, n)` on p (and its negative on n), through the solver's
//!   charge path: `dq/dv(p, n)` is the capacitance and `dq/du` is `v(p, n)` times its partials.
//!   The charge is a function of the state, so a cycle that returns the state returns the charge
//!   exactly; the integrator turns it into current, a capacitance that changes in time included.
//! - `L=expr` claims a branch current `i` from p to n, as an inductor does: the row
//!   `v(p, n) - d(flux)/dt = 0` with the flux `expr * i` as its charge, `dflux/di` the inductance
//!   and `dflux/du` `i` times its partials. An inductance may read its own current (`i(l1)`), so
//!   a saturating core is one expression; K does not couple it.
//! - A step in the expression (a comparison, `u`, `sgn`, `floor`, `ceil`, a logical operator or
//!   a choice) must choose the same branches in two evaluations running before Newton may
//!   converge, so an iteration that flips branches back and forth fails as non-convergence and
//!   cuts the step instead of passing on a stale branch. A transient step whose ends lie on two
//!   sides of a comparison, a sign or a rounding edge is cut to the interpolated crossing, as a
//!   switch's threshold is.
//! - A value or partial that is not finite fails the iteration. The instance then stamps nothing
//!   of its expression: a voltage keeps its unit row, a resistor a 1 S placeholder so the matrix
//!   stays solvable, an inductor its unit rows (a short). Newton's diagnostic names the instance
//!   and its expression.
//! - AC sees the linearisation (and a capacitor's capacitances) at the operating point.

use crate::circuit::{BKind, Behavioural, GND};
use crate::expr::{Env, Leaf, Ref, Scratch, Tape};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};
use std::cell::RefCell;

/// An evaluation's working storage, shared by the bank's tapes.
#[derive(Clone, Default)]
struct Work {
    scratch: Scratch,
    grad: Vec<f64>,
    states: Vec<f64>,
    m0: Vec<f64>,
    m1: Vec<f64>,
    sides: Vec<f64>,
}

/// Why an instance last failed to converge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    Converging,
    NotFinite,
    Branch,
}

#[derive(Clone)]
pub struct BBank {
    pub inst: Vec<Behavioural>,
    tapes: Vec<Tape>,
    /// Own branch (V, L), 0 for the others.
    br: Vec<Unk>,
    /// Each instance's first slot: V claims (p, own), (n, own), (own, p), (own, n), then
    /// (own, input) per input; I claims (p, input), (n, input) per input; R claims the four
    /// entries between p and n, then (p, input), (n, input) per input, as C does; L claims
    /// (p, own), (n, own), (own, p), (own, n), (own, own), then (own, input) per input.
    slot_at: Vec<usize>,
    slots: Vec<u32>,
    /// Each instance's first step state, and the states its last evaluation chose.
    state_at: Vec<usize>,
    state: Vec<f64>,
    why: Vec<Why>,
    /// The environment of the last evaluation and of the last accepted point (a crossing reads
    /// both ends of the step).
    at_eval: Env,
    at_accept: Env,
    // `&self` evaluations (terminal currents, crossings) borrow it; `eval` takes it by `get_mut`
    work: RefCell<Work>,
    /// `Options::bypass`: Fast may keep an instance's last evaluation (`EvalCx::bypass`) while its
    /// inputs (and a two-terminal element's own voltage or current) stay within a tenth of the
    /// tolerance, at the same temperature, at the same time if it reads time, and never for a tape
    /// whose value jumps (a jump inside the band would keep the old branch); a tape whose slope
    /// jumps keeps its line only while each corner's operands stay on the side they were on
    /// (`Tape::sides`).
    pub bypass: bool,
    /// Evaluations the bypass skipped (`Bank::bypassed`).
    bypassed: u64,
    /// Each instance's last evaluation, and its first input in `kept_in` (the inputs' values then),
    /// `kept_grad` (the stamped partials) and `in_abs` (each input's absolute tolerance: a branch
    /// current's is `abstol`, a node's `vntol`; true for a current).
    kept: Vec<Kept>,
    in_at: Vec<usize>,
    kept_in: Vec<f64>,
    kept_grad: Vec<f64>,
    in_current: Vec<bool>,
    /// Each instance's first corner in `kept_sides`: the side of each corner at its last
    /// evaluation.
    side_at: Vec<usize>,
    kept_sides: Vec<f64>,
}

/// An instance's last evaluation, which Fast's bypass reuses: the expression's value `f`, the
/// element's output `y` and slope `g` (as stamped), its own voltage or current `ctl` then.
#[derive(Clone, Copy, Debug, Default)]
struct Kept {
    evaluated: bool,
    f: f64,
    y: f64,
    g: f64,
    ctl: f64,
    env: Env,
}

impl BBank {
    pub fn new(inst: Vec<Behavioural>) -> Self {
        BBank {
            inst,
            tapes: Vec::new(),
            br: Vec::new(),
            slot_at: Vec::new(),
            slots: Vec::new(),
            state_at: Vec::new(),
            state: Vec::new(),
            why: Vec::new(),
            at_eval: Env::default(),
            at_accept: Env::default(),
            work: RefCell::new(Work::default()),
            bypass: false,
            bypassed: 0,
            kept: Vec::new(),
            in_at: Vec::new(),
            kept_in: Vec::new(),
            kept_grad: Vec::new(),
            in_current: Vec::new(),
            side_at: Vec::new(),
            kept_sides: Vec::new(),
        }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        let e = env(cx);
        // Crossings use this attempt's environment for every tape, even when instance 0 rests.
        self.at_eval = e;
        let w = self.work.get_mut();
        for i in insts {
            let b = &self.inst[i];
            let tape = &self.tapes[i];
            let k = tape.inputs().len();
            let (vpn, ctl) = control(b, self.br[i], x);
            let (kept, at) = (self.kept[i], self.in_at[i]);
            // Fast's bypass: the kept stamps, the value carried along them
            let sa = self.side_at[i];
            let bypassed = self.bypass && cx.bypass && keeps(&kept, tape, e)
                && near(b.kind, ctl, &kept, tape, x, &self.kept_in[at..at + k], &self.in_current[at..at + k], cx)
                && sides_hold(tape, x, e, &mut w.scratch, &mut w.sides, &self.kept_sides[sa..sa + tape.corners()]);
            let (mut f, mut y, mut g);
            if bypassed {
                self.bypassed += 1;
                w.grad[..k].copy_from_slice(&self.kept_grad[at..at + k]);
                let moved: f64 = tape.inputs().iter().enumerate().map(|(j, &u)| w.grad[j] * (x[u as usize] - self.kept_in[at + j])).sum();
                (f, y, g) = match b.kind {
                    BKind::V | BKind::I => (kept.f + moved, 0.0, 0.0),
                    BKind::R | BKind::C | BKind::L => (kept.f, kept.y + kept.g * (ctl - kept.ctl) + moved, kept.g),
                };
            } else {
                f = tape.eval(x, e, &mut w.scratch, &mut w.grad);
                // the same branches as the last evaluation, or this iteration has not converged
                let n = tape.steps();
                tape.branches(&w.scratch, &mut w.states[..n]);
                let prev = &mut self.state[self.state_at[i]..self.state_at[i] + n];
                let flipped = prev.iter().zip(&w.states[..n]).any(|(a, b)| a.to_bits() != b.to_bits());
                prev.copy_from_slice(&w.states[..n]);
                self.why[i] = Why::Converging;
                if flipped {
                    self.why[i] = Why::Branch;
                    st.not_converged(i);
                }
                let finite;
                (y, g, finite) = outputs(b.kind, f, vpn, ctl, &mut w.grad[..k]);
                if !finite {
                    self.why[i] = Why::NotFinite;
                    st.not_converged(i);
                    (f, y, g) = (0.0, 0.0, if b.kind == BKind::R { 1.0 } else { 0.0 });
                    w.grad[..k].fill(0.0);
                }
                self.kept[i] = Kept { evaluated: finite, f, y, g, ctl, env: e };
                tape.sides(&w.scratch, &mut self.kept_sides[sa..sa + tape.corners()]);
                for (j, &u) in tape.inputs().iter().enumerate() {
                    self.kept_in[at + j] = x[u as usize];
                }
                self.kept_grad[at..at + k].copy_from_slice(&w.grad[..k]);
            }
            let s = &self.slots[self.slot_at[i]..];
            match b.kind {
                BKind::V => {
                    let o = self.br[i];
                    st.g[s[0] as usize] += 1.0;
                    st.g[s[1] as usize] -= 1.0;
                    st.g[s[2] as usize] += 1.0;
                    st.g[s[3] as usize] -= 1.0;
                    st.i2(b.p, b.n, x[o as usize]);
                    for (j, &d) in w.grad[..k].iter().enumerate() {
                        st.g[s[4 + j] as usize] -= d;
                    }
                    st.f[o as usize] += vpn - f;
                }
                BKind::I | BKind::R => {
                    let s = if b.kind == BKind::R {
                        st.g4(&s[..4], g);
                        &s[4..]
                    } else {
                        y = f;
                        s
                    };
                    for (j, &d) in w.grad[..k].iter().enumerate() {
                        st.g[s[2 * j] as usize] += d;
                        st.g[s[2 * j + 1] as usize] -= d;
                    }
                    st.i2(b.p, b.n, y);
                }
                BKind::L => {
                    let o = self.br[i];
                    st.g[s[0] as usize] += 1.0;
                    st.g[s[1] as usize] -= 1.0;
                    st.g[s[2] as usize] += 1.0;
                    st.g[s[3] as usize] -= 1.0;
                    st.c[s[4] as usize] -= g;
                    for (j, &d) in w.grad[..k].iter().enumerate() {
                        st.c[s[5 + j] as usize] -= d;
                    }
                    st.i2(b.p, b.n, x[o as usize]);
                    st.f[o as usize] += vpn;
                    st.q[o as usize] -= y;
                }
                BKind::C => {
                    st.c4(&s[..4], g);
                    for (j, &d) in w.grad[..k].iter().enumerate() {
                        st.c[s[4 + 2 * j] as usize] += d;
                        st.c[s[5 + 2 * j] as usize] -= d;
                    }
                    st.q2(b.p, b.n, y);
                }
            }
        }
    }
}

/// An instance's `v(p, n)` and its control: a two-terminal element's own voltage or current,
/// which its output also follows (0 for a source).
fn control(b: &Behavioural, br: Unk, x: &[f64]) -> (f64, f64) {
    let vpn = x[b.p as usize] - x[b.n as usize];
    let ctl = match b.kind {
        BKind::R | BKind::C => vpn,
        BKind::L => x[br as usize],
        BKind::V | BKind::I => 0.0,
    };
    (vpn, ctl)
}

/// A two-terminal element's output y and its slope g from the tape's value `f`: a resistor's
/// current by v(p, n) (i = v / r, di/du = -(i / r) dr/du), a capacitor's charge by v(p, n)
/// (q = c v, dq/du = v dc/du), an inductor's flux by its current (flux = l i, dflux/du = i dl/du).
/// Scales the tape's input partials `grad` to the output's; false when any value is not finite.
fn outputs(kind: BKind, f: f64, vpn: f64, ctl: f64, grad: &mut [f64]) -> (f64, f64, bool) {
    let (y, g, scale) = match kind {
        BKind::R => (vpn / f, 1.0 / f, -vpn / (f * f)),
        BKind::C => (f * vpn, f, vpn),
        BKind::L => (f * ctl, f, ctl),
        BKind::V | BKind::I => (0.0, 0.0, 1.0),
    };
    for d in grad.iter_mut() {
        *d *= scale;
    }
    let finite = f.is_finite() && y.is_finite() && g.is_finite() && grad.iter().all(|d| d.is_finite());
    (y, g, finite)
}

/// A kept evaluation that a bypass may reuse: finite, of a tape without branch steps, at this
/// temperature and (when the tape reads it) time.
fn keeps(kept: &Kept, tape: &Tape, e: Env) -> bool {
    kept.evaluated && tape.steps() == 0 && kept.env.temper == e.temper && (kept.env.time == e.time || !tape.reads_time())
}

/// Fast's bypass band: the control and every input within a tenth of the tolerance of the kept
/// evaluation's (`abstol` for a current, `vntol` for a voltage).
fn near(kind: BKind, ctl: f64, kept: &Kept, tape: &Tape, x: &[f64], kept_in: &[f64], in_current: &[bool], cx: &EvalCx) -> bool {
    let abs = |current: bool| if current { cx.abstol } else { cx.vntol };
    cx.near(ctl, kept.ctl, abs(kind == BKind::L))
        && tape.inputs().iter().enumerate().all(|(j, &u)| cx.near(x[u as usize], kept_in[j], abs(in_current[j])))
}

/// Whether every corner of `tape` is at `x` on the side it was at the kept evaluation (`kept`),
/// evaluating only the corners' operands. The band bounds each input against its own value, not
/// a corner's operand, so without this the kept line would carry one side's slope past the
/// corner (an op-amp's rail clamp, kept on, held its node on the rail while its drive pulled it
/// off).
fn sides_hold(tape: &Tape, x: &[f64], e: Env, scratch: &mut Scratch, buf: &mut [f64], kept: &[f64]) -> bool {
    let n = tape.corners();
    if n == 0 {
        return true;
    }
    tape.sides_at(x, e, scratch, &mut buf[..n]);
    buf[..n].iter().zip(kept).all(|(a, b)| a.to_bits() == b.to_bits())
}

fn env(cx: &EvalCx) -> Env {
    Env { time: if cx.tran { cx.t } else { 0.0 }, temper: cx.temp - 273.15 }
}

impl Bank for BBank {
    fn kind(&self) -> &'static str {
        "b"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let b = &self.inst[i];
        out.extend_from_slice(&[b.p, b.n, self.br[i]]);
        out.extend_from_slice(self.tapes[i].inputs());
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        let end = self.slot_at.get(i + 1).copied().unwrap_or(self.slots.len());
        out.extend_from_slice(&self.slots[self.slot_at[i]..end]);
    }
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], xdot: &[f64], cx: &EvalCx, out: &mut [f64]) {
        let b = &self.inst[inst];
        let i = match b.kind {
            BKind::V | BKind::L => x[self.br[inst] as usize],
            BKind::I | BKind::R => {
                let w = &mut *self.work.borrow_mut();
                let f = self.tapes[inst].value(x, env(cx), &mut w.scratch);
                if b.kind == BKind::R { (x[b.p as usize] - x[b.n as usize]) / f } else { f }
            }
            BKind::C => {
                // dq/dt = dc/dt v + c dv/dt, dc/dt along the solution and the time
                let w = &mut *self.work.borrow_mut();
                let tdot = if cx.tran { 1.0 } else { 0.0 };
                let (c, cdot) = self.tapes[inst].rate(x, xdot, tdot, env(cx), &mut w.scratch);
                cdot * (x[b.p as usize] - x[b.n as usize]) + c * (xdot[b.p as usize] - xdot[b.n as usize])
            }
        };
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        // every own branch first, so an expression may read any instance's branch
        for b in &self.inst {
            let own = match b.kind {
                BKind::V | BKind::L => topo.branch(&b.name),
                BKind::I | BKind::R | BKind::C => 0,
            };
            self.br.push(own);
        }
        let w = self.work.get_mut();
        for (i, b) in self.inst.iter().enumerate() {
            let mut currents = Vec::new();
            let tape = b.expr.compile(&mut |r| match r {
                Ref::Node(n) => {
                    let node = b.reads.iter().find(|(s, _)| s == n).map(|&(_, node)| node).ok_or_else(|| format!("no node named {n}"))?;
                    Ok(if node == GND { Leaf::Value(0.0) } else { Leaf::Unknown(node) })
                }
                Ref::Branch(d) => {
                    let u = topo.branch_of(d).ok_or_else(|| format!("no voltage source or inductor named {d} to read in i({d})"))?;
                    currents.push(u);
                    Ok(Leaf::Unknown(u))
                }
                Ref::Param(p) => Err(format!("unknown parameter {p}")),
            });
            let tape = tape.map_err(|e| format!("{}: {}: {e}", b.name, b.expr))?;
            let own = self.br[i];
            self.slot_at.push(self.slots.len());
            match b.kind {
                BKind::V => {
                    let s = [topo.slot(b.p, own), topo.slot(b.n, own), topo.slot(own, b.p), topo.slot(own, b.n)];
                    self.slots.extend_from_slice(&s);
                    for &u in tape.inputs() {
                        self.slots.push(topo.slot(own, u));
                    }
                }
                BKind::L => {
                    let s = [topo.slot(b.p, own), topo.slot(b.n, own), topo.slot(own, b.p), topo.slot(own, b.n), topo.slot(own, own)];
                    self.slots.extend_from_slice(&s);
                    for &u in tape.inputs() {
                        self.slots.push(topo.slot(own, u));
                    }
                }
                BKind::I | BKind::R | BKind::C => {
                    if b.kind != BKind::I {
                        self.slots.extend_from_slice(&topo.slot4(b.p, b.n));
                    }
                    for &u in tape.inputs() {
                        self.slots.push(topo.slot(b.p, u));
                        self.slots.push(topo.slot(b.n, u));
                    }
                }
            }
            self.state_at.push(self.state.len());
            self.state.extend(std::iter::repeat(0.0).take(tape.steps()));
            self.side_at.push(self.kept_sides.len());
            self.kept_sides.extend(std::iter::repeat(0.0).take(tape.corners()));
            let c = tape.corners().max(w.sides.len());
            w.sides.resize(c, 0.0);
            self.kept.push(Kept::default());
            self.in_at.push(self.kept_in.len());
            for &u in tape.inputs() {
                self.kept_in.push(0.0);
                self.kept_grad.push(0.0);
                self.in_current.push(currents.contains(&u));
            }
            self.why.push(Why::Converging);
            w.scratch.fit(&tape);
            let k = tape.inputs().len().max(w.grad.len());
            w.grad.resize(k, 0.0);
            let n = tape.steps().max(w.states.len());
            for v in [&mut w.states, &mut w.m0, &mut w.m1] {
                v.resize(n, 0.0);
            }
            self.tapes.push(tape);
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn iter_state(&self, out: &mut Vec<f64>) {
        out.extend_from_slice(&self.state);
        for k in &self.kept {
            out.extend_from_slice(&[k.evaluated as u8 as f64, k.f, k.y, k.g, k.ctl, k.env.time, k.env.temper]);
        }
        out.extend_from_slice(&self.kept_in);
        out.extend_from_slice(&self.kept_grad);
    }
    fn value_edited(&mut self) {
        self.kept.iter_mut().for_each(|k| k.evaluated = false);
    }
    fn bypassed(&self) -> u64 {
        self.bypassed
    }
    fn settle(&mut self, x: &[f64], cx: &EvalCx) -> bool {
        if !self.bypass {
            return false;
        }
        let e = env(cx);
        let w = self.work.get_mut();
        for i in 0..self.inst.len() {
            let (b, tape) = (&self.inst[i], &self.tapes[i]);
            let k = tape.inputs().len();
            let (vpn, ctl) = control(b, self.br[i], x);
            let (kept, at) = (self.kept[i], self.in_at[i]);
            if !keeps(&kept, tape, e) {
                return false;
            }
            // grade 1: the next load would bypass it
            let sa = self.side_at[i];
            if near(b.kind, ctl, &kept, tape, x, &self.kept_in[at..at + k], &self.in_current[at..at + k], cx)
                && sides_hold(tape, x, e, &mut w.scratch, &mut w.sides, &self.kept_sides[sa..sa + tape.corners()])
            {
                continue;
            }
            // grade 2: evaluated afresh, on the solve's line
            if !cx.settle {
                return false;
            }
            let f = tape.eval(x, e, &mut w.scratch, &mut w.grad);
            let (y, g, finite) = outputs(b.kind, f, vpn, ctl, &mut w.grad[..k]);
            if !finite {
                return false;
            }
            let moved: f64 = tape.inputs().iter().enumerate().map(|(j, &u)| self.kept_grad[at + j] * (x[u as usize] - self.kept_in[at + j])).sum();
            let line = kept.y + kept.g * (ctl - kept.ctl) + moved;
            let agrees = match b.kind {
                BKind::V => cx.agrees(f, kept.f + moved, cx.vntol),
                BKind::I => cx.agrees(f, kept.f + moved, cx.abstol),
                BKind::R => cx.agrees(y, line, cx.abstol),
                BKind::C | BKind::L => cx.agrees(y, line, cx.chgtol),
            };
            if !agrees {
                return false;
            }
            self.kept[i] = Kept { evaluated: true, f, y, g, ctl, env: e };
            tape.sides(&w.scratch, &mut self.kept_sides[sa..sa + tape.corners()]);
            for (j, &u) in tape.inputs().iter().enumerate() {
                self.kept_in[at + j] = x[u as usize];
            }
            self.kept_grad[at..at + k].copy_from_slice(&w.grad[..k]);
        }
        true
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn accept(&mut self, _x: &[f64], cx: &EvalCx) {
        self.at_accept = env(cx);
    }
    fn varies_after(&self, _t: f64) -> bool {
        self.tapes.iter().any(Tape::reads_time)
    }
    fn crossing(&self, x0: &[f64], x1: &[f64]) -> Option<f64> {
        let w = &mut *self.work.borrow_mut();
        let mut first: Option<f64> = None;
        for tape in self.tapes.iter().filter(|t| t.steps() > 0) {
            let n = tape.steps();
            tape.value(x0, self.at_accept, &mut w.scratch);
            tape.margins(&w.scratch, &mut w.m0[..n]);
            tape.value(x1, self.at_eval, &mut w.scratch);
            tape.margins(&w.scratch, &mut w.m1[..n]);
            if let Some(f) = tape.crossing(&w.m0[..n], &w.m1[..n]) {
                first = Some(first.map_or(f, |g| g.min(f)));
            }
        }
        first
    }
    fn noncon_reason(&self, inst: usize) -> Option<String> {
        let b = &self.inst[inst];
        let kind = match b.kind {
            BKind::V => "V",
            BKind::I => "I",
            BKind::R => "R",
            BKind::C => "C",
            BKind::L => "L",
        };
        match self.why[inst] {
            Why::Converging => None,
            Why::NotFinite => Some(format!("{}: {kind}={} is not finite", b.name, b.expr)),
            Why::Branch => Some(format!("{}: {kind}={} changed its branch", b.name, b.expr)),
        }
    }
}
