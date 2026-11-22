//! Rigid-body dynamics of a 6-axis arm with elastic joints.
//!
//! - [`rnea`]: recursive Newton-Euler inverse dynamics (the joint torques for a motion, with
//!   gravity and a wrench the environment puts on the tool), in the cell frame;
//! - [`mass_matrix`]: the joint-space inertia, column by column from RNEA;
//! - [`step`]: one fixed step of the elastic-joint model. Each joint is a motor (its rotor
//!   reflected through the gear, angle `th`) and a link (angle `q`) joined by a spring and a
//!   damper: `M(q) q̈ + b(q, q̇) = K (th − q) + D (thd − q̇)` and
//!   `B thdd + K (th − q) + D (thd − q̇) + friction(thd) = τ_motor`. The motor torque is the
//!   drive's servo law on the motor angle: `kp (th_cmd − th) + kd (thd_cmd − thd) + τ_ff`,
//!   limited to the drive's peak. Integration is semi-implicit Euler (symplectic), with
//!   substeps when the stiffest joint mode would be under-resolved; the same inputs give the
//!   same bits on every run.

use crate::arm::*;
use crate::kin::{frames, Frames};
use crate::math::*;

/// A wrench the environment puts on the tool: a force (at the TCP) and a moment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wrench {
    pub force: V3,
    pub moment: V3,
}

impl Wrench {
    pub const NONE: Wrench = Wrench { force: ZERO, moment: ZERO };

    pub fn plus(&self, o: &Wrench) -> Wrench {
        Wrench { force: add(self.force, o.force), moment: add(self.moment, o.moment) }
    }
}

/// The joint torques that move the arm with `qd`, `qdd` against `gravity` (a vector, mm/s²)
/// while the environment pushes the tool with `wrench` at the TCP.
pub fn rnea(arm: &ArmModel, f: &Frames, qd: &Q, qdd: &Q, gravity: V3, wrench: &Wrench) -> Q {
    let mut w = ZERO;
    let mut wd = ZERO;
    // The acceleration of the previous joint centre (gravity enters as the base accelerating up).
    let mut a = scale(gravity, -1.0);
    let mut p_prev = f.point[0];
    let mut force = [ZERO; DOF];
    let mut torque = [ZERO; DOF];
    let mut com = [ZERO; DOF];
    for i in 0..DOF {
        let z = f.axis[i];
        let d = sub(f.point[i], p_prev);
        a = add(a, add(cross(wd, d), cross(w, cross(w, d))));
        let w_prev = w;
        w = add(w_prev, scale(z, qd[i]));
        wd = add(add(wd, scale(z, qdd[i])), cross(w_prev, scale(z, qd[i])));
        let b = &arm.bodies[i];
        let l = &f.link[i];
        let c = l.apply(b.com);
        com[i] = c;
        let r = sub(c, f.point[i]);
        let ac = add(a, add(cross(wd, r), cross(w, cross(w, r))));
        force[i] = scale(ac, b.mass);
        let iw = mm(&mm(&l.r, &b.inertia), &transpose(&l.r));
        torque[i] = add(mv(&iw, wd), cross(w, mv(&iw, w)));
        p_prev = f.point[i];
    }
    let mut tau = [0.0; DOF];
    // What the next link (or the environment, past the tool) puts on this one, about p_next.
    let mut f_next = scale(wrench.force, -1.0);
    let mut n_next = scale(wrench.moment, -1.0);
    let mut p_next = f.tcp.t;
    for i in (0..DOF).rev() {
        let p = f.point[i];
        let fi = add(force[i], f_next);
        let ni = add(add(torque[i], cross(sub(com[i], p), force[i])), add(n_next, cross(sub(p_next, p), f_next)));
        tau[i] = dot(ni, f.axis[i]);
        f_next = fi;
        n_next = ni;
        p_next = p;
    }
    tau
}

/// The torques the gears supply to hold the arm still against gravity (and a tool wrench):
/// what that needs less what the joints' preloads already push.
pub fn static_torque(arm: &ArmModel, f: &Frames, gravity: V3, wrench: &Wrench) -> Q {
    let t = rnea(arm, f, &[0.0; DOF], &[0.0; DOF], gravity, wrench);
    std::array::from_fn(|i| t[i] - arm.joints[i].drive.preload)
}

/// Point each joint's preload with gravity at `q`, the pose the arm works in: its gear then
/// holds gravity plus the preload on one flank, and a load that reverses by less than that
/// never opens the lost motion. A joint gravity hardly loads there (under a tenth of the
/// preload) keeps the preset's direction.
pub fn aim_preload(arm: &mut ArmModel, q: &Q, gravity: V3) {
    let hold = rnea(arm, &frames(arm, q), &[0.0; DOF], &[0.0; DOF], gravity, &Wrench::NONE);
    for (j, h) in arm.joints.iter_mut().zip(hold) {
        let p = j.drive.preload.abs();
        if h.abs() >= 0.1 * p {
            // The gear holds `h - preload`: a preload against `h`'s sign adds to it.
            j.drive.preload = -p.copysign(h);
        }
    }
}

/// The joint-space inertia `M(q)`, row-major 6 x 6.
pub fn mass_matrix(arm: &ArmModel, f: &Frames) -> [f64; DOF * DOF] {
    let mut m = [0.0; DOF * DOF];
    for j in 0..DOF {
        let mut e = [0.0; DOF];
        e[j] = 1.0;
        let c = rnea(arm, f, &[0.0; DOF], &e, ZERO, &Wrench::NONE);
        for i in 0..DOF {
            m[i * DOF + j] = c[i];
        }
    }
    // Exactly symmetric (the columns agree to rounding).
    for i in 0..DOF {
        for j in i + 1..DOF {
            let v = 0.5 * (m[i * DOF + j] + m[j * DOF + i]);
            m[i * DOF + j] = v;
            m[j * DOF + i] = v;
        }
    }
    m
}

/// An arm's joint state: link angles and speeds, and motor angles and speeds (link side:
/// the motor angle over the gear ratio).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JointState {
    pub q: Q,
    pub qd: Q,
    pub th: Q,
    pub thd: Q,
}

impl JointState {
    /// At rest at `q`, the motors where their springs hold `hold` (the torques the pose needs).
    pub fn at_rest(arm: &ArmModel, q: Q, hold: &Q) -> JointState {
        // The motor sits past the link by the spring's wind-up plus half the lost-motion band,
        // on the side the load presses.
        let th = std::array::from_fn(|i| {
            let d = &arm.joints[i].drive;
            let flank = if hold[i] == 0.0 { 0.0 } else { (0.5 * d.lost_motion).copysign(hold[i]) };
            q[i] + hold[i] / d.stiffness + flank
        });
        JointState { q, qd: [0.0; DOF], th, thd: [0.0; DOF] }
    }
}

/// What a drive is told: the motor angle and speed to hold, a feed-forward torque and the
/// servo gains (impedance style, per joint, link side).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DriveCmd {
    pub th: Q,
    pub thd: Q,
    pub tau: Q,
    pub kp: Q,
    pub kd: Q,
}

impl DriveCmd {
    /// Hold `th` with the drives' own gains and no feed-forward.
    pub fn hold(arm: &ArmModel, th: Q) -> DriveCmd {
        DriveCmd { th, thd: [0.0; DOF], tau: [0.0; DOF], kp: std::array::from_fn(|i| arm.joints[i].drive.kp), kd: std::array::from_fn(|i| arm.joints[i].drive.kd) }
    }
}

/// What a step did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StepInfo {
    pub substeps: u32,
    /// The motor torques applied (after the peak limit).
    pub tau_motor: Q,
    /// A joint's motor torque hit its peak.
    pub saturated: bool,
}

/// Below this motor speed (rad/s) Coulomb friction ramps in smoothly instead of flipping sign.
pub const FRICTION_EPS: f64 = 0.02;

/// The fastest joint mode under `cmd` with the inertia `m` (rad/s): spring, servo and
/// friction ramp against the rotor and the link. Explicit steps must resolve it.
pub fn fastest_mode(arm: &ArmModel, m: &[f64; DOF * DOF], cmd: &DriveCmd) -> f64 {
    let mut w2 = 0.0f64;
    for i in 0..DOF {
        let d = &arm.joints[i].drive;
        let b = d.reflected().max(1e-12);
        let link = m[i * DOF + i].max(1e-12);
        w2 = w2.max(d.stiffness * (1.0 / b + 1.0 / link) + cmd.kp[i] / b);
        // The servo damping and the friction ramp over the rotor (first-order rates).
        let rate = (cmd.kd[i] + d.damping + d.viscous + d.coulomb_total() / FRICTION_EPS) / b;
        w2 = w2.max(0.25 * rate * rate);
    }
    w2.sqrt()
}

/// The torque the gear passes from motor to link at `twist` (motor minus link angle) and
/// `rate` (its speed): the spring and its damping past the lost-motion band, none inside it.
/// With lost motion the flanks only push: a flank parting faster than the spring unloads
/// passes nothing rather than pulling the link back.
pub fn gear_torque(d: &Drive, twist: f64, rate: f64) -> f64 {
    let w = d.windup(twist);
    let t = d.stiffness * w + d.damping * rate;
    if d.lost_motion == 0.0 {
        t
    } else if w > 0.0 {
        t.max(0.0)
    } else if w < 0.0 {
        t.min(0.0)
    } else {
        0.0
    }
}

/// Advance `s` by `dt` under `cmd`, gravity and the tool `wrench`. The joint-space inertia and
/// the bias torques (gravity, Coriolis, the wrench) are taken once per call: over a millisecond
/// they change far less than the springs, servos and friction stepped at the substep rate.
pub fn step(arm: &ArmModel, s: &mut JointState, cmd: &DriveCmd, gravity: V3, wrench: &Wrench, dt: f64) -> StepInfo {
    let f = frames(arm, &s.q);
    let m = mass_matrix(arm, &f);
    let bias = rnea(arm, &f, &s.qd, &[0.0; DOF], gravity, wrench);
    let mut fac = m;
    let ok = cholesky(DOF, &mut fac);
    debug_assert!(ok, "mass matrix not positive definite");
    let w = fastest_mode(arm, &m, cmd);
    let n = ((w * dt / 0.4).ceil() as u32).clamp(1, 256);
    let h = dt / n as f64;
    let mut info = StepInfo { substeps: n, ..Default::default() };
    for _ in 0..n {
        let mut qdd = [0.0; DOF];
        for i in 0..DOF {
            let d = &arm.joints[i].drive;
            let spring = gear_torque(d, s.th[i] - s.q[i], s.thd[i] - s.qd[i]);
            qdd[i] = spring + d.preload - bias[i];
            let servo = cmd.kp[i] * (cmd.th[i] - s.th[i]) + cmd.kd[i] * (cmd.thd[i] - s.thd[i]) + cmd.tau[i];
            let tm = servo.clamp(-d.torque_peak, d.torque_peak);
            if tm != servo {
                info.saturated = true;
            }
            info.tau_motor[i] = tm;
            let fric = d.coulomb_total() * (s.thd[i] / FRICTION_EPS).tanh() + d.viscous * s.thd[i];
            s.thd[i] += h * (tm - spring - fric) / d.reflected().max(1e-12);
        }
        cholesky_back(DOF, &fac, &mut qdd);
        for i in 0..DOF {
            s.qd[i] += h * qdd[i];
            s.q[i] += h * s.qd[i];
            s.th[i] += h * s.thd[i];
        }
    }
    info
}

/// The arm's energy: link and rotor kinetic, spring and gravity potential (kg·mm²/s²).
pub fn energy(arm: &ArmModel, s: &JointState, gravity: V3) -> f64 {
    let f = frames(arm, &s.q);
    let m = mass_matrix(arm, &f);
    let mut e = 0.0;
    for i in 0..DOF {
        for j in 0..DOF {
            e += 0.5 * s.qd[i] * m[i * DOF + j] * s.qd[j];
        }
        let d = &arm.joints[i].drive;
        e += 0.5 * d.reflected() * s.thd[i] * s.thd[i];
        e += 0.5 * d.stiffness * d.windup(s.th[i] - s.q[i]).powi(2);
        e -= d.preload * s.q[i];
        let c = f.link[i].apply(arm.bodies[i].com);
        e -= arm.bodies[i].mass * dot(gravity, c);
    }
    e
}

/// Each joint's holding torque at `f` (gravity on the arm, tool and payload, and `wrench`) as
/// a share of its rated torque: past 1 the drive overheats if it holds there.
pub fn torque_use(arm: &ArmModel, f: &Frames, gravity: V3, wrench: &Wrench) -> Q {
    let tau = static_torque(arm, f, gravity, wrench);
    std::array::from_fn(|i| tau[i].abs() / arm.joints[i].drive.torque_rated.max(1e-9))
}

/// The extra mass (kg) the tool could take at its centre of mass in this pose before a joint's
/// holding torque reaches its rating (negative: already past it).
pub fn payload_margin(arm: &ArmModel, f: &Frames, gravity: V3, wrench: &Wrench) -> f64 {
    let tau = static_torque(arm, f, gravity, wrench);
    let at = f.flange.apply(arm.tool().body.com);
    let jac = crate::kin::jacobian(f, at);
    let mut best = f64::MAX;
    for i in 0..DOF {
        // One kilogram more at the tool's centre of mass: its holding torque on joint i.
        let u = -(jac[0][i] * gravity[0] + jac[1][i] * gravity[1] + jac[2][i] * gravity[2]);
        if u.abs() < 1e-9 {
            continue;
        }
        let rated = arm.joints[i].drive.torque_rated;
        let m = if u > 0.0 { (rated - tau[i]) / u } else { (-rated - tau[i]) / u };
        best = best.min(m);
    }
    best
}

/// The arm's lowest mode at `q` (Hz). Under our own joint loop each joint's gear spring is in
/// series with the servo's stiffness (`kp`, an impedance a disturbance meets) against the
/// link-side inertia plus the rotor's part (the rotor moves with the servo spring's share of
/// the deflection). Under a maker's position loop the motor holds and its lag is led out by
/// the planner, so the mode is the gears' and links' alone.
pub fn first_mode(arm: &ArmModel, q: &Q) -> f64 {
    let m = mass_matrix(arm, &frames(arm, q));
    let mut k = [0.0; DOF];
    let mut rotor = [0.0; DOF];
    let held = matches!(arm.controller.servo, ServoLoop::Vendor { .. });
    for i in 0..DOF {
        let d = &arm.joints[i].drive;
        let kg = d.stiffness.max(1e-9);
        if held {
            k[i] = kg;
            continue;
        }
        let ks = d.kp.max(1e-9);
        k[i] = kg * ks / (kg + ks);
        let share = kg / (kg + ks);
        rotor[i] = d.reflected() * share * share;
    }
    // K^-1/2 (M + R) K^-1/2: its largest eigenvalue is 1/ω² of the lowest mode.
    let mut a = [0.0; DOF * DOF];
    for i in 0..DOF {
        for j in 0..DOF {
            let r = if i == j { rotor[i] } else { 0.0 };
            a[i * DOF + j] = (m[i * DOF + j] + r) / (k[i] * k[j]).sqrt();
        }
    }
    let largest = symmetric_eigenvalues(&mut a).iter().fold(0.0f64, |x, v| x.max(*v));
    (1.0 / largest.max(1e-30)).sqrt() / std::f64::consts::TAU
}

/// Each joint's static stiffness as the TCP meets a slow push (torque per rad): the gear's
/// under a maker's position loop (its integrator holds the motor), the gear's in series with
/// the servo's kp under our own impedance loop.
fn joint_stiffness(arm: &ArmModel) -> Q {
    let held = matches!(arm.controller.servo, ServoLoop::Vendor { .. });
    std::array::from_fn(|i| {
        let d = &arm.joints[i].drive;
        let kg = d.stiffness.max(1e-9);
        if held {
            kg
        } else {
            let ks = d.kp.max(1e-9);
            kg * ks / (kg + ks)
        }
    })
}

/// The arm's stiffness at the TCP at `q` against a push in its softest direction (force per
/// mm): the joints' springs through the Jacobian, the links rigid.
pub fn tcp_stiffness(arm: &ArmModel, q: &Q) -> f64 {
    let f = frames(arm, q);
    let jac = crate::kin::jacobian(&f, f.tcp.t);
    let k = joint_stiffness(arm);
    // The compliance J K⁻¹ Jᵀ (translation rows), in the corner of a DOF×DOF matrix.
    let mut c = [0.0; DOF * DOF];
    for r in 0..3 {
        for s in 0..3 {
            c[r * DOF + s] = (0..DOF).map(|i| jac[r][i] * jac[s][i] / k[i]).sum();
        }
    }
    let softest = symmetric_eigenvalues(&mut c).iter().fold(0.0f64, |x, v| x.max(*v));
    1.0 / softest.max(1e-30)
}

/// The eigenvalues of a symmetric DOF×DOF matrix (cyclic Jacobi rotations; `a` is consumed).
fn symmetric_eigenvalues(a: &mut [f64; DOF * DOF]) -> [f64; DOF] {
    let n = DOF;
    for _ in 0..64 {
        let off: f64 = (0..n).flat_map(|i| (0..n).filter(move |j| *j != i).map(move |j| (i, j))).map(|(i, j)| a[i * n + j] * a[i * n + j]).sum();
        let diag: f64 = (0..n).map(|i| a[i * n + i] * a[i * n + i]).sum();
        if off <= 1e-24 * diag.max(1e-300) {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = 0.5 * (a[q * n + q] - a[p * n + p]) / apq;
                let t = theta.signum().max(0.0) * 2.0 - 1.0;
                let t = t / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akq) = (a[k * n + p], a[k * n + q]);
                    a[k * n + p] = c * akp - s * akq;
                    a[k * n + q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[p * n + k], a[q * n + k]);
                    a[p * n + k] = c * apk - s * aqk;
                    a[q * n + k] = s * apk + c * aqk;
                }
            }
        }
    }
    std::array::from_fn(|i| a[i * n + i])
}
