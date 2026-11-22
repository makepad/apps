//! A contour that touches itself at a vertex (a zero-width pinch) plans as the
//! pieces it splits into, and no path leaves the part.

use crate::*;

/// The same regions on every layer up to 1 mm.
struct Fixed(Vec<ExPolygon>);

impl ContourSource for Fixed {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        ([90.0, 90.0, 0.0], [110.0, 110.0, 1.0])
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        zs.iter().map(|&z| if z < 1.0 { self.0.clone() } else { Vec::new() }).collect()
    }
}

#[test]
fn a_pinched_square_plans_as_its_pieces_inside_the_part() {
    let pt = |x: f64, y: f64| Pt::from_mm(x, y);
    // Two 10 mm squares meeting at one corner, as one ring through it twice.
    let v = pt(100.0, 100.0);
    let low = [v, pt(90.0, 100.0), pt(90.0, 90.0), pt(100.0, 90.0)];
    let high = [v, pt(110.0, 100.0), pt(110.0, 110.0), pt(100.0, 110.0)];
    let pinched = vec![ExPolygon::new(Polygon::new(low.iter().chain(&high).copied().collect()))];
    let split = vec![ExPolygon::new(Polygon::new(low.to_vec())), ExPolygon::new(Polygon::new(high.to_vec()))];
    assert_eq!(split_pinches(pinched.clone()), split, "the ring splits at the vertex it passes twice");

    let mut cfg = makepad_fab_settings::config::Config::new();
    for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(key, "zig-zag");
    }
    for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "2"), ("sparse_infill_density", "15%")] {
        cfg.set_str(key, value);
    }
    let job = Job::new(cfg);
    let plan = |regions: Vec<ExPolygon>| {
        let o = ObjectInput { overrides: Vec::new(), name: "pinch".into(), source: Box::new(Fixed(regions)), filament: 0 };
        plan_one(par::Ctl::default(), 0, &o, &job).unwrap()
    };
    let (a, b) = (plan(pinched), plan(split));
    assert!(!a.layers.is_empty());
    assert_eq!(format!("{a:?}"), format!("{b:?}"), "the pinched ring plans as its pieces");
    let part = BBox::of(&[pt(90.0, 90.0), pt(110.0, 110.0)]);
    for (z, _, layer) in &a.layers {
        for p in layer.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
            assert!(p.pts.iter().all(|q| q.x >= part.min.x && q.x <= part.max.x && q.y >= part.min.y && q.y <= part.max.y), "a {:?} path at z {z} leaves the part", p.role);
        }
    }
}
