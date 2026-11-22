//! A preset's text cannot add a line to the G-code outside the executable block: the header and
//! the settings block are comments, whatever a field holds.

use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::{slice, PlacedObject};
use makepad_fab_slicer::mesh::Mesh;

fn cube() -> Mesh {
    let verts = (0..8u32).map(|i| [100.0 + 10.0 * (i & 1) as f64, 100.0 + 10.0 * ((i >> 1) & 1) as f64, 2.0 * ((i >> 2) & 1) as f64]).collect();
    let tris = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
    Mesh { verts, tris, paint: Vec::new() }
}

#[test]
fn a_line_break_in_a_preset_stays_in_its_comment() {
    let mut filament = Preset::default();
    filament.values.set_vec("filament_density", vec!["1.24\nM104 S400".into()]);
    filament.values.set_vec("made_up_list", vec!["a\nG28".into(), "b\rG28".into()]);
    filament.values.set_str("made_up\nG28", "1");
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.4");
    let setup = JobSetup {
        machine: Preset::default(), process, filaments: vec![filament], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()],
        bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None],
    };
    let gcode = slice(&setup, vec![PlacedObject { name: "cube".into(), mesh: cube(), filament: 0 }]).expect("slices").gcode;
    let (start, end) = (gcode.find("; EXECUTABLE_BLOCK_START").expect("a block"), gcode.find("; EXECUTABLE_BLOCK_END").expect("an end"));
    for line in gcode[..start].lines().chain(gcode[end..].lines()) {
        assert!(line.is_empty() || line.starts_with(';'), "outside the block: {line:?}");
    }
    assert!(!gcode.contains('\r'), "a CR is a line end to some firmware");
}
