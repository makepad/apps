//! Offsets as the reference makes every one (R14 §1.5): before a ring is offset it loses its
//! close points, at a threshold of 0.005 × the offset's distance, and a ring left with under 3
//! points is not offset at all; a mitre limit under 2 acts as 2 ([`as_engine_takes`]); and a round
//! join places its points as the reference's engine places them ([`round_ring`]). Every offset the
//! tree makes goes through here; its booleans, smoothing outward (§8.5) and simplification drop
//! nothing this way. An offset's islands come out as the reference's engine hands them on (§1.6),
//! as the tree's booleans do (clip.rs).

use makepad_fab_slicer::clip::FillRule;
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt};
use makepad_fab_slicer::offset::{offset_polygons, Join};

use super::clip::in_reference_order;

/// The share of an offset's distance under which two points of a ring are close (§1.5).
pub const NEAR_SHARE: f64 = 0.005;

/// The smallest mitre limit the reference's engine uses (R14 §1.5, Mitre limits).
pub const MITRE_FLOOR: f64 = 2.0;

/// `join` as the reference's engine takes it (R14 §1.5): a mitre limit of 2 or less acts as 2, so
/// every corner of 60° or more takes its full mitre. A sharper corner is squared at the offset's
/// distance from it, as the slicer's offsets square one past its limit.
pub fn as_engine_takes(join: Join) -> Join {
    match join {
        Join::Miter(limit) if limit < MITRE_FLOOR => Join::Miter(MITRE_FLOOR),
        other => other,
    }
}

/// `set` offset by `delta` (grid units; positive grows) with `join` as the engine takes it (§1.5):
/// its rings together, the contour and each hole alike, each first rid of its close points
/// ([`sparse`]), and the result in the reference's order (§1.6).
pub fn offset(set: &[ExPolygon], delta: f64, join: Join) -> Vec<ExPolygon> {
    in_reference_order(engine(&sparse_rings(set, delta), delta, join))
}

/// `set`'s rings each offset by `delta` (grid units) on its own, close points dropped first and
/// the join taken as [`offset`] has them (§4.2, §9.5): a contour moves out by `delta`, and a hole
/// moves in by it and stays a hole, clockwise; one too small for that is gone. They come back as
/// rings, with no union, for a boolean under the non-zero rule.
pub fn ring_by_ring(set: &[ExPolygon], delta: f64, join: Join) -> Vec<Polygon> {
    let mut out = Vec::new();
    for mut ring in sparse_rings(set, delta) {
        let hole = ring.area2() < 0;
        if hole {
            ring.reverse();
        }
        for e in engine(&[ring], if hole { -delta } else { delta }, join) {
            for mut r in e.rings().cloned() {
                if hole {
                    r.reverse();
                }
                out.push(r);
            }
        }
    }
    out
}

/// `rings` offset by `delta` (grid units) as the reference's engine offsets them (§1.5): a mitre
/// limit as [`as_engine_takes`] has it, round joins by [`round_ring`], and the raw rings united
/// under the positive rule, which drops the loops a closing corner leaves.
fn engine(rings: &[Polygon], delta: f64, join: Join) -> Vec<ExPolygon> {
    match join {
        Join::Round(tolerance) if delta != 0.0 => {
            let raw: Vec<Polygon> = rings.iter().filter_map(|r| round_ring(r, delta, tolerance)).collect();
            makepad_fab_slicer::clip::union(&raw, FillRule::Positive)
        }
        _ => offset_polygons(rings, delta, as_engine_takes(join)),
    }
}

/// `ring` offset by `delta` (grid units) with round joins whose arc tolerance is `tolerance` (grid
/// units), as the reference's engine places their points (R14 §1.5, Round joins), before the
/// union. The ring is walked anticlockwise: a clockwise one is reversed, offset by −`delta` and
/// turned back. One step angle serves every corner, s = 2 arccos(1 − y ÷ |δ|), y the tolerance but
/// at most |δ| ÷ 4: the angle whose chord sags y inside the arc. At each vertex, from the edge
/// that comes in to the one that goes out:
/// - a turn whose sine × |δ| is under 1 nm, while it is under 90°, gives the incoming edge's end;
/// - a corner the offset closes gives that end, the vertex and the outgoing edge's end;
/// - a corner it opens, or an edge that doubles straight back, gives n + 1 points, n the turn
///   between the edges' normals ÷ s, rounded half up and at least 1: the incoming edge's end, then
///   n − 1 points each s further round (anticlockwise on a grow, clockwise on a shrink, the
///   direction turned on by rotation in doubles), then the outgoing edge's end. So the last gap
///   takes what is left, from 0.5 s to 1.5 s, and a turn under 1.5 s is a bare chord, with no
///   point between the edges' ends.
///
/// Every point is the vertex plus `delta` times a unit direction, each coordinate rounded to whole
/// nm, halves up.
fn round_ring(ring: &Polygon, delta: f64, tolerance: f64) -> Option<Polygon> {
    let hole = ring.area2() < 0;
    let mut pts = ring.pts.clone();
    let d = if hole {
        pts.reverse();
        -delta
    } else {
        delta
    };
    let n = pts.len();
    if n < 3 {
        return None;
    }
    // Each edge's unit normal, outward for an anticlockwise ring.
    let normals: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
            let l = (dx * dx + dy * dy).sqrt();
            if l == 0.0 { (0.0, 0.0) } else { (dy / l, -dx / l) }
        })
        .collect();
    let r = d.abs();
    let step = 2.0 * (1.0 - tolerance.min(r / 4.0) / r).acos();
    let (turn_sin, turn_cos) = (step.sin() * d.signum(), step.cos());
    let at = |p: Pt, u: (f64, f64)| Pt::new((p.x as f64 + u.0 * d + 0.5).floor() as i64, (p.y as f64 + u.1 * d + 0.5).floor() as i64);
    let mut out = Vec::with_capacity(n + 8);
    for (i, &p) in pts.iter().enumerate() {
        let (n1, n2) = (normals[(i + n - 1) % n], normals[i]);
        let sin = n1.0 * n2.1 - n1.1 * n2.0;
        let cos = n1.0 * n2.0 + n1.1 * n2.1;
        if (sin * r).abs() < 1.0 && cos > 0.0 {
            out.push(at(p, n1));
        } else if sin * d < 0.0 {
            out.extend([at(p, n1), p, at(p, n2)]);
        } else {
            let steps = ((sin.abs().atan2(cos) / step + 0.5).floor() as usize).max(1);
            out.push(at(p, n1));
            let mut u = n1;
            for _ in 1..steps {
                u = (u.0 * turn_cos - u.1 * turn_sin, u.0 * turn_sin + u.1 * turn_cos);
                out.push(at(p, u));
            }
            out.push(at(p, n2));
        }
    }
    if hole {
        out.reverse();
    }
    (out.len() >= 3).then(|| Polygon::new(out))
}

/// `set`'s rings as an offset by `delta` takes them (§1.5): each rid of its close points, and
/// those left with under 3 points gone.
fn sparse_rings(set: &[ExPolygon], delta: f64) -> Vec<Polygon> {
    let near = NEAR_SHARE * delta.abs();
    set.iter().flat_map(|e| e.rings()).map(|r| Polygon::new(sparse(&r.pts, near))).filter(|r| r.len() >= 3).collect()
}

/// Ring `pts` rid of its points nearer than `near` (grid units) (§1.5): first, from its end back,
/// those nearer than that to its first point; then, going forward, those nearer than that to the
/// last one kept. Distances are compared squared, in doubles, and strictly.
pub fn sparse(pts: &[Pt], near: f64) -> Vec<Pt> {
    let close = |a: Pt, b: Pt| {
        let (dx, dy) = ((a.x - b.x) as f64, (a.y - b.y) as f64);
        dx * dx + dy * dy < near * near
    };
    let mut end = pts.len();
    while end > 1 && close(pts[end - 1], pts[0]) {
        end -= 1;
    }
    let mut out: Vec<Pt> = Vec::with_capacity(end);
    for &p in &pts[..end] {
        if out.last().is_none_or(|&l| !close(p, l)) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::geom::SCALE;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
        Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1))
    }

    fn area(set: &[ExPolygon]) -> f64 {
        set.iter().map(|e| e.area_mm2()).sum()
    }

    /// The rule of §1.5: from the end back against the first point, then forward against the
    /// last one kept, strictly.
    #[test]
    fn close_points_go_from_the_end_back_then_forward() {
        let p = |x: i64, y: i64| Pt::new(x, y);
        // The last two lie within 10 of the first; the third within 10 of the second.
        let ring = [p(0, 0), p(1000, 0), p(1009, 0), p(1000, 1000), p(0, 1000), p(3, 9), p(5, 5)];
        assert_eq!(sparse(&ring, 10.0), vec![p(0, 0), p(1000, 0), p(1000, 1000), p(0, 1000)]);
        // Exactly the threshold apart is not close.
        let ring = [p(0, 0), p(10, 0), p(1000, 0), p(1000, 1000)];
        assert_eq!(sparse(&ring, 10.0).len(), 4, "10 apart at a threshold of 10 stays");
        // Dropped points are not compared with: a run of close points all go until one is far
        // enough from the last point kept.
        let ring = [p(0, 0), p(1000, 0), p(1006, 0), p(1012, 0), p(1018, 0), p(1000, 1000)];
        assert_eq!(sparse(&ring, 10.0), vec![p(0, 0), p(1000, 0), p(1012, 0), p(1000, 1000)]);
    }

    /// An offset drops the close points first, at 0.005 of its distance; a ring left with under 3
    /// points is not offset.
    #[test]
    fn an_offset_drops_points_within_its_share_of_the_distance() {
        // A 10 mm square with a stray point 1.4 µm in from a corner, which tilts its top edge.
        let mut pts = square(0.0, 0.0, 10.0, 10.0).pts;
        pts.insert(3, Pt::new(9_999_000, 9_999_000));
        let stray = ExPolygon::new(Polygon::new(pts));
        let plain = ExPolygon::new(square(0.0, 0.0, 10.0, 10.0));
        let inset = |e: &ExPolygon, mm: f64| offset(std::slice::from_ref(e), -mm * SCALE, Join::Miter(3.0));
        // At 0.42 mm the threshold is 2.1 µm: the point goes first, and the square shrinks as if
        // it had none.
        assert_eq!(inset(&stray, 0.42)[0].contour.pts, inset(&plain, 0.42)[0].contour.pts, "the stray point is gone before the shrink");
        // At 0.1 mm the threshold is 0.5 µm: the point stays, and so does the tilt.
        let lost = area(&inset(&plain, 0.1)) - area(&inset(&stray, 0.1));
        assert!(lost > 1e-3, "a 0.1 mm shrink keeps the tilted edge: {lost} mm2");
        // A ring of three points within the threshold of each other is not offset at all.
        let speck = ExPolygon::new(Polygon::new(vec![Pt::new(0, 0), Pt::new(1000, 0), Pt::new(0, 1000)]));
        assert!(offset(&[speck], 0.42 * SCALE, Join::Miter(3.0)).is_empty(), "a speck under 2.1 µm across is not grown");
        // The share itself: under the inner line's 419 999 nm shrink (§8.6 step 5) the threshold is
        // 2 099.995 nm, so a point 2 099 nm on from a corner goes and one 2 100 nm on stays. Each
        // sits 30 nm out of the bottom edge, so it shows in the shrunk edge's tilt.
        let shrink = |d: i64| {
            let mut pts = square(0.0, 0.0, 10.0, 10.0).pts;
            pts.insert(1, Pt::new(d, -30));
            let e = ExPolygon::new(Polygon::new(pts));
            let out = offset(std::slice::from_ref(&e), -419_999.0, Join::Miter(1.2));
            out[0].contour.pts.iter().map(|p| p.y).min().unwrap()
        };
        assert_eq!(shrink(2099), 419_999, "2 099 nm from the corner: dropped, the edge stays level");
        assert!(shrink(2100) < 419_999, "2 100 nm from the corner: kept, the edge tilts down to it");
    }

    /// R14 §1.5's mitre limits: under 2 a limit acts as 2, so a square's corner, whose mitre is
    /// √2 times the offset, takes it in full at 1.2; a corner whose mitre is past 2 times the
    /// offset is squared at the offset, under a limit of 1.2 or 2 alike, and a limit of 3 mitres it.
    #[test]
    fn a_mitre_limit_under_two_acts_as_two() {
        let grown = offset(&[ExPolygon::new(square(0.0, 0.0, 10.0, 10.0))], 1.0 * SCALE, Join::Miter(1.2));
        assert_eq!(grown[0].contour.len(), 4, "four mitred corners, none squared");
        assert!(grown[0].contour.pts.contains(&Pt::from_mm(11.0, 11.0)), "the corner's mitre in full");
        // A wedge of 50° at its tip (11, 0): its mitre reaches 1 ÷ sin 25° = 2.37 times the offset.
        let h = 11.0 * 25f64.to_radians().tan();
        let wedge = ExPolygon::new(Polygon::new(vec![Pt::from_mm(0.0, -h), Pt::from_mm(11.0, 0.0), Pt::from_mm(0.0, h)]));
        let reach = |limit: f64| {
            let out = offset(std::slice::from_ref(&wedge), 1.0 * SCALE, Join::Miter(limit));
            out[0].contour.pts.iter().map(|p| p.x).max().unwrap() as f64 / SCALE - 11.0
        };
        assert!((reach(1.2) - 1.0).abs() < 1e-5 && (reach(2.0) - 1.0).abs() < 1e-5, "squared at the offset under 1.2 and 2: {} {}", reach(1.2), reach(2.0));
        assert!(reach(3.0) > 2.0, "mitred under 3: {}", reach(3.0));
    }

    /// The step angle §1.5 gives a round join at a tolerance of 0.01 mm and an offset of `mm`.
    fn step_deg(mm: f64) -> f64 {
        (2.0 * (1.0 - 0.01f64.min(mm / 4.0) / mm).acos()).to_degrees()
    }

    /// R14 §1.5's round joins: n = the turn ÷ s rounded, the steps counted from the incoming
    /// edge's end. A square grown by 0.0525 mm at a 0.01 mm tolerance (s = 71.9°) has bare chords
    /// for corners, no point between the edges' ends; grown by 0.079 mm (s = 58.4°) each corner
    /// takes one arc point, 58.4° round from the incoming edge's end, not on the bisector.
    #[test]
    fn a_round_join_steps_from_the_incoming_edge() {
        let sq = ExPolygon::new(square(0.0, 0.0, 10.0, 10.0));
        let tol = Join::Round(0.01 * SCALE);
        let chords = offset(std::slice::from_ref(&sq), 0.0525 * SCALE, tol);
        assert_eq!(chords[0].contour.len(), 8, "two edge ends a corner and nothing between, at s = {}°", step_deg(0.0525));
        let arcs = offset(std::slice::from_ref(&sq), 0.079 * SCALE, tol);
        assert_eq!(arcs[0].contour.len(), 12, "one arc point a corner");
        // The corner (10, 0): the bottom edge comes in, its normal straight down.
        let v = Pt::from_mm(10.0, 0.0);
        let mid = arcs[0].contour.pts.iter().find(|p| p.x > v.x && p.y < v.y).copied().expect("the arc point");
        let (dx, dy) = ((mid.x - v.x) as f64, (mid.y - v.y) as f64);
        assert!((dx.hypot(dy) - 0.079 * SCALE).abs() <= 1.0, "on the arc: {mid:?}");
        let round = dx.atan2(-dy).to_degrees();
        assert!((round - step_deg(0.079)).abs() < 0.01, "{round}° round from the incoming edge's end, s = {}°", step_deg(0.079));
    }

    /// A hole is walked the other way round (§1.5): reversed, offset and turned back, so its arcs
    /// step from what is its outgoing edge as stored. A 2 mm square hole whose region shrinks by
    /// 0.079 mm grows round its corners; at (4, 2) the arc point lies s round from the bottom
    /// edge's end.
    #[test]
    fn a_holes_round_joins_step_from_its_reversed_walk() {
        let mut hole = square(2.0, 2.0, 4.0, 4.0);
        hole.reverse();
        let e = ExPolygon { contour: square(0.0, 0.0, 10.0, 10.0), holes: vec![hole] };
        let out = offset(&[e], -0.079 * SCALE, Join::Round(0.01 * SCALE));
        let v = Pt::from_mm(4.0, 2.0);
        let mid = out[0].holes[0].pts.iter().find(|p| p.x > v.x && p.y < v.y).copied().expect("the arc point");
        let (dx, dy) = ((mid.x - v.x) as f64, (mid.y - v.y) as f64);
        let round = dx.atan2(-dy).to_degrees();
        assert!((round - step_deg(0.079)).abs() < 0.01, "{round}° round from the bottom edge's end, s = {}°", step_deg(0.079));
    }

    /// A round join's points are rounded to whole nm halves up (§1.5): a square at negative
    /// coordinates shrunk by 2.5 nm has its edges at −97.5 and −12.5 nm come out at −97 and −12.
    #[test]
    fn round_join_points_round_halves_up() {
        let e = ExPolygon::new(Polygon::rect(Pt::new(-100, -100), Pt::new(-10, -10)));
        let b = offset(&[e], -2.5, Join::Round(0.01 * SCALE))[0].contour.bbox();
        assert_eq!((b.min, b.max), (Pt::new(-97, -97), Pt::new(-12, -12)));
    }

    /// Ring by ring: a contour grows, a hole shrinks and stays a clockwise hole, and a hole too
    /// small to shrink is gone.
    #[test]
    fn ring_by_ring_keeps_holes_as_holes() {
        let mut hole = square(4.0, 4.0, 6.0, 6.0);
        hole.reverse();
        let mut speck = Polygon::new(vec![Pt::new(8_000_000, 8_000_000), Pt::new(8_000_015, 8_000_000), Pt::new(8_000_015, 8_000_015), Pt::new(8_000_000, 8_000_015)]);
        speck.reverse();
        let e = ExPolygon { contour: square(0.0, 0.0, 10.0, 10.0), holes: vec![hole, speck] };
        let rings = ring_by_ring(&[e], 10.0, Join::Miter(3.0));
        assert_eq!(rings.len(), 2, "the contour and the 2 mm hole; the 15 nm hole is gone");
        assert!(rings[0].area2() > 0 && rings[1].area2() < 0, "the contour runs anticlockwise, the hole clockwise");
        let b = rings[0].bbox();
        assert_eq!((b.min, b.max), (Pt::new(-10, -10), Pt::new(10_000_010, 10_000_010)), "the contour grows by 10 nm");
        let b = rings[1].bbox();
        assert_eq!((b.min, b.max), (Pt::new(4_000_010, 4_000_010), Pt::new(5_999_990, 5_999_990)), "the hole shrinks by 10 nm");
    }
}
