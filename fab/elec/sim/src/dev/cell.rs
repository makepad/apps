//! Timing cells (`Cell`, Logic mode's closed form): a threshold inverter whose input network, one
//! node with capacitors to rails and resistors to outside nodes, is out of the solve. Over a step
//! the outside nodes hold their voltages at the step's end, so the input is
//!   v(t) = v_inf + (v0 - v_inf) exp(-(t - t0) / tau)
//! with `tau = C / G` over the network's capacitance C and conductance G, `v_inf = sum g u / G`
//! and `u` the outside voltages the step solves for. The input is solved with them: it follows a
//! node that jumps where a step starts (another cell's flip, a pad's edge) without a step's lag,
//! and loads a node it moves (a pull-up it hangs from) as its resistor does. Every accepted point
//! re-forms it from where it stands, so it stays continuous.
//!
//! The output flips where a Schmitt section's switch pair commits (`dev::sw`, `commits`): as the
//! input enters the active switch's band, or with a band wider than twice the hysteresis as it
//! leaves the other position's. That crossing is one logarithm and a break, so a step lands on it to
//! the double, and the pair changes position there at once, as theirs does. The switches'
//! conductances depend on the position only, so the cell is linear in the solve: its output's two
//! conductances, and the input loading each outside node as `g_j (u_j - v(t))` with
//! `v(t) = (1 - E) sum g_k u_k / G + E v0`, `E = exp(-(t - t0) / tau)`: a block over the outside
//! nodes, one entry for each pair of its resistors.
//!
//! Cost: O(resistors^2) an evaluation, O(resistors) an accepted point; the instances are few and
//! their resistors two or three.

use crate::circuit::Cell;
use crate::stamp::{Bank, EvalCx, Given, RunState, Stamps, Topology, Unk};

/// A cell's switch pair: where the input takes the output low (rising) and high (falling), volts
/// against vss (`commits`), and each switch's closed and open conductances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellModel {
    pub rise: f64,
    pub fall: f64,
    pub down: (f64, f64),
    pub up: (f64, f64),
}

/// Where a mirrored switch pair commits, rising and falling, volts against vss (`dev::sw`'s
/// accept): a switch commits once its control is in its band and past the other position's.
/// With the band narrower than twice the hysteresis that is where it enters its band, and the
/// switch snaps there; with a wider one, where it leaves the other position's, and the switch has
/// moved partway through its band before (a cell's calibration measures what that costs). They
/// meet when the band is exactly twice the hysteresis: no hysteresis left for a cell.
pub fn commits(m: &super::sw::SwModel) -> (f64, f64) {
    let past = (m.vh - 0.5 * m.w).abs();
    (m.vt + past, m.vt - past)
}

/// The input's closed form since `t0`, its crossing of the active edge and the flip that follows
/// it by the cell's delay.
#[derive(Clone, Copy, Debug, Default)]
struct Form {
    t0: f64,
    v0: f64,
    vinf: f64,
    tau: f64,
    cross: f64,
    flip: f64,
}

fn after(b: f64, t: f64) -> bool {
    b - t > 1e-13 * t.abs().max(1e-300)
}

/// The least time a flip is set ahead of the point it is set at, seconds. The stepper crosses a
/// corner closer than its sliver (64e-9 of its largest step) without landing on it, and a flip
/// must land: the step that lands on a corner restarts the history, and a flip inside a step
/// would carry the output's jump into a trapezoidal step (a ring) and a step that ends late. A
/// flip once set moves no less than this: the stepper's first steps after a restart are a tenth
/// of the way to the next corner, and a corner that slid on with every point would never come.
const FLIP_LEAD: f64 = 1e-10;

/// Whether the point at `t` is on the flip at `f`, within a float's noise: the stepper lands on
/// a corner exactly, and a flip once set moves only by more than `FLIP_LEAD`.
fn landed(f: f64, t: f64) -> bool {
    !after(f, t) && !after(t, f)
}

#[derive(Clone)]
pub struct CellBank {
    pub inst: Vec<Cell>,
    model: Vec<CellModel>,
    high: Vec<bool>,
    form: Vec<Form>,
    /// Per instance: its capacitance and conductance, and whether its form waits for the outside
    /// voltages (before the first point, after a state hand-over).
    cap: Vec<f64>,
    g: Vec<f64>,
    fresh: Vec<bool>,
    /// Per instance, a flip a point passed without landing on it: when it was due and the input
    /// there (infinite: none). The output turns at the corner set after it, the input from here.
    late: Vec<(f64, f64)>,
    /// Per instance, its first slot: 8 for the output (out with vdd, out with vss), one for each
    /// pair of its resistors (their outside nodes' row and column), then the input's row: its own
    /// column and each outside node's; then each inner node's row: its own column, the input's and
    /// its entry's outside node's.
    first: Vec<usize>,
    slots: Vec<u32>,
    /// The last accepted point's time: a value edit starts the input's form again there.
    at: f64,
    /// The terminals an instance reads out, the most any has (`Cell::terminals`).
    terminals: usize,
    /// The flips `accept` has made (`Bank::commits`).
    commits: u64,
}

impl CellBank {
    pub fn new(inst: Vec<Cell>, model: Vec<CellModel>) -> Self {
        let cap = inst.iter().map(|c| c.caps.iter().map(|&(_, f, _)| f).sum()).collect();
        let g: Vec<f64> = inst.iter().map(|c| c.res.iter().map(|&(_, _, r)| 1.0 / r).sum()).collect();
        let high = inst.iter().map(|c| c.high).collect();
        let form = inst.iter().map(|c| Form { t0: 0.0, v0: c.v0, vinf: c.v0, tau: f64::INFINITY, cross: f64::INFINITY, flip: f64::INFINITY }).collect();
        let fresh = vec![true; inst.len()];
        let late = vec![(f64::INFINITY, 0.0); inst.len()];
        let terminals = inst.iter().map(Cell::terminals).max().unwrap_or(3);
        CellBank { inst, model, high, form, cap, g, fresh, late, first: Vec::new(), slots: Vec::new(), at: 0.0, terminals, commits: 0 }
    }

    /// The input at time `t` (its start value before the form's start).
    fn value(&self, i: usize, t: f64) -> f64 {
        let f = self.form[i];
        if t <= f.t0 || !f.tau.is_finite() {
            f.v0
        } else {
            f.vinf + (f.v0 - f.vinf) * (-(t - f.t0) / f.tau).exp()
        }
    }

    /// A new form from (t0, v0) with the outside voltages at `x`; just after a flip the output's
    /// own node is read as the rail it now goes to, which the solve has not reached yet. The flip
    /// is the crossing plus the delay, at least `FLIP_LEAD` ahead. A flip set and still ahead
    /// stays where it is when the input is past its edge (the delay counts from the crossing
    /// once) or when the new one is within `FLIP_LEAD` of it, the crossing then as computed here
    /// (what a hand-over carries is the form's own, given back the same); one passed without a
    /// landing is set `FLIP_LEAD` ahead.
    fn reform(&mut self, i: usize, t0: f64, v0: f64, x: &[f64], flipped: bool) {
        let c = &self.inst[i];
        let level = if self.high[i] { c.vdd } else { c.vss };
        let mut gu = 0.0;
        for &(_, node, r) in &c.res {
            let u = if flipped && node == c.out { x[level as usize] } else { x[node as usize] };
            gu += u / r;
        }
        let g = self.g[i];
        let (vinf, tau) = (gu / g, self.cap[i] / g);
        let vss = x[c.vss as usize];
        let m = self.model[i];
        let (v, w) = (v0, vinf);
        // the time from t0 at which the input reaches `edge` heading for `w`, if it does
        let reach = |edge: f64, rising: bool| -> f64 {
            if (rising && v >= edge) || (!rising && v <= edge) {
                0.0
            } else if (rising && w > edge) || (!rising && w < edge) {
                tau * ((w - v) / (w - edge)).ln()
            } else {
                f64::INFINITY
            }
        };
        let r = if self.high[i] { reach(vss + m.rise, true) } else { reach(vss + m.fall, false) };
        let old = self.form[i];
        let pending = !flipped && old.flip.is_finite() && after(old.flip, t0);
        let (cross, flip) = if r == 0.0 && !flipped {
            if pending {
                (old.cross, old.flip)
            } else {
                let cross = if old.cross.is_finite() { old.cross.min(t0) } else { t0 };
                (cross, (cross + c.delay).max(t0 + FLIP_LEAD))
            }
        } else {
            let cross = t0 + r;
            let flip = (cross + c.delay).max(t0 + FLIP_LEAD);
            if pending && (flip - old.flip).abs() < FLIP_LEAD { (cross, old.flip) } else { (cross, flip) }
        };
        self.form[i] = Form { t0, v0, vinf, tau, cross, flip };
        self.fresh[i] = false;
    }

    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let c = &self.inst[i];
            let m = self.model[i];
            let (gu, gd) = if self.high[i] { (m.up.0, m.down.1) } else { (m.up.1, m.down.0) };
            let s0 = self.first[i];
            st.g4(&self.slots[s0..s0 + 4], gu);
            st.g4(&self.slots[s0 + 4..s0 + 8], gd);
            st.i2(c.out, c.vdd, gu * (x[c.out as usize] - x[c.vdd as usize]));
            st.i2(c.out, c.vss, gd * (x[c.out as usize] - x[c.vss as usize]));
            // the step from the form's start with the outside nodes held at x; at DC the input
            // holds its start
            let f = self.form[i];
            let g = self.g[i];
            let e = if cx.tran { (-(cx.t - f.t0).max(0.0) * g / self.cap[i]).exp() } else { 1.0 };
            let k = (1.0 - e) / g;
            let v = k * c.res.iter().map(|&(_, node, r)| x[node as usize] / r).sum::<f64>() + e * f.v0;
            let n = c.res.len();
            for (j, &(_, a, ra)) in c.res.iter().enumerate() {
                st.i2(a, 0, (x[a as usize] - v) / ra);
                for (m, &(_, _, rb)) in c.res.iter().enumerate() {
                    let d = if j == m { 1.0 / ra } else { 0.0 } - k / (ra * rb);
                    st.g[self.slots[s0 + 8 + j * n + m] as usize] += d;
                }
            }
            // the input node is that value, its row scaled by the network's conductance (a
            // current, as every node's row is); nothing reads it back
            let si = s0 + 8 + n * n;
            st.i2(c.inp, 0, g * (x[c.inp as usize] - v));
            st.g[self.slots[si] as usize] += g;
            for (j, &(_, _, rj)) in c.res.iter().enumerate() {
                st.g[self.slots[si + 1 + j] as usize] -= g * k / rj;
            }
            // a chain's inner node stands its share of the way from the input to the chain's
            // outside node (the chain's current through the resistors between), its row scaled
            // by the chain's conductance
            for (q, &(m, entry, between)) in c.inner.iter().enumerate() {
                let (_, a, r) = c.res[entry as usize];
                let share = c.share(entry, between);
                let sm = si + 1 + n + 3 * q;
                st.i2(m, 0, (x[m as usize] - (1.0 - share) * x[c.inp as usize] - share * x[a as usize]) / r);
                st.g[self.slots[sm] as usize] += 1.0 / r;
                st.g[self.slots[sm + 1] as usize] -= (1.0 - share) / r;
                st.g[self.slots[sm + 2] as usize] -= share / r;
            }
        }
    }

    /// The instance that absorbed the resistor or capacitor `device`, with its value set to `v`
    /// (`key` as the part's own bank takes it); None when none did or the value cannot be the
    /// part's. The cell's conductance or capacitance follows.
    fn set_part(&mut self, device: &str, key: &str, v: f64) -> Option<usize> {
        let i = self.inst.iter().position(|c| c.parts.iter().any(|p| p.0 == device) || c.caps.iter().any(|k| k.0 == device))?;
        let c = &mut self.inst[i];
        let takes = if c.parts.iter().any(|p| p.0 == device) {
            matches!(key, "r" | "value") && v.is_finite() && v > 0.0
        } else {
            let rest: f64 = c.caps.iter().filter(|k| k.0 != device).map(|k| k.1).sum();
            matches!(key, "c" | "value") && v.is_finite() && v >= 0.0 && rest + v > 0.0
        };
        if !takes || !c.set_part(device, v) {
            return None;
        }
        self.g[i] = c.res.iter().map(|&(_, _, r)| 1.0 / r).sum();
        self.cap[i] = c.caps.iter().map(|&(_, f, _)| f).sum();
        Some(i)
    }
}

impl Bank for CellBank {
    fn kind(&self) -> &'static str {
        "cell"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let c = &self.inst[i];
        out.extend_from_slice(&[c.out, c.vdd, c.vss, c.inp]);
        out.extend(c.res.iter().map(|&(_, node, _)| node));
        out.extend(c.inner.iter().map(|&(node, _, _)| node));
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        let end = self.first.get(i + 1).copied().unwrap_or(self.slots.len());
        out.extend_from_slice(&self.slots[self.first[i]..end]);
    }
    /// The output's: into the output, into vdd (the up switch), into vss (the down switch); then
    /// each absorbed part's two ends (`Cell::terminal_of`): a resistor carries its chain's current,
    /// from its outside node into the input, and the capacitors share what reaches the input by
    /// their capacitance, so the pins of the parts a cell took read what they carry.
    fn terminals(&self) -> usize {
        self.terminals
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let c = &self.inst[inst];
        let m = self.model[inst];
        let (gu, gd) = if self.high[inst] { (m.up.0, m.down.1) } else { (m.up.1, m.down.0) };
        let ih = gu * (x[c.out as usize] - x[c.vdd as usize]);
        let il = gd * (x[c.out as usize] - x[c.vss as usize]);
        out[..3].copy_from_slice(&[ih + il, -ih, -il]);
        let v = x[c.inp as usize];
        let chain = |entry: u32| c.res.get(entry as usize).map_or(0.0, |&(_, a, r)| (x[a as usize] - v) / r);
        let mut k = 3;
        for &(_, entry, _, inward) in &c.parts {
            let i = if inward { -chain(entry) } else { chain(entry) };
            out[k..k + 2].copy_from_slice(&[i, -i]);
            k += 2;
        }
        let into: f64 = (0..c.res.len() as u32).map(chain).sum::<f64>() / self.cap[inst];
        for &(_, f, on_input) in &c.caps {
            let i = if on_input { into * f } else { -into * f };
            out[k..k + 2].copy_from_slice(&[i, -i]);
            k += 2;
        }
        out[k..self.terminals].fill(0.0);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        self.first.clear();
        self.slots.clear();
        for c in &self.inst {
            if c.res.is_empty() || c.res.iter().any(|&(_, _, r)| !(r.is_finite() && r > 0.0)) {
                return Err(format!("{}: a timing cell needs finite, positive resistors", c.name));
            }
            if !c.caps.iter().any(|&(_, f, _)| f > 0.0) || c.caps.iter().any(|&(_, f, _)| !(f.is_finite() && f >= 0.0)) {
                return Err(format!("{}: a timing cell needs a finite, positive capacitance", c.name));
            }
            self.first.push(self.slots.len());
            self.slots.extend_from_slice(&topo.slot4(c.out, c.vdd));
            self.slots.extend_from_slice(&topo.slot4(c.out, c.vss));
            for &(_, a, _) in &c.res {
                for &(_, b, _) in &c.res {
                    self.slots.push(topo.slot(a, b));
                }
            }
            self.slots.push(topo.slot(c.inp, c.inp));
            for &(_, a, _) in &c.res {
                self.slots.push(topo.slot(c.inp, a));
            }
            for &(m, entry, _) in &c.inner {
                let a = c.res.get(entry as usize).map(|r| r.1).ok_or_else(|| format!("{}: an inner node of no chain", c.name))?;
                self.slots.extend_from_slice(&[topo.slot(m, m), topo.slot(m, c.inp), topo.slot(m, a)]);
            }
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
        for ((f, &high), &(tl, vl)) in self.form.iter().zip(&self.high).zip(&self.late) {
            out.extend_from_slice(&[high as u8 as f64, f.t0, f.v0, f.vinf, f.tau, f.cross, f.flip, tl, vl]);
        }
    }
    fn commits(&self) -> u64 {
        self.commits
    }
    /// A flip the accepted point landed on turns the output; then each cell re-forms from there.
    /// One the point passed without landing on it (a step decided before its form moved it
    /// earlier: a provisional point's form is made once the step after it is solved, so a rail's
    /// jump there moves a flip into the step) does not turn inside the step: it is set
    /// `FLIP_LEAD` ahead, where the output turns, and the input's new form starts where the flip
    /// was due, so the oscillator's phase carries no error past the one late edge.
    fn accept(&mut self, x: &[f64], cx: &EvalCx) {
        self.at = cx.t;
        for i in 0..self.inst.len() {
            if !cx.tran {
                let v0 = self.form[i].v0;
                self.reform(i, cx.t, v0, x, false);
                continue;
            }
            // the step just solved held the outside nodes at their values here: its form with
            // them, and the flip it makes (one set within `FLIP_LEAD` of it stays put)
            let pre = self.form[i];
            if cx.t > pre.t0 {
                self.reform(i, pre.t0, pre.v0, x, false);
            }
            let v = self.value(i, cx.t);
            let f = self.form[i];
            // The stepper restarts on a corner it lands on. At its own, a flip the settle moved
            // off the point (what the step solved moved its crossing) turns here all the same
            // when it was due inside the step or is due within the step's tolerance after it,
            // its input's form from where it was due: no corner a hair off, no phase error.
            let moved = landed(pre.flip, cx.t) && !landed(f.flip, cx.t) && f.flip - cx.t <= cx.reltol * (cx.t - pre.t0);
            if landed(f.flip, cx.t) || moved {
                self.high[i] = !self.high[i];
                self.commits += 1;
                let (tl, vl) = std::mem::replace(&mut self.late[i], (f64::INFINITY, 0.0));
                if tl < cx.t {
                    self.reform(i, tl, vl, x, true);
                } else if moved {
                    let due = f.flip;
                    self.reform(i, due, self.value(i, due), x, true);
                } else {
                    self.reform(i, cx.t, v, x, true);
                }
            } else {
                if !after(f.flip, cx.t) && !self.late[i].0.is_finite() {
                    self.late[i] = (f.flip, self.value(i, f.flip));
                }
                self.reform(i, cx.t, v, x, false);
            }
        }
    }
    /// Before a cell's first point (or after a hand-over) its form takes the outside voltages
    /// from where the step starts.
    fn step_from(&mut self, x: &[f64]) {
        for i in 0..self.inst.len() {
            if self.fresh[i] {
                let f = self.form[i];
                self.reform(i, f.t0, f.v0, x, false);
            }
        }
    }
    fn next_break(&self, t: f64) -> f64 {
        self.form.iter().map(|f| f.flip).filter(|&b| after(b, t)).fold(f64::INFINITY, f64::min)
    }
    fn varies_after(&self, t: f64) -> bool {
        self.form.iter().enumerate().any(|(i, f)| (f.flip.is_finite() && after(f.flip, t)) || (self.value(i, t) - f.vinf).abs() > 1e-12 * (1.0 + f.vinf.abs()))
    }
    /// The output's position, the form's start, the input there, its crossing of the edge (a
    /// flip inside its delay keeps it) and a passed flip's time and input.
    fn state(&self, inst: usize) -> Vec<f64> {
        let f = self.form[inst];
        let (tl, vl) = self.late[inst];
        vec![self.high[inst] as u8 as f64, f.t0, f.v0, f.cross, tl, vl]
    }
    fn set_state(&mut self, inst: usize, s: &[f64]) {
        if let &[high, t0, v0, cross, tl, vl] = s {
            self.high[inst] = high != 0.0;
            self.form[inst] = Form { t0, v0, vinf: v0, tau: f64::INFINITY, cross, flip: f64::INFINITY };
            self.late[inst] = (tl, vl);
            self.fresh[inst] = true;
        }
    }
    /// A resistor or capacitor a cell absorbed takes its value from the last accepted point on:
    /// the input's form starts again there from its value (a crossing passed is kept, and with it
    /// a flip due within the delay) and waits for the outside voltages, as after a hand-over.
    fn set_absorbed(&mut self, device: &str, key: &str, v: f64) -> Option<usize> {
        let at = self.at;
        let i = self.inst.iter().position(|c| c.parts.iter().any(|p| p.0 == device) || c.caps.iter().any(|k| k.0 == device))?;
        let v0 = self.value(i, at);
        self.set_part(device, key, v)?;
        let f = self.form[i];
        self.form[i] = Form { t0: at, v0, vinf: v0, tau: f64::INFINITY, cross: f.cross, flip: f64::INFINITY };
        self.fresh[i] = true;
        Some(i)
    }
    /// The input's voltage at `t`, the down switch closed while the output is low, the up
    /// switch while it is high (their `Bank::state`).
    fn stands_for(&self, inst: usize, t: f64, out: &mut Vec<Given>) {
        let c = &self.inst[inst];
        let high = self.high[inst];
        out.push(Given::Unknown(c.input.clone(), self.value(inst, t)));
        out.push(Given::State(c.down.0.clone(), vec![!high as u8 as f64]));
        out.push(Given::State(c.up.0.clone(), vec![high as u8 as f64]));
    }
    /// The input's voltage at `t` from its node, the output's position from the down switch
    /// (open: high); the form waits for the outside voltages, as after `set_state`.
    fn adopt(&mut self, inst: usize, t: f64, from: &dyn RunState) -> bool {
        let c = &self.inst[inst];
        let (Some(v), Some(down)) = (from.value(&c.input), from.state_of(&c.down.0)) else { return false };
        let Some(&closed) = down.first() else { return false };
        self.set_state(inst, &[(closed == 0.0) as u8 as f64, t, v, f64::INFINITY, f64::INFINITY, 0.0]);
        true
    }
}
