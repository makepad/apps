//! The per-device checks of E3 2.2, for any bank: analytic Jacobians against central differences,
//! charge neutrality and state-free charges, and continuity of currents, charges and their
//! derivatives along a bias path.

use fab_elec_sim::stamp::{EvalCx, UnkKind};
use fab_elec_sim::{SimCircuit, Solver};

pub struct Lcg(pub u64);
impl Lcg {
    pub fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    /// Uniform in [a, b).
    pub fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.next()
    }
}

/// An exact evaluation context: no limiting, no initial junction guess.
pub fn exact_cx(s: &Solver) -> EvalCx {
    let mut cx = s.base_cx();
    cx.limit = false;
    cx
}

/// (f, q) at `x`.
pub fn fq(s: &mut Solver, x: &[f64]) -> (Vec<f64>, Vec<f64>) {
    s.x.copy_from_slice(x);
    let cx = exact_cx(s);
    s.load(&cx);
    (s.f.clone(), s.q.clone())
}

/// (f, q, G, C) at `x`, the matrices dense by unknown.
pub fn eval(s: &mut Solver, x: &[f64]) -> (Vec<f64>, Vec<f64>, Vec<Vec<f64>>, Vec<Vec<f64>>) {
    fq(s, x);
    let n1 = s.n + 1;
    let mut g = vec![vec![0.0; n1]; n1];
    let mut c = vec![vec![0.0; n1]; n1];
    for k in 1..s.g.len() {
        let (r, col) = (s.slot_row[k] as usize, s.slot_col[k] as usize);
        g[r][col] += s.g[k];
        c[r][col] += s.c[k];
    }
    (s.f.clone(), s.q.clone(), g, c)
}

pub struct JacReport {
    /// Worst relative error of G and of C, with the bias and (row, col) where it occurred.
    pub g: (f64, Vec<f64>, (usize, usize)),
    pub c: (f64, Vec<f64>, (usize, usize)),
    /// Largest |sum of the node rows' charges| (ground's included) relative to the largest charge.
    pub neutral: f64,
}

/// Analytic G = df/dx and C = dq/dx against central differences at `points` biases drawn by
/// `bias` (which fills every unknown, internal nodes included; index 0 stays 0).
pub fn jacobian(c: &SimCircuit, points: usize, rng: &mut Lcg, bias: &mut dyn FnMut(&mut Lcg, &Solver, &mut [f64])) -> JacReport {
    let mut s = Solver::new(c).unwrap_or_else(|d| panic!("{d}"));
    let n1 = s.n + 1;
    let mut rep = JacReport { g: (0.0, Vec::new(), (0, 0)), c: (0.0, Vec::new(), (0, 0)), neutral: 0.0 };
    let mut x = vec![0.0; n1];
    for _ in 0..points {
        x.iter_mut().for_each(|v| *v = 0.0);
        bias(rng, &s, &mut x);
        x[0] = 0.0;
        let (f0, q0, g, cm) = eval(&mut s, &x);
        // node rows only (ground's included): a branch row's q is a flux, not a charge
        let node_q = |i: &usize| *i == 0 || s.unknowns[*i].kind == UnkKind::Voltage;
        let qsum: f64 = (0..n1).filter(node_q).map(|i| q0[i]).sum();
        let qmax = (0..n1).filter(node_q).fold(0.0f64, |m, i| m.max(q0[i].abs()));
        if qmax > 0.0 {
            rep.neutral = rep.neutral.max(qsum.abs() / qmax);
        }
        for j in 1..n1 {
            // central differences at d and d/2, Richardson-extrapolated: the truncation error is
            // O(d^4), so a smooth corner a few tens of microvolts wide (a regulator's current
            // clamp) is resolved; the extrapolation weights raise the rounding floor (below)
            let d = 1e-6 * x[j].abs().max(1e-3);
            let mut diff = |h: f64| {
                let mut xp = x.clone();
                xp[j] += h;
                let mut xm = x.clone();
                xm[j] -= h;
                let (fp, qp) = fq(&mut s, &xp);
                let (fm, qm) = fq(&mut s, &xm);
                let df: Vec<f64> = (0..n1).map(|i| (fp[i] - fm[i]) / (2.0 * h)).collect();
                let dq: Vec<f64> = (0..n1).map(|i| (qp[i] - qm[i]) / (2.0 * h)).collect();
                (df, dq)
            };
            let (df1, dq1) = diff(d);
            let (df2, dq2) = diff(0.5 * d);
            for i in 1..n1 {
                // the row's scale: its largest analytic entry, so a tiny entry beside a large one
                // is judged against the row; and the difference quotient's own rounding, from the
                // size of the row's terms (an exponentially small junction conductance in a row
                // that carries amperes through a series resistor is below what it can resolve;
                // a model's own evaluation is good to some tens of ulps of those terms)
                let gs = g[i].iter().fold(0.0f64, |m, v| m.max(v.abs()));
                let cs = cm[i].iter().fold(0.0f64, |m, v| m.max(v.abs()));
                let fterms = f0[i].abs() + (0..n1).map(|k| (g[i][k] * x[k]).abs()).sum::<f64>();
                let qterms = q0[i].abs() + (0..n1).map(|k| (cm[i][k] * x[k]).abs()).sum::<f64>();
                // (4 * noise(d/2) + noise(d)) / 3 = 3 * noise(d)
                let (gnoise, cnoise) = (192.0 * f64::EPSILON * fterms / d, 192.0 * f64::EPSILON * qterms / d);
                let ng = (4.0 * df2[i] - df1[i]) / 3.0;
                let nc = (4.0 * dq2[i] - dq1[i]) / 3.0;
                let eg = ((ng - g[i][j]).abs() - gnoise).max(0.0) / (ng.abs().max(g[i][j].abs()) + 1e-6 * gs + 1e-300);
                let ec = ((nc - cm[i][j]).abs() - cnoise).max(0.0) / (nc.abs().max(cm[i][j].abs()) + 1e-6 * cs + 1e-300);
                if eg > rep.g.0 {
                    rep.g = (eg, x.clone(), (i, j));
                }
                if ec > rep.c.0 {
                    rep.c = (ec, x.clone(), (i, j));
                }
            }
        }
    }
    rep
}

/// Charges are a function of the bias alone: a closed bias cycle returns the same charges, bit
/// for bit. Returns the largest change.
pub fn charge_cycle(c: &SimCircuit, rng: &mut Lcg, bias: &mut dyn FnMut(&mut Lcg, &Solver, &mut [f64])) -> f64 {
    let mut s = Solver::new(c).unwrap_or_else(|d| panic!("{d}"));
    let n1 = s.n + 1;
    let mut x0 = vec![0.0; n1];
    bias(rng, &s, &mut x0);
    x0[0] = 0.0;
    let (_, q0) = fq(&mut s, &x0);
    let mut x = vec![0.0; n1];
    for _ in 0..20 {
        x.iter_mut().for_each(|v| *v = 0.0);
        bias(rng, &s, &mut x);
        x[0] = 0.0;
        fq(&mut s, &x);
    }
    let (_, q1) = fq(&mut s, &x0);
    q0.iter().zip(&q1).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max)
}

/// A point on a continuity path: values and their slopes along the path.
struct PathPoint {
    t: f64,
    f: Vec<f64>,
    q: Vec<f64>,
    df: Vec<f64>,
    dq: Vec<f64>,
}

fn path_point(s: &mut Solver, t: f64, path: &dyn Fn(f64, &Solver, &mut [f64], &mut [f64])) -> PathPoint {
    let n1 = s.n + 1;
    let mut x = vec![0.0; n1];
    let mut dx = vec![0.0; n1];
    path(t, s, &mut x, &mut dx);
    x[0] = 0.0;
    dx[0] = 0.0;
    let (f, q, g, cm) = eval(s, &x);
    let df = (0..n1).map(|i| (0..n1).map(|j| g[i][j] * dx[j]).sum()).collect();
    let dq = (0..n1).map(|i| (0..n1).map(|j| cm[i][j] * dx[j]).sum()).collect();
    PathPoint { t, f, q, df, dq }
}

/// The worst row's trapezoid defect between two path points: the change of a value against the
/// mean of its slopes, relative to the slopes' own contribution.
fn defect(a: &PathPoint, b: &PathPoint) -> f64 {
    let h = b.t - a.t;
    let mut worst = 0.0f64;
    for i in 1..a.f.len() {
        for (va, vb, da, db) in [(a.f[i], b.f[i], a.df[i], b.df[i]), (a.q[i], b.q[i], a.dq[i], b.dq[i])] {
            let scale = 0.5 * h * (da.abs() + db.abs()) + 1e-12 * va.abs().max(vb.abs()) + 1e-300;
            worst = worst.max(((vb - va) - 0.5 * h * (da + db)).abs() / scale);
        }
    }
    worst
}

/// Continuity of currents, charges and their slopes along the path `x(t)` (the closure fills x
/// and dx/dt), t in [t0, t1] on `steps` points. A step whose trapezoid defect exceeds `bar` is
/// bisected toward its worse half down to rounding, which pins a boundary to a point; there the
/// one-sided values must agree (a jump stays a jump at any width) and so must the one-sided
/// slopes, relative to the slopes at the step's ends (a smooth or C1 but curved stretch converges,
/// a kink does not). Returns the worst slope mismatch, or a value jump's ratio (above 4), and where.
pub fn continuity(c: &SimCircuit, t0: f64, t1: f64, steps: usize, bar: f64, path: &dyn Fn(f64, &Solver, &mut [f64], &mut [f64])) -> (f64, f64) {
    let mut s = Solver::new(c).unwrap_or_else(|d| panic!("{d}"));
    let h = (t1 - t0) / steps as f64;
    let mut worst = (0.0f64, t0);
    let mut prev = path_point(&mut s, t0, path);
    for k in 1..=steps {
        let next = path_point(&mut s, t0 + h * k as f64, path);
        if defect(&prev, &next) > bar {
            let (ends_f, ends_q): (Vec<f64>, Vec<f64>) =
                (0..prev.f.len()).map(|i| (prev.df[i].abs() + next.df[i].abs(), prev.dq[i].abs() + next.dq[i].abs())).unzip();
            let (mut l, mut r) = (path_point(&mut s, prev.t, path), path_point(&mut s, next.t, path));
            while r.t - l.t > 4.0 * f64::EPSILON * l.t.abs().max(r.t.abs()).max(1e-3) {
                let m = path_point(&mut s, 0.5 * (l.t + r.t), path);
                if defect(&l, &m) >= defect(&m, &r) {
                    r = m;
                } else {
                    l = m;
                }
            }
            let w = r.t - l.t;
            for i in 1..l.f.len() {
                for (va, vb, da, db, ends) in [(l.f[i], r.f[i], l.df[i], r.df[i], ends_f[i]), (l.q[i], r.q[i], l.dq[i], r.dq[i], ends_q[i])] {
                    let value = (vb - va).abs() / (1e-12 * va.abs().max(vb.abs()) + w * (da.abs() + db.abs() + ends) + 1e-300);
                    let slope = (db - da).abs() / (da.abs() + db.abs() + 1e-6 * ends + 1e-300);
                    // a continuous value moves by about its slope times the width (a ratio near
                    // or below 1); a jump is a value off by more than a few of those
                    let e = if value > 4.0 { value } else { slope };
                    if e > worst.0 {
                        worst = (e, 0.5 * (l.t + r.t));
                    }
                }
            }
        }
        prev = next;
    }
    worst
}
