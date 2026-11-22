//! The dynamics' own checks, as the tests run them and the Arm workspace shows them: each one
//! a verdict and the figures behind it.

use crate::arm::*;
use crate::dynamics::*;
use crate::kin::*;
use crate::math::*;
use crate::presets::{desktop_qdd_754, uf850_stream};

/// One check's outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub pass: bool,
    /// What it measured, one line.
    pub figures: String,
}

/// A check and what it is called.
pub type Named = (&'static str, fn() -> Check);

const G: V3 = [0.0, 0.0, -GRAVITY];

/// The dynamics checks, in the order the workspace lists them.
pub const PHYSICS: &[Named] = &[
    ("Gravity torque is the potential's gradient", gravity_is_the_potential_gradient),
    ("Energy is conserved without damping", energy_is_conserved),
    ("One-joint step response", one_joint_step),
    ("The 850 is as stiff as its sheet", uf850_is_as_stiff_as_its_sheet),
];

fn potential(arm: &ArmModel, q: &Q) -> f64 {
    let f = frames(arm, q);
    // Gravity's, and the preloads' (a constant torque each: -preload times the angle).
    (0..DOF).map(|i| -arm.bodies[i].mass * dot(G, f.link[i].apply(arm.bodies[i].com)) - arm.joints[i].drive.preload * q[i]).sum()
}

/// The holding torques are the potential's gradient in four poses, and a 2 kg load at the TCP
/// adds 2 g times its lever on the shoulder.
pub fn gravity_is_the_potential_gradient() -> Check {
    let arm = desktop_qdd_754();
    let mut rng = Rng::new(3);
    let (lo, hi) = arm.limits();
    let mut worst = 0.0f64;
    for _ in 0..4 {
        let q: Q = std::array::from_fn(|i| {
            let (l, h) = (lo[i].max(-3.0), hi[i].min(3.0));
            l + (h - l) * (0.2 + 0.6 * rng.unit())
        });
        let tau = static_torque(&arm, &frames(&arm, &q), G, &Wrench::NONE);
        for j in 0..DOF {
            let h = 1e-6;
            let (mut a, mut b) = (q, q);
            a[j] += h;
            b[j] -= h;
            let grad = (potential(&arm, &a) - potential(&arm, &b)) / (2.0 * h);
            worst = worst.max((grad - tau[j]).abs() / grad.abs().max(NEWTON_METRE));
        }
    }
    let f = frames(&arm, &[0.0, 1.0, 1.0, 0.0, 0.3, 0.0]);
    let load = Wrench { force: [0.0, 0.0, -2.0 * GRAVITY], moment: ZERO };
    let lever = cross(f.axis[1], sub(f.tcp.t, f.point[1]));
    let added = static_torque(&arm, &f, G, &load)[1] - static_torque(&arm, &f, G, &Wrench::NONE)[1];
    let load_err = (added - 2.0 * GRAVITY * lever[2]).abs() / NEWTON_METRE;
    Check {
        pass: worst < 1e-6 && load_err < 1e-6,
        figures: format!("worst relative gap {worst:.1e}; a 2 kg TCP load off by {load_err:.1e} Nm"),
    }
}

/// Undamped and unpowered, the arm swings 0.4 s at 1 kHz and keeps its energy within 1 % of
/// its motion's.
pub fn energy_is_conserved() -> Check {
    let mut arm = desktop_qdd_754();
    for j in arm.joints.iter_mut() {
        let d = &mut j.drive;
        d.damping = 0.0;
        d.coulomb = 0.0;
        d.preload_friction = 0.0;
        d.viscous = 0.0;
        d.torque_peak = f64::MAX;
    }
    let q = [0.3, 1.2, 1.4, 0.2, 0.4, 0.1];
    let mut s = JointState { q, qd: [0.4, -0.3, 0.5, 0.6, -0.4, 0.8], th: q, thd: [0.0; DOF] };
    let free = DriveCmd { th: [0.0; DOF], thd: [0.0; DOF], tau: [0.0; DOF], kp: [0.0; DOF], kd: [0.0; DOF] };
    let e0 = energy(&arm, &s, G);
    let (mut worst, mut kinetic) = (0.0f64, 0.0f64);
    for _ in 0..400 {
        step(&arm, &mut s, &free, G, &Wrench::NONE, 1e-3);
        let e = energy(&arm, &s, G);
        worst = worst.max((e - e0).abs());
        kinetic = kinetic.max(e - potential(&arm, &s.q));
    }
    let share = worst / kinetic.max(1e-12);
    Check { pass: share < 0.01 && s.q != q, figures: format!("drift {:.3} % of the motion's energy over 0.4 s", share * 100.0) }
}

/// A 0.05 rad step on the base joint, gravity held by feed-forward: some overshoot, under
/// half; settled within 2 % in 0.6 s; the other joints held.
pub fn one_joint_step() -> Check {
    let arm = desktop_qdd_754();
    let q = [0.0, 1.2, 1.4, 0.0, 0.5, 0.0];
    let hold = static_torque(&arm, &frames(&arm, &q), G, &Wrench::NONE);
    let mut s = JointState::at_rest(&arm, q, &hold);
    let mut cmd = DriveCmd::hold(&arm, s.th);
    cmd.tau = hold;
    let target = q[0] + 0.05;
    cmd.th[0] += 0.05;
    let (mut peak, mut settled_at) = (0.0f64, None);
    for k in 0..1500 {
        step(&arm, &mut s, &cmd, G, &Wrench::NONE, 1e-3);
        peak = peak.max(s.q[0] - q[0]);
        let near = (s.q[0] - target).abs() < 0.02 * 0.05;
        match (near, settled_at) {
            (true, None) => settled_at = Some(k),
            (false, Some(_)) => settled_at = None,
            _ => {}
        }
    }
    let overshoot = peak / 0.05 - 1.0;
    let error = (s.q[0] - target).abs();
    let others = (1..DOF).map(|j| (s.q[j] - q[j]).abs()).fold(0.0f64, f64::max);
    let settled = settled_at.map_or("never".to_string(), |k| format!("{k} ms"));
    Check {
        pass: overshoot > 0.0 && overshoot < 0.5 && settled_at.is_some_and(|k| k < 600) && error < 5e-4 && others < 2e-3,
        figures: format!("overshoot {:.0} %, settles in {settled}, final error {:.1e} rad, others held to {:.1e} rad", overshoot * 100.0, error, others),
    }
}

/// The 850 on its controller as the note's §5b has it: with 3 kg on the flange, in four poses
/// across its working range, its lowest mode in 6-15 Hz and the TCP's stiffness in its softest
/// direction in 15-50 N/mm (both estimates there).
pub fn uf850_is_as_stiff_as_its_sheet() -> Check {
    let mut arm = uf850_stream();
    arm.payload = Body::block((3.0 - arm.tool().body.mass).max(0.0), [0.0, 0.0, 120.0], [50.0, 50.0, 50.0]);
    arm.refresh();
    let poses = [[0.0, 0.3, -1.2, 0.0, 0.9, 0.0], [0.0, 0.0, -0.8, 0.0, 0.8, 0.0], [0.0, 0.6, -1.0, 0.0, 0.4, 0.0], [0.0, -0.3, -1.6, 0.0, 1.9, 0.0]];
    let span = |v: &[f64]| v.iter().fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(*x), hi.max(*x)));
    let modes: Vec<f64> = poses.iter().map(|q| first_mode(&arm, q)).collect();
    let stiff: Vec<f64> = poses.iter().map(|q| tcp_stiffness(&arm, q) / NEWTON).collect();
    let (m, k) = (span(&modes), span(&stiff));
    Check {
        pass: m.0 >= 6.0 && m.1 <= 15.0 && k.0 >= 15.0 && k.1 <= 50.0,
        figures: format!("3 kg on the flange: lowest mode {:.1}-{:.1} Hz (6-15), TCP {:.1}-{:.1} N/mm (15-50)", m.0, m.1, k.0, k.1),
    }
}
