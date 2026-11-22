//! Region offsets (grow / shrink) on the integer grid.
//!
//! Each ring is offset edge by edge along its outward normal. Where the
//! offset edges of a vertex separate, a join (miter, square or round)
//! bridges them; where they overlap, the raw path detours through the
//! original vertex. The raw rings are then unioned under the positive fill
//! rule, which drops the inverted loops of collapsed parts and detours.

use crate::clip::{self, FillRule};
use crate::geom::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Join {
    /// Miter with a limit (as a multiple of the offset); beyond it, square.
    Miter(f64),
    Square,
    /// Round with a maximum chord deviation in units: never past the arc. A turn one chord can
    /// follow takes one point on the arc, none at its ends.
    Round(f64),
}

pub const DEFAULT_MITER: Join = Join::Miter(3.0);

/// How far (units) past `e`'s box `offset_ex(&[e], delta, Join::Miter(limit))` can reach. Each
/// ring is untoothed as [`raw_ring`] takes it, and corners follow [`offset_ring`]: straight
/// corners and detours through the vertex reach at most `|delta|`. A miter, when `r = 1 + cos`
/// exceeds `2 / (limit * limit)`, reaches `|delta| * sqrt(2 / r)`: its point is `delta / r`
/// along `n1 + n2`, whose length is `sqrt(2 * r)`. Otherwise the square join's ends reach
/// `|delta| * sqrt(1 + tan^2(a / 4))`, `a` the angle between normals, at most `|delta| * sqrt(2)`.
/// Take the largest factor, at least 1, and add two units for the offset points' rounding and
/// the booleans' snap rounding.
pub fn miter_reach(e: &ExPolygon, delta: f64, limit: f64) -> f64 {
    let mut factor: f64 = 1.0;
    for ring in e.rings() {
        let pts = untoothed(ring);
        let n = pts.len();
        if n < 3 {
            continue;
        }
        let normal = |i: usize| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
            let l = (dx * dx + dy * dy).sqrt();
            if l == 0.0 { (0.0, 0.0) } else { (dy / l, -dx / l) }
        };
        let mut n1 = normal(n - 1);
        for i in 0..n {
            let n2 = normal(i);
            let sin = n1.0 * n2.1 - n1.1 * n2.0;
            let cos = n1.0 * n2.0 + n1.1 * n2.1;
            n1 = n2;
            let straight = (sin * delta).abs() < 1e-12 * delta.abs() && cos > 0.0;
            let exact_reversal = sin.abs() < 1e-9 && cos < 0.0;
            if straight || sin * delta < 0.0 || (delta < 0.0 && exact_reversal) {
                continue;
            }
            let r = 1.0 + cos;
            factor = factor.max(if r > 2.0 / (limit * limit) { (2.0 / r).sqrt() } else { std::f64::consts::SQRT_2 });
        }
    }
    delta.abs() * factor + 2.0
}

/// Raw offset of one ring (no cleanup).
fn offset_ring(pts: &[Pt], delta: f64, join: Join, out: &mut Vec<Pt>) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    // Unit normals of each edge i (from pts[i] to pts[i+1]), outward for a
    // counter-clockwise ring: (dy, -dx).
    let mut normals = Vec::with_capacity(n);
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let dx = (b.x - a.x) as f64;
        let dy = (b.y - a.y) as f64;
        let l = (dx * dx + dy * dy).sqrt();
        if l == 0.0 {
            normals.push((0.0, 0.0));
        } else {
            normals.push((dy / l, -dx / l));
        }
    }
    let at = |p: Pt, nx: f64, ny: f64, d: f64| Pt::new((p.x as f64 + nx * d).round() as i64, (p.y as f64 + ny * d).round() as i64);
    for i in 0..n {
        let p = pts[i];
        let (n1x, n1y) = normals[(i + n - 1) % n];
        let (n2x, n2y) = normals[i];
        // sin of the turn (prev edge → next edge), positive for left turns.
        let sin = n1x * n2y - n1y * n2x;
        let cos = n1x * n2x + n1y * n2y;
        if (sin * delta).abs() < 1e-12 * delta.abs() && cos > 0.0 {
            // Straight: one point.
            out.push(at(p, n1x, n1y, delta));
            continue;
        }
        // A turn back so exact it has no side: under an inset, the tip of a needle.
        let exact_reversal = sin.abs() < 1e-9 && cos < 0.0;
        if sin * delta < 0.0 || (delta < 0.0 && exact_reversal) {
            // The offset edges overlap: route through the vertex, however sharp the turn. A cap
            // here would reach past the ring's far end when the ring is shorter than the offset.
            out.push(at(p, n1x, n1y, delta));
            out.push(p);
            out.push(at(p, n2x, n2y, delta));
            continue;
        }
        match join {
            Join::Miter(limit) => {
                let r = 1.0 + cos;
                if r > 2.0 / (limit * limit) {
                    let k = delta / r;
                    out.push(Pt::new((p.x as f64 + (n1x + n2x) * k).round() as i64, (p.y as f64 + (n1y + n2y) * k).round() as i64));
                } else {
                    square_join(p, (n1x, n1y), (n2x, n2y), delta, out);
                }
            }
            Join::Square => square_join(p, (n1x, n1y), (n2x, n2y), delta, out),
            Join::Round(tol) => {
                let r = delta.abs();
                let tol = tol.max(1.0).min(r);
                let half_step = 1.0 - tol / r;
                if cos > 0.0 && cos >= 2.0 * half_step * half_step - 1.0 {
                    let l = (2.0 * (1.0 + cos)).sqrt();
                    out.push(at(p, (n1x + n2x) / l, (n1y + n2y) / l, delta));
                    continue;
                }
                let a1 = n1y.atan2(n1x);
                // Signed turn from n1 to n2, the short way; it has the sign of
                // delta here.
                let sweep = sin.atan2(cos);
                let step = 2.0 * (1.0 - tol / r).clamp(-1.0, 1.0).acos();
                let steps = ((sweep.abs() / step.max(1e-6)).ceil() as usize).clamp(1, 1024);
                out.push(at(p, n1x, n1y, delta));
                for s in 1..steps {
                    let a = a1 + sweep * s as f64 / steps as f64;
                    out.push(at(p, a.cos(), a.sin(), delta));
                }
                out.push(at(p, n2x, n2y, delta));
            }
        }
    }
}

fn square_join(p: Pt, n1: (f64, f64), n2: (f64, f64), delta: f64, out: &mut Vec<Pt>) {
    // The square edge is perpendicular to the bisector at distance |delta|
    // from the vertex; its ends lie on the two offset lines.
    let t1 = (-n1.1, n1.0);
    let t2 = (-n2.1, n2.0);
    let (bx, by) = (n1.0 + n2.0, n1.1 + n2.1);
    let bl = (bx * bx + by * by).sqrt();
    let u = if bl < 1e-9 { t1 } else { (bx / bl, by / bl) };
    let d1 = t1.0 * u.0 + t1.1 * u.1;
    let d2 = t2.0 * u.0 + t2.1 * u.1;
    if d1.abs() < 1e-9 || d2.abs() < 1e-9 {
        out.push(Pt::new((p.x as f64 + n1.0 * delta).round() as i64, (p.y as f64 + n1.1 * delta).round() as i64));
        out.push(Pt::new((p.x as f64 + n2.0 * delta).round() as i64, (p.y as f64 + n2.1 * delta).round() as i64));
        return;
    }
    let s1 = delta * (1.0 - (n1.0 * u.0 + n1.1 * u.1)) / d1;
    let s2 = delta * (1.0 - (n2.0 * u.0 + n2.1 * u.1)) / d2;
    out.push(Pt::new((p.x as f64 + n1.0 * delta + t1.0 * s1).round() as i64, (p.y as f64 + n1.1 * delta + t1.1 * s1).round() as i64));
    out.push(Pt::new((p.x as f64 + n2.0 * delta + t2.0 * s2).round() as i64, (p.y as f64 + n2.1 * delta + t2.1 * s2).round() as i64));
}

/// Offset rings (contours counter-clockwise, holes clockwise) by `delta`
/// units; positive grows.
pub fn offset_polygons(rings: &[Polygon], delta: f64, join: Join) -> Vec<ExPolygon> {
    if delta == 0.0 {
        return clip::union(rings, FillRule::NonZero);
    }
    let raw: Vec<Polygon> = rings.iter().filter_map(|r| raw_ring(r, delta, join)).collect();
    clip::union(&raw, FillRule::Positive)
}

/// Points this close (units) to the last one kept are dropped before a ring is offset. The
/// booleans round crossings to the unit grid and can leave a tooth there: an edge a unit or so
/// long, pointing anywhere. Offset along its normal and mitred to the next edge's copy, such an
/// edge cuts a notch up to the miter's reach into the region. Dropping it moves the ring by at
/// most this much.
const TOOTH: i128 = 2;

/// The cosine of a turn at or past which a ring turns back on itself: where such a corner needs a
/// join, [`offset_ring`] caps it short of the mitre's reach.
const REVERSAL: f64 = -0.99999;

/// A ring's points as an offset takes them: none within [`TOOTH`] of the last one kept, and the
/// last none within it of the first. A ring that would turn back on itself without its teeth
/// keeps them: at a needle's tip a tooth splits a near-180 degree turn in two, and the tip's cap
/// would reach less on a grow than the two mitres do.
fn untoothed(r: &Polygon) -> Vec<Pt> {
    let mut pts: Vec<Pt> = Vec::with_capacity(r.pts.len());
    for &p in &r.pts {
        if pts.last().is_none_or(|q| q.dist2(p) > TOOTH * TOOTH) {
            pts.push(p);
        }
    }
    while pts.len() > 1 && pts[0].dist2(pts[pts.len() - 1]) <= TOOTH * TOOTH {
        pts.pop();
    }
    if pts.len() != r.pts.len() {
        let mut exact = r.pts.clone();
        exact.dedup();
        while exact.len() > 1 && exact.first() == exact.last() {
            exact.pop();
        }
        if exact.len() != pts.len() && reversals(&pts) > reversals(&exact) {
            return exact;
        }
    }
    pts
}

/// The corners where a ring turns back on itself.
fn reversals(pts: &[Pt]) -> usize {
    let n = pts.len();
    if n < 3 {
        return 0;
    }
    let dir = |i: usize| {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
        let l = (dx * dx + dy * dy).sqrt();
        if l == 0.0 { (0.0, 0.0) } else { (dx / l, dy / l) }
    };
    (0..n)
        .filter(|&i| {
            let (u, v) = (dir((i + n - 1) % n), dir(i));
            u.0 * v.0 + u.1 * v.1 <= REVERSAL
        })
        .count()
}

/// The raw offset of one ring (before the union that cleans it up), as [`offset_polygons`]
/// unions it; `None` for a ring it skips. Every point of it lies within the join's reach of
/// the ring (a miter at most its limit times `delta`), and it passes through the ring's
/// vertices where offset edges overlap.
pub fn raw_ring(r: &Polygon, delta: f64, join: Join) -> Option<Polygon> {
    let pts = untoothed(r);
    if pts.len() < 3 {
        return None;
    }
    let mut out = Vec::with_capacity(pts.len() + 8);
    offset_ring(&pts, delta, join, &mut out);
    (out.len() >= 3).then(|| Polygon::new(out))
}

pub fn offset_ex(ex: &[ExPolygon], delta: f64, join: Join) -> Vec<ExPolygon> {
    offset_polygons(&to_polygons(ex), delta, join)
}

/// How far (units) under twice an inset's depth a region's width must stay for
/// [`inset_is_empty`] to answer without the inset: the offset points round by under a unit and
/// the snap rounding moves a segment by under one.
const INSET_MARGIN: f64 = 16.0;

/// Whether the inset of `e` by `r` units leaves nothing: `offset_ex(&[e], -r, DEFAULT_MITER)`'s
/// answer. A region (as the booleans make it: contour counter-clockwise, holes clockwise) that
/// fits between two parallel lines less than `2r` apart, less [`INSET_MARGIN`], gets it without
/// the inset. Per ring, the raw inset ring is the ring less, per edge, the `r`-wide rectangle on
/// the region's side and, per reflex corner, its miter kite or square cap (each holding the
/// corner's `r`-disk sector), plus a detour of no area through each convex corner. So every point
/// nearer the boundary than `r` winds at most 0 and the positive union keeps none of them, and in
/// so narrow a region every point is that near. A corner offset_ring joins otherwise (the ring
/// doubling back on itself) is outside this argument: such a region takes the inset, as does
/// every wider one.
pub fn inset_is_empty(e: &ExPolygon, r: f64) -> bool {
    if r > 0.0 && narrower_than(&e.contour, 2.0 * r - INSET_MARGIN) && joins_argued(e, -r) {
        return true;
    }
    offset_ex(std::slice::from_ref(e), -r, DEFAULT_MITER).is_empty()
}

/// Whether the round-join inset of `e` by `r` units with arc tolerance `tol` is argued empty
/// without making it; false means not argued, not that it keeps anything. As in [`inset_is_empty`],
/// the edge rectangles and reflex corners' caps cover a region narrow enough. [`offset_ring`]
/// clamps the tolerance to `t = tol.max(1.0).min(r)`: its chords and one-point joins stay within
/// `t` of the corner's `r`-disk (or, where it caps an arc at 1024 chords, within the sagitta of a
/// half turn's 1024th), so each cap holds the corner's `(r - t)`-disk sector. With `r > t`, a
/// region between parallel lines less than `2 * (r - t)` apart, less [`INSET_MARGIN`], therefore
/// leaves nothing, provided every corner is joined as that argument requires.
pub fn round_inset_argued_empty(e: &ExPolygon, r: f64, tol: f64) -> bool {
    let t = tol.max(1.0).min(r).max(r * (1.0 - (std::f64::consts::PI / 2048.0).cos()));
    r > t && narrower_than(&e.contour, 2.0 * (r - t) - INSET_MARGIN) && joins_argued(e, -r)
}

/// Whether `ring` lies between two parallel lines less than `w` units apart: its box's sides, then
/// its hull's edges (rotating calipers).
fn narrower_than(ring: &Polygon, w: f64) -> bool {
    if w <= 0.0 || ring.pts.len() < 3 {
        return false;
    }
    let b = ring.bbox();
    if ((b.max.x - b.min.x) as f64) < w || ((b.max.y - b.min.y) as f64) < w {
        return true;
    }
    let h = convex_hull(&mut ring.pts.clone());
    let n = h.len();
    if n < 3 {
        return false;
    }
    let mut j = 1;
    for i in 0..n {
        let (a, c) = (h[i], h[(i + 1) % n]);
        while orient(a, c, h[(j + 1) % n]) > orient(a, c, h[j]) {
            j = (j + 1) % n;
        }
        let len = ((c.x - a.x) as f64).hypot((c.y - a.y) as f64);
        if (orient(a, c, h[j]) as f64) < w * len {
            return true;
        }
    }
    false
}

/// Whether offset_ring by `delta` (< 0) joins every corner of `e` as [`inset_is_empty`] argues:
/// the rings oriented as the booleans make them, and every corner straight, a convex corner's
/// detour, or a reflex corner's miter or square cap short of doubling back. The same tests on
/// the same points as offset_polygons and offset_ring.
fn joins_argued(e: &ExPolygon, delta: f64) -> bool {
    if e.contour.area2() <= 0 || e.holes.iter().any(|h| h.area2() >= 0) {
        return false;
    }
    e.rings().all(|r| {
        let pts = untoothed(r);
        let n = pts.len();
        if n < 3 {
            return true;
        }
        let normal = |i: usize| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
            let l = (dx * dx + dy * dy).sqrt();
            (dy / l, -dx / l)
        };
        let mut n1 = normal(n - 1);
        (0..n).all(|i| {
            let n2 = normal(i);
            let sin = n1.0 * n2.1 - n1.1 * n2.0;
            let cos = n1.0 * n2.0 + n1.1 * n2.1;
            n1 = n2;
            let straight = (sin * delta).abs() < 1e-12 * delta.abs() && cos > 0.0;
            straight || sin * delta < 0.0 || (sin * delta > 0.0 && cos > REVERSAL)
        })
    })
}

pub fn offset_mm(ex: &[ExPolygon], delta_mm: f64) -> Vec<ExPolygon> {
    offset_ex(ex, delta_mm * SCALE, DEFAULT_MITER)
}

/// Shrink then grow: removes features thinner than `2 * shrink`.
pub fn opening(ex: &[ExPolygon], shrink: f64, grow: f64, join: Join) -> Vec<ExPolygon> {
    let a = offset_ex(ex, -shrink, join);
    if a.is_empty() {
        return a;
    }
    offset_ex(&a, grow, join)
}

/// Grow then shrink: closes gaps narrower than `2 * grow`.
pub fn closing(ex: &[ExPolygon], grow: f64, shrink: f64, join: Join) -> Vec<ExPolygon> {
    let a = offset_ex(ex, grow, join);
    if a.is_empty() {
        return a;
    }
    offset_ex(&a, -shrink, join)
}

/// How close (units) unrelated segments of the grown rings may come for [`closing_keeps`]:
/// the union moves each by under a unit.
const KEEP_CLEAR: f64 = 4.0;
/// The least length (units) of an edge's grown copy for [`closing_keeps`]: the union rounds its
/// ends by under a unit each, which turns a shorter copy by more than [`KEEP_TURN`] allows.
const KEEP_EDGE: f64 = 64.0;
/// How far `1 + cos` of each corner's turn must clear the miter limit's bound for
/// [`closing_keeps`], so that the shrink, on the grown region's rounded edges, mitres every
/// corner the grow did.
const KEEP_TURN: f64 = 0.05;

/// Whether `closing(ex, r, r, Join::Miter(limit))` gives `ex` back, up to its offsets'
/// rounding (a unit or two per vertex): no gap closes and no corner changes.
///
/// It does when the grow stays local. Every corner, convex or reflex, takes the miter (the grow
/// mitres the convex ones, the shrink the reflex ones); each edge's grown copy, trimmed where it
/// meets its neighbours' copies, keeps its direction; and the raw grown rings meet only at
/// their own corners (a segment and the next three of its ring: a reflex corner's detour) and
/// come within [`KEEP_CLEAR`] of each other nowhere else. The grown region is then exactly the
/// rings' trimmed copies, the shrink's offset lines are the original edges, and they meet again
/// at the original vertices.
pub fn closing_keeps(ex: &[ExPolygon], r: f64, limit: f64) -> bool {
    let bound = 2.0 / (limit * limit) + KEEP_TURN;
    let mut segs: Vec<(Pt, Pt, (u32, u32))> = Vec::new();
    let mut lens: Vec<u32> = Vec::new();
    let mut raw = Vec::new();
    let mut corner = Vec::new();
    for ring in ex.iter().flat_map(|e| e.rings()) {
        let pts = untoothed(ring);
        let n = pts.len();
        if n < 3 {
            return false;
        }
        // unit direction and outward normal of each edge, as offset_ring takes them
        let edge = |i: usize| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
            let l = (dx * dx + dy * dy).sqrt();
            ((dx / l, dy / l), (dy / l, -dx / l))
        };
        // each vertex's corner: where the grown copies of its two edges meet
        corner.clear();
        for i in 0..n {
            let ((_, n1), (_, n2)) = (edge((i + n - 1) % n), edge(i));
            let cos = n1.0 * n2.0 + n1.1 * n2.1;
            if 1.0 + cos <= bound {
                return false;
            }
            let k = r / (1.0 + cos);
            corner.push((pts[i].x as f64 + (n1.0 + n2.0) * k, pts[i].y as f64 + (n1.1 + n2.1) * k));
        }
        for i in 0..n {
            let ((dx, dy), _) = edge(i);
            let (s, t) = (corner[i], corner[(i + 1) % n]);
            if (t.0 - s.0) * dx + (t.1 - s.1) * dy < KEEP_EDGE {
                return false;
            }
        }
        raw.clear();
        offset_ring(&pts, r, Join::Miter(limit), &mut raw);
        let (id, m) = (lens.len() as u32, raw.len());
        segs.extend((0..m).map(|j| (raw[j], raw[(j + 1) % m], (id, j as u32))));
        lens.push(m as u32);
    }
    let grid = crate::grid::SegGrid::new(segs);
    let reach = KEEP_CLEAR as i64 + 1;
    grid.segments().iter().all(|&(a, b, (ring, j))| {
        let mut bb = BBox::of(&[a, b]);
        bb.min = Pt::new(bb.min.x - reach, bb.min.y - reach);
        bb.max = Pt::new(bb.max.x + reach, bb.max.y + reach);
        let mut hit = false;
        grid.visit_box(bb, |c, d, (ring2, j2)| {
            // each pair once; a ring's own corners meet
            if hit || (ring2, j2) <= (ring, j) || ring2 == ring && (j2 - j).min(lens[ring as usize] - (j2 - j)) <= 3 {
                return;
            }
            hit = segments_within(a, b, c, d, KEEP_CLEAR);
        });
        !hit
    })
}

/// Offset an open path into a band of half-width `delta` (square ends).
pub fn offset_polyline(pl: &[Pt], delta: f64) -> Vec<ExPolygon> {
    if pl.len() < 2 {
        return Vec::new();
    }
    // A lone segment gets its midpoint: a two-vertex ring has no out-and-back
    // corners to offset and comes back empty.
    let split;
    let pl = if pl.len() == 2 {
        split = [pl[0], Pt { x: (pl[0].x + pl[1].x) / 2, y: (pl[0].y + pl[1].y) / 2 }, pl[1]];
        &split[..]
    } else {
        pl
    };
    // Walk out along one side and back along the other, as a closed ring.
    let mut ring: Vec<Pt> = pl.to_vec();
    for p in pl.iter().rev().skip(1).take(pl.len().saturating_sub(2)) {
        ring.push(*p);
    }
    // The ring runs out and back along the path; offsetting it gives the
    // band, with square caps at the two reversals.
    let mut raw = Vec::new();
    offset_ring(&ring, delta, Join::Square, &mut raw);
    clip::union(&[Polygon::new(raw)], FillRule::Positive)
}

#[cfg(test)]
mod teeth_tests {
    use super::*;

    #[test]
    fn a_miter_offset_stays_within_its_reach() {
        let ring = |pts: &[(i64, i64)]| ExPolygon::new(Polygon::new(pts.iter().map(|&(x, y)| Pt::new(x, y)).collect()));
        for (name, e) in [
            ("square", ring(&[(0, 0), (1_000_000, 0), (1_000_000, 1_000_000), (0, 1_000_000)])),
            ("spike", ring(&[(0, 0), (1_000_000, 0), (100_000, 100_000)])),
            ("tooth", ring(&[(0, 0), (1_000_000, 0), (1_000_000, 1_000_000), (500_000, 1_000_000), (500_000, 1_000_001), (0, 1_000_001)])),
        ] {
            let bb = e.bbox();
            for delta in [-100_000.0, -500.0, 500.0, 100_000.0] {
                let reach = miter_reach(&e, delta, 3.0);
                let got = offset_ex(std::slice::from_ref(&e), delta, Join::Miter(3.0));
                if delta > 0.0 {
                    assert!(!got.is_empty(), "{name} grow {delta} keeps a region");
                }
                for p in got.iter().flat_map(|e| e.rings()).flat_map(|r| &r.pts) {
                    assert!(p.x as f64 >= bb.min.x as f64 - reach && p.x as f64 <= bb.max.x as f64 + reach
                        && p.y as f64 >= bb.min.y as f64 - reach && p.y as f64 <= bb.max.y as f64 + reach,
                        "{name} offset {delta}: {p:?} outside {bb:?} grown by {reach}");
                }
            }
        }
    }

    #[test]
    fn an_inset_of_a_ring_thinner_than_the_inset_leaves_nothing() {
        let ring = |pts: &[(i64, i64)]| ExPolygon::new(Polygon::new(pts.iter().map(|&(x, y)| Pt::new(x, y)).collect()));
        // Rings the booleans left, a unit or a few wide. Each turns back at a tip: the old cap
        // there came out past the ring's far end, 0.19 mm2 of the first one at 0.4 mm in.
        for hair in [
            ring(&[(199593812, 145350284), (199726880, 145434736), (199726888, 145434742)]),
            ring(&[(148337045, 147566852), (148665109, 146869907), (148753567, 146734576), (148665110, 146869907)]),
            ring(&[(147158295, 169586662), (147038892, 169145472), (147158298, 169586668), (147193679, 169681417)]),
            ring(&[(147970971, 149186252), (148052001, 148561255), (148052002, 148561252)]),
            // A tip that turns back exactly, its two edges on one line.
            ring(&[(0, 0), (100000, 0), (50000, 0), (40000, 3)]),
        ] {
            assert!(hair.contour.area2() > 0, "{hair:?}");
            for inset in [10_000.0, 100_000.0, 399_000.0, 806_000.0] {
                let got = offset_ex(std::slice::from_ref(&hair), -inset, DEFAULT_MITER);
                assert!(got.is_empty(), "inset {inset} of {hair:?}: {got:?}");
                assert!(inset_is_empty(&hair, inset), "{hair:?}");
            }
        }
    }

    #[test]
    fn a_round_inset_argued_empty_leaves_nothing() {
        let rect = |x, y| ExPolygon::new(Polygon::rect(Pt::new(0, 0), Pt::new(x, y)));
        let (r, t) = (450_000.0, 20_000.0);
        // Just inside the bound: 2 * (r - t) less the margin is 859_984.
        let thin = rect(8_000_000, 859_000);
        let mut ring = rect(8_000_000, 800_000);
        let mut hole = Polygon::rect(Pt::new(100_000, 100_000), Pt::new(7_900_000, 700_000));
        hole.reverse();
        ring.holes.push(hole);
        let l = ExPolygon::new(Polygon::new([(0, 0), (8_000_000, 0), (8_000_000, 200_000), (200_000, 200_000), (200_000, 800_000), (0, 800_000)].map(|(x, y)| Pt::new(x, y)).to_vec()));
        let wide = rect(8_000_000, 8_000_000);
        for (name, e) in [("thin strip", &thin), ("thin ring", &ring), ("thin L", &l)] {
            assert!(round_inset_argued_empty(e, r, t), "{name} is argued empty");
        }
        assert!(!round_inset_argued_empty(&rect(8_000_000, 861_000), r, t), "a strip past the bound is not argued empty");
        assert!(!round_inset_argued_empty(&wide, r, t), "wide square is not argued empty");
        for (name, e) in [("thin strip", thin), ("strip just over two insets", rect(8_000_000, 910_000)), ("thin ring", ring), ("thin L", l), ("wide square", wide)] {
            if round_inset_argued_empty(&e, r, t) {
                let got = offset_ex(std::slice::from_ref(&e), -r, Join::Round(t));
                assert!(got.is_empty(), "round inset of {name}: {got:?}");
            }
        }
    }

    #[test]
    fn a_round_join_keeps_to_its_arc_with_one_point_on_a_shallow_turn() {
        let (delta, tol) = (100_000.0, 1_000.0);
        // Three sharp corners and, on the long edge, one shallow turn.
        let (a, shallow, c, d) = (Pt::new(0, 0), Pt::new(1_000_000, -1_000), Pt::new(2_000_000, 0), Pt::new(1_000_000, 2_000_000));
        let mut out = Vec::new();
        offset_ring(&[a, shallow, c, d], delta, Join::Round(tol), &mut out);
        let reach = |p: Pt, q: Pt| (p.dist2(q) as f64).sqrt();
        // Every corner's points lie on its arc, up to rounding: one for the shallow turn.
        for (corner, one) in [(a, false), (shallow, true), (c, false), (d, false)] {
            let arc: Vec<Pt> = out.iter().copied().filter(|&p| reach(p, corner) <= delta + tol).collect();
            assert!(if one { arc.len() == 1 } else { arc.len() > 2 }, "{corner:?}: {arc:?}");
            assert!(arc.iter().all(|&p| (reach(p, corner) - delta).abs() <= 2.0), "{corner:?}: {arc:?}");
        }
    }

    #[test]
    fn a_tooth_goes_unless_the_ring_would_turn_back_on_itself() {
        let ring = |pts: &[(i64, i64)]| Polygon::new(pts.iter().map(|&(x, y)| Pt::new(x, y)).collect());
        // A near-straight top with a one-unit step: the step goes.
        let square = ring(&[(0, 0), (1_000_000, 0), (1_000_000, 1_000_000), (500_000, 1_000_000), (500_000, 1_000_001), (0, 1_000_001)]);
        assert_eq!(untoothed(&square).len(), 5);
        // A needle whose tip is one unit wide: without its tooth it reverses, so it keeps it.
        let needle = ring(&[(0, 0), (1_000_000, 0), (1_000_000, 1), (0, 10)]);
        assert_eq!(untoothed(&needle).len(), 4);
    }
}
