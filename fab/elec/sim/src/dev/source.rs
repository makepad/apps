//! Independent sources (V, I) and the four linear controlled sources (E, F, G, H).

use crate::circuit::{Controlled, CtlKind, Indep, Retuned, Source, Wave};
use crate::num::C64;
use crate::stamp::{Bank, EvalCx, Stamps, Tier, Topology, Unk};

/// A source with no waveform and no pending switch reads its DC value at every time: `Fixed`;
/// any other is `Timed`.
fn src_tier(s: &Indep) -> Tier {
    if s.src.then.is_none() && matches!(s.src.wave, Wave::None) { Tier::Fixed } else { Tier::Timed }
}

/// The source's value at the evaluation, read at the run's `cell` (`Source::value_in`).
fn src_value(s: &Indep, cx: &EvalCx, cell: f64) -> f64 {
    let v = if cx.tran { s.src.value_in(cx.t, cell) } else { s.src.value_dc() };
    v * cx.src_scale
}

/// Voltage sources: a branch current from p through the source to n.
#[derive(Clone)]
pub struct VBank {
    pub inst: Vec<Indep>,
    pub br: Vec<Unk>,
    slots: Vec<u32>,
    /// The cell the run's steps cross (`Bank::set_cell`), 0 for none.
    cell: f64,
}

impl VBank {
    pub fn new(inst: Vec<Indep>) -> Self {
        VBank { inst, br: Vec::new(), slots: Vec::new(), cell: 0.0 }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let v = &self.inst[i];
            let s = &self.slots[i * 4..i * 4 + 4];
            let br = self.br[i] as usize;
            st.g[s[0] as usize] += 1.0;
            st.g[s[1] as usize] -= 1.0;
            st.g[s[2] as usize] += 1.0;
            st.g[s[3] as usize] -= 1.0;
            st.i2(v.p, v.n, x[br]);
            st.f[br] += x[v.p as usize] - x[v.n as usize] - src_value(v, cx, self.cell);
        }
    }
}

impl Bank for VBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let i = x[self.br[inst] as usize];
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "V"
    }
    fn tier(&self, i: usize) -> Tier {
        src_tier(&self.inst[i])
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let v = &self.inst[i];
        out.extend_from_slice(&[v.p, v.n, self.br[i]]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * 4..(i + 1) * 4]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for v in &self.inst {
            let br = topo.branch(&v.name);
            self.br.push(br);
            self.slots.extend_from_slice(&[topo.slot(v.p, br), topo.slot(v.n, br), topo.slot(br, v.p), topo.slot(br, v.n)]);
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
    fn next_break(&self, t: f64) -> f64 {
        self.inst.iter().map(|v| v.src.next_break(t)).fold(f64::INFINITY, f64::min)
    }
    fn varies_after(&self, t: f64) -> bool {
        self.inst.iter().any(|v| v.src.varies_after(t))
    }
    fn ac_rhs(&self, rhs: &mut [C64]) {
        for (i, v) in self.inst.iter().enumerate() {
            if v.src.ac_mag != 0.0 {
                rhs[self.br[i] as usize] += C64::polar(v.src.ac_mag, v.src.ac_phase.to_radians());
            }
        }
    }
    fn set_param(&mut self, inst: usize, key: &str, val: f64) -> bool {
        let s = &mut self.inst[inst].src;
        match key {
            "dc" | "value" => s.dc = Some(val),
            "ac" | "ac_mag" => s.ac_mag = val,
            "ac_phase" => s.ac_phase = val,
            // a noise source's standard deviation, keeping its seed and spacing
            "sigma" => match &mut s.wave {
                Wave::Noise { sigma, .. } => *sigma = val,
                _ => return false,
            },
            _ => return false,
        }
        true
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        let s = &self.inst[inst].src;
        match key {
            "dc" | "value" => s.dc,
            "ac" | "ac_mag" => Some(s.ac_mag),
            "ac_phase" => Some(s.ac_phase),
            "sigma" => match s.wave {
                Wave::Noise { sigma, .. } => Some(sigma),
                _ => None,
            },
            _ => None,
        }
    }
    fn retune(&mut self, inst: usize, t: f64, source: Source) -> Option<Retuned> {
        Some(self.inst[inst].src.retune(t, source))
    }
    fn set_cell(&mut self, cell: f64) {
        self.cell = cell;
    }
}

/// Current sources: the current flows from p through the source to n.
#[derive(Clone)]
pub struct IBank {
    pub inst: Vec<Indep>,
    /// The transient time each source was last evaluated at, and its unscaled value there:
    /// every Newton iteration of a step reads the same values (a noise sample costs two normal
    /// draws), so they are evaluated once per time point. NaN when stale.
    at: Vec<f64>,
    values: Vec<f64>,
    /// The cell the run's steps cross (`Bank::set_cell`), 0 for none.
    cell: f64,
}

impl IBank {
    pub fn new(inst: Vec<Indep>) -> Self {
        let values = vec![0.0; inst.len()];
        let at = vec![f64::NAN; inst.len()];
        IBank { inst, at, values, cell: 0.0 }
    }
    fn eval_iter(&mut self, _x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for k in insts {
            let i = &self.inst[k];
            if !cx.tran {
                st.i2(i.p, i.n, src_value(i, cx, self.cell));
            } else {
                // A subset may omit any source, so freshness belongs to each instance.
                if self.at[k] != cx.t {
                    self.values[k] = i.src.value_in(cx.t, self.cell);
                    self.at[k] = cx.t;
                }
                let v = self.values[k];
                st.i2(i.p, i.n, v * cx.src_scale);
            }
        }
    }
}

impl Bank for IBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, _x: &[f64], _xdot: &[f64], cx: &EvalCx, out: &mut [f64]) {
        let i = src_value(&self.inst[inst], cx, self.cell);
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "I"
    }
    fn tier(&self, i: usize) -> Tier {
        src_tier(&self.inst[i])
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let src = &self.inst[i];
        out.extend_from_slice(&[src.p, src.n]);
    }
    fn inst_slots(&self, _i: usize, _out: &mut Vec<u32>) {}
    fn setup(&mut self, _topo: &mut Topology) -> Result<(), String> {
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut []
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn iter_state(&self, out: &mut Vec<f64>) {
        out.extend_from_slice(&self.at);
        out.extend_from_slice(&self.values);
    }
    fn next_break(&self, t: f64) -> f64 {
        self.inst.iter().map(|v| v.src.next_break(t)).fold(f64::INFINITY, f64::min)
    }
    fn varies_after(&self, t: f64) -> bool {
        self.inst.iter().any(|v| v.src.varies_after(t))
    }
    fn ac_rhs(&self, rhs: &mut [C64]) {
        for i in &self.inst {
            if i.src.ac_mag != 0.0 {
                let a = C64::polar(i.src.ac_mag, i.src.ac_phase.to_radians());
                rhs[i.p as usize] -= a;
                rhs[i.n as usize] += a;
            }
        }
    }
    fn set_param(&mut self, inst: usize, key: &str, val: f64) -> bool {
        self.at[inst] = f64::NAN;
        let s = &mut self.inst[inst].src;
        match key {
            "dc" | "value" => s.dc = Some(val),
            "ac" | "ac_mag" => s.ac_mag = val,
            "ac_phase" => s.ac_phase = val,
            // a noise source's standard deviation, keeping its seed and spacing
            "sigma" => match &mut s.wave {
                Wave::Noise { sigma, .. } => *sigma = val,
                _ => return false,
            },
            _ => return false,
        }
        true
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        let s = &self.inst[inst].src;
        match key {
            "dc" | "value" => s.dc,
            "ac" | "ac_mag" => Some(s.ac_mag),
            "ac_phase" => Some(s.ac_phase),
            "sigma" => match s.wave {
                Wave::Noise { sigma, .. } => Some(sigma),
                _ => None,
            },
            _ => None,
        }
    }
    fn retune(&mut self, inst: usize, t: f64, source: Source) -> Option<Retuned> {
        self.at[inst] = f64::NAN;
        Some(self.inst[inst].src.retune(t, source))
    }
    fn set_cell(&mut self, cell: f64) {
        if cell != self.cell {
            self.cell = cell;
            self.at.fill(f64::NAN);
        }
    }
}

/// E, F, G, H.
#[derive(Clone)]
pub struct CtlBank {
    pub inst: Vec<Controlled>,
    /// Own branch (E, H) and controlling branch (F, H); 0 where unused.
    br: Vec<(Unk, Unk)>,
    slots: Vec<u32>,
}

impl CtlBank {
    pub fn new(inst: Vec<Controlled>) -> Self {
        CtlBank { inst, br: Vec::new(), slots: Vec::new() }
    }
    fn eval_iter(&mut self, x: &[f64], _cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let e = &self.inst[i];
            let s = &self.slots[i * CTL_SLOTS..(i + 1) * CTL_SLOTS];
            let (own, ctrl) = self.br[i];
            let k = e.gain;
            match e.kind {
                CtlKind::Vcvs | CtlKind::Ccvs => {
                    let o = own as usize;
                    st.g[s[0] as usize] += 1.0;
                    st.g[s[1] as usize] -= 1.0;
                    st.g[s[2] as usize] += 1.0;
                    st.g[s[3] as usize] -= 1.0;
                    st.i2(e.p, e.n, x[o]);
                    let ctl = if e.kind == CtlKind::Vcvs {
                        st.g[s[4] as usize] -= k;
                        st.g[s[5] as usize] += k;
                        x[e.cp as usize] - x[e.cn as usize]
                    } else {
                        st.g[s[4] as usize] -= k;
                        x[ctrl as usize]
                    };
                    st.f[o] += x[e.p as usize] - x[e.n as usize] - k * ctl;
                }
                CtlKind::Vccs => {
                    st.g[s[0] as usize] += k;
                    st.g[s[1] as usize] -= k;
                    st.g[s[2] as usize] -= k;
                    st.g[s[3] as usize] += k;
                    st.i2(e.p, e.n, k * (x[e.cp as usize] - x[e.cn as usize]));
                }
                CtlKind::Cccs => {
                    st.g[s[0] as usize] += k;
                    st.g[s[1] as usize] -= k;
                    st.i2(e.p, e.n, k * x[ctrl as usize]);
                }
            }
        }
    }
}

const CTL_SLOTS: usize = 6;

impl Bank for CtlBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let e = &self.inst[inst];
        let (own, ctrl) = self.br[inst];
        let i = match e.kind {
            CtlKind::Vcvs | CtlKind::Ccvs => x[own as usize],
            CtlKind::Vccs => e.gain * (x[e.cp as usize] - x[e.cn as usize]),
            CtlKind::Cccs => e.gain * x[ctrl as usize],
        };
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "ctl"
    }
    fn tier(&self, _i: usize) -> Tier {
        Tier::Fixed
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let e = &self.inst[i];
        let (own, ctrl) = self.br[i];
        out.extend_from_slice(&[e.p, e.n, own]);
        match e.kind {
            CtlKind::Vcvs | CtlKind::Vccs => out.extend_from_slice(&[e.cp, e.cn]),
            CtlKind::Cccs | CtlKind::Ccvs => out.push(ctrl),
        }
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * CTL_SLOTS..(i + 1) * CTL_SLOTS]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for e in &self.inst {
            let ctrl = match e.kind {
                CtlKind::Cccs | CtlKind::Ccvs => {
                    topo.branch_of(&e.ctrl).ok_or_else(|| format!("{}: no voltage source or inductor named {} to sense", e.name, e.ctrl))?
                }
                _ => 0,
            };
            let own = match e.kind {
                CtlKind::Vcvs | CtlKind::Ccvs => topo.branch(&e.name),
                _ => 0,
            };
            self.br.push((own, ctrl));
            let s: [u32; CTL_SLOTS] = match e.kind {
                CtlKind::Vcvs => [
                    topo.slot(e.p, own),
                    topo.slot(e.n, own),
                    topo.slot(own, e.p),
                    topo.slot(own, e.n),
                    topo.slot(own, e.cp),
                    topo.slot(own, e.cn),
                ],
                CtlKind::Vccs => [topo.slot(e.p, e.cp), topo.slot(e.p, e.cn), topo.slot(e.n, e.cp), topo.slot(e.n, e.cn), 0, 0],
                CtlKind::Cccs => [topo.slot(e.p, ctrl), topo.slot(e.n, ctrl), 0, 0, 0, 0],
                CtlKind::Ccvs => [
                    topo.slot(e.p, own),
                    topo.slot(e.n, own),
                    topo.slot(own, e.p),
                    topo.slot(own, e.n),
                    topo.slot(own, ctrl),
                    0,
                ],
            };
            self.slots.extend_from_slice(&s);
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
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        match key {
            "gain" | "value" => {
                self.inst[inst].gain = v;
                true
            }
            _ => false,
        }
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        matches!(key, "gain" | "value").then(|| self.inst[inst].gain)
    }
}
