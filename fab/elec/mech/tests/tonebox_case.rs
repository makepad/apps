//! The tone box's case from its documents alone: the second project the closed box runs on, with
//! one key, one knob, one LED and one jack, none of which the app knows by name.

use fab_elec_core::to_mm;
use fab_elec_mech::bridge::{board_inputs, CaseInputs};
use fab_elec_mech::{assembly_plan, board_mech, case_binding, case_checks, case_pipes, case_poses, case_solids, CaseBinding, CaseRole, CheckOpts, Level, StepKind};
use fab_elec_parts::BodyPortKind;
use fab_model::eval::Evaluator;
use fab_model::{project, Change, FabModel};
use fab_plate::machine::Machine;

const CASE: &str = include_str!("../../fixtures/tonebox.cad.splash");
const PLATE: &str = include_str!("../../../app/resources/demos/tonebox/tonebox.fab.splash");

#[test]
fn the_case_fits_its_board_opens_for_its_jack_and_binds_its_key_knob_and_pipe() {
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box's documents");
    let design = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())
        .unwrap_or_else(|d| panic!("the tone box's documents evaluate: {d:?}"))
        .design;
    let unit = design.units.iter().find(|u| u.name == "tonebox").expect("the tone box's unit");
    let pcb = unit.board.as_deref().expect("the documents include the placed board");
    let mech = board_mech(pcb, &unit.circuit);
    let mouths: Vec<_> = mech.ports.iter().filter(|p| p.kind == BodyPortKind::Plug).map(|p| p.path.as_str()).collect();
    assert_eq!(mouths, ["usb.j", "jack"], "the power and the line out are the plugs the walls open for");

    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let mut model = FabModel::with_script("tonebox", "tonebox.cad.splash", CASE);
    model.set_env(inputs.env());
    let changes: Vec<_> = project::changes(PLATE).expect("the demo plate parses").into_iter()
        .filter(|c| !matches!(c, Change::AddInstance { .. })).collect();
    let mut evaluator = Evaluator::new(Machine::dual_nozzle_350());
    let head = model.head();
    let (_, evaluated) = model.apply(&mut evaluator, "case", head, &changes, None)
        .unwrap_or_else(|e| panic!("the tone box's case builds: {e}"));
    let (_, e) = evaluated.expect("the first plate placement evaluates the case");

    let parts = &e.plate.parts;
    let mut names: Vec<_> = parts.iter().filter(|p| p.optics.is_none()).map(|p| p.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Base", "Knob pitch", "Lid", "Pin key"], "the case prints its base, its lid, the keycap and the knob");
    let pipes: Vec<_> = parts.iter().filter(|p| p.optics.is_some()).collect();
    let lid = parts.iter().find(|p| p.name == "Lid").expect("the lid prints");
    assert!(pipes.len() == 1 && pipes[0].body_of == lid.body_of && pipes[0].color != lid.color,
        "the LED's clear pipe prints into the lid: {names:?}");
    for shell in parts.iter().filter(|p| p.name == "Base" || p.name == "Lid") {
        assert_eq!(shell.mesh.open_edges(), 0, "{} is a closed solid", shell.name);
    }

    // the walls open for both plugs, the lid for the key's stem, the pot's shaft and the pipe: nothing blocks,
    // misses or fails to hold
    let case = case_solids(&e, "tonebox");
    let findings = case_checks(&mech, &case, &CheckOpts { pipes: case_pipes(&e, "tonebox"), ..CheckOpts::default() });
    assert!(findings.iter().all(|f| f.level == Level::Info),
        "the assembled case has no fit, port or holding error or warning: {findings:#?}");

    // closed, the keycap presses the key through its travel, the knob turns the pot round its
    // shaft and the pipe shows the LED
    let poses = case_poses(&e, "tonebox");
    let mut bound: Vec<_> = poses.iter().filter_map(|pose| match (case_binding(pose, &mech), &pose.role) {
        (CaseBinding::Key { comp, travel }, CaseRole::Key(path)) => {
            assert!(travel > 1.0, "the keycap travels with the key: {travel} mm");
            Some(format!("key {path} {}", unit.circuit.comps[comp.comp.ix()].path))
        }
        (CaseBinding::Knob { comp, axis }, CaseRole::Knob(_)) => {
            let shaft = mech.ports.iter().find(|p| p.comp == comp.comp && p.kind == BodyPortKind::Shaft).expect("the knob's pot has its shaft");
            assert_eq!(axis, [to_mm(shaft.at[0]), to_mm(shaft.at[1])], "the knob turns round its shaft's axis");
            Some(format!("knob {}", unit.circuit.comps[comp.comp.ix()].path))
        }
        (CaseBinding::Pipe { comp }, CaseRole::Body { host, .. }) if host == "lid" => Some(format!("pipe {}", unit.circuit.comps[comp.comp.ix()].path)),
        (CaseBinding::None, CaseRole::Shell(_)) => None,
        (binding, _) => panic!("{binding:?} binds a part it should not: {pose:?}"),
    }).collect();
    bound.sort();
    assert_eq!(bound, ["key key key", "knob pitch", "pipe lamp.d"], "the keycap presses the key, the knob turns the pitch pot and the pipe carries the LED's light");

    // it goes together base, board, keycap, lid with its pipe, knob: each part on once, each
    // arriving from above, the base first and staying
    let plan = assembly_plan(&poses);
    let kinds: Vec<_> = plan.steps.iter().map(|s| s.kind.clone()).collect();
    assert_eq!(kinds, [StepKind::Base, StepKind::Board, StepKind::Keys, StepKind::Lid, StepKind::Knobs], "the tone box's build order");
    let role_of = |part: usize| &poses.iter().find(|p| p.part == part).expect("a planned part is a case part").role;
    let lid = &plan.steps[3];
    assert!(lid.parts.iter().any(|&p| matches!(role_of(p), CaseRole::Body { host, .. } if host == "lid")),
        "the pipe printed in the lid goes on with the lid");
    let mut planned: Vec<usize> = plan.steps.iter().flat_map(|s| s.parts.iter().copied()).collect();
    planned.sort_unstable();
    let mut cased: Vec<usize> = poses.iter().map(|p| p.part).collect();
    cased.sort_unstable();
    assert_eq!(planned, cased, "every case part goes on in exactly one step");
    assert_eq!(plan.steps[0].from, [0.0, 0.0, -1.0], "the base is what the rest come to");
    assert!(plan.steps[1..].iter().all(|s| s.from == [0.0, 0.0, 1.0]), "every other part seats from above");
}
