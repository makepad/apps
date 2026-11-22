//! The whole plate's print never sees a test print: the same bytes with and without.

use fab_model::eval::Evaluator;
use fab_model::project::TestPrint;
use fab_model::{Change, FabModel};
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};

fn setup() -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.2");
    process.values.set_str("initial_layer_print_height", "0.2");
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

fn print_of(m: &FabModel) -> String {
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let mut ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    let mut setup = setup();
    makepad_fab_slice::fab_doc::slice(&mut setup, &mut ev, &[]).expect("slices").gcode
}

#[test]
fn cubes_on_the_plate_leave_its_print_byte_identical() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = m.box({size: [20mm 20mm 8mm]})\nobject(block)\n";
    let bare = FabModel::with_script("block", "block.cad.splash", source);
    let mut with_cubes = FabModel::with_script("block", "block.cad.splash", source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let cube = |name: &str, on: bool, x: f64| TestPrint { name: name.into(), instance: "block".into(), object: "block".into(), link: 0, at: [x, 0.0, 0.0], size: [6.0; 3], turn: [0.0; 3], pose: None, on, brim: false };
    let changes = [
        Change::TestPrint { name: "Ticked".into(), set: Some(cube("Ticked", true, 5.0)) },
        Change::TestPrint { name: "Unticked".into(), set: Some(cube("Unticked", false, -5.0)) },
    ];
    let head = with_cubes.head();
    with_cubes.apply(&mut e, "cubes", head, &changes, None).unwrap_or_else(|f| panic!("{f}"));
    assert_ne!(with_cubes.project_text(), bare.project_text(), "the cubes are in the project");
    assert_eq!(print_of(&with_cubes), print_of(&bare));
}
