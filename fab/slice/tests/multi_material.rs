//! Colour end to end: a two-colour SDF part (a model with a painted solid)
//! and a painted mesh slice to dual-filament G-code (filament 1 on the left
//! nozzle, 2 on the right). The model's print colours map onto the job's tools
//! (red on tool 2). (The printer safety check of this G-code needs a real machine's
//! presets, which the tests do not have: it is checked with the app's presets.)

use fab_model::eval::Evaluator;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use fab_plate::{Part, PartMesh, Plate};
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use std::sync::Arc;

const TWO_COLOUR: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\nlet red = Material{albedo: [0.8 0.05 0.05] name: \"Red\"}\nlet body = m.blend([m.box({size: [30mm 30mm 20mm]}) m.sphere({radius: 13mm})], 1mm)\nobject(body)\nm.fill(m.sphere({radius: 13mm at: [0 18mm 0]}), red)\n";
const PLAIN: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\nlet body = m.blend([m.box({size: [30mm 30mm 20mm]}) m.sphere({radius: 13mm})], 1mm)\nobject(body)\n";

fn model(src: &str) -> fab_model::eval::Evaluated {
    let (m, mut e) = (FabModel::with_script("mm", "mm.cad.splash", src), Evaluator::new(Machine::dual_nozzle_350()));
    let mut ev = m.evaluate(&mut e, None).expect("evaluates").1;
    // The job puts red on tool 2 (right) and the part's own colour on tool 1 (left).
    let colors = ev.part_colors.clone();
    ev.plate.map_colors(&colors, &|name| Some(if name == "Red" { (2, 1) } else { (1, 0) }));
    ev
}

/// A 20 mm cube at `at` with its +x side painted filament 2 (and the paint per triangle).
fn painted_cube(at: [f64; 3]) -> (Part, Vec<String>) {
    let positions = (0..8u32).map(|i| [at[0] + 20.0 * (i & 1) as f64, at[1] + 20.0 * ((i >> 1) & 1) as f64, at[2] + 20.0 * ((i >> 2) & 1) as f64]).collect();
    let triangles = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
    let paint = (0..12).map(|i| if i >= 10 { makepad_fab_slicer::paint::encode_leaf(2) } else { String::new() }).collect();
    (Part { body_of: None, print_mesh: None, paint: None, object: "cube".into(), name: "cube".into(), filament: 1, color: [0.9, 0.9, 0.9, 1.0], nozzle: None, mesh: PartMesh { positions, triangles }, print: Default::default(), field_bounds: None, field_floor: None, ..Default::default() }, paint)
}

/// Two filaments on one nozzle from the option defaults, thick layers.
fn setup() -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.4");
    JobSetup {
        machine: Preset::default(),
        process,
        filaments: vec![Preset::default(), Preset::default()],
        filament_map: vec![1, 1],
        nozzle_volume_type: vec!["Standard".into()],
        bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None, None],
    }
}

/// A two-colour field part and a painted mesh cube slice together to dual-filament G-code
/// with tool changes, and a model with no colour adds no paint.
#[test]
fn two_colour_sdf_and_painted_mesh_slice_to_dual_filament_gcode() {
    let plain = model(PLAIN);
    assert!(plain.paint.iter().all(|p| p.is_none()), "an uncoloured model has no paint");
    assert!(plain.plate.parts.iter().all(|p| p.paint.is_none()), "an uncoloured part has no paint");
    let ev = model(TWO_COLOUR);
    assert!(ev.paint[0].is_some(), "the model part is two-coloured");
    // The model part's paint is on the part, by position; red maps to tool 2.
    let sp = ev.plate.parts[0].paint.as_ref().expect("painted by position");
    assert_eq!(sp.states(1), vec![0, 2], "red maps to tool 2");
    // The part is a field part (it has no mesh): sliced from its field, beside the cube.
    let pf = ev.fields[0].as_ref().expect("a field part");
    let fr = &ev.frames[0];
    let mut to = pf.to_printer;
    for (r, o) in fr.offset.iter().enumerate() {
        to[r][3] -= o;
    }
    let (cube, cube_paint) = painted_cube([-10.0, -10.0, 0.0]);
    let at = [fr.offset[0] + 40.0, fr.offset[1], 0.0];
    let mut placed = cube.clone();
    placed.mesh.translate(at);
    let frames = vec![Frame { local: fr.local.clone(), offset: fr.offset, field: Some(PartField::new(pf.shape.clone(), to)) }, Frame { local: Arc::new(cube.mesh), offset: at, field: None }];
    let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![ev.plate.parts[0].clone(), placed] };
    let r = slice_frames(&setup(), &plate, &frames, &[Vec::new(), cube_paint], None).expect("sliced");
    assert_eq!(r.stats.used, vec![0, 1], "both filaments print");
    assert!(r.timeline.tool_changes >= 2, "tool changes: {}", r.timeline.tool_changes);
}
