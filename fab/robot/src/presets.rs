//! Arms, tools and cells to start from. Kinematics and masses come from the makers' published
//! figures (a DH table and link data where one is published, else the published reach and joint
//! ranges with our own split of them), each figure's source named where it is used; what no maker
//! publishes (rotor, gear elasticity, friction, servo gains, the controller's timing) are our
//! estimates and say so in their [`Estimated`] flags. The research note behind the figures is
//! `local/research/dual-arm-metal-am.md` §4.

use crate::arm::*;
use crate::cell::{CellSetup, Positioner, Substrate};
use crate::math::*;
use crate::dynamics::mass_matrix;
use crate::kin::frames;

/// One preset: an arm and the figures the panel shows for it.
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub make: fn() -> ArmModel,
    /// Rated payload (kg) as published.
    pub rated_payload: f64,
    /// Repeatability as published (±, mm), taken as 3 sigma.
    pub repeatability: f64,
    /// Absolute accuracy uncalibrated (mm, estimate).
    pub accuracy: f64,
    /// The figures behind the model and where each comes from (P published, M measured,
    /// E estimated), as the research note's preset tables give them.
    pub sheet: &'static [SheetRow],
}

impl Preset {
    /// The repeatability's sigma per axis (mm).
    pub fn sigma(&self) -> f64 {
        self.repeatability / 3.0
    }
}

use Tag::{Estimated as E, Published as P};

const SHEET_B601: &[SheetRow] = &[
    ("Reach; arm mass", "754.7 mm with gripper, 587.5 mm without; 6.7 kg", P),
    ("Joint ranges", "J1 ±150°, J2 and J3 0-220°, J4 and J5 ±90°, J6 ±180°", P),
    ("Link lengths", "the reach split: J2 110 mm up, 230 mm upper arm, 281.5 mm forearm, 76 mm wrist", E),
    ("Link masses", "each link's motor at its published weight plus its structure", E),
    ("Payload", "2.5 kg rated, 5 kg max", P),
    ("Repeatability", "±0.1 mm", P),
    ("Joint drives", "J1-J3 9:1, 11/36 Nm; J4-J6 10:1, 5/14 Nm", P),
    ("Gear stiffness J1-J3 / J4-J6", "1e4 / 3e3 Nm/rad", E),
    ("Lost motion", "8 arcmin per joint (5-10)", E),
    ("Preload", "a 1 Nm bias on J1-J3, 0.5 Nm on J4-J6, with gravity where it loads the joint", E),
    ("Preload's cost", "0.07 / 0.035 Nm more friction; the gears hold the bias on top of gravity", E),
    ("Command; loop delay", "250 Hz impedance setpoints on one bus; 2 ms", E),
];

const SHEET_COBOT: &[SheetRow] = &[
    ("Reach; payload", "1300 mm; 12.5 kg", P),
    ("Kinematics, link masses, inertias", "the maker's DH and dynamics table", P),
    ("Repeatability", "±0.05 mm", P),
    ("Gear stiffness J1-J3 / J4-J6", "6e4-4e4 / 1.5e4 Nm/rad (strain wave)", E),
    ("Lost motion", "0.5 arcmin", E),
    ("Command; loop delay", "500 Hz streaming; 6 ms", P),
];

pub const SHEET_850: &[SheetRow] = &[
    ("Reach; arm mass; payload; speed", "850 mm; 20 kg; 5 kg; 1 m/s", P),
    ("Kinematics; link masses and centres", "the maker's DH and mass table", P),
    ("Joint ranges; speed", "±360°, ±132°, -242-3.5°, ±360°, ±124°, ±360°; 180°/s", P),
    ("Link inertias", "a block around each published centre of mass", E),
    ("Joint torques", "rated 155, 155, 75, 15, 15, 15 Nm; peak 2.5 times", E),
    ("Repeatability", "±0.02 mm", P),
    ("Gear stiffness J1-J3 / J4-J6", "3e4, 3e4, 2e4 / 1e4, 4.5e3, 4.5e3 Nm/rad (1.3e4-4e4 / 3e3-1.5e4)", E),
    ("Lost motion", "1 arcmin per joint behind motor-side encoders (0.25 mm at reach)", E),
    ("Servo command", "250 Hz, no buffer, faster ones dropped", P),
    ("Loop delay", "12 ms, 3 ms jitter (8-20 ms)", E),
    ("Its position loop", "10 Hz, damping 0.9", E),
];

const SHEET_850_GALVO: &[SheetRow] = &[
    ("Arm and controller", "as the 850 with its controller", P),
    ("Lost motion", "1 arcmin per joint behind motor-side encoders", E),
    ("Loop delay", "12 ms, 3 ms jitter (8-20 ms)", E),
    ("Galvo on the head", "±1 mm, 1 kHz corrections, a 400 Hz loop, half the beam's offset moves a wire bead", E),
];

const SHEET_850_OWN: &[SheetRow] = &[
    ("Arm", "as the 850 with its controller", P),
    ("Command (what-if: the controller replaced)", "1 kHz joint impedance and torque, our feed-forward", E),
    ("Loop delay", "2 ms (1-3)", E),
    ("Lost motion", "1 arcmin per joint, still behind motor-side encoders", E),
];

/// The research note's `abb_irb1100` table (§5d.5), the 0.58 m variant.
const SHEET_IRB1100: &[SheetRow] = &[
    ("Reach; arm mass", "580 mm; 21 kg", P),
    ("Payload; armload", "4 kg; 0.5 kg", P),
    ("Joint limits, deg", "J1 ±230; J2 -115..+113; J3 -205..+55; J4 ±230; J5 -125..+120; J6 ±400", P),
    ("Joint speed limits, deg/s", "460 / 380 / 280 / 560 / 420 / 750", P),
    ("Axis 2 height; axis 2 to wrist centre", "327 mm (907 working-range top less 580); 580 mm", P),
    ("Lower arm / upper arm; wrist to flange", "280 / 300 mm; 64 mm", E),
    ("RP -> sigma per axis", "0.01 -> 0.0033 mm", P),
    ("Path repeatability", "0.02-0.05 mm", E),
    ("Gear stiffness J1-J3 / J4-J6", "3e4, 3e4, 2e4 / 1e4, 8e3, 5e3 Nm/rad (2e4-8e4 / 3e3-1.5e4)", E),
    ("Lost motion", "1 arcmin (1-2)", E),
    ("Gear; reflected rotor", "100:1; 0.15 / 0.05 kg m2", E),
    ("Link mass split", "base+J1 35 %, lower arm 25 %, upper arm 25 %, wrist 15 %", E),
    ("External command", "joint stream every 4 ms (250 Hz)", P),
    ("Loop delay", "15 ms (10-20, maker's forum); 40-50 ms end to end on an older controller", P),
    ("Its position loop", "15 Hz, damping 0.9", E),
];

const SHEET_INDUSTRIAL: &[SheetRow] = &[
    ("Reach; payload; arm mass", "1.3 m; 10 kg; 55 kg", E),
    ("Kinematics", "a generic 6-axis with a shoulder and elbow offset", E),
    ("Repeatability", "±0.03 mm", E),
    ("Gear stiffness J1-J3 / J4-J6", "3e5, 3e5, 2e5 / 4e4, 3e4, 2e4 Nm/rad (cycloid class)", E),
    ("Lost motion", "0.5 arcmin", E),
    ("Gear; reflected rotor", "150:1; 3.4 / 0.7 kg m2", E),
    ("Link mass split", "column 45 %, lower arm 18 %, upper arm 17 %, wrist 4 %, base the rest", E),
    ("External command; loop delay", "250 Hz joint stream; 12 ms (8-40)", E),
    ("Its position loop", "25 Hz, damping 0.9", E),
];

pub const PRESETS: &[Preset] = &[
    Preset { id: "desktop_qdd_754", label: "6-axis 754 mm desktop QDD", make: desktop_qdd_754, rated_payload: 2.5, repeatability: 0.1, accuracy: 2.0, sheet: SHEET_B601 },
    Preset { id: "cobot_10kg", label: "6-axis 1300 mm 10 kg cobot", make: cobot_10kg, rated_payload: 12.5, repeatability: 0.05, accuracy: 1.0, sheet: SHEET_COBOT },
    Preset { id: "uf850_stream", label: "UFactory 850, its controller (servo streaming)", make: uf850_stream, rated_payload: 5.0, repeatability: 0.02, accuracy: 0.5, sheet: SHEET_850 },
    Preset { id: "uf850_stream_galvo", label: "UFactory 850, servo streaming + galvo head", make: uf850_stream_galvo, rated_payload: 5.0, repeatability: 0.02, accuracy: 0.5, sheet: SHEET_850_GALVO },
    Preset { id: "uf850_own", label: "UFactory 850, our joint loop (what-if: controller replaced)", make: uf850_own, rated_payload: 5.0, repeatability: 0.02, accuracy: 0.5, sheet: SHEET_850_OWN },
    Preset { id: "irb1100_058", label: "ABB IRB 1100-4/0.58, externally guided motion", make: irb1100_058, rated_payload: 4.0, repeatability: 0.01, accuracy: 0.35, sheet: SHEET_IRB1100 },
    Preset { id: "industrial_10kg", label: "6-axis 1300 mm 10 kg industrial", make: industrial_10kg, rated_payload: 10.0, repeatability: 0.03, accuracy: 0.75, sheet: SHEET_INDUSTRIAL },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// One arc minute (rad).
pub const ARCMIN: f64 = std::f64::consts::PI / (180.0 * 60.0);

#[allow(clippy::too_many_arguments)]
const fn drive(gear: f64, rotor_kgm2: f64, stiffness_nm: f64, damping_nms: f64, coulomb_nm: f64, viscous_nms: f64, rated_nm: f64, peak_nm: f64, kp_nm: f64, kd_nms: f64, lost_arcmin: f64, estimated: Estimated) -> Drive {
    Drive {
        gear,
        rotor: rotor_kgm2 * KG_M2,
        stiffness: stiffness_nm * NEWTON_METRE,
        damping: damping_nms * NEWTON_METRE,
        coulomb: coulomb_nm * NEWTON_METRE,
        viscous: viscous_nms * NEWTON_METRE,
        torque_rated: rated_nm * NEWTON_METRE,
        torque_peak: peak_nm * NEWTON_METRE,
        kp: kp_nm * NEWTON_METRE,
        kd: kd_nms * NEWTON_METRE,
        lost_motion: lost_arcmin * ARCMIN,
        preload: 0.0,
        preload_friction: 0.0,
        estimated,
    }
}

/// The friction a preload costs, as a share of it: a planetary mesh carrying a torque loses
/// about this much of it (90-95 % efficient; estimate).
pub const PRELOAD_MESH: f64 = 0.07;

/// `d` with a bias of `preload_nm` on its link taking up its lost motion, and the friction
/// that costs.
const fn preloaded(mut d: Drive, preload_nm: f64) -> Drive {
    d.preload = preload_nm * NEWTON_METRE;
    d.preload_friction = PRELOAD_MESH * preload_nm * NEWTON_METRE;
    d
}

/// The large QDD actuator: 9:1 planetary, 11 Nm rated, 36 Nm peak (maker); rotor, gear
/// elasticity, friction and the default impedance gains are estimates (MIT-mode kp 0..5000).
/// The planetary's lost motion, 8 arcmin (5 to 10, est.; the QDD class lists 8-9), is taken up
/// by a 1 Nm bias on each joint (a spring or a constant-force cable, the note's §5.3; est.),
/// pointed with gravity where gravity loads the joint: the gear then holds gravity plus the
/// bias, and the mesh carrying it costs 0.07 Nm of friction.
const QDD_LARGE: Drive = preloaded(drive(9.0, 1.0e-4, 1.0e4, 2.0, 0.2, 0.02, 11.0, 36.0, 1500.0, 15.0, 8.0, Estimated { gear: false, rotor: true, stiffness: true, friction: true, torque: false, gains: true }), 1.0);
/// The small QDD actuator: 10:1, 5 Nm rated, 14 Nm peak (maker); the rest estimates
/// (MIT-mode kp 0..500). Its 8 arcmin taken up by a 0.5 Nm bias.
const QDD_SMALL: Drive = preloaded(drive(10.0, 2.0e-5, 3.0e3, 0.5, 0.08, 0.01, 5.0, 14.0, 150.0, 1.5, 8.0, Estimated { gear: false, rotor: true, stiffness: true, friction: true, torque: false, gains: true }), 0.5);

/// The laser-wire head on the welding arm: a light weld head with its melt-pool camera and
/// wire feed nozzle (1.2 kg, estimate), the umbilical carried on a boom (what still hangs on
/// the tool: 0.2 kg plus a light pull toward the boom, estimates). Cost: head and wire feeder.
/// It lays 0.4 kg/h of steel on 1.5 kW, the middle of the note's 0.2-0.5 kg/h for a 1-2 kW
/// cobot head (estimates).
pub fn laser_wire_head() -> Tool {
    Tool {
        name: "Laser-wire head".into(),
        kind: ToolKind::LaserWire { wire_mm: 0.8, standoff: 10.0, power: 1.5, rate: 0.4 },
        tcp: Pose::at([0.0, 0.0, 170.0]),
        body: Body::block(1.2, [0.0, 0.0, 70.0], [60.0, 60.0, 140.0]),
        radius: 30.0,
        cable: Some(Cable { attach: [0.0, 0.0, 60.0], anchor: [0.0, 0.0, 900.0], stiffness: 0.02 * NEWTON, weight: 0.2 }),
        galvo: None,
        switch: 0.0,
        cost: 8000.0,
    }
}

/// A laser cutting head with its capacitive height control (3 kg, estimate: such heads weigh
/// 2 to 5 kg): trims without contact, a 0.2 mm kerf, nitrogen assist; the beam still damages
/// what it meets within 30 mm past the cut (estimate).
pub fn laser_cut_head() -> Tool {
    Tool {
        name: "Laser cutting head".into(),
        kind: ToolKind::LaserCut { kerf: 0.2, standoff: 1.0, defocus: 30.0, gas: AssistGas::Nitrogen, height_control: true },
        tcp: Pose::at([0.0, 0.0, 150.0]),
        body: Body::block(3.0, [0.0, 0.0, 60.0], [60.0, 60.0, 130.0]),
        radius: 30.0,
        cable: Some(Cable { attach: [0.0, 0.0, 50.0], anchor: [0.0, 0.0, 900.0], stiffness: 0.015 * NEWTON, weight: 0.15 }),
        galvo: None,
        switch: 0.0,
        cost: 6000.0,
    }
}

/// A compact cutting nozzle (0.9 kg, estimate: the handheld class weighs 0.7 to 1 kg) for an
/// arm too light for a cutting head: no height control (the line scan and the loop keep the
/// stand-off, and the kerf widens as the focus wanders), for thin sections only, nitrogen
/// assist (the note's §5.2).
pub fn compact_cut_nozzle() -> Tool {
    Tool {
        name: "Compact cutting nozzle".into(),
        kind: ToolKind::LaserCut { kerf: 0.2, standoff: 1.0, defocus: 30.0, gas: AssistGas::Nitrogen, height_control: false },
        tcp: Pose::at([0.0, 0.0, 120.0]),
        body: Body::block(0.9, [0.0, 0.0, 50.0], [45.0, 45.0, 110.0]),
        radius: 25.0,
        cable: Some(Cable { attach: [0.0, 0.0, 40.0], anchor: [0.0, 0.0, 900.0], stiffness: 0.015 * NEWTON, weight: 0.15 }),
        galvo: None,
        switch: 0.0,
        cost: 2000.0,
    }
}

/// A small milling spindle with a 4 mm end mill (1.5 kg, estimate): trims by cutting, with a
/// reaction force the arm must take.
pub fn spindle() -> Tool {
    Tool {
        name: "Milling spindle".into(),
        kind: ToolKind::Spindle { radius: 2.0, flute: 10.0 },
        tcp: Pose::at([0.0, 0.0, 140.0]),
        body: Body::block(1.5, [0.0, 0.0, 55.0], [55.0, 55.0, 110.0]),
        radius: 28.0,
        cable: Some(Cable { attach: [0.0, 0.0, 40.0], anchor: [0.0, 0.0, 900.0], stiffness: 0.01 * NEWTON, weight: 0.1 }),
        galvo: None,
        switch: 0.0,
        cost: 2500.0,
    }
}

/// A pulsed-laser ablation head: a ns source under a scan head with an f-theta lens (2.5 kg,
/// a 70 mm field, ±0.5 mm focus depth). The removal figures are estimates for steel (a 100 W
/// source, 1 mJ pulses at 100 kHz on a 50 µm spot; about 0.3 mm³/s), to be replaced by measured
/// ones; the trimming arm holds still while it scans. Cost includes the ns source.
pub fn ablation_head() -> Tool {
    let process = Ablation {
        field: 70.0,
        focus_depth: 0.5,
        fluence: 50.0,
        threshold: 1.5,
        penetration: 0.5e-3,
        spot: 0.05,
        rep_hz: 1.0e5,
        scan: 2000.0,
        hatch: 0.025,
        recast: 0.003,
        ra: 0.0015,
    };
    Tool {
        name: "Ablation scan head".into(),
        kind: ToolKind::Ablation(process),
        tcp: Pose::at([0.0, 0.0, 260.0]),
        body: Body::block(2.5, [0.0, 0.0, 60.0], [110.0, 110.0, 120.0]),
        radius: 60.0,
        cable: Some(Cable { attach: [0.0, 0.0, 50.0], anchor: [0.0, 0.0, 900.0], stiffness: 0.01 * NEWTON, weight: 0.1 }),
        galvo: Some(Galvo { range: 35.0, bandwidth: 1000.0, hz: 1.0e4, latency: 0.0005, capture: 1.0 }),
        switch: 2.0,
        cost: 12000.0,
    }
}

/// The laser-wire head with scanner mirrors for the last millimetre (estimates: a small
/// two-mirror unit adds 0.4 kg; ±1 mm, 1 kHz updates, a 400 Hz loop; with wire, half of a beam
/// offset moves the bead).
pub fn laser_wire_galvo_head() -> Tool {
    let mut t = laser_wire_head();
    t.name = "Laser-wire head with galvo".into();
    t.body = Body::block(1.6, [0.0, 0.0, 70.0], [70.0, 70.0, 140.0]);
    t.radius = 35.0;
    t.galvo = Some(Galvo { range: 1.0, bandwidth: 400.0, hz: 1000.0, latency: 0.001, capture: 0.5 });
    t.cost += 5000.0;
    t
}

/// How far past the longest head's tip beside it a line scanner's line falls (mm): the heads
/// stay clear of the part while it scans (estimate).
pub const SCAN_PAST: f64 = 30.0;

/// `heads` and a line scanner beside them, `side` off the flange's axis, mounted down its
/// bracket as far as its line must fall `SCAN_PAST` beyond their longest tip.
pub fn with_scanner(mut heads: Vec<Tool>, side: V3) -> Vec<Tool> {
    let tip = heads.iter().map(|h| h.tcp.t[2]).fold(0.0, f64::max);
    let standoff = line_scanner(ZERO).tcp.t[2];
    heads.push(line_scanner(add(side, [0.0, 0.0, (tip + SCAN_PAST - standoff).max(0.0)])));
    heads
}

/// A line scanner on the flange, used as a head for scan passes (0.4 kg; a 40 mm line, ±15 mm
/// depth of field; its line 120 mm past its mount; estimates), offset beside the process head
/// on a bracket ([`with_scanner`]).
pub fn line_scanner(beside: V3) -> Tool {
    Tool {
        name: "Line scanner".into(),
        kind: ToolKind::Scanner { width: 40.0, range: 15.0 },
        tcp: Pose::at(add(beside, [0.0, 0.0, 120.0])),
        body: Body::block(0.4, add(beside, [0.0, 0.0, 40.0]), [40.0, 60.0, 80.0]),
        radius: 0.0,
        cable: None,
        galvo: None,
        switch: 0.0,
        cost: 6000.0,
    }
}

/// (i) One head for both passes: welds with wire, cuts after a nozzle and focus change
/// (`switch` s, 15 by default; estimate). A handheld-class cutting mode: no height control.
/// Two entries, one per mode (the cutting mode adds no mass), then the line scanner.
pub fn combined_head(switch: f64) -> Vec<Tool> {
    let mut weld = laser_wire_head();
    weld.name = "Combined head (weld)".into();
    weld.body = Body::block(1.8, [0.0, 0.0, 70.0], [70.0, 70.0, 150.0]);
    weld.radius = 35.0;
    weld.switch = switch;
    weld.cost = 9000.0;
    let cut = Tool {
        name: "Combined head (cut)".into(),
        kind: ToolKind::LaserCut { kerf: 0.3, standoff: 1.5, defocus: 40.0, gas: AssistGas::Nitrogen, height_control: false },
        tcp: Pose::at([0.0, 0.0, 165.0]),
        body: Body::NONE,
        radius: 35.0,
        cable: None,
        galvo: None,
        switch,
        cost: 0.0,
    };
    with_scanner(vec![weld, cut], [0.0, 70.0, 0.0])
}

/// (ii) Two heads on one flange behind one source and a beam switch (0.5 s; estimate): the
/// weld head 60 mm to one side, the cutting head with height control 60 mm to the other, on a
/// 0.3 kg bracket; then the line scanner.
pub fn dual_head() -> Vec<Tool> {
    let side = |mut t: Tool, x: f64, name: &str| {
        t.name = name.into();
        t.tcp = Pose::at([x, 0.0, t.tcp.t[2]]);
        t.body.com[0] += x;
        t.switch = 0.5;
        t
    };
    let mut weld = side(laser_wire_head(), 60.0, "Dual head (weld)");
    weld.body = weld.body.joined(&Body::block(0.3, [0.0, 0.0, 15.0], [180.0, 40.0, 20.0]), &Pose::IDENTITY);
    let cut = side(laser_cut_head(), -60.0, "Dual head (cut)");
    with_scanner(vec![weld, cut], [0.0, 70.0, 0.0])
}

fn template(name: &str, controller: Controller, radius: Q) -> ArmModel {
    ArmModel {
        name: name.into(),
        base: Pose::IDENTITY,
        joints: Vec::new(),
        links: Vec::new(),
        radius,
        flange: Pose::IDENTITY,
        heads: vec![laser_wire_head()],
        head: 0,
        payload: Body::NONE,
        controller,
        bodies: [Body::NONE; DOF],
    }
}

/// The open 6-axis desktop arm, the reBot Arm B601-RS (754.7 mm reach with its gripper, 2.5 kg
/// rated), QDD drives, 250 Hz impedance setpoints over one CAN bus (about 500 Hz at most).
/// Published, on Seeed Studio's wiki (<https://wiki.seeedstudio.com/rebot_arm_b601_rs_pinocchio_meshcat/>):
/// the reach (754.7 mm with the gripper, 587.5 mm without), the joint ranges, 6.7 kg, the
/// payload, and its motors: three RobStride 06 (9:1, 11 Nm rated, 36 Nm peak, 621 g, 82 mm
/// across, 49 mm high) and four RobStride 00 (10:1, 5 / 14 Nm, 310 g, 57 mm across, 51 mm high),
/// the 06s on J1-J3 and the 00s on J4-J6 and the gripper. Its link lengths are not published:
/// the 587.5 mm reach is split by our estimate. J2's axis 110 mm up (J1's motor on a 20 mm
/// base, J2's axis one 06 radius above it), 76 mm from the wrist centre to the flange (J6's
/// motor and a 25 mm flange), the 511.5 mm left as a 230 mm upper arm and a 281.5 mm forearm
/// (the desktop pair's cut and rating checks hold for the splits near it we tried, not for an
/// even one). Zero is folded, as the published 0-220° of J2 and J3 suggest: the upper arm lying
/// back, the forearm on it; J2 raises it forward, J3 opens the forearm; at zero J5 holds the
/// tool square to the forearm. The 6.7 kg: the base 2.62 kg (J1's motor, the plate and the
/// motor drivers) and the gripper 0.56 kg (its 00 and fingers; the tool replaces it) left out,
/// each link its motor at its published weight plus a light structure (estimates).
pub fn desktop_qdd_754() -> ArmModel {
    use std::f64::consts::{FRAC_PI_2 as H, PI};
    let controller = Controller {
        setpoint_hz: 250.0,
        setpoint_hz_max: 500.0,
        latency: 0.002,
        jitter: 0.0003,
        interp: Interp::Hold,
        accel: [12.0, 12.0, 12.0, 20.0, 20.0, 20.0],
        jerk: [300.0, 300.0, 300.0, 600.0, 600.0, 600.0],
        follow_max: 0.35,
        watchdog: 0.1,
        servo: ServoLoop::Own,
    };
    let name = "6-axis 754 mm desktop QDD";
    let mut t = template(name, controller, [45.0, 40.0, 35.0, 30.0, 28.0, 25.0]);
    let deg = PI / 180.0;
    // The published ranges (degrees); safe working speeds, rad/s (estimates, under the motors'
    // published 100 rpm rated speed).
    let ranges = [(-150.0, 150.0), (0.0, 220.0), (0.0, 220.0), (-90.0, 90.0), (-90.0, 90.0), (-180.0, 180.0)];
    let speeds = [3.0, 3.0, 3.0, 4.0, 4.0, 4.0];
    t.joints = (0..DOF)
        .map(|i| Joint { name: format!("joint{}", i + 1), origin: Pose::IDENTITY, axis: [0.0, 0.0, 1.0], lo: ranges[i].0 * deg, hi: ranges[i].1 * deg, vmax: speeds[i], drive: if i < 3 { QDD_LARGE } else { QDD_SMALL } })
        .collect();
    // (a, alpha, d, theta offset): J2's axis 110 mm up; the 230 mm upper arm, flipped so J3
    // opens the other way round; the 281.5 mm forearm along J4; 76 mm to the flange.
    let dh = [(0.0, -H, 110.0, 0.0), (230.0, PI, 0.0, PI), (0.0, -H, 0.0, H), (0.0, H, 281.5, 0.0), (0.0, -H, 0.0, -H), (0.0, 0.0, 76.0, 0.0)];
    // Each in its DH frame: the turret round J2's axis, J2's 06 (0.621 kg) on a 0.3 kg bracket;
    // the upper arm, J3's 06 at the elbow on a 0.45 kg tube, its centre 48 mm short of the elbow;
    // the forearm, J4's 00 (0.31 kg) at the elbow on a 0.33 kg tube, its centre 92 mm along; the
    // wrist round J5's 00; J6's 00 halfway to the flange; the flange.
    let bodies = [
        Body::block(0.92, ZERO, [90.0, 110.0, 90.0]),
        Body::block(1.07, [-48.3, 0.0, 0.0], [230.0, 60.0, 90.0]),
        Body::block(0.64, [0.0, 0.0, 92.0], [60.0, 60.0, 281.5]),
        Body::block(0.41, ZERO, [60.0, 60.0, 60.0]),
        Body::block(0.38, [0.0, 0.0, 38.0], [57.0, 57.0, 76.0]),
        Body::block(0.1, ZERO, [50.0, 50.0, 10.0]),
    ];
    ArmModel::from_dh(name, &dh, &bodies, &t)
}

/// A 10 kg-class cobot (1300 mm reach, 12.5 kg payload; a published DH table and link data),
/// harmonic drives (estimates), 500 Hz streaming with a few ms of latency (estimate).
pub fn cobot_10kg() -> ArmModel {
    use std::f64::consts::FRAC_PI_2 as H;
    let controller = Controller {
        setpoint_hz: 500.0,
        setpoint_hz_max: 500.0,
        latency: 0.006,
        jitter: 0.001,
        interp: Interp::Velocity,
        accel: [8.0, 8.0, 8.0, 15.0, 15.0, 15.0],
        jerk: [150.0, 150.0, 150.0, 300.0, 300.0, 300.0],
        follow_max: 0.05,
        watchdog: 0.05,
        servo: ServoLoop::Own,
    };
    let mut t = template("6-axis 1300 mm 10 kg cobot", controller, [75.0, 65.0, 55.0, 45.0, 45.0, 40.0]);
    let big = drive(101.0, 0.5e-4, 6.0e4, 30.0, 6.0, 2.0, 150.0, 330.0, 2.0e4, 400.0, 0.5, Estimated::ALL);
    let mid = drive(101.0, 0.3e-4, 4.0e4, 20.0, 3.0, 1.0, 80.0, 150.0, 1.5e4, 250.0, 0.5, Estimated::ALL);
    let wrist = drive(101.0, 0.1e-4, 1.5e4, 5.0, 1.0, 0.3, 28.0, 56.0, 5.0e3, 60.0, 0.5, Estimated::ALL);
    let drives = [big, big, mid, wrist, wrist, wrist];
    t.joints = (0..DOF).map(|i| Joint { name: format!("joint{}", i + 1), origin: Pose::IDENTITY, axis: [0.0, 0.0, 1.0], lo: -std::f64::consts::TAU, hi: std::f64::consts::TAU, vmax: if i < 3 { 2.0 } else { 3.0 }, drive: drives[i] }).collect();
    // Elbow kept on one side: the third joint never folds through straight.
    t.joints[2].lo = -2.9;
    t.joints[2].hi = -0.05;
    // (a, alpha, d, theta offset) mm, rad.
    let dh = [(0.0, H, 180.7, 0.0), (-612.7, 0.0, 0.0, 0.0), (-571.55, 0.0, 0.0, 0.0), (0.0, H, 174.15, 0.0), (0.0, -H, 119.85, 0.0), (0.0, 0.0, 116.55, 0.0)];
    // Link mass, centre of mass and principal inertia in each DH frame (published).
    let body = |m: f64, c: V3, i: V3| Body { mass: m, com: scale(c, 1e3), inertia: [[i[0] * KG_M2, 0.0, 0.0], [0.0, i[1] * KG_M2, 0.0], [0.0, 0.0, i[2] * KG_M2]] };
    let bodies = [
        body(7.369, [0.021, 0.0, 0.027], [0.0341, 0.0353, 0.0216]),
        body(13.051, [0.38, 0.0, 0.158], [0.0281, 0.7707, 0.7694]),
        body(3.989, [0.24, 0.0, 0.068], [0.0101, 0.3093, 0.3065]),
        body(2.1, [0.0, 0.007, 0.018], [0.0030, 0.0022, 0.0026]),
        body(1.98, [0.0, 0.007, 0.018], [0.0030, 0.0022, 0.0026]),
        body(0.615, [0.0, 0.0, -0.026], [0.0001, 0.0004, 0.0003]),
    ];
    ArmModel::from_dh("6-axis 1300 mm 10 kg cobot", &dh, &bodies, &t)
}

/// The 850's harmonic drives, 100:1. No torques are published: rated is 1.5 times the most
/// a joint holds with the rated 5 kg 100 mm past the flange anywhere in its range (101.5, 49.4
/// and 9.7 Nm on J2, J3 and J5), rounded up to 5 Nm, J1 sized as J2 and J4 and J6 as J5;
/// peak 2.5 times rated. Rotor, elasticity, friction and gains are estimates too.
fn uf850_drives() -> [Drive; DOF] {
    let e = Estimated { gear: false, rotor: true, stiffness: true, friction: true, torque: true, gains: true };
    // (rotor kg·m², stiffness N·m/rad, its damping N·m·s/rad, coulomb N·m, viscous N·m·s/rad,
    // own-loop kp N·m/rad, kd N·m·s/rad, rated N·m)
    // Gear stiffness inside the note's §5b range (1.3e4–4e4 J1–J3, 3e3–1.5e4 J4–J6).
    let est = [
        (4.0e-5, 3.0e4, 21.0, 5.0, 3.0, 4.0e4, 600.0, 155.0),
        (4.0e-5, 3.0e4, 21.0, 6.0, 3.0, 4.0e4, 600.0, 155.0),
        (2.5e-5, 2.0e4, 14.0, 3.0, 2.0, 2.0e4, 300.0, 75.0),
        (1.0e-5, 1.0e4, 5.7, 2.0, 1.0, 6.0e3, 80.0, 15.0),
        (0.4e-5, 4.5e3, 2.0, 1.0, 0.5, 1.5e3, 20.0, 15.0),
        (0.4e-5, 4.5e3, 2.0, 1.0, 0.5, 1.5e3, 20.0, 15.0),
    ];
    std::array::from_fn(|i| {
        let (rotor, k, damping, coulomb, viscous, kp, kd, rated) = est[i];
        // A strain-wave gear's lost motion, about 1 arcmin: 0.25 mm at full reach.
        drive(100.0, rotor, k, damping, coulomb, viscous, rated, 2.5 * rated, kp, kd, 1.0, e)
    })
}

/// The 850 (850 mm reach, 5 kg, ±0.02 mm, 20 kg), controlled by `servo` at `hz` setpoints per
/// second (`hz_max` the most it takes) with `latency`. Published, on UFACTORY's
/// "Kinematic and Dynamic Parameters: UFactory 850"
/// (<https://docs.supportarticle.ufactory.cc/support_articles/developer/kinematic-and-dynamic-parameters/ufactory-850.html>):
/// the standard DH table and each link's mass and centre of mass (mm, in the modified DH frames);
/// on its technical specifications (<https://docs.850.ufactory.cc/8.technical_specifications.html>):
/// the joint ranges, 180°/s on every joint, the payload, mass and repeatability; on its robot
/// specifications (<https://docs.api.ufactory.cc/private_modbus_tcp/p04robot-specifications.html>):
/// at most 1145°/s² (20 rad/s²) and 28647°/s³ (500 rad/s³) per joint, which the controller's
/// limits below stay within. The link inertias (a block round each published centre) and the
/// drives (`uf850_drives`) are estimates.
fn uf850(name: &str, servo: ServoLoop, hz: f64, hz_max: f64, latency: f64, jitter: f64, interp: Interp) -> ArmModel {
    use std::f64::consts::PI;
    let controller = Controller {
        setpoint_hz: hz,
        setpoint_hz_max: hz_max,
        latency,
        jitter,
        interp,
        accel: [10.0, 10.0, 10.0, 15.0, 15.0, 15.0],
        jerk: [100.0, 100.0, 100.0, 200.0, 200.0, 200.0],
        follow_max: 0.05,
        watchdog: 0.1,
        servo,
    };
    let mut t = template(name, controller, [70.0, 60.0, 50.0, 45.0, 40.0, 38.0]);
    let deg = PI / 180.0;
    let ranges = [(-360.0, 360.0), (-132.0, 132.0), (-242.0, 3.5), (-360.0, 360.0), (-124.0, 124.0), (-360.0, 360.0)];
    let drives = uf850_drives();
    t.joints = (0..DOF)
        .map(|i| Joint { name: format!("joint{}", i + 1), origin: Pose::IDENTITY, axis: [0.0, 0.0, 1.0], lo: ranges[i].0 * deg, hi: ranges[i].1 * deg, vmax: 180.0 * deg, drive: drives[i] })
        .collect();
    // The published standard DH table: (theta offset °, d mm, alpha °, a mm) per joint.
    let published = [(0.0, 364.0, 90.0, 0.0), (90.0, 0.0, 180.0, 390.0), (90.0, 0.0, -90.0, 150.0), (0.0, 426.0, -90.0, 0.0), (0.0, 0.0, 90.0, 0.0), (0.0, 90.0, 0.0, 0.0)];
    let dh = published.map(|(theta, d, alpha, a)| (a, alpha * deg, d, theta * deg));
    // The published masses (kg) and centres of mass (mm, each in its link's modified DH frame).
    let mass = [4.811, 3.166, 2.247, 2.12, 1.346, 0.225];
    let com = [[-0.049, 13.92, -20.31], [327.98, 0.074, 6.48], [130.23, 105.18, 8.97], [0.016, -38.47, -98.3], [-0.42, -17.6, -1.42], [0.15, 1.38, -13.97]];
    // Each link's block (mm, estimates) along its standard DH frame's axes: the shoulder, the
    // 390 mm upper arm along x, the elbow, the 426 mm forearm along y, the wrist, the flange.
    let size = [[130.0, 130.0, 150.0], [390.0, 100.0, 100.0], [150.0, 100.0, 150.0], [90.0, 300.0, 90.0], [80.0, 80.0, 80.0], [70.0, 70.0, 30.0]];
    // Link i's modified DH frame sits on joint i's axis d_i above the joint frame; its standard
    // frame is a_i along x and alpha_i about x past that: the centre moves by both.
    let bodies: [Body; DOF] = std::array::from_fn(|i| {
        let (a, alpha, _, _) = dh[i];
        Body::block(mass[i], mv(&transpose(&rot_x(alpha)), sub(com[i], [a, 0.0, 0.0])), size[i])
    });
    let mut arm = ArmModel::from_dh(name, &dh, &bodies, &t);
    // J2 and J5 count positive the other way round from the table's z axes (J2 positive leans
    // the arm forward), the sense Fab's poses for the 850 use; their ranges are symmetric.
    for j in [1, 4] {
        arm.joints[j].axis = [0.0, 0.0, -1.0];
    }
    // J4 turns about the forearm's own line, so where on it its centre sits changes no pose. The
    // table puts it at the elbow's 150 mm offset; at the line's wrist end (where J5 crosses it)
    // the reach counts the offset and the 426 mm forearm as the straight 451.6 mm they span.
    let along = 426.0;
    arm.joints[3].origin = arm.joints[3].origin.mul(&Pose::at([0.0, 0.0, along]));
    arm.joints[4].origin = Pose::at([0.0, 0.0, -along]).mul(&arm.joints[4].origin);
    arm.links[3].com = sub(arm.links[3].com, [0.0, 0.0, along]);
    arm.refresh();
    if let ServoLoop::Vendor { bandwidth, zeta } = servo {
        vendor_gains(&mut arm, bandwidth, zeta, &[0.0, 0.3, -1.2, 0.0, 0.9, 0.0]);
    }
    arm
}

/// (a) The 850 under its own controller in servo mode: joint setpoints at 250 Hz with no
/// buffer (it drops what comes faster), 12 ms from command to motion with 3 ms of jitter (8 to
/// 20 ms, unpublished: estimate), its position loop about 10 Hz (estimate); we close only an
/// outer loop on tracker and IMU.
pub fn uf850_stream() -> ArmModel {
    uf850("UFactory 850 (servo streaming)", ServoLoop::Vendor { bandwidth: 10.0, zeta: 0.9 }, 250.0, 250.0, 0.012, 0.003, Interp::Hold)
}

/// (c) As (a), with scanner mirrors on the weld head for kHz last-mm correction.
pub fn uf850_stream_galvo() -> ArmModel {
    uf850_stream().with_tool(laser_wire_galvo_head())
}

/// (b) The 850 with its controller replaced by ours: 1 kHz impedance and torque commands per
/// joint (our feed-forward, active damping from tracker and IMU), 2 ms latency (1 to 3).
pub fn uf850_own() -> ArmModel {
    uf850("UFactory 850 (our joint loop)", ServoLoop::Own, 1000.0, 1000.0, 0.002, 0.0003, Interp::Hold)
}

/// The gains a maker's position loop of `bandwidth` Hz and damping ratio `zeta` amounts to per
/// joint: tuned on each joint's inertia (its link side at pose `q` plus its rotor) as the maker
/// would on its own model.
pub fn vendor_gains(arm: &mut ArmModel, bandwidth: f64, zeta: f64, q: &Q) {
    let m = mass_matrix(arm, &frames(arm, q));
    let w = std::f64::consts::TAU * bandwidth;
    for i in 0..DOF {
        let inertia = m[i * DOF + i] + arm.joints[i].drive.reflected();
        let d = &mut arm.joints[i].drive;
        d.kp = inertia * w * w;
        d.kd = 2.0 * zeta * inertia * w;
    }
}

/// The filament hot end with its direct-drive extruder, at `nozzle` mm (its range 0.4 to 1.2):
/// the high-flow hot end the research note takes (35 mm³/s at a 1.0 mm nozzle, 37 at 1.2, the
/// maker's PLA figures at 220 °C; linear between and beyond them), under 1 kg with the
/// extruder (0.9, estimate), its melt 30 ms behind the drive (estimate), a layer cooling 20 s
/// per mm of its height (12 s at 0.6 mm; the note's §5d.6 estimates).
pub fn filament_extruder(nozzle: f64) -> Tool {
    let nozzle = nozzle.clamp(0.4, 1.2);
    let process = Extruder { nozzle, feed: Feed::Filament { diameter: 1.75 }, flow_max: 35.0 + 10.0 * (nozzle - 1.0), lag: 0.03, cool_per_mm: 20.0 };
    Tool {
        name: format!("Filament hot end, {nozzle:.1} mm"),
        kind: ToolKind::Extruder(process),
        tcp: Pose::at([0.0, 0.0, 110.0]),
        body: Body::block(0.9, [0.0, 0.0, 50.0], [60.0, 60.0, 110.0]),
        radius: 35.0,
        cable: Some(Cable { attach: [0.0, 0.0, 60.0], anchor: [0.0, 0.0, 900.0], stiffness: 0.005 * NEWTON, weight: 0.05 }),
        galvo: None,
        switch: 0.0,
        cost: 350.0,
    }
}

/// The pellet extruder (a screw), at `nozzle` mm (0.4 to 2.5): at most 1.5 kg (maker); its
/// flow nozzle-pressure-limited at about 100 mm³/s at 1.0 mm, 175 at 1.5 and 215 at 2.0 (the
/// last the maker's 0.8-1.1 kg/h of PLA; the others estimates), below 1.0 mm with the bore's
/// area; the melt 0.3 s behind the screw (estimate).
pub fn pellet_extruder(nozzle: f64) -> Tool {
    let nozzle = nozzle.clamp(0.4, 2.5);
    let flow = if nozzle <= 1.0 {
        100.0 * nozzle * nozzle
    } else if nozzle <= 1.5 {
        100.0 + 150.0 * (nozzle - 1.0)
    } else {
        175.0 + 80.0 * (nozzle - 1.5)
    };
    let process = Extruder { nozzle, feed: Feed::Pellets, flow_max: flow, lag: 0.3, cool_per_mm: 20.0 };
    Tool {
        name: format!("Pellet extruder, {nozzle:.1} mm"),
        kind: ToolKind::Extruder(process),
        tcp: Pose::at([0.0, 0.0, 180.0]),
        body: Body::block(1.5, [0.0, 0.0, 80.0], [70.0, 70.0, 180.0]),
        radius: 40.0,
        cable: Some(Cable { attach: [0.0, 0.0, 120.0], anchor: [0.0, 0.0, 900.0], stiffness: 0.01 * NEWTON, weight: 0.15 }),
        galvo: None,
        switch: 0.0,
        cost: 2500.0,
    }
}

/// An extruder head to pick: what it is and the nozzles it takes.
#[derive(Clone, Copy, Debug)]
pub struct HeadPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub make: fn(f64) -> Tool,
    pub nozzles: [f64; 2],
    pub sheet: &'static [SheetRow],
}

pub const HEADS: &[HeadPreset] = &[
    HeadPreset {
        id: "filament",
        label: "Filament hot end (high flow)",
        make: filament_extruder,
        nozzles: [0.4, 1.2],
        sheet: &[
            ("Flow ceiling", "35 mm³/s at 1.0 mm, 37 at 1.2 (PLA, 220 °C)", P),
            ("Mass with its extruder", "0.9 kg", E),
            ("Melt lag", "30 ms", E),
            ("Shortest layer", "20 s per mm of layer height (12 s at 0.6 mm)", E),
        ],
    },
    HeadPreset {
        id: "pellet",
        label: "Pellet extruder",
        make: pellet_extruder,
        nozzles: [0.4, 2.5],
        sheet: &[
            ("Flow ceiling", "215 mm³/s at 2.0 mm (0.8-1.1 kg/h)", P),
            ("Flow at 1.0 / 1.5 mm", "100 / 175 mm³/s (nozzle pressure)", E),
            ("Mass", "1.5 kg", P),
            ("Melt lag", "0.3 s (screw)", E),
            ("Shortest layer", "20 s per mm of layer height", E),
        ],
    },
];

/// The IRB 1100-4/0.58 (580 mm, 4 kg, 0.01 mm RP, 21 kg) streamed joint targets every 4 ms
/// over externally guided motion, 15 ms from target to motion (10 to 20 by the maker's forum),
/// its own position loop about 15 Hz (estimate). Joint ranges and speeds as published; the
/// axis 2 height (327 mm) and the 580 mm from axis 2 to the wrist centre from the published
/// working range, their split into a 280 mm lower and a 300 mm upper arm and the 64 mm wrist
/// estimates; masses the note's split of the 21 kg, gears and stiffness estimates (§5d.5).
pub fn irb1100_058() -> ArmModel {
    use std::f64::consts::FRAC_PI_2 as H;
    let deg = std::f64::consts::PI / 180.0;
    let controller = Controller {
        setpoint_hz: 250.0,
        setpoint_hz_max: 250.0,
        latency: 0.015,
        jitter: 0.002,
        interp: Interp::Hold,
        accel: [20.0, 20.0, 20.0, 30.0, 30.0, 30.0],
        jerk: [300.0, 300.0, 300.0, 600.0, 600.0, 600.0],
        follow_max: 0.05,
        watchdog: 0.1,
        servo: ServoLoop::Vendor { bandwidth: 15.0, zeta: 0.9 },
    };
    let name = "ABB IRB 1100-4/0.58 (externally guided)";
    let mut t = template(name, controller, [60.0, 50.0, 45.0, 38.0, 35.0, 30.0]);
    let e = Estimated::ALL;
    // (rotor kg·m², stiffness, damping, coulomb, viscous, rated, peak) per joint.
    let est = [
        (1.5e-5, 3.0e4, 25.0, 2.0, 0.4, 40.0, 100.0),
        (1.5e-5, 3.0e4, 25.0, 4.0, 0.8, 80.0, 200.0),
        (1.5e-5, 2.0e4, 12.0, 2.0, 0.4, 40.0, 100.0),
        (0.5e-5, 1.0e4, 2.0, 0.4, 0.08, 8.0, 20.0),
        (0.5e-5, 8.0e3, 1.5, 0.4, 0.08, 8.0, 20.0),
        (0.5e-5, 5.0e3, 0.8, 0.2, 0.04, 4.0, 10.0),
    ];
    let ranges = [(-230.0, 230.0), (-115.0, 113.0), (-205.0, 55.0), (-230.0, 230.0), (-125.0, 120.0), (-400.0, 400.0)];
    let speeds = [460.0, 380.0, 280.0, 560.0, 420.0, 750.0];
    t.joints = (0..DOF)
        .map(|i| {
            let (rotor, k, c, coulomb, viscous, rated, peak) = est[i];
            Joint { name: format!("axis{}", i + 1), origin: Pose::IDENTITY, axis: [0.0, 0.0, 1.0], lo: ranges[i].0 * deg, hi: ranges[i].1 * deg, vmax: speeds[i] * deg, drive: drive(100.0, rotor, k, c, coulomb, viscous, rated, peak, 0.0, 0.0, 1.0, e) }
        })
        .collect();
    // Axis 2 above axis 1, the lower arm up at zero, the upper arm level and forward.
    let dh = [(0.0, -H, 327.0, 0.0), (280.0, 0.0, 0.0, -H), (0.0, -H, 0.0, 0.0), (0.0, H, 300.0, 0.0), (0.0, -H, 0.0, 0.0), (0.0, 0.0, 64.0, 0.0)];
    // 21 kg: the column with the base 35 %, the lower arm 25 %, the upper arm with its drives
    // 25 %, the wrist 15 % (forearm tube, wrist housing, flange); each in its DH frame.
    let bodies = [
        Body::block(7.35, [0.0, 170.0, 0.0], [160.0, 327.0, 160.0]),
        Body::block(5.25, [-140.0, 0.0, 0.0], [340.0, 90.0, 110.0]),
        Body::block(5.25, [0.0, 0.0, 60.0], [110.0, 110.0, 160.0]),
        Body::block(1.6, [0.0, -150.0, 0.0], [70.0, 300.0, 70.0]),
        Body::block(1.0, ZERO, [70.0, 70.0, 70.0]),
        Body::block(0.55, [0.0, 0.0, -20.0], [60.0, 60.0, 40.0]),
    ];
    let mut arm = ArmModel::from_dh(name, &dh, &bodies, &t);
    vendor_gains(&mut arm, 15.0, 0.9, &[0.0, 0.3, 0.6, 0.0, 0.7, 0.0]);
    arm
}

/// A generic 10 kg industrial arm (1.3 m; cycloid-class gears, 150:1, heavy reflected
/// rotors; a 25 Hz position loop; 250 Hz joint streaming with 12 ms of delay): every figure an
/// estimate from the note's §4 (`industrial_10kg`).
pub fn industrial_10kg() -> ArmModel {
    use std::f64::consts::FRAC_PI_2 as H;
    let controller = Controller {
        setpoint_hz: 250.0,
        setpoint_hz_max: 250.0,
        latency: 0.012,
        jitter: 0.002,
        interp: Interp::Hold,
        accel: [10.0, 10.0, 10.0, 20.0, 20.0, 20.0],
        jerk: [100.0, 100.0, 100.0, 300.0, 300.0, 300.0],
        follow_max: 0.05,
        watchdog: 0.1,
        servo: ServoLoop::Vendor { bandwidth: 25.0, zeta: 0.9 },
    };
    let name = "6-axis 1300 mm 10 kg industrial";
    let mut t = template(name, controller, [110.0, 90.0, 75.0, 55.0, 50.0, 40.0]);
    let e = Estimated::ALL;
    let est = [
        (1.5e-4, 3.0e5, 200.0, 15.0, 3.0, 300.0, 800.0),
        (1.5e-4, 3.0e5, 210.0, 17.0, 3.5, 350.0, 900.0),
        (1.0e-4, 2.0e5, 90.0, 7.5, 1.5, 150.0, 400.0),
        (0.3e-4, 4.0e4, 4.5, 2.0, 0.4, 40.0, 100.0),
        (0.3e-4, 3.0e4, 3.0, 2.0, 0.4, 40.0, 100.0),
        (0.2e-4, 2.0e4, 1.4, 1.0, 0.2, 20.0, 50.0),
    ];
    let deg = std::f64::consts::PI / 180.0;
    let ranges = [(-170.0, 170.0), (-185.0, 65.0), (-137.0, 163.0), (-185.0, 185.0), (-120.0, 120.0), (-350.0, 350.0)];
    let speeds = [220.0, 210.0, 270.0, 380.0, 310.0, 410.0];
    t.joints = (0..DOF)
        .map(|i| {
            let (rotor, k, c, coulomb, viscous, rated, peak) = est[i];
            Joint { name: format!("axis{}", i + 1), origin: Pose::IDENTITY, axis: [0.0, 0.0, 1.0], lo: ranges[i].0 * deg, hi: ranges[i].1 * deg, vmax: speeds[i] * deg, drive: drive(150.0, rotor, k, c, coulomb, viscous, rated, peak, 0.0, 0.0, 0.5, e) }
        })
        .collect();
    let dh = [(150.0, -H, 450.0, 0.0), (560.0, 0.0, 0.0, -H), (35.0, -H, 0.0, 0.0), (0.0, H, 600.0, 0.0), (0.0, -H, 0.0, 0.0), (0.0, 0.0, 90.0, 0.0)];
    let bodies = [
        Body::block(24.75, [-100.0, 200.0, 0.0], [300.0, 450.0, 300.0]),
        Body::block(9.9, [-280.0, 0.0, 0.0], [620.0, 150.0, 180.0]),
        Body::block(9.35, [0.0, 0.0, 100.0], [180.0, 180.0, 300.0]),
        Body::block(1.2, [0.0, -250.0, 0.0], [90.0, 500.0, 90.0]),
        Body::block(0.6, ZERO, [80.0, 80.0, 80.0]),
        Body::block(0.4, [0.0, 0.0, -25.0], [70.0, 70.0, 50.0]),
    ];
    let mut arm = ArmModel::from_dh(name, &dh, &bodies, &t);
    vendor_gains(&mut arm, 25.0, 0.9, &[0.0, 0.3, 0.6, 0.0, 0.7, 0.0]);
    arm
}

/// Two arms around a plate centred at `centre` (cell frame): the welding arm `w` on the −X
/// side facing +X, the trimming arm `c` on the +X side facing −X, both `out` mm from the centre
/// with their bases `drop` mm below the plate.
pub fn place_pair(mut w: ArmModel, mut c: ArmModel, centre: V3, out: f64, drop: f64) -> Vec<ArmModel> {
    w.base = Pose::new(I3, [centre[0] - out, centre[1], centre[2] - drop]);
    c.base = Pose::new(rot_z(std::f64::consts::PI), [centre[0] + out, centre[1], centre[2] - drop]);
    vec![w, c]
}

/// A two-axis tilt and turn positioner (tilt ±135°, endless turn, 50 kg, 40 arcsec; estimates)
/// with its tilt axis at `at` (cell frame) and the table top 120 mm above it.
pub fn positioner(at: V3) -> Positioner {
    let deg = std::f64::consts::PI / 180.0;
    Positioner {
        name: "Tilt and turn positioner".into(),
        base: Pose::at(at),
        table: Pose::at([0.0, 0.0, 120.0]),
        tilt: [-135.0 * deg, 135.0 * deg],
        vmax: [1.0, 2.0],
        accel: [3.0, 6.0],
        payload: 50.0,
        sigma: 40.0 / 3600.0 * deg,
        lag: 0.02,
        latency: 0.004,
        cost: 7000.0,
    }
}

/// Arm prices (USD, estimates).
const COST_850: f64 = 9000.0;
const COST_B601: f64 = 1500.0;
/// A 1.5 kW fibre source, an optical tracker over the cell, IMUs, the enclosure and its
/// interlocks; a beam switch where one source feeds two heads (USD, estimates).
const COST_SOURCE: f64 = 15000.0;
const COST_TRACKER: f64 = 15000.0;
const COST_ENCLOSURE: f64 = 5000.0;
const COST_BEAM_SWITCH: f64 = 4000.0;
const COST_IMU: f64 = 300.0;

fn others(arms: usize, beam_switch: bool) -> f64 {
    COST_SOURCE + COST_TRACKER + COST_ENCLOSURE + arms as f64 * COST_IMU + if beam_switch { COST_BEAM_SWITCH } else { 0.0 }
}

/// The plate top in a two-arm cell (cell frame).
const PLATE: V3 = [0.0, 0.0, 0.0];

/// A two-arm cell's fixed plate, the arms standing beside it: 300 mm square, 15 mm deep. An
/// estimate: as wide as the parts the cell is for (the note's ±0.5 mm budget runs over about
/// 300 mm), as deep as a weld substrate that the first layers' heat does not bow (10-20 mm).
pub const FIXED_PLATE: Substrate = Substrate { half: [150.0, 150.0], thick: 15.0, stub: None };
/// A positioner's table: 300 mm square, 20 mm deep. An estimate: the same parts' width, a
/// faceplate deep enough for the positioner's 50 kg.
pub const TABLE: Substrate = Substrate { half: [150.0, 150.0], thick: 20.0, stub: None };
/// A heated bed for printing plastic, the arm standing beside it: 350 x 320 mm, the H2D's
/// published build area (its own bed, so a part either prints on both), 10 mm deep (an
/// estimate: the heated plate and its sheet).
pub const BED: Substrate = Substrate { half: [175.0, 160.0], thick: 10.0, stub: None };

/// One arm over a positioner: the arm at the origin, the positioner's tilt axis 450 mm out in
/// front of it and 300 mm below the arm's base.
fn single(arm: ArmModel, label: &str, beam_switch: bool) -> CellSetup {
    CellSetup {
        label: label.into(),
        arms: vec![arm],
        positioner: Some(positioner([450.0, 0.0, -300.0])),
        fixtures: Vec::new(),
        plate: Pose::IDENTITY,
        substrate: TABLE,
        other_cost: others(1, beam_switch),
        arm_cost: vec![COST_850],
    }
}

/// Two arms facing over a fixed plate, `out` from its centre, bases 60 mm below it.
fn pair(w: ArmModel, c: ArmModel, label: &str, cost: [f64; 2]) -> CellSetup {
    let out = standoff(&w).max(standoff(&c));
    CellSetup {
        label: label.into(),
        arms: place_pair(w, c, PLATE, out, 60.0),
        positioner: None,
        fixtures: Vec::new(),
        plate: Pose::at(PLATE),
        substrate: FIXED_PLATE,
        other_cost: others(2, true),
        arm_cost: cost.to_vec(),
    }
}

/// One 850 with the combined head (i) over a positioner.
pub fn cell_single_850_combined() -> CellSetup {
    single(uf850_stream().with_heads(combined_head(15.0)), "One 850, combined head, positioner", false)
}

/// One 850 with the dual head (ii) over a positioner.
pub fn cell_single_850_dual_head() -> CellSetup {
    single(uf850_stream().with_heads(dual_head()), "One 850, dual head, positioner", true)
}

/// Two 850s: one deposits, one cuts; each with a line scanner.
pub fn cell_dual_850() -> CellSetup {
    let w = uf850_stream().with_heads(with_scanner(vec![laser_wire_head()], [0.0, 70.0, 0.0]));
    let c = uf850_stream().with_heads(with_scanner(vec![laser_cut_head()], [0.0, 70.0, 0.0]));
    pair(w, c, "Two 850s (deposit, cut)", [COST_850, COST_850])
}

/// Two desktop arms: one deposits, one cuts with a compact nozzle; each with a line scanner.
pub fn cell_dual_b601() -> CellSetup {
    let w = desktop_qdd_754().with_heads(with_scanner(vec![laser_wire_head()], [0.0, 70.0, 0.0]));
    // A 3 kg cutting head is past what the arm holds within its ratings (the note's §5.2).
    let c = desktop_qdd_754().with_heads(with_scanner(vec![compact_cut_nozzle()], [0.0, 70.0, 0.0]));
    pair(w, c, "Two desktop arms (deposit, cut)", [COST_B601, COST_B601])
}

/// The research note's split: the 850 does the process (the dual head without its scanner),
/// a desktop arm carries the line scanner.
pub fn cell_850_process_b601_scanner() -> CellSetup {
    let mut heads = dual_head();
    heads.retain(|h| !h.scans());
    let process = uf850_stream().with_heads(heads);
    let scanner = desktop_qdd_754().with_tool(line_scanner(ZERO));
    pair(process, scanner, "850 process, desktop arm scanning", [COST_850, COST_B601])
}

/// A cell to compare.
#[derive(Clone, Copy, Debug)]
pub struct CellPreset {
    pub id: &'static str,
    pub make: fn() -> CellSetup,
}

pub const CELLS: &[CellPreset] = &[
    CellPreset { id: "single_850_combined", make: cell_single_850_combined },
    CellPreset { id: "single_850_dual_head", make: cell_single_850_dual_head },
    CellPreset { id: "dual_850", make: cell_dual_850 },
    CellPreset { id: "dual_b601", make: cell_dual_b601 },
    CellPreset { id: "850_process_b601_scanner", make: cell_850_process_b601_scanner },
];

/// How far out a preset's bases go: inside the comfortable part of its reach.
pub fn standoff(arm: &ArmModel) -> f64 {
    (0.45 * arm.reach()).clamp(250.0, 900.0)
}

/// The IRB 1100's list price (EUR 26,300, about USD 28,000) and an industrial 10 kg arm's
/// (estimate); a heated bed for printing plastic (estimate).
const COST_IRB1100: f64 = 28000.0;
const COST_INDUSTRIAL: f64 = 25000.0;
const COST_BED: f64 = 500.0;

/// An arm that prints plastic and where it stands: the bed's centre `out` mm in front of its
/// base, its base `drop` mm below the bed's top (chosen so the bed and the parts on it sit in
/// the comfortable part of its reach, tool down).
#[derive(Clone, Copy, Debug)]
pub struct FdmArm {
    pub id: &'static str,
    pub label: &'static str,
    pub make: fn() -> ArmModel,
    pub out: f64,
    pub drop: f64,
    pub cost: f64,
    /// Where the arm's figures come from (its preset's sheet).
    pub sheet: &'static [SheetRow],
}

pub const FDM_ARMS: &[FdmArm] = &[
    FdmArm { id: "irb1100", label: "IRB 1100-4/0.58", make: irb1100_058, out: 340.0, drop: 150.0, cost: COST_IRB1100, sheet: SHEET_IRB1100 },
    FdmArm { id: "uf850_stream", label: "850, its controller", make: uf850_stream, out: 450.0, drop: 200.0, cost: COST_850, sheet: SHEET_850 },
    FdmArm { id: "uf850_own", label: "850, own 1 kHz loop (what-if)", make: uf850_own, out: 450.0, drop: 200.0, cost: COST_850, sheet: SHEET_850_OWN },
    FdmArm { id: "desktop_qdd_754", label: "Desktop QDD (B601)", make: desktop_qdd_754, out: 300.0, drop: 60.0, cost: COST_B601, sheet: SHEET_B601 },
    FdmArm { id: "industrial_10kg", label: "Industrial 10 kg", make: industrial_10kg, out: 650.0, drop: 250.0, cost: COST_INDUSTRIAL, sheet: SHEET_INDUSTRIAL },
];

/// One arm printing plastic over a fixed bed (the part frame at the bed's top centre) with an
/// optical tracker and an IMU on the head for the live loop.
pub fn fdm_cell(arm: &FdmArm, head: Tool) -> CellSetup {
    let mut a = (arm.make)().with_tool(head);
    a.base = Pose::at([-arm.out, 0.0, -arm.drop]);
    CellSetup {
        label: format!("{}, {}", arm.label, a.tool().name),
        arms: vec![a],
        positioner: None,
        fixtures: Vec::new(),
        plate: Pose::IDENTITY,
        substrate: BED,
        other_cost: COST_TRACKER + COST_IMU + COST_BED,
        arm_cost: vec![arm.cost],
    }
}
