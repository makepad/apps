use fab_elec_mech::bridge::{board_inputs, CaseInputs};
use fab_elec_core::to_mm;
use fab_elec_mech::{board_mech, case_binding, case_checks, case_pipes, case_poses, case_solids, CaseBinding, CaseCheck, CaseRole, CheckOpts, Level};
use fab_elec_parts::BodyPortKind;
use fab_model::eval::Evaluator;
use fab_model::{project, Change, FabModel};
use fab_plate::machine::Machine;

const CASE: &str = include_str!("../../fixtures/drum16.cad.splash");
const PLATE: &str = include_str!("../../../app/resources/demos/drum16/drum16.fab.splash");
const MODELS: &str = include_str!("../../fixtures/drum16.models.part.splash");

#[test]
fn the_case_fits_its_board_and_prints_all_of_its_controls() {
    let mut sources = fab_elec_fixtures::docs("drum16").expect("the drum machine's documents");
    // As in a project that holds its parts' models: each override poses its part's body (the fit
    // check reads boxes where the files themselves are absent), and the ports stay on the parts.
    for mesh in MODELS.split("mesh: \"").skip(1).filter_map(|rest| rest.split('"').next()) {
        sources.add_asset(mesh);
    }
    let design = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())
        .unwrap_or_else(|d| panic!("the drum machine's documents evaluate: {d:?}"))
        .design;
    let unit = design.units.iter().find(|u| u.name == "drum16").expect("the drum machine's unit");
    let pcb = unit.board.as_deref().expect("the documents include the placed board");
    let mech = board_mech(pcb, &unit.circuit);
    assert!(mech.holes.is_empty(), "occupied switch posts and pot lugs are not case mounts: {:?}", mech.holes);

    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    // Inspect the helper's actual engagement values in this same build. Only a failing knob
    // emits a marker; its name identifies the shaft that cannot hold a third of the bore.
    // shaft_knob leaves a 1.2 mm skin over the bore, as its public contract states.
    let source = format!("{CASE}\n{}", r#"
for k in case.knobs {
    if !(k.engage >= (k.height - 1.2mm) / 3) {
        object(m.box({size: [1mm 1mm 1mm]}), name: "Short shaft " + k.ref)
    }
}
"#);
    let mut model = FabModel::with_script("drum16", "drum16.cad.splash", &source);
    model.set_env(inputs.env());
    // Apply the demo's colours and placements before its first evaluation; the script already
    // exists in with_script. This checks the actual plate without building the case twice.
    let changes: Vec<_> = project::changes(PLATE).expect("the demo plate parses").into_iter()
        .filter(|c| !matches!(c, Change::AddInstance { .. })).collect();
    let mut evaluator = Evaluator::new(Machine::dual_nozzle_350());
    let head = model.head();
    let (_, evaluated) = model.apply(&mut evaluator, "case", head, &changes, None)
        .unwrap_or_else(|e| panic!("the drum machine's case builds: {e}"));
    let (compiled, e) = evaluated.expect("the first plate placement evaluates the case");
    let script = &compiled.instance("drum16").expect("the plate has its case instance").script;
    let short: Vec<_> = script.objects.iter().filter(|o| o.name.starts_with("Short shaft ")).map(|o| &o.name).collect();
    assert!(short.is_empty(), "every knob engages at least a third of its bore: {short:?}");

    // This plate prints the base; the lid with its pipes, the keycaps and the knobs print on the
    // demo's other plate and are shown here, so Mechanics closes the whole box. The checks read
    // every placed part, printed or shown.
    let parts: Vec<&fab_plate::Part> = (0..e.plate.parts.len() + e.plate.excluded.len()).map(|i| e.placed_part(i)).collect();
    let names: Vec<_> = parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names.iter().filter(|n| n.starts_with("Pin ")).count(), 20, "one keycap per switch: {names:?}");
    assert_eq!(names.iter().filter(|n| n.starts_with("Knob ")).count(), 8, "one knob per pot: {names:?}");
    let pipes: Vec<_> = parts.iter().copied().filter(|p| p.optics.is_some()).collect();
    assert_eq!(pipes.len(), 20, "one clear light pipe per LED: {names:?}");
    let part = |name: &str| parts.iter().copied().find(|p| p.name == name).unwrap_or_else(|| panic!("{name} is on the plate: {names:?}"));
    let (base, lid) = (part("Base"), part("Lid"));
    for shell in [base, lid] {
        assert!(!shell.mesh.triangles.is_empty(), "{} has a mesh", shell.name);
        assert_eq!(shell.mesh.open_edges(), 0, "{} is a closed solid", shell.name);
    }
    assert!(lid.body_of.is_some(), "the lid groups its shell and clear pipes");
    for pipe in pipes {
        assert_eq!(pipe.body_of, lid.body_of, "{} prints into the lid", pipe.name);
        assert_ne!(pipe.color, lid.color, "{} prints in clear filament", pipe.name);
        assert!((pipe.bounds().min[2] - lid.bounds().min[2]).abs() < 1e-6,
            "{} is flush with the lid's top, which faces the bed", pipe.name);
    }
    // Each pot's legend, from its label, is engraved into the lid's top and filled in the pipes'
    // clear filament, so it reads light on the dark lid.
    let legends: Vec<_> = parts.iter().copied().filter(|p| p.name.starts_with("Legend ")).collect();
    assert_eq!(legends.len(), 8, "one legend per labelled pot: {names:?}");
    for legend in &legends {
        assert_eq!(legend.body_of, lid.body_of, "{} prints into the lid", legend.name);
        assert_ne!(legend.color, lid.color, "{} prints in another filament than the lid", legend.name);
        assert!((legend.bounds().min[2] - lid.bounds().min[2]).abs() < 1e-6,
            "{} is flush with the lid's top, which faces the bed", legend.name);
    }
    let board = e.plate.excluded.iter().find(|p| p.part.name == "Board").expect("the board never prints");
    assert_eq!(board.insert.as_ref().expect("the board is an insert").into, base.object,
        "the board rides in the base");

    let case = case_solids(&e, "drum16");
    assert_eq!(case.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Base", "Lid"],
        "the checks find both shells in the assembled board frame");
    // The lid stands 12.8 mm over the LEDs, deeper than a bare LED's 10 mm reach: the pipes
    // carry their light, and without them each LED is too deep.
    let bare = case_checks(&mech, &case, &CheckOpts::default());
    assert_eq!(bare.iter().filter(|f| f.check == CaseCheck::Depth && f.level == Level::Error).count(), 20,
        "without its pipe each LED sits deeper than its light reaches: {bare:#?}");
    let pipes = case_pipes(&e, "drum16");
    assert_eq!(pipes.len(), 20, "the checks find every pipe in the board's frame");
    let findings = case_checks(&mech, &case, &CheckOpts { pipes, ..CheckOpts::default() });
    assert!(findings.iter().all(|f| f.level == Level::Info),
        "the assembled case has no fit, port or holding error or warning: {findings:#?}");

    // The closed case: every part where it sits, whatever pose it prints in. Each keycap stands
    // on its switch's stem top and presses it, each knob centres on its shaft and turns its pot,
    // each pipe in the lid carries its LED's light.
    let poses = case_poses(&e, "drum16");
    let count = |f: &dyn Fn(&CaseRole) -> bool| poses.iter().filter(|p| f(&p.role)).count();
    assert_eq!(count(&|r| matches!(r, CaseRole::Shell(_))), 2, "the base and the lid: {poses:#?}");
    assert_eq!(count(&|r| matches!(r, CaseRole::Key(_))), 20, "a keycap per switch");
    assert_eq!(count(&|r| matches!(r, CaseRole::Knob(_))), 8, "a knob per pot");
    let in_board = |pose: &fab_elec_mech::CasePose| -> Vec<[f64; 3]> {
        let m = &pose.to_board;
        e.placed_part(pose.part).mesh.positions.iter().map(|p| {
            let p = p.map(f64::from);
            std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
        }).collect()
    };
    let port_of = |path: &str, kind: BodyPortKind| mech.ports.iter().find(|p| p.path == path && p.kind == kind)
        .unwrap_or_else(|| panic!("{path} has its {kind:?} port"));
    for pose in &poses {
        let (path, kind) = match &pose.role {
            CaseRole::Key(path) => (path, BodyPortKind::Press),
            CaseRole::Knob(path) => (path, BodyPortKind::Shaft),
            _ => continue,
        };
        let port = port_of(path, kind);
        let at = port.at.map(to_mm);
        let points = in_board(pose);
        let lo = |k: usize| points.iter().map(|p| p[k]).fold(f64::MAX, f64::min);
        let hi = |k: usize| points.iter().map(|p| p[k]).fold(f64::MIN, f64::max);
        let centre = [(lo(0) + hi(0)) / 2.0, (lo(1) + hi(1)) / 2.0];
        assert!((centre[0] - at[0]).abs() < 0.05 && (centre[1] - at[1]).abs() < 0.05,
            "{path}: its {kind:?} part centres on the port at {at:?}, not {centre:?}");
        if kind == BodyPortKind::Press {
            assert!((lo(2) - at[2]).abs() < 0.05, "{path}: the keycap's foot is on the stem's top at {:.2} mm, not {:.2} mm", at[2], lo(2));
        } else {
            assert!(lo(2) > at[2] - to_mm(port.stand) && hi(2) > at[2], "{path}: the knob sits on its shaft over the lid");
        }
    }
    let has = |comp: fab_elec_core::CompRef, kind: BodyPortKind| comp.board == mech.unit
        && mech.ports.iter().any(|p| p.comp == comp.comp && p.kind == kind);
    let (mut keys, mut knobs, mut pipes) = (0, 0, 0);
    let mut marks = Vec::new();
    for pose in &poses {
        match (case_binding(pose, &mech), &pose.role) {
            (CaseBinding::Key { comp, travel }, CaseRole::Key(_)) => {
                assert!(has(comp, BodyPortKind::Press) && travel > 1.0, "a keycap presses its switch through its travel: {pose:?}");
                keys += 1;
            }
            (CaseBinding::Knob { comp, axis }, CaseRole::Knob(_)) => {
                assert!(has(comp, BodyPortKind::Shaft), "a knob turns its pot: {pose:?}");
                let shaft = mech.ports.iter().find(|p| p.comp == comp.comp && p.kind == BodyPortKind::Shaft).expect("its shaft");
                assert_eq!(axis, [to_mm(shaft.at[0]), to_mm(shaft.at[1])], "a knob turns round its shaft's axis: {pose:?}");
                knobs += 1;
            }
            (CaseBinding::Pipe { comp }, CaseRole::Body { host, .. }) if host == "lid" => {
                assert!(has(comp, BodyPortKind::Light), "a pipe in the lid carries an LED's light: {pose:?}");
                pipes += 1;
            }
            (CaseBinding::None, CaseRole::Body { host, link: None }) if host == "lid" => marks.push(pose),
            (CaseBinding::None, CaseRole::Shell(_)) => {}
            (binding, _) => panic!("{binding:?} binds a part it should not: {pose:?}"),
        }
    }
    assert_eq!((keys, knobs, pipes), (20, 8, 20), "every keycap, knob and pipe drives or shows the board: {poses:#?}");
    // On the closed box each legend shows beside its knob: in the lid's top, clear of every knob,
    // keycap and pipe opening.
    assert_eq!(marks.len(), 8, "every legend sits in the lid: {poses:#?}");
    let plan = |pose: &fab_elec_mech::CasePose| {
        let points = in_board(pose);
        let lo = |k: usize| points.iter().map(|p| p[k]).fold(f64::MAX, f64::min);
        let hi = |k: usize| points.iter().map(|p| p[k]).fold(f64::MIN, f64::max);
        ([lo(0), lo(1)], [hi(0), hi(1)], hi(2))
    };
    let mut over: Vec<([f64; 2], [f64; 2])> = poses.iter().filter(|p| matches!(p.role, CaseRole::Knob(_)))
        .map(|p| { let (lo, hi, _) = plan(p); (lo, hi) }).collect();
    for port in mech.ports.iter().filter(|p| matches!(p.kind, BodyPortKind::Press | BodyPortKind::Light) && p.dir[2] > 0.7) {
        let b = port.opening.bbox();
        let at = port.at.map(to_mm);
        over.push(([at[0] + to_mm(b.min.x), at[1] + to_mm(b.min.y)], [at[0] + to_mm(b.max.x), at[1] + to_mm(b.max.y)]));
    }
    let (lid_lo, lid_hi, lid_top) = plan(poses.iter().find(|p| matches!(&p.role, CaseRole::Shell(r) if r == "lid")).expect("the lid's pose"));
    for mark in &marks {
        let (lo, hi, top) = plan(mark);
        assert!((top - lid_top).abs() < 1e-3, "a legend is flush with the lid's top: {top} vs {lid_top}");
        assert!(lo[0] > lid_lo[0] && lo[1] > lid_lo[1] && hi[0] < lid_hi[0] && hi[1] < lid_hi[1],
            "a legend at {lo:?}..{hi:?} lies inside the lid's top {lid_lo:?}..{lid_hi:?}");
        for (olo, ohi) in &over {
            assert!(hi[0] <= olo[0] || lo[0] >= ohi[0] || hi[1] <= olo[1] || lo[1] >= ohi[1],
                "a legend at {lo:?}..{hi:?} stays clear of the knob or opening at {olo:?}..{ohi:?}");
        }
    }

    let bounds = base.bounds();
    assert!(bounds.max[0] - bounds.min[0] <= e.machine.bed.width()
        && bounds.max[1] - bounds.min[1] <= e.machine.bed.depth(),
        "the base fits the print check's 350 x 320 mm bed: {bounds:?}");
    // The plate's own reports, as the app shows them: each object where every nozzle its
    // colours print with reaches it (the lid's shell and pipes are one object).
    let blocking: Vec<_> = e.reports.iter().filter(|r| r.blocking())
        .map(|r| (e.plate.parts.get(r.part).map(|p| p.name.as_str()), &r.issues)).collect();
    assert!(blocking.is_empty(), "every part sits on the bed where its nozzles reach it: {blocking:?}");
}
