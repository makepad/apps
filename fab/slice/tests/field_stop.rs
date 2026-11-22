//! A field whose `while` reaches the language's stop says so to whoever reads it, the slicer too.

use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};

const SOURCE: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.sdf({field: fn(p) { let i = 0\n let n = int(LOOPS + 0.0 * p.x)\n while i < n { i = i + 1 }\n return length(p) - 1mm - 0.001mm * float(i) } bounds: [[-5mm, -5mm, -5mm], [5mm, 5mm, 5mm]] cell: 1mm}))\n";

fn stops(loops: &str) -> bool {
    let source = SOURCE.replace("LOOPS", loops);
    let m = FabModel::with_script("stop", "stop.cad.splash", &source);
    let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
    let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    let (field, frame) = (ev.fields[0].as_ref().expect("a field part"), &ev.frames[0]);
    let mut to = field.to_printer;
    for (r, o) in frame.offset.iter().enumerate() {
        to[r][3] -= o;
    }
    let frames = [Frame { local: frame.local.clone(), offset: frame.offset, field: Some(PartField::new(field.shape.clone(), to)) }];
    let setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    slice_frames(&setup, &ev.plate, &frames, &[], None).expect("slices from the field");
    field.shape.root.loop_stopped()
}

#[test]
fn a_loop_the_slicer_stops_shows_on_the_shape_it_read() {
    assert!(!stops("100.0"), "a loop that ends is no stop");
    assert!(stops("3000.0"), "the stop reached by slicing shows");
}
