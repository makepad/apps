//! Transmission lines (T, and O with an `Ltra` card) by the method of characteristics.
//!
//! A line from port 1 (p1, n1) to port 2 (p2, n2) claims the two port currents i1 and i2, each
//! flowing into its port's + terminal and out of its - terminal. With the per-metre r, l, g, c
//! and the length len: z0 = sqrt(l / c), the delay td = len sqrt(l c) and the attenuation
//! a = exp(-len r / z0). A wave launched at one port arrives at the other td later and a times
//! weaker:
//!
//! ```text
//! v1(t) - z0 i1(t) = a (v2(t - td) + z0 i2(t - td))
//! v2(t) - z0 i2(t) = a (v1(t - td) + z0 i1(t - td))
//! ```
//!
//! This is exact for a lossless line (r = g = 0) and a distortionless one (r / l = g / c, so that
//! z0 is real at every frequency). The rows are linear; nothing iterates.
//! - In a transient each right-hand side comes from the line's own record of the accepted waves
//!   `v + z0 i`, interpolated linearly at t - td. The record is known up to the last accepted
//!   point, and a provisional step is accepted only after the step that confirms it. So the step
//!   is at most td / 2 (`Bank::max_step`), which keeps t - td behind both.
//! - A wave's corner (a point the chord of its neighbours misses by more than the tolerance)
//!   becomes a breakpoint td later. A delayed corner is stepped on, not interpolated across.
//! - Before the first recorded point, the line holds the state it started from.
//! - At DC and in AC the line is its exact two-port: `v1 = A v2 - B i2`, `i1 = C v2 - D i2`, with
//!   `A = D = cosh(y)`, `B = Z len sinhc(y)`, `C = Y len sinhc(y)`, `y^2 = len^2 Z Y`,
//!   `Z = r + jwl` and `Y = g + jwc`. Both are even in y, so no branch of the root is chosen, and
//!   any r, l, g, c work. At DC a lossless line is a short.
//! - A lossy line that is not distortionless runs at DC and in AC, and is a located error in a
//!   transient (`Bank::tran_unsupported`).
//! - A live rebuild carries each line's record by name (`Bank::state`), so the waves in flight
//!   arrive as they would have. A line whose z0 or td changed starts afresh from the present.

use crate::circuit::Tline;
use crate::num::C64;
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};
use std::collections::VecDeque;

/// Entries per instance: the KCL columns (p1, i1), (n1, i1), (p2, i2), (n2, i2); row i1 at
/// p1, n1, i1, p2, n2, i2; row i2 at p2, n2, i2, i1.
const SLOTS: usize = 14;

/// A line's constants.
#[derive(Clone, Copy, Debug)]
struct Line {
    z0: f64,
    td: f64,
    a: f64,
    /// The DC two-port: A, B, C, D.
    dc: [f64; 4],
    /// Lossless or distortionless: the transient is exact.
    exact: bool,
}

/// The two-port at angular frequency `w`: A (= D), B, C.
fn two_port(o: &Tline, w: f64) -> (C64, C64, C64) {
    let z = C64::new(o.r, w * o.l).scale(o.len);
    let y = C64::new(o.g, w * o.c).scale(o.len);
    let y2 = z * y;
    // cosh and sinhc of y as series in y^2 near 0, from the root's exponentials elsewhere
    let (cosh, sinhc) = if y2.norm() < 1e-4 {
        let t = y2;
        let cosh = C64::new(1.0, 0.0) + t.scale(0.5) + (t * t).scale(1.0 / 24.0);
        let sinhc = C64::new(1.0, 0.0) + t.scale(1.0 / 6.0) + (t * t).scale(1.0 / 120.0);
        (cosh, sinhc)
    } else {
        let root = C64::polar(y2.norm().sqrt(), 0.5 * y2.arg());
        let (e, f) = (root.exp(), (-root).exp());
        ((e + f).scale(0.5), (e - f).scale(0.5) / root)
    };
    (cosh, z * sinhc, y * sinhc)
}

/// The accepted waves `v1 + z0 i1` and `v2 + z0 i2` at their times, and the corners still ahead
/// of the delay.
#[derive(Clone, Debug, Default)]
struct Record {
    t: VecDeque<f64>,
    w: VecDeque<[f64; 2]>,
    corners: VecDeque<f64>,
}

impl Record {
    /// The waves at `t`, linear between recorded points, the first before them, the last after.
    fn at(&self, t: f64) -> [f64; 2] {
        let k = self.t.partition_point(|&s| s <= t);
        if k == 0 {
            return self.w.front().copied().unwrap_or([0.0; 2]);
        }
        if k == self.t.len() {
            return self.w[k - 1];
        }
        let (t0, t1) = (self.t[k - 1], self.t[k]);
        let f = (t - t0) / (t1 - t0);
        let (w0, w1) = (self.w[k - 1], self.w[k]);
        [w0[0] + f * (w1[0] - w0[0]), w0[1] + f * (w1[1] - w0[1])]
    }

    /// Records the waves accepted at `t`. A point at or before the newest (a step retaken) replaces
    /// what it follows. Marks the newest-but-one point as a corner when the slope changes there by
    /// more than half the larger slope and the chord of its neighbours misses it by more than
    /// `tol(w)` (a smooth wave the step follows changes slope by a small fraction per step), and
    /// forgets what lies more than `keep` behind `t`.
    fn push(&mut self, t: f64, w: [f64; 2], keep: f64, tol: impl Fn(f64) -> f64) {
        while self.t.back().is_some_and(|&s| s >= t) {
            self.t.pop_back();
            self.w.pop_back();
        }
        while self.corners.back().is_some_and(|&c| c >= t) {
            self.corners.pop_back();
        }
        self.t.push_back(t);
        self.w.push_back(w);
        let n = self.t.len();
        if n >= 2 {
            // before the first point the waves held still: a virtual neighbour of equal value
            let (tb, wb) = (self.t[n - 2], self.w[n - 2]);
            let (ta, wa) = if n >= 3 { (self.t[n - 3], self.w[n - 3]) } else { (f64::NEG_INFINITY, wb) };
            let corner = (0..2).any(|j| {
                let after = (w[j] - wb[j]) / (t - tb);
                let (before, miss) = if ta.is_finite() {
                    let f = (tb - ta) / (t - ta);
                    ((wb[j] - wa[j]) / (tb - ta), (wa[j] + f * (w[j] - wa[j]) - wb[j]).abs())
                } else {
                    (0.0, (w[j] - wb[j]).abs())
                };
                (after - before).abs() > 0.5 * after.abs().max(before.abs()) && miss > tol(wb[j].abs().max(w[j].abs()))
            });
            if corner && self.corners.back().is_none_or(|&c| c < tb) {
                self.corners.push_back(tb);
            }
        }
        // keep the last point at or before t - keep, which interpolation still reads
        let k = self.t.partition_point(|&s| s <= t - keep);
        for _ in 0..k.saturating_sub(1) {
            self.t.pop_front();
            self.w.pop_front();
        }
    }
}

#[derive(Clone)]
pub struct TBank {
    inst: Vec<Tline>,
    line: Vec<Line>,
    br: Vec<[Unk; 2]>,
    slots: Vec<u32>,
    rec: Vec<Record>,
}

impl TBank {
    /// The lines, their card values already resolved; a line without a positive l, c and len, or
    /// with a negative r or g, is a located error.
    pub fn new(inst: Vec<Tline>) -> Result<Self, String> {
        let mut line = Vec::new();
        for o in &inst {
            if ![o.l, o.c, o.len].iter().all(|v| v.is_finite() && *v > 0.0) || ![o.r, o.g].iter().all(|v| v.is_finite() && *v >= 0.0) {
                return Err(format!("{}: a line needs positive l, c and len and nonnegative r and g (r={} l={} g={} c={} len={})", o.name, o.r, o.l, o.g, o.c, o.len));
            }
            let z0 = (o.l / o.c).sqrt();
            let td = o.len * (o.l * o.c).sqrt();
            let (a, b, c) = two_port(o, 0.0);
            let exact = (o.r * o.c - o.g * o.l).abs() <= 1e-9 * (o.r * o.c).max(o.g * o.l);
            line.push(Line { z0, td, a: (-o.len * o.r / z0).exp(), dc: [a.re, b.re, c.re, a.re], exact });
        }
        let n = inst.len();
        Ok(TBank { inst, line, br: Vec::new(), slots: Vec::new(), rec: vec![Record::default(); n] })
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for k in insts {
            let o = &self.inst[k];
            let (line, s) = (&self.line[k], &self.slots[k * SLOTS..(k + 1) * SLOTS]);
            let [b1, b2] = self.br[k];
            let (i1, i2) = (x[b1 as usize], x[b2 as usize]);
            let v1 = x[o.p1 as usize] - x[o.n1 as usize];
            let v2 = x[o.p2 as usize] - x[o.n2 as usize];
            for (slot, d) in s[..4].iter().zip([1.0, -1.0, 1.0, -1.0]) {
                st.g[*slot as usize] += d;
            }
            st.i2(o.p1, o.n1, i1);
            st.i2(o.p2, o.n2, i2);
            st.g[s[4] as usize] += 1.0;
            st.g[s[5] as usize] -= 1.0;
            if cx.tran && line.exact {
                // each port: v - z0 i = the other port's wave launched td ago, attenuated
                let [w1, w2] = self.rec[k].at(cx.t - line.td);
                st.g[s[6] as usize] -= line.z0;
                st.f[b1 as usize] += v1 - line.z0 * i1 - line.a * w2;
                st.g[s[10] as usize] += 1.0;
                st.g[s[11] as usize] -= 1.0;
                st.g[s[12] as usize] -= line.z0;
                st.f[b2 as usize] += v2 - line.z0 * i2 - line.a * w1;
            } else {
                // the DC two-port: v1 - A v2 + B i2 = 0, i1 - C v2 + D i2 = 0
                let [a, b, c, d] = line.dc;
                st.g[s[7] as usize] -= a;
                st.g[s[8] as usize] += a;
                st.g[s[9] as usize] += b;
                st.f[b1 as usize] += v1 - a * v2 + b * i2;
                st.g[s[13] as usize] += 1.0;
                st.g[s[10] as usize] -= c;
                st.g[s[11] as usize] += c;
                st.g[s[12] as usize] += d;
                st.f[b2 as usize] += i1 - c * v2 + d * i2;
            }
        }
    }
}

impl Bank for TBank {
    fn kind(&self) -> &'static str {
        "O"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let o = &self.inst[i];
        out.extend_from_slice(&[o.p1, o.n1, o.p2, o.n2]);
        out.extend_from_slice(&self.br[i]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * SLOTS..(i + 1) * SLOTS]);
    }
    fn terminals(&self) -> usize {
        4
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let [b1, b2] = self.br[inst];
        let (i1, i2) = (x[b1 as usize], x[b2 as usize]);
        out[..4].copy_from_slice(&[i1, -i1, i2, -i2]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for o in &self.inst {
            let (b1, b2) = (topo.branch(&format!("{}#1", o.name)), topo.branch(&format!("{}#2", o.name)));
            self.br.push([b1, b2]);
            self.slots.extend_from_slice(&[
                topo.slot(o.p1, b1), topo.slot(o.n1, b1), topo.slot(o.p2, b2), topo.slot(o.n2, b2),
                topo.slot(b1, o.p1), topo.slot(b1, o.n1), topo.slot(b1, b1), topo.slot(b1, o.p2), topo.slot(b1, o.n2), topo.slot(b1, b2),
                topo.slot(b2, o.p2), topo.slot(b2, o.n2), topo.slot(b2, b2), topo.slot(b2, b1),
            ]);
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
    fn accept(&mut self, x: &[f64], cx: &EvalCx) {
        if !cx.tran {
            return;
        }
        for (k, o) in self.inst.iter().enumerate() {
            let line = &self.line[k];
            let [b1, b2] = self.br[k];
            let v1 = x[o.p1 as usize] - x[o.n1 as usize];
            let v2 = x[o.p2 as usize] - x[o.n2 as usize];
            let w = [v1 + line.z0 * x[b1 as usize], v2 + line.z0 * x[b2 as usize]];
            let (reltol, vntol) = (cx.reltol, cx.vntol);
            self.rec[k].push(cx.t, w, line.td, |scale| reltol * scale + vntol);
            let rec = &mut self.rec[k];
            while rec.corners.front().is_some_and(|&c| c + line.td < cx.t) {
                rec.corners.pop_front();
            }
        }
    }
    fn next_break(&self, t: f64) -> f64 {
        let mut next = f64::INFINITY;
        for (rec, line) in self.rec.iter().zip(&self.line) {
            // a corner reached within rounding of t is behind
            if let Some(&c) = rec.corners.iter().find(|&&c| c + line.td - t > 1e-13 * t.abs().max(line.td)) {
                next = next.min(c + line.td);
            }
        }
        next
    }
    fn max_step(&self) -> f64 {
        self.line.iter().map(|l| 0.5 * l.td).fold(f64::INFINITY, f64::min)
    }
    fn tran_unsupported(&self) -> Option<(usize, String)> {
        let k = self.line.iter().position(|l| !l.exact)?;
        let o = &self.inst[k];
        Some((k, format!("{}: a lossy line other than distortionless (r / l = g / c) is not supported in a transient yet (r={} l={} g={} c={})", o.name, o.r, o.l, o.g, o.c)))
    }
    /// `[z0, td, points, t.., w1.., w2.., corners..]`.
    fn state(&self, inst: usize) -> Vec<f64> {
        let (rec, line) = (&self.rec[inst], &self.line[inst]);
        let mut out = vec![line.z0, line.td, rec.t.len() as f64];
        out.extend(rec.t.iter());
        out.extend(rec.w.iter().map(|w| w[0]));
        out.extend(rec.w.iter().map(|w| w[1]));
        out.extend(rec.corners.iter());
        out
    }
    fn set_state(&mut self, inst: usize, s: &[f64]) {
        let line = &self.line[inst];
        // waves of another impedance or delay are not this line's
        let [z0, td, n, rest @ ..] = s else { return };
        let n = *n as usize;
        if *z0 != line.z0 || *td != line.td || rest.len() < 3 * n {
            return;
        }
        let (t, rest) = rest.split_at(n);
        let (w1, rest) = rest.split_at(n);
        let (w2, corners) = rest.split_at(n);
        self.rec[inst] = Record {
            t: t.iter().copied().collect(),
            w: w1.iter().zip(w2).map(|(a, b)| [*a, *b]).collect(),
            corners: corners.iter().copied().collect(),
        };
    }
    fn ac_stamp(&self, w: f64, vals: &mut [C64]) {
        for (k, o) in self.inst.iter().enumerate() {
            let s = &self.slots[k * SLOTS..(k + 1) * SLOTS];
            let [a0, b0, c0, d0] = self.line[k].dc;
            let (a, b, c) = two_port(o, w);
            // the operating point stamped the DC two-port; add what changes at w
            let (da, db, dc, dd) = (a - C64::new(a0, 0.0), b - C64::new(b0, 0.0), c - C64::new(c0, 0.0), a - C64::new(d0, 0.0));
            for (slot, v) in [(s[7], -da), (s[8], da), (s[9], db), (s[10], -dc), (s[11], dc), (s[12], dd)] {
                vals[slot as usize] += v;
            }
        }
    }
}
