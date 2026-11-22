//! The class program (makepad_csg_sdf::regions) run with the planner's own polygon booleans and
//! offsets gives the planner's classes (fab toolpath planner_classes): the rounds the GPU runs as
//! field compositions are the planner's chain, restated. A GPU round is then checked against the
//! same boolean on its own.
use makepad_csg_sdf::regions::{self, Expr, Op, Reg, Src, Test};
use makepad_fab_settings::{config::Config, machine::Job};
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::offset::{offset_ex, DEFAULT_MITER};
use makepad_fab_slicer::sdf::SdfSource;
use makepad_fab_slicer::source::{Classes, ContourSource};
use makepad_fab_slicer::{ExPolygon, SCALE};
use makepad_fab_toolpath::{stages::planner_classes, Params};
use std::collections::HashMap;
use std::sync::Arc;

fn box3(p: [f64; 3], c: [f64; 3], r: [f64; 3]) -> f64 {
    (0..3).map(|i| (p[i] - c[i]).abs() - r[i]).fold(f64::MIN, f64::max)
}

/// A block with a round hole and a top step, a ledge from the step out over a pillar beside it
/// (an internal bridge over the gap above the pillar), on a thin plinth that overhangs one side.
fn stepped(x: f64, y: f64, z: f64) -> f64 {
    let p = [x, y, z];
    let left = box3(p, [7.0, 10.0, 1.5], [7.0, 7.0, 1.5]);
    let right = box3(p, [17.0, 10.0, 1.0], [3.0, 7.0, 1.0]);
    let hole = 2.5 - ((x - 7.0).powi(2) + (y - 10.0).powi(2)).sqrt();
    let ledge = box3(p, [22.0, 10.0, 1.7], [4.0, 2.0, 0.3]);
    let pillar = box3(p, [24.0, 10.0, 0.5], [2.0, 2.0, 0.5]);
    left.min(right).max(hole).min(ledge).min(pillar)
}

/// A cone frustum over an inverted one: sloped tops and overhanging bottoms on every layer.
fn slopes(x: f64, y: f64, z: f64) -> f64 {
    let r = ((x - 10.0).powi(2) + (y - 10.0).powi(2)).sqrt();
    let upper = (r - (8.0 - 1.6 * (z - 1.6))).max(z - 3.2).max(1.6 - z);
    let lower = (r - (4.0 + 2.4 * z)).max(-z).max(z - 1.6);
    upper.min(lower)
}

fn params(top: &str, bottom: &str) -> Params {
    let mut cfg = Config::new();
    for k in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(k, "zig-zag");
    }
    for (k, v) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "2"), ("top_shell_layers", top), ("bottom_shell_layers", bottom), ("sparse_infill_density", "15%"), ("detect_narrow_internal_solid_infill", "1"), ("detect_floating_vertical_shell", "1")] {
        cfg.set_str(k, v);
    }
    Params::from_job(&Job::new(cfg), 0)
}

/// The rounds of every plane, run round by round across the planes with the planner's booleans.
struct Run<'a> {
    outline: &'a [Vec<ExPolygon>],
    fill: &'a [Vec<ExPolygon>],
    made: Vec<HashMap<Reg, Vec<ExPolygon>>>,
}

impl Run<'_> {
    fn region(&self, src: Src, k: usize) -> &[ExPolygon] {
        match src {
            Src::Outline => &self.outline[k],
            Src::Fill => &self.fill[k],
            Src::Reg(r) => &self.made[k][&r],
        }
    }

    fn at(&self, src: Src, k: usize, dz: i8, level: f32) -> Vec<ExPolygon> {
        let r = self.region(src, (k as i64 + dz as i64) as usize);
        if level == 0.0 { r.to_vec() } else { offset_ex(r, level as f64 * SCALE, DEFAULT_MITER) }
    }

    fn expr(&self, e: &Expr, k: usize) -> Vec<ExPolygon> {
        match e {
            Expr::Term(t) => {
                assert!(!t.neg, "a complement only inside All");
                self.at(t.src, k, t.dz, t.level)
            }
            Expr::Any(of) => union_ex(&of.iter().flat_map(|e| self.expr(e, k)).collect::<Vec<_>>()),
            Expr::All(of) => {
                let neg = |e: &Expr| matches!(e, Expr::Term(t) if t.neg);
                let mut pos = of.iter().filter(|e| !neg(e)).map(|e| self.expr(e, k));
                let mut acc = pos.next().expect("a positive term");
                for p in pos {
                    acc = intersection_ex(&acc, &p);
                }
                for e in of.iter().filter(|e| neg(e)) {
                    let Expr::Term(t) = e else { unreachable!() };
                    acc = difference_ex(&acc, &self.at(t.src, k, t.dz, t.level));
                }
                acc
            }
        }
    }

    fn test(&self, c: &ExPolygon, t: &Test, k: usize) -> bool {
        let one = std::slice::from_ref(c);
        match *t {
            Test::AreaOver(a) => c.area_mm2() > a as f64,
            Test::Thin(r) => offset_ex(one, -(r as f64) * SCALE, DEFAULT_MITER).is_empty(),
            Test::Meets(src, dz) => !intersection_ex(one, self.region(src, (k as i64 + dz as i64) as usize)).is_empty(),
        }
    }

    fn op(&self, op: &Op, k: usize) -> Vec<ExPolygon> {
        match op {
            Op::Empty => Vec::new(),
            Op::Same(src) => self.region(*src, k).to_vec(),
            Op::Compose(e) => self.expr(e, k),
            Op::Keep { of, tests, hold } => self.made[k][of].iter().filter(|c| tests.iter().all(|t| self.test(c, t, k)) == *hold).cloned().collect(),
            Op::Concat(of) => of.iter().flat_map(|r| self.made[k][r].iter().cloned()).collect(),
            Op::IfAny { of, dz, then, otherwise } => {
                if self.made[(k as i64 + *dz as i64) as usize][of].is_empty() { self.op(otherwise, k) } else { self.op(then, k) }
            }
        }
    }
}

const SLIVER: f64 = 1e-3;

fn sym_area(a: &[ExPolygon], b: &[ExPolygon]) -> f64 {
    difference_ex(a, b).iter().chain(difference_ex(b, a).iter()).map(|e| e.area_mm2()).sum()
}

/// Per class, the layers where the planner has some of it.
fn check(name: &str, field: fn(f64, f64, f64) -> f64, top: f64, p: &Params) -> [usize; 7] {
    let n = (top / 0.2).round() as usize;
    let heights: Vec<(f64, f64)> = (0..n).map(|i| ((i + 1) as f64 * 0.2, 0.2)).collect();
    let zs: Vec<f64> = heights.iter().map(|&(z, h)| z - h / 2.0).collect();
    let src = SdfSource::new(Arc::new(move |x, y, z| field(x, y, z)), ([-1.0, -1.0, 0.0], [31.0, 21.0, top]));
    let outline = src.contours(&zs);
    let fill: Vec<Vec<ExPolygon>> = outline.iter().map(|o| offset_ex(o, -0.8 * SCALE, DEFAULT_MITER)).collect();
    let want: Vec<Classes> = planner_classes(p, &heights, &outline, &outline, &fill);

    let spec = makepad_fab_slice::field_regions::shell_spec(p, &[0.2]);
    let plans: Vec<Vec<regions::Round>> = (0..n).map(|k| regions::rounds(&spec, &spec.sets[0], k, n)).collect();
    let mut run = Run { outline: &outline, fill: &fill, made: vec![HashMap::new(); n] };
    for i in 0..plans[0].len() {
        let made: Vec<(Reg, Vec<ExPolygon>)> = (0..n).map(|k| (plans[k][i].out, run.op(&plans[k][i].op, k))).collect();
        for (k, (r, v)) in made.into_iter().enumerate() {
            run.made[k].insert(r, v);
        }
    }

    let mut seen = [0; 7];
    for k in 0..n {
        let c = &want[k];
        let top_final = if c.bottom.is_empty() { c.top.clone() } else { difference_ex(&c.top, &c.bottom) };
        for (i, (class, reg)) in regions::CLASSES.into_iter().enumerate() {
            let planner: &[ExPolygon] = match class {
                makepad_csg_sdf::plan::Region::Top => &top_final,
                makepad_csg_sdf::plan::Region::Bottom => &c.bottom,
                makepad_csg_sdf::plan::Region::Solid => &c.solid,
                makepad_csg_sdf::plan::Region::Sparse => &c.sparse,
                makepad_csg_sdf::plan::Region::InternalBridge => &c.bridge,
                makepad_csg_sdf::plan::Region::VerticalShell => &c.vertical,
                makepad_csg_sdf::plan::Region::ShellBand => &c.band,
            };
            let ours = &run.made[k][&reg];
            let diff = sym_area(planner, ours);
            let area: f64 = planner.iter().map(|e| e.area_mm2()).sum();
            seen[i] += usize::from(area > 0.0);
            assert!(diff <= 1e-3, "{name} layer {k} {class:?}: {diff:.5} mm² apart (planner {area:.3} mm² in {} pieces, ours {} pieces)", planner.len(), ours.len());
            // Slivers under SLIVER mm² are the booleans' rounding (the two orders cut them differently).
            let big = |v: &[ExPolygon]| v.iter().filter(|e| e.area_mm2() > SLIVER).count();
            let areas = |v: &[ExPolygon]| { let mut a: Vec<f64> = v.iter().map(|e| e.area_mm2()).collect(); a.sort_by(f64::total_cmp); a };
            assert_eq!(big(planner), big(ours), "{name} layer {k} {class:?}: pieces {:?} vs {:?}", areas(planner), areas(ours));
        }
        if let Some((reg, test)) = regions::narrow(&spec, &spec.sets[0]) {
            let radius = makepad_fab_toolpath::stages::ClassRules::of(p, 0.2).narrow * SCALE;
            let planner: Vec<bool> = c.solid.iter().map(|e| offset_ex(std::slice::from_ref(e), -radius, DEFAULT_MITER).is_empty()).collect();
            let ours: Vec<bool> = run.made[k][&reg].iter().map(|e| run.test(e, &test, k)).collect();
            assert_eq!(planner, ours, "{name} layer {k}: narrow flags");
        }
    }
    seen
}

#[test]
fn class_program_is_the_planners_chain() {
    let mut seen = [0; 7];
    for (top, bottom) in [("3", "2"), ("4", "3"), ("1", "1")] {
        let p = params(top, bottom);
        for (name, field, height) in [("stepped", stepped as fn(f64, f64, f64) -> f64, 3.4), ("slopes", slopes, 3.2)] {
            let s = check(name, field, height, &p);
            eprintln!("{name} top {top} bottom {bottom}: layers per class {:?}", regions::CLASSES.iter().map(|c| c.0).zip(s).collect::<Vec<_>>());
            seen.iter_mut().zip(s).for_each(|(a, b)| *a += b);
        }
    }
    // Every class but the band occurs (the band needs a bridge over sparse with a sparse wall gap).
    assert!(seen[..6].iter().all(|&n| n > 0), "{seen:?}");
}
