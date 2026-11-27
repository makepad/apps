//! Q11 test 3, the mounted frame: a motion sensor's axes from its part's fields, through its
//! placement on the board (turned, on either face), into the case part the board is fixed to.

use fab_elec_board::{Board, Origin, Placed};
use fab_elec_circuit::{Circuit, Component};
use fab_elec_core::{to_mm, Angle, BlockId, BoardId, CompId, CompRef, ItemId, Side, P};
use fab_elec_design::fixture::fp;
use fab_elec_mech::motion::{board_sensors, mounted};
use fab_elec_mech::{CasePose, CaseRole};
use fab_elec_parts::{Fields, Footprint, PartDef, PartKind, Symbol};
use std::sync::Arc;

fn fields(pairs: &[(&str, &str)]) -> Fields {
    Fields(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
}

fn comp(id: u32, refdes: &str, footprint: &Arc<Footprint>, part_fields: Fields, own: Fields) -> Component {
    let part = Arc::new(PartDef {
        name: "imu".into(), kind: PartKind::of_name("imu"), symbol: Symbol::default(),
        footprint: Some(footprint.clone()), body: None, pins: Vec::new(), fields: part_fields, emitter: None, gpio: None, lifecycle: None, approved: false,
    });
    Component {
        id: CompId(id), path: refdes.to_lowercase(), refdes: refdes.into(), ref_kind: Default::default(), part, value: None,
        conns: Vec::new(), nc: Vec::new(), block: BlockId(0), dnp: false, fields: own,
    }
}

fn placed(comp: &Component, at: P, rot: Angle, side: Side) -> Placed {
    let footprint = comp.part.footprint.clone().expect("a footprint");
    Placed {
        id: ItemId(100 + comp.id.0), comp: comp.id, path: comp.path.clone(), refdes: comp.refdes.clone(),
        pad_nets: vec![None; footprint.pads.len()], footprint, at, rot, side, locked: false,
        origin: Origin::Authored, body_z: Placed::band_of(None), ref_label: None, value_label: None, legend: None, value: None,
    }
}

fn near(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-9)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// The direction the board maps the footprint's +x to, in the board's plane: the pads' own rule.
fn board_x(placed: &Placed) -> [f64; 3] {
    let (o, x) = (placed.to_board(P::ZERO), placed.to_board(P::mm(1.0, 0.0)));
    [to_mm(x.x - o.x), to_mm(x.y - o.y), 0.0]
}

#[test]
fn mounted_frame_follows_placement_and_case() {
    let footprint = Arc::new(fp::chip("lga", "0603"));
    let imu = fields(&[("motion.x", "+x"), ("motion.z", "+z")]);
    let comps = vec![
        comp(0, "U1", &footprint, imu.clone(), Fields::default()),
        comp(1, "U2", &footprint, imu.clone(), Fields::default()),
        comp(2, "U3", &footprint, imu.clone(), Fields::default()),
        comp(3, "U4", &footprint, imu.clone(), fields(&[("motion.x", "-y")])),
        comp(4, "U5", &footprint, fields(&[("motion.x", "+x")]), Fields::default()),
        comp(5, "U6", &footprint, fields(&[("motion.x", "+x"), ("motion.z", "-x")]), Fields::default()),
        comp(6, "R1", &footprint, Fields::default(), Fields::default()),
    ];
    let mut board = Board::new(BoardId(0), "main");
    let at = |i: usize| P::mm(10.0 * (i + 1) as f64, 5.0);
    let sides = [(Angle::deg(0.0), Side::Top), (Angle::QUARTER, Side::Top), (Angle::deg(0.0), Side::Bottom), (Angle::deg(0.0), Side::Top),
        (Angle::deg(0.0), Side::Top), (Angle::deg(0.0), Side::Top), (Angle::deg(0.0), Side::Top)];
    for (i, c) in comps.iter().enumerate() {
        board.placed.push(placed(c, at(i), sides[i].0, sides[i].1));
    }
    let circuit = Circuit { unit: BoardId(0), name: "main".into(), comps, ..Circuit::default() };
    let mut said = Vec::new();
    let sensors = board_sensors(&board, &circuit, &mut |m| said.push(m));
    let thick = to_mm(board.stackup.thickness);

    assert_eq!(sensors.iter().map(|s| s.comp.0).collect::<Vec<_>>(), [0, 1, 2, 3],
        "the parts that state both fields are sensors; a part with neither is not");
    assert!(said.len() == 2 && said[0].starts_with("U5: motion.x and motion.z come together")
        && said[1].starts_with("U6: motion.x '+x' and motion.z '-x' are not perpendicular"), "malformed fields are said, by refdes: {said:?}");
    let [u1, u2, u3, u4] = [&sensors[0], &sensors[1], &sensors[2], &sensors[3]];
    assert!(near(u1.at, [10.0, 5.0, thick]) && near(u3.at, [30.0, 5.0, 0.0]), "a sensor samples at its origin on its own face");
    assert!(near(u1.axes[0], [1.0, 0.0, 0.0]) && near(u1.axes[1], [0.0, 1.0, 0.0]) && near(u1.axes[2], [0.0, 0.0, 1.0]),
        "unturned on the top face, the sensor's axes are the board's");
    assert!(near(u2.axes[0], [0.0, 1.0, 0.0]) && near(u2.axes[1], [-1.0, 0.0, 0.0]) && near(u2.axes[2], [0.0, 0.0, 1.0]),
        "turned a quarter, X goes to +y and Y to -x");
    assert!(near(u3.axes[0], [-1.0, 0.0, 0.0]) && near(u3.axes[1], [0.0, 1.0, 0.0]) && near(u3.axes[2], [0.0, 0.0, -1.0]),
        "on the bottom face, X is mirrored and Z points down out of the board");
    for (sensor, placed) in sensors.iter().zip(&board.placed) {
        assert!(near(cross(sensor.axes[0], sensor.axes[1]), sensor.axes[2]), "{}: the frame stays right-handed", placed.refdes);
        if sensor.comp.0 != 3 {
            assert!(near(sensor.axes[0], board_x(placed)), "{}: X follows the footprint's +x as the pads map it", placed.refdes);
        }
    }
    assert!(near(u4.axes[0], [0.0, -1.0, 0.0]) && near(u4.axes[1], [1.0, 0.0, 0.0]), "a component's field overrides its part's");

    // The board in its case: face up, 3 mm above the plate; then face down (turned about x).
    let up = CasePose { part: 7, role: CaseRole::Shell("base".into()), to_board: [[1.0, 0.0, 0.0, -2.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, -3.0]] };
    let down = CasePose { part: 7, role: CaseRole::Shell("base".into()), to_board: [[1.0, 0.0, 0.0, 0.0], [0.0, -1.0, 0.0, 0.0], [0.0, 0.0, -1.0, 9.0]] };
    let m = mounted(u1, BoardId(4), up.part, &up.to_board).expect("a rigid frame inverts");
    assert_eq!((m.comp, m.part), (CompRef { board: BoardId(4), comp: CompId(0) }, 7), "the mount names the comp and the base");
    assert!(near(m.at, [12.0, 5.0, thick + 3.0]), "the sampling point is carried into the plate");
    assert!(near(m.axes[2], [0.0, 0.0, 1.0]), "face up, at rest the sensor reads +1 g on Z");
    let m = mounted(u1, BoardId(4), down.part, &down.to_board).expect("a rigid frame inverts");
    assert!(near(m.axes[2], [0.0, 0.0, -1.0]) && near(m.axes[1], [0.0, -1.0, 0.0]) && near(m.axes[0], [1.0, 0.0, 0.0]),
        "face down, the sensor reads -1 g on Z and its Y is reversed");
    assert!(near(m.at, [10.0, -5.0, 9.0 - thick]), "face down, the point is mirrored through the plate");
}

/// The handheld's IMU sits as on the real board (measured 2026-11-19): X toward the panel's top
/// edge, Y along the panel's columns, Z into the board, so the twin reads what the board reads.
#[test]
fn handheld_imu_axes_are_the_real_boards() {
    let sources = fab_elec_fixtures::docs("handheld").expect("handheld is a registered stock project");
    let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())
        .unwrap_or_else(|diags| panic!("handheld documents evaluate: {diags:?}"));
    let unit = evaluated.design.units.iter().find(|u| u.name == "handheld").expect("handheld lowers its unit");
    let board = unit.board.as_ref().expect("handheld has its board");
    let mut said = Vec::new();
    let sensors = board_sensors(board, &unit.circuit, &mut |m| said.push(m));
    assert!(said.is_empty() && sensors.len() == 1, "the IMU is the one motion sensor: {said:?}");
    let panel = board.placed.iter().find(|p| p.path == "panel").expect("the panel is placed");
    let (o, up) = (panel.to_board(P::ZERO), panel.to_board(P::mm(0.0, 1.0)));
    let up = [to_mm(up.x - o.x), to_mm(up.y - o.y), 0.0];
    let face = [0.0, 0.0, if panel.side == Side::Top { 1.0 } else { -1.0 }];
    let [x, y, z] = sensors[0].axes;
    assert!(near(x, up), "X points to the panel's top edge (memory row 0): {x:?}");
    assert!(near(y, cross(up, face)), "Y runs along the panel's columns: {y:?}");
    assert!(near(z, face.map(|v| -v)),
        "Z points into the board, so face up at rest it reads -1 g as the real board does (-1.08 g): {z:?}");
}
