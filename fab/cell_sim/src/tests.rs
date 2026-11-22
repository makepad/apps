use super::*;
use fab_robot::kin::{ik, IkGoal, IkOpts};
use fab_robot::presets::*;

/// The desktop pair over a plate at the cell origin, both arms elbow up with the tool down.
fn pair() -> (CellSetup, Vec<Q>) {
    let setup = cell_dual_b601();
    let home = vec![HOME; 2];
    (setup, home)
}

/// The desktop arm's elbow-up pose that points its tool down, the wrist square to the forearm.
const HOME: Q = [0.0, 2.2, 2.0, 0.0, 0.2, 0.0];

/// The joints that put arm `arm`'s TCP at `p` (part frame = cell frame here), tool down.
fn joints_at(cell: &SimCell, arm: ArmId, p: V3, seed: &Q) -> Q {
    let a = cell.info().arm(arm);
    let goal = IkGoal { p, z: [0.0, 0.0, -1.0], x: None };
    ik(a, &goal, seed, &IkOpts::default()).unwrap_or_else(|m| panic!("{p:?}: {:.3} mm", m.err_mm))
}

/// Stream rigid setpoints (no feed-forward) along `path` at `speed` for arm W, laser and wire
/// on when `deposit`; returns the observations.
fn drive(cell: &mut SimCell, path: &[V3], speed: f64, deposit: bool) -> Vec<Observation> {
    let arm = ArmId::W;
    let model = cell.info().arm(arm).clone();
    let hz = model.controller.setpoint_hz;
    let mut obs = Vec::new();
    let mut q = joints_at(cell, arm, path[0], &cell.true_q(arm));
    let gains = |m: &ArmModel| -> (Q, Q) { (std::array::from_fn(|j| m.joints[j].drive.kp), std::array::from_fn(|j| m.joints[j].drive.kd)) };
    let (kp, kd) = gains(&model);
    let mut t = cell.now();
    let send = |cell: &mut SimCell, t: f64, q: Q, obs: &mut Vec<Observation>| {
        cell.send(Command::Heartbeat { at: t }).unwrap();
        cell.send(Command::Joints { arm, at: t, cmd: JointCmd { th: q, thd: [0.0; DOF], tau: [0.0; DOF], kp, kd } }).unwrap();
        cell.advance(t + 1.0 / hz, obs);
    };
    // Move there in a second, then settle.
    let from = cell.true_q(arm);
    let n = hz as usize;
    for k in 1..=n {
        let s = k as f64 / n as f64;
        let s = s * s * (3.0 - 2.0 * s);
        send(cell, t, std::array::from_fn(|j| from[j] + (q[j] - from[j]) * s), &mut obs);
        t += 1.0 / hz;
    }
    for _ in 0..(0.3 * hz) as usize {
        send(cell, t, q, &mut obs);
        t += 1.0 / hz;
    }
    if deposit {
        let (watts, feed) = BeadModel::NOMINAL.settings(0.8, 3.0, 1.0, speed);
        cell.send(Command::Laser { arm, at: t, watts }).unwrap();
        cell.send(Command::Wire { arm, at: t, mm_s: feed }).unwrap();
    }
    for w in path.windows(2) {
        let n = (dist(w[0], w[1]) / speed * hz).ceil().max(1.0) as usize;
        for k in 1..=n {
            q = joints_at(cell, arm, lerp(w[0], w[1], k as f64 / n as f64), &q);
            send(cell, t, q, &mut obs);
            t += 1.0 / hz;
        }
    }
    cell.send(Command::Laser { arm, at: t, watts: 0.0 }).unwrap();
    cell.send(Command::Wire { arm, at: t, mm_s: 0.0 }).unwrap();
    for _ in 0..(0.1 * hz) as usize {
        send(cell, t, q, &mut obs);
        t += 1.0 / hz;
    }
    obs
}

#[test]
fn holds_still_and_is_deterministic() {
    let (setup, home) = pair();
    let run = |seed: u64| {
        let mut cell = SimCell::new(&setup, SimConfig::new(seed, Errors::TYPICAL, home.clone()));
        let mut obs = Vec::new();
        let start = cell.true_frames(ArmId::W).tcp.t;
        let mut t = 0.0;
        let mut settled = start;
        while t < 0.8 {
            t += 0.004;
            cell.send(Command::Heartbeat { at: t }).unwrap();
            cell.advance(t, &mut obs);
            if (t - 0.6).abs() < 1e-9 {
                settled = cell.true_frames(ArmId::W).tcp.t;
            }
        }
        let end = cell.true_frames(ArmId::W).tcp.t;
        (dist(start, settled), dist(settled, end), cell.true_q(ArmId::C), obs.len())
    };
    let (sag, moved, q, n) = run(5);
    assert!(sag < 10.0, "the unmodelled load sags the held arm a little ({sag:.3} mm)");
    assert!(moved < 0.05, "then it stays put ({moved:.4} mm)");
    assert!(n > 800, "encoders, trackers, IMUs report ({n})");
    assert_eq!(run(5).2, q, "the same seed, the same bits");
}

#[test]
fn deposits_where_the_tool_truly_is() {
    let (setup, home) = pair();
    let mut cell = SimCell::new(&setup, SimConfig::new(1, Errors::TYPICAL, home));
    // A 10 mm bead along y in front of arm W, at the plate.
    let base = cell.info().arm(ArmId::W).base.t;
    let x = base[0] + 420.0;
    let obs = drive(&mut cell, &[[x, -5.0, 0.0], [x, 5.0, 0.0]], 8.0, true);
    let log = cell.log();
    assert!(!log.beads.is_empty() && log.deposited > 10.0, "beads laid ({:.1} mm³)", log.deposited);
    // The bead sits under where the tool truly went (it sags, so not where it was sent).
    let (_, b) = log.beads[log.beads.len() / 2];
    let f = cell.truth_field();
    let mid = lerp(b.a, b.b, 0.5);
    assert!(f.distance([mid[0], mid[1], 0.3]) < -0.2, "solid at the bead");
    let top = f.top_below([mid[0], mid[1], 5.0], 10.0).expect("a top");
    assert!(top > 0.6 && top < 1.6, "about the planned 1 mm high ({top:.2})");
    assert!(b.width > 2.7 && b.width < 3.6, "about 3 mm wide, the true process fuller ({:.2})", b.width);
    assert!(obs.iter().any(|o| matches!(o, Observation::BeadWidth { .. })), "the melt pool reports");
    let tracker = obs.iter().find_map(|o| if let Observation::Tracker { tcp, at, .. } = o { Some((*at, tcp.t)) } else { None });
    assert!(tracker.is_some(), "the tracker reports");
    assert!(cell.temperature() > 0.0, "the part warmed");
}

#[test]
fn scans_the_plate_and_drops_fast_setpoints() {
    let (setup, mut home) = pair();
    // Arm W holds its scanner's line 2 mm over the plate.
    let mut w = setup.arms[0].clone();
    w.set_head(1);
    home[0] = ik(&w, &IkGoal { p: [0.0, 0.0, 2.0], z: [0.0, 0.0, -1.0], x: None }, &HOME, &IkOpts { iters: 200, ..IkOpts::default() }).unwrap_or_else(|m| panic!("the scanner over the plate: {:.3} mm", m.err_mm));
    let mut cell = SimCell::new(&setup, SimConfig::new(2, Errors::NONE, home));
    // The scanner is arm W's second head.
    cell.send(Command::Head { arm: ArmId::W, at: 0.0, head: 1 }).unwrap();
    let mut obs = Vec::new();
    let mut t = 0.0;
    while t < 0.3 {
        t += 0.004;
        cell.send(Command::Heartbeat { at: t }).unwrap();
        cell.advance(t, &mut obs);
    }
    assert!(obs.iter().any(|o| matches!(o, Observation::HeadReady { head: 1, .. })));
    let profile = obs.iter().rev().find_map(|o| if let Observation::Profile { points, .. } = o { Some(points.clone()) } else { None });
    let points = profile.expect("a profile");
    assert!(!points.is_empty(), "the plate is in range");
    // Through the scanner's true pose the hits lie on the plate.
    let at = cell.true_frames(ArmId::W).tcp;
    let worst = points.iter().map(|p| at.apply(*p)[2].abs()).fold(0.0f64, f64::max);
    assert!(worst < 0.1, "the plate scans at z = 0 ({worst:.3})");
    // A controller that takes 250 setpoints a second drops the ones sent between.
    let mut stream = cell_single_850_combined();
    stream.arms[0] = uf850_stream().with_heads(combined_head(15.0));
    let mut c = SimCell::new(&stream, SimConfig::new(3, Errors::NONE, vec![[0.0, 0.3, -1.2, 0.0, 0.9, 0.0]]));
    let m = c.info().arm(ArmId(0)).clone();
    let q = c.true_q(ArmId(0));
    let cmd = JointCmd { th: q, thd: [0.0; DOF], tau: [0.0; DOF], kp: std::array::from_fn(|j| m.joints[j].drive.kp), kd: [0.0; DOF] };
    for k in 0..20 {
        c.send(Command::Joints { arm: ArmId(0), at: k as f64 * 0.002, cmd }).unwrap();
    }
    assert_eq!(c.log().dropped, 10, "500 Hz into a 250 Hz controller");
}

#[test]
fn step_cost() {
    let (setup, home) = pair();
    let mut cell = SimCell::new(&setup, SimConfig::new(5, Errors::TYPICAL, home));
    let mut obs = Vec::new();
    let t0 = std::time::Instant::now();
    let mut t = 0.0;
    while t < 1.0 {
        t += 0.004;
        cell.send(Command::Heartbeat { at: t }).unwrap();
        cell.advance(t, &mut obs);
        obs.clear();
    }
    let cell_us = t0.elapsed().as_secs_f64() * 1e6 / 1000.0;
    let arm = cell.info().arm(ArmId::W).clone();
    let mut s = JointState { q: HOME, qd: [0.0; DOF], th: HOME, thd: [0.0; DOF] };
    let cmd = DriveCmd::hold(&arm, HOME);
    let t1 = std::time::Instant::now();
    for _ in 0..1000 {
        step(&arm, &mut s, &cmd, [0.0, 0.0, -GRAVITY], &Wrench::NONE, DT);
    }
    let step_us = t1.elapsed().as_secs_f64() * 1e6 / 1000.0;
    let t2 = std::time::Instant::now();
    let mut acc = 0.0;
    for _ in 0..1000 {
        let f = frames(&arm, &s.q);
        acc += mass_matrix(&arm, &f)[0];
    }
    let mm_us = t2.elapsed().as_secs_f64() * 1e6 / 1000.0;
    let base = cell.info().arm(ArmId::W).base.t;
    let mut q = joints_at(&cell, ArmId::W, [base[0] + 420.0, 0.0, 0.0], &HOME);
    let t3 = std::time::Instant::now();
    for k in 0..250 {
        q = joints_at(&cell, ArmId::W, [base[0] + 420.0, k as f64 * 0.04, 0.0], &q);
    }
    let ik_us = t3.elapsed().as_secs_f64() * 1e6 / 250.0;
    let mut field = BuildField::new(0.25);
    let t4 = std::time::Instant::now();
    for k in 0..40 {
        let a = [k as f64 * 0.5, 0.0, 0.0];
        field.add(&Bead { a, b: add(a, [0.5, 0.0, 0.0]), width: 3.0, height: 1.0, up: [0.0, 0.0, 1.0] }.shape(), 255);
    }
    let bead_us = t4.elapsed().as_secs_f64() * 1e6 / 40.0;
    println!("STEPCOST ik {ik_us:.1} us, bead stamp {bead_us:.1} us");
    println!("STEPCOST cell {cell_us:.1} us/ms (2 arms), plant step {step_us:.1} us, frames+mass matrix {mm_us:.1} us ({acc:.0})");
}
