//! Integration formulas, local truncation error and the predictor (E3 1.7).
//!
//! The state integrated is the row vector `q` (charges in node rows, fluxes in branch rows). Each
//! formula is `dq/dt (t_n) ~ a0 q_n + hist`, with `hist` built from the accepted past points.
//! The error estimate is the divided difference of `q` over the new point and `order + 1` past
//! points, which is the predictor-corrector difference scaled by the step products.

use crate::circuit::Method;
use std::collections::VecDeque;

/// BDF error constants, orders 1..6.
const BDF_ERR: [f64; 6] = [1.0 / 2.0, 2.0 / 9.0, 3.0 / 22.0, 12.0 / 125.0, 10.0 / 137.0, 20.0 / 343.0];
const TRAP_ERR: f64 = 1.0 / 12.0;

/// An accepted time point.
#[derive(Clone, Debug)]
pub struct Point {
    pub t: f64,
    pub x: Vec<f64>,
    pub q: Vec<f64>,
    pub qdot: Vec<f64>,
    /// The integrator's own derivative of `x` at this point: the same formula and coefficients
    /// that give `qdot` from `q`, applied to `x` (so a linear charge's rate is `C xdot` exactly).
    pub xdot: Vec<f64>,
}

/// The accepted points since the last restart (a breakpoint), newest first.
#[derive(Clone)]
pub struct History {
    pub pts: VecDeque<Point>,
    spare: Vec<Point>,
    cap: usize,
}

impl History {
    pub fn new(cap: usize) -> History {
        History { pts: VecDeque::new(), spare: Vec::new(), cap }
    }
    pub fn newest(&self) -> &Point {
        &self.pts[0]
    }
    /// Drops all but the newest point (after a breakpoint: older points straddle a corner).
    pub fn restart(&mut self) {
        while self.pts.len() > 1 {
            let p = self.pts.pop_back().unwrap();
            self.spare.push(p);
        }
    }
    /// The oldest point once the history is full, else a spare one (or a new one).
    fn recycled(&mut self) -> Point {
        if self.pts.len() >= self.cap {
            self.pts.pop_back().unwrap()
        } else {
            self.spare.pop().unwrap_or_else(|| Point { t: 0.0, x: Vec::new(), q: Vec::new(), qdot: Vec::new(), xdot: Vec::new() })
        }
    }

    /// `push`, taking `q`, `qdot` and `xdot` by exchange: each comes back holding an older point's
    /// values (zeros if there was none), as long as it was, for the caller to overwrite.
    pub fn push_taking(&mut self, t: f64, x: &[f64], q: &mut Vec<f64>, qdot: &mut Vec<f64>, xdot: &mut Vec<f64>) {
        let mut p = self.recycled();
        p.t = t;
        p.x.clear();
        p.x.extend_from_slice(x);
        for (mine, theirs) in [(&mut p.q, q), (&mut p.qdot, qdot), (&mut p.xdot, xdot)] {
            mine.resize(theirs.len(), 0.0);
            std::mem::swap(mine, theirs);
        }
        self.pts.push_front(p);
    }

    pub fn push(&mut self, t: f64, x: &[f64], q: &[f64], qdot: &[f64], xdot: &[f64]) {
        let mut p = self.recycled();
        p.t = t;
        p.x.clear();
        p.x.extend_from_slice(x);
        p.q.clear();
        p.q.extend_from_slice(q);
        p.qdot.clear();
        p.qdot.extend_from_slice(qdot);
        p.xdot.clear();
        p.xdot.extend_from_slice(xdot);
        self.pts.push_front(p);
    }
}

/// The order a method uses with `past` points available: at most `past - 1`, so every step
/// but the first after a restart has the `order + 1` past points its error estimate needs.
pub fn order_for(method: Method, max_order: u8, past: usize) -> usize {
    let target = match method {
        Method::Euler => 1,
        Method::Trap => 2,
        Method::Gear => max_order.clamp(1, 6) as usize,
    };
    target.min(past.saturating_sub(1)).max(1)
}

/// `a0` and `hist` (by row) for a step to `t` at `order`: the charge rate at `t` is
/// `a0 * q(t) + hist`.
pub fn coefficients(method: Method, order: usize, t: f64, h: &History, hist: &mut [f64]) -> f64 {
    coefficients_of(method, order, t, h, hist, |p| (p.q.as_slice(), p.qdot.as_slice()))
}

/// How a formula builds `hist` from the past points.
enum Form {
    /// From the newest point: `-a0 v0`.
    Last,
    /// From the newest point and its recorded rate: `-a0 v0 - vdot0`.
    Trap,
    /// From the newest `k` points: `sum a[j] v_j`.
    Bdf { k: usize, a: [f64; 7] },
}

/// `a0` and the form of `hist` for a step to `t` at `order`.
fn form(method: Method, order: usize, t: f64, h: &History) -> (f64, Form) {
    let step = t - h.pts[0].t;
    match (method, order) {
        (_, 1) => (1.0 / step, Form::Last),
        (Method::Trap, _) => (2.0 / step, Form::Trap),
        _ => {
            // variable-step BDF: the derivative at t of the interpolant through t, t_{n-1}..t_{n-k}
            let k = order;
            let mut ts = [0.0f64; 7];
            ts[0] = t;
            for j in 1..=k {
                ts[j] = h.pts[j - 1].t;
            }
            let a0: f64 = (1..=k).map(|m| 1.0 / (ts[0] - ts[m])).sum();
            let mut a = [0.0f64; 7];
            for j in 1..=k {
                let mut num = 1.0;
                let mut den = 1.0;
                for m in 1..=k {
                    if m != j {
                        num *= ts[0] - ts[m];
                    }
                }
                for m in 0..=k {
                    if m != j {
                        den *= ts[j] - ts[m];
                    }
                }
                a[j - 1] = num / den;
            }
            (a0, Form::Bdf { k, a })
        }
    }
}

/// The newest `m` points' recorded quantity.
fn newest<'a>(h: &'a History, m: usize, of: impl Fn(&'a Point) -> &'a [f64]) -> [&'a [f64]; 8] {
    let mut v: [&[f64]; 8] = [&[]; 8];
    for (j, v) in v.iter_mut().enumerate().take(m) {
        *v = of(&h.pts[j]);
    }
    v
}

/// `out[i] = f(i, 0 + w[0] v[0][i] + w[1] v[1][i] + ...)` in one pass over the rows, the sum taken
/// in that order: bit for bit what a pass per point accumulating into a zeroed `out` gives.
#[inline(always)]
fn weighted(out: &mut [f64], w: &[f64], v: &[&[f64]], f: impl Fn(usize, f64) -> f64) {
    let n = out.len();
    match *v {
        [v0] => {
            let v0 = &v0[..n];
            for i in 0..n {
                out[i] = f(i, 0.0 + w[0] * v0[i]);
            }
        }
        [v0, v1] => {
            let (v0, v1) = (&v0[..n], &v1[..n]);
            for i in 0..n {
                out[i] = f(i, 0.0 + w[0] * v0[i] + w[1] * v1[i]);
            }
        }
        [v0, v1, v2] => {
            let (v0, v1, v2) = (&v0[..n], &v1[..n], &v2[..n]);
            for i in 0..n {
                out[i] = f(i, 0.0 + w[0] * v0[i] + w[1] * v1[i] + w[2] * v2[i]);
            }
        }
        _ => {
            for i in 0..n {
                let mut sum = 0.0;
                for (j, v) in v.iter().enumerate() {
                    sum += w[j] * v[i];
                }
                out[i] = f(i, sum);
            }
        }
    }
}

/// `coefficients` for another recorded quantity and its recorded rate (`x` and `xdot`).
pub fn coefficients_of(method: Method, order: usize, t: f64, h: &History, hist: &mut [f64], of: impl Fn(&Point) -> (&[f64], &[f64])) -> f64 {
    let (a0, form) = form(method, order, t, h);
    match form {
        Form::Last => {
            for (o, &q) in hist.iter_mut().zip(of(&h.pts[0]).0) {
                *o = -a0 * q;
            }
        }
        Form::Trap => {
            let (q0, qd0) = of(&h.pts[0]);
            for ((o, &q), &qd) in hist.iter_mut().zip(q0).zip(qd0) {
                *o = -a0 * q - qd;
            }
        }
        Form::Bdf { k, a } => {
            let v = newest(h, k, |p| of(p).0);
            weighted(hist, &a[..k], &v[..k], |_, sum| sum);
        }
    }
    a0
}

/// The formula's rate at `t` of a quantity `v` recorded with the points (with its recorded rate,
/// which the trapezoid reads): `a0 * v[i] + hist[i]` with `hist` from `coefficients_of`, bit for
/// bit, in one pass.
pub fn rate_of(method: Method, order: usize, t: f64, h: &History, v: &[f64], out: &mut [f64], of: impl Fn(&Point) -> (&[f64], &[f64])) {
    let n = out.len();
    let v = &v[..n];
    let (a0, form) = form(method, order, t, h);
    match form {
        Form::Last => {
            let v0 = &of(&h.pts[0]).0[..n];
            for i in 0..n {
                out[i] = a0 * v[i] + -a0 * v0[i];
            }
        }
        Form::Trap => {
            let (v0, vd0) = of(&h.pts[0]);
            let (v0, vd0) = (&v0[..n], &vd0[..n]);
            for i in 0..n {
                out[i] = a0 * v[i] + (-a0 * v0[i] - vd0[i]);
            }
        }
        Form::Bdf { k, a } => {
            let past = newest(h, k, |p| of(p).0);
            weighted(out, &a[..k], &past[..k], |i, sum| a0 * v[i] + sum);
        }
    }
}

/// Lagrange weights at `t` of the polynomial through the newest `m` past points.
pub fn extrapolation_weights(t: f64, h: &History, m: usize, w: &mut [f64]) {
    for j in 0..m {
        let mut v = 1.0;
        for i in 0..m {
            if i != j {
                v *= (t - h.pts[i].t) / (h.pts[j].t - h.pts[i].t);
            }
        }
        w[j] = v;
    }
}

/// Predicts `x` at `t` from the past points (degree `order`, or less with fewer points).
pub fn predict(t: f64, h: &History, order: usize, out: &mut [f64]) {
    let m = (order + 1).min(h.pts.len());
    let mut w = [0.0f64; 8];
    extrapolation_weights(t, h, m, &mut w);
    let x = newest(h, m, |p| &p.x);
    weighted(out, &w[..m], &x[..m], |_, sum| sum);
}

/// The largest ratio of truncation error to tolerance over the rows, for the step to `t` with
/// charges `q` (and their derivative `qdot`), and its row. `None` when the history is too short to
/// tell. `cap(i)` caps row `i`'s charge-level allowance. With `first_over` the scan stops at the
/// first row over tolerance, for a caller that any ratio above 1 decides the same.
#[allow(clippy::too_many_arguments)]
pub fn lte_ratio(method: Method, order: usize, t: f64, q: &[f64], qdot: &[f64], h: &History, current_row: &[bool], tol: &LteTol, cap: Option<impl Fn(usize) -> f64>, first_over: bool) -> Option<(f64, usize)> {
    let lte = Lte::new(method, order, t, h)?;
    Some(error_ratio(|i| lte.error(q, i), t, q, qdot, h, current_row, tol, cap, first_over))
}

/// Writes the signed per-row charge error into `out` (row 0 is zero). `None` when the
/// history is too short to tell; in that case `out` is unchanged.
pub fn lte_charge_error(method: Method, order: usize, t: f64, q: &[f64], h: &History, out: &mut [f64]) -> Option<()> {
    let lte = Lte::new(method, order, t, h)?;
    debug_assert_eq!(out.len(), q.len());
    out[0] = 0.0;
    for i in 1..q.len() {
        out[i] = lte.error(q, i);
    }
    Some(())
}

/// The largest ratio of a charge-error vector to the step's row tolerances, and its row.
#[allow(clippy::too_many_arguments)]
pub fn charge_error_ratio(e: &[f64], t: f64, q: &[f64], qdot: &[f64], h: &History, current_row: &[bool], tol: &LteTol, cap: Option<impl Fn(usize) -> f64>) -> (f64, usize) {
    debug_assert_eq!(e.len(), q.len());
    error_ratio(|i| e[i], t, q, qdot, h, current_row, tol, cap, false)
}

/// The error estimate's predictor through the newest `order + 1` points, and the scale that turns
/// a row's distance from it into its truncation error.
struct Lte<'a> {
    w: [f64; 8],
    q: [&'a [f64]; 8],
    m: usize,
    scale: f64,
}

impl<'a> Lte<'a> {
    fn new(method: Method, order: usize, t: f64, h: &'a History) -> Option<Lte<'a>> {
        let m = order + 1;
        if h.pts.len() < m {
            return None;
        }
        let mut w = [0.0f64; 8];
        extrapolation_weights(t, h, m, &mut w);
        let mut prod = 1.0;
        let mut fact = 1.0;
        for j in 0..m {
            prod *= t - h.pts[j].t;
            fact *= (j + 1) as f64;
        }
        let step = t - h.pts[0].t;
        let err_c = match method {
            Method::Trap if order == 2 => TRAP_ERR,
            _ => BDF_ERR[order - 1],
        };
        let scale = err_c * fact * step.powi(m as i32) / prod;
        Some(Lte { w, q: newest(h, m, |p| &p.q), m, scale })
    }

    /// Row `i`'s signed charge error with the new charges `q` (the prediction summed point by
    /// point from zero).
    #[inline(always)]
    fn error(&self, q: &[f64], i: usize) -> f64 {
        let (w, p) = (&self.w, &self.q);
        let pred = match self.m {
            2 => 0.0 + w[0] * p[0][i] + w[1] * p[1][i],
            3 => 0.0 + w[0] * p[0][i] + w[1] * p[1][i] + w[2] * p[2][i],
            m => {
                let mut pred = 0.0;
                for j in 0..m {
                    pred += w[j] * p[j][i];
                }
                pred
            }
        };
        self.scale * (q[i] - pred)
    }
}

/// One row's truncation-error allowance for a step of length `step` between two points.
pub fn row_tolerance(q_new: f64, q_old: f64, qdot_new: f64, qdot_old: f64, step: f64, current_row: bool, tol: &LteTol) -> f64 {
    let abs_rate = if current_row { tol.vntol } else { tol.abstol };
    let tq = (tol.reltol * q_new.abs().max(q_old.abs())).max(tol.chgtol);
    let tr = (tol.reltol * qdot_new.abs().max(qdot_old.abs()) + abs_rate) * step;
    tol.trtol * tq.max(tr)
}

/// `row_tolerance` with its charge-level term capped at `cap` before the charge floor.
#[allow(clippy::too_many_arguments)]
fn capped_row_tolerance(q_new: f64, q_old: f64, qdot_new: f64, qdot_old: f64, step: f64, current_row: bool, tol: &LteTol, cap: f64) -> f64 {
    let abs_rate = if current_row { tol.vntol } else { tol.abstol };
    let tq = (tol.reltol * q_new.abs().max(q_old.abs())).min(cap).max(tol.chgtol);
    let tr = (tol.reltol * qdot_new.abs().max(qdot_old.abs()) + abs_rate) * step;
    tol.trtol * tq.max(tr)
}

#[allow(clippy::too_many_arguments)]
fn error_ratio(error: impl Fn(usize) -> f64, t: f64, q: &[f64], qdot: &[f64], h: &History, current_row: &[bool], tol: &LteTol, cap: Option<impl Fn(usize) -> f64>, first_over: bool) -> (f64, usize) {
    let p0 = &h.pts[0];
    let step = t - p0.t;
    let mut worst = 0.0f64;
    let mut at = 0;
    for i in 1..q.len() {
        let lte = error(i).abs();
        if lte == 0.0 {
            continue;
        }
        let allow = match &cap {
            Some(cap) => capped_row_tolerance(q[i], p0.q[i], qdot[i], p0.qdot[i], step, current_row[i], tol, cap(i)),
            None => row_tolerance(q[i], p0.q[i], qdot[i], p0.qdot[i], step, current_row[i], tol),
        };
        let r = lte / allow;
        if r > worst {
            worst = r;
            at = i;
            if first_over && r > 1.0 {
                break;
            }
        }
    }
    (worst, at)
}

#[derive(Clone, Copy, Debug)]
pub struct LteTol {
    pub reltol: f64,
    pub abstol: f64,
    pub vntol: f64,
    pub chgtol: f64,
    pub trtol: f64,
}
