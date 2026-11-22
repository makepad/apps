//! Kinematics: every link's frame at a joint pose, the Jacobian at a point, and the inverse:
//! the joint pose that puts the tool centre point at a position with its axis along a
//! direction (five constraints; the turn about the tool axis stays free for the solver and the
//! planner), or a full pose.
//!
//! The solver is damped least squares on a reach-normalised Jacobian. The damping grows as the
//! arm nears a singularity (its manipulability falls), and a null-space pull keeps joints off
//! their limits. Joint limits clamp every step.

use crate::arm::*;
use crate::math::*;

/// Every frame of an arm at one joint pose, in the cell frame.
#[derive(Clone, Copy, Debug)]
pub struct Frames {
    /// Each link's frame (after its joint turned).
    pub link: [Pose; DOF],
    /// Each joint's centre and axis.
    pub point: [V3; DOF],
    pub axis: [V3; DOF],
    pub flange: Pose,
    pub tcp: Pose,
}

pub fn frames(arm: &ArmModel, q: &Q) -> Frames {
    let mut t = arm.base;
    let mut link = [Pose::IDENTITY; DOF];
    let mut point = [ZERO; DOF];
    let mut axis = [ZERO; DOF];
    for i in 0..DOF {
        let j = &arm.joints[i];
        let jf = t.mul(&j.origin);
        point[i] = jf.t;
        axis[i] = jf.rotate(j.axis);
        t = jf.mul(&Pose::new(rot_axis(j.axis, q[i]), ZERO));
        link[i] = t;
    }
    let flange = t.mul(&arm.flange);
    let tcp = flange.mul(&arm.tool().tcp);
    Frames { link, point, axis, flange, tcp }
}

/// The geometric Jacobian at the point `at` (cell frame): rows 0..3 its velocity, 3..6 the
/// angular velocity, one column per joint.
pub fn jacobian(f: &Frames, at: V3) -> [[f64; DOF]; 6] {
    let mut j = [[0.0; DOF]; 6];
    for i in 0..DOF {
        let v = cross(f.axis[i], sub(at, f.point[i]));
        for r in 0..3 {
            j[r][i] = v[r];
            j[3 + r][i] = f.axis[i][r];
        }
    }
    j
}

/// How far `q` is from a singularity of the full tool pose: √det(J Jᵀ), the position rows over
/// the reach (the measure the IK damps by under `w0`). Zero at a singularity; a wrist one
/// (two wrist axes in line) takes it down with the sine of the angle between them.
pub fn manipulability(arm: &ArmModel, q: &Q) -> f64 {
    let f = frames(arm, q);
    let jac = jacobian(&f, f.tcp.t);
    let reach = arm.reach().max(1.0);
    let row = |r: usize| -> [f64; DOF] { std::array::from_fn(|k| if r < 3 { jac[r][k] / reach } else { jac[r][k] }) };
    let rows: [[f64; DOF]; 6] = std::array::from_fn(row);
    let mut a = [0.0; 36];
    for r in 0..6 {
        for c in 0..6 {
            a[r * 6 + c] = (0..DOF).map(|k| rows[r][k] * rows[c][k]).sum();
        }
    }
    if cholesky(6, &mut a) {
        (0..6).map(|i| a[i * 6 + i]).product()
    } else {
        0.0
    }
}

/// Where the tool should be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IkGoal {
    /// The tool centre point (cell frame, mm).
    pub p: V3,
    /// The tool axis (unit).
    pub z: V3,
    /// The tool's x axis too (a full pose); None leaves the turn about the tool axis free.
    pub x: Option<V3>,
}

#[derive(Clone, Copy, Debug)]
pub struct IkOpts {
    pub iters: usize,
    pub tol_mm: f64,
    pub tol_rad: f64,
    /// The largest damping (normalised units), reached at a singularity.
    pub damping: f64,
    /// Below this manipulability (normalised) damping sets in.
    pub w0: f64,
    /// Null-space pull toward mid-range per iteration, for joints in the outer part of
    /// their range.
    pub centre: f64,
}

impl Default for IkOpts {
    fn default() -> Self {
        IkOpts { iters: 80, tol_mm: 1e-4, tol_rad: 1e-6, damping: 0.06, w0: 0.02, centre: 0.05 }
    }
}

/// Why no pose was found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IkMiss {
    /// The best pose reached and how far it is from the goal.
    pub q: Q,
    pub err_mm: f64,
    pub err_rad: f64,
    /// A joint was held at a limit at the end.
    pub at_limit: bool,
}

/// The pose error at `f`: (position mm, rotation vector rad), the rotation without its part
/// about the tool axis when the goal leaves that free.
fn error(f: &Frames, goal: &IkGoal) -> (V3, V3) {
    let ep = sub(goal.p, f.tcp.t);
    let er = match goal.x {
        Some(x) => {
            let want = frame_z(goal.z, x);
            log_rot(&mm(&want, &transpose(&f.tcp.r)))
        }
        None => {
            let z = f.tcp.z();
            let c = cross(z, goal.z);
            let s = len(c);
            let angle = s.atan2(dot(z, goal.z));
            if s < 1e-12 {
                if dot(z, goal.z) > 0.0 {
                    ZERO
                } else {
                    scale(f.tcp.x(), std::f64::consts::PI)
                }
            } else {
                scale(c, angle / s)
            }
        }
    };
    (ep, er)
}

/// The joint pose for `goal`, starting from `seed` (the nearest solution to it is found).
pub fn ik(arm: &ArmModel, goal: &IkGoal, seed: &Q, opts: &IkOpts) -> Result<Q, IkMiss> {
    let (lo, hi) = arm.limits();
    let reach = arm.reach().max(1.0);
    let mut q = *seed;
    for i in 0..DOF {
        q[i] = q[i].clamp(lo[i], hi[i]);
    }
    let mut best = (f64::MAX, q, 0.0, 0.0);
    for _ in 0..opts.iters {
        let f = frames(arm, &q);
        let (ep, er) = error(&f, goal);
        let (emm, erad) = (len(ep), len(er));
        let score = emm / reach + erad;
        if score < best.0 {
            best = (score, q, emm, erad);
        }
        if emm <= opts.tol_mm && erad <= opts.tol_rad {
            return Ok(q);
        }
        // Close in, Gauss-Newton: no damping or limit pull to hold the last micrometres off.
        let near = emm < 0.5 && erad < 1e-3;
        let jac = jacobian(&f, f.tcp.t);
        // The task rows (reach-normalised): three of position, then three of rotation or the
        // two across the tool axis.
        let (n, rows, err) = task_rows(&jac, &f, goal, ep, er, reach);
        // A = J Jᵀ (n x n), its manipulability and the damping it asks for.
        let mut a = [0.0; 36];
        for r in 0..n {
            for c in 0..n {
                a[r * n + c] = (0..DOF).map(|k| rows[r][k] * rows[c][k]).sum();
            }
        }
        let mut fac = a;
        let w = if cholesky(n, &mut fac[..n * n]) { (0..n).map(|i| fac[i * n + i]).product::<f64>() } else { 0.0 };
        let lambda2 = if w < opts.w0 && !near { opts.damping * opts.damping * (1.0 - (w / opts.w0).powi(2)) } else { 0.0 } + 1e-12;
        for i in 0..n {
            a[i * n + i] += lambda2;
        }
        let mut y = err;
        if !cholesky_solve(n, &mut a[..n * n], &mut y[..n]) {
            break;
        }
        let mut dq = [0.0; DOF];
        for k in 0..DOF {
            dq[k] = (0..n).map(|r| rows[r][k] * y[r]).sum();
        }
        // Null space: pull joints in the outer fifth of their range toward the middle,
        // without moving the task (v - J⁺ J v).
        let mut v = [0.0; DOF];
        for i in 0..DOF {
            let mid = 0.5 * (lo[i] + hi[i]);
            let half = 0.5 * (hi[i] - lo[i]);
            if half > 0.0 {
                let u = (q[i] - mid) / half;
                if u.abs() > 0.6 {
                    v[i] = -opts.centre * (u - 0.6 * u.signum()) * half;
                }
            }
        }
        if !near && v.iter().any(|x| *x != 0.0) {
            let mut jv = [0.0; 6];
            for r in 0..n {
                jv[r] = (0..DOF).map(|k| rows[r][k] * v[k]).sum();
            }
            // a still holds the factor of J Jᵀ + λ².
            cholesky_back(n, &a[..n * n], &mut jv[..n]);
            for k in 0..DOF {
                dq[k] += v[k] - (0..n).map(|r| rows[r][k] * jv[r]).sum::<f64>();
            }
        }
        let big = dq.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        let s = if big > 0.3 { 0.3 / big } else { 1.0 };
        for i in 0..DOF {
            q[i] = (q[i] + dq[i] * s).clamp(lo[i], hi[i]);
        }
    }
    let f = frames(arm, &q);
    let (ep, er) = error(&f, goal);
    if len(ep) <= opts.tol_mm && len(er) <= opts.tol_rad {
        return Ok(q);
    }
    let (_, bq, emm, erad) = best;
    let at_limit = (0..DOF).any(|i| bq[i] <= lo[i] + 1e-9 || bq[i] >= hi[i] - 1e-9);
    Err(IkMiss { q: bq, err_mm: emm, err_rad: erad, at_limit })
}

/// The solver's task rows and their errors, reach-normalised so position and rotation weigh
/// alike: (row count, rows, errors).
#[allow(clippy::type_complexity)]
fn task_rows(jac: &[[f64; DOF]; 6], f: &Frames, goal: &IkGoal, ep: V3, er: V3, reach: f64) -> (usize, [[f64; DOF]; 6], [f64; 6]) {
    let mut rows = [[0.0; DOF]; 6];
    let mut err = [0.0; 6];
    for r in 0..3 {
        rows[r] = std::array::from_fn(|k| jac[r][k] / reach);
        err[r] = ep[r] / reach;
    }
    if goal.x.is_some() {
        for r in 0..3 {
            rows[3 + r] = jac[3 + r];
            err[3 + r] = er[r];
        }
        return (6, rows, err);
    }
    let (x, y) = (f.tcp.x(), col(&f.tcp.r, 1));
    for (r, d) in [x, y].into_iter().enumerate() {
        rows[3 + r] = std::array::from_fn(|k| d[0] * jac[3][k] + d[1] * jac[4][k] + d[2] * jac[5][k]);
        err[3 + r] = dot(d, er);
    }
    (5, rows, err)
}
