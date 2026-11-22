//! Exact edge-to-edge distances: straight edges use integer predicates, and arcs are measured
//! analytically, never polygonised. Filled cores include their boundaries and exclude hole interiors.
//! Gaps are rounded down with the prescribed floating-point tolerance; unrepresentable distances
//! saturate at `Nm::MAX`. Arc calculations use f64; arc-arc error is below 1 nm for radii under 1 m.

use crate::geom::{orient, ArcGeom, BBox, Nm, P};
use crate::poly::PolySet;
use crate::shape::{core_bbox, Core, Shape};

/// Edge-to-edge gap between two filled shapes in nm: the true Euclidean gap rounded DOWN to a whole nm
/// (never reports more room than there is); 0 when they touch or overlap.
/// Empty shapes and gaps beyond the coordinate range return `Nm::MAX`.
pub fn clearance(a: &Shape, b: &Shape) -> Nm {
    let (a, ra) = a.core();
    let (b, rb) = b.core();
    rounded_gap(core_distance(&a, &b) - (ra + rb))
}

/// True when the gap is strictly less than `min_gap` (a collision for DRC and the router).
/// Agrees exactly with `clearance(a, b) < min_gap`, including nonpositive thresholds and empty shapes.
pub fn collides(a: &Shape, b: &Shape, min_gap: Nm) -> bool {
    if min_gap <= 0 || !a.bbox().inflate(min_gap).intersects(&b.bbox()) {
        return false;
    }
    clearance(a, b) < min_gap
}

/// Gap from a point to a shape (0 inside), rounded down; used for picking.
/// Empty shapes and gaps beyond the coordinate range return `Nm::MAX`.
pub fn point_clearance(p: P, s: &Shape) -> Nm {
    let (core, r) = s.core();
    rounded_gap(core_distance(&Core::Point(p), &core) - r)
}

/// Distance between two cores in nm as f64 (0 when they overlap).
/// Set and Convex cores are filled regions, so a point inside has distance 0. An empty Set is
/// infinitely far from every core, including another empty Set. Arc-arc calculations have error
/// below 1 nm for radii under 1 m (f64).
pub fn core_distance(a: &Core<'_>, b: &Core<'_>) -> f64 {
    match (a, b) {
        (Core::Set(s), other) | (other, Core::Set(s)) => set_distance(s, other),
        (Core::Convex(pts), other) | (other, Core::Convex(pts)) => convex_distance(pts, other),
        (Core::Point(a), Core::Point(b)) => point_distance(*a, *b),
        (Core::Point(p), Core::Seg(a, b)) | (Core::Seg(a, b), Core::Point(p)) => {
            point_segment(*p, *a, *b)
        }
        (Core::Seg(a, b), Core::Seg(c, d)) => segment_segment(*a, *b, *c, *d),
        (Core::Point(p), Core::Arc(a, m, b)) | (Core::Arc(a, m, b), Core::Point(p)) => {
            point_arc(p.f(), &arc(*a, *m, *b))
        }
        (Core::Seg(a, b), Core::Arc(c, m, d)) | (Core::Arc(c, m, d), Core::Seg(a, b)) => {
            segment_arc(*a, *b, &arc(*c, *m, *d))
        }
        (Core::Arc(a, m, b), Core::Arc(c, n, d)) => {
            // Fix the calculation order so swapping the inputs preserves even the rounding bits.
            if (a, m, b) <= (c, n, d) {
                arc_arc(&arc(*a, *m, *b), &arc(*c, *n, *d))
            } else {
                arc_arc(&arc(*c, *n, *d), &arc(*a, *m, *b))
            }
        }
    }
}

pub(crate) fn rounded_gap(gap: f64) -> Nm {
    let g = gap.max(0.0);
    // Rust's float-to-integer conversion saturates, including positive infinity.
    (g + g.abs() * 1e-12 + 1e-9).floor() as Nm
}

fn point_distance(a: P, b: P) -> f64 {
    (a.dist2(b) as f64).sqrt()
}

fn point_segment(p: P, a: P, b: P) -> f64 {
    let dx = b.x as i128 - a.x as i128;
    let dy = b.y as i128 - a.y as i128;
    let px = p.x as i128 - a.x as i128;
    let py = p.y as i128 - a.y as i128;
    let len2 = dx * dx + dy * dy;
    let dot = px * dx + py * dy;
    if dot <= 0 {
        return point_distance(p, a);
    }
    if dot >= len2 {
        return point_distance(p, b);
    }
    // Exact projection classification and cross product avoid subtracting nearly equal positions.
    (orient(a, b, p).abs() as f64) / (len2 as f64).sqrt()
}

fn segments_intersect(a: P, b: P, c: P, d: P) -> bool {
    let ac = orient(a, b, c);
    let ad = orient(a, b, d);
    let ca = orient(c, d, a);
    let cb = orient(c, d, b);
    (ac == 0 && BBox::new(a, b).contains(c))
        || (ad == 0 && BBox::new(a, b).contains(d))
        || (ca == 0 && BBox::new(c, d).contains(a))
        || (cb == 0 && BBox::new(c, d).contains(b))
        || (((ac < 0 && ad > 0) || (ac > 0 && ad < 0))
            && ((ca < 0 && cb > 0) || (ca > 0 && cb < 0)))
}

fn segment_segment(a: P, b: P, c: P, d: P) -> f64 {
    if segments_intersect(a, b, c, d) {
        return 0.0;
    }
    point_segment(a, c, d)
        .min(point_segment(b, c, d))
        .min(point_segment(c, a, b))
        .min(point_segment(d, a, b))
}

fn arc(a: P, mid: P, b: P) -> ArcGeom {
    ArcGeom::new(a, mid, b).expect("Core::Arc points are noncollinear")
}

fn point_arc(p: (f64, f64), g: &ArcGeom) -> f64 {
    let endpoint = |q: P| (p.0 - q.x as f64).hypot(p.1 - q.y as f64);
    let ends = endpoint(g.a).min(endpoint(g.b));
    if ends == 0.0 {
        return 0.0;
    }
    let dx = p.0 - g.center.0;
    let dy = p.1 - g.center.1;
    if (dx == 0.0 && dy == 0.0) || g.contains_angle(dy.atan2(dx)) {
        ends.min((dx.hypot(dy) - g.radius).abs())
    } else {
        ends
    }
}

fn segment_arc(a: P, b: P, g: &ArcGeom) -> f64 {
    if a == b {
        return point_arc(a.f(), g);
    }
    let mut best = point_segment(g.a, a, b)
        .min(point_segment(g.b, a, b))
        .min(point_arc(a.f(), g))
        .min(point_arc(b.f(), g));
    if best == 0.0 {
        return 0.0;
    }
    let dx = b.x as f64 - a.x as f64;
    let dy = b.y as f64 - a.y as f64;
    let cx = g.center.0 - a.x as f64;
    let cy = g.center.1 - a.y as f64;
    let len2 = a.dist2(b) as f64;
    let length = len2.sqrt();
    let t = cx.mul_add(dx, cy * dy) / len2;
    let cross = cx.mul_add(dy, -cy * dx);
    let h = cross.abs() / length;

    // Solve the line-circle quadratic around the projection of the centre, avoiding b^2 - 4ac.
    if h <= g.radius {
        let dt = ((g.radius - h) * (g.radius + h)).sqrt() / length;
        for u in [t - dt, t + dt] {
            if (0.0..=1.0).contains(&u) {
                let x = dx.mul_add(u, -cx);
                let y = dy.mul_add(u, -cy);
                if g.contains_angle(y.atan2(x)) {
                    return 0.0;
                }
            }
        }
    }
    if (0.0..=1.0).contains(&t) {
        // Vector from the centre to its foot on the line, kept in floating point.
        let x = -dy * cross / len2;
        let y = dx * cross / len2;
        if h == 0.0 || g.contains_angle(y.atan2(x)) {
            best = best.min((h - g.radius).abs());
        }
    }
    best
}

// Intersections, endpoints and centre-line stationary points exhaust the distance candidates.
// This analytic f64 pair has error below 1 nm for radii under 1 m.
fn arc_arc(a: &ArcGeom, b: &ArcGeom) -> f64 {
    let mut best = point_arc(a.a.f(), b)
        .min(point_arc(a.b.f(), b))
        .min(point_arc(b.a.f(), a))
        .min(point_arc(b.b.f(), a));
    if best == 0.0 {
        return 0.0;
    }
    let dx = b.center.0 - a.center.0;
    let dy = b.center.1 - a.center.1;
    let d = dx.hypot(dy);
    if d == 0.0 {
        let overlaps = a.contains_angle(b.start)
            || a.contains_angle(b.start + b.sweep)
            || b.contains_angle(a.start)
            || b.contains_angle(a.start + a.sweep);
        return if overlaps { best.min((a.radius - b.radius).abs()) } else { best };
    }
    let ux = dx / d;
    let uy = dy / d;
    if d <= a.radius + b.radius && d >= (a.radius - b.radius).abs() {
        let x = 0.5 * (d + (a.radius - b.radius) * (a.radius + b.radius) / d);
        let h = ((a.radius - x) * (a.radius + x)).max(0.0).sqrt();
        for side in [-1.0, 1.0] {
            let px = ux * x - side * uy * h;
            let py = uy * x + side * ux * h;
            if a.contains_angle(py.atan2(px)) && b.contains_angle((py - dy).atan2(px - dx)) {
                return 0.0;
            }
        }
    }
    for (g, other) in [(a, b), (b, a)] {
        for sign in [-1.0, 1.0] {
            let vx = sign * ux;
            let vy = sign * uy;
            if g.contains_angle(vy.atan2(vx)) {
                let p = (g.center.0 + g.radius * vx, g.center.1 + g.radius * vy);
                best = best.min(point_arc(p, other));
            }
        }
    }
    best
}

fn edges(pts: &[P]) -> impl Iterator<Item = (P, P)> + '_ {
    pts.iter().copied().zip(pts.iter().copied().cycle().skip(1))
}

pub(crate) fn inside_convex(p: P, pts: &[P]) -> bool {
    edges(pts).all(|(a, b)| orient(a, b, p) >= 0)
}

pub(crate) fn any_point(core: &Core<'_>, mut contains: impl FnMut(P) -> bool) -> bool {
    match core {
        Core::Point(p) => contains(*p),
        Core::Seg(a, b) => contains(*a) || contains(*b),
        Core::Arc(a, m, b) => contains(*a) || contains(*m) || contains(*b),
        Core::Convex(pts) => pts.iter().copied().any(contains),
        Core::Set(s) => s.rings().flatten().copied().any(contains),
    }
}

fn convex_distance(pts: &[P], other: &Core<'_>) -> f64 {
    if any_point(other, |p| inside_convex(p, pts)) {
        return 0.0;
    }
    if let Core::Convex(other_pts) = other {
        if pts.iter().any(|&p| inside_convex(p, other_pts)) {
            return 0.0;
        }
    }
    boundary_distance(edges(pts), other)
}

fn set_distance(set: &PolySet, other: &Core<'_>) -> f64 {
    if set.is_empty() {
        return f64::INFINITY;
    }
    if any_point(other, |p| set.contains(p)) {
        return 0.0;
    }
    match other {
        Core::Convex(pts) => {
            if set.rings().flatten().any(|&p| inside_convex(p, pts)) {
                return 0.0;
            }
        }
        Core::Set(s) => {
            if s.is_empty() {
                return f64::INFINITY;
            }
            if set.rings().flatten().any(|&p| s.contains(p)) {
                return 0.0;
            }
        }
        _ => {}
    }
    // Hole boundaries participate too: a core wholly inside a hole is outside the filled set.
    boundary_distance(set.rings().flat_map(edges), other)
}

fn boundary_distance(boundary: impl Iterator<Item = (P, P)>, other: &Core<'_>) -> f64 {
    boundary_min(boundary, core_bbox(other), |a, b| core_distance(&Core::Seg(a, b), other))
}

/// Minimum distance from boundary edges to a core with `bbox`, measured by `dist` in iterator order.
/// Prunes by exact box separation with outward floating slack; an empty box or boundary returns infinity.
pub(crate) fn boundary_min(
    boundary: impl Iterator<Item = (P, P)>,
    bbox: BBox,
    mut dist: impl FnMut(P, P) -> f64,
) -> f64 {
    if bbox.is_empty() {
        return f64::INFINITY;
    }
    let mut best = f64::INFINITY;
    for (a, b) in boundary {
        let edge_box = BBox::new(a, b);
        // Clamp the edge box's minimum to the other box to obtain their exact squared separation.
        let nearest = P::new(
            edge_box.min.x.clamp(bbox.min.x, bbox.max.x),
            edge_box.min.y.clamp(bbox.min.y, bbox.max.y),
        );
        // A little outward slack keeps floating rounding from pruning an equal-distance candidate.
        let limit = best + best.abs() * 1e-12 + 1e-9;
        if edge_box.dist2_to(nearest) as f64 > limit * limit {
            continue;
        }
        best = best.min(dist(a, b));
        if best == 0.0 {
            return 0.0;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{mm, Angle};

    #[test]
    fn pairs_with_known_gaps() {
        let circle = |x, y, r| Shape::Circle { c: P::mm(x, y), r: mm(r) };
        let track = |a, b, w| Shape::Segment { a, b, w: mm(w) };
        let rect = Shape::Rect { c: P::ZERO, half: P::mm(1.0, 0.5), rot: Angle::ZERO, corner: 0 };
        let rounded = Shape::Rect {
            c: P::ZERO,
            half: P::mm(1.0, 0.5),
            rot: Angle::ZERO,
            corner: mm(0.25),
        };
        let quarter = Shape::Arc {
            a: P::mm(10.0, 0.0),
            mid: P::mm(10.0 / 2.0_f64.sqrt(), 10.0 / 2.0_f64.sqrt()),
            b: P::mm(0.0, 10.0),
            w: mm(0.2),
        };
        let square = PolySet::rect(BBox::new(P::ZERO, P::mm(10.0, 10.0)));
        let hole = PolySet::rect(BBox::new(P::mm(-10.0, -10.0), P::mm(10.0, 10.0)))
            .difference(&PolySet::rect(BBox::new(P::mm(-5.0, -5.0), P::mm(5.0, 5.0))));
        let upper = |x, r| Shape::Arc {
            a: P::mm(x + r, 0.0),
            mid: P::mm(x, r),
            b: P::mm(x - r, 0.0),
            w: 0,
        };
        let rounded_gap = (mm(1.25) as f64 * 2.0_f64.sqrt() - mm(0.5) as f64).floor() as Nm;
        let beyond_gap = (mm(5.0) as f64 * 5.0_f64.sqrt() - mm(0.1) as f64).floor() as Nm;
        let cases = vec![
            ("separated circles", circle(0.0, 0.0, 1.0), circle(3.0, 0.0, 1.0), 1_000_000, 0),
            (
                "track and circle",
                track(P::ZERO, P::mm(10.0, 0.0), 0.2),
                circle(5.0, 1.0, 0.25),
                650_000,
                0,
            ),
            (
                "parallel tracks",
                track(P::ZERO, P::mm(10.0, 0.0), 0.15),
                track(P::mm(0.0, 0.3), P::mm(10.0, 0.3), 0.15),
                150_000,
                0,
            ),
            (
                "crossing tracks",
                track(P::ZERO, P::mm(10.0, 0.0), 0.15),
                track(P::mm(5.0, -1.0), P::mm(5.0, 1.0), 0.15),
                0,
                0,
            ),
            ("touching circles", circle(0.0, 0.0, 1.0), circle(2.0, 0.0, 1.0), 0, 0),
            ("3-4-5 points", circle(0.0, 0.0, 0.0), circle(3.0, 4.0, 0.0), 5_000_000, 0),
            ("rect and circle", rect.clone(), circle(3.0, 0.0, 0.5), 1_500_000, 0),
            (
                "quarter-turned rect",
                rect.rotated_about(P::ZERO, Angle::QUARTER),
                circle(3.0, 0.0, 0.5),
                2_000_000,
                0,
            ),
            ("rounded corner diagonal", rounded, circle(2.0, 1.5, 0.25), rounded_gap, 0),
            ("quarter arc and origin", quarter.clone(), circle(0.0, 0.0, 1.0), 8_900_000, 1),
            ("beyond arc sweep", quarter.clone(), circle(-5.0, 0.0, 0.0), beyond_gap, 0),
            (
                "oval and circle",
                Shape::Oval { c: P::ZERO, half: P::mm(2.0, 1.0), rot: Angle::ZERO },
                circle(4.0, 0.0, 0.5),
                1_500_000,
                0,
            ),
            ("circle inside poly", Shape::Poly(square.clone()), circle(5.0, 5.0, 0.5), 0, 0),
            ("circle outside poly", Shape::Poly(square.clone()), circle(12.0, 5.0, 0.5), 1_500_000, 0),
            ("circle inside hole", Shape::Poly(hole.clone()), circle(0.0, 0.0, 1.0), 4_000_000, 0),
            ("concentric arcs", upper(0.0, 5.0), upper(0.0, 3.0), 2_000_000, 0),
            ("intersecting arcs", upper(0.0, 5.0), upper(6.0, 5.0), 0, 0),
            ("externally tangent arcs", upper(0.0, 5.0), upper(10.0, 5.0), 0, 0),
            ("separated arcs", upper(0.0, 5.0), upper(13.0, 5.0), 3_000_000, 0),
            (
                "concentric disjoint sweeps",
                Shape::Arc { a: P::mm(5.0, 0.0), mid: P::mm(3.0, 4.0), b: P::mm(0.0, 5.0), w: 0 },
                Shape::Arc { a: P::mm(-3.0, 0.0), mid: P::mm(-1.8, -2.4), b: P::mm(0.0, -3.0), w: 0 },
                (34.0_f64.sqrt() * mm(1.0) as f64).floor() as Nm,
                0,
            ),
            (
                "segment crossing arc",
                track(P::mm(-6.0, 4.0), P::mm(6.0, 4.0), 0.0),
                upper(0.0, 5.0),
                0,
                0,
            ),
            (
                "segment tangent to arc interior",
                track(P::mm(-6.0, 5.0), P::mm(6.0, 5.0), 0.0),
                upper(0.0, 5.0),
                0,
                0,
            ),
            (
                "segment perpendicular arc minimum",
                track(P::mm(-6.0, 7.0), P::mm(6.0, 7.0), 0.0),
                upper(0.0, 5.0),
                2_000_000,
                0,
            ),
            (
                "collinear overlapping segments",
                track(P::ZERO, P::mm(10.0, 0.0), 0.0),
                track(P::mm(4.0, 0.0), P::mm(12.0, 0.0), 0.0),
                0,
                0,
            ),
            (
                "collinear separated segments",
                track(P::ZERO, P::mm(10.0, 0.0), 0.0),
                track(P::mm(12.0, 0.0), P::mm(13.0, 0.0), 0.0),
                2_000_000,
                0,
            ),
            (
                "convex containing convex",
                rect.clone(),
                Shape::Rect { c: P::ZERO, half: P::mm(0.2, 0.1), rot: Angle::ZERO, corner: 0 },
                0,
                0,
            ),
            ("separated convex polygons", rect.clone(), rect.translated(P::mm(5.0, 0.0)), 3_000_000, 0),
            ("convex inside hole", Shape::Poly(hole.clone()), rect.clone(), 4_000_000, 0),
            ("convex intersects set", Shape::Poly(square.clone()), rect.clone(), 0, 0),
            (
                "set contained by convex",
                Shape::Poly(PolySet::rect(BBox::new(P::mm(-0.2, -0.1), P::mm(0.2, 0.1)))),
                rect.clone(),
                0,
                0,
            ),
            (
                "set inside hole",
                Shape::Poly(hole),
                Shape::Poly(PolySet::rect(BBox::new(P::mm(-1.0, -1.0), P::mm(1.0, 1.0)))),
                4_000_000,
                0,
            ),
            (
                "separated sets",
                Shape::Poly(square.clone()),
                Shape::Poly(square.translated(P::mm(12.0, 0.0))),
                2_000_000,
                0,
            ),
            (
                "nested sets",
                Shape::Poly(square.clone()),
                Shape::Poly(PolySet::rect(BBox::new(P::mm(2.0, 2.0), P::mm(3.0, 3.0)))),
                0,
                0,
            ),
            ("arc crossing poly boundary", quarter.clone(), Shape::Poly(square), 0, 0),
            (
                "arc outside convex",
                upper(0.0, 5.0),
                rect.translated(P::mm(0.0, 7.0)),
                1_500_000,
                0,
            ),
        ];
        for (name, a, b, expected, tolerance) in cases {
            let gap = clearance(&a, &b);
            assert!((gap - expected).abs() <= tolerance, "{name}: gap {gap}, expected {expected}");
            assert_eq!(gap, clearance(&b, &a), "{name}: symmetry");
            for g in [gap, gap + 1] {
                assert_eq!(collides(&a, &b, g), gap < g, "{name}: collision threshold {g}");
                assert_eq!(collides(&b, &a, g), gap < g, "{name}: reverse collision threshold {g}");
            }
        }
        assert_eq!(
            core_distance(
                &Core::Point(P::new(999_999_999, 3)),
                &Core::Seg(P::new(-1_000_000_000, 0), P::new(1_000_000_000, 0)),
            ),
            3.0,
            "two metre segment: exact perpendicular projection",
        );
        assert_eq!(
            core_distance(&Core::Seg(P::ZERO, P::ZERO), &Core::Point(P::mm(3.0, 4.0))),
            5_000_000.0,
            "degenerate segment: point distance",
        );
    }

    #[test]
    fn random_segments_match_sampling() {
        const SAMPLES: usize = 2_000;
        const BLOCK: usize = 32;

        fn random(seed: &mut u64) -> f64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            (*seed >> 11) as f64 / ((1_u64 << 53) as f64)
        }

        fn shape(seed: &mut u64, curved: bool) -> Shape {
            let w = mm(0.05 + 0.25 * random(seed));
            if curved {
                let c = P::mm(5.0 + 10.0 * random(seed), 5.0 + 10.0 * random(seed));
                let r = mm(0.5 + 4.0 * random(seed)) as f64;
                let start = random(seed) * std::f64::consts::TAU;
                let direction = if random(seed) < 0.5 { -1.0 } else { 1.0 };
                let sweep = direction * (0.1 + 1.8 * random(seed)) * std::f64::consts::PI;
                let at = |f: f64| {
                    let t = start + sweep * f;
                    P::from_f(c.x as f64 + r * t.cos(), c.y as f64 + r * t.sin())
                };
                Shape::Arc { a: at(0.0), mid: at(0.5), b: at(1.0), w }
            } else {
                Shape::Segment {
                    a: P::mm(20.0 * random(seed), 20.0 * random(seed)),
                    b: P::mm(20.0 * random(seed), 20.0 * random(seed)),
                    w,
                }
            }
        }

        fn sample(s: &Shape) -> (Vec<(f64, f64)>, f64, f64) {
            let (a, b, w, arc) = match s {
                Shape::Segment { a, b, w } => (*a, *b, *w, None),
                Shape::Arc { a, mid, b, w } => (*a, *b, *w, ArcGeom::new(*a, *mid, *b)),
                _ => unreachable!("sampling case: only tracks are generated"),
            };
            let dx = b.x as f64 - a.x as f64;
            let dy = b.y as f64 - a.y as f64;
            let length = arc.map_or_else(|| dx.hypot(dy), |g| g.length());
            let points = (0..SAMPLES)
                .map(|i| {
                    if i == 0 {
                        return a.f();
                    }
                    if i == SAMPLES - 1 {
                        return b.f();
                    }
                    let t = i as f64 / (SAMPLES - 1) as f64;
                    if let Some(g) = arc {
                        let angle = g.start + g.sweep * t;
                        (g.center.0 + g.radius * angle.cos(), g.center.1 + g.radius * angle.sin())
                    } else {
                        (a.x as f64 + dx * t, a.y as f64 + dy * t)
                    }
                })
                .collect();
            (points, length, w as f64 / 2.0)
        }

        fn boxes(points: &[(f64, f64)]) -> Vec<[f64; 4]> {
            points
                .chunks(BLOCK)
                .map(|chunk| {
                    let (x, y) = chunk[0];
                    chunk.iter().fold([x, y, x, y], |b, &(x, y)| {
                        [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]
                    })
                })
                .collect()
        }

        fn sampled_min(a: &[(f64, f64)], b: &[(f64, f64)]) -> f64 {
            let squared = |p: (f64, f64), q: (f64, f64)| {
                let dx = p.0 - q.0;
                let dy = p.1 - q.1;
                dx * dx + dy * dy
            };
            let mut best = a.iter().zip(b).map(|(&p, &q)| squared(p, q)).fold(f64::INFINITY, f64::min);
            let ab = boxes(a);
            let bb = boxes(b);
            // Exhaustive sample-pair distances, pruning only blocks whose lower bound exceeds an
            // already measured pair. This preserves the brute-force minimum without 1.2 billion
            // pair evaluations; the oracle does not use any production distance calculation.
            for (i, ac) in a.chunks(BLOCK).enumerate() {
                for (j, bc) in b.chunks(BLOCK).enumerate() {
                    let dx = (ab[i][0] - bb[j][2]).max(bb[j][0] - ab[i][2]).max(0.0);
                    let dy = (ab[i][1] - bb[j][3]).max(bb[j][1] - ab[i][3]).max(0.0);
                    if dx * dx + dy * dy > best + best * 1e-12 + 1e-9 {
                        continue;
                    }
                    for &p in ac {
                        for &q in bc {
                            best = best.min(squared(p, q));
                        }
                    }
                }
            }
            best.sqrt()
        }

        let mut seed = 0x9e37_79b9_7f4a_7c15;
        for case in 0..300 {
            // Equal coverage of segment-segment, segment-arc and arc-arc, including major/CW arcs.
            let a = shape(&mut seed, case % 3 == 2);
            let b = shape(&mut seed, case % 3 != 0);
            let (ap, al, ar) = sample(&a);
            let (bp, bl, br) = sample(&b);
            let sampled = (sampled_min(&ap, &bp) - ar - br).max(0.0);
            let exact = clearance(&a, &b);
            // With endpoints included, every curve point is within half a sample interval of a
            // sample. The triangle inequality bounds the overestimate by (La + Lb)/(2*(N-1));
            // subtracting widths and clamping at zero are 1-Lipschitz. Add 1 nm for flooring and
            // a sub-nm allowance for f64 sampling. No timing assertion depends on the test host.
            let error = (al + bl) / (2 * (SAMPLES - 1)) as f64 + 1.001;
            assert!(exact as f64 <= sampled + 1.0, "random pair {case}: exact exceeds sampled {sampled}");
            assert!(
                exact as f64 >= sampled - error,
                "random pair {case}: gap {exact} below sampled {sampled} minus bound {error}",
            );
            assert_eq!(exact, clearance(&b, &a), "random pair {case}: symmetry");
        }
    }

    #[test]
    fn point_clearance_and_empty() {
        let pad = Shape::Rect {
            c: P::ZERO,
            half: P::mm(1.0, 0.5),
            rot: Angle::ZERO,
            corner: mm(0.25),
        };
        let empty = Shape::Poly(PolySet::new());
        assert_eq!(point_clearance(P::ZERO, &pad), 0, "point inside pad");
        assert_eq!(point_clearance(P::mm(3.0, 0.0), &pad), 2_000_000, "point outside pad");
        assert_eq!(point_clearance(P::mm(1.0, 0.0), &pad), 0, "point on pad boundary");
        assert_eq!(point_clearance(P::ZERO, &empty), Nm::MAX, "point and empty set");
        assert_eq!(clearance(&empty, &pad), Nm::MAX, "empty set and pad");
        assert_eq!(clearance(&pad, &empty), Nm::MAX, "pad and empty set");
        assert_eq!(clearance(&empty, &empty), Nm::MAX, "two empty sets");
        assert_eq!(core_distance(&empty.core().0, &pad.core().0), f64::INFINITY, "empty core distance");
        assert!(!collides(&empty, &pad, Nm::MAX), "empty set never collides");
        assert!(!collides(&pad, &pad, 0), "zero threshold is strict");
        assert!(!collides(&pad, &pad, -1), "negative threshold never collides");
        let odd_track = Shape::Segment { a: P::ZERO, b: P::mm(1.0, 0.0), w: 3 };
        assert_eq!(point_clearance(P::new(0, 3), &odd_track), 1, "odd width preserves half nm before floor");
        let far = Shape::Circle { c: P::new(Nm::MAX, Nm::MAX), r: 0 };
        assert_eq!(
            point_clearance(P::new(Nm::MIN, Nm::MIN), &far),
            Nm::MAX,
            "full coordinate range: distance saturates without integer overflow",
        );
    }
}
