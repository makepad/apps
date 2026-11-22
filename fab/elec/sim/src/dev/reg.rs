//! Averaged linear regulator: enable, dropout and current limits, with no internal poles.
//! Board capacitances are separate devices; this bank stamps only currents and conductances.
//! A Newton step that crosses a knee is limited to just past it (`knee_step`).

use crate::circuit::{ModelCard, Reg};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};

#[derive(Clone, Debug)]
pub struct RegModel {
    pub vref: f64,
    pub vdo: f64,
    pub rout: f64,
    pub iq: f64,
    pub ilim: f64,
    pub isink: f64,
    pub ven: f64,
    pub wen: f64,
    pub w: f64,
}

impl RegModel {
    pub fn from_card(m: &ModelCard) -> Self {
        Self {
            vref: m.or("vref", 3.3),
            vdo: m.or("vdo", 0.25),
            rout: m.or("rout", 0.05),
            iq: m.or("iq", 50e-6),
            ilim: m.or("ilim", 0.8),
            isink: m.or("isink", 1e-3),
            ven: m.or("ven", 1.0),
            wen: m.or("wen", 0.05),
            w: m.or("w", 0.02),
        }
    }

    /// Smoothing widths for the source limit, sink limit and zero-current split.
    fn current_widths(&self) -> (f64, f64, f64) {
        let wl = 1e-3 * self.ilim;
        let ws = if self.isink > 0.0 { 1e-3 * self.isink } else { wl };
        let wz = if self.iq > 0.0 { 1e-3 * self.iq } else { ws };
        (wl, ws, wz)
    }
}

/// (smin, d/da, d/db), where smin = (a + b - sqrt((a - b)^2 + w^2)) / 2.
fn smin(a: f64, b: f64, w: f64) -> (f64, f64, f64) {
    let d = a - b;
    let h = d.hypot(w);
    // Rationalise h - |d| and 1 - |d|/h to retain the small tails and their slopes.
    let tail = 0.5 * w * (w / (h + d.abs()));
    let small = tail / h;
    if d >= 0.0 {
        (b - tail, small, 1.0 - small)
    } else {
        (a - tail, 1.0 - small, small)
    }
}

/// (smax, d/da, d/db), with smax(a, b, w) = -smin(-a, -b, w).
fn smax(a: f64, b: f64, w: f64) -> (f64, f64, f64) {
    let (v, da, db) = smin(-a, -b, w);
    (-v, da, db)
}

/// (soft_on, d/dv) = ((1 + tanh(u))/2, (1 - tanh(u)^2)/(2w)), u = (v-thr)/w.
fn soft_on(v: f64, thr: f64, w: f64) -> (f64, f64) {
    let u = (v - thr) / w;
    // The equivalent logistic form preserves the tail below threshold without overflow.
    let e = (-2.0 * u.abs()).exp();
    let den = 1.0 + e;
    (if u >= 0.0 { 1.0 / den } else { e / den }, 2.0 * e / (w * den * den))
}

/// A current and its partials with respect to [vi, vo, ve], all relative to device ground.
#[derive(Clone, Copy, Debug, Default)]
struct Current {
    v: f64,
    d: [f64; 3],
}

impl Current {
    fn predict(self, delta: [f64; 3]) -> f64 {
        self.v + self.d[0] * delta[0] + self.d[1] * delta[1] + self.d[2] * delta[2]
    }
}

/// A limited step stops this many smoothing widths past the knee it crosses.
const KNEE: f64 = 3.0;

/// The fraction of a step from `q0` to `q1` that ends `KNEE` widths past the first knee it crosses,
/// or 1 when it crosses none. Each entry in `knees` is (knee, width). A step that starts within
/// half that knee's `KNEE` widths and ends beyond the full distance on its other side crosses it;
/// one that starts further out does not, so a limited step makes progress on the next iteration.
fn first_knee(q0: f64, q1: f64, knees: &[(f64, f64)]) -> f64 {
    let mut alpha = 1.0f64;
    for &(knee, w) in knees {
        let side = if q1 > knee { 1.0 } else { -1.0 };
        if side * (q1 - knee) > KNEE * w && side * (q0 - knee) <= 0.5 * KNEE * w {
            alpha = alpha.min((knee + side * KNEE * w - q0) / (q1 - q0));
        }
    }
    alpha
}

/// The bias ([vi, vo, ve]) to evaluate a Newton step from `b0` to `b1` at, when the step crosses
/// a knee: beyond one the linearisation says little about the other side (a limited output is a
/// current source), so Newton would otherwise jump far across the narrow regulation band and
/// back. The enable threshold and the dropout corner limit the input step; the output moves in
/// headroom (the regulated level less vo), so an input held at a knee keeps the output's place in
/// the band; then the output knees (the source limit, zero current and the sink limit) limit the
/// headroom.
fn knee_step(m: &RegModel, b0: [f64; 3], b1: [f64; 3]) -> Option<[f64; 3]> {
    let level = |vi: f64, ve: f64| {
        let (on, _) = soft_on(ve, m.ven, m.wen);
        (on * smin(m.vref, vi - m.vdo, m.w).0, on)
    };
    let alpha = first_knee(b0[2], b1[2], &[(m.ven, m.wen)])
        .min(first_knee(b0[0], b1[0], &[(m.vref + m.vdo, m.w)]));
    let (vi, ve) = (b0[0] + alpha * (b1[0] - b0[0]), b0[2] + alpha * (b1[2] - b0[2]));
    let u0 = level(b0[0], b0[2]).0 - b0[1];
    let u1 = level(b1[0], b1[2]).0 - b1[1];
    let ua = u0 + alpha * (u1 - u0);
    let (at, on) = level(vi, ve);
    let (wl, ws, wz) = m.current_widths();
    let knees = [
        (m.ilim * m.rout, wl * m.rout),
        (0.0, wz * m.rout),
        (-m.isink * on * m.rout, ws * m.rout),
    ];
    let beta = first_knee(u0, ua, &knees);
    (alpha < 1.0 || beta < 1.0).then(|| [vi, at - (u0 + beta * (ua - u0)), ve])
}

#[derive(Clone, Copy, Debug, Default)]
struct Last {
    bias: [f64; 3],
    io: Current,
    ii: Current,
}

#[derive(Clone)]
pub struct RegBank {
    pub inst: Vec<Reg>,
    // One owned copy per instance, including when several instances use the same card.
    model: Vec<RegModel>,
    last: Vec<Last>,
    slots: Vec<u32>,
}

const SLOTS: usize = 12;

impl RegBank {
    pub fn new(inst: Vec<Reg>, model: Vec<RegModel>) -> Self {
        let n = inst.len();
        Self { inst, model, last: vec![Last::default(); n], slots: Vec::new() }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let (r, m) = (&self.inst[i], &self.model[i]);
            let vg = x[r.gnd as usize];
            let actual = [x[r.vin as usize] - vg, x[r.vout as usize] - vg, x[r.en as usize] - vg];
            let last = self.last[i];
            // Evaluate at the limited bias and stamp its linearisation, extended to the actual
            // bias, as a diode does after pnjlim.
            let limited = if cx.limit { knee_step(m, last.bias, actual) } else { None };
            if limited.is_some() {
                st.not_converged(i);
            }
            let bias = limited.unwrap_or(actual);
            let [vi, vo, ve] = bias;
            let (on, don) = soft_on(ve, m.ven, m.wen);
            let (target, _, dtarget) = smin(m.vref, vi - m.vdo, m.w);
            let raw = (on * target - vo) / m.rout;
            let (wl, ws, wz) = m.current_widths();
            let (upper, dupper, _) = smin(raw, m.ilim, wl);
            let (out, dout, dsink) = smax(upper, -m.isink * on, ws);
            let gain = dout * dupper / m.rout;
            let io = Current {
                v: out,
                d: [gain * on * dtarget, -gain, (gain * target - dsink * m.isink) * don],
            };
            let (forward, dforward, _) = smax(io.v, 0.0, wz);
            // This 1 nS input shunt keeps vin from floating when the regulator is disabled.
            let idle = m.iq * on + 1e-9 * vi;
            let didle = [1e-9, 0.0, m.iq * don];
            let ii = Current {
                v: forward + idle,
                d: std::array::from_fn(|k| dforward * io.d[k] + didle[k]),
            };
            // smax(io, 0) - io = smax(-io, 0): avoid subtracting load currents to get
            // the much smaller ground current -(ii - io), including its derivatives.
            let (reverse, dreverse, _) = smax(-io.v, 0.0, wz);
            let ig = Current {
                v: -(reverse + idle),
                d: std::array::from_fn(|k| dreverse * io.d[k] - didle[k]),
            };

            let delta = std::array::from_fn(|k| bias[k] - last.bias[k]);
            let (po, pi) = (last.io.predict(delta), last.ii.predict(delta));
            if (po - io.v).abs() > cx.reltol * po.abs().max(io.v.abs()) + cx.abstol
                || (pi - ii.v).abs() > cx.reltol * pi.abs().max(ii.v.abs()) + cx.abstol
            {
                st.not_converged(i);
            }
            self.last[i] = Last { bias, io, ii };

            let s = &self.slots[i * SLOTS..(i + 1) * SLOTS];
            let output = Current { v: -io.v, d: io.d.map(|v| -v) };
            let off: [f64; 3] = std::array::from_fn(|k| actual[k] - bias[k]);
            for (row, (node, cur)) in [(r.vin, ii), (r.vout, output), (r.gnd, ig)].into_iter().enumerate() {
                st.f[node as usize] += if limited.is_some() { cur.predict(off) } else { cur.v };
                let [di, do_, de] = cur.d;
                for (col, g) in [di, -(di + do_ + de), do_, de].into_iter().enumerate() {
                    st.g[s[row * 4 + col] as usize] += g;
                }
            }
        }
    }
}

impl Bank for RegBank {
    fn terminals(&self) -> usize {
        4
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let (r, m) = (&self.inst[inst], &self.model[inst]);
        let vg = x[r.gnd as usize];
        let (vi, vo, ve) = (x[r.vin as usize] - vg, x[r.vout as usize] - vg, x[r.en as usize] - vg);
        let (on, _) = soft_on(ve, m.ven, m.wen);
        let (target, _, _) = smin(m.vref, vi - m.vdo, m.w);
        let raw = (on * target - vo) / m.rout;
        let (wl, ws, wz) = m.current_widths();
        let (upper, _, _) = smin(raw, m.ilim, wl);
        let (io, _, _) = smax(upper, -m.isink * on, ws);
        let (forward, _, _) = smax(io, 0.0, wz);
        let idle = m.iq * on + 1e-9 * vi;
        let (reverse, _, _) = smax(-io, 0.0, wz);
        out[..4].copy_from_slice(&[forward + idle, -(reverse + idle), -io, 0.0]);
    }
    fn kind(&self) -> &'static str {
        "Reg"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let r = &self.inst[i];
        out.extend_from_slice(&[r.vin, r.gnd, r.vout, r.en]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * SLOTS..(i + 1) * SLOTS]);
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for r in &self.inst {
            for row in [r.vin, r.vout, r.gnd] {
                for col in [r.vin, r.gnd, r.vout, r.en] {
                    self.slots.push(topo.slot(row, col));
                }
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
        for last in &self.last {
            out.extend_from_slice(&last.bias);
            out.push(last.io.v);
            out.extend_from_slice(&last.io.d);
            out.push(last.ii.v);
            out.extend_from_slice(&last.ii.d);
        }
    }
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        let Some(m) = self.model.get_mut(inst) else { return false };
        match key {
            "vref" => m.vref = v,
            "ilim" => m.ilim = v,
            "iq" => m.iq = v,
            "rout" => m.rout = v,
            _ => return false,
        }
        true
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        let m = self.model.get(inst)?;
        Some(match key {
            "vref" => m.vref,
            "ilim" => m.ilim,
            "iq" => m.iq,
            "rout" => m.rout,
            _ => return None,
        })
    }
    // The default state/set_state are empty: last is only a Newton prediction cache.
}
