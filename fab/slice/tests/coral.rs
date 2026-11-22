//! Coral through the cube's plate script, slice, G-code and preview records.

use fab_model::eval::Evaluator;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_gcode::record::MoveKind;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use makepad_fab_toolpath::{par, prof, Params, Role};
use std::collections::{BTreeMap, BTreeSet};

const PLATE: &str = "use mod.fab.*
settings({enable_support: \"1\" support_type: \"coral(auto)\" support_on_build_plate_only: \"1\" support_object_xy_distance: \"0.35\" support_threshold_angle: \"45\" support_remove_small_overhang: \"0\" layer_height: \"0.3\" initial_layer_print_height: \"0.3\"})
script(\"lattice_cube.cad.splash\", params: {lattice_period: 6mm lattice_size: 12mm})
place(\"lattice_cube\", object: \"lattice\", at: [90mm 160mm], name: \"Block\")
";

/// What a slice of `plate` makes, and how long each stage took.
fn slice_plate(plate: &str) -> (makepad_fab_slice::SliceResult, Vec<(prof::Stage, f64)>) {
    let scripts: BTreeMap<String, String> = [("lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash"))].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let model = FabModel::load("plate", plate, scripts).expect("plate loads");
    let ev = model.evaluate(&mut Evaluator::new(Machine::dual_nozzle_350()), None).expect("plate evaluates").1;
    let mut setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default(); 2], filament_map: vec![1, 2], nozzle_volume_type: vec!["Standard".into(); 2], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None; 2] };
    for (k, v) in &ev.plate.settings { setup.process.values.set_str(k, v); }
    let frames: Vec<Frame> = ev.frames.iter().enumerate().map(|(i, f)| {
        let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
        Frame { local: f.local.clone(), offset: f.offset, field }
    }).collect();
    prof::take();
    prof::enable(true);
    let sliced = slice_frames(&setup, &ev.plate, &frames, &[], None);
    prof::enable(false);
    (sliced.expect("plate slices"), prof::take())
}

#[test]
fn coral_slices_to_gcode_on_the_cube() {
    par::set_threads(par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)));
    let (result, stages) = slice_plate(PLATE);
    let params = Params::from_job(&result.job, 0);
    assert!(params.coral && params.support.is_some() && params.normal_support.is_none(), "coral uses the support settings");
    assert!(stages.iter().any(|&(s, t)| s == prof::Stage::CoralSupport && t > 0.0), "coral generator ran");
    assert!(stages.iter().all(|&(s, t)| s != prof::Stage::TreeSupport || t == 0.0), "tree generator did not run");
    let mut planned_layers = 0;
    for layer in result.layers() {
        let paths: Vec<_> = layer.objects.iter().flat_map(|o| &o.islands).flat_map(|i| &i.fills).filter(|p| matches!(p.role, Role::Support | Role::SupportInterface)).collect();
        if !paths.is_empty() { planned_layers += 1; }
        assert!(paths.iter().all(|p| !p.closed && p.loop_pts.is_empty() && p.area > 0.0), "coral stays open extrusion with positive flow");
    }
    let printed_layers: BTreeSet<_> = result.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && m.e > 0.0 && matches!(m.role, Role::Support | Role::SupportInterface)).map(|m| m.layer).collect();
    assert!(planned_layers >= 3, "support paths on several layers: {planned_layers}");
    assert!(printed_layers.len() >= 3, "G-code extrudes support on several layers: {}", printed_layers.len());
    assert!(result.gcode.contains("; FEATURE: Support"), "G-code identifies the support role");
    eprintln!("coral cube: {planned_layers} planned support layers, {} G-code support layers", printed_layers.len());
}
