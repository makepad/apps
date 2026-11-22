//! The non-planar top's hooks in the planner (NONPLANAR-DESIGN §3): planar layers leave out the
//! plan's cap clip, and its regions come out as blocks after their schedule layer.

use crate::*;
use makepad_fab_slicer::nonplanar::{plan, NpPlan, NpSettings};
use makepad_fab_slicer::sdf::SdfSource;

/// A 24 x 16 block whose top rises 4 deg along X (5.3 -> 7 mm), centred at (100, 100).
fn wedge(x: f64, y: f64, z: f64) -> f64 {
    let (x, y) = (x - 100.0, y - 100.0);
    let (s, c) = 4f64.to_radians().sin_cos();
    (x.abs() - 12.0).max(y.abs() - 8.0).max(-z).max(-s * x + c * z - c * 6.1)
}
const WEDGE: ([f64; 3], [f64; 3]) = ([88.0, 92.0, 0.0], [112.0, 108.0, 7.1]);

struct NpWedge {
    src: SdfSource,
    np: Option<Arc<NpPlan>>,
}

impl ContourSource for NpWedge {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.src.bounds
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        self.src.contours(zs)
    }
    fn np_plan(&self) -> Option<Arc<NpPlan>> {
        self.np.clone()
    }
}

fn job() -> Job {
    let mut cfg = makepad_fab_settings::config::Config::new();
    for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(key, "zig-zag");
    }
    for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "2"), ("top_shell_layers", "4"), ("sparse_infill_density", "15%")] {
        cfg.set_str(key, value);
    }
    cfg.set_vec("filament_map", vec!["1".into(), "1".into()]);
    Job::new(cfg)
}

fn input(np: Option<Arc<NpPlan>>) -> ObjectInput {
    ObjectInput { overrides: Vec::new(), name: "wedge".into(), source: Box::new(NpWedge { src: SdfSource::new(Arc::new(wedge), WEDGE), np }), filament: 0 }
}

#[test]
fn planar_layers_leave_out_the_cap_and_regions_print_as_blocks() {
    let np = Arc::new(plan(&wedge, WEDGE, &NpSettings::default(), &np_test_head()).expect("the wedge plans"));
    assert!(!np.regions.is_empty(), "the gentle wedge has a non-planar region");
    let job = job();
    let flat = plan_one(par::Ctl::default(), 0, &input(None), &job).unwrap();
    let with = plan_one(par::Ctl::default(), 0, &input(Some(np.clone())), &job).unwrap();
    assert!(flat.np_blocks.is_empty());
    assert_eq!(flat.layers.len(), with.layers.len());
    // Each planar layer is its flat slice minus the clip at its top.
    let mut clipped = 0;
    for ((z, _, a), (_, _, b)) in flat.layers.iter().zip(&with.layers) {
        let area = |v: &[ExPolygon]| v.iter().map(|e| e.area_mm2()).sum::<f64>();
        let want = area(&makepad_fab_slicer::clip::difference_ex(&a.slices, &np.cap_clip(*z)));
        // The flat slice minus the clip, less only the hair-thin slivers the clip leaves.
        let got = area(&b.slices);
        assert!(got <= want + 1e-6 && want - got < 1.0, "layer at {z}: the slice ({got} mm2) is the flat one minus the clip ({want} mm2)");
        if area(&b.slices) < area(&a.slices) - 1e-3 {
            clipped += 1;
        }
    }
    assert!(clipped > 0, "some planar layers lose their capped columns");
    // One block per region, after the last layer at or below its schedule z, the rings in order.
    assert_eq!(with.np_blocks.len(), np.regions.len());
    for (b, r) in with.np_blocks.iter().zip(&np.regions) {
        assert_eq!(b.region, r.id);
        assert!(b.z <= r.schedule_z + 1e-9 && with.layers.get(b.layer + 1).map_or(true, |l| l.0 > r.schedule_z + 1e-9));
        assert_eq!(with.layers[b.layer].0, b.z);
        let tops: Vec<f64> = with.layers.iter().map(|l| l.0).collect();
        let want = crate::nonplanar::block_paths(&np, r, b.z, &tops, b.paths.first().map_or(0.0, |p| p.speed));
        assert_eq!(b.paths.len(), want.len());
        assert!(!b.paths.is_empty());
        for (p, q) in b.paths.iter().zip(&want) {
            assert_eq!((&p.pts, &p.z, &p.flow), (&q.pts, &q.z, &q.flow));
        }
    }
    // Placed at an offset, the blocks move with the part and index the plate's layers.
    let d = Pt::from_mm(5.0, -3.0);
    let tp = merge(vec![(Arc::new(with.clone()), d)]);
    assert_eq!(tp.np_blocks.len(), with.np_blocks.len());
    for (a, b) in tp.np_blocks.iter().zip(&with.np_blocks) {
        assert_eq!((a.object, tp.layers[a.layer].print_z), (0, b.z));
        assert_eq!(a.paths[0].pts[0], b.paths[0].pts[0].add(d));
    }
    // Assembled windows keep the same clipped layers and non-planar blocks.
    let o = input(Some(np));
    let heights = layers::object_heights(&*o.source, &Params::from_job(&job, o.filament));
    let zs: Vec<_> = heights.iter().map(|(z, h)| z - h / 2.0).collect();
    let outline = plan_outline(par::Ctl::default(), &o, &job, &heights, o.source.contours(&zs)).unwrap();
    let layers = plan_window(par::Ctl::default(), 0, &o, &job, &heights, 0, heights.len()).unwrap();
    let supports = plan_supports(par::Ctl::default(), 0, &o, &job, &outline, &heights, &mut Default::default()).unwrap();
    let again = finish_plan(&o, &job, &heights, &outline, layers, supports);
    assert_eq!(format!("{:?}", again.np_blocks), format!("{:?}", with.np_blocks));
    for ((_, _, a), (_, _, b)) in again.layers.iter().zip(&with.layers) {
        assert_eq!(a.slices, b.slices);
    }
}
