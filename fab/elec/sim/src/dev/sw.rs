//! Voltage-controlled (S) and current-controlled (W) switches with hysteresis. A W reads the
//! branch current of its voltage source or inductor where an S reads its control voltage; the
//! model is the same in amps (`it`, `ih`, `iband` for `vt`, `vh`, `vband`).
//!
//! The conductance blends on a log scale across a band of width `vband` centred on the active
//! threshold (`vt + vh` while open, `vt - vh` while closed), with a C1 smoothstep, so Newton sees
//! no jump. The step is cut where the control enters the band, so a switching instant is located
//! to the tolerance and not to the step. The position flips at the first accepted point inside
//! the band when the hysteresis holds the other position there (a Schmitt trigger commits), and
//! otherwise once an accepted step has carried the control through the band.
//!
//! A step's Newton iterate that carries the control past the edge of the band the position can
//! enter evaluates the switch with the control held at that edge: the conductance stays what it
//! was. In a relaxation oscillator a step whose end lies past the edge has its solution inside
//! the band, where 12 decades of conductance turn on a few millivolts and the output swinging the
//! whole supply moves the input across the band: Newton from outside lands on either side in
//! turn and the step fails. Held, the iterate converges past the edge and the step is cut to the
//! crossing as above. The hold applies only while the step's start (the point the stepper
//! measures a crossing from) is outside the edge by more than a thousandth of the iterate's
//! travel, the share the stepper takes as a crossing at the start: a converged iterate that is held therefore always shows the
//! stepper a crossing it cuts to, or one within the 0.1 % of a step it lands on, and no accepted
//! point is otherwise a held one.

use crate::circuit::{ModelCard, ModelKind, Switch};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};

#[derive(Clone, Copy, Debug)]
pub struct SwModel {
    pub ln_gon: f64,
    pub ln_goff: f64,
    pub vt: f64,
    pub vh: f64,
    pub w: f64,
    /// The conductance at either end of the band, as `g` computes it there: outside the band a
    /// switch evaluates no `exp`.
    g_ends: [f64; 2],
}

impl SwModel {
    pub fn from_card(m: &ModelCard) -> SwModel {
        let current = m.kind == ModelKind::Csw;
        let (thr, his, band) = if current { ("it", "ih", "iband") } else { ("vt", "vh", "vband") };
        let ron = m.or("ron", 1.0);
        let roff = m.or("roff", 1e12);
        let vt = m.or(thr, 0.0);
        let vh = m.or(his, 0.0).abs();
        // a thousandth of the threshold's reach, at least 1 mV (S) or 1 uA (W)
        let floor = if current { 1e-3 } else { 1.0 };
        let w = m.or(band, 1e-3 * (vt.abs() + vh).max(floor));
        let (ln_gon, ln_goff) = ((1.0 / ron).ln(), (1.0 / roff).ln());
        let span = ln_gon - ln_goff;
        SwModel { ln_gon, ln_goff, vt, vh, w, g_ends: [(ln_goff + span * 0.0).exp(), (ln_goff + span * 1.0).exp()] }
    }
    fn thr(&self, on: bool) -> f64 {
        if on {
            self.vt - self.vh
        } else {
            self.vt + self.vh
        }
    }
    /// Where `vc` lies across the band, from 0 to 1.
    fn u(&self, on: bool, vc: f64) -> f64 {
        ((vc - self.thr(on)) / self.w + 0.5).clamp(0.0, 1.0)
    }
    /// (u in [0, 1] across the band, conductance, d conductance / d control).
    fn g(&self, on: bool, vc: f64) -> (f64, f64, f64) {
        let u = self.u(on, vc);
        let s = u * u * (3.0 - 2.0 * u);
        let ds = 6.0 * u * (1.0 - u) / self.w;
        let span = self.ln_gon - self.ln_goff;
        // at an end s is exactly 0 or 1, so the end's conductance is the same value
        let g = if u == 0.0 {
            self.g_ends[0]
        } else if u == 1.0 {
            self.g_ends[1]
        } else {
            (self.ln_goff + span * s).exp()
        };
        (u, g, g * span * ds)
    }
}

#[derive(Clone)]
pub struct SwBank {
    pub inst: Vec<Switch>,
    model: Vec<SwModel>,
    /// The control's two unknowns per instance: (cp, cn) for S, (branch, ground) for W.
    ctl: Vec<(Unk, Unk)>,
    on: Vec<bool>,
    /// The control at the step's start (`step_from`; NaN before one), which decides whether the
    /// step starts outside the band.
    vc0: Vec<f64>,
    /// Last linearisation per instance: (v, vc, i, di/dv, di/dvc).
    last: Vec<[f64; 5]>,
    slots: Vec<u32>,
    /// The flips `accept` has made (`Bank::commits`).
    commits: u64,
}

impl SwBank {
    pub fn new(inst: Vec<Switch>, model: Vec<SwModel>) -> Self {
        let on = inst.iter().map(|s| s.on).collect();
        let last = vec![[0.0; 5]; inst.len()];
        let vc0 = vec![f64::NAN; inst.len()];
        SwBank { inst, model, ctl: Vec::new(), on, vc0, last, slots: Vec::new(), commits: 0 }
    }
    fn vc(&self, i: usize, x: &[f64]) -> f64 {
        let (cp, cn) = self.ctl[i];
        x[cp as usize] - x[cn as usize]
    }
    /// The control a step's Newton iterate evaluates: held at the edge of the band the position
    /// can enter while the iterate is past it and the step's start is outside it by more than a
    /// thousandth of the iterate's travel (the stepper's crossing-at-the-start share).
    fn held(&self, i: usize, vc: f64) -> f64 {
        let (m, start) = (&self.model[i], self.vc0[i]);
        let (edge, outside, past) = if self.on[i] {
            let edge = m.thr(true) + 0.5 * m.w;
            (edge, start - edge, vc < edge)
        } else {
            let edge = m.thr(false) - 0.5 * m.w;
            (edge, edge - start, vc > edge)
        };
        if past && outside > 1e-3 * (vc - start).abs() { edge } else { vc }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let s = &self.inst[i];
            let v = x[s.p as usize] - x[s.n as usize];
            let (cp, cn) = self.ctl[i];
            let vc = x[cp as usize] - x[cn as usize];
            let vc = if cx.edge_hold && cx.limit { self.held(i, vc) } else { vc };
            let (_, g, dg) = self.model[i].g(self.on[i], vc);
            let cur = g * v;
            let didvc = dg * v;
            let l = self.last[i];
            let pred = l[2] + l[3] * (v - l[0]) + l[4] * (vc - l[1]);
            if (cur - pred).abs() > cx.reltol * cur.abs().max(pred.abs()) + cx.abstol {
                st.not_converged(i);
            }
            self.last[i] = [v, vc, cur, g, didvc];
            let sl = &self.slots[i * 8..i * 8 + 8];
            st.g4(&sl[0..4], g);
            st.g[sl[4] as usize] += didvc;
            st.g[sl[5] as usize] -= didvc;
            st.g[sl[6] as usize] -= didvc;
            st.g[sl[7] as usize] += didvc;
            st.i2(s.p, s.n, cur);
        }
    }
}

impl Bank for SwBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let s = &self.inst[inst];
        let vc = self.vc(inst, x);
        let (_, g, _) = self.model[inst].g(self.on[inst], vc);
        let i = g * (x[s.p as usize] - x[s.n as usize]);
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "S"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let s = &self.inst[i];
        let (cp, cn) = self.ctl[i];
        out.extend_from_slice(&[s.p, s.n, cp, cn]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * 8..(i + 1) * 8]);
    }
    fn nonlinear(&self) -> bool {
        true
    }
    /// At rest outside its band a switch is a fixed conductance: the next load would stamp the
    /// same one with no control slope, and its current is linear in `x`. Inside the band (or
    /// across to the other end) it answers false.
    fn settle(&mut self, x: &[f64], cx: &EvalCx) -> bool {
        (0..self.inst.len()).all(|i| {
            let vc = self.vc(i, x);
            let vc = if cx.edge_hold && cx.limit { self.held(i, vc) } else { vc };
            let (_, g, dg) = self.model[i].g(self.on[i], vc);
            let l = self.last[i];
            dg == 0.0 && l[4] == 0.0 && g.to_bits() == l[3].to_bits()
        })
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for s in &self.inst {
            let (cp, cn) = match &s.ctrl {
                Some(d) => (topo.branch_of(d).ok_or_else(|| format!("{}: no voltage source or inductor named {d} to control it", s.name))?, 0),
                None => (s.cp, s.cn),
            };
            self.ctl.push((cp, cn));
            self.slots.extend_from_slice(&topo.slot4(s.p, s.n));
            self.slots.extend_from_slice(&[topo.slot(s.p, cp), topo.slot(s.p, cn), topo.slot(s.n, cp), topo.slot(s.n, cn)]);
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
        for (&on, last) in self.on.iter().zip(&self.last) {
            out.push(on as u8 as f64);
            out.extend_from_slice(last);
        }
    }
    /// The position flips once the control has crossed the band, or, where the other position
    /// would hold it, as soon as the control is inside it: a switch with hysteresis then commits
    /// at the band's edge, as a Schmitt trigger's own regeneration does. Feedback that turns the
    /// control back inside the band (the RC of a relaxation oscillator) would otherwise hold it
    /// half closed, at the point where the blended output meets its own input. The step is cut
    /// where the control enters the band, so the flip lands on that crossing's restart.
    fn accept(&mut self, x: &[f64], _cx: &EvalCx) {
        for i in 0..self.inst.len() {
            let vc = self.vc(i, x);
            let m = &self.model[i];
            let on = self.on[i];
            let u = m.u(on, vc);
            // u in the other position: 1 holds a switch that closes, 0 one that opens
            let held = m.u(!on, vc);
            if !on && (u >= 1.0 || (u > 0.0 && held >= 1.0)) {
                self.on[i] = true;
                self.commits += 1;
            } else if on && (u <= 0.0 || (u < 1.0 && held <= 0.0)) {
                self.on[i] = false;
                self.commits += 1;
            }
        }
    }
    fn commits(&self) -> u64 {
        self.commits
    }
    fn step_from(&mut self, x: &[f64]) {
        for i in 0..self.inst.len() {
            self.vc0[i] = self.vc(i, x);
        }
    }
    fn crossing(&self, x0: &[f64], x1: &[f64]) -> Option<f64> {
        let mut first: Option<f64> = None;
        for (i, m) in self.model.iter().enumerate() {
            let (vc0, vc1) = (self.vc(i, x0), self.vc(i, x1));
            let (edge, crossed) = if self.on[i] {
                let e = m.thr(true) + 0.5 * m.w;
                (e, vc0 > e && vc1 <= e)
            } else {
                let e = m.thr(false) - 0.5 * m.w;
                (e, vc0 < e && vc1 >= e)
            };
            if crossed {
                let f = (edge - vc0) / (vc1 - vc0);
                first = Some(first.map_or(f, |g: f64| g.min(f)));
            }
        }
        first
    }
    fn state(&self, inst: usize) -> Vec<f64> {
        vec![self.on[inst] as u8 as f64]
    }
    /// The conductance at the held position's end of the band, the same across it.
    fn hold_position(&mut self, inst: usize, closed: Option<bool>) -> bool {
        let m = &mut self.model[inst];
        if closed.unwrap_or(!self.on[inst]) {
            m.ln_goff = m.ln_gon;
        } else {
            m.ln_gon = m.ln_goff;
        }
        m.g_ends = [m.ln_goff.exp(), m.ln_gon.exp()];
        true
    }
    fn set_state(&mut self, inst: usize, s: &[f64]) {
        if let Some(&v) = s.first() {
            self.on[inst] = v != 0.0;
        }
    }
}
