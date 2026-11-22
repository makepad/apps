//! Level-1 square-law MOSFETs, with body effect, channel-length modulation, bulk junctions,
//! series resistances, overlap charges and a conservative intrinsic charge partition.
//!
//! The card is S16's MOS1: `vto` (`vt0`), `kp` (or `u0` (`uo`, cm^2/V/s) with `tox`, giving
//! `u0 eps_ox / tox`), `gamma phi lambda rd rs cbd cbs is pb cgso cgdo cgbo rsh cj mj cjsw mjsw js
//! tox ld fc tnom`; the noise parameters `kf af` are not used. The channel runs on the effective length
//! `l - 2 ld`. A junction's saturation current is `js` times its area when both are given, else
//! `is`. With no `rd` / `rs`, the series resistances are `rsh` times the instance's squares `nrd` /
//! `nrs` (1 each by default). The instance multiplier `m` is that many devices in parallel. A card
//! of another `level`, or one that derives its threshold from the doping (`nsub`), is a located
//! setup error.
//!
//! The body root is `sqrt(max(phi - vbs, phi * 1e-3))`; its derivative is zero while clamped.
//! Intrinsic charge is continuous across accumulation, threshold, saturation and mode reversal
//! in the unclamped domain. Capacitances jump at threshold, as expected for this square-law
//! model. The specified body clamp is an exception to global charge continuity: at strongly
//! forward body bias the supplied cutoff and inversion formulas need not meet at threshold.
//! Junction temperature scaling uses silicon's 1.11 eV gap and exponent 3; channel parameters
//! and oxide/overlap capacitances retain their model-card values. All charges are functions of
//! voltage alone; the solver integrates them. Terminal order is drain, gate, source, bulk.

use super::junction::{self, depletion, exp_lin, pnjlim, vcrit};
use crate::circuit::{ModelCard, ModelKind, Mos};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};

/// The oxide's permittivity, F/m.
const EPS_OX: f64 = 3.9 * 8.854e-12;

#[derive(Clone, Debug)]
pub struct MosModel {
    /// The card's `level`: only 1 runs.
    pub level: f64,
    /// Substrate doping, cm^-3: nonzero is not supported yet.
    pub nsub: f64,
    /// Lateral diffusion: the channel runs on `l - 2 ld`.
    pub ld: f64,
    /// Diffusion sheet resistance, ohm per square.
    pub rsh: f64,
    /// Junction saturation current density, A/m^2.
    pub js: f64,
    pub p: f64,
    /// Threshold in polarity-normalised coordinates (a PMOS card normally has negative vto).
    pub vto: f64,
    pub kp: f64,
    pub gamma: f64,
    pub phi: f64,
    pub lambda: f64,
    pub rd: f64,
    pub rs: f64,
    pub cbd: Option<f64>,
    pub cbs: Option<f64>,
    pub is: f64,
    pub pb: f64,
    pub cgso: f64,
    pub cgdo: f64,
    pub cgbo: f64,
    pub cj: f64,
    pub mj: f64,
    pub cjsw: f64,
    pub mjsw: f64,
    pub tox: Option<f64>,
    pub fc: f64,
    /// Kelvin; absent takes the circuit nominal temperature.
    pub tnom: Option<f64>,
}

impl MosModel {
    pub fn from_card(m: &ModelCard) -> Self {
        let p = if m.kind == ModelKind::Pmos { -1.0 } else { 1.0 };
        let tox = m.get("tox");
        // kp from the mobility (cm^2/V/s) and the oxide when the card gives no kp
        let kp = match (m.get("kp"), m.get("u0").or(m.get("uo")), tox) {
            (Some(kp), _, _) => kp,
            (None, Some(u0), Some(tox)) if tox > 0.0 => u0 * 1e-4 * EPS_OX / tox,
            _ => 2e-5,
        };
        Self {
            level: m.or("level", 1.0),
            nsub: m.or("nsub", 0.0),
            ld: m.or("ld", 0.0),
            rsh: m.or("rsh", 0.0),
            js: m.or("js", 0.0),
            p, vto: p * m.get("vto").or(m.get("vt0")).unwrap_or(0.0), kp, gamma: m.or("gamma", 0.0),
            phi: m.or("phi", 0.6), lambda: m.or("lambda", 0.0), rd: m.or("rd", 0.0), rs: m.or("rs", 0.0),
            cbd: m.get("cbd"), cbs: m.get("cbs"), is: m.or("is", 1e-14), pb: m.or("pb", 0.8),
            cgso: m.or("cgso", 0.0), cgdo: m.or("cgdo", 0.0), cgbo: m.or("cgbo", 0.0),
            cj: m.or("cj", 0.0), mj: m.or("mj", 0.5), cjsw: m.or("cjsw", 0.0), mjsw: m.or("mjsw", 0.5),
            tox, fc: m.or("fc", 0.5), tnom: m.get("tnom").map(|t| t + 273.15),
        }
    }

    /// The series drain and source resistances of `inst`: the card's `rd` / `rs`, else `rsh`
    /// times the instance's squares.
    fn series(&self, inst: &Mos) -> (f64, f64) {
        let pick = |r: f64, squares: f64| if r > 0.0 { r } else { self.rsh * squares };
        (pick(self.rd, inst.nrd), pick(self.rs, inst.nrs))
    }

    fn validate(&self, inst: &Mos) -> Result<(), String> {
        if self.level != 1.0 {
            return Err(format!("{}: model {}: level {} is not supported (MOS level 1 only)", inst.name, inst.model, self.level));
        }
        if self.nsub != 0.0 {
            return Err(format!("{}: model {}: nsub (a threshold from the doping) is not supported yet: give vto, gamma and phi", inst.name, inst.model));
        }
        let leff = inst.l - 2.0 * self.ld;
        for (key, value) in [("w", inst.w), ("l", inst.l), ("l - 2 ld", leff), ("m", inst.mult), ("phi", self.phi), ("pb", self.pb)] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!("{}: {key} must be finite and positive", inst.name));
            }
        }
        for (key, value) in [
            ("kp", self.kp), ("gamma", self.gamma), ("lambda", self.lambda), ("rd", self.rd), ("rs", self.rs),
            ("is", self.is), ("cgso", self.cgso), ("cgdo", self.cgdo), ("cgbo", self.cgbo), ("cj", self.cj),
            ("cjsw", self.cjsw), ("ad", inst.ad), ("as", inst.as_), ("pd", inst.pd), ("ps", inst.ps),
            ("cbd", self.cbd.unwrap_or(0.0)), ("cbs", self.cbs.unwrap_or(0.0)),
            ("rsh", self.rsh), ("js", self.js), ("nrd", inst.nrd), ("nrs", inst.nrs), ("ld", self.ld),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{}: {key} must be finite and nonnegative", inst.name));
            }
        }
        for (key, value) in [("mj", self.mj), ("mjsw", self.mjsw), ("fc", self.fc)] {
            if !value.is_finite() || !(0.0..1.0).contains(&value) {
                return Err(format!("{}: {key} must be in [0, 1)", inst.name));
            }
        }
        for (key, value) in [("tox", self.tox), ("tnom (kelvin)", self.tnom), ("temp (kelvin)", inst.temp.map(|c| c + 273.15))] {
            if let Some(value) = value {
                if !value.is_finite() || value <= 0.0 {
                    return Err(format!("{}: {key} must be finite and positive", inst.name));
                }
            }
        }
        if !self.vto.is_finite() || (self.p != -1.0 && self.p != 1.0) {
            return Err(format!("{}: invalid threshold or polarity", inst.name));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct Derived {
    beta: f64,
    cox: f64,
    overlap: [f64; 3],
    vt: f64,
    /// Saturation current of the drain and source junctions.
    is: [f64; 2],
    pb: f64,
    bottom: [f64; 2],
    side: [f64; 2],
    vcrit: [f64; 2],
    gd: f64,
    gs: f64,
}

impl Derived {
    fn new(m: &MosModel, inst: &Mos, temp: f64, tnom: f64) -> Self {
        let tn = m.tnom.unwrap_or(tnom);
        let temp = inst.temp.map_or(temp, |c| c + 273.15);
        let vt = junction::vt(temp);
        // m devices in parallel: every current, charge and conductance times m
        let k = inst.mult;
        let saturation = |area: f64| {
            let i0 = if m.js > 0.0 && area > 0.0 { m.js * area } else { m.is };
            if i0 == 0.0 { 0.0 } else { k * junction::is_at(i0, temp, tn, 1.11, 3.0, 1.0) }
        };
        let is = [saturation(inst.ad), saturation(inst.as_)];
        let pb = junction::pb_at(m.pb, temp, tn);
        let cj = |c, grading| k * junction::cj_at(c, grading, temp, tn, m.pb, pb);
        let leff = inst.l - 2.0 * m.ld;
        let (rd, rs) = m.series(inst);
        Self {
            beta: k * m.kp * (inst.w / leff),
            cox: k * m.tox.map_or(0.0, |tox| EPS_OX / tox * inst.w * leff),
            overlap: [k * m.cgso * inst.w, k * m.cgdo * inst.w, k * m.cgbo * leff],
            vt, is, pb,
            bottom: [cj(m.cbd.unwrap_or(m.cj * inst.ad), m.mj), cj(m.cbs.unwrap_or(m.cj * inst.as_), m.mj)],
            side: [
                if m.cbd.is_some() { 0.0 } else { cj(m.cjsw * inst.pd, m.mjsw) },
                if m.cbs.is_some() { 0.0 } else { cj(m.cjsw * inst.ps, m.mjsw) },
            ],
            vcrit: is.map(|is| if is == 0.0 { f64::INFINITY } else { vcrit(vt, is) }),
            gd: if rd > 0.0 { k / rd } else { 0.0 },
            gs: if rs > 0.0 { k / rs } else { 0.0 },
        }
    }
}

/// A value and analytic partials in polarity-normalised terminal order [d, g, s, b].
#[derive(Clone, Copy, Debug, Default)]
struct Bias {
    v: f64,
    d: [f64; 4],
}

impl Bias {
    fn add(self, other: Self) -> Self {
        Self { v: self.v + other.v, d: std::array::from_fn(|k| self.d[k] + other.d[k]) }
    }
    fn scale(self, k: f64) -> Self {
        Self { v: self.v * k, d: self.d.map(|v| v * k) }
    }
    fn linearised(self, delta: [f64; 4]) -> f64 {
        self.v + self.d.iter().zip(delta).map(|(g, v)| g * v).sum::<f64>()
    }
    fn branch(v: f64, g: f64, a: usize, b: usize) -> Self {
        let mut d = [0.0; 4];
        d[a] = g;
        d[b] = -g;
        Self { v, d }
    }
}

/// Biases in the forward orientation; `reverse` selects which terminal acts as the source.
#[derive(Clone, Copy, Debug, Default)]
struct Channel {
    gs: f64,
    ds: f64,
    bs: f64,
    reverse: bool,
}

impl Channel {
    fn at(v: [f64; 4], reverse: bool) -> Self {
        let (d, s) = if reverse { (2, 0) } else { (0, 2) };
        Self { gs: v[1] - v[s], ds: v[d] - v[s], bs: v[3] - v[s], reverse }
    }
    fn voltages(self) -> [f64; 4] {
        if self.reverse { [-self.ds, self.gs - self.ds, 0.0, self.bs - self.ds] } else { [self.ds, self.gs, 0.0, self.bs] }
    }
    fn value(self, v: f64, gs: f64, ds: f64, bs: f64) -> Bias {
        let mut d = [ds, gs, -gs - ds - bs, bs];
        if self.reverse {
            d.swap(0, 2);
        }
        Bias { v, d }
    }
}

fn body(m: &MosModel, vbs: f64) -> (f64, f64, f64) {
    let arg = m.phi - vbs;
    let floor = m.phi * 1e-3;
    let s = arg.max(floor).sqrt();
    let ds = if arg > floor { -0.5 / s } else { 0.0 };
    (s, ds, m.vto + m.gamma * (s - m.phi.sqrt()))
}

fn channel(m: &MosModel, d: &Derived, v: Channel) -> (Bias, [Bias; 4]) {
    let (s, ds, vth) = body(m, v.bs);
    let eta = -m.gamma * ds;
    let vgt = v.gs - vth;
    let clm = 1.0 + m.lambda * v.ds;
    let (ids, gm, gds) = if vgt <= 0.0 {
        (0.0, 0.0, 0.0)
    } else if v.ds < vgt {
        let base = d.beta * (vgt - 0.5 * v.ds) * v.ds;
        (base * clm, d.beta * v.ds * clm, d.beta * (vgt - v.ds) * clm + base * m.lambda)
    } else {
        let base = 0.5 * d.beta * vgt * vgt;
        (base * clm, d.beta * vgt * clm, base * m.lambda)
    };
    let current = v.value(ids, gm, gds, gm * eta);
    let mut q = [Bias::default(); 4];
    if d.cox == 0.0 {
        return (current, q);
    }
    if vgt <= 0.0 {
        let vfb = m.vto - m.phi - m.gamma * m.phi.sqrt();
        let u = v.gs - v.bs - vfb;
        let (qg, c) = if u <= 0.0 {
            (d.cox * u, d.cox)
        } else if m.gamma == 0.0 {
            (0.0, 0.0)
        } else {
            // Rationalise the subtraction; hypot also avoids squaring gamma explicitly.
            let root = m.gamma.hypot(2.0 * u.sqrt());
            (2.0 * d.cox * u * (m.gamma / (root + m.gamma)), d.cox * m.gamma / root)
        };
        q[1] = v.value(qg, c, 0.0, -c);
        q[3] = q[1].scale(-1.0);
    } else {
        let linear = v.ds < vgt;
        let a = if linear { 1.0 - v.ds / vgt } else { 0.0 };
        let denom = 15.0 * (1.0 + a) * (1.0 + a);
        let partition = |n: f64, dn: f64| {
            let f = n / denom;
            let df = dn / denom - 2.0 * f / (1.0 + a);
            let dt = -d.cox * (f + if linear { df * (1.0 - a) } else { 0.0 });
            v.value(-d.cox * vgt * f, dt, if linear { d.cox * df } else { 0.0 }, dt * eta)
        };
        let qd = partition(4.0 + a * (8.0 + a * (12.0 + 6.0 * a)), 8.0 + a * (24.0 + 18.0 * a));
        let qs = partition(6.0 + a * (12.0 + a * (8.0 + 4.0 * a)), 12.0 + a * (16.0 + 12.0 * a));
        let (drain, source) = if v.reverse { (2, 0) } else { (0, 2) };
        q[drain] = qd;
        q[source] = qs;
        q[3] = v.value(-d.cox * m.gamma * s, 0.0, 0.0, d.cox * eta);
        q[1] = qd.add(qs).add(q[3]).scale(-1.0);
    }
    (current, q)
}

#[derive(Clone, Copy, Debug, Default)]
struct Rows {
    i: [Bias; 4],
    q: [Bias; 4],
}

fn pair(rows: &mut [Bias; 4], a: usize, b: usize, v: Bias) {
    rows[a] = rows[a].add(v);
    rows[b] = rows[b].add(v.scale(-1.0));
}

/// Independent junction limits need not describe the same point as the channel limits. Each
/// contribution is tangent-extended back to the requested voltages before rows are combined.
fn evaluate(m: &MosModel, d: &Derived, raw: [f64; 4], v: Channel, jun: [f64; 2], gmin: f64) -> Rows {
    let (mut ids, mut q) = channel(m, d, v);
    let point = v.voltages();
    let delta = std::array::from_fn(|k| raw[k] - point[k]);
    ids.v = ids.linearised(delta);
    for row in &mut q {
        row.v = row.linearised(delta);
    }
    let mut rows = Rows { i: [Bias::default(); 4], q };
    let (drain, source) = if v.reverse { (2, 0) } else { (0, 2) };
    pair(&mut rows.i, drain, source, ids);
    pair(&mut rows.i, 0, 2, Bias::branch(gmin * (raw[0] - raw[2]), gmin, 0, 2));
    for (k, terminal) in [0, 2].into_iter().enumerate() {
        let voltage = raw[3] - raw[terminal];
        let (i, g) = if d.is[k] == 0.0 {
            (0.0, 0.0)
        } else {
            let (e, de) = exp_lin(jun[k] / d.vt);
            (d.is[k] * (e - 1.0), d.is[k] * de / d.vt)
        };
        pair(&mut rows.i, 3, terminal, Bias::branch(i + g * (voltage - jun[k]) + gmin * voltage, g + gmin, 3, terminal));
        let (qb, cb) = depletion(jun[k], d.bottom[k], d.pb, m.mj, m.fc);
        let (qw, cw) = depletion(jun[k], d.side[k], d.pb, m.mjsw, m.fc);
        pair(&mut rows.q, 3, terminal, Bias::branch(qb + qw + (cb + cw) * (voltage - jun[k]), cb + cw, 3, terminal));
    }
    for (terminal, cap) in [2, 0, 3].into_iter().zip(d.overlap) {
        pair(&mut rows.q, 1, terminal, Bias::branch(cap * (raw[1] - raw[terminal]), cap, 1, terminal));
    }
    rows
}

#[derive(Clone, Copy, Debug, Default)]
struct Last {
    raw: [f64; 4],
    channel: Channel,
    jun: [f64; 2],
    i: [Bias; 4],
}

fn limited(m: &MosModel, d: &Derived, raw: [f64; 4], last: Last) -> (Channel, [f64; 2], bool) {
    let mut v = Channel::at(raw, raw[0] < raw[2]);
    // Re-express the previous point using the NEW forward source before applying step bounds.
    let old = Channel::at(last.channel.voltages(), v.reverse);
    let (_, _, vth) = body(m, old.bs);
    let gate_step = 0.5 + (old.gs - vth).abs();
    let mut gs = v.gs.max(old.gs - gate_step).min(old.gs + gate_step);
    if old.gs <= vth && v.gs > vth + 0.5 {
        gs = gs.min(vth + 0.5);
    }
    let drain_step = 1.0 + 2.0 * old.ds.abs();
    let ds = v.ds.max(old.ds - drain_step).min(old.ds + drain_step).max(0.0);
    let (bd, lbd) = pnjlim(raw[3] - raw[0], last.jun[0], d.vt, d.vcrit[0]);
    let (bs, lbs) = pnjlim(raw[3] - raw[2], last.jun[1], d.vt, d.vcrit[1]);
    let changed = gs != v.gs || ds != v.ds || lbd || lbs;
    v.gs = gs;
    v.ds = ds;
    v.bs = if v.reverse { bd } else { bs };
    (v, [bd, bs], changed)
}

fn voltages(nodes: [Unk; 4], x: &[f64], p: f64) -> Option<[f64; 4]> {
    let [d, g, s, b] = nodes.map(|u| x.get(u as usize).copied());
    let s = s?;
    Some([p * (d? - s), p * (g? - s), 0.0, p * (b? - s)])
}

#[derive(Clone)]
pub struct MosBank {
    inst: Vec<Mos>,
    model: Vec<MosModel>,
    der: Vec<Derived>,
    last: Vec<Last>,
    nodes: Vec<[Unk; 4]>,
    slots: Vec<u32>,
}

// Intrinsic 4x4 (including bulk junctions and overlaps), drain and source resistors.
const SLOTS: usize = 24;

impl MosBank {
    pub fn new(inst: Vec<Mos>, model: Vec<MosModel>) -> Self {
        let n = inst.len();
        Self { inst, model, der: vec![Derived::default(); n], last: vec![Last::default(); n], nodes: Vec::new(), slots: Vec::new() }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let nodes = self.nodes[i];
            let slots = &self.slots[i * SLOTS..(i + 1) * SLOTS];
            let (inst, m, d) = (&self.inst[i], &self.model[i], &self.der[i]);
            let Some(raw) = voltages(nodes, x, m.p) else { st.not_converged(i); continue };
            if nodes.into_iter().chain([inst.d, inst.s]).any(|u| u as usize >= st.f.len().min(st.q.len()).min(x.len()))
                || slots.iter().any(|&s| s as usize >= st.g.len().min(st.c.len())) {
                st.not_converged(i);
                continue;
            }
            let last = self.last[i];
            let (v, jun) = if !cx.limit {
                (Channel::at(raw, raw[0] < raw[2]), [raw[3] - raw[0], raw[3] - raw[2]])
            } else if cx.init_junctions {
                st.not_converged(i);
                (Channel { gs: if inst.off { 0.0 } else { m.vto + 0.5 }, ..Channel::default() }, [0.0; 2])
            } else {
                let (v, jun, changed) = limited(m, d, raw, last);
                if changed {
                    st.not_converged(i);
                }
                (v, jun)
            };
            let rows = evaluate(m, d, raw, v, jun, cx.gmin);
            let step = std::array::from_fn(|k| raw[k] - last.raw[k]);
            if last.i.iter().zip(rows.i).any(|(old, new)| {
                let pred = old.linearised(step);
                (pred - new.v).abs() > cx.reltol * pred.abs().max(new.v.abs()) + cx.abstol
            }) {
                st.not_converged(i);
            }
            self.last[i] = Last { raw, channel: v, jun, i: rows.i };
            for (r, node) in nodes.into_iter().enumerate() {
                st.f[node as usize] += m.p * rows.i[r].v;
                st.q[node as usize] += m.p * rows.q[r].v;
                for col in 0..4 {
                    let slot = slots[4 * r + col] as usize;
                    // Polarity enters both the input voltage and the output value, cancelling.
                    st.g[slot] += rows.i[r].d[col];
                    st.c[slot] += rows.q[r].d[col];
                }
            }
            for (ext, inner, g, start) in [(inst.d, nodes[0], d.gd, 16), (inst.s, nodes[2], d.gs, 20)] {
                if g > 0.0 {
                    st.g4(&slots[start..start + 4], g);
                    st.i2(ext, inner, g * (x[ext as usize] - x[inner as usize]));
                }
            }
        }
    }
}

impl Bank for MosBank {
    fn kind(&self) -> &'static str {
        "M"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        self.inst.get(i).map_or("", |inst| inst.name.as_str())
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let m = &self.inst[i];
        out.extend_from_slice(&[m.d, m.g, m.s, m.b]);
        out.extend_from_slice(&self.nodes[i]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * SLOTS..(i + 1) * SLOTS]);
    }
    fn terminals(&self) -> usize {
        4
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        if self.inst.len() != self.model.len() {
            return Err("MOS bank: instance/model count mismatch".into());
        }
        self.nodes.clear();
        self.slots.clear();
        for (inst, m) in self.inst.iter().zip(&self.model) {
            m.validate(inst)?;
            if [inst.d, inst.g, inst.s, inst.b].iter().any(|&u| u as usize >= topo.unknowns.len()) {
                return Err(format!("{}: invalid terminal node", inst.name));
            }
            let (rd, rs) = m.series(inst);
            let di = if rd > 0.0 { topo.internal(&inst.name, "d") } else { inst.d };
            let si = if rs > 0.0 { topo.internal(&inst.name, "s") } else { inst.s };
            let nodes = [di, inst.g, si, inst.b];
            self.nodes.push(nodes);
            for row in nodes {
                for col in nodes {
                    self.slots.push(topo.slot(row, col));
                }
            }
            self.slots.extend_from_slice(&if rd > 0.0 { topo.slot4(inst.d, di) } else { [0; 4] });
            self.slots.extend_from_slice(&if rs > 0.0 { topo.slot4(inst.s, si) } else { [0; 4] });
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn temperature(&mut self, temp: f64, tnom: f64) {
        for ((inst, m), d) in self.inst.iter().zip(&self.model).zip(&mut self.der) {
            *d = Derived::new(m, inst, temp, tnom);
        }
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], xdot: &[f64], cx: &EvalCx, out: &mut [f64]) {
        let (Some(inst), Some(m), Some(d), Some(&nodes)) =
            (self.inst.get(inst), self.model.get(inst), self.der.get(inst), self.nodes.get(inst)) else { return };
        let (Some(raw), Some(dot)) = (voltages(nodes, x, m.p), voltages(nodes, xdot, m.p)) else { return };
        let rows = evaluate(m, d, raw, Channel::at(raw, raw[0] < raw[2]), [raw[3] - raw[0], raw[3] - raw[2]], cx.gmin);
        for (k, output) in out.iter_mut().take(4).enumerate() {
            *output = m.p * (rows.i[k].v + rows.q[k].d.iter().zip(dot).map(|(c, dv)| c * dv).sum::<f64>());
            let resistor = match k {
                0 if d.gd > 0.0 => Some((inst.d, nodes[0], d.gd)),
                2 if d.gs > 0.0 => Some((inst.s, nodes[2], d.gs)),
                _ => None,
            };
            if let Some((ext, inner, g)) = resistor {
                if let (Some(&a), Some(&b)) = (x.get(ext as usize), x.get(inner as usize)) {
                    *output = g * (a - b);
                }
            }
        }
    }
    fn iter_state(&self, out: &mut Vec<f64>) {
        for last in &self.last {
            out.extend_from_slice(&last.raw);
            out.extend_from_slice(&[last.channel.gs, last.channel.ds, last.channel.bs, last.channel.reverse as u8 as f64]);
            out.extend_from_slice(&last.jun);
            for current in &last.i {
                out.push(current.v);
                out.extend_from_slice(&current.d);
            }
        }
    }
    fn state(&self, inst: usize) -> Vec<f64> {
        self.last.get(inst).map_or_else(Vec::new, |last| {
            let v = last.channel.voltages();
            vec![v[1] - v[2], v[0] - v[2], v[3] - v[2], last.jun[0], last.jun[1]]
        })
    }
    fn set_state(&mut self, inst: usize, state: &[f64]) {
        if let (Some(last), [gs, ds, bs, bdj, bsj, ..]) = (self.last.get_mut(inst), state) {
            if [gs, ds, bs, bdj, bsj].iter().all(|v| v.is_finite()) {
                let raw = [*ds, *gs, 0.0, *bs];
                *last = Last { raw, channel: Channel::at(raw, *ds < 0.0), jun: [*bdj, *bsj], ..Last::default() };
            }
        }
    }
}
