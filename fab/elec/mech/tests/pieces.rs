//! The enclosure pieces build from the board summary alone and stay with their case half
//! when the lid turns into its print pose.

use fab_elec_core::{mm, to_mm, CompRef, FeatureRef, Side, Xform3, P};
use fab_elec_fixtures::ledctl;
use fab_elec_mech::bridge::{board_inputs, CaseInputs};
use fab_elec_mech::keys::feature_key;
use fab_elec_mech::{board_mech, case_checks, case_solids, BodyBox, CaseCheck, CaseFinding, CheckOpts, Level};
use fab_elec_parts::BodyPortKind;
use fab_model::eval::Evaluator;
use fab_model::program::split_id;
use fab_model::{Change, FabModel, PlaceArg};
use fab_plate::machine::Machine;
use fab_plate::{Optics, OpticsSource, PartMesh};

const CASE: &str = r#"use mod.model.*
use mod.fab.*
let pcb = board("ledctl")
let case = enclosure(pcb, {
    fasten: @snaps
    vents: [
        {face: @top at: [-12mm, 0] size: [10mm 12mm]}
        {face: @right at: [0, 4mm] size: [12mm 3mm]}
    ]
    exits: [{face: @back at: -10mm width: 12mm thick: 0.3mm}]
})
object(case.base, name: "Base")
object(case.lid, name: "Lid")
"#;

/// Is `p` inside the closed mesh? Parity of crossings along a ray in a direction no face of a
/// case lines up with.
fn inside(mesh: &PartMesh, p: [f64; 3]) -> bool {
    let d = [0.5773, 0.5774, 0.5775];
    let mut n = 0;
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|i| mesh.positions[i as usize]);
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let h = [d[1] * e2[2] - d[2] * e2[1], d[2] * e2[0] - d[0] * e2[2], d[0] * e2[1] - d[1] * e2[0]];
        let det = e1[0] * h[0] + e1[1] * h[1] + e1[2] * h[2];
        if det.abs() < 1e-12 {
            continue;
        }
        let s = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
        let u = (s[0] * h[0] + s[1] * h[1] + s[2] * h[2]) / det;
        let q = [s[1] * e1[2] - s[2] * e1[1], s[2] * e1[0] - s[0] * e1[2], s[0] * e1[1] - s[1] * e1[0]];
        let v = (d[0] * q[0] + d[1] * q[1] + d[2] * q[2]) / det;
        let t = (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) / det;
        if u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t > 0.0 {
            n += 1;
        }
    }
    n % 2 == 1
}

#[test]
fn a_vented_snapped_case_with_a_cable_exit_builds_closed_and_prints_clean() {
    let design = ledctl();
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    // the host's path: the board through the bridge, the prelude as a library
    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let env = inputs.env();
    let mut model = FabModel::with_script("case", "case.cad.splash", CASE);
    model.set_env(env);
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str, arg| Change::Place { instance: "case".into(), object: object.into(), arg };
    let changes = [place("Base", PlaceArg::At(Some([150.0, 150.0])))];
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "case", head, &changes, None).unwrap_or_else(|e| panic!("the vented snapped case with a cable exit builds: {e}"));
    let (_, e) = evaluated.expect("a fresh apply evaluates");
    let names: Vec<&str> = e.plate.parts.iter().map(|p| p.name.as_str()).collect();
    let part = |n: &str| e.plate.parts.iter().find(|p| p.name == n).unwrap_or_else(|| panic!("{n} on the plate: {names:?}"));
    let (base, lid) = (part("Base"), part("Lid"));
    assert_eq!(base.mesh.open_edges(), 0, "the vented base is closed");
    assert_eq!(lid.mesh.open_edges(), 0, "the vented lid with its snap bumps is closed");
    assert!(base.mesh.bounds().min[2].abs() < 1e-6 && lid.mesh.bounds().min[2].abs() < 1e-6, "both stand on the bed (the lid on its roof)");
    let errors: Vec<_> = e.printability.issues.iter().filter(|i| i.severity != fab_printability::Severity::Info).collect();
    assert!(errors.is_empty(), "the pieces print with no printability errors or warnings: {errors:?}");

    // Assembled model mm: wi = 50 + 2 * 0.4 = 50.8, di = 30.8; wo = 54.8, dd = 34.8.
    // The solid standoffs are max(3, 2.5 + 1) = 3.5 tall, board bottom = 1.6 + 3.5 = 5.1,
    // split = 5.1 + 1.6 = 6.7; roof_in = 6.7 + 3.16 + 1 = 10.86 (the receptacle is the tallest
    // part; the button's press pin fits under it), top = 10.86 + 1.6 = 12.46.
    // Snaps default to lip = 3, fit = 0.2. The lid prints at x + (54.8 + 10), 12.46 - y, -z.
    let ix = |n: &str| e.plate.parts.iter().position(|p| p.name == n).unwrap();
    let solid_at = |n: &str, p: [f64; 3]| {
        let p = if n == "Lid" { [p[0] + 64.8, 12.46 - p[1], -p[2]] } else { p };
        let m = e.model_to_plate[ix(n)];
        let q = [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        inside(&e.plate.parts[ix(n)].mesh, q)
    };
    // Three 1.8-wide top slots, pitch 3.6: x = -12 + [-3.6, 0, 3.6], length 12 along z.
    // -13.8 is halfway between the first two, with the roof sampled at (10.86 + 12.46) / 2.
    assert!(!solid_at("Lid", [-12.0, 11.66, 0.0]), "a top vent slot is empty through the roof");
    assert!(solid_at("Lid", [-13.8, 11.66, 0.0]), "the roof between two vent slots stays solid");
    // The right wall spans x = 50.8 / 2 .. 54.8 / 2; its middle slot is at z = 0, y = 4.
    assert!(!solid_at("Base", [26.4, 4.0, 0.0]), "a right-wall vent slot is empty in the base wall");
    // The cable notch is w = 12 + 2 * 0.3 = 12.6 wide, with h = 0.3 + 0.6 = 0.9:
    // x = -16.3 .. -3.7, y = 6.7 - 0.9 = 5.8 .. 6.7 + 3 + 0.5 = 10.2, above the step.
    // The back wall spans z = -17.4 .. -15.4; the notch cuts it through at the cable.
    assert!(!solid_at("Base", [-10.0, 6.3, -16.4]), "the cable notch is empty through the base's back wall");
    assert!(solid_at("Base", [-10.0, 5.0, -16.4]), "the base's wall under the cable notch stays solid");
    // The inner corner is y = 5.8, z = -30.8 / 2 = -15.4. A point 0.2 in from both
    // faces lies in the 1mm chamfer (0.2 + 0.2 < 1); one 1.5 below lies beneath it.
    assert!(!solid_at("Base", [-10.0, 5.8 - 0.2, -15.4 - 0.2]), "the cable bend chamfer is cut from the inner corner");
    assert!(solid_at("Base", [-10.0, 5.8 - 1.5, -15.4 - 0.2]), "the wall below the cable bend chamfer stays solid");
    // The lid's tongue spans z = -16.3 .. -15.4, with its rim at y = 6.7 + 0.2 = 6.9.
    assert!(solid_at("Lid", [-10.0, 8.0, -15.8]), "the lid's tongue over the cable notch stays solid");
    assert!(solid_at("Base", [-2.0, 6.3, -16.4]), "the base's wall beside the cable notch stays solid");
    // The front tongue face is (30.8 + 2 - 0.2) / 2 = 16.3, the step's inner face 16.5.
    // A long-side bump is at x = -(54.8 / 2 - 3 - 4 - 1) = -19.4, y = 6.7 + 3 / 2 = 8.2.
    // z = 16.6 is 0.3 proud: within the 0.4 bump and the 0.4 + 0.2 / 2 = 0.5 groove.
    assert!(solid_at("Lid", [-19.4, 8.2, 16.6]), "a lid snap bump is solid outside the tongue face");
    assert!(!solid_at("Base", [-19.4, 8.2, 16.6]), "the base's matching snap groove is empty at the bump");
    // mount1 at board (46, 4) becomes model (46 - 25, y, 15 - 4) = (21, y, 11).
    assert!(solid_at("Lid", [21.0, 11.66, 11.0]), "the snapped lid has no screw bore through the roof over mount1");
}

const PIPED: &str = r#"use mod.model.*
use mod.fab.*
use mod.math.*
let m = mod.model
let pcb = board("ledctl")
let case = enclosure(pcb, {pipes: {}})
object(case.base, name: "Base")
let clear = Material{albedo: [0.9 0.92 0.9] name: "Clear"}
let lid = [case.lid]
for p in case.pipes {
    m.fill(p.pipe, clear)
    lid.push(p.pipe)
}
object(lid, name: "Lid")
for i in case.inserts {
    insert(i.part, name: i.name, into: "Base", melt: i)
}
// One LED alone, one with its neighbour 2mm away, one with an LED 6mm away; and the row with
// channels too wide for the 2mm pitch.
let leds = []
for p in pcb.ports {
    if p.kind == @light { leds.push(p) }
}
let first = leds[0]
let near = []
let far = []
for q in leds {
    let dx = abs(q.at[0] - first.at[0])
    if dx > 1.9mm && dx < 2.1mm { near.push(q) }
    if dx > 5.9mm && dx < 6.1mm { far.push(q) }
}
let names = ["A" "B" "C"]
let lone = light_block([first], case.frame, case.shell, {})
let pair = light_block([first, near[0]], case.frame, case.shell, {})
let apart = light_block([first, far[0]], case.frame, case.shell, {})
for i in 0..lone.lid.add.len() { object(lone.lid.add[i], name: "Lone " + names[i]) }
for i in 0..pair.lid.add.len() { object(pair.lid.add[i], name: "Pair " + names[i]) }
for i in 0..apart.lid.add.len() { object(apart.lid.add[i], name: "Apart " + names[i]) }
let tight = light_block(pcb.ports, case.frame, case.shell, {along: 1.5mm})
if tight.pipes.len() == 0 && tight.lid.add.len() == 0 && tight.refused.len() == 1 {
    object(m.box({size: [1mm 1mm 1mm]}), name: "Refused")
}
"#;

fn apply(m: &[[f64; 4]; 3], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

#[test]
fn light_pipes_carry_each_led_through_the_lid_and_print_into_it() {
    let design = ledctl();
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mech = board_mech(pcb, &unit.circuit);
    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let mut model = FabModel::with_script("case", "case.cad.splash", PIPED);
    model.set_env(inputs.env());
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str, arg| Change::Place { instance: "case".into(), object: object.into(), arg };
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "case", head, &[place("Base", PlaceArg::At(Some([150.0, 150.0])))], None)
        .unwrap_or_else(|e| panic!("the case with light pipes builds: {e}"));
    let (compiled, e) = evaluated.expect("a fresh apply evaluates");
    let parts = &e.plate.parts;
    let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();

    // Grouping and refusal.
    for (name, on) in [("Lone A", true), ("Lone B", false), ("Pair A", true), ("Pair B", false), ("Apart A", true), ("Apart B", true), ("Refused", true)] {
        assert_eq!(names.contains(&name), on, "{name} on the plate: a lone LED and two neighbours get one block each, two LEDs 6mm apart two, and a 1.5mm channel at a 2mm pitch is refused: {names:?}");
    }

    // The lid's object: its shell and one pipe per LED, glued, each pipe with its optics.
    let estimate = Some(Optics { throughput: 0.2, source: OpticsSource::Estimate });
    let pipes: Vec<usize> = (0..parts.len()).filter(|&i| parts[i].optics.is_some()).collect();
    assert_eq!(pipes.len(), 8, "a pipe per LED: {names:?}");
    let group = parts[pipes[0]].body_of.clone();
    assert!(group.is_some(), "the pipes are bodies of the lid's object");
    let lid: Vec<usize> = (0..parts.len()).filter(|&i| parts[i].body_of == group && parts[i].optics.is_none()).collect();
    assert_eq!(lid.len(), 1, "the lid's object holds the lid's shell and its pipes: {names:?}");
    let lid = lid[0];
    assert_eq!(parts[lid].name, "Lid", "the lid's shell reads as the lid");
    assert!(parts.iter().all(|p| !p.name.contains('#')), "no part shows a generated name: {names:?}");
    for &i in &pipes {
        assert_eq!(parts[i].body_of, group, "every pipe prints into the lid");
        assert_eq!(parts[i].optics, estimate, "each pipe carries its throughput as an estimate after the print pose");
        assert_ne!(parts[i].color, parts[lid].color, "the pipes print in their own colour");
    }

    // The lid's board frame, as the checks read it: board mm -> model (print pose) -> plate.
    let frame = *e.objects.iter().find_map(|o| o.frame.as_ref().filter(|(n, _)| n == "case:ledctl:lid").map(|(_, f)| f))
        .expect("the lid names its board frame");
    let to_plate = |p: [f64; 3]| apply(&e.model_to_plate[lid], apply(&frame, p));
    let solids = case_solids(&e, "ledctl");
    let shell = solids.iter().find(|s| s.name != "Base").expect("the checks find the lid");
    let top = shell.positions.iter().map(|p| p[2]).fold(f64::MIN, f64::max);
    let lid_at = |p: [f64; 3]| inside(&parts[lid].mesh, to_plate(p));
    let program = &compiled.instances[0].script.program;
    let leds: Vec<_> = mech.ports.iter().filter(|p| p.kind == BodyPortKind::Light).collect();
    assert_eq!(leds.len(), 8, "ledctl has eight LEDs facing up");
    for port in &leds {
        let key = feature_key(&FeatureRef::Port(CompRef { board: mech.unit, comp: port.comp }, port.index)).expect("an LED port has a feature key");
        let mine: Vec<usize> = pipes.iter().copied().filter(|&i| {
            let (_, solid) = split_id(&parts[i].object).expect("a part id names its solid");
            program.solids.iter().any(|s| s.id == solid && s.link.as_deref() == Some(key.as_str()))
        }).collect();
        assert_eq!(mine.len(), 1, "{}'s pipe is linked to its feature", port.path);
        let pipe = mine[0];
        assert_eq!(parts[pipe].name, format!("Light pipe {}", port.refdes), "a pipe reads by the LED it serves");
        let pipe_at = |p: [f64; 3]| inside(&parts[pipe].mesh, to_plate(p));
        let [x, y, z] = port.at.map(to_mm);
        let entry = z + 0.4;
        let mid = (entry + top) / 2.0;
        let path = &port.path;
        assert!(!pipe_at([x, y, entry - 0.2]) && !lid_at([x, y, entry - 0.2]), "{path}: a 0.4mm gap over the LED");
        assert!(pipe_at([x, y, entry + 0.2]) && !lid_at([x, y, entry + 0.2]), "{path}: the pipe starts 0.4mm over the LED, in its channel");
        assert!(pipe_at([x, y, top - 0.1]) && !lid_at([x, y, top - 0.1]), "{path}: the pipe reaches the lid's top face");
        assert!(!pipe_at([x, y, top + 0.1]), "{path}: the tip is flush with the lid's top");
        assert!(pipe_at([x + 0.5, y, mid]) && !pipe_at([x + 0.7, y, mid]), "{path}: the channel is 1.2mm along the row");
        assert!(pipe_at([x, y + 0.9, mid]) && !pipe_at([x, y + 1.1, mid]), "{path}: the channel is 2mm across it");
        assert!(lid_at([x, y + 1.6, entry + 0.2]) && !lid_at([x, y + 2.4, entry + 0.2]), "{path}: the block's wall is 1.2mm round the channel");
        if leds.iter().any(|q| (to_mm(q.at[0]) - x - 2.0).abs() < 1e-6) {
            assert!(lid_at([x + 0.65, y, mid]) && lid_at([x + 1.35, y, mid]), "{path}: 0.8mm of lid between its channel and the next one");
        }
        let (name, exit) = e.objects.iter().find(|o| o.name == parts[pipe].object)
            .and_then(|o| o.frame.as_ref()).expect("the pipe names its tip's frame");
        assert_eq!(name, "exit");
        let m2p = &e.model_to_plate[pipe];
        let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3);
        assert!(near(apply(m2p, apply(exit, [0.0, 0.0, 0.0])), to_plate([x, y, top])), "{path}: the exit frame sits on the tip");
        assert!(near(apply(m2p, apply(exit, [0.0, 0.0, 1.0])), to_plate([x, y, top + 1.0])), "{path}: the exit frame's Z leaves the lid");
    }

    // The board space the block takes, for the board to reserve: one record, on the lid alone,
    // naming the board and the block's LEDs, over every channel and its 1.2mm wall, from the
    // block's underside up to the roof's, measured from the board's top surface.
    assert!(pipes.iter().all(|&i| parts[i].keepouts.is_empty()), "the pipes keep no board space of their own");
    let [block] = parts[lid].keepouts.as_slice() else { panic!("the lid keeps one block's board space: {:?}", parts[lid].keepouts) };
    assert_eq!(block.board, "ledctl", "the record names its board");
    assert!(leds.iter().all(|port| block.name.split(' ').any(|word| word == port.refdes)), "the record names the block by its LEDs: {}", block.name);
    let xs: Vec<f64> = leds.iter().map(|port| to_mm(port.at[0])).collect();
    let (x0, x1) = (xs.iter().copied().fold(f64::MAX, f64::min), xs.iter().copied().fold(f64::MIN, f64::max));
    let [_, y, z] = leds[0].at.map(to_mm);
    let lo = [0, 1].map(|k| block.area.iter().map(|p| p[k]).fold(f64::MAX, f64::min));
    let hi = [0, 1].map(|k| block.area.iter().map(|p| p[k]).fold(f64::MIN, f64::max));
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    assert!(block.area.len() == 4 && near(lo[0], x0 - 1.8) && near(hi[0], x1 + 1.8) && near(lo[1], y - 2.2) && near(hi[1], y + 2.2),
        "the area is the block's plan, its channels and their walls: {lo:?}..{hi:?} for LEDs at x {x0}..{x1}, y {y}");
    let surface = to_mm(pcb.stackup.thickness);
    assert!(near(surface + block.z[0], z + 0.4), "the band starts at the block's underside, 0.4mm over the LEDs: {:?}", block.z);
    assert!(lid_at([x0 - 1.5, y, surface + block.z[0] + 0.1]) && !lid_at([x0 - 1.5, y, surface + block.z[0] - 0.1]), "the block's wall starts at the band's foot");
    assert!(lid_at([x0 - 3.0, y, surface + block.z[1] + 0.1]) && !lid_at([x0 - 3.0, y, surface + block.z[1] - 0.1]), "beside the block, the band's top is the roof's underside");

    // The checks: the piped case passes; a tall part under the block is found.
    let quiet = |found: &[CaseFinding]| found.iter().filter(|f| f.level != Level::Info).map(|f| f.message.clone()).collect::<Vec<_>>();
    assert!(quiet(&case_checks(&mech, &solids, &CheckOpts::default())).is_empty(), "the piped case passes its checks");

    // The standoffs hold heat-set inserts (the default fastening): one in each, down into the base
    // from the standoff's top under the board, the standoff as wide and tall as it wants round
    // and under the hole, so the build warns of nothing.
    let melts: Vec<_> = e.plate.excluded.iter().filter_map(|p| p.melt()).collect();
    assert_eq!(melts.len(), mech.holes.len(), "an insert in every standoff");
    assert!(!e.warnings.iter().any(|w| w.starts_with("'M3 insert")), "the standoffs hold their inserts: {:?}", e.warnings);
    let base = e.plate.parts.iter().find(|p| p.name == "Base").expect("the base");
    for m in &melts {
        assert!(m.size == "M3" && m.axis[2] < -0.999, "an M3 insert down into the base: {m:?}");
        assert!(e.plate.excluded.iter().any(|p| p.insert.as_ref().is_some_and(|i| i.into == base.object)), "it rides in the base");
        assert!(matches!(m.seat(&base.mesh), fab_plate::melt::Seat::Held { wall, floor } if wall >= m.wall - 0.05 && floor >= m.floor - 0.05), "its standoff holds it: {:?}", m.seat(&base.mesh));
    }
    let led = leds[0];
    let [x, y] = [led.at[0], led.at[1]];
    let mut tall = mech.clone();
    tall.bodies.push(BodyBox {
        comp: led.comp, path: "tall".into(), side: Side::Top,
        min: P { x: x - mm(0.5), y: y + mm(1.3) }, max: P { x: x + mm(0.5), y: y + mm(1.8) },
        height: mm(3.0), known: true, source: None, pose: Xform3::IDENTITY,
    });
    let found = case_checks(&tall, &solids, &CheckOpts::default());
    assert!(found.iter().any(|f| f.check == CaseCheck::Fit && f.level != Level::Info && f.message.starts_with("tall ")),
        "a 3mm part under the pipes' block is found: {:?}", quiet(&found));

    // A copy of the script (a fork) keeps the records.
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "copy", head, &[Change::Copy { instance: "case".into(), at: Some([150.0, 60.0]) }], None)
        .unwrap_or_else(|e| panic!("the case copies: {e}"));
    let (_, e) = evaluated.expect("a fresh apply evaluates");
    assert_eq!(e.plate.parts.iter().filter(|p| p.optics == estimate).count(), 16, "the copy's pipes keep their optics");
}

const SEATS: &str = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [30mm 12mm 20mm] at: [0, 6mm, 0]})
let a = heat_insert("M3", {at: [-6mm, 12mm, 0]})
let b = heat_insert(3mm, {at: [6mm, 12mm, 0] taper: 0.2mm lead_in: 0})
object(heat_seat(block, [a b]), name: "Block")
let c = heat_insert("M3", {at: [0, 2mm, 0]})
object(heat_seat(m.box({size: [30mm 2mm 20mm] at: [0, 1mm, 0]}), [c]), name: "Panel")
let d = heat_insert("M3", {at: [0, 2mm, 0] boss: false})
let e = heat_insert("M3", {at: [13.5mm, 2mm, 0] boss: false})
object(heat_seat(m.box({size: [30mm 2mm 20mm] at: [0, 1mm, 0]}), [d e]), name: "Bare")
let f = heat_insert("M3", {at: [0, 10mm, 0]})
object(m.box({size: [10mm 10mm 10mm] at: [0, 5mm, 0]}), name: "Solid")
insert(a.part, name: "A", into: "Block", melt: a)
insert(b.part, name: "B", into: "Block", melt: b)
insert(c.part, name: "C", into: "Panel", melt: c)
insert(d.part, name: "D", into: "Bare", melt: d)
insert(e.part, name: "E", into: "Bare", melt: e)
insert(f.part, name: "F", into: "Solid", melt: f)
"#;

/// A heat-set insert's seat: the hole across, deep, tapered and chamfered at its mouth as asked,
/// the floor under it, a boss behind a thin panel; and the warnings where the part has no hole for
/// one, too little wall round it or nothing under it.
#[test]
fn heat_set_inserts_seat_in_their_holes_and_warn_where_the_part_holds_them_thinly() {
    let mut model = FabModel::with_script("seats", "seats.cad.splash", SEATS);
    model.set_env(CaseInputs::default().env());
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str| Change::Place { instance: "seats".into(), object: object.into(), arg: PlaceArg::At(Some([150.0, 150.0])) };
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "seats", head, &[place("Block")], None).unwrap_or_else(|e| panic!("the seats build: {e}"));
    let (_, e) = evaluated.expect("a fresh apply evaluates");
    let part = |n: &str| e.plate.parts.iter().find(|p| p.name == n).unwrap_or_else(|| panic!("{n} on the plate"));
    let melt = |n: &str| e.plate.excluded.iter().find(|p| p.part.name == n).and_then(|p| p.melt()).unwrap_or_else(|| panic!("{n} is a melted-in insert"));

    // The warnings: each fault, and nothing on the seats that hold.
    let says = |who: &str, what: &str| e.warnings.iter().any(|w| w.starts_with(&format!("'{who}")) && w.contains(what));
    for who in ["A", "B", "C"] {
        assert!(!e.warnings.iter().any(|w| w.starts_with(&format!("'{who}'"))), "{who} sits in a part that holds it: {:?}", e.warnings);
    }
    assert!(says("D''s hole runs through", "'Bare'"), "a hole through a bare 2 mm panel has nothing under it: {:?}", e.warnings);
    assert!(says("E' has", "round its hole; an M3 insert wants 2.0 mm"), "a hole at the panel's edge has too little wall: {:?}", e.warnings);
    assert!(says("F' has no hole", "'Solid'"), "an insert with no seat says so: {:?}", e.warnings);

    // The hole, in the plate's frame: down from the block's top face.
    let (a, b) = (melt("A"), melt("B"));
    assert_eq!((a.size.as_str(), a.hole, a.depth, a.wall, a.floor), ("M3", 4.0, 5.0, 2.0, 1.2), "the M3 size's own hole");
    assert!(a.axis[2] < -0.999, "the insert goes down into the part: {:?}", a.axis);
    let block = &part("Block").mesh;
    let (u, w) = ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    let at = |m: &fab_plate::melt::Melt, t: f64, r: f64, d: [f64; 3]| std::array::from_fn(|k| m.mouth[k] + m.axis[k] * t + d[k] * r);
    let solid = |p: [f64; 3]| inside(block, p);
    for d in [u, w] {
        assert!(!solid(at(&a, 2.5, 1.9, d)) && solid(at(&a, 2.5, 2.1, d)), "the hole is 4 mm across");
        assert!(!solid(at(&a, 0.1, 2.25, d)) && solid(at(&a, 0.1, 2.45, d)), "a 0.4 mm chamfer round the mouth");
        assert!(solid(at(&b, 4.9, 1.96, d)) && !solid(at(&b, 0.2, 1.96, d)), "the tapered hole narrows by 0.2 mm to its bottom");
        assert!(solid(at(&b, 0.1, 2.1, d)), "no chamfer when lead_in is 0");
    }
    assert!(!solid(at(&a, 4.9, 0.0, u)) && solid(at(&a, 5.1, 0.0, u)), "the hole is 5 mm deep, the block under it");

    // The panel grows a boss under its 2 mm: the hole's wall and floor round and under the hole.
    let (c, panel) = (melt("C"), &part("Panel").mesh);
    assert!(inside(panel, at(&c, 3.5, 3.9, u)) && !inside(panel, at(&c, 3.5, 4.1, u)), "a boss 2 mm round the hole below the panel");
    assert!(inside(panel, at(&c, 6.1, 0.0, u)) && !inside(panel, at(&c, 6.3, 0.0, u)), "and 1.2 mm under the hole");
    assert!(!inside(&part("Bare").mesh, at(&melt("D"), 3.5, 3.0, u)), "boss: false grows none");

    // The insert itself: brass, as long as its size, its knurl as wide, standing in its hole.
    let ins = e.plate.excluded.iter().find(|p| p.part.name == "A").expect("A");
    let bb = ins.part.mesh.bounds();
    assert!((bb.max[2] - a.mouth[2]).abs() < 0.02 && (a.mouth[2] - bb.min[2] - 4.0).abs() < 0.02, "4 mm long from the face down: {bb:?}");
    assert!((bb.max[0] - bb.min[0] - 4.6).abs() < 0.05, "4.6 mm across its knurl: {bb:?}");
    assert_eq!(ins.part.mesh.open_edges(), 0, "the insert is a closed solid");
}

const MIXED: &str = r#"use mod.model.*
use mod.fab.*
let pcb = board("ledctl")
let case = enclosure(pcb, {holes: [{name: "mount2" insert: @self_tap}]})
object(case.base, name: "Base")
for i in case.inserts {
    insert(i.part, name: i.name, into: "Base", melt: i)
}
"#;

/// A board with holes of two sizes gets each its own fixing from the shared screw table
/// (`m.screw`), by its drill: ledctl with mount2 drilled 2.2 mm (an M2) and self-tapped there,
/// mount1 at 3.2 mm (an M3) on its default heat-set insert.
#[test]
fn each_hole_takes_its_screw_insert_and_standoff_from_the_screw_table() {
    let mut design = ledctl();
    let unit = &mut design.units[0];
    let board = std::sync::Arc::make_mut(unit.board.as_mut().expect("the fixture has a board"));
    board.holes.iter_mut().find(|h| h.name == "mount2").expect("ledctl has mount2").drill = mm(2.2);
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let mut model = FabModel::with_script("case", "case.cad.splash", MIXED);
    model.set_env(inputs.env());
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str, arg| Change::Place { instance: "case".into(), object: object.into(), arg };
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "case", head, &[place("Base", PlaceArg::At(Some([150.0, 150.0])))], None)
        .unwrap_or_else(|e| panic!("the case with mixed holes builds: {e}"));
    let (_, e) = evaluated.expect("a fresh apply evaluates");
    let ix = e.plate.parts.iter().position(|p| p.name == "Base").expect("the base");
    let base = &e.plate.parts[ix];
    // Model mm, board (x, y) at (x - 25, height, 15 - y): mount1 (46, 4), mount2 (46, 26); 4 mm
    // up is within the standoffs (the floor's top 1.6, the board's underside 6.2).
    let solid = |x: f64, z: f64| inside(&base.mesh, apply(&e.model_to_plate[ix], [x, 4.0, z]));

    let melts: Vec<_> = e.plate.excluded.iter().filter_map(|p| p.melt()).collect();
    assert_eq!(melts.iter().map(|m| m.size.as_str()).collect::<Vec<_>>(), ["M3"], "one insert, mount1's M3; mount2 is self-tapped");
    assert!(matches!(melts[0].seat(&base.mesh), fab_plate::melt::Seat::Held { .. }), "mount1's standoff holds its insert: {:?}", melts[0].seat(&base.mesh));
    assert!(solid(21.0 - 3.6, 11.0) && !solid(21.0 - 4.4, 11.0), "mount1's standoff is the table's 8 mm boss round an M3 insert");
    assert!(!solid(21.0 - 0.5, -11.0), "mount2 has the M2's 1.6 mm self-tapping pilot");
    assert!(solid(21.0 - 2.2, -11.0) && !solid(21.0 - 2.8, -11.0), "round it the table's 5 mm boss for a self-tapped M2");
    assert!(!e.warnings.iter().any(|w| w.contains("insert")), "no insert wants more wall or floor: {:?}", e.warnings);

    // Align reads the base back: the standoffs' axes under the board's holes and the board's
    // underside on their tops, from a board set 0.8 mm off.
    let up = [0.0, 0.0, 1.0];
    let bosses = fab_elec_mech::align::find_bosses(&base.mesh.positions, &base.mesh.triangles, up);
    let holes: Vec<[f64; 2]> = pcb.holes.iter().map(|h| [to_mm(h.at.x), to_mm(h.at.y)]).collect();
    let board_at = |p: [f64; 2]| apply(&e.model_to_plate[ix], [p[0] - 25.0, 6.2, 15.0 - p[1]]);
    let o = board_at([0.0, 0.0]);
    let prior = [[1.0, 0.0, 0.0, o[0] + 0.7], [0.0, 1.0, 0.0, o[1] - 0.4], [0.0, 0.0, 1.0, o[2]]];
    let fit = fab_elec_mech::align::fit(&holes, &bosses, up, Some(&prior)).expect("the board's holes and the base's bosses fit");
    for (h, at) in fit.holes.iter().zip(&holes) {
        let p = apply(&fit.pose, [at[0], at[1], 0.0]);
        let want = board_at(*at);
        let d = ((p[0] - want[0]).powi(2) + (p[1] - want[1]).powi(2) + (p[2] - want[2]).powi(2)).sqrt();
        assert!(d < 0.01 && h.residual < 0.01, "the hole at {at:?} lands on its standoff's top, on its axis: {d} mm off, {h:?} of {bosses:?}");
    }
}
