//! The demo's lattice cube (smaller; a gyroid sheet in a cube, a Splash-kernel `m.sdf` part: a field that
//! is no distance, its gradient up to about 1.9) slices from its field (the app's path for a
//! field part: it has no mesh) with no printability errors. The user saw jagged, torn
//! fragments and 17 errors: the mesher and the field slicer took it for a distance.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};

/// The demo's lattice cube at half its size and period (12 mm, 6 mm: the Splash kernel field
/// costs seconds to evaluate at 24 mm and 8 mm, and the gyroid sheet is the same).
fn build() -> Evaluated {
    let mut m = FabModel::with_script("lattice_cube", "lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash"));
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    let head = m.head();
    let param = |name: &str, metres: f64| fab_model::Change::Param { instance: "lattice_cube".into(), name: name.into(), value: Some(metres) };
    m.apply(&mut e, "small", head, &[param("lattice_size", 0.012), param("lattice_period", 0.006)], None).expect("the lattice resizes").1.expect("it rebuilds").1
}

fn setup() -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.6");
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

fn errors(r: &fab_printability::Report) -> Vec<String> {
    r.issues.iter().filter(|i| matches!(i.severity, fab_printability::Severity::Error)).map(|i| format!("{}: {} at ({:.2}, {:.2}) {:.3} mm2", i.kind, i.message, i.at_mm[0], i.at_mm[1], i.area_mm2)).collect()
}

#[test]
fn the_lattice_cube_slices_clean_from_its_field() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let ev = build();
    let pf = ev.fields[0].as_ref().expect("a field part");
    let fr = &ev.frames[0];
    let mut to = pf.to_printer;
    for (r, o) in fr.offset.iter().enumerate() {
        to[r][3] -= o;
    }
    let frames = vec![Frame { local: fr.local.clone(), offset: fr.offset, field: Some(PartField::new(pf.shape.clone(), to)) }];
    let from_field = slice_frames(&setup(), &ev.plate, &frames, &[], None).expect("slices from the field");
    let materials = fab_printability::Materials::default();
    let mut report = fab_printability::analyse_plate(&ev.machine, &ev.plate, &materials);
    fab_printability::analyse_slice(&ev.machine, &ev.plate, &materials, from_field.layers(), &from_field.timeline, &mut report);
    let errors = errors(&report);
    assert!(errors.is_empty(), "printability errors sliced from the field: {errors:?}");
}
