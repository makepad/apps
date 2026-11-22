//! First-layer brim loops, ordered from the outside towards the object
//! for outer brims and from the hole wall towards its centre for inner brims.

use crate::{flow, ExtrusionPath, Role};
use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::clip::union_ex;
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_slicer::offset::{offset_ex, Join};

/// Round joins at a chord deviation matching the reference brim corners
/// (about ten segments per quarter turn at a 4.4 mm radius).
const ROUND: Join = Join::Round(0.0135 * SCALE);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrimKind {
    NoBrim,
    OuterOnly,
    InnerOnly,
    OuterAndInner,
    AutoBrim,
}

impl BrimKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "no_brim" => Some(Self::NoBrim),
            "outer_only" => Some(Self::OuterOnly),
            "inner_only" => Some(Self::InnerOnly),
            "outer_and_inner" => Some(Self::OuterAndInner),
            "auto_brim" => Some(Self::AutoBrim),
            _ => None,
        }
    }
}

pub(crate) fn check(job: &Job) -> Result<(), String> {
    if !job.cfg.has("brim_type") || BrimKind::parse(job.s("brim_type")).is_some() {
        Ok(())
    } else {
        Err(format!("unknown brim_type \"{}\" (Fab prints: no_brim, outer_only, inner_only, outer_and_inner, auto_brim)", job.s("brim_type")))
    }
}

pub(crate) fn kind_of(job: &Job) -> BrimKind {
    // Unset: the automatic brim, as the reference profiles default to.
    if !job.cfg.has("brim_type") {
        return BrimKind::AutoBrim;
    }
    BrimKind::parse(job.s("brim_type")).unwrap_or_else(|| panic!("brim_type \"{}\" is not supported; toolpath::check the job before planning", job.s("brim_type")))
}

/// Fitted approximation pending calibration. Zero means no brim; a tall,
/// narrow footprint gets an outer brim capped by its larger bounding side.
pub fn auto_brim_width(outline: &[ExPolygon], height_mm: f64) -> f64 {
    let bb = bbox_of(outline);
    if bb.is_empty() || !height_mm.is_finite() || height_mm <= 0.0 {
        return 0.0;
    }
    let dx = unscaled(bb.max.x - bb.min.x);
    let dy = unscaled(bb.max.y - bb.min.y);
    let (a, b) = (dx.min(dy), dx.max(dy));
    if a <= 0.0 {
        return 0.0;
    }
    let x = height_mm * height_mm / (a * a * b);
    if x < 2.3 { 0.0 } else { (2.1 * x).min(b) }
}

/// The forbidden region for a brim: first-layer outlines grown by the gap.
pub(crate) fn keep_out(outline: &[ExPolygon], gap_mm: f64) -> Vec<ExPolygon> {
    offset_ex(&union_ex(outline), gap_mm.max(0.0) * SCALE, ROUND)
}

/// Brim centre lines. `keep_out` contains the plate's first-layer outlines
/// already grown by the object gap; the object's own clearance is also
/// enforced here. For `AutoBrim`, the caller resolves `width_mm` using the
/// object's height and [`auto_brim_width`]. Speeds are assigned by the plan.
#[allow(clippy::too_many_arguments)]
pub fn rings(outline: &[ExPolygon], kind: BrimKind, width_mm: f64, gap_mm: f64, line_width: f64, layer_height: f64, keep_out: &[ExPolygon]) -> Vec<ExtrusionPath> {
    if kind == BrimKind::NoBrim || outline.is_empty() || !width_mm.is_finite() || !gap_mm.is_finite() || !line_width.is_finite() || !layer_height.is_finite() || line_width <= 0.0 || layer_height <= 0.0 {
        return Vec::new();
    }
    let gap = gap_mm.max(0.0);
    let spacing = flow::spacing(line_width, layer_height);
    if spacing <= 0.0 || width_mm - gap < spacing {
        return Vec::new();
    }
    let n = ((width_mm - gap) / spacing).floor() as usize;
    let outline = union_ex(outline);
    let mut paths = Vec::new();
    let mut append = |regions: Vec<ExPolygon>| {
        for e in regions {
            let mut path = crate::walls::loop_path(&e.contour.pts, 0, Role::Brim, line_width, 0.0);
            path.height = layer_height;
            path.area = flow::cross_section(line_width, layer_height);
            paths.push(path);
        }
    };
    if matches!(kind, BrimKind::OuterOnly | BrimKind::OuterAndInner | BrimKind::AutoBrim) {
        for k in (0..n).rev() {
            let distance = (gap + spacing / 2.0 + k as f64 * spacing) * SCALE;
            for e in &outline {
                // Offset each outer contour as a filled region, including
                // islands nested inside holes of other islands.
                append(offset_ex(&[ExPolygon::new(e.contour.clone())], distance, ROUND));
            }
        }
    }
    if matches!(kind, BrimKind::InnerOnly | BrimKind::OuterAndInner) {
        for hole in outline.iter().flat_map(|e| &e.holes) {
            let mut contour = hole.clone();
            contour.reverse();
            let inside = [ExPolygon::new(contour)];
            for k in 0..n {
                let distance = (gap + spacing / 2.0 + k as f64 * spacing) * SCALE;
                let regions = offset_ex(&inside, -distance, ROUND);
                if regions.is_empty() {
                    break;
                }
                append(regions);
            }
        }
    }
    let mut forbidden = self::keep_out(&outline, gap);
    forbidden.extend_from_slice(keep_out);
    clip_paths(paths, &union_ex(&forbidden))
}

/// Difference of paths and an already expanded forbidden region. Split
/// segments at boundary hits and classify the intervening spans, including
/// tangencies and collinear overlaps. Cuts stay open; no new extrusion is
/// added along the forbidden region's boundary.
pub(crate) fn clip_paths(paths: Vec<ExtrusionPath>, keep_out: &[ExPolygon]) -> Vec<ExtrusionPath> {
    if paths.is_empty() || keep_out.is_empty() {
        return paths;
    }
    let mut bb = BBox::empty();
    for path in &paths {
        bb.merge(&BBox::of(&path.pts));
    }
    if bb.is_empty() || !bb.overlaps(&bbox_of(keep_out)) {
        return paths;
    }
    let grid = SegGrid::of_regions(keep_out, |k| k);
    let mut out = Vec::new();
    for path in paths {
        let mut runs: Vec<Vec<Pt>> = Vec::new();
        let mut run = Vec::new();
        for edge in path.pts.windows(2) {
            let (a, b) = (edge[0], edge[1]);
            if a == b {
                continue;
            }
            let d = b.sub(a);
            let mut cuts = vec![0.0, 1.0];
            let mut boundary = Vec::new();
            grid.visit_box(BBox::of(edge), |u, v, _| {
                let e = v.sub(u);
                let den = cross(d, e);
                if den != 0 {
                    let sign = den.signum();
                    let t = cross(u.sub(a), e) * sign;
                    let s = cross(u.sub(a), d) * sign;
                    let den = den.abs();
                    if 0 < t && t < den && 0 <= s && s <= den {
                        cuts.push(t as f64 / den as f64);
                    }
                } else if cross(u.sub(a), d) == 0 {
                    let len2 = dot(d, d) as f64;
                    let t0 = dot(u.sub(a), d) as f64 / len2;
                    let t1 = dot(v.sub(a), d) as f64 / len2;
                    let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(1.0));
                    if lo < hi {
                        cuts.extend([lo, hi]);
                        boundary.push((lo, hi));
                    }
                }
            });
            cuts.sort_by(f64::total_cmp);
            cuts.dedup();
            for span in cuts.windows(2) {
                let (p, q) = (lerp(a, b, span[0]), lerp(a, b, span[1]));
                if p == q {
                    continue;
                }
                let t = (span[0] + span[1]) / 2.0;
                if boundary.iter().any(|&(lo, hi)| lo <= t && t <= hi) || grid.contains_any(lerp(a, b, t)) {
                    if run.len() >= 2 {
                        runs.push(std::mem::take(&mut run));
                    }
                } else {
                    if run.is_empty() {
                        run.push(p);
                    }
                    run.push(q);
                }
            }
        }
        if run.len() >= 2 {
            runs.push(run);
        }
        // A cut away from the seam splits one open piece across the start
        // of the original loop. Rejoin those ends without closing the cut.
        if path.closed && runs.len() > 1 && runs[0].first() == runs.last().unwrap().last() {
            let mut last = runs.pop().unwrap();
            last.extend_from_slice(&runs[0][1..]);
            runs[0] = last;
        }
        for pts in runs {
            let closed = path.closed && pts.len() > 3 && pts.first() == pts.last();
            out.push(ExtrusionPath { loop_pts: if closed { pts.clone() } else { Vec::new() }, pts, closed, ..path.clone() });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(min: f64, max: f64) -> ExPolygon {
        ExPolygon::new(Polygon::rect(Pt::from_mm(min, min), Pt::from_mm(max, max)))
    }

    fn job() -> Job {
        let mut cfg = makepad_fab_settings::config::Config::new();
        for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
            cfg.set_str(key, "zig-zag");
        }
        for (key, value) in [
            ("nozzle_diameter", "0.4"), ("initial_layer_line_width", "0.5"),
            ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"),
            ("wall_loops", "1"), ("initial_layer_speed", "20"),
        ] {
            cfg.set_str(key, value);
        }
        cfg.set_vec("filament_map", vec!["1".into(), "1".into()]);
        Job::new(cfg)
    }

    struct SquareSource { painted: bool }

    impl makepad_fab_slicer::source::ContourSource for SquareSource {
        fn bounds(&self) -> ([f64; 3], [f64; 3]) {
            ([0.0, 0.0, 0.0], [20.0, 20.0, 0.4])
        }
        fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
            vec![vec![square(0.0, 20.0)]; zs.len()]
        }
        fn painted(&self) -> bool {
            self.painted
        }
        fn paint_regions(&self, _zs: &[f64], slices: &[Vec<ExPolygon>], _p: &makepad_fab_slicer::paint::PaintParams) -> Vec<makepad_fab_slicer::paint::LayerPaint> {
            // The base filament has no painted model region at all.
            slices.iter().map(|s| vec![(2, s.clone())]).collect()
        }
    }

    fn input(painted: bool) -> crate::ObjectInput {
        crate::ObjectInput { overrides: Vec::new(), name: "square".into(), source: Box::new(SquareSource { painted }), filament: 0 }
    }

    #[test]
    fn square_outer_loops_use_flow_spacing_and_print_outermost_first() {
        let paths = rings(&[square(0.0, 20.0)], BrimKind::OuterOnly, 5.0, 0.1, 0.5, 0.2, &[]);
        assert_eq!(paths.len(), 10);
        let reach = |p: &ExtrusionPath| -BBox::of(&p.pts).min.x_mm();
        assert!((reach(&paths[0]) - 4.442).abs() < 0.01);
        assert!((reach(&paths[9]) - 0.329).abs() < 0.01);
        let spacing = flow::spacing(0.5, 0.2);
        for pair in paths.windows(2) {
            assert!((reach(&pair[0]) - reach(&pair[1]) - spacing).abs() < 0.00001);
        }
        for p in &paths {
            assert_eq!(p.role, Role::Brim);
            assert!(p.closed);
            assert_eq!(p.pts.first(), p.pts.last());
            assert_eq!(p.loop_pts, p.pts);
            assert_eq!((p.width, p.height), (0.5, 0.2));
            assert!((p.area - flow::cross_section(0.5, 0.2)).abs() < 1e-12);
            // Round joins add arc points beyond the square's four corners.
            assert!(p.pts.len() > 9);
        }
    }

    #[test]
    fn square_ring_prints_inner_loops_towards_the_centre_and_stops() {
        let mut outline = square(0.0, 20.0);
        let mut hole = square(5.0, 15.0).contour;
        hole.reverse();
        outline.holes.push(hole);
        let inner = rings(&[outline.clone()], BrimKind::InnerOnly, 20.0, 0.1, 0.5, 0.2, &[]);
        assert_eq!(inner.len(), 11);
        assert!((BBox::of(&inner[0].pts).min.x_mm() - 5.329).abs() < 0.01);
        for p in &inner {
            assert!(p.closed);
            assert!(p.pts.iter().all(|q| q.x_mm() > 5.1 && q.x_mm() < 14.9 && q.y_mm() > 5.1 && q.y_mm() < 14.9));
        }
        for pair in inner.windows(2) {
            assert!(BBox::of(&pair[0].pts).min.x < BBox::of(&pair[1].pts).min.x);
        }
        let both = rings(&[outline], BrimKind::OuterAndInner, 5.0, 0.1, 0.5, 0.2, &[]);
        assert_eq!(both.len(), 20);
        assert!(both[..10].iter().all(|p| BBox::of(&p.pts).min.x < 0));
        assert!(both[10..].iter().all(|p| BBox::of(&p.pts).min.x > scaled(5.0)));
    }

    #[test]
    fn no_brim_has_no_paths() {
        assert!(rings(&[square(0.0, 20.0)], BrimKind::NoBrim, 5.0, 0.1, 0.5, 0.2, &[]).is_empty());
    }

    #[test]
    fn neighbouring_object_cuts_rings_into_open_paths() {
        let neighbour = ExPolygon::new(Polygon::rect(Pt::from_mm(21.0, -10.0), Pt::from_mm(30.0, 30.0)));
        let forbidden = keep_out(&[neighbour], 0.1);
        let paths = rings(&[square(0.0, 20.0)], BrimKind::OuterOnly, 5.0, 0.1, 0.5, 0.2, &forbidden);
        assert_eq!(paths.len(), 10);
        assert!(paths[..8].iter().all(|p| !p.closed && p.loop_pts.is_empty()));
        assert!(paths[8..].iter().all(|p| p.closed));
        assert!(paths.iter().flat_map(|p| &p.pts).all(|p| p.x <= scaled(20.9)));
    }

    #[test]
    fn clipping_handles_tangency_and_collinear_boundary_spans() {
        let path = crate::walls::loop_path(&square(0.0, 20.0).contour.pts, 0, Role::Brim, 0.5, 0.0);
        let tangent = ExPolygon::new(Polygon::new(vec![Pt::from_mm(10.0, 20.0), Pt::from_mm(12.0, 22.0), Pt::from_mm(8.0, 22.0)]));
        let untouched = clip_paths(vec![path.clone()], &[tangent]);
        assert_eq!(untouched.len(), 1);
        assert!(untouched[0].closed);
        assert!((untouched[0].length_mm() - 80.0).abs() < 1e-6);
        let beside = ExPolygon::new(Polygon::rect(Pt::from_mm(20.0, 5.0), Pt::from_mm(25.0, 15.0)));
        let cut = clip_paths(vec![path.clone()], &[beside]);
        assert_eq!(cut.len(), 1);
        assert!(!cut[0].closed && cut[0].loop_pts.is_empty());
        assert!((cut[0].length_mm() - 70.0).abs() < 1e-6);
        assert!(clip_paths(vec![path], &[square(-1.0, 21.0)]).is_empty());
    }

    #[test]
    fn painted_object_gets_one_brim_on_its_base_filament_only() {
        let mut job = job();
        job.cfg.set_str("brim_type", "outer_only");
        let tp = crate::generate(&[input(true)], &job);
        let brims: Vec<_> = tp.layers[0].objects.iter().flat_map(|o| o.islands.iter().flat_map(move |i| i.walls.iter().map(move |p| (o.filament, p)))).filter(|(_, p)| p.role == Role::Brim).collect();
        assert_eq!(brims.len(), 10);
        assert!(brims.iter().all(|(f, p)| *f == 0 && p.speed == 20.0 && p.width == 0.5 && p.height == 0.2));
        assert_eq!(tp.layers[0].objects[0].islands[0].walls[0].role, Role::Brim);
        assert!(tp.layers[1..].iter().flat_map(|l| &l.objects).flat_map(|o| &o.islands).flat_map(|i| i.walls.iter().chain(&i.fills)).all(|p| p.role != Role::Brim));
    }

    #[test]
    fn placed_copies_clip_against_each_others_first_layer() {
        let mut job = job();
        job.cfg.set_str("brim_type", "outer_only");
        let plan = std::sync::Arc::new(crate::plan_one(crate::par::Ctl::default(), 0, &input(false), &job).unwrap());
        let delta = Pt::from_mm(22.0, 0.0);
        let tp = crate::merge(vec![(plan.clone(), Pt::default()), (plan.clone(), delta)]);
        let mut forbidden = plan.brim_keep_out.clone();
        let mut neighbour = forbidden.clone();
        for e in &mut neighbour {
            e.translate(delta);
        }
        forbidden.extend(neighbour);
        for object in &tp.layers[0].objects {
            let paths = &object.islands[0].walls;
            assert!(paths.iter().any(|p| !p.closed));
            assert!(paths.iter().all(|p| p.role == Role::Brim && p.speed == 20.0));
            for edge in paths.iter().flat_map(|p| p.pts.windows(2)) {
                assert!(!forbidden.iter().any(|e| e.contains(lerp(edge[0], edge[1], 0.5))));
            }
        }
        // Placement never clips the reusable representative in place.
        assert!(plan.layers[0].2.islands[0].walls.iter().all(|p| p.closed));
    }

    #[test]
    fn disabled_and_zero_width_brims_leave_model_layers_identical() {
        let mut job = job();
        let omitted = crate::generate(&[input(false)], &job);
        job.cfg.set_str("brim_type", "no_brim");
        let disabled = crate::generate(&[input(false)], &job);
        job.cfg.set_str("brim_type", "outer_only");
        job.cfg.set_str("brim_width", "0");
        let zero = crate::generate(&[input(false)], &job);
        assert_eq!(format!("{:?}", omitted.layers), format!("{:?}", disabled.layers));
        assert_eq!(format!("{:?}", disabled.layers), format!("{:?}", zero.layers));
    }

    #[test]
    fn automatic_width_uses_height_and_bounding_sides() {
        let outline = [square(0.0, 20.0)];
        assert_eq!(auto_brim_width(&outline, 100.0), 0.0);
        assert!((auto_brim_width(&outline, 150.0) - 5.90625).abs() < 1e-12);
        assert_eq!(auto_brim_width(&outline, 1000.0), 20.0);
        assert_eq!(auto_brim_width(&[], 150.0), 0.0);
    }

    #[test]
    fn unsupported_kind_is_rejected_by_toolpath_check() {
        let mut job = job();
        assert!(crate::check(&job).is_ok());
        let p = crate::Params::from_job(&job, 0);
        assert_eq!(p.brim_kind, BrimKind::AutoBrim);
        assert_eq!((p.brim_width, p.brim_gap), (5.0, 0.1));
        for name in ["no_brim", "outer_only", "inner_only", "outer_and_inner", "auto_brim"] {
            job.cfg.set_str("brim_type", name);
            assert!(crate::check(&job).is_ok());
        }
        job.cfg.set_str("brim_object_gap", "0");
        job.cfg.set_str("brim_width", "0");
        let p = crate::Params::from_job(&job, 0);
        assert_eq!((p.brim_width, p.brim_gap), (0.0, 0.0));
        for name in ["brim_ears", "", "unknown"] {
            job.cfg.set_str("brim_type", name);
            let error = crate::check(&job).unwrap_err();
            assert!(error.contains("brim_type") && error.contains(name));
        }
    }
}
