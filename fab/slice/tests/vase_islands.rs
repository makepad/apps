//! The demo vase slices with nothing floating: no layer region without support under it
//! (the user saw a solid disk inside the neck of the old Splash-kernel vase), sliced from its
//! field (the app's path for a field part: the part has no mesh).

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};

const KERNEL: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet vase = m.sdf({\n    bounds: [[-25.25mm, 0, -25.25mm], [25.25mm, 72mm, 25.25mm]]\n    cell: 0.9mm\n    field: fn(p) {\n        let belly = sdf_ellipsoid(p - vec3(0, 23.8mm, 0), vec3(21mm, 25.2mm, 21mm))\n        let neck = sdf_cylinder(p - vec3(0, 51.8mm, 0), 9.5mm, 29.4mm)\n        let lip = sdf_torus(p - vec3(0, 65.8mm, 0), 10.7mm, 1.8mm)\n        let body = smooth_union(smooth_union(belly, neck, 9mm), lip, 9mm)\n        let foot = sdf_box(p - vec3(0, -7mm, 0), vec3(84mm, 20mm, 84mm))\n        let hollow = sdf_shell(sdf_difference(body, foot), 1.6mm)\n        let mouth = sdf_cylinder(p - vec3(0, 74mm, 0), 8.7mm, 20mm)\n        return sdf_difference(hollow, mouth)\n    }\n})\nobject(vase)\n";

fn build(source: &str) -> Evaluated {
    let m = FabModel::with_script("vase", "vase.cad.splash", source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1
}

/// Thick layers: whether a layer is empty or floating does not depend on how many there are.
fn setup() -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.6");
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

/// The unsupported regions of a slice (`mid_air`, `empty_layer`): their messages and where.
fn floating(ev: &Evaluated, r: &makepad_fab_slice::SliceResult) -> Vec<String> {
    let mut report = fab_printability::Report::default();
    fab_printability::analyse_slice(&ev.machine, &ev.plate, &fab_printability::Materials::default(), &r.layers(), &r.timeline, &mut report);
    let c = ev.plate.parts[0].mesh.bounds().center();
    report.issues.iter().filter(|i| matches!(i.kind, "mid_air" | "empty_layer")).map(|i| format!("{} (r {:.1} mm, {:.2} mm2)", i.message, ((i.at_mm[0] - c[0]).powi(2) + (i.at_mm[1] - c[1]).powi(2)).sqrt(), i.area_mm2)).collect()
}

/// The floating regions of the vase sliced from its field (a field part has no mesh: the
/// app slices it from its field).
fn check(label: &str, source: &str) -> Vec<String> {
    let ev = build(source);
    let pf = ev.fields[0].as_ref().expect("a field part");
    let fr = &ev.frames[0];
    let mut to = pf.to_printer;
    for (r, o) in fr.offset.iter().enumerate() {
        to[r][3] -= o;
    }
    let frames = vec![Frame { local: fr.local.clone(), offset: fr.offset, field: Some(PartField::new(pf.shape.clone(), to)) }];
    let field = slice_frames(&setup(), &ev.plate, &frames, &[], None).expect("slices from the field");
    let found = floating(&ev, &field);
    println!("{label}: {found:?}");
    found
}

/// The old vase blended its lip into the neck with the 9 mm body blend and then hollowed the
/// whole: the blend's crown over the neck top was hollowed too, and the mouth cut only its
/// middle, so the crown's inner rim hung in the mouth as a ring with nothing under it (a
/// floating island at its lowest layer). The demo vase puts the lip on after hollowing.
#[test]
fn the_vases_slice_with_nothing_floating() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let kernel = check("kernel vase", KERNEL);
    assert!(!kernel.is_empty() && kernel[0].contains("floating island"), "the old vase's hanging ring is found: {kernel:?}");
    let port = check("vase.cad.splash", include_str!("../../app/resources/vase.cad.splash"));
    assert!(port.is_empty(), "the demo vase has floating regions: {port:?}");
}

/// The shipped Voronoi vase stands on its foot and holds its collar: every strut rests on what is
/// under it, up from the bed.
#[test]
fn the_voronoi_vase_slices_with_nothing_floating() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let found = check("voronoi_vase.cad.splash", include_str!("../../app/resources/voronoi_vase.cad.splash"));
    assert!(found.is_empty(), "{found:?}");
}
