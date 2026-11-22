use fab_elec_core::{mm, to_mm, Angle, BBox, CompRef, FeatureRef, PolySet, Xform3, P};
use fab_elec_fixtures::ledctl;
use fab_elec_mech::bridge::{board_inputs, body_generator, CaseInputs, ELEC_BODY};
use fab_elec_mech::keys::feature_key;
use fab_elec_mech::{board_mech, case_checks, case_solids, CaseCheck, CaseSolid, CheckOpts, Level};
use fab_elec_parts::{BodyPortKind, BodySource};
use fab_model::eval::Evaluator;
use fab_model::program::{Compiled, ScriptEnv};
use fab_model::{Change, FabModel, PlaceArg};
use fab_plate::machine::Machine;
use makepad_csg_math::dvec3;
use makepad_csg_mesh::bvh::Bvh;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const CASE: &str = r#"use mod.model.*
use mod.fab.*
let pcb = board("ledctl")
let case = enclosure(pcb, {})
object(case.base, name: "Base")
object(case.lid_on, name: "LidOn")
object(board_solid(pcb, board_bodies("ledctl"), case.frame), name: "Board")
"#;

fn build_case(
    model: &mut FabModel,
    ev: &mut Evaluator,
    env: ScriptEnv,
    request: &str,
    changes: &[Change],
) -> (Vec<CaseSolid>, Compiled) {
    model.set_env(env);
    // Placements are edits (applied once); a new env alone re-evaluates the same document.
    let (compiled, e) = if changes.is_empty() {
        model.evaluate(ev, None).unwrap_or_else(|e| panic!("the case script builds: {e}"))
    } else {
        let head = model.head();
        let (_, evaluated) = model.apply(ev, request, head, changes, None)
            .unwrap_or_else(|e| panic!("the case script builds: {e}"));
        evaluated.expect("a fresh apply evaluates")
    };
    let case = case_solids(&e, "ledctl");
    assert_eq!(case.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Base", "LidOn"], "case_solids finds the base and the lid in place by their board frame");
    (case, compiled)
}

#[test]
fn the_ledctl_case_passes_its_checks_and_a_moved_part_fails_them() {
    let design = ledctl();
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mech = board_mech(pcb, &unit.circuit);
    let mut inputs = CaseInputs::default();
    let first_inputs = board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills());
    let first_board_hash = first_inputs.board.hash;
    inputs.update(first_inputs);
    let env = inputs.env();
    let mut model = FabModel::with_script("case", "case.cad.splash", CASE);
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str, arg| Change::Place { instance: "case".into(), object: object.into(), arg };
    let changes = [
        place("Base", PlaceArg::At(Some([150.0, 150.0]))),
        place("LidOn", PlaceArg::At(Some([150.0, 150.0]))),
        place("LidOn", PlaceArg::Drop(false)),
        place("LidOn", PlaceArg::Print(false)),
        place("Board", PlaceArg::At(Some([150.0, 150.0]))),
        place("Board", PlaceArg::Drop(false)),
        place("Board", PlaceArg::Print(false)),
    ];
    let (case, compiled) = build_case(&mut model, &mut ev, env, "case", &changes);

    let findings = case_checks(&mech, &case, &CheckOpts::default());
    assert!(findings.iter().all(|f| f.level == Level::Info), "the assembled case has no errors or warnings: {findings:#?}");
    assert_eq!(mech.holes.len(), 2, "the fixture supplies both mounting holes to the checks");
    for hole in &mech.holes {
        assert!(!findings.iter().any(|f| f.check == CaseCheck::Hold && f.level == Level::Warning && f.feature == hole.feature), "{} is held from below: {findings:#?}", hole.name);
    }
    let usb = unit.circuit.comp("usb.j").expect("the fixture has its receptacle").id;
    let plug = mech.ports.iter().find(|p| p.comp == usb && p.kind == BodyPortKind::Plug).expect("the receptacle declares its plug port");
    let port_feature = FeatureRef::Port(CompRef { board: mech.unit, comp: usb }, plug.index);
    assert!(!findings.iter().any(|f| f.check == CaseCheck::Reach && f.feature == port_feature), "the receptacle's plug path is clear and the wall is within reach: {findings:#?}");

    // Change the circuit body only: the already-built case and all its meshes stay fixed.
    let mut taller = (*unit.circuit).clone();
    let mut part = (*taller.comps[usb.ix()].part).clone();
    part.body.as_mut().expect("the receptacle has a body").height = mm(9.0);
    taller.comps[usb.ix()].part = Arc::new(part);
    let moved = case_checks(&board_mech(pcb, &taller), &case, &CheckOpts::default());
    let body_feature = FeatureRef::Body(CompRef { board: mech.unit, comp: usb });
    assert!(moved.iter().any(|f| f.check == CaseCheck::Fit && f.level == Level::Error && f.feature == body_feature && f.message.contains("LidOn")), "raising the receptacle's body runs into the assembled lid: {moved:#?}");
    let fit = moved.iter().find(|f| f.check == CaseCheck::Fit && f.level == Level::Error && f.feature == body_feature).expect("the taller receptacle has a fit error");
    assert!(fit.message.contains("into the LidOn above it"), "the fit error identifies the roof above the receptacle: {fit:#?}");
    let raise: f64 = fit.hint.strip_prefix("Raise the roof by ").and_then(|s| s.split_once(" mm")).expect("the fit hint gives the roof adjustment in mm").0.parse().expect("the roof adjustment is a number");
    let envelope_top = to_mm(mech.envelope[0]);
    assert!((raise - (9.0 - envelope_top - 1.0 + 0.2)).abs() <= 0.02, "the roof adjustment clears the taller receptacle with 0.2 mm to spare: {fit:#?}");

    let mut closer = (*unit.circuit).clone();
    let mut part = (*closer.comps[usb.ix()].part).clone();
    part.body.as_mut().expect("the receptacle has a body").height = mm(envelope_top + 1.0 - 0.1);
    closer.comps[usb.ix()].part = Arc::new(part);
    let near = case_checks(&board_mech(pcb, &closer), &case, &CheckOpts::default());
    let clearance = near.iter().find(|f| f.check == CaseCheck::Fit && f.level == Level::Warning && f.feature == body_feature).expect("the receptacle 0.1 mm below the roof has a fit warning");
    assert!(clearance.message.contains("0.1 mm from the LidOn above it"), "the clearance warning measures the gap above the receptacle: {clearance:#?}");
    assert!(clearance.hint.contains("Raise the roof by 0.1 mm"), "the clearance hint gives the missing clearance: {clearance:#?}");

    let unsupported = case_checks(&mech, &case[1..], &CheckOpts::default());
    let holes: Vec<_> = unsupported.iter().filter(|f| f.check == CaseCheck::Hold && f.level == Level::Warning).map(|f| &f.feature).collect();
    let expected: Vec<_> = mech.holes.iter().map(|h| &h.feature).collect();
    assert_eq!(holes, expected, "removing the base leaves both mounting holes unsupported: {unsupported:#?}");
    assert!(unsupported.iter().filter(|f| f.check == CaseCheck::Hold && f.level == Level::Warning).all(|f| f.message.ends_with("nothing is under it within 20 mm")), "each unsupported hole reports the empty search below it: {unsupported:#?}");
    let opts = CheckOpts { free: vec!["mount1".into()], ..Default::default() };
    let free = case_checks(&mech, &case[1..], &opts);
    let holes: Vec<_> = free.iter().filter(|f| f.check == CaseCheck::Hold && f.level == Level::Warning).map(|f| &f.feature).collect();
    let mount2 = &mech.holes.iter().find(|h| h.name == "mount2").expect("the fixture has mount2").feature;
    assert_eq!(holes, vec![mount2], "a free mount1 leaves only mount2's missing-support warning: {free:#?}");

    let has_link = |feature: &FeatureRef| {
        let key = feature_key(feature).expect("a derived case feature has a host key");
        compiled.instances.iter().any(|i| i.script.program.solids.iter().any(|s| s.link.as_deref() == Some(key.as_str())))
    };
    assert!(has_link(&port_feature), "the plug's cutter retains its board feature link in the compiled program");
    for hole in &mech.holes {
        assert!(has_link(&hole.feature), "the support for {} retains its board feature link in the compiled program", hole.name);
    }

    // Slide 3 mm along the left wall: the body stays within the board and clear of the
    // resistors at x = 10 mm, while the plug extends beyond the old opening's 0.3 mm margin.
    let mut moved_board = pcb.clone();
    moved_board.placed.iter_mut().find(|p| p.comp == usb).expect("the fixture places its receptacle").at.y += mm(3.0);
    let moved_mech = board_mech(&moved_board, &unit.circuit);
    let moved_inputs = board_inputs(&unit.name, &moved_board, &unit.circuit, &moved_board.drills());
    assert_ne!(moved_inputs.board.hash, first_board_hash, "moving the connector changes the board input hash so the case rebuilds");
    let blocked = case_checks(&moved_mech, &case, &CheckOpts::default());
    assert!(blocked.iter().any(|f| f.check == CaseCheck::Reach && f.level == Level::Error && f.feature == port_feature), "the old case wall blocks the moved plug: {blocked:#?}");

    inputs.update(moved_inputs);
    let env = inputs.env();
    let (moved_case, _) = build_case(&mut model, &mut ev, env, "moved-case", &[]);
    let followed = case_checks(&moved_mech, &moved_case, &CheckOpts::default());
    assert!(followed.iter().all(|f| f.level == Level::Info), "the rebuilt case has no errors or warnings after the connector moves: {followed:#?}");
    assert!(!followed.iter().any(|f| f.check == CaseCheck::Reach && f.feature == port_feature), "the rebuilt cutout follows the connector and clears its plug path: {followed:#?}");
}

fn port_check_case(pcb: &fab_elec_board::Board, circuit: &fab_elec_circuit::Circuit, script: &str) -> Vec<CaseSolid> {
    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs("ledctl", pcb, circuit, &pcb.drills()));
    let env = inputs.env();
    let mut model = FabModel::with_script("case", "case.cad.splash", script);
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str, arg| Change::Place { instance: "case".into(), object: object.into(), arg };
    let changes = [
        place("Base", PlaceArg::At(Some([150.0, 150.0]))),
        place("LidOn", PlaceArg::At(Some([150.0, 150.0]))),
        place("LidOn", PlaceArg::Drop(false)),
        place("LidOn", PlaceArg::Print(false)),
        place("Board", PlaceArg::At(Some([150.0, 150.0]))),
        place("Board", PlaceArg::Drop(false)),
        place("Board", PlaceArg::Print(false)),
    ];
    build_case(&mut model, &mut ev, env, "port-check-case", &changes).0
}

#[test]
fn a_port_sweep_names_its_blocker_and_measures_the_cutout_widening() {
    let design = ledctl();
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mech = board_mech(pcb, &unit.circuit);
    let case = port_check_case(pcb, &unit.circuit, CASE);
    let opts = CheckOpts::default();
    let findings = case_checks(&mech, &case, &opts);
    assert!(!findings.iter().any(|f| matches!(f.check, CaseCheck::Reach | CaseCheck::Depth) && matches!(f.feature, FeatureRef::Port(..))), "every assembled port is clear and within reach: {findings:#?}");

    let usb = unit.circuit.comp("usb.j").expect("the fixture has its receptacle").id;
    let plug = mech.ports.iter().find(|p| p.comp == usb && p.kind == BodyPortKind::Plug).expect("the receptacle declares its plug port");
    let port_feature = FeatureRef::Port(CompRef { board: mech.unit, comp: usb }, plug.index);
    let on_port = |f: &&fab_elec_mech::CaseFinding| f.check == CaseCheck::Reach && f.feature == port_feature;

    // Reuse the original case: moving the receptacle along the wall leaves a partial opening.
    let mut shifted = pcb.clone();
    shifted.placed.iter_mut().find(|p| p.comp == usb).expect("the fixture places its receptacle").at.y += mm(3.0);
    let findings = case_checks(&board_mech(&shifted, &unit.circuit), &case, &opts);
    let reach: Vec<_> = findings.iter().filter(on_port).collect();
    assert_eq!(reach.len(), 1, "the shifted plug has exactly one blocking finding: {findings:#?}");
    assert_eq!(reach[0].level, Level::Error, "a blocked plug is an error");
    let widening: f64 = reach[0].hint.strip_prefix("Widen its cutout by ")
        .and_then(|hint| hint.strip_suffix(" mm, or move usb.j to its opening."))
        .unwrap_or_else(|| panic!("a partial opening gives a widening amount: {}", reach[0].hint))
        .parse().expect("the widening amount is a number");
    assert!((widening - 3.0).abs() <= 0.1, "a 3 mm shift needs 3 mm of widening, got {widening}: {}", reach[0].hint);

    // Put a small body in front of the mouth, inside the profile rather than on its rim.
    let cc = unit.circuit.comp("usb.cc[0]").expect("the fixture has its configuration resistor").id;
    let mut obstructed = pcb.clone();
    obstructed.placed.iter_mut().find(|p| p.comp == cc).expect("the fixture places its configuration resistor").at = fab_elec_core::P::mm(-1.0, 15.0);
    let findings = case_checks(&board_mech(&obstructed, &unit.circuit), &case, &opts);
    let reach: Vec<_> = findings.iter().filter(on_port).collect();
    assert_eq!(reach.len(), 1, "the body in the plug path has exactly one blocking finding: {findings:#?}");
    assert_eq!(reach[0].level, Level::Error, "another body blocking the plug is an error");
    assert!(reach[0].message.contains("is blocked by usb.cc[0] "), "the nearest blocker is named: {}", reach[0].message);
    assert!(reach[0].hint.starts_with("Move usb.cc[0]"), "the hint identifies the body to move: {}", reach[0].hint);

    const NO_PORTS: &str = r#"use mod.model.*
use mod.fab.*
let pcb = board("ledctl")
let case = enclosure(pcb, {ports: []})
object(case.base, name: "Base")
object(case.lid_on, name: "LidOn")
object(board_solid(pcb, board_bodies("ledctl"), case.frame), name: "Board")
"#;
    let closed_case = port_check_case(pcb, &unit.circuit, NO_PORTS);
    let findings = case_checks(&mech, &closed_case, &opts);
    let reach: Vec<_> = findings.iter().filter(on_port).collect();
    assert_eq!(reach.len(), 1, "a closed wall has exactly one blocking finding: {findings:#?}");
    assert_eq!(reach[0].level, Level::Error, "a closed wall blocking the plug is an error");
    assert!(reach[0].message.starts_with("Nothing opens in front of usb.j"), "a missing opening is distinct from a narrow one: {}", reach[0].message);
    assert_eq!(reach[0].hint, "Give usb.j a cutout (the case's ports), or move it to a wall that has one.");
}

fn pyramid(closed: bool) -> Vec<u8> {
    use fab_elec_import::{glb::write_glb, Material, MeshBuilder, Normals};
    // A square pyramid in the body frame (mm): base 2 x 2 at z = 0, apex at z = 2. Its box is the
    // whole 2 x 2 x 2 cube; near the box's top corners the pyramid is far away.
    let mut b = MeshBuilder::new();
    let grey = b.add_material(Material::named("body"));
    let (apex, base) = ([0.0, 0.0, 2.0], [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]]);
    let crease = Normals::Crease { domain: 0, angle: 0.5 };
    if closed {
        assert!(b.polygon(grey, &[base[3], base[2], base[1], base[0]], crease));
    }
    for i in 0..4 {
        assert!(b.polygon(grey, &[base[i], base[(i + 1) % 4], apex], crease));
    }
    write_glb(&b.finish())
}

#[test]
fn a_mesh_body_is_fitted_by_its_hull_not_its_box() {
    let dir = std::env::temp_dir().join(format!("fab-elec-mech-hull-fit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let glb = dir.join("pyramid.glb");
    std::fs::write(&glb, pyramid(true)).expect("write the mesh");

    let design = ledctl();
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mut mech = board_mech(pcb, &unit.circuit);
    let body = &mut mech.bodies[0];
    body.source = Some(BodySource::Mesh(glb.display().to_string()));
    let (pose, feature) = (body.pose, FeatureRef::Body(CompRef { board: mech.unit, comp: body.comp }));
    // A small closed tetrahedron of case around a body-frame point, in the board frame.
    let probe = |at: [f64; 3]| {
        let p = |d: [f64; 3]| pose.apply_f([(at[0] + d[0]) * 1e6, (at[1] + d[1]) * 1e6, (at[2] + d[2]) * 1e6]).map(|v| v * 1e-6);
        CaseSolid {
            name: "Probe".into(),
            positions: vec![p([0.0, 0.0, 0.0]), p([0.05, 0.0, 0.0]), p([0.0, 0.05, 0.0]), p([0.0, 0.0, 0.05])],
            triangles: vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]],
        }
    };
    let fits = |at| case_checks(&mech, &[probe(at)], &CheckOpts::default()).into_iter().filter(|f| f.check == CaseCheck::Fit && f.feature == feature).collect::<Vec<_>>();
    assert!(fits([0.9, 0.9, 1.8]).is_empty(), "inside the box but 0.7 mm off the pyramid: no fault: {:#?}", fits([0.9, 0.9, 1.8]));
    let inside = fits([0.2, 0.0, 0.8]);
    assert!(inside.iter().any(|f| f.level == Level::Error && f.message.contains("Probe")), "inside the pyramid: a fit error: {inside:#?}");
    let near = fits([0.0, 0.0, 2.1]);
    assert!(near.iter().any(|f| f.level == Level::Warning), "0.1 mm above the apex: a clearance warning: {near:#?}");
    // A lip of case over the pyramid's slant (body frame x 0.1..0.3, y -0.1..0.1, z 1.7..3): at z 1.7
    // the slant is 0.15 out, so the lip sits 0.05 mm into it on the +x side (moving down would take
    // 0.1 mm). Its box would read 0.3 mm (the box's top, 2, less the lip's underside).
    let lip = {
        let p = |x: f64, y: f64, z: f64| pose.apply_f([x * 1e6, y * 1e6, z * 1e6]).map(|v| v * 1e-6);
        let mut positions = Vec::new();
        for z in [1.7, 3.0] {
            for (x, y) in [(0.1, -0.1), (0.3, -0.1), (0.3, 0.1), (0.1, 0.1)] {
                positions.push(p(x, y, z));
            }
        }
        CaseSolid {
            name: "Lip".into(),
            positions,
            triangles: vec![[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
                [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]],
        }
    };
    let into: Vec<_> = case_checks(&mech, &[lip], &CheckOpts::default()).into_iter().filter(|f| f.check == CaseCheck::Fit && f.feature == feature).collect();
    assert!(into.iter().any(|f| f.level == Level::Error && f.message.contains("runs 0.05 mm into the Lip")),
        "a contact's depth is measured against the hull's slant, not its box: {into:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 2 x 3 pin header at 2.54 mm as a part library's model has it, in its own frame (centred on
/// its pin field, its three pairs of pins along x, the board's top at z = 0): the plastic spacer
/// 2.5 mm tall round the pins, and the six 0.64 mm square pins from 3 mm under the board to 8.5 mm
/// over it.
fn header_2x3() -> Vec<u8> {
    use fab_elec_import::{glb::write_glb, Material, MeshBuilder, Normals};
    let mut b = MeshBuilder::new();
    let plastic = b.add_material(Material::named("body"));
    let flat = Normals::Crease { domain: 0, angle: 0.5 };
    let mut cuboid = |lo: [f32; 3], hi: [f32; 3]| {
        let c = |x: usize, y: usize, z: usize| [[lo[0], hi[0]][x], [lo[1], hi[1]][y], [lo[2], hi[2]][z]];
        for face in [
            [c(0, 0, 0), c(0, 1, 0), c(1, 1, 0), c(1, 0, 0)], [c(0, 0, 1), c(1, 0, 1), c(1, 1, 1), c(0, 1, 1)],
            [c(0, 0, 0), c(1, 0, 0), c(1, 0, 1), c(0, 0, 1)], [c(0, 1, 0), c(0, 1, 1), c(1, 1, 1), c(1, 1, 0)],
            [c(0, 0, 0), c(0, 0, 1), c(0, 1, 1), c(0, 1, 0)], [c(1, 0, 0), c(1, 1, 0), c(1, 1, 1), c(1, 0, 1)],
        ] {
            assert!(b.polygon(plastic, &face, flat));
        }
    };
    cuboid([-3.81, -2.54, 0.0], [3.81, 2.54, 2.5]);
    for row in 0..3 {
        for col in 0..2 {
            let (x, y) = (row as f32 * 2.54 - 2.54, col as f32 * 2.54 - 1.27);
            cuboid([x - 0.32, y - 0.32, -3.0], [x + 0.32, y + 0.32, 8.5]);
        }
    }
    write_glb(&b.finish())
}

/// Why: a pin header plugged from above passes up through the lid. With its model on, its posts
/// stand 8.5 mm over the board, and the case cut its opening from the header's port, which the std
/// two-row header (`header2`) sizes to its pin field (0.5 mm round the posts). The insulator is
/// wider than that: the model's hull met the lid round the opening (11-17), and the mating plug's
/// housing, the header's size, could not go on with the case closed. The opening is now the
/// header's whole plan grown by its margin, whatever the port's profile: the LED chaser's case with
/// such a header passes its checks with no hand-sized port.
#[test]
fn a_header_plugged_from_above_opens_the_lid_round_its_whole_body() {
    let dir = std::env::temp_dir().join(format!("fab-elec-mech-header-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let glb = dir.join("header_2x3.glb");
    std::fs::write(&glb, header_2x3()).expect("write the header's model");

    let mut sources = fab_elec_fixtures::docs("ledavr").expect("the LED chaser's documents");
    sources.add("ledavr.pcb.splash", include_str!("../../fixtures/ledavr.pcb.splash")).expect("the LED chaser's board document");
    let mut design = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap_or_else(|d| panic!("the LED chaser evaluates: {d:?}")).design;
    let unit = design.units.iter_mut().find(|u| u.name == "ledavr").expect("the LED chaser's board unit");
    let circuit = Arc::make_mut(&mut unit.circuit);
    let isp = circuit.comp("mcu.isp_j").expect("the LED chaser has its ISP header").id;
    let body = Arc::make_mut(&mut circuit.comps[isp.ix()].part).body.as_mut().expect("the header declares a body");
    body.source = BodySource::Mesh(glb.display().to_string());
    // As the LED chaser's models document places it: the model's pin field centred on its origin,
    // turned a quarter round onto the footprint's columns.
    body.offset = Xform3::axis_angle([mm(1.27), mm(-2.54), 0], [0.0, 0.0, 1.0], Angle::deg(-90.0));
    // The port as `header2` gives a 2 x 3 header at 2.54 mm: its pin field and 0.5 mm round it.
    let port = body.ports.iter_mut().find(|p| p.kind == BodyPortKind::Plug).expect("the header is plugged from above");
    port.profile = PolySet::rect(BBox::new(P { x: mm(-2.09), y: mm(-3.36) }, P { x: mm(2.09), y: mm(3.36) }));
    let pcb = unit.board.as_deref().expect("the LED chaser has a board");
    let mech = board_mech(pcb, &unit.circuit);

    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let mut model = FabModel::with_script("ledavr", "ledavr.cad.splash", include_str!("../../fixtures/ledavr.cad.splash"));
    model.set_env(inputs.env());
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let (_, e) = model.evaluate(&mut ev, None).unwrap_or_else(|e| panic!("the LED chaser's case builds: {e}"));
    let case = case_solids(&e, "ledavr");
    assert_eq!(case.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Base", "Lid"], "the base and the lid in place");
    let findings = case_checks(&mech, &case, &CheckOpts::default());
    let header: Vec<_> = findings.iter().filter(|f| f.level != Level::Info && f.message.contains("mcu.isp_j")).collect();
    assert!(header.is_empty(), "the header's model passes up through its opening clear of the lid: {header:#?}");

    let port = mech.ports.iter().find(|p| p.comp == isp && p.kind == BodyPortKind::Plug).expect("the header's port on the board");
    let plan = mech.bodies.iter().find(|b| b.comp == isp).expect("the header's body");
    assert!(port.dir[2] > 0.99, "the header's plug comes from above");
    let (open, pins) = (port.opening.bbox(), port.profile.offset(port.margin, mm(0.02)).bbox());
    let grown = |lo: i32, hi: i32, at: i32| (lo - at - port.margin, hi - at + port.margin);
    let ((x0, x1), (y0, y1)) = (grown(plan.min.x, plan.max.x, port.at[0]), grown(plan.min.y, plan.max.y, port.at[1]));
    let um = mm(0.001);
    assert!(open.min.x <= x0 + um && open.max.x >= x1 - um && open.min.y <= y0 + um && open.max.y >= y1 - um,
        "the opening covers the header's plan grown by its margin: {open:?} for {x0}..{x1}, {y0}..{y1}");
    assert!(open.width() > pins.width() && open.height() > pins.height(), "and is wider than its pin field's: {open:?} against {pins:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn board_solid_generates_mesh_hulls_and_missing_file_boxes_once() {
    use fab_elec_mech::scene::NO_BODY_HEIGHT;
    let dir = std::env::temp_dir().join(format!("fab-elec-mech-board-bodies-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let paths = [dir.join("closed.glb"), dir.join("open.glb"), dir.join("missing.glb")];
    for (i, path) in paths.iter().enumerate() {
        std::fs::write(path, pyramid(i != 1)).expect("write the body mesh");
    }

    let mut design = ledctl();
    let unit = &mut design.units[0];
    let circuit = Arc::make_mut(&mut unit.circuit);
    let refs = ["reg.u", "mcu.u", "btn.sw"];
    for (path, file) in refs.iter().zip(&paths) {
        let comp = circuit.comp(path).expect("the fixture has the component").id;
        let part = Arc::make_mut(&mut circuit.comps[comp.ix()].part);
        part.body.as_mut().expect("the component declares a body").source = BodySource::Mesh(file.display().to_string());
    }
    // A KiCad model not here yet: a body of no stated height (EKMODELS).
    let kicad = circuit.comp("mcu.boot_r").expect("the fixture has the boot resistor").id;
    let body = Arc::make_mut(&mut circuit.comps[kicad.ix()].part).body.as_mut().expect("the resistor declares a body");
    (body.source, body.height) = (BodySource::Extrude, 0);
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mech = board_mech(pcb, &unit.circuit);
    let board = board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills());
    assert_eq!(board.mesh_bodies.len(), 3, "all three mesh bodies are available to the generator");
    assert_eq!(board.bodies.source.matches("mesh: \"").count(), 3, "all three files have keys before evaluation");
    let body_boxes = board.mesh_bodies.clone();
    let mut inputs = CaseInputs::default();
    inputs.update(board);
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (count, seen) = (runs.clone(), requests.clone());
    let generator = body_generator(Arc::new(inputs.clone()));
    let env = inputs.env().with_generator(ELEC_BODY, Arc::new(move |input| {
        count.fetch_add(1, Ordering::Relaxed);
        seen.lock().unwrap().push(input.params.to_string());
        generator(input)
    }));
    // The input retains its mesh key, but a failed cache read must fall back to the plan box.
    std::fs::remove_file(&paths[2]).expect("remove the third body before evaluation");
    let source = "use mod.model.*\nuse mod.fab.*\nlet pcb = board(\"ledctl\")\nlet case = enclosure(pcb, {})\nobject(board_solid(pcb, board_bodies(\"ledctl\"), case.frame), name: \"Board\")";
    let mut model = FabModel::with_script("case", "case.cad.splash", source);
    model.set_env(env);
    let mut evaluator = Evaluator::new(Machine::dual_nozzle_350());
    let (compiled, evaluated) = model.evaluate(&mut evaluator, None).expect("mesh bodies and a missing file evaluate together");
    assert_eq!(runs.load(Ordering::Relaxed), 3, "the first evaluation runs once for each mesh body's distinct request");
    assert_eq!(compiled.instances[0].script.generated.len(), 3, "all three requests resolve without a preview fallback");
    let object = evaluated.placed_part(0);
    assert_eq!(object.name, "Board", "the evaluated object is the board solid");
    let mesh = evaluator.object_mesh(&object.object).expect("the Board object has a model-frame mesh");
    let has_vertex = |p: [f64; 3]| mesh.vertices.iter().any(|v| {
        (v.x - p[0]).abs() < 1e-6 && (v.y - p[1]).abs() < 1e-6 && (v.z - p[2]).abs() < 1e-6
    });
    // The case frame as board_solid passed it (lengths in metres in the request).
    let request = requests.lock().unwrap()[0].clone();
    let frame = |key: &str| {
        let at = request.find(&format!("{key}:")).unwrap_or_else(|| panic!("the request names {key}: {request}")) + key.len() + 1;
        let value = request[at..].trim_start().split(|c: char| c == ',' || c == '}' || c.is_whitespace()).next().unwrap_or("");
        value.parse::<f64>().unwrap_or_else(|_| panic!("{key} is a number in {request}")) * 1000.0
    };
    let (cx, cy, y0) = (frame("cx"), frame("cy"), frame("y0"));
    let model_point = |[x, y, z]: [f64; 3]| [x - cx, y0 + z, cy - y];
    for (path, shape) in refs[..2].iter().zip(["closed mesh's hull", "open mesh's hull"]) {
        let body = &body_boxes[*path];
        let apex = body.pose.apply_f([0.0, 0.0, 2e6]).map(|v| v * 1e-6);
        assert!(has_vertex(model_point(apex)), "the {shape}'s posed apex is a Board vertex");
    }
    let corner = |path: &str| {
        let body = &body_boxes[path];
        model_point([to_mm(body.max.x), to_mm(body.max.y), to_mm(mech.thickness + body.height)])
    };
    let bvh = Bvh::build(&mesh);
    for (path, shape) in refs[..2].iter().zip(["closed", "open"]) {
        let p = corner(path);
        assert!(!bvh.inside(dvec3(p[0], p[1], p[2])), "the {shape} pyramid's hull does not fill its plan box's top corner");
        assert!(!has_vertex(p), "the {shape} pyramid's hull has no plan-box top corner");
    }
    assert!(has_vertex(corner(refs[2])), "the missing file draws its plan box's top corner");
    let plain = mech.bodies.iter().find(|b| b.path == "mcu.boot_r").expect("the boot resistor's body");
    assert_eq!(plain.height, NO_BODY_HEIGHT, "a body of no stated height stands as the scene's plain body");
    assert!(has_vertex(model_point([to_mm(plain.max.x), to_mm(plain.max.y), to_mm(mech.thickness + plain.height)])),
        "and the board solid draws it as that box, not a box of no height");

    model.evaluate(&mut evaluator, None).expect("the unchanged case evaluates again");
    assert_eq!(runs.load(Ordering::Relaxed), 3, "an unchanged second evaluation runs no body generator");
    std::fs::remove_dir_all(&dir).expect("remove the body meshes");
}
