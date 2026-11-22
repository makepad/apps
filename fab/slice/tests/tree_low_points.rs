//! The low points of the user's Lattice cube are held. Each face of its lattice starts at a low
//! point: a few layers of narrow pieces that widen up into the face. R14's small-overhang removal
//! (§2.7) took those first layers for small clusters of their own and dropped them, so the face
//! printed over air there. Fab keeps what its own overhang measure counts.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{plan_frames_outlines, plan_frames_supports, Frame, PartField};
use makepad_fab_slicer::geom::*;
use makepad_fab_toolpath::par::Ctl;
use std::collections::BTreeMap;

const SCRIPTS: [(&str, &str); 1] = [("lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash"))];

/// The user's Lattice cube: trees from the bed only, 2 mm off its walls, at 46 deg.
const PLATE: &str = "use mod.fab.*
settings({enable_support: \"1\" support_type: \"tree(auto)\" support_on_build_plate_only: \"1\" support_object_xy_distance: \"2\" support_threshold_angle: \"46\"})
script(\"lattice_cube.cad.splash\", params: {lattice_period: 13.2195mm lattice_size: 27mm})
place(\"lattice_cube\", object: \"lattice\", at: [80mm 165mm], scale: 1.6, name: \"Lattice cube\", settings: {sparse_infill_density: \"100%\"})
";

fn evaluated() -> Evaluated {
    let scripts: BTreeMap<String, String> = SCRIPTS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let m = FabModel::load("plate", PLATE, scripts).unwrap_or_else(|f| panic!("{f}"));
    m.evaluate(&mut Evaluator::new(Machine::dual_nozzle_350()), None).unwrap_or_else(|f| panic!("{f}")).1
}

fn setup(ev: &Evaluated) -> JobSetup {
    let mut setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    setup.machine.values.set_vec("printable_area", ev.machine.bed.corners());
    for (k, v) in &ev.plate.settings {
        setup.process.values.set_str(k, v);
    }
    setup
}

fn frames_of(ev: &Evaluated) -> Vec<Frame> {
    ev.frames.iter().enumerate().map(|(i, f)| {
        let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
        Frame { local: f.local.clone(), offset: f.offset, field }
    }).collect()
}

#[test]
fn the_lattice_cubes_low_points_are_held() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let ev = evaluated();
    let (setup, frames) = (setup(&ev), frames_of(&ev));
    let supports = plan_frames_supports(Ctl::default(), &setup, &ev.plate, &frames, &[], None).unwrap_or_else(|f| panic!("{f}")).remove(0);
    let (outline, _, _) = plan_frames_outlines(Ctl::default(), &setup, &ev.plate, &frames, &[]).unwrap_or_else(|f| panic!("{f}")).remove(0);
    let mut bb = BBox::empty();
    outline.iter().flatten().flat_map(|e| e.contour.pts.iter()).for_each(|&p| bb.add(p));
    let mid = [(bb.min.x_mm() + bb.max.x_mm()) / 2.0, (bb.min.y_mm() + bb.max.y_mm()) / 2.0];
    let sk = &supports.skeleton;
    let hold = sk.hold.expect("trees measure what they hold");
    let left: Vec<String> = sk.given_up.iter().map(|g| format!("L{} {:.2} mm2 at ({:.1}, {:.1})", g.layer, g.area, g.at[0] - mid[0], g.at[1] - mid[1])).collect();
    eprintln!("lattice cube: held {:.1} of {:.1} mm2; left {left:?}", hold.held, hold.need);
    assert!(hold.held >= 0.96 * hold.need, "held {:.1} of {:.1} mm2", hold.held, hold.need);
    // The south-west low point, at 11.4 to 12.2 mm, 13.5 mm west and 11.4 mm south of the middle.
    let sw: Vec<&String> = sk.given_up.iter().zip(&left).filter(|(g, _)| (54..=62).contains(&g.layer) && (g.at[0] - mid[0] + 13.5).hypot(g.at[1] - mid[1] + 11.4) < 3.0).map(|(_, t)| t).collect();
    assert!(sw.is_empty(), "the south-west low point is left without support: {sw:?}");
}
