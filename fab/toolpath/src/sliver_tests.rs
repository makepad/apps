//! Where an island gets too thin for the outer wall to go round: the wall closes it off where it
//! is the narrowest printable bead wide, and the sliver past that prints nothing. No infill or gap
//! fill comes out to the surface, no line there is narrower than a printable bead, and none hangs
//! past what the layer below laid down further than a bead bridges.

use crate::*;
use makepad_fab_slicer::grid::SegGrid;

/// A part whose regions at each height a function gives.
struct Shaped {
    shape: fn(f64) -> Vec<ExPolygon>,
    top: f64,
}

impl ContourSource for Shaped {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for z in [0.0, self.top] {
            for q in (self.shape)(z).iter().flat_map(|e| e.contour.pts.iter()) {
                lo = [lo[0].min(q.x_mm()), lo[1].min(q.y_mm())];
                hi = [hi[0].max(q.x_mm()), hi[1].max(q.y_mm())];
            }
        }
        ([lo[0], lo[1], 0.0], [hi[0], hi[1], self.top])
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        zs.iter().map(|&z| if z < self.top { (self.shape)(z) } else { Vec::new() }).collect()
    }
}

fn job() -> Job {
    let mut cfg = makepad_fab_settings::config::Config::new();
    for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(key, "zig-zag");
    }
    for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "2"), ("wall_generator", "classic"), ("sparse_infill_density", "15%")] {
        cfg.set_str(key, value);
    }
    Job::new(cfg)
}

fn region(pts: &[(f64, f64)]) -> ExPolygon {
    let mut ring = Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect());
    if ring.area2() < 0 {
        ring.pts.reverse();
    }
    ExPolygon::new(ring)
}

/// A wedge of `angle` degrees with its tip at (`tip`, 100), `length` long, backed by a flat end.
fn wedge(angle: f64, tip: f64, length: f64) -> ExPolygon {
    let half = length * (angle / 2.0).to_radians().tan();
    region(&[(tip, 100.0), (tip - length, 100.0 + half), (tip - length, 100.0 - half)])
}

fn wedge10(_: f64) -> Vec<ExPolygon> {
    vec![wedge(10.0, 100.0, 10.0)]
}
fn wedge20(_: f64) -> Vec<ExPolygon> {
    vec![wedge(20.0, 100.0, 10.0)]
}
fn wedge40(_: f64) -> Vec<ExPolygon> {
    vec![wedge(40.0, 100.0, 10.0)]
}
/// A fin a printable bead wide, standing on the plate.
fn fin(_: f64) -> Vec<ExPolygon> {
    vec![region(&[(90.0, 99.75), (100.0, 99.75), (100.0, 100.25), (90.0, 100.25)])]
}
/// Two struts meeting at a shallow angle, as in a lattice: their outer sides make a tip, the
/// crotch between them a slot, and the joint itself is wide.
fn strut_joint(_: f64) -> Vec<ExPolygon> {
    let (a, w) = (12f64.to_radians(), 1.2);
    let (c, s) = (a.cos(), a.sin());
    // Each strut 8 mm long from the tip, `w` wide, the two at ±a from the x axis.
    let far = 8.0;
    let up = [(100.0, 100.0), (100.0 - far * c, 100.0 + far * s), (100.0 - far * c - w * s, 100.0 + far * s - w * c)];
    let down = [(100.0 - far * c - w * s, 100.0 - far * s + w * c), (100.0 - far * c, 100.0 - far * s)];
    // Where the inner sides of the two struts meet.
    let inner = 100.0 - w / s;
    vec![region(&[up[0], up[1], up[2], (inner, 100.0), down[0], down[1]])]
}
/// Two surfaces meeting at a shallow crease that leans out: each layer's tip lies half a
/// millimetre past the one below, as where a surface grazes the layers.
fn crease_out(z: f64) -> Vec<ExPolygon> {
    vec![wedge(12.0, 100.0 + 2.5 * z, 10.0 + 2.5 * z)]
}
/// The same crease leaning in: each tip sits back on the one below.
fn crease_in(z: f64) -> Vec<ExPolygon> {
    vec![wedge(12.0, 105.0 - 2.5 * z, 15.0 - 2.5 * z)]
}

fn plan(shape: fn(f64) -> Vec<ExPolygon>, top: f64) -> ObjectPlan {
    let o = ObjectInput { overrides: Vec::new(), name: "sliver".into(), source: Box::new(Shaped { shape, top }), filament: 0 };
    plan_one(par::Ctl::default(), 0, &o, &job()).unwrap()
}

fn is_wall(role: Role) -> bool {
    matches!(role, Role::OuterWall | Role::InnerWall | Role::OverhangWall)
}

/// The distance (mm) from `q` to the nearest edge of `regions`.
fn to_edge(regions: &[ExPolygon], q: Pt) -> f64 {
    regions.iter().flat_map(|e| e.rings()).flat_map(|r| (0..r.pts.len()).map(move |i| dist_point_segment(q, r.pts[i], r.pts[(i + 1) % r.pts.len()]))).fold(f64::MAX, f64::min) / SCALE
}

/// Each segment of `path` with the width (spacing, mm) it lays.
fn segments(path: &ExtrusionPath, h: f64) -> Vec<(Pt, Pt, f64)> {
    let n = path.pts.len();
    let s = flow::spacing(path.width, h);
    (0..if path.closed { n } else { n.saturating_sub(1) }).map(|i| (path.pts[i], path.pts[(i + 1) % n], s * path.flow.get(i).copied().unwrap_or(1.0))).collect()
}

/// Points along a segment, a fine step apart.
fn along(a: Pt, b: Pt) -> impl Iterator<Item = Pt> {
    let k = (a.dist(b) / SCALE / 0.02).ceil().max(1.0) as usize;
    (0..=k).map(move |j| lerp(a, b, j as f64 / k as f64))
}

/// Checks every layer of `shape`: no infill or gap fill in the outer wall's place; past where
/// the outer wall's loop turns back (the sliver), every line at least the narrowest printable
/// bead wide, in a place that wide, and on what the layer below laid down to within the overhang
/// rule's support margin. Returns, per layer, how near the outer wall comes to the island's tip,
/// its point furthest in x (mm).
fn check(name: &str, shape: fn(f64) -> Vec<ExPolygon>, top: f64) -> Vec<f64> {
    let plan = plan(shape, top);
    let p = Params::from_job(&job(), 0);
    assert!(!plan.layers.is_empty(), "{name}: plans");
    let margin = walls::support_margin(p.overhang.nozzle);
    let mut below: Option<(Vec<(Pt, Pt, f64)>, f64)> = None;
    let mut reach = Vec::new();
    for (k, (z, h, layer)) in plan.layers.iter().enumerate() {
        let (z, h) = (*z, *h);
        let first = k == 0;
        let w = if first { p.w_first } else { p.w_outer };
        let band = flow::spacing(w, h);
        // The narrowest printable bead, and the width it lays (its spacing).
        let bead = p.gap_fill.min_bead_width;
        let least = flow::spacing(bead, h);
        let outline = &layer.slices;
        // Where the outer wall's loop goes: past it, the island is too thin to go round.
        let r = (w + flow::spacing(w, h)) / 2.0 * SCALE;
        let looped = SegGrid::of_regions(&offset_ex(&opening(outline, r, r, Join::Miter(3.0)), GAP_SIMPLIFY.0 * SCALE, Join::Miter(3.0)), |k| k as u32);
        let paths: Vec<&ExtrusionPath> = layer.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).collect();
        for path in &paths {
            for (a, b, s) in segments(path, h) {
                for q in along(a, b) {
                    let d = to_edge(outline, q);
                    assert!(is_wall(path.role) || d >= band, "{name} z {z:.1}: a {:?} line in the outer wall's place, {d:.3} mm in at ({:.2}, {:.2})", path.role, q.x_mm(), q.y_mm());
                    if looped.contains_any(q) {
                        continue;
                    }
                    assert!(s >= least - 1e-9, "{name} z {z:.1}: a {:?} line {s:.3} mm wide in the sliver at ({:.2}, {:.2}), under {least:.3}", path.role, q.x_mm(), q.y_mm());
                    assert!(d >= bead / 2.0 - GAP_SIMPLIFY.0, "{name} z {z:.1}: a {:?} line where the part is {:.3} mm wide at ({:.2}, {:.2}), under {bead:.3}", path.role, 2.0 * d, q.x_mm(), q.y_mm());
                    if let Some((lower, hl)) = &below {
                        let past = lower.iter().map(|&(c, e, sl)| dist_point_segment(q, c, e) / SCALE - flow::width_for_spacing(sl, *hl) / 2.0).fold(f64::MAX, f64::min);
                        assert!(past <= margin + 1e-6, "{name} z {z:.1}: a {:?} line {past:.3} mm past the layer below at ({:.2}, {:.2})", path.role, q.x_mm(), q.y_mm());
                    }
                }
            }
        }
        let tip = outline.iter().flat_map(|e| e.contour.pts.iter()).copied().max_by_key(|q| q.x).expect("an outline");
        reach.push(paths.iter().filter(|p| matches!(p.role, Role::OuterWall | Role::OverhangWall)).flat_map(|p| p.pts.iter()).map(|q| q.dist(tip) / SCALE).fold(f64::MAX, f64::min));
        below = Some((paths.iter().flat_map(|p| segments(p, h)).collect(), h));
    }
    reach
}

/// How far from a wedge's tip (mm) it gets as wide as the narrowest printable bead.
fn printable_from_tip(angle: f64) -> f64 {
    Params::from_job(&job(), 0).gap_fill.min_bead_width / 2.0 / (angle / 2.0).to_radians().tan()
}

#[test]
fn a_wedge_closes_where_a_bead_fits() {
    let p = Params::from_job(&job(), 0);
    for (angle, shape) in [(10.0, wedge10 as fn(f64) -> Vec<ExPolygon>), (20.0, wedge20), (40.0, wedge40)] {
        let reach = check(&format!("{angle} degree wedge"), shape, 1.0);
        let least = flow::spacing(p.gap_fill.min_bead_width, 0.2);
        let want = printable_from_tip(angle);
        // From the second layer up (the first is compensated for its foot).
        for (k, r) in reach.iter().enumerate().skip(1) {
            assert!(*r <= want + least, "{angle} degree wedge, layer {k}: the outer wall stops {r:.2} mm from the tip, where it is a bead wide {want:.2} mm from it");
        }
    }
}

#[test]
fn a_thin_fin_is_its_outer_wall() {
    check("fin", fin, 1.0);
    let plan = plan(fin, 1.0);
    for (z, _, layer) in &plan.layers {
        let lines: Vec<&ExtrusionPath> = layer.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).collect();
        assert!(!lines.is_empty() && lines.iter().all(|l| l.role == Role::OuterWall), "z {z:.1}: the fin prints as one outer wall line, {:?}", lines.iter().map(|l| l.role).collect::<Vec<_>>());
    }
}

#[test]
fn a_strut_joint_closes_its_tip() {
    let reach = check("strut joint", strut_joint, 1.0);
    let want = printable_from_tip(24.0);
    let least = flow::spacing(Params::from_job(&job(), 0).gap_fill.min_bead_width, 0.2);
    for (k, r) in reach.iter().enumerate().skip(1) {
        assert!(*r <= want + least, "layer {k}: the outer wall stops {r:.2} mm from the joint's tip, a bead wide {want:.2} mm from it");
    }
}

/// A sliver whose tip leans out past each layer below prints nothing past the outer wall's loop;
/// leaning in, each layer's tip sits on the one below and the wall runs out to where a bead fits.
#[test]
fn a_grazing_sliver_prints_no_extension_line() {
    check("crease leaning out", crease_out, 2.0);
    let reach = check("crease leaning in", crease_in, 2.0);
    let want = printable_from_tip(12.0);
    let least = flow::spacing(Params::from_job(&job(), 0).gap_fill.min_bead_width, 0.2);
    for (k, r) in reach.iter().enumerate().skip(1) {
        assert!(*r <= want + least, "leaning in, layer {k}: the outer wall stops {r:.2} mm from the tip, a bead wide {want:.2} mm from it");
    }
}
