//! A measured development board's case, variants and refusals through the script provider.

mod common;

use common::{inside, open_at};
use fab_elec_core::{mm, to_mm};
use fab_elec_design::fixture::devboard::{devboard_over, devboard_with, MEASURED};
use fab_elec_mech::bridge::{libraries, record_leaves, LazyBoards, BOARD, BOARD_BODIES};
use fab_elec_mech::{board_mech, case_checks, case_solids, BoardMech, CaseCheck, CheckOpts, Level};
use fab_model::eval::{Evaluated, Evaluator, Failure};
use fab_model::program::ScriptEnv;
use fab_model::{Change, FabModel, PlaceArg};
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_model_script::lang::{Input, Inputs, Lazy, Provide, RecordValue, Refusal};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const CASE: &str = include_str!("../../fixtures/devboard/devboard.cad.splash");

struct Recording {
    boards: LazyBoards,
    record: Arc<Mutex<Option<RecordValue>>>,
}

impl Provide for Recording {
    fn provide(&self, kind: &str, name: &str) -> Option<Input> {
        self.boards.provide(kind, name)
    }

    fn provide_with(&self, kind: &str, name: &str, record: &RecordValue) -> Result<Option<Input>, Refusal> {
        if kind == BOARD {
            *self.record.lock().unwrap() = Some(record.clone());
        }
        self.boards.provide_with(kind, name, record)
    }

    fn names(&self, kind: &str) -> Vec<String> {
        self.boards.names(kind)
    }

    fn hash(&self, kind: &str, name: &str) -> Option<[u8; 16]> {
        self.boards.hash(kind, name)
    }
}

fn case_model(source: &str) -> (FabModel, Arc<Mutex<Option<RecordValue>>>) {
    let record = Arc::new(Mutex::new(None));
    let recording = Recording { boards: LazyBoards::stock(), record: record.clone() };
    let mut model = FabModel::with_script("case", "devboard.cad.splash", source);
    model.set_env(ScriptEnv::new(libraries(), Inputs {
        kinds: vec![BOARD.into(), BOARD_BODIES.into()],
        values: Default::default(),
        lazy: Some(Lazy::new(Arc::new(recording))),
    }));
    (model, record)
}

/// Evaluate each case once, retaining the measurements its board call actually supplied.
fn evaluate_case(source: &str) -> (Evaluated, RecordValue, usize) {
    let (mut model, record) = case_model(source);
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let place = |object: &str| Change::Place {
        instance: "devboard".into(), object: object.into(), arg: PlaceArg::At(Some([110.0, 160.0])),
    };
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "devboard-case", head, &[place("Base"), place("Lid")], None)
        .unwrap_or_else(|err| panic!("the measured development board's case builds: {err}\n{source}"));
    let (compiled, e) = evaluated.expect("one fresh apply evaluates the measured development board's case");
    let solid_count = compiled.instances.iter().map(|i| i.script.program.solids.len()).sum();
    let record = record.lock().unwrap().clone().expect("the case asks for its board with a measurement record");
    (e, record, solid_count)
}

fn recorded_board(record: &RecordValue) -> BoardMech {
    let leaves = record_leaves(record).expect("the case's recorded measurements are valid board fields");
    let design = devboard_over(&leaves).expect("the case's recorded measurements build its board");
    let unit = design.units.iter().find(|u| u.name == "devboard").expect("the recorded design names its development board");
    let pcb = unit.board.as_deref().expect("the recorded development board has a board");
    board_mech(pcb, &unit.circuit)
}

fn with_measured(record: &str) -> String {
    let lines: Vec<_> = CASE.lines().filter(|line| line.starts_with("let measured = ")).collect();
    assert_eq!(lines.len(), 1, "the case has exactly one measurement record line to replace");
    CASE.replacen(lines[0], &format!("let measured = {record}"), 1)
}

#[test]
fn the_devboard_gets_a_clean_case_that_slices_round_it() {
    let started = Instant::now();
    let (e, record, solid_count) = evaluate_case(CASE);
    let mech = recorded_board(&record);
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
    assert_eq!(board.link.as_deref(), Some("board:devboard"), "Board is linked to the measured development board");
    let (frame_name, _) = board.frame.as_ref().expect("Board carries its named board frame");
    assert_eq!(frame_name, "board:devboard", "Board's frame is named board:devboard");

    let case = case_solids(&e, "devboard");
    assert_eq!(case.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Base", "Lid"], "the case solids are Base and Lid in the board frame");
    let findings = case_checks(&mech, &case, &CheckOpts::default());
    assert!(findings.iter().all(|f| f.level == Level::Info), "every body fits, every port is reachable and every mounting hole is held; no case finding above Info: {findings:#?}");

    // Both ports face the board's x = 0 edge. Sample the wall's middle, not the mouth's x.
    // In their opening planes u points along board -y and v along board +z.
    let gap = 0.4;
    let wall = 2.0;
    let midpoint = 0.0 - gap - wall / 2.0;
    for path in ["usb.j", "dc.j"] {
        let port = mech.ports.iter().find(|p| p.path == path).unwrap_or_else(|| panic!("the measured board has its {path} port"));
        assert!(port.dir[0] < -0.999 && port.dir[1].abs() < 1e-6 && port.dir[2].abs() < 1e-6, "{path} faces the board's left edge: {:?}", port.dir);
        let at = port.at.map(to_mm);
        let distance = (midpoint - at[0]) / port.dir[0];
        let point: [f64; 3] = std::array::from_fn(|axis| at[axis] + port.dir[axis] * distance);
        for solid in &case {
            assert!(!inside(solid, point), "{path}'s mouth is open through {} at the board-frame wall midpoint {point:?}", solid.name);
        }
        let opening = port.profile.offset(port.margin, mm(0.02)).bbox();
        let u0 = to_mm(opening.min.x);
        let u1 = to_mm(opening.max.x);
        let v0 = to_mm(opening.min.y);
        let v1 = to_mm(opening.max.y);
        for (edge, u, v) in [
            ("top", (u0 + u1) / 2.0, v1 + 0.5),
            ("first side", u0 - 0.5, (v0 + v1) / 2.0),
            ("second side", u1 + 0.5, (v0 + v1) / 2.0),
        ] {
            let beside = [point[0], point[1] - u, point[2] + v];
            assert!(case.iter().any(|solid| inside(solid, beside)), "{path}'s wall stands 0.5 mm beyond the opening's {edge} edge at board {beside:?}");
        }
    }

    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.2");
    let setup = JobSetup {
        machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1],
        nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(), filament_colours: vec![None],
    };
    let sliced = makepad_fab_slice::plate::slice_plate(&setup, &e.plate.printable())
        .unwrap_or_else(|err| panic!("the measured development board's case slices: {err}"));
    assert_eq!(sliced.objects.len(), 2, "the slice contains only Base and Lid");
    let mut report = fab_printability::Report::default();
    fab_printability::analyse_slice(&e.machine, &e.plate, &fab_printability::Materials::default(), sliced.layers(), &sliced.timeline, &mut report);
    let slice_issues: Vec<_> = report.issues.iter().filter(|i| i.severity != fab_printability::Severity::Info).collect();
    assert!(slice_issues.is_empty(), "the sliced case has no issue above Info: {slice_issues:#?}");
    let inserts: Vec<_> = e.plate.inserts().into_iter().map(|(part, insert, _)| (part, insert)).collect();
    assert_eq!(inserts.len(), 1, "the bead check receives the Board insert with its printed host");
    let bead_issues = fab_printability::insert::bead_issues(&inserts, &mut sliced.timeline.moves().copied());
    assert!(bead_issues.is_empty(), "the slice's beads clear the inserted board: {bead_issues:#?}");

    eprintln!("devboard case: one evaluation and all checks, {solid_count} compiled solids, elapsed {:?}", started.elapsed());
}

#[test]
fn devboard_variants_build_clean_and_a_mismeasured_usb_is_named() {
    for record in [
        "{usb: {kind: @micro height: 2.6mm overhang: 1mm}}",
        "{usb: {kind: @c height: 3.2mm overhang: 1mm}}",
        "{mcu: @smd}",
        "{dc: {present: false}}",
    ] {
        let source = with_measured(record);
        let (e, recorded, _) = evaluate_case(&source);
        let mech = recorded_board(&recorded);
        let case = case_solids(&e, "devboard");
        assert_eq!(case.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Base", "Lid"], "variant {record} has both case solids in its board frame");
        let findings = case_checks(&mech, &case, &CheckOpts::default());
        assert!(findings.iter().all(|f| f.level == Level::Info), "variant {record} fits its own recorded board with no finding above Info: {findings:#?}");
    }

    let (e, record, _) = evaluate_case(CASE);
    let mut measured = MEASURED;
    for (key, value) in record_leaves(&record).expect("the original document's record has board measurement leaves") {
        measured.set(&key, &value).unwrap_or_else(|err| panic!("the original document's measurement {key} is valid: {err}"));
    }
    measured.usb.height += 1.0;
    let design = devboard_with(&measured);
    let unit = design.units.iter().find(|u| u.name == "devboard").expect("the mismeasured design names its development board");
    let pcb = unit.board.as_deref().expect("the mismeasured development board has a board");
    let mech = board_mech(pcb, &unit.circuit);
    let findings = case_checks(&mech, &case_solids(&e, "devboard"), &CheckOpts::default());
    let issues: Vec<_> = findings.iter().filter(|f| f.level != Level::Info).collect();
    assert!(!issues.is_empty() && issues.iter().all(|f| f.message.contains("usb.j")), "only usb.j has findings above Info when its measured height grows by 1 mm: {findings:#?}");
    let fit = issues.iter().find(|f| f.check == CaseCheck::Fit && f.message.contains("into the Lid"))
        .unwrap_or_else(|| panic!("the taller usb.j has a Fit finding into the Lid: {findings:#?}"));
    let amount = fit.message.split_once("runs ").and_then(|(_, rest)| rest.split_whitespace().next()).and_then(|number| number.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("the usb.j Fit finding states its intrusion in mm after 'runs ': {}", fit.message));
    assert!((amount - 0.7).abs() < 0.05, "usb.j intrudes by 1 mm minus the opening's 0.3 mm margin, within 0.05 mm: {}", fit.message);

    for (record, key, message) in [
        ("{usb: {hieght: 7.9mm}}", "'usb.hieght'", "is not a devboard measurement"),
        ("{usb: {height: 7.9}}", "'usb.height'", "7900 mm"),
    ] {
        let source = with_measured(record);
        let line = source.lines().position(|line| line.contains("board(\"devboard\", measured)"))
            .expect("the refusal source has the measured board call") + 1;
        let (model, _) = case_model(&source);
        let failure: Failure = match model.compile() {
            Ok(_) => panic!("the invalid measurement record {record} must fail compilation"),
            Err(failure) => failure,
        };
        assert_eq!(failure.line, line as u32, "record {record} is refused at the measured board call: {failure}");
        assert!(failure.message.contains(key), "record {record} names its refused dotted key {key}: {failure}");
        assert!(failure.message.contains(message), "record {record} explains the measurement error with '{message}': {failure}");
    }
}
