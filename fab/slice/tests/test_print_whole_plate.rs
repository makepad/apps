//! The whole plate produces the same G-code and package in either colour mode and with test cubes.

use fab_model::eval::{Evaluated, Evaluator, Mapped};
use fab_plate::ColourMode;
use fab_model::project::{ColorDef, TestPrint};
use fab_model::{Change, FabModel};
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};

fn setup() -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.2");
    process.values.set_str("initial_layer_print_height", "0.2");
    process.values.set_str("fab_paint_first_layer", "1");
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default(); 2], filament_map: vec![1; 2], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None; 2] }
}

fn print_of(ev: &mut Evaluated, mode: ColourMode) -> (String, Vec<u8>, Vec<Option<String>>) {
    ev.set_colour_mode(mode);
    let mut setup = setup();
    let result = makepad_fab_slice::fab_doc::slice(&mut setup, ev, &["Body".into(), "Stripe".into()]).expect("slices");
    assert_eq!(result.stats.used.len(), 2, "both the whole-part colour and painted region printed");
    let package = result.package().expect("package as the slice CLI does");
    (result.gcode, package, setup.filament_colours)
}

fn same_output(a: &(String, Vec<u8>, Vec<Option<String>>), b: &(String, Vec<u8>, Vec<Option<String>>)) {
    if a.0 != b.0 {
        let at = a.0.bytes().zip(b.0.bytes()).position(|(a, b)| a != b).unwrap_or(a.0.len().min(b.0.len()));
        let line = 1 + a.0.as_bytes()[..at].iter().filter(|&&c| c == b'\n').count();
        panic!("G-code differs at line {line}, byte {at}");
    }
    if a.1 != b.1 {
        let at = a.1.iter().zip(&b.1).position(|(a, b)| a != b).unwrap_or(a.1.len().min(b.1.len()));
        panic!("package differs at byte {at}: {:?} vs {:?}", a.1.get(at), b.1.get(at));
    }
    assert_eq!(a.2, b.2, "filament_colours differ");
}

#[test]
fn colour_modes_and_test_cubes_leave_the_whole_plate_print_byte_identical() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let source = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [6mm 0.6mm 3mm]})
object(block)
m.fill(block, Material{albedo: #3030cc name: "Body"})
m.fill(m.box({size: [3mm 1mm 5mm] at: [2mm 0 0]}), Material{albedo: #f02020 name: "Stripe"})
"#;
    let mut with_cubes = FabModel::with_script("block", "block.cad.splash", source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    e.mapped = std::sync::Arc::new(Mapped {
        inks: [("Body".into(), [0.8, 0.8, 0.8]), ("Stripe".into(), [0.0, 0.75, 0.0])].into(),
        tools: [("Body".into(), (1, 0)), ("Stripe".into(), (2, 0))].into(),
    });
    let stripe = ColorDef { rgb: Some(0xc52c99ff), ..ColorDef::new("Stripe") };
    let head = with_cubes.head();
    let (_, built) = with_cubes.apply(&mut e, "colour", head, &[Change::Color { name: stripe.name.clone(), set: Some(stripe) }], None).unwrap();
    let (_, mut bare) = built.unwrap();
    let bare_text = with_cubes.project_text();
    let model = print_of(&mut bare, ColourMode::Model);
    same_output(&model, &print_of(&mut bare, ColourMode::Material));
    let cube = |name: &str, on: bool, x: f64| TestPrint { name: name.into(), instance: "block".into(), object: "block".into(), link: 0, at: [x, 0.0, 0.0], size: [1.0; 3], turn: [0.0; 3], pose: None, on, brim: false };
    let changes = [
        Change::TestPrint { name: "Ticked".into(), set: Some(cube("Ticked", true, 1.0)) },
        Change::TestPrint { name: "Unticked".into(), set: Some(cube("Unticked", false, -1.0)) },
    ];
    let head = with_cubes.head();
    let (_, built) = with_cubes.apply(&mut e, "cubes", head, &changes, None).unwrap();
    assert_ne!(with_cubes.project_text(), bare_text, "the cubes are in the project");
    let (_, mut ev) = built.unwrap_or_else(|| with_cubes.evaluate(&mut e, None).unwrap());
    same_output(&model, &print_of(&mut ev, ColourMode::Model));
}
