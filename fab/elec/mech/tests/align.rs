//! Align finds a printed part's bosses on its mesh and fits a board's mounting holes onto them,
//! hole by hole.

use fab_elec_core::to_mm;
use fab_elec_fixtures::ledctl;
use fab_elec_mech::align::{compose, find_bosses, fit, invert, mount_of, Fit};
use fab_elec_mech::align_answer::{align_answer, snap_turn};
use fab_elec_mech::bridge::{board_inputs, CaseInputs};
use fab_model::eval::{Evaluated, Evaluator};
use fab_model::{Change, FabModel, PlaceArg};
use fab_plate::machine::Machine;

/// A 60 x 40 mm floor, 2 mm thick, with a column 6 mm across standing 5 mm on it at each of `at`
/// (model x, z; y up), a 2.5 mm hole 4.5 mm down each, and a 3 mm hole through the floor off the
/// pattern. "Off" has its second column 0.6 mm along +x.
const PARTS: &str = r#"use mod.model.*
use mod.fab.*
let m = mod.model
fn bossed(at) {
    let adds = []
    let cuts = [m.cylinder({radius: 1.5mm height: 4mm at: [-10mm, 1mm, 0]})]
    for a in at {
        adds.push(m.cylinder({radius: 3mm height: 5.01mm at: [a[0], 4.495mm, a[1]]}))
        cuts.push(m.cylinder({radius: 1.25mm height: 5mm at: [a[0], 5mm, a[1]]}))
    }
    return m.subtract(m.union(m.box({size: [60mm 2mm 40mm] at: [0, 1mm, 0]}), adds), cuts)
}
object(bossed([[-22mm, 12mm], [22mm, 12mm], [-22mm, -12mm], [22mm, -12mm]]), name: "Square")
object(bossed([[-22mm, 12mm], [22.6mm, 12mm], [-22mm, -12mm], [22mm, -12mm]]), name: "Off")
"#;

/// A 50 x 30 mm board's mounting holes (its frame, mm); its middle sits over the model's origin,
/// its x along the model's +x and its y along -z.
const HOLES: [[f64; 2]; 4] = [[3.0, 3.0], [47.0, 3.0], [3.0, 27.0], [47.0, 27.0]];

fn apply(m: &[[f64; 4]; 3], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// A pose `turn` (degrees) round the board's z and `shift` (mm) away from the board at `origin`
/// with its axes along the plate's.
fn pose(origin: [f64; 3], turn: f64, shift: [f64; 2]) -> [[f64; 4]; 3] {
    let (s, c) = turn.to_radians().sin_cos();
    [[c, -s, 0.0, origin[0] + shift[0]], [s, c, 0.0, origin[1] + shift[1]], [0.0, 0.0, 1.0, origin[2]]]
}

#[test]
fn a_board_set_askew_fits_onto_its_bosses_and_a_boss_out_of_place_is_named() {
    let mut model = FabModel::with_script("parts", "parts.cad.splash", PARTS);
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str, at: [f64; 2]| Change::Place { instance: "parts".into(), object: object.into(), arg: PlaceArg::At(Some(at)) };
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "parts", head, &[place("Square", [100.0, 100.0]), place("Off", [200.0, 100.0])], None)
        .unwrap_or_else(|e| panic!("the bossed parts build: {e}"));
    let (_, e) = evaluated.expect("a fresh apply evaluates");
    let up = [0.0, 0.0, 1.0];
    let part = |name: &str| {
        let ix = e.plate.parts.iter().position(|p| p.name == name).unwrap_or_else(|| panic!("{name} is on the plate"));
        (&e.plate.parts[ix].mesh, e.model_to_plate[ix])
    };
    // A board hole at its boss's top on the plate, the model's top 7 mm up.
    let at_top = |m: &[[f64; 4]; 3], h: [f64; 2]| apply(m, [h[0] - 25.0, 7.0, 15.0 - h[1]]);
    let on = |fit: &Fit, m: &[[f64; 4]; 3], i: usize| dist(apply(&fit.pose, [HOLES[i][0], HOLES[i][1], 0.0]), at_top(m, HOLES[i]));

    let (mesh, m) = part("Square");
    let origin = at_top(&m, [0.0, 0.0]);
    assert!(dist(at_top(&m, [1.0, 0.0]), [origin[0] + 1.0, origin[1], origin[2]]) < 1e-9
        && dist(at_top(&m, [0.0, 1.0]), [origin[0], origin[1] + 1.0, origin[2]]) < 1e-9, "the board's axes lie along the plate's");

    let bosses = find_bosses(&mesh.positions, &mesh.triangles, up);
    let columns: Vec<_> = bosses.iter().filter(|b| b.outer_d.is_some()).collect();
    assert_eq!(columns.len(), 4, "four columns: {bosses:?}");
    for b in &columns {
        assert!((b.outer_d.unwrap() - 6.0).abs() < 0.02 && b.hole_d.is_some_and(|d| (d - 2.5).abs() < 0.02) && (b.top - 7.0).abs() < 1e-6,
            "each column is 6 mm across with its 2.5 mm hole, its top 7 mm up: {b:?}");
        assert!(dist(b.axis, [b.at[0], b.at[1], b.top]) < 1e-9, "its axis point is its top on its axis, the plate's frame being the seat's: {b:?}");
    }
    assert!(bosses.iter().any(|b| b.outer_d.is_none() && b.hole_d.is_some_and(|d| (d - 3.0).abs() < 0.02) && (b.top - 2.0).abs() < 1e-6),
        "the hole through the floor is a boss of its own, at the floor's top: {bosses:?}");
    assert_eq!(bosses.len(), 5, "a column's foot and its hole's floor fold into it: {bosses:?}");

    let askew = pose(origin, 6.0, [1.2, -0.8]);
    let square = fit(&HOLES, &bosses, up, Some(&askew)).expect("holes and bosses fit");
    assert_eq!(square.on, 4, "every hole on its boss: {square:?}");
    for i in 0..4 {
        assert!(on(&square, &m, i) < 0.01, "hole {i} lands on its boss's axis at its top: {} mm off", on(&square, &m, i));
        assert!(square.holes[i].residual < 0.01 && square.holes[i].gap.abs() < 1e-6, "hole {i}'s report: {:?}", square.holes[i]);
    }
    let turned = fit(&HOLES, &bosses, up, Some(&pose(origin, 175.0, [1.2, -0.8]))).expect("holes and bosses fit");
    assert!(turned.on == 4 && (turned.pose[0][0] + 1.0).abs() < 1e-6,
        "a board turned near half round keeps its way round on a symmetric pattern: {:?}", turned.pose);

    let (mesh, m) = part("Off");
    let bosses = find_bosses(&mesh.positions, &mesh.triangles, up);
    let off = fit(&HOLES, &bosses, up, Some(&pose(at_top(&m, [0.0, 0.0]), 6.0, [1.2, -0.8]))).expect("holes and bosses fit");
    assert_eq!(off.on, 3, "three holes on their bosses: {off:?}");
    for i in [0, 2, 3] {
        assert!(on(&off, &m, i) < 0.01, "hole {i} still lands on its boss: {} mm off", on(&off, &m, i));
    }
    let miss = off.holes[1];
    assert!((miss.residual - 0.6).abs() < 0.01 && (miss.off[0] - 0.6).abs() < 0.01 && miss.off[1].abs() < 0.01,
        "the hole whose boss moved is 0.6 mm from its axis, along the board's +x: {miss:?}");
}

/// A bracket with two columns where the LED controller's mounting holes fall when its middle
/// stands at (4, 7, 6) mm in the model, turned 90 degrees, and the board inserted into it on
/// `board_mount(pcb, {MOUNT})`.
const BRACKET: &str = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let pcb = board("ledctl")
let adds = []
let cuts = []
for a in [[15mm, -15mm], [-7mm, -15mm]] {
    adds.push(m.cylinder({radius: 3mm height: 5.01mm at: [a[0], 4.495mm, a[1]]}))
    cuts.push(m.cylinder({radius: 1.25mm height: 5mm at: [a[0], 5mm, a[1]]}))
}
object(m.subtract(m.union(m.box({size: [40mm 2mm 40mm] at: [4mm, 1mm, -6mm]}), adds), cuts), name: "Bracket")
let ledctl_mount = board_mount(pcb, {MOUNT})
insert(board_solid(pcb, board_bodies("ledctl"), ledctl_mount), name: "Board", into: "Bracket")
"#;

/// Align on a part of the person's own: the board inserted askew on its mount, fitted onto the
/// bracket's columns, read back as the mount that seats it, and that mount written back seats
/// every hole on its column's axis. The board's bodies ride the mount with its slab.
#[test]
fn align_reads_back_the_mount_that_seats_a_board_on_a_bracket() {
    let design = ledctl();
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let holes: Vec<[f64; 2]> = pcb.holes.iter().map(|h| [to_mm(h.at.x), to_mm(h.at.y)]).collect();
    let build = |mount: &str| -> Evaluated {
        let mut model = FabModel::with_script("bracket", "bracket.cad.splash", &BRACKET.replace("MOUNT", mount));
        model.set_env(inputs.env());
        let mut ev = Evaluator::new(Machine::dual_nozzle_350());
        let head = model.head();
        let place = Change::Place { instance: "bracket".into(), object: "Bracket".into(), arg: PlaceArg::At(Some([150.0, 150.0])) };
        let (_, evaluated) = model.apply(&mut ev, "bracket", head, &[place], None).unwrap_or_else(|e| panic!("the bracket on {mount} builds: {e}"));
        evaluated.expect("a fresh apply evaluates").1
    };
    // The bracket's model-to-plate map and the board's frame in the model (board mm to model mm).
    let poses = |e: &Evaluated| {
        let ix = e.plate.parts.iter().position(|p| p.name == "Bracket").expect("the bracket");
        let board = e.plate.excluded.iter().find(|p| p.part.name == "Board").expect("the board rides the bracket");
        let (name, frame) = board.frame.clone().expect("the board carries its frame");
        assert_eq!(name, "board:ledctl", "the board's frame is the one the host draws it by");
        (ix, e.model_to_plate[ix], frame)
    };
    let close = |a: [f64; 3], b: [f64; 3], tol: f64| (0..3).all(|i| (a[i] - b[i]).abs() < tol);

    let askew = build("at: [5.5mm, 7mm, 5mm] turn: 84deg");
    let (ix, to_plate, frame) = poses(&askew);
    let (at, turn) = mount_of(&frame, [25.0, 15.0]).expect("a mounted board lies flat, facing up");
    assert!(close(at, [5.5, 7.0, 5.0], 1e-6) && (turn - 84.0).abs() < 1e-6, "the mount reads back as written: {at:?} {turn}");

    let mesh = &askew.plate.parts[ix].mesh;
    let prior = compose(&to_plate, &frame);
    let up = [prior[0][2], prior[1][2], prior[2][2]];
    let bosses = find_bosses(&mesh.positions, &mesh.triangles, up);
    let fitted = fit(&holes, &bosses, up, Some(&prior)).expect("the holes fit the columns");
    assert_eq!(fitted.on, 2, "both holes on their columns: {fitted:?}");
    let model_from_board = compose(&invert(&to_plate).expect("a placement inverts"), &fitted.pose);
    let (at, turn) = mount_of(&model_from_board, [25.0, 15.0]).expect("the fitted board lies flat");
    assert!(close(at, [4.0, 7.0, 6.0], 0.01) && (turn - 90.0).abs() < 0.01, "the fit is the bracket's mount: {at:?} {turn}");
    // fab.align says the same, with the edit that writes it.
    let source = BRACKET.replace("MOUNT", "at: [5.5mm, 7mm, 5mm] turn: 84deg");
    let script_of = |instance: &str| (instance == "bracket").then(|| ("bracket.cad.splash".to_string(), source.clone()));
    let answer = align_answer(&design, &askew, &script_of, Some("ledctl"), None).expect("fab.align answers");
    assert!(answer.contains("part: \"Bracket\" on: 2 of: 2")
        && answer.contains(r#"edits: "replace(ledctl_mount, board_mount(board(\"ledctl\"), {at: [4mm, 7mm, 6mm] turn: 90deg}))""#)
        && !answer.contains("misses"), "the answer: {answer}");

    let seated = build(&format!("at: [{}mm, {}mm, {}mm] turn: {}deg", at[0], at[1], at[2], turn));
    let (ix, to_plate, frame) = poses(&seated);
    let mesh = &seated.plate.parts[ix].mesh;
    let bosses = find_bosses(&mesh.positions, &mesh.triangles, up);
    let to_plate = compose(&to_plate, &frame);
    for h in &holes {
        let p = apply(&to_plate, [h[0], h[1], 0.0]);
        assert!(bosses.iter().any(|b| close(p, b.axis, 0.01)), "the hole at {h:?} sits on a column's axis at its top: {p:?} of {bosses:?}");
    }
}

/// A fitted turn snaps to an exact quarter turn only within what its holes resolve (0.1 mm across 20 mm is 0.29
/// degrees); a real angle, even a close one, is kept, and one hole alone resolves nothing.
#[test]
fn a_fitted_turn_snaps_to_a_quarter_turn_only_within_what_the_fit_resolves() {
    let pair = [[0.0, 0.0], [20.0, 0.0]];
    assert_eq!(snap_turn(89.9998, &pair), 90.0, "facet noise on a quarter turn reads exact");
    assert_eq!(snap_turn(-0.0001, &pair), 0.0, "and so does noise below zero, without a minus sign");
    assert_eq!(snap_turn(-179.9999, &pair), 180.0, "a half turn reads 180 either way round");
    assert_eq!(snap_turn(37.5, &pair), 37.5, "a non-round angle is kept");
    assert_eq!(snap_turn(84.0, &pair), 84.0, "a turn six degrees off a quarter is kept");
    assert_eq!(snap_turn(90.5, &pair), 90.5, "a turn past what the holes resolve is kept, not snapped");
    assert_eq!(snap_turn(89.9998, &[[0.0, 0.0]]), 89.9998, "one hole on resolves no turn: kept");
}
