//! The junction diode (E3 2.1): series resistance, diffusion and recombination currents, high
//! injection, reverse breakdown, transit time, graded depletion capacitance, temperature.
//!
//! Parameters by their classic names: `is n rs cjo vj m fc tt bv ibv nbv isr nr ikf eg xti`.

use super::junction::{self, exp_lin, pnjlim, vcrit, Depletion};
use crate::circuit::{Diode, ModelCard};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};

#[derive(Clone, Debug)]
pub struct DiodeModel {
    pub is: f64,
    pub n: f64,
    pub rs: f64,
    pub cjo: f64,
    pub vj: f64,
    pub m: f64,
    pub fc: f64,
    pub tt: f64,
    pub bv: f64,
    pub ibv: f64,
    pub nbv: f64,
    pub isr: f64,
    pub nr: f64,
    pub ikf: f64,
    pub eg: f64,
    pub xti: f64,
    /// Kelvin; `None` takes the circuit's nominal temperature.
    pub tnom: Option<f64>,
}

impl DiodeModel {
    pub fn from_card(m: &ModelCard) -> DiodeModel {
        let n = m.or("n", 1.0);
        DiodeModel {
            is: m.or("is", 1e-14),
            n,
            rs: m.or("rs", 0.0),
            cjo: m.get("cjo").or(m.get("cj0")).unwrap_or(0.0),
            vj: m.get("vj").or(m.get("pb")).unwrap_or(1.0),
            m: m.or("m", 0.5),
            fc: m.or("fc", 0.5),
            tt: m.or("tt", 0.0),
            bv: m.or("bv", f64::INFINITY),
            ibv: m.or("ibv", 1e-3),
            nbv: m.or("nbv", n),
            isr: m.or("isr", 0.0),
            nr: m.or("nr", 2.0),
            ikf: m.or("ikf", f64::INFINITY),
            eg: m.or("eg", 1.11),
            xti: m.or("xti", 3.0),
            tnom: m.get("tnom").map(|t| t + 273.15),
        }
    }
}

/// Values derived for the circuit temperature and the instance's area.
#[derive(Clone, Debug, Default)]
struct Derived {
    vt: f64,
    is: f64,
    isr: f64,
    ikf: f64,
    ibv: f64,
    gs: f64,
    vcrit: f64,
    depl: Depletion,
}

/// The junction's last linearisation: voltage used, current, conductance, charge and
/// capacitance (depletion and transit time), the junction's `gmin` they include, and whether they
/// were evaluated at `vd` (a restored state sets `vd` alone).
#[derive(Clone, Copy, Debug, Default)]
struct Last {
    vd: f64,
    id: f64,
    gd: f64,
    q: f64,
    c: f64,
    gmin: f64,
    evaluated: bool,
}

#[derive(Clone)]
pub struct DiodeBank {
    pub inst: Vec<Diode>,
    model: Vec<DiodeModel>,
    der: Vec<Derived>,
    last: Vec<Last>,
    /// The junction's anode side: an internal node when the model has series resistance.
    ai: Vec<Unk>,
    slots: Vec<u32>,
    /// `Options::bypass`: Fast may keep a resting junction's last evaluation (`EvalCx::bypass`).
    pub bypass: bool,
    /// Evaluations the bypass skipped (`Bank::bypassed`).
    bypassed: u64,
    /// The circuit temperature and its nominal (`temperature`), kelvin.
    temp: f64,
    tnom: f64,
}

const SLOTS: usize = 8;

impl DiodeBank {
    pub fn new(inst: Vec<Diode>, model: Vec<DiodeModel>) -> Self {
        let n = inst.len();
        DiodeBank {
            inst, model, der: vec![Derived::default(); n], last: vec![Last::default(); n], ai: Vec::new(), slots: Vec::new(),
            bypass: false, bypassed: 0, temp: 300.15, tnom: 300.15,
        }
    }

    /// Instance `i`'s values at the bank's temperature. Its last evaluation was of the old values,
    /// so Fast does not bypass it.
    fn derive(&mut self, i: usize) {
        let (d, m) = (&self.inst[i], &self.model[i]);
        let tn = m.tnom.unwrap_or(self.tnom);
        let vt = junction::vt(self.temp);
        let is = junction::is_at(m.is, self.temp, tn, m.eg, m.xti, m.n) * d.area;
        let isr = junction::is_at(m.isr, self.temp, tn, m.eg, m.xti, m.nr) * d.area;
        let vj = junction::pb_at(m.vj, self.temp, tn);
        let cj0 = junction::cj_at(m.cjo, m.m, self.temp, tn, m.vj, vj) * d.area;
        self.der[i] = Derived {
            vt,
            is,
            isr,
            ikf: m.ikf * d.area,
            ibv: m.ibv * d.area,
            gs: if m.rs > 0.0 { d.area / m.rs } else { 0.0 },
            vcrit: vcrit(m.n * vt, is),
            depl: Depletion::new(cj0, vj, m.m, m.fc),
        };
        // the last evaluation was of the old temperature's junction: no bypass on it
        self.last[i].evaluated = false;
    }

    /// The model's DC current and conductance at junction voltage `vd`, and the diffusion part
    /// (for the transit-time charge).
    fn current(m: &DiodeModel, d: &Derived, vd: f64, gmin: f64) -> (f64, f64, f64, f64) {
        let nvt = m.n * d.vt;
        let (e, de) = exp_lin(vd / nvt);
        let mut idf = d.is * (e - 1.0);
        let mut gdf = d.is * de / nvt;
        if d.ikf.is_finite() && idf > 0.0 {
            // high injection: idf sqrt(ikf / (ikf + idf))
            let s = (d.ikf / (d.ikf + idf)).sqrt();
            let ds = -0.5 * s / (d.ikf + idf);
            gdf = gdf * s + idf * ds * gdf;
            idf *= s;
        }
        let (mut id, mut gd) = (idf, gdf);
        if d.isr > 0.0 {
            let nrvt = m.nr * d.vt;
            let (e, de) = exp_lin(vd / nrvt);
            id += d.isr * (e - 1.0);
            gd += d.isr * de / nrvt;
        }
        if m.bv.is_finite() {
            let nbvt = m.nbv * d.vt;
            let (e, de) = exp_lin(-(m.bv + vd) / nbvt);
            id -= d.ibv * e;
            gd += d.ibv * de / nbvt;
        }
        (id + gmin * vd, gd + gmin, idf, gdf)
    }
    /// The junction voltage limited from `vold` (SPICE's `pnjlim`, and below the breakdown knee
    /// the same limiting mirrored on `-(v + bv)`), and whether the limiter acted.
    fn limited(m: &DiodeModel, dv: &Derived, vx: f64, vold: f64) -> (f64, bool) {
        let nvt = m.n * dv.vt;
        let (mut v, mut lim) = pnjlim(vx, vold, nvt, dv.vcrit);
        if m.bv.is_finite() && v < (-m.bv + 10.0 * m.nbv * dv.vt).min(0.0) {
            // the breakdown knee mirrored: the same limiting on -(v + bv)
            let nbvt = m.nbv * dv.vt;
            let (r, l2) = pnjlim(-(v + m.bv), -(vold + m.bv), nbvt, vcrit(nbvt, dv.ibv));
            v = -(r + m.bv);
            lim |= l2;
        }
        (v, lim)
    }
    /// Current, conductance, charge and capacitance at `vd` (the evaluation a load keeps).
    fn evaluate(m: &DiodeModel, dv: &Derived, vd: f64, gmin: f64) -> Last {
        let (id, gd, idf, gdf) = Self::current(m, dv, vd, gmin);
        let (qj, cj) = dv.depl.at(vd);
        Last { vd, id, gd, q: qj + m.tt * idf, c: cj + m.tt * gdf, gmin, evaluated: true }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let (d, m, dv) = (&self.inst[i], &self.model[i], &self.der[i]);
            let ai = self.ai[i];
            let vx = x[ai as usize] - x[d.k as usize];
            let last = self.last[i];
            let s = &self.slots[i * SLOTS..(i + 1) * SLOTS];
            // Fast's bypass: the junction moved by less than a tenth of the tolerance since its last
            // evaluation, made at the junction's `gmin` now (a failed step's retry raises it), so
            // that evaluation's stamps stand, its current and charge carried along their slopes (the
            // same stamps, so the matrix does not change)
            if self.bypass && cx.bypass && !cx.init_junctions && last.evaluated && last.gmin == cx.gmin && cx.near(vx, last.vd, cx.vntol) {
                st.g4(&s[0..4], last.gd);
                st.i2(ai, d.k, last.id + last.gd * (vx - last.vd));
                if last.c != 0.0 || last.q != 0.0 {
                    st.c4(&s[0..4], last.c);
                    st.q2(ai, d.k, last.q + last.c * (vx - last.vd));
                }
                if dv.gs > 0.0 {
                    st.g4(&s[4..8], dv.gs);
                    st.i2(d.a, ai, dv.gs * (x[d.a as usize] - x[ai as usize]));
                }
                self.bypassed += 1;
                continue;
            }
            let vd = if cx.init_junctions {
                st.not_converged(i);
                if d.off {
                    0.0
                } else {
                    dv.vcrit
                }
            } else if cx.limit {
                let (v, lim) = Self::limited(m, dv, vx, last.vd);
                if lim {
                    st.not_converged(i);
                }
                v
            } else {
                vx
            };
            let now = Self::evaluate(m, dv, vd, cx.gmin);
            let Last { id, gd, q, c, .. } = now;
            let pred = last.id + last.gd * (vx - last.vd);
            if (pred - id).abs() > cx.reltol * pred.abs().max(id.abs()) + cx.abstol {
                st.not_converged(i);
            }
            st.g4(&s[0..4], gd);
            st.i2(ai, d.k, id + gd * (vx - vd));
            self.last[i] = now;
            if c != 0.0 || q != 0.0 {
                st.c4(&s[0..4], c);
                st.q2(ai, d.k, q + c * (vx - vd));
            }
            if dv.gs > 0.0 {
                st.g4(&s[4..8], dv.gs);
                st.i2(d.a, ai, dv.gs * (x[d.a as usize] - x[ai as usize]));
            }
        }
    }
}

impl Bank for DiodeBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], xdot: &[f64], cx: &EvalCx, out: &mut [f64]) {
        let (d, m, dv) = (&self.inst[inst], &self.model[inst], &self.der[inst]);
        let i = if m.rs > 0.0 {
            dv.gs * (x[d.a as usize] - x[self.ai[inst] as usize])
        } else {
            let vd = x[d.a as usize] - x[d.k as usize];
            let (id, _, _, gdf) = Self::current(m, dv, vd, cx.gmin);
            let (_, cj) = dv.depl.at(vd);
            id + (cj + m.tt * gdf) * (xdot[d.a as usize] - xdot[d.k as usize])
        };
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "D"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let d = &self.inst[i];
        out.extend_from_slice(&[d.a, d.k, self.ai[i]]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * SLOTS..(i + 1) * SLOTS]);
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for (d, m) in self.inst.iter().zip(&self.model) {
            let ai = if m.rs > 0.0 { topo.internal(&d.name, "a") } else { d.a };
            self.ai.push(ai);
            let s4 = topo.slot4(ai, d.k);
            let r4 = if m.rs > 0.0 { topo.slot4(d.a, ai) } else { [0; 4] };
            self.slots.extend_from_slice(&s4);
            self.slots.extend_from_slice(&r4);
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn temperature(&mut self, temp: f64, tnom: f64) {
        (self.temp, self.tnom) = (temp, tnom);
        for i in 0..self.inst.len() {
            self.derive(i);
        }
    }
    /// A live edit of the instance's area or a model value; the series resistance may change but
    /// not appear or vanish (that would add or remove the internal node).
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        if inst >= self.inst.len() || !v.is_finite() {
            return false;
        }
        let m = &mut self.model[inst];
        match key {
            "area" if v > 0.0 => self.inst[inst].area = v,
            "is" if v > 0.0 => m.is = v,
            "n" if v > 0.0 => m.n = v,
            "rs" if v > 0.0 && m.rs > 0.0 => m.rs = v,
            "cjo" | "cj0" if v >= 0.0 => m.cjo = v,
            "vj" | "pb" if v > 0.0 => m.vj = v,
            "m" if v > 0.0 && v < 1.0 => m.m = v,
            "fc" if (0.0..1.0).contains(&v) => m.fc = v,
            "tt" if v >= 0.0 => m.tt = v,
            "bv" if v > 0.0 => m.bv = v,
            "ibv" if v > 0.0 => m.ibv = v,
            "nbv" if v > 0.0 => m.nbv = v,
            "isr" if v >= 0.0 => m.isr = v,
            "nr" if v > 0.0 => m.nr = v,
            "ikf" if v > 0.0 => m.ikf = v,
            "eg" => m.eg = v,
            "xti" => m.xti = v,
            _ => return false,
        }
        self.derive(inst);
        true
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        let m = self.model.get(inst)?;
        Some(match key {
            "area" => self.inst[inst].area,
            "is" => m.is,
            "n" => m.n,
            "rs" => m.rs,
            "cjo" | "cj0" => m.cjo,
            "vj" | "pb" => m.vj,
            "m" => m.m,
            "fc" => m.fc,
            "tt" => m.tt,
            "bv" => m.bv,
            "ibv" => m.ibv,
            "nbv" => m.nbv,
            "isr" => m.isr,
            "nr" => m.nr,
            "ikf" => m.ikf,
            "eg" => m.eg,
            "xti" => m.xti,
            _ => return None,
        })
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn iter_state(&self, out: &mut Vec<f64>) {
        for last in &self.last {
            out.extend_from_slice(&[last.vd, last.id, last.gd, last.q, last.c, last.gmin, last.evaluated as u8 as f64]);
        }
    }
    fn state(&self, inst: usize) -> Vec<f64> {
        vec![self.last[inst].vd]
    }
    fn set_state(&mut self, inst: usize, s: &[f64]) {
        if let Some(&v) = s.first() {
            self.last[inst].vd = v;
            self.last[inst].evaluated = false;
        }
    }
    fn value_edited(&mut self) {
        self.last.iter_mut().for_each(|last| last.evaluated = false);
    }
    fn bypassed(&self) -> u64 {
        self.bypassed
    }
    fn settle(&mut self, x: &[f64], cx: &EvalCx) -> bool {
        if !self.bypass || cx.init_junctions {
            return false;
        }
        for i in 0..self.inst.len() {
            let (d, m, dv) = (&self.inst[i], &self.model[i], &self.der[i]);
            let vx = x[self.ai[i] as usize] - x[d.k as usize];
            let last = self.last[i];
            if !last.evaluated {
                return false;
            }
            // grade 1: the next load would bypass it
            if last.gmin == cx.gmin && cx.near(vx, last.vd, cx.vntol) {
                continue;
            }
            // grade 2: evaluated afresh where the limiter would leave it, on the solve's line
            if !cx.settle || (cx.limit && Self::limited(m, dv, vx, last.vd).1) {
                return false;
            }
            let now = Self::evaluate(m, dv, vx, cx.gmin);
            let step = vx - last.vd;
            if !cx.agrees(now.id, last.id + last.gd * step, cx.abstol) || !cx.agrees(now.q, last.q + last.c * step, cx.chgtol) {
                return false;
            }
            self.last[i] = now;
        }
        true
    }
}
