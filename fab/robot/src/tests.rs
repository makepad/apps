use crate::arm::*;
use crate::collide::*;
use crate::dynamics::*;
use crate::field::*;
use crate::kin::*;
use crate::math::*;
use crate::presets::*;

const G: V3 = [0.0, 0.0, -GRAVITY];

/// A pose inside every joint's range, `k` of the way from the low to the high end.
fn pose_at(arm: &ArmModel, rng: &mut Rng) -> Q {
    let (lo, hi) = arm.limits();
    std::array::from_fn(|i| {
        let (l, h) = (lo[i].max(-3.0), hi[i].min(3.0));
        l + (h - l) * (0.2 + 0.6 * rng.unit())
    })
}

#[test]
fn desktop_arm_from_its_published_figures() {
    use std::f64::consts::FRAC_PI_2 as H;
    let arm = desktop_qdd_754();
    assert_eq!(arm.joints.len(), DOF, "six revolute joints from the base to the last link");
    let links: f64 = arm.links.iter().map(|b| b.mass).sum();
    assert!((links - (6.7 - 2.62 - 0.56)).abs() < 1e-9, "the published 6.7 kg less the base and the gripper: {links}");
    assert!((arm.moving_mass() - links - arm.heads_mass()).abs() < 1e-9, "the tool rides on the last link");
    assert!((arm.reach() - len(arm.tool().tcp.t) - 587.5).abs() < 1e-9, "the published reach without the gripper: {}", arm.reach());
    let (lo, hi) = arm.limits();
    assert!((hi[1] - lo[1] - 220f64.to_radians()).abs() < 1e-12 && lo[2] == 0.0 && (hi[5] - std::f64::consts::PI).abs() < 1e-12, "the published ranges");
    // Folded at zero, the upper arm lying back; J2 and J3 at 90° hold it up, the forearm level
    // forward and the tool down.
    assert!(dist(frames(&arm, &[0.0; DOF]).point[2], [-230.0, 0.0, 110.0]) < 1e-9, "folded at zero");
    let f = frames(&arm, &[0.0, H, H, 0.0, 0.0, 0.0]);
    assert!(dist(f.point[2], [0.0, 0.0, 340.0]) < 1e-9 && dist(f.point[4], [281.5, 0.0, 340.0]) < 1e-9, "elbow {:?}, wrist {:?}", f.point[2], f.point[4]);
    assert!(dot(f.tcp.z(), [0.0, 0.0, -1.0]) > 1.0 - 1e-12, "the tool down {:?}", f.tcp.z());
    let cobot = cobot_10kg();
    assert!((cobot.links.iter().map(|b| b.mass).sum::<f64>() - 29.104).abs() < 1e-6, "the DH preset keeps its link masses");
}

#[test]
fn the_850_from_its_published_figures() {
    let arm = uf850_own();
    let links: f64 = arm.links.iter().map(|b| b.mass).sum();
    assert!((links - 13.915).abs() < 1e-9, "link masses as published: {links}");
    let d = &arm.joints[1].drive;
    assert!((d.torque_peak - 2.5 * d.torque_rated).abs() < 1e-9 && d.estimated.torque, "no torque is published: estimates, flagged");
    // The published centre of the upper arm, (327.98, 0.074, 6.48) in its modified DH frame:
    // at zero the arm stands up from J2's axis 364 mm over the base, so it is 327.98 mm above it.
    let c = frames(&arm, &[0.0; DOF]).link[1].apply(arm.links[1].com);
    assert!(dist(c, [-0.074, -6.48, 364.0 + 327.98]) < 1e-9, "the upper arm's centre {c:?}");
    assert!((arm.reach() - (390.0 + 150f64.hypot(426.0) + 90.0 + 170.0)).abs() < 1e-9, "the reach at full stretch: {}", arm.reach());
    // The published DH table: folded at zero, tool down; joint 3 at -90° stretches the forearm
    // out level, tool forward.
    let tip = |q: Q| {
        let f = frames(&arm, &q);
        (f.tcp.t, f.tcp.z())
    };
    let (p, z) = tip([0.0; DOF]);
    assert!(dist(p, [150.0, 0.0, 68.0]) < 0.1 && dot(z, [0.0, 0.0, -1.0]) > 0.9999, "zero pose {p:?} {z:?}");
    let (p, z) = tip([0.0, 0.0, -std::f64::consts::FRAC_PI_2, 0.0, 0.0, 0.0]);
    assert!(dist(p, [686.0, 0.0, 904.0]) < 0.1 && dot(z, [1.0, 0.0, 0.0]) > 0.9999, "stretched {p:?} {z:?}");
    // The maker's loop: tuned to its bandwidth on the joint's inertia.
    let stream = uf850_stream();
    let (d, own) = (&stream.joints[0].drive, &arm.joints[0].drive);
    assert!(d.kp < own.kp && d.kp > 0.0, "a 10 Hz position loop is softer than ours");
    assert!(matches!(stream.controller.servo, ServoLoop::Vendor { .. }) && stream.controller.setpoint_hz_max == 250.0);
    assert!(uf850_stream_galvo().tool().galvo.is_some());
}

#[test]
fn ablation_removal_figures() {
    let ToolKind::Ablation(a) = ablation_head().kind else { panic!("an ablation head") };
    let pulse = a.penetration * (a.fluence / a.threshold).ln();
    assert!((a.depth_per_pulse() - pulse).abs() < 1e-15);
    let pass = a.depth_per_pass();
    assert!(pass > 0.003 && pass < 0.015, "a few µm per pass ({pass:.4} mm)");
    assert!((a.removal_rate() - pulse * std::f64::consts::PI * 0.25 * a.spot * a.spot * a.rep_hz).abs() < 1e-9, "rate = depth per pulse x spot x rate");
    assert!((a.pass_time(100.0) - 2.0).abs() < 1e-12, "100 mm² at 2 m/s on 25 µm lines");
    let below = Ablation { fluence: a.threshold * 0.9, ..a };
    assert_eq!(below.depth_per_pass(), 0.0, "below threshold nothing comes off");
}

#[test]
fn thermal_warp_flares_the_hot_top() {
    use crate::process::{Heat, Thermal};
    let th = Thermal::NOMINAL;
    let cold = Heat::default();
    let p = [7.1, -2.0, 2.0];
    assert_eq!(th.warp(&cold, p), p, "a cold part stands where it is");
    // A 1.5 kW weld (525 W absorbed) on the top of a 2 mm wall for 10 s, the plate 50 J/K.
    let mut h = cold;
    for _ in 0..10_000 {
        h = th.step(h, 525.0, 2.0, 50.0, 1e-3);
    }
    let steady = th.excess * 525.0;
    assert!(h.excess > 0.95 * steady && h.excess < steady && h.bulk > 50.0, "the work zone near its steady excess, the part warm: {h:?}");
    for q in [p, [7.1, 0.0, 0.0], [-4.9, 3.0, 1.0], [0.0, 0.0, 6.0]] {
        assert!(dist(th.cold(&h, th.warp(&h, q)), q) < 1e-9, "cold undoes warp at {q:?}");
    }
    let out = |z: f64| th.warp(&h, [7.1, 0.0, z])[0] - 7.1;
    assert!(out(2.0) > out(0.0) + 0.005 && out(0.0) > 0.0, "the hot top flares out past the base (a warp, not one growth): {:.4} against {:.4} mm", out(2.0), out(0.0));
    let up = th.warp(&h, [0.0, 0.0, 2.0])[2] - 2.0;
    assert!(up > 2.0 * th.expansion * h.bulk && up < 2.0 * th.expansion * (h.bulk + h.excess), "the top rises by its slices' growth: {up:.5} mm");
}

#[test]
fn ik_round_trips() {
    for arm in [desktop_qdd_754(), cobot_10kg(), uf850_own()] {
        let mut rng = Rng::new(7);
        let opts = IkOpts::default();
        for k in 0..24 {
            let q = pose_at(&arm, &mut rng);
            let f = frames(&arm, &q);
            let goal = IkGoal { p: f.tcp.t, z: f.tcp.z(), x: if k % 2 == 0 { None } else { Some(f.tcp.x()) } };
            let seed: Q = std::array::from_fn(|i| q[i] + 0.08 * rng.signed());
            let sol = ik(&arm, &goal, &seed, &opts).unwrap_or_else(|m| panic!("{}: pose {k} unsolved ({:.4} mm, {:.2e} rad)", arm.name, m.err_mm, m.err_rad));
            let g = frames(&arm, &sol);
            assert!(dist(g.tcp.t, goal.p) < 1e-3, "{}: pose {k} position", arm.name);
            assert!(dot(g.tcp.z(), goal.z) > 1.0 - 1e-9, "{}: pose {k} tool axis", arm.name);
            if let Some(x) = goal.x {
                assert!(dot(g.tcp.x(), x) > 1.0 - 1e-8, "{}: pose {k} full orientation", arm.name);
            }
        }
    }
}

#[test]
fn lost_motion_is_a_push_only_band() {
    // The desktop arm with its planetaries unloaded: 7 arcmin each.
    let mut arm = desktop_qdd_754();
    for j in &mut arm.joints {
        j.drive.lost_motion = 7.0 * ARCMIN;
    }
    let d = arm.joints[1].drive;
    let l = d.lost_motion;
    // Inside the band the gear passes nothing, however fast it closes.
    assert_eq!(gear_torque(&d, 0.4 * l, 5.0), 0.0);
    assert_eq!(gear_torque(&d, -0.4 * l, -5.0), 0.0);
    // Past it, the spring on the wind-up beyond half the band.
    let t = gear_torque(&d, 0.6 * l, 0.0);
    assert!((t - d.stiffness * 0.1 * l).abs() < 1e-9 * t.abs());
    // A flank parting fast does not pull.
    assert_eq!(gear_torque(&d, 0.6 * l, -1e3), 0.0);
    assert_eq!(gear_torque(&d, -0.6 * l, 1e3), 0.0);
    // Without a band it is the plain spring and damper.
    let mut s = d;
    s.lost_motion = 0.0;
    assert_eq!(gear_torque(&s, 1e-3, -1.0), s.stiffness * 1e-3 - s.damping);
    // At rest under gravity the motors sit on the loaded flank: holding them, the links stay.
    let q = [0.0, 1.2, 1.4, 0.0, 0.5, 0.0];
    let hold = static_torque(&arm, &frames(&arm, &q), G, &Wrench::NONE);
    let mut st = JointState::at_rest(&arm, q, &hold);
    assert!((st.th[1] - q[1] - hold[1] / d.stiffness).abs() > 0.49 * l);
    let mut cmd = DriveCmd::hold(&arm, st.th);
    cmd.tau = hold;
    for _ in 0..200 {
        step(&arm, &mut st, &cmd, G, &Wrench::NONE, 1e-3);
    }
    let moved = (1..DOF).map(|j| (st.q[j] - q[j]).abs()).fold(0.0f64, f64::max);
    assert!(moved < 1e-4, "loaded links moved {moved} rad");
}

#[test]
fn physics_checks() {
    for (name, check) in crate::checks::PHYSICS {
        let c = check();
        println!("CHECK {name}: {}", c.figures);
        assert!(c.pass, "{name}: {}", c.figures);
    }
}

#[test]
fn capsules_and_arm_clearance() {
    let a = Capsule { a: [0.0, 0.0, 0.0], b: [10.0, 0.0, 0.0], r: 1.0 };
    let b = Capsule { a: [5.0, 4.0, -3.0], b: [5.0, 4.0, 3.0], r: 1.0 };
    assert!((capsule_gap(&a, &b) - 2.0).abs() < 1e-12, "crossing segments 4 apart, radii 1 and 1");
    let c = Capsule { a: [12.0, 0.0, 0.0], b: [20.0, 0.0, 0.0], r: 0.5 };
    assert!((capsule_gap(&a, &c) - 0.5).abs() < 1e-12, "collinear segments 2 apart");
    let pair = place_pair(desktop_qdd_754(), desktop_qdd_754().with_tool(laser_cut_head()), [0.0; 3], 500.0, 60.0);
    let (w, c) = (&pair[0], &pair[1]);
    let home = [0.0, 1.2, 1.4, 0.0, 0.5, 0.0];
    let (gap, _, _) = arms_gap(&capsules(&w, &frames(&w, &home)), &capsules(&c, &frames(&c, &home)));
    assert!(gap > 50.0, "arms 1000 mm apart, both reaching in, keep a gap ({gap:.1})");
    let same = w.clone().with_base(c.base);
    let (gap, _, _) = arms_gap(&capsules(&same, &frames(&same, &home)), &capsules(&c, &frames(&c, &home)));
    assert!(gap < 0.0, "two arms on one base collide ({gap:.1})");
    // The part check leaves the tool's working end out.
    let f = frames(&w, &home);
    let tip = f.tcp.t;
    let ball = move |p: V3| dist(p, tip) - 5.0;
    let (g, _) = part_gap(&capsules(&w, &f), &ball, 40.0);
    assert!(g > 0.0, "a part at the TCP is the work, not a collision ({g:.1})");
    // The working head's capsule ends its radius short of the TCP, not past it.
    let caps = capsules(&c, &frames(&c, &home));
    let head = caps.last().expect("the head's capsule");
    let tip = frames(&c, &home).tcp.t;
    assert!(c.tool().radius > 0.0 && (dist(head.b, tip) - c.tool().radius).abs() < 1e-9, "the head ends {:.1} short", dist(head.b, tip));
}

/// The plate is a finite slab (an arm beside it is clear of it), a stub stands on it.
#[test]
fn substrate_is_a_finite_plate() {
    use crate::cell::Substrate;
    let s = Substrate::plate([100.0, 50.0], 10.0);
    for (p, d) in [([0.0, 0.0, 5.0], 5.0), ([0.0, 0.0, -4.0], -4.0), ([150.0, 0.0, -5.0], 50.0), ([0.0, 0.0, -30.0], 20.0)] {
        assert!((s.distance(p) - d).abs() < 1e-9, "{p:?}: {} not {d}", s.distance(p));
    }
    let st = s.on_stub(7.0, 40.0);
    assert!((st.distance([0.0, 0.0, 5.0]) - 5.0).abs() < 1e-9 && (st.distance([20.0, 0.0, -20.0]) - 13.0).abs() < 1e-9);
    assert_eq!((st.top(0.0, 0.0), st.top(20.0, 0.0), st.plate_top()), (0.0, -40.0, -40.0));
}

#[test]
fn field_beads_cuts_and_heights() {
    let mut f = BuildField::new(0.25);
    let bead = Bead { a: [0.0, 0.0, 0.0], b: [10.0, 0.0, 0.0], width: 3.0, height: 1.0, up: [0.0, 0.0, 1.0] };
    assert!((bead.distance([5.0, 0.0, 1.5]) - 0.5).abs() < 1e-12, "above the crown");
    assert!((bead.distance([5.0, 2.5, 0.5]) - 1.0).abs() < 1e-12, "beside the flank");
    f.add(&bead, 255);
    assert!(f.distance([5.0, 0.0, 0.5]) < -0.4, "inside the bead");
    assert!((f.distance([5.0, 0.0, 1.25]) - 0.25).abs() < 0.02, "a quarter above the crown");
    let top = f.top_below([5.0, 0.0, 5.0], 10.0).expect("the crown below");
    assert!((top - 1.0).abs() < 0.02, "crown at 1 mm ({top})");
    // Cut the right half away with a vertical 2 mm cylinder swept across.
    f.remove(&Cut { a: [8.0, -5.0, -1.0], b: [8.0, 5.0, -1.0], axis: [0.0, 0.0, 1.0], radius: 2.0, length: 5.0 }, 255);
    // A slab tilted 30° about y takes a sliver off the bead's start.
    let slab = Slab { centre: [0.5, 0.0, 0.5], r: rot_y(0.5), half: [0.3, 3.0, 2.0] };
    assert!(slab.distance([0.5, 0.0, 0.5]) < -0.29 && (slab.distance([0.5, 0.0, 3.0]) - (2.5f64 * 0.5f64.sin() - 0.3).hypot(2.5 * 0.5f64.cos() - 2.0)).abs() < 1e-9);
    f.remove(&slab, 255);
    assert!(f.distance([0.5, 0.0, 0.5]) > 0.2, "the sliver is gone");
    assert!(f.distance([8.0, 0.0, 0.5]) > f.band() * 0.99, "cut through (empty past the band)");
    assert!(f.distance([4.0, 0.0, 0.5]) < -0.4, "the rest stays");
    // Past the band the clearance is the way to what the field holds, never more.
    let far = f.clearance([5.0, 0.0, 30.0]);
    assert!(far > 20.0 && far <= 29.0 + 1e-9, "clearance 29 mm over the bead: {far:.2}");
    assert_eq!(BuildField::new(0.25).clearance([0.0; 3]), f64::INFINITY);
    // A height scan of a fresh 2 mm block surface.
    let mut e = BuildField::new(0.25);
    for i in 0..9 {
        for j in 0..9 {
            e.fuse_height(i as f64 * 0.5, j as f64 * 0.5, 2.0, 0.3, 200);
        }
    }
    assert!((e.distance([2.0, 2.0, 2.5]) - 0.5).abs() < 0.02, "half a mm above the scanned top");
    assert!(e.distance([2.0, 2.0, 0.5]) < -0.9, "solid under it");
    assert!(e.confidence([2.0, 2.0, 2.0]) > 0.7 && e.confidence([20.0, 20.0, 2.0]) == 0.0, "seen there, unseen far away");
    let mut pts = Vec::new();
    f.surface_points(&mut pts);
    // On the surface: all within half a cell, nearly all within 0.05 mm (the cut's edges are
    // kinks a sample's gradient only roughly follows).
    let off: Vec<f64> = pts.iter().map(|(p, _)| f.distance(*p).abs()).collect();
    let worst = off.iter().fold(0.0f64, |m, d| m.max(*d));
    assert!(!off.is_empty() && worst < 0.5 * f.cell(), "surface points on the surface (worst {worst:.3})");
    let close = off.iter().filter(|d| **d < 0.05).count() as f64 / off.len() as f64;
    assert!(close > 0.95, "{close:.3} of the points within 0.05 mm");
}

#[test]
fn payload_margin_falls_with_reach() {
    let arm = desktop_qdd_754();
    // The tool pointing down at the base's height, near and far out.
    let at = |x: f64, seed: Q| {
        let goal = IkGoal { p: arm.base.apply([x, 0.0, 0.0]), z: [0.0, 0.0, -1.0], x: None };
        let q = ik(&arm, &goal, &seed, &IkOpts::default()).unwrap_or_else(|m| panic!("{x} mm out: {:.3} mm", m.err_mm));
        frames(&arm, &q)
    };
    // Elbow up, the wrist square to the forearm: how this arm points its tool down.
    let seed = [0.0, 2.2, 2.0, 0.0, 0.2, 0.0];
    let (near, far) = (at(330.0, seed), at(450.0, seed));
    let (a, b) = (payload_margin(&arm, &near, G, &Wrench::NONE), payload_margin(&arm, &far, G, &Wrench::NONE));
    assert!(a > b, "330 mm out {a:.2} kg vs 450 mm out {b:.2} kg");
    let reaching = far;
    let use_ = torque_use(&arm, &reaching, G, &Wrench::NONE);
    assert!(use_.iter().all(|u| u.is_finite()), "torque use per joint");
}

#[test]
fn cell_setups_cost_and_carry() {
    for c in CELLS {
        let cell = (c.make)();
        assert!(cell.cost() > 20000.0, "{}: a priced cell ({})", c.id, cell.cost());
        assert!(!cell.arms.is_empty() && cell.arms.len() <= 2);
        assert_eq!(cell.positioner.is_some(), cell.arms.len() == 1, "{}: one arm works over a positioner", c.id);
        let deposits = cell.arms.iter().any(|a| a.heads.iter().any(|h| h.deposits()));
        let removes = cell.arms.iter().any(|a| a.heads.iter().any(|h| h.removes()));
        let scans = cell.arms.iter().any(|a| a.heads.iter().any(|h| h.scans()));
        assert!(deposits && removes && scans, "{}: deposits, cuts and scans", c.id);
    }
    // The dual head's mass on one 850: weld head and bracket, cutting head, scanner.
    let dual = cell_single_850_dual_head();
    assert!((dual.arms[0].heads_mass() - 4.9).abs() < 1e-9, "{}", dual.arms[0].heads_mass());
    // The positioner tilts the table about x, then turns it.
    let p = positioner([0.0; 3]);
    let t = p.table_frame([std::f64::consts::FRAC_PI_2, 0.0]);
    assert!(dist(t.t, [0.0, -120.0, 0.0]) < 1e-9 && dot(t.z(), [0.0, -1.0, 0.0]) > 1.0 - 1e-12, "tilted 90°: {:?}", t.t);
}

/// The IRB 1100-4/0.58 from its published working range: the wrist centre 300 mm out and
/// 607 mm up at zero, 907 mm up with the forearm raised.
#[test]
fn irb1100_working_range() {
    let arm = irb1100_058();
    let w = frames(&arm, &[0.0; DOF]).point[4];
    assert!(dist(w, [300.0, 0.0, 607.0]) < 1e-6, "{w:?}");
    let h = std::f64::consts::FRAC_PI_2;
    let up = [-h, h].into_iter().filter(|q| *q >= arm.joints[2].lo && *q <= arm.joints[2].hi).map(|q| frames(&arm, &[0.0, 0.0, q, 0.0, 0.0, 0.0]).point[4][2]).fold(0.0f64, f64::max);
    assert!((up - 907.0).abs() < 1e-6, "{up}");
    assert!(arm.joints.iter().all(|j| (j.drive.lost_motion - ARCMIN).abs() < 1e-12));
}
