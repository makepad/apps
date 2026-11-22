//! A field part's box holds its solid: every demo part's, and the vase's under a sculpt. A raise
//! drawn round the vase's belly with a 24 mm brush keeps every layer (the rim prints) and keeps
//! the part where it stood (its box moved by nothing the stroke does not reach). A Splash
//! kernel's declared bounds are its solid's box: the contract its part is placed and sliced by.

use fab_model::eval::{Evaluated, Evaluator, PartField as ModelField, EXTENT_TOL};
use fab_model::{Change, FabModel, PlaceArg};
use fab_plate::machine::Machine;
use makepad_csg_sdf::strokes::{StrokeOp, Strokes};
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use std::collections::BTreeMap;
use std::sync::Arc;

const PARTS: [(&str, &str); 8] = [
    ("bracket", include_str!("../../app/resources/bracket.cad.splash")),
    ("chain", include_str!("../../app/resources/chain.cad.splash")),
    ("chainmail", include_str!("../../app/resources/chainmail.cad.splash")),
    ("knurled_knob", include_str!("../../app/resources/knurled_knob.cad.splash")),
    ("lattice_cube", include_str!("../../app/resources/lattice_cube.cad.splash")),
    ("motor_mount", include_str!("../../app/resources/motor_mount.cad.splash")),
    ("slice_bench_plate", include_str!("../../app/resources/slice_bench_plate.cad.splash")),
    ("vase", include_str!("../../app/resources/vase.cad.splash")),
];

/// The part evaluated; the lattice cube at its smallest (its Splash kernel costs seconds at the
/// default size).
fn build(name: &str, source: &str) -> Evaluated {
    let mut m = FabModel::with_script(name, &format!("{name}.cad.splash"), source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{name}: {f}")).1;
    if name != "lattice_cube" {
        return ev;
    }
    let head = m.head();
    let change = Change::Param { instance: name.into(), name: "lattice_size".into(), value: Some(0.010) };
    m.apply(&mut e, "small", head, &[change], None).expect("the lattice resizes").1.expect("it rebuilds").1
}

/// No solid past a side of the field part's box: on a grid across each side (about 120 points a
/// row), just past it and a little farther, read in the shape's frame. (Just past: 1e-5 mm, as
/// an f32 kernel's solid ends up to half an ulp past a face its box is on.)
fn holds(name: &str, pf: &ModelField) {
    let b = pf.bounds.unwrap_or_else(|| panic!("{name}: no box"));
    let f = pf.shape.field();
    let m = pf.to_part;
    let s2 = m[0][0] * m[0][0] + m[1][0] * m[1][0] + m[2][0] * m[2][0];
    let shape = |q: [f64; 3]| -> [f64; 3] {
        let d = [q[0] - m[0][3], q[1] - m[1][3], q[2] - m[2][3]];
        std::array::from_fn(|c| (m[0][c] * d[0] + m[1][c] * d[1] + m[2][c] * d[2]) / s2)
    };
    let step = (0..3).map(|k| b.max[k] - b.min[k]).fold(0.0f64, f64::max) / 120.0;
    for axis in 0..3 {
        let (j, k) = ((axis + 1) % 3, (axis + 2) % 3);
        let n = |a: usize| ((b.max[a] - b.min[a]) / step) as usize + 1;
        for (side, sign) in [(b.max[axis], 1.0), (b.min[axis], -1.0)] {
            for off in [1e-5, 0.02, 0.3] {
                for s in 0..n(j) {
                    for t in 0..n(k) {
                        let mut q = [0.0; 3];
                        q[axis] = side + sign * off;
                        q[j] = b.min[j] + (s as f64 + 0.37) * step;
                        q[k] = b.min[k] + (t as f64 + 0.61) * step;
                        let p = shape(q);
                        let v = f.distance(makepad_csg_math::Vec3d::new(p[0], p[1], p[2]));
                        assert!(v > 0.0, "{name}: solid at {q:?} (part frame) past its box {:?}..{:?}", b.min, b.max);
                    }
                }
            }
        }
    }
}

#[test]
fn every_demo_parts_box_holds_its_solid() {
    let mut fields = 0;
    for (name, source) in PARTS {
        let ev = build(name, source);
        for pf in ev.fields.iter().flatten() {
            holds(name, pf);
            fields += 1;
        }
    }
    assert!(fields >= 3, "only {fields} field parts");
}

/// The Splash kernels in a field's tree, each with the scale its frame is under (a kernel's frame
/// is in metres).
fn kernels(n: &makepad_csg_sdf::SdfNode, scale: f64, out: &mut Vec<(makepad_csg_sdf::SdfKernel, f64)>) {
    use makepad_csg_sdf::SdfNode as N;
    match n {
        N::Kernel(k) => out.push((k.clone(), scale)),
        N::Transform { scale: s, of, .. } => kernels(of, scale * s, out),
        N::Combine { of, .. } => of.iter().for_each(|c| kernels(c, scale, out)),
        N::Material { of, within, .. } => {
            kernels(of, scale, out);
            if let Some(w) = within {
                kernels(w, scale, out);
            }
        }
        N::Shell { of, .. } | N::Round { of, .. } | N::Instance { of, .. } | N::Bone { of, .. } | N::Deform { of, .. } | N::Id { of, .. } | N::Sculpt { of, .. } | N::Texture { of, .. } => kernels(of, scale, out),
        _ => {}
    }
}

/// Each side a demo kernel declares is within the extent tolerance of a solid point.
#[test]
fn every_demo_kernels_bounds_are_its_solids_box() {
    let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    let mut count = 0;
    for (name, source) in PARTS {
        let ev = build(name, source);
        let mut found = Vec::new();
        for pf in ev.fields.iter().flatten() {
            kernels(&pf.shape.root, 1.0, &mut found);
        }
        for (k, scale) in found {
            let (lo, hi) = k.bounds.unwrap_or_else(|| panic!("{name}: a kernel without bounds"));
            let shape = makepad_csg_sdf::SdfShape { root: makepad_csg_sdf::SdfNode::Kernel(k), min: lo, max: hi, materials: Vec::new() };
            let tol = EXTENT_TOL / scale;
            let e = makepad_csg_sdf::shape_bounds(&shape, &identity, tol).unwrap_or_else(|| panic!("{name}: no solid"));
            let (a, b) = e.found.unwrap_or_else(|| panic!("{name}: no solid point"));
            let mm = |p: [f64; 3]| p.map(|x| (x * scale * 1e4).round() / 1e4);
            // its solid reaches every side it declares (the box holding it: `every_demo_parts_box_holds_its_solid`)
            assert!((0..3).all(|i| a[i] - lo[i] <= tol && hi[i] - b[i] <= tol), "{name}: declares {:?}..{:?} mm, its solid reaches {:?}..{:?}", mm(lo), mm(hi), mm(a), mm(b));
            count += 1;
        }
    }
    assert!(count >= 2, "only {count} kernels");
}

/// Layers 0.2 mm thick on the first plate setup.
fn setup() -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.2");
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

/// The vase's first part sliced as the app slices it: over its box, in its part frame.
fn sliced(ev: &Evaluated) -> makepad_fab_slice::SliceResult {
    let pf = ev.fields[0].as_ref().expect("the vase is a field part");
    let mut field = PartField::new(pf.shape.clone(), pf.to_part);
    field.bounds = pf.bounds.map(|b| (b.min, b.max));
    let fr = &ev.frames[0];
    let frames = vec![Frame { local: fr.local.clone(), offset: fr.offset, field: Some(field) }];
    slice_frames(&setup(), &ev.plate, &frames, &[], None).expect("slices from the field")
}

/// One raise point (a click with no drag) adds a dab, and every layer keeps its outline: its
/// capsule has no length, which must not read as no surface on the layers it reaches.
#[test]
fn a_one_point_raise_keeps_every_layer() {
    let areas = |strokes: Option<Strokes>| -> Vec<f64> {
        let root = makepad_csg_sdf::SdfNode::Cylinder { radius: 15.0, top_radius: 15.0, height: 30.0 };
        let shape = makepad_csg_sdf::SdfShape { root, min: [-15.0; 3], max: [15.0; 3], materials: Vec::new() };
        let shape = match strokes {
            Some(s) => shape.sculpted(Arc::new(s)),
            None => shape,
        };
        // the model's Y up
        let up = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 15.0]];
        let source = PartField::new(Arc::new(shape), up).source().expect("a field source");
        let zs: Vec<f64> = (0..30).map(|k| k as f64 + 0.5).collect();
        source.contours(&zs).iter().map(|l| l.iter().map(|e| e.area_mm2()).sum()).collect()
    };
    let mut strokes = Strokes::new();
    strokes.push(StrokeOp::Raise, 3.0, 1.0, &[[15.0, 3.5, 0.0]]);
    let (plain, dabbed) = (areas(None), areas(Some(strokes)));
    let full = std::f64::consts::PI * 15.0 * 15.0;
    for (k, (a, b)) in plain.iter().zip(&dabbed).enumerate() {
        assert!((a - full).abs() < 0.01 * full, "layer {k} of the plain cylinder holds {a} mm2");
        assert!(*b >= a - 0.01 && *b < a + 30.0, "layer {k}: {a} mm2 plain, {b} mm2 under the dab");
    }
    assert!(dabbed[18] > plain[18] + 0.1, "the dab raised nothing: {} mm2 from {} mm2", dabbed[18], plain[18]);
}

#[test]
fn a_belly_raise_keeps_every_layer_and_the_place() {
    let source = format!("{}\nm.strokes(\"vase.strokes\")\n", include_str!("../../app/resources/vase.cad.splash"));
    let logs = BTreeMap::from([("vase.strokes".to_string(), Arc::new(Strokes::new()))]);
    let mut model = FabModel::with_script_files("vase", "vase.cad.splash", &source, BTreeMap::new(), logs);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let (c, before) = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    let pf = before.fields[0].as_ref().expect("the vase is a field part");
    let object = before.plate.parts[0].object.clone();
    // A 24 mm brush (12 mm radius) at its default strength, drawn 12 mm round the belly's widest
    // ring (the model frame: Y up, the belly 0.34 of the 70 mm height up and 21 mm out).
    let points: Vec<[f64; 3]> = (0..7).map(|k| {
        let a = (k as f64 - 3.0) * 2.0 / 21.0;
        [21.0 * a.cos(), 23.8, 21.0 * a.sin()]
    }).collect();
    let raise = Change::Stroke { file: "vase.strokes".into(), op: StrokeOp::Raise, rgb: None, radius: 12.0, strength: 0.5, points };
    // the app's sculpt pins a free part at its origin's place (paint_edit)
    let pin = Change::place(&c, &object, PlaceArg::At(Some([pf.to_printer[0][3], pf.to_printer[1][3]]))).expect("the vase's object");
    let after = model.apply(&mut e, "belly", model.head(), &[raise, pin], None).unwrap_or_else(|f| panic!("{f:?}")).1.expect("it rebuilds").1;
    let (b0, b1) = (pf.bounds.expect("a box"), after.fields[0].as_ref().and_then(|f| f.bounds).expect("a box"));
    assert_eq!(after.frames[0].offset, before.frames[0].offset, "the part moved: box {b0:?} -> {b1:?}");
    assert!(b1.max[0] - b1.min[0] > b0.max[0] - b0.min[0] + 1.0, "the raise widened nothing: {b0:?} -> {b1:?}");
    assert_eq!((b1.min[2], b1.max[2]), (b0.min[2], b0.max[2]), "the belly's raise moved the foot or the rim");
    holds("sculpted vase", after.fields[0].as_ref().unwrap());
    // The raise keeps every layer of the unsculpted vase, up to the rim's.
    let mut count = None;
    for (label, ev) in [("before", &before), ("after", &after)] {
        let s = sliced(ev);
        let layers = s.layers();
        assert_eq!(*count.get_or_insert(layers.len()), layers.len(), "{label}: the layer count changed");
        let top = layers.last().unwrap();
        let area: f64 = top.objects.iter().flat_map(|o| o.slices.iter()).map(|e| e.area_mm2()).sum();
        assert!(area > 1.0 && top.print_z > 64.5, "{label}: the rim's layer (z = {:.2} mm) holds {area} mm2", top.print_z);
    }
}
