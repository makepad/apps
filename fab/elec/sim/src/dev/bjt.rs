//! Gummel-Poon bipolar transistors: transport, base recombination, Early effect, high injection,
//! bias-dependent base resistance and conservative depletion/diffusion charges.

use super::junction::{self, exp_lin, pnjlim, vcrit, Depletion};
use crate::circuit::{Bjt, ModelCard, ModelKind};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};
use std::f64::consts::PI;

#[derive(Clone, Debug)]
pub struct BjtModel {
    pub p: f64,
    pub is: f64,
    pub bf: f64,
    pub nf: f64,
    pub vaf: f64,
    pub ikf: f64,
    pub ise: f64,
    pub ne: f64,
    pub br: f64,
    pub nr: f64,
    pub var: f64,
    pub ikr: f64,
    pub isc: f64,
    pub nc: f64,
    pub rb: f64,
    pub irb: f64,
    pub rbm: f64,
    pub re: f64,
    pub rc: f64,
    pub cje: f64,
    pub vje: f64,
    pub mje: f64,
    pub tf: f64,
    pub xtf: f64,
    pub vtf: f64,
    pub itf: f64,
    pub cjc: f64,
    pub vjc: f64,
    pub mjc: f64,
    pub xcjc: f64,
    pub tr: f64,
    pub cjs: f64,
    pub vjs: f64,
    pub mjs: f64,
    pub xtb: f64,
    pub eg: f64,
    pub xti: f64,
    pub fc: f64,
    pub ptf: f64,
    /// Kelvin; `None` takes the circuit's nominal temperature.
    pub tnom: Option<f64>,
}

impl BjtModel {
    pub fn from_card(m: &ModelCard) -> Self {
        let rb = m.or("rb", 0.0);
        Self {
            p: if m.kind == ModelKind::Pnp { -1.0 } else { 1.0 },
            is: m.or("is", 1e-16),
            bf: m.or("bf", 100.0),
            nf: m.or("nf", 1.0),
            vaf: m.get("vaf").or(m.get("va")).unwrap_or(f64::INFINITY),
            ikf: m.get("ikf").or(m.get("ik")).unwrap_or(f64::INFINITY),
            ise: m.or("ise", 0.0),
            ne: m.or("ne", 1.5),
            br: m.or("br", 1.0),
            nr: m.or("nr", 1.0),
            var: m.get("var").or(m.get("vb")).unwrap_or(f64::INFINITY),
            ikr: m.or("ikr", f64::INFINITY),
            isc: m.or("isc", 0.0),
            nc: m.or("nc", 2.0),
            rb,
            irb: m.or("irb", f64::INFINITY),
            rbm: m.or("rbm", rb),
            re: m.or("re", 0.0),
            rc: m.or("rc", 0.0),
            cje: m.or("cje", 0.0),
            vje: m.get("vje").or(m.get("pe")).unwrap_or(0.75),
            mje: m.get("mje").or(m.get("me")).unwrap_or(0.33),
            tf: m.or("tf", 0.0),
            xtf: m.or("xtf", 0.0),
            vtf: m.or("vtf", f64::INFINITY),
            itf: m.or("itf", 0.0),
            cjc: m.or("cjc", 0.0),
            vjc: m.get("vjc").or(m.get("pc")).unwrap_or(0.75),
            mjc: m.get("mjc").or(m.get("mc")).unwrap_or(0.33),
            xcjc: m.or("xcjc", 1.0),
            tr: m.or("tr", 0.0),
            cjs: m.or("cjs", 0.0),
            vjs: m.get("vjs").or(m.get("ps")).unwrap_or(0.75),
            mjs: m.get("mjs").or(m.get("ms")).unwrap_or(0.0),
            xtb: m.or("xtb", 0.0),
            eg: m.or("eg", 1.11),
            xti: m.or("xti", 3.0),
            fc: m.or("fc", 0.5),
            ptf: m.or("ptf", 0.0),
            tnom: m.get("tnom").map(|t| t + 273.15),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Derived {
    vt: f64,
    is: f64,
    bf: f64,
    br: f64,
    ise: f64,
    isc: f64,
    ikf: f64,
    ikr: f64,
    rb: f64,
    rbm: f64,
    irb: f64,
    gc: f64,
    ge: f64,
    itf: f64,
    /// The junctions' depletion: base-emitter, intrinsic base-collector, the base-collector part
    /// outside the internal base (`1 - xcjc` of it), substrate.
    be: Depletion,
    bc: Depletion,
    bx: Depletion,
    sub: Depletion,
    vcrit_be: f64,
    vcrit_bc: f64,
}

impl Derived {
    fn new(m: &BjtModel, area: f64, temp: f64, tnom: f64) -> Self {
        let tn = m.tnom.unwrap_or(tnom);
        let vt = junction::vt(temp);
        let ist = junction::is_at(m.is, temp, tn, m.eg, m.xti, 1.0);
        let factor_b = (temp / tn).powf(m.xtb);
        let leakage = |i: f64, n: f64| {
            if m.is == 0.0 { i * area } else { i / factor_b * (ist / m.is).powf(1.0 / n) * area }
        };
        let vje = junction::pb_at(m.vje, temp, tn);
        let vjc = junction::pb_at(m.vjc, temp, tn);
        let vjs = junction::pb_at(m.vjs, temp, tn);
        let (cje, cjc, cjs) = (
            junction::cj_at(m.cje, m.mje, temp, tn, m.vje, vje) * area,
            junction::cj_at(m.cjc, m.mjc, temp, tn, m.vjc, vjc) * area,
            junction::cj_at(m.cjs, m.mjs, temp, tn, m.vjs, vjs) * area,
        );
        Self {
            vt,
            is: ist * area,
            bf: m.bf * factor_b,
            br: m.br * factor_b,
            ise: leakage(m.ise, m.ne),
            isc: leakage(m.isc, m.nc),
            ikf: m.ikf * area,
            ikr: m.ikr * area,
            rb: m.rb / area,
            rbm: m.rbm / area,
            irb: m.irb * area,
            gc: if m.rc > 0.0 { area / m.rc } else { 0.0 },
            ge: if m.re > 0.0 { area / m.re } else { 0.0 },
            itf: m.itf * area,
            be: Depletion::new(cje, vje, m.mje, m.fc),
            bc: Depletion::new(cjc, vjc, m.mjc, m.fc),
            bx: Depletion::new((1.0 - m.xcjc) * cjc, vjc, m.mjc, m.fc),
            sub: Depletion::new(cjs, vjs, m.mjs, m.fc),
            vcrit_be: vcrit(m.nf * vt, ist * area),
            vcrit_bc: vcrit(m.nr * vt, ist * area),
        }
    }
}

/// A value and its partial derivatives with respect to the polarity-normalised vbe and vbc.
#[derive(Clone, Copy, Debug, Default)]
struct Bias {
    v: f64,
    be: f64,
    bc: f64,
}

impl Bias {
    fn scale(self, k: f64) -> Self {
        Self { v: self.v * k, be: self.be * k, bc: self.bc * k }
    }
    fn add(self, other: Self) -> Self {
        Self { v: self.v + other.v, be: self.be + other.be, bc: self.bc + other.bc }
    }
    fn mul(self, other: Self) -> Self {
        Self { v: self.v * other.v, be: self.be * other.v + self.v * other.be, bc: self.bc * other.v + self.v * other.bc }
    }
    fn div(self, other: Self) -> Self {
        let v = self.v / other.v;
        Self { v, be: (self.be - v * other.be) / other.v, bc: (self.bc - v * other.bc) / other.v }
    }
    fn linearised(self, delta: [f64; 2]) -> f64 {
        self.v + self.be * delta[0] + self.bc * delta[1]
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Last {
    vbe: f64,
    vbc: f64,
    ic: Bias,
    ib: Bias,
    /// The rest of the last evaluation, which Fast's bypass reuses: the charges, the base
    /// resistance, the extrinsic and substrate junctions' voltages, charges and capacitances, and
    /// whether they were evaluated at `vbe`, `vbc` (a restored state sets those alone).
    qbe: Bias,
    qbc: Bias,
    rbb: Bias,
    vbext: f64,
    qx: f64,
    cxj: f64,
    vsub: f64,
    qs: f64,
    cs: f64,
    /// The junctions' `gmin` the currents include.
    gmin: f64,
    evaluated: bool,
}

struct Currents {
    ibf: Bias,
    ibr: Bias,
    qb: Bias,
    ic: Bias,
    ib: Bias,
}

fn currents(m: &BjtModel, d: &Derived, vbe: f64, vbc: f64, gmin: f64) -> Currents {
    let (ef, df) = exp_lin(vbe / (m.nf * d.vt));
    let (er, dr) = exp_lin(vbc / (m.nr * d.vt));
    let ibf = Bias { v: d.is * (ef - 1.0), be: d.is * df / (m.nf * d.vt), bc: 0.0 };
    let ibr = Bias { v: d.is * (er - 1.0), be: 0.0, bc: d.is * dr / (m.nr * d.vt) };
    let den = 1.0 - vbc / m.vaf - vbe / m.var;
    let q1 = 1.0 / den.max(1e-12);
    let q1 = Bias {
        v: q1,
        be: if den > 1e-12 { q1 * q1 / m.var } else { 0.0 },
        bc: if den > 1e-12 { q1 * q1 / m.vaf } else { 0.0 },
    };
    let q2 = ibf.scale(1.0 / d.ikf).add(ibr.scale(1.0 / d.ikr));
    let root = (1.0 + 4.0 * q2.v).max(0.0).sqrt();
    let high = Bias {
        v: 0.5 * (1.0 + root),
        be: if root > 0.0 { q2.be / root } else { 0.0 },
        bc: if root > 0.0 { q2.bc / root } else { 0.0 },
    };
    let qb = q1.mul(high);
    let ice = ibf.add(ibr.scale(-1.0)).div(qb);
    let (ee, de) = exp_lin(vbe / (m.ne * d.vt));
    let (ec, dc) = exp_lin(vbc / (m.nc * d.vt));
    let ibe = ibf.scale(1.0 / d.bf).add(Bias { v: d.ise * (ee - 1.0) + gmin * vbe, be: d.ise * de / (m.ne * d.vt) + gmin, bc: 0.0 });
    let ibc = ibr.scale(1.0 / d.br).add(Bias { v: d.isc * (ec - 1.0) + gmin * vbc, be: 0.0, bc: d.isc * dc / (m.nc * d.vt) + gmin });
    Currents { ibf, ibr, qb, ic: ice.add(ibc.scale(-1.0)), ib: ibe.add(ibc) }
}

/// The finite-IRB spreading factor and d(factor)/dx, x = max(ib/irb, 0).
fn spreading(x: f64) -> (f64, f64) {
    let root = (1.0 + 144.0 / (PI * PI) * x).sqrt();
    // Rationalise sqrt(1 + a*x) - 1; work in z^2 so x = 0 needs no division by sqrt(x).
    let z2 = 36.0 * x / ((root + 1.0) * (root + 1.0));
    let dz2 = 36.0 / (root * (root + 1.0) * (root + 1.0));
    let (f, df) = if z2 < 0.1 {
        // 3(tan(z) - z)/(z tan(z)^2), including its x -> 0 limit. The series also
        // avoids cancellation in tan(z) - z well above x = 1e-12.
        let coefficients = [1.0, -4.0 / 15.0, -4.0 / 105.0, -8.0 / 1575.0, -4.0 / 6237.0,
            -5528.0 / 70945875.0, -8.0 / 868725.0, -173616.0 / 162820783125.0];
        let (mut f, mut df) = (coefficients[7], 0.0);
        for &a in coefficients[..7].iter().rev() {
            df = df * z2 + f;
            f = f * z2 + a;
        }
        (f, df)
    } else {
        let z = z2.sqrt();
        let cot = 1.0 / z.tan();
        let f = 3.0 * (cot / z - cot * cot);
        let df = 3.0 * ((2.0 * cot - 1.0 / z) * (1.0 + cot * cot) - cot / z2) / (2.0 * z);
        (f, df)
    };
    (f, df * dz2)
}

/// The evaluation a load keeps: currents and charges at the (limited) junction voltages `vbe`,
/// `vbc`, the extrinsic and substrate junctions at `vbext`, `vsub`.
fn evaluate(m: &BjtModel, d: &Derived, vbe: f64, vbc: f64, vbext: f64, vsub: f64, gmin: f64) -> Last {
    let cur = currents(m, d, vbe, vbc, gmin);
    let qbe = be_charge(m, d, &cur, vbe, vbc);
    let (qj, cj) = d.bc.at(vbc);
    let qbc = Bias { v: m.xcjc * qj, be: 0.0, bc: m.xcjc * cj }.add(cur.ibr.scale(m.tr));
    let rbb = if m.rb > 0.0 { base_resistance(d, &cur) } else { Bias::default() };
    let (qx, cxj) = d.bx.at(vbext);
    let (qs, cs) = d.sub.at(vsub);
    Last { vbe, vbc, ic: cur.ic, ib: cur.ib, qbe, qbc, rbb, vbext, qx, cxj, vsub, qs, cs, gmin, evaluated: true }
}

/// The current through the base resistance, `x[b] - x[bi]` over the resistance `r` kept at the
/// junction voltages `delta` back, carried along its partials.
fn base_current(r: Bias, vb: f64, delta: [f64; 2]) -> f64 {
    let g = 1.0 / r.v;
    let ibb = vb * g;
    Bias { v: ibb, be: -ibb * g * r.be, bc: -ibb * g * r.bc }.linearised(delta)
}

/// Fast's bypass band on every junction: the next load would keep `last`.
fn holds(last: &Last, v: [f64; 4], cx: &EvalCx) -> bool {
    cx.near(v[0], last.vbe, cx.vntol) && cx.near(v[1], last.vbc, cx.vntol) && cx.near(v[2], last.vbext, cx.vntol) && cx.near(v[3], last.vsub, cx.vntol)
}

fn base_resistance(d: &Derived, cur: &Currents) -> Bias {
    let mut r = if d.irb.is_finite() {
        let x = cur.ib.v / d.irb;
        let (f, df) = spreading(x.max(0.0));
        let slope = if x > 0.0 { (d.rb - d.rbm) * df / d.irb } else { 0.0 };
        Bias { v: (d.rb - d.rbm) * f, be: slope * cur.ib.be, bc: slope * cur.ib.bc }
    } else {
        Bias { v: d.rb - d.rbm, be: 0.0, bc: 0.0 }.div(cur.qb)
    };
    r.v += d.rbm;
    r
}

fn be_charge(m: &BjtModel, d: &Derived, cur: &Currents, vbe: f64, vbc: f64) -> Bias {
    let (qj, cj) = d.be.at(vbe);
    let mut q = Bias { v: qj, be: cj, bc: 0.0 };
    if m.tf != 0.0 {
        q = q.add(cur.ibf.div(cur.qb).scale(m.tf));
        if m.xtf != 0.0 {
            let den = cur.ibf.v + d.itf;
            let storage = if d.itf == 0.0 {
                // ibf * (ibf/(ibf + itf))^2 has the removable limit ibf here. At
                // ibf = 0 its value is zero, and its derivative still includes d(ibf).
                cur.ibf
            } else if den == 0.0 {
                Bias::default()
            } else {
                let ratio = Bias { v: cur.ibf.v / den, be: (d.itf / den) * (cur.ibf.be / den), bc: 0.0 };
                cur.ibf.mul(ratio.mul(ratio))
            };
            let ev = (vbc / (1.44 * m.vtf)).exp();
            let field = Bias { v: ev, be: 0.0, bc: ev / (1.44 * m.vtf) };
            q = q.add(storage.div(cur.qb).mul(field).scale(m.tf * m.xtf));
        }
    }
    q
}

/// Intrinsic rows [c', b', e'], columns [b', e', c']. Polarity cancels in the derivatives.
fn intrinsic(matrix: &mut [f64], slots: &[u32], rows: [Bias; 3]) {
    for (r, v) in rows.iter().enumerate() {
        for (col, dv) in [v.be + v.bc, -v.be, -v.bc].into_iter().enumerate() {
            matrix[slots[r * 3 + col] as usize] += dv;
        }
    }
}

#[derive(Clone)]
pub struct BjtBank {
    pub inst: Vec<Bjt>,
    model: Vec<BjtModel>,
    der: Vec<Derived>,
    last: Vec<Last>,
    bi: Vec<Unk>,
    ci: Vec<Unk>,
    ei: Vec<Unk>,
    slots: Vec<u32>,
    temp: f64,
    tnom: f64,
    /// `Options::bypass`: Fast may keep a resting transistor's last evaluation (`EvalCx::bypass`).
    pub bypass: bool,
    /// Evaluations the bypass skipped (`Bank::bypassed`).
    bypassed: u64,
}

// Intrinsic 3x3, base resistor 2x4, collector/emitter resistors, external BC and substrate.
const SLOTS: usize = 33;

impl BjtBank {
    pub fn new(inst: Vec<Bjt>, model: Vec<BjtModel>) -> Self {
        let n = inst.len();
        Self {
            inst, model, der: vec![Derived::default(); n], last: vec![Last::default(); n],
            bi: Vec::new(), ci: Vec::new(), ei: Vec::new(), slots: Vec::new(), temp: 300.15, tnom: 300.15,
            bypass: false, bypassed: 0,
        }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let (q, m, d) = (&self.inst[i], &self.model[i], &self.der[i]);
            let (bi, ci, ei) = (self.bi[i], self.ci[i], self.ei[i]);
            let (be, bc) = (m.p * (x[bi as usize] - x[ei as usize]), m.p * (x[bi as usize] - x[ci as usize]));
            let vbext = m.p * (x[q.b as usize] - x[ci as usize]);
            let vsub = m.p * (x[q.s as usize] - x[ci as usize]);
            let last = self.last[i];
            let s = &self.slots[i * SLOTS..(i + 1) * SLOTS];
            // Fast's bypass: every junction moved by less than a tenth of the tolerance since the
            // last evaluation, made at the junctions' `gmin` now (a failed step's retry raises it),
            // whose stamps stand (the same stamps, so the matrix does not change), its currents and
            // charges carried along their slopes
            let bypassed = self.bypass && cx.bypass && !cx.init_junctions && last.evaluated && last.gmin == cx.gmin && holds(&last, [be, bc, vbext, vsub], cx);
            let lin = if bypassed {
                self.bypassed += 1;
                last
            } else {
                let (vbe, vbc) = if !cx.limit {
                (be, bc)
            } else if cx.init_junctions {
                st.not_converged(i);
                (if q.off { 0.0 } else { d.vcrit_be }, 0.0)
            } else {
                let (vbe, lbe) = pnjlim(be, last.vbe, m.nf * d.vt, d.vcrit_be);
                let (vbc, lbc) = pnjlim(bc, last.vbc, m.nr * d.vt, d.vcrit_bc);
                if lbe || lbc {
                    st.not_converged(i);
                }
                    (vbe, vbc)
                };
                let lin = evaluate(m, d, vbe, vbc, vbext, vsub, cx.gmin);
                let step = [be - last.vbe, bc - last.vbc];
                let pred_ic = last.ic.linearised(step);
                let pred_ib = last.ib.linearised(step);
                if (pred_ic - lin.ic.v).abs() > cx.reltol * pred_ic.abs().max(lin.ic.v.abs()) + cx.abstol
                    || (pred_ib - lin.ib.v).abs() > cx.reltol * pred_ib.abs().max(lin.ib.v.abs()) + cx.abstol {
                    st.not_converged(i);
                }
                self.last[i] = lin;
                lin
            };
            let delta = [be - lin.vbe, bc - lin.vbc];
            intrinsic(st.g, &s[0..9], [lin.ic, lin.ib, lin.ic.add(lin.ib).scale(-1.0)]);
            st.i2(ci, ei, m.p * lin.ic.linearised(delta));
            st.i2(bi, ei, m.p * lin.ib.linearised(delta));

            let (qbe, qbc) = (lin.qbe, lin.qbc);
            intrinsic(st.c, &s[0..9], [qbc.scale(-1.0), qbe.add(qbc), qbe.scale(-1.0)]);
            st.q2(bi, ei, m.p * qbe.linearised(delta));
            st.q2(bi, ci, m.p * qbc.linearised(delta));

            if m.rb > 0.0 {
                let r = lin.rbb;
                let g = 1.0 / r.v;
                // The external base drop is an independent, unlimited voltage. Its current
                // uses physical polarity; the junction partials therefore acquire one p.
                let ibb = (x[q.b as usize] - x[bi as usize]) * g;
                let ir = Bias { v: ibb, be: -ibb * g * r.be, bc: -ibb * g * r.bc };
                let cols = [g, -g + m.p * (ir.be + ir.bc), -m.p * ir.be, -m.p * ir.bc];
                for (col, value) in cols.into_iter().enumerate() {
                    st.g[s[9 + col] as usize] += value;
                    st.g[s[13 + col] as usize] -= value;
                }
                st.i2(q.b, bi, base_current(r, x[q.b as usize] - x[bi as usize], delta));
            }
            if d.gc > 0.0 {
                st.g4(&s[17..21], d.gc);
                st.i2(q.c, ci, d.gc * (x[q.c as usize] - x[ci as usize]));
            }
            if d.ge > 0.0 {
                st.g4(&s[21..25], d.ge);
                st.i2(q.e, ei, d.ge * (x[q.e as usize] - x[ei as usize]));
            }
            // a bypass carries the extrinsic and substrate charges along their capacitances
            let (qx, qs) = if bypassed { (lin.qx + lin.cxj * (vbext - lin.vbext), lin.qs + lin.cs * (vsub - lin.vsub)) } else { (lin.qx, lin.qs) };
            st.c4(&s[25..29], lin.cxj);
            st.q2(q.b, ci, m.p * qx);
            st.c4(&s[29..33], lin.cs);
            st.q2(q.s, ci, m.p * qs);
        }
    }
}

impl Bank for BjtBank {
    fn terminals(&self) -> usize {
        4
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], xdot: &[f64], cx: &EvalCx, out: &mut [f64]) {
        let (q, m, d) = (&self.inst[inst], &self.model[inst], &self.der[inst]);
        let (bi, ci, ei) = (self.bi[inst] as usize, self.ci[inst] as usize, self.ei[inst] as usize);
        let p = m.p;
        let (vbe, vbc) = (p * (x[bi] - x[ei]), p * (x[bi] - x[ci]));
        let (vbe_dot, vbc_dot) = (p * (xdot[bi] - xdot[ei]), p * (xdot[bi] - xdot[ci]));
        let cur = currents(m, d, vbe, vbc, cx.gmin);
        let qbe = be_charge(m, d, &cur, vbe, vbc);
        let (qj, cj) = d.bc.at(vbc);
        let qbc = Bias { v: m.xcjc * qj, be: 0.0, bc: m.xcjc * cj }.add(cur.ibr.scale(m.tr));
        let qbe_dot = qbe.be * vbe_dot + qbe.bc * vbc_dot;
        let qbc_dot = qbc.be * vbe_dot + qbc.bc * vbc_dot;
        let vbext = p * (x[q.b as usize] - x[ci]);
        let (_, cxj) = d.bx.at(vbext);
        let qx_dot = cxj * p * (xdot[q.b as usize] - xdot[ci]);
        let vsub = p * (x[q.s as usize] - x[ci]);
        let (_, cs) = d.sub.at(vsub);
        let qs_dot = cs * p * (xdot[q.s as usize] - xdot[ci]);
        out[0] = if m.rc > 0.0 {
            d.gc * (x[q.c as usize] - x[ci])
        } else {
            p * cur.ic.v - p * qbc_dot - p * qx_dot - p * qs_dot
        };
        out[1] = if m.rb > 0.0 {
            let g = 1.0 / base_resistance(d, &cur).v;
            (x[q.b as usize] - x[bi]) * g + p * qx_dot
        } else {
            p * cur.ib.v + p * qbe_dot + p * qbc_dot + p * qx_dot
        };
        out[2] = if m.re > 0.0 {
            d.ge * (x[q.e as usize] - x[ei])
        } else {
            -p * (cur.ic.v + cur.ib.v) - p * qbe_dot
        };
        out[3] = p * qs_dot;
    }
    fn kind(&self) -> &'static str {
        "Q"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let q = &self.inst[i];
        out.extend_from_slice(&[q.c, q.b, q.e, q.s, self.ci[i], self.bi[i], self.ei[i]]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * SLOTS..(i + 1) * SLOTS]);
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for (q, m) in self.inst.iter().zip(&self.model) {
            if m.ptf != 0.0 {
                return Err(format!("{}: ptf (excess phase) is not supported yet", q.name));
            }
            let bi = if m.rb > 0.0 { topo.internal(&q.name, "b") } else { q.b };
            let ci = if m.rc > 0.0 { topo.internal(&q.name, "c") } else { q.c };
            let ei = if m.re > 0.0 { topo.internal(&q.name, "e") } else { q.e };
            self.bi.push(bi);
            self.ci.push(ci);
            self.ei.push(ei);
            for row in [ci, bi, ei] {
                for col in [bi, ei, ci] {
                    self.slots.push(topo.slot(row, col));
                }
            }
            for row in [q.b, bi] {
                for col in [q.b, bi, ei, ci] {
                    self.slots.push(if m.rb > 0.0 { topo.slot(row, col) } else { 0 });
                }
            }
            self.slots.extend_from_slice(&if m.rc > 0.0 { topo.slot4(q.c, ci) } else { [0; 4] });
            self.slots.extend_from_slice(&if m.re > 0.0 { topo.slot4(q.e, ei) } else { [0; 4] });
            self.slots.extend_from_slice(&topo.slot4(q.b, ci));
            self.slots.extend_from_slice(&topo.slot4(q.s, ci));
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn temperature(&mut self, temp: f64, tnom: f64) {
        self.temp = temp;
        self.tnom = tnom;
        for (i, (q, m)) in self.inst.iter().zip(&self.model).enumerate() {
            self.der[i] = Derived::new(m, q.area, temp, tnom);
            self.last[i].evaluated = false;
        }
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn iter_state(&self, out: &mut Vec<f64>) {
        for last in &self.last {
            out.extend_from_slice(&[last.vbe, last.vbc, last.ic.v, last.ic.be, last.ic.bc, last.ib.v, last.ib.be, last.ib.bc]);
            for b in [last.qbe, last.qbc, last.rbb] {
                out.extend_from_slice(&[b.v, b.be, b.bc]);
            }
            out.extend_from_slice(&[last.vbext, last.qx, last.cxj, last.vsub, last.qs, last.cs, last.gmin, last.evaluated as u8 as f64]);
        }
    }
    /// A live edit of the instance's area or a model value; the series resistances may change but
    /// not appear or vanish (that would add or remove an internal node).
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        if inst >= self.inst.len() || !v.is_finite() {
            return false;
        }
        let m = &mut self.model[inst];
        let (has_rb, has_re, has_rc) = (m.rb > 0.0, m.re > 0.0, m.rc > 0.0);
        let (field, ok) = match key {
            "area" => (&mut self.inst[inst].area, v > 0.0),
            "is" => (&mut m.is, v > 0.0),
            "bf" => (&mut m.bf, v > 0.0),
            "br" => (&mut m.br, v > 0.0),
            "nf" => (&mut m.nf, v > 0.0),
            "nr" => (&mut m.nr, v > 0.0),
            "vaf" | "va" => (&mut m.vaf, v > 0.0),
            "var" | "vb" => (&mut m.var, v > 0.0),
            "ikf" | "ik" => (&mut m.ikf, v > 0.0),
            "ikr" => (&mut m.ikr, v > 0.0),
            "ise" => (&mut m.ise, v >= 0.0),
            "ne" => (&mut m.ne, v > 0.0),
            "isc" => (&mut m.isc, v >= 0.0),
            "nc" => (&mut m.nc, v > 0.0),
            "rb" => (&mut m.rb, v > 0.0 && has_rb),
            "rbm" => (&mut m.rbm, v > 0.0 && has_rb),
            "re" => (&mut m.re, v > 0.0 && has_re),
            "rc" => (&mut m.rc, v > 0.0 && has_rc),
            "cje" => (&mut m.cje, v >= 0.0),
            "cjc" => (&mut m.cjc, v >= 0.0),
            "cjs" => (&mut m.cjs, v >= 0.0),
            "tf" => (&mut m.tf, v >= 0.0),
            "tr" => (&mut m.tr, v >= 0.0),
            _ => return false,
        };
        if !ok {
            return false;
        }
        *field = v;
        self.der[inst] = Derived::new(&self.model[inst], self.inst[inst].area, self.temp, self.tnom);
        self.last[inst].evaluated = false;
        true
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        let m = self.model.get(inst)?;
        Some(match key {
            "area" => self.inst[inst].area,
            "is" => m.is,
            "bf" => m.bf,
            "br" => m.br,
            "nf" => m.nf,
            "nr" => m.nr,
            "vaf" | "va" => m.vaf,
            "var" | "vb" => m.var,
            "ikf" | "ik" => m.ikf,
            "ikr" => m.ikr,
            "ise" => m.ise,
            "ne" => m.ne,
            "isc" => m.isc,
            "nc" => m.nc,
            "rb" => m.rb,
            "rbm" => m.rbm,
            "re" => m.re,
            "rc" => m.rc,
            "cje" => m.cje,
            "cjc" => m.cjc,
            "cjs" => m.cjs,
            "tf" => m.tf,
            "tr" => m.tr,
            _ => return None,
        })
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
            let (q, m, d) = (&self.inst[i], &self.model[i], &self.der[i]);
            let (bi, ci, ei) = (self.bi[i] as usize, self.ci[i] as usize, self.ei[i] as usize);
            let (be, bc) = (m.p * (x[bi] - x[ei]), m.p * (x[bi] - x[ci]));
            let (vbext, vsub) = (m.p * (x[q.b as usize] - x[ci]), m.p * (x[q.s as usize] - x[ci]));
            let last = self.last[i];
            if !last.evaluated {
                return false;
            }
            // grade 1: the next load would bypass it
            if last.gmin == cx.gmin && holds(&last, [be, bc, vbext, vsub], cx) {
                continue;
            }
            // grade 2: evaluated afresh where the limiters would leave it, on the solve's lines
            if !cx.settle
                || (cx.limit && (pnjlim(be, last.vbe, m.nf * d.vt, d.vcrit_be).1 || pnjlim(bc, last.vbc, m.nr * d.vt, d.vcrit_bc).1))
            {
                return false;
            }
            let now = evaluate(m, d, be, bc, vbext, vsub, cx.gmin);
            let step = [be - last.vbe, bc - last.vbc];
            let vb = x[q.b as usize] - x[bi];
            let agrees = cx.agrees(now.ic.v, last.ic.linearised(step), cx.abstol)
                && cx.agrees(now.ib.v, last.ib.linearised(step), cx.abstol)
                && cx.agrees(now.qbe.v, last.qbe.linearised(step), cx.chgtol)
                && cx.agrees(now.qbc.v, last.qbc.linearised(step), cx.chgtol)
                && cx.agrees(now.qx, last.qx + last.cxj * (vbext - last.vbext), cx.chgtol)
                && cx.agrees(now.qs, last.qs + last.cs * (vsub - last.vsub), cx.chgtol)
                && (m.rb <= 0.0 || cx.agrees(vb / now.rbb.v, base_current(last.rbb, vb, step), cx.abstol));
            if !agrees {
                return false;
            }
            self.last[i] = now;
        }
        true
    }
    fn state(&self, inst: usize) -> Vec<f64> {
        vec![self.last[inst].vbe, self.last[inst].vbc]
    }
    fn set_state(&mut self, inst: usize, s: &[f64]) {
        if let [vbe, vbc, ..] = s {
            self.last[inst].vbe = *vbe;
            self.last[inst].vbc = *vbc;
            self.last[inst].evaluated = false;
        }
    }
}
