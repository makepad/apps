//! An exact part through the slicer: at print tolerance, and its layers follow the true surface.

use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::plate::slice_plate;

const MOUNT: &str = include_str!("../../app/resources/motor_mount.cad.splash");

fn closed(m: &fab_plate::PartMesh) -> bool {
    let mut edges = std::collections::HashMap::<(u32, u32), i32>::new();
    for t in &m.triangles {
        for (x, y) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let (lo, hi, s) = if x < y { (x, y, 1) } else { (y, x, -1) };
            *edges.entry((lo, hi)).or_default() += s;
        }
    }
    edges.values().all(|v| *v == 0)
}

#[test]
fn the_motor_mount_slices_at_print_tolerance() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let m = FabModel::with_script("motor_mount", "motor_mount.cad.splash", MOUNT);
    let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    let part = &ev.plate.parts[0];
    let fine = (part.print_mesh.as_ref().expect("an exact part has a print mesh").0)().unwrap();
    assert!(fine.triangles.len() > part.mesh.triangles.len());
    assert!(closed(&part.mesh), "the display mesh is open");
    assert!(closed(&fine), "the print mesh is open");
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.4");
    let setup = JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    let res = slice_plate(&setup, &ev.plate).unwrap();
    assert!(res.layers().len() > 50, "{} layers", res.layers().len());
}

/// The motor mount sliced from the evaluator's frames, as the document slice and the supports
/// tool take them: its frame's offset is its place on the bed and its print mesh is in plate
/// coordinates, and every layer lands where the view shows the part, as the plate's slice does.
#[test]
fn an_exact_part_sliced_from_its_frame_lands_where_the_view_shows_it() {
    use makepad_fab_slice::live::{slice_frames, Frame};
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let m = FabModel::with_script("motor_mount", "motor_mount.cad.splash", MOUNT);
    let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    let (part, frame) = (&ev.plate.parts[0], &ev.frames[0]);
    assert!(part.print_mesh.is_some() && ev.fields[0].is_none(), "an exact part");
    assert!(frame.offset[0].abs() > 10.0 && frame.offset[1].abs() > 10.0, "its frame sits on the bed: {:?}", frame.offset);
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.4");
    let setup = JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    let frames = vec![Frame { local: frame.local.clone(), offset: frame.offset, field: None }];
    let from_frame = slice_frames(&setup, &ev.plate, &frames, &[], None).expect("slices from its frame");
    let on_plate = slice_plate(&setup, &ev.plate).unwrap();
    // each slice's model outline over all its layers, against the view's mesh
    let reach = |res: &makepad_fab_slice::SliceResult| {
        let mut b = makepad_fab_slicer::geom::BBox::empty();
        for o in res.toolpaths.layers.iter().flat_map(|l| &l.objects).filter(|o| !o.support_only) {
            b.merge(&makepad_fab_slicer::geom::bbox_of(&o.slices));
        }
        [b.min.x_mm(), b.min.y_mm(), b.max.x_mm(), b.max.y_mm()]
    };
    let shown = part.mesh.bounds();
    let view = [shown.min[0], shown.min[1], shown.max[0], shown.max[1]];
    for (how, got) in [("from its frame", reach(&from_frame)), ("on the plate", reach(&on_plate))] {
        let off = got.iter().zip(&view).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        assert!(off < 0.3, "sliced {how}, the part's layers reach {got:?}, the view shows it at {view:?} ({off:.3} mm off)");
    }
}
