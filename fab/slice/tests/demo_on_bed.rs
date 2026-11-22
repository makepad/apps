//! The demo plate's parts sit on the bed: a field part rests on the lowest solid point its
//! field's proof found, its box (which holds the solid) at most the extent tolerance below it.
//! Resting on a box too big underneath would lift the part: its first layers empty, the print
//! starting in the air.

use fab_model::eval::{Evaluated, Evaluator, EXTENT_TOL};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};

const PARTS: [(&str, &str, &str); 3] = [
    ("bracket", "bracket.cad.splash", include_str!("../../app/resources/bracket.cad.splash")),
    ("vase", "vase.cad.splash", include_str!("../../app/resources/vase.cad.splash")),
    ("lattice_cube", "lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash")),
];

/// The part evaluated; the lattice cube at its smallest (a 10 mm cube: its Splash kernel
/// field costs seconds to evaluate at the default size, and the box under it is the same).
fn build(name: &str, file: &str, source: &str) -> Evaluated {
    let mut m = FabModel::with_script(name, file, source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    if name != "lattice_cube" {
        return ev;
    }
    let head = m.head();
    let change = fab_model::Change::Param { instance: name.into(), name: "lattice_size".into(), value: Some(0.010) };
    m.apply(&mut e, "small", head, &[change], None).expect("the lattice resizes").1.expect("it rebuilds").1
}

/// Thick layers: only the first one matters here. (Below 1.86 mm: the default walls' bead
/// spacing at 0.4 mm widths stays positive.)
fn setup() -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "1.5");
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

/// Part 0 of `ev` rests on the bed and the plate check passes: a field part on its solid's
/// underside, its box reaching under the bed by its proof's gap, at most `gap_max`. Sliced
/// (`slice`), its first layer prints.
fn starts_on_the_bed(name: &str, ev: &Evaluated, gap_max: f64, slice: bool) {
    let fr = &ev.frames[0];
    assert!(ev.reports.iter().all(|r| r.issues.is_empty()), "{name}: {:?}", ev.reports);
    let underside = match ev.fields[0].as_ref() {
        Some(pf) => {
            let placed = pf.extent_under(&[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]).expect("a solid");
            let gap = pf.bottom_gap.expect("a solid point found");
            assert!(gap <= gap_max && (placed.min[2] + gap).abs() < 1e-9, "{name}: the box reaches {} mm under the bed, its gap {gap}", placed.min[2]);
            let underside = placed.found.expect("solid points").0[2];
            assert_eq!(ev.plate.parts[0].floor(), underside, "{name}: the check tests where it rests");
            underside
        }
        None => ev.plate.parts[0].mesh.bounds().min[2],
    };
    assert!(underside.abs() < 1e-9, "{name}: the solid's underside is at z = {underside} mm");
    if !slice {
        return;
    }
    let sliced = match ev.fields[0].as_ref() {
        Some(pf) => {
            let mut to = pf.to_printer;
            for (r, o) in fr.offset.iter().enumerate() {
                to[r][3] -= o;
            }
            let frames = vec![Frame { local: fr.local.clone(), offset: fr.offset, field: Some(PartField::new(pf.shape.clone(), to)) }];
            slice_frames(&setup(), &ev.plate, &frames, &[], None).expect("slices from the field")
        }
        None => makepad_fab_slice::plate::slice_plate(&setup(), &ev.plate).expect("slices from the mesh"),
    };
    let first = sliced.layers().first().expect("a layer");
    let area: f64 = first.objects.iter().flat_map(|o| o.slices.iter()).map(|e| e.area_mm2()).sum();
    assert!(first.print_z > 0.0 && area > 1.0, "{name}: the first layer (z = {:.2} mm) is empty: {area} mm2", first.print_z);
}

#[test]
fn the_demo_parts_start_on_the_bed() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    for (name, file, source) in PARTS {
        starts_on_the_bed(name, &build(name, file, source), EXTENT_TOL, true);
    }
}

/// A project keeps its own copy of a script: an older lattice cube's box reaches a millimetre
/// past its solid, where the steep kernel (a gyroid's Lipschitz is near 3) leaves the bottom's
/// proof open (a restored project's: half a millimetre at 60 mm, more than the tolerance at
/// 10 mm too). It still rests on its solid's underside and passes the plate check, the gap a
/// warning, and its first layer prints.
#[test]
fn a_box_bigger_than_its_solid_rests_on_the_solid() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let (_, file, source) = PARTS[2];
    let exact = "bounds: [[-lattice_size / 2, 0, -lattice_size / 2], [lattice_size / 2, lattice_size, lattice_size / 2]]";
    let margin = "bounds: [[-lattice_size / 2 - 1mm, -1mm, -lattice_size / 2 - 1mm], [lattice_size / 2 + 1mm, lattice_size + 1mm, lattice_size / 2 + 1mm]]";
    // at 10 mm, built once (the proof that stays open spends its whole budget)
    let (size, small) = ("let lattice_size = param(24mm,", "let lattice_size = param(10mm,");
    assert!(source.contains(exact) && source.contains(size), "the demo's box is its solid's, 24 mm");
    let source = source.replace(exact, margin).replace(size, small);
    let m = FabModel::with_script("lattice_cube", file, &source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    let gap = ev.fields[0].as_ref().and_then(|f| f.bottom_gap).expect("a solid point found");
    assert!(gap > EXTENT_TOL && ev.warnings.iter().any(|w| w.contains("may sit up to")), "the open gap is said: {gap} {:?}", ev.warnings);
    starts_on_the_bed("lattice_cube", &ev, 1.0, true);
}
