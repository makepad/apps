//! The flagship's committed case on its real board fixture, through the host's bridge.
//! One evaluation checks the fit, the cuts and the slice against the inserted board.

mod common;

use common::{inside, open_at};
use fab_elec_core::{mm, to_mm, BBox};
use fab_elec_design::fixture::sbc;
use fab_elec_mech::bridge::{board_inputs, CaseInputs};
use fab_elec_mech::{board_mech, case_checks, case_solids, CheckOpts, Level};
use fab_elec_parts::BodyPortKind;
use fab_model::eval::Evaluator;
use fab_model::{Change, FabModel, PlaceArg};
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use std::time::Instant;

const CASE: &str = include_str!("../../fixtures/sbc/sbc.cad.splash");

#[test]
fn the_flagship_board_gets_a_clean_case_that_slices_round_it() {
    let started = Instant::now();
    let design = sbc();
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the flagship fixture has a board");
    assert_eq!(pcb.placed.len(), 20, "the flagship fixture has twenty placements");
    let mech = board_mech(pcb, &unit.circuit);
    for (kind, count) in [(BodyPortKind::Plug, 6), (BodyPortKind::Cable, 2), (BodyPortKind::Press, 1), (BodyPortKind::Light, 2)] {
        assert_eq!(mech.ports.iter().filter(|p| p.kind == kind).count(), count, "the flagship has {count} {kind:?} ports");
    }
    assert_eq!(mech.ports.len(), 11, "the flagship has only its six plug, two cable, one press and two light ports");
    let outline = pcb.outline[0].bbox();
    // These courtyards are rectangles at quarter turns: their placed bounds are their areas.
    let courtyards: Vec<_> = pcb.placed.iter().map(|p| {
        let path = &unit.circuit.comps[p.comp.ix()].path;
        assert!(!p.footprint.courtyard.is_empty(), "{path} has a courtyard to check");
        let bounds = BBox::of_points(p.footprint.courtyard.rings().flatten().map(|q| p.to_board(*q)));
        (path, p.side, bounds)
    }).collect();
    for (i, (path, side, bounds)) in courtyards.iter().enumerate() {
        assert!(outline.contains_box(bounds), "{path}'s courtyard stays inside the outline's bounds: {bounds:?} in {outline:?}");
        for (other, other_side, other_bounds) in &courtyards[..i] {
            if side == other_side {
                assert!(!bounds.intersects(other_bounds), "{path} and {other} have disjoint {side:?} courtyards: {bounds:?} and {other_bounds:?}");
            }
        }
    }
    let again = board_mech(pcb, &unit.circuit);
    assert_eq!((again.hash, again.bodies_hash), (mech.hash, mech.bodies_hash), "summarising the same board twice preserves both hashes");
    let soc = unit.circuit.comp("soc.u").expect("the fixture has its processor").id;
    let mut nudged = pcb.clone();
    nudged.placed.iter_mut().find(|p| p.comp == soc).expect("the fixture places its processor").at.x += mm(1.0);
    let nudged = board_mech(&nudged, &unit.circuit);
    assert_ne!(nudged.bodies_hash, mech.bodies_hash, "moving soc.u by 1 mm changes the bodies hash");
    assert_eq!(nudged.hash, mech.hash, "moving soc.u by 1 mm preserves the case-facing hash");

    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let mut model = FabModel::with_script("case", "sbc.cad.splash", CASE);
    model.set_env(inputs.env());
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str, arg| Change::Place { instance: "sbc".into(), object: object.into(), arg };
    // Base and Lid keep the script's own layout, the lid upside down beside the base, with their
    // shared model origin on the bed; the insert follows Base.
    let changes = [place("Base", PlaceArg::At(Some([110.0, 160.0]))), place("Lid", PlaceArg::At(Some([110.0, 160.0])))];
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "sbc-case", head, &changes, None)
        .unwrap_or_else(|err| panic!("the flagship's committed case builds: {err}"));
    let (compiled, e) = evaluated.expect("one fresh apply evaluates the flagship case");
    let solid_count: usize = compiled.instances.iter().map(|i| i.script.program.solids.len()).sum();
    let names: Vec<_> = e.plate.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Base", "Lid"], "only Base and Lid print");
    assert_eq!(e.plate.excluded.len(), 1, "only the inserted Board is excluded from printing");
    for (i, part) in e.plate.parts.iter().enumerate() {
        let open = open_at(&part.mesh, &e.model_to_plate[i]);
        assert!(open.is_empty(), "{} is closed and manifold; its open or shared edges in model mm: {open:?}", part.name);
        assert!(part.mesh.bounds().min[2].abs() < 1e-6, "{} stands on the bed: {:?}", part.name, part.mesh.bounds());
    }
    let print_issues: Vec<_> = e.printability.issues.iter().filter(|i| i.severity != fab_printability::Severity::Info).collect();
    assert!(print_issues.is_empty(), "Base and the upside-down Lid have no printability issue above Info: {print_issues:#?}");

    let base = &e.plate.parts[0];
    let board = e.plate.excluded.iter().find(|p| p.part.name == "Board").expect("Board is excluded from printing");
    let insert = board.insert.as_ref().expect("Board is an insert");
    assert_eq!(insert.into, base.object, "Board is inserted into Base's object");
    assert_eq!(board.link.as_deref(), Some("board:sbc"), "Board is linked to the flagship board");
    let (frame_name, _) = board.frame.as_ref().expect("Board carries its named board frame");
    assert_eq!(frame_name, "board:sbc", "Board's frame is named board:sbc");

    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.2");
    let setup = JobSetup {
        machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1],
        nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(), filament_colours: vec![None],
    };
    let sliced = makepad_fab_slice::plate::slice_plate(&setup, &e.plate.printable())
        .unwrap_or_else(|err| panic!("the flagship case slices: {err}"));
    assert_eq!(sliced.objects.len(), 2, "the slice contains only Base and Lid");
    let mut report = fab_printability::Report::default();
    fab_printability::analyse_slice(&e.machine, &e.plate, &fab_printability::Materials::default(), sliced.layers(), &sliced.timeline, &mut report);
    let slice_issues: Vec<_> = report.issues.iter().filter(|i| i.severity != fab_printability::Severity::Info).collect();
    assert!(slice_issues.is_empty(), "the sliced case has no issue above Info: {slice_issues:#?}");
    let inserts: Vec<_> = e.plate.inserts().into_iter().map(|(part, insert, _)| (part, insert)).collect();
    assert_eq!(inserts.len(), 1, "the bead check receives the Board insert with its printed host");
    // live_slice feeds packed layer records; slice_plate retains the same move records in its timeline.
    let bead_issues = fab_printability::insert::bead_issues(&inserts, &mut sliced.timeline.moves().copied());
    assert!(bead_issues.is_empty(), "the slice's beads clear the inserted board: {bead_issues:#?}");

    let case = case_solids(&e, "sbc");
    assert_eq!(case.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Base", "Lid"], "the case solids are Base and Lid in the board frame");
    let findings = case_checks(&mech, &case, &CheckOpts::default());
    assert!(findings.iter().all(|f| f.level == Level::Info), "every body fits, every port is reachable and every mounting hole is held; no case finding above Info: {findings:#?}");

    // Sample the true wall midpoint, measured from the outline rather than from a mouth
    // 0.3 mm inside it. Board-frame z starts at the board's underside, not at the bed.
    let gap = 0.4;
    let wall = 2.0;
    let roof_mid = to_mm(mech.thickness + mech.envelope[0]) + 1.0 + 1.6 / 2.0;
    for port in &mech.ports {
        let at = port.at.map(to_mm);
        let distance = if port.dir[2] > 0.7 {
            (roof_mid - at[2]) / port.dir[2]
        } else {
            let axis = if port.dir[0].abs() >= port.dir[1].abs() { 0 } else { 1 };
            let lo = [to_mm(outline.min.x), to_mm(outline.min.y)][axis];
            let hi = [to_mm(outline.max.x), to_mm(outline.max.y)][axis];
            let midpoint = if port.dir[axis] > 0.0 { hi + gap + wall / 2.0 } else { lo - gap - wall / 2.0 };
            (midpoint - at[axis]) / port.dir[axis]
        };
        let point = std::array::from_fn(|axis| at[axis] + port.dir[axis] * distance);
        for solid in &case {
            assert!(!inside(solid, point), "{} {:?} mouth has its own cut through {}; board-frame wall or roof midpoint {point:?}", port.path, port.kind, solid.name);
        }
    }
    // The notch is at model [along 6, y 3.675], board [y 22, z -0.925], the card slot's middle.
    // The slot's joint notch opens the wall above that middle, so the dish shows below it: 2 mm
    // under the centre it is 0.69 mm deep (R = 6.625), and 4.5 mm beside it, 1.5 mm under the
    // slot's middle (past the slot's opening), the wall stands. Both 0.5 mm inside the outer face.
    let face = to_mm(outline.min.x) - gap - wall + 0.5;
    let notch = [face, 22.0, -0.925 - 2.0];
    for solid in &case {
        assert!(!inside(solid, notch), "the card finger notch is empty in {} at board {notch:?}", solid.name);
    }
    let beside_notch = [face, 22.0 + 4.5, -0.925 - 1.5];
    assert!(case.iter().any(|s| inside(s, beside_notch)), "the wall stands beside the card finger notch's rim at board {beside_notch:?}");
    // The 14 mm field over board (34, 30) holds four 1.8 mm slots on a 3.6 mm pitch:
    // x = 28.6, 32.2, 35.8, 39.4. The processor's centre is between the middle pair.
    let vent_slot = [32.2, 30.0, roof_mid];
    for solid in &case {
        assert!(!inside(solid, vent_slot), "the processor vent slot is empty in {} at board {vent_slot:?}", solid.name);
    }
    let between_slots = [34.0, 30.0, roof_mid];
    assert!(case.iter().any(|s| inside(s, between_slots)), "the roof stands between the processor vent slots at board {between_slots:?}");
    eprintln!("sbc case: one evaluation and all checks, {solid_count} compiled solids, elapsed {:?}", started.elapsed());
}
