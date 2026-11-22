//! Exact integer primitives for octilinear bands; no predicate uses a floating point value.

use super::band::PartKey;
use super::hull::{Hull, NORMALS};
use super::side::Keep;
use fab_elec_core::geom::orient;
use fab_elec_core::{Nm, P};
use std::cmp::Ordering;

/// Straight-first and diagonal-first corners, or `[p, p]` for an octilinear chord.
pub fn corners(p: P, q: P) -> [P; 2] {
    let dx = q.x as i64 - p.x as i64;
    let dy = q.y as i64 - p.y as i64;
    let (x, y) = (dx.abs(), dy.abs());
    if x == 0 || y == 0 || x == y {
        return [p, p];
    }
    // Each coordinate is between its two input coordinates, including at Nm's limits.
    if x > y {
        [
            P::new((q.x as i64 - dx.signum() * y) as Nm, p.y),
            P::new((p.x as i64 + dx.signum() * y) as Nm, q.y),
        ]
    } else {
        [
            P::new(p.x, (q.y as i64 - dy.signum() * x) as Nm),
            P::new(q.x, (p.y as i64 + dy.signum() * x) as Nm),
        ]
    }
}

/// Axis and diagonal coefficients of length; unrepresentable coefficients saturate at i64::MAX.
/// The band operations check representability and report `Unsettled` before comparing lengths.
pub fn len(points: &[P]) -> (i64, i64) {
    points.windows(2).fold((0_i64, 0_i64), |(axis, diag), pair| {
        let dx = (pair[1].x as i64 - pair[0].x as i64).abs();
        let dy = (pair[1].y as i64 - pair[0].y as i64).abs();
        if dx == dy {
            (axis, diag.saturating_add(dx))
        } else {
            (axis.saturating_add(dx.max(dy)), diag)
        }
    })
}

/// Compare `axis + diag * sqrt(2)` exactly, including opposite signs and full i64 inputs.
pub fn cmp_len(x: (i64, i64), y: (i64, i64)) -> Ordering {
    let a = x.0 as i128 - y.0 as i128;
    let d = x.1 as i128 - y.1 as i128;
    if a == 0 {
        return d.cmp(&0);
    }
    if d == 0 || a.signum() == d.signum() {
        return a.cmp(&0);
    }
    let cmp = products([a.abs(), a.abs(), 1], [d.abs(), d.abs(), 2]);
    if a > 0 { cmp } else { cmp.reverse() }
}

/// Entry infimum in [0, 1] when a closed segment meets the octagon's open interior.
pub fn enters(hull: &Hull, a: P, b: P) -> Option<(i128, i128)> {
    let mut lo = (0, 1);
    let mut hi = (1, 1);
    for (k, &(nx, ny)) in NORMALS.iter().enumerate() {
        let start = nx as i128 * a.x as i128 + ny as i128 * a.y as i128;
        let delta = nx as i128 * (b.x as i128 - a.x as i128)
            + ny as i128 * (b.y as i128 - a.y as i128);
        let room = hull.s[k] as i128 - start;
        if delta == 0 {
            if room <= 0 {
                return None;
            }
        } else if delta > 0 {
            let bound = (room, delta);
            if rational_cmp(bound, hi).is_lt() {
                hi = bound;
            }
        } else {
            let bound = (-room, -delta);
            if rational_cmp(bound, lo).is_gt() {
                lo = bound;
            }
        }
    }
    // Every hull plane is strict. Equality of clip bounds is just a touch, not entry.
    if rational_cmp(lo, hi).is_lt() { Some(lo) } else { None }
}

/// Whether the open octagons overlap, using their eight separating directions.
pub fn octagons_overlap(x: &Hull, y: &Hull) -> bool {
    (0..8).all(|k| x.s[k] as i128 + y.s[(k + 4) % 8] as i128 > 0)
}

/// Convex boundary from a to b, CCW for Left and CW for Right, retaining endpoint vertices.
/// Shared vertices name the smallest part key.
///
/// A touch at a hidden endpoint: an endpoint on the run's own hull that the other endpoint's cap
/// covers (not a vertex of the hull of the run plus a and b) is where the path touches the run and
/// leaves. Its boundary is the direct leg [a, b] when that leg misses the run's open hull and every run
/// vertex ahead of the touch lies on the side's half-plane of a->b: the region between that leg and
/// any boundary path then holds no run interior. Any other endpoint the boundary misses is interior
/// and returns `None`.
pub fn chain(
    a: P, b: P, pts_of_run: &[(PartKey, P)], side: Keep,
) -> Option<Vec<(P, Option<PartKey>)>> {
    let mut points: Vec<_> = pts_of_run.iter().map(|&(key, p)| (p, Some(key))).collect();
    points.extend([(a, None), (b, None)]);
    points.sort_by_key(|&(p, key)| (p, key.is_none(), key));
    points.dedup_by_key(|point| point.0);
    if points.len() <= 2 || points.iter().all(|p| orient(points[0].0, points[points.len() - 1].0, p.0) == 0) {
        let at = |p| points.iter().find(|point| point.0 == p).copied();
        return if a == b { Some(vec![at(a)?]) } else { Some(vec![at(a)?, at(b)?]) };
    }
    let mut lower: Vec<(P, Option<PartKey>)> = Vec::new();
    let mut upper: Vec<(P, Option<PartKey>)> = Vec::new();
    // Stack pops are bounded by pushes over this finite collection.
    for &point in &points {
        for _ in 0..lower.len() {
            let n = lower.len();
            if n < 2 || orient(lower[n - 2].0, lower[n - 1].0, point.0) > 0 {
                break;
            }
            lower.pop();
        }
        lower.push(point);
    }
    for &point in points.iter().rev() {
        for _ in 0..upper.len() {
            let n = upper.len();
            if n < 2 || orient(upper[n - 2].0, upper[n - 1].0, point.0) > 0 {
                break;
            }
            upper.pop();
        }
        upper.push(point);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    // Reinsert a/b on collinear boundary edges, but no other collinear points.
    let mut boundary = Vec::new();
    for i in 0..lower.len() {
        let p = lower[i];
        let q = lower[(i + 1) % lower.len()];
        boundary.push(p);
        let mut ends: Vec<_> = [a, b].into_iter().filter(|&end| {
            end != p.0 && end != q.0 && on_segment(p.0, q.0, end)
        }).collect();
        ends.sort_by_key(|&end| p.0.dist2(end));
        ends.dedup();
        for end in ends {
            boundary.push(points.iter().find(|point| point.0 == end).copied()?);
        }
    }
    let (start, end) = (boundary.iter().position(|point| point.0 == a), boundary.iter().position(|point| point.0 == b));
    let (Some(start), Some(_)) = (start, end) else {
        let touch = touches_at_hidden_end(a, b, [start.is_none(), end.is_none()], pts_of_run, side);
        let at = |p| points.iter().find(|point| point.0 == p).copied();
        return if touch { Some(vec![at(a)?, at(b)?]) } else { None };
    };
    let mut result = Vec::new();
    let n = boundary.len();
    let mut index = start;
    for _ in 0..n {
        let point = boundary[index];
        result.push(point);
        if point.0 == b {
            return Some(result);
        }
        index = if side == Keep::Left {
            if index + 1 == n { 0 } else { index + 1 }
        } else if index == 0 { n - 1 } else { index - 1 };
    }
    None
}

/// The exact test for `chain`'s touch at a hidden endpoint (a != b; `hidden` names which of a, b the
/// boundary misses): the leg misses the run's open hull, and every run vertex is on the side's
/// half-plane of a->b except those behind a hidden end (not ahead of it toward the other end).
fn touches_at_hidden_end(a: P, b: P, hidden: [bool; 2], run: &[(PartKey, P)], side: Keep) -> bool {
    if a == b { return false; }
    let hull = convex_ccw(run.iter().map(|&(_, p)| p).collect());
    let sided = |v: P| {
        let o = orient(a, b, v);
        if side == Keep::Left { o >= 0 } else { o <= 0 }
    };
    let ahead = |from: P, to: P, v: P| {
        (to.x as i128 - from.x as i128) * (v.x as i128 - from.x as i128)
            + (to.y as i128 - from.y as i128) * (v.y as i128 - from.y as i128) > 0
    };
    let behind = |v: P| (hidden[0] && !ahead(a, b, v)) || (hidden[1] && !ahead(b, a, v));
    misses_open_hull(a, b, &hull) && hull.iter().all(|&v| sided(v) || behind(v))
}

/// Whether the closed segment a-b misses the open interior of a CCW convex polygon: an exact
/// separating-axis test over the polygon's edges and the segment's own line.
fn misses_open_hull(a: P, b: P, hull: &[P]) -> bool {
    let n = hull.len();
    if n < 3 { return true; }
    (0..n).any(|i| orient(hull[i], hull[(i + 1) % n], a) <= 0 && orient(hull[i], hull[(i + 1) % n], b) <= 0)
        || hull.iter().all(|&v| orient(a, b, v) >= 0)
        || hull.iter().all(|&v| orient(a, b, v) <= 0)
}

/// Counter-clockwise convex hull without collinear points (monotone chain).
fn convex_ccw(mut points: Vec<P>) -> Vec<P> {
    points.sort();
    points.dedup();
    if points.len() < 3 { return points; }
    let mut hull: Vec<P> = Vec::new();
    for pass in 0..2 {
        let start = hull.len();
        let ordered: Vec<P> = if pass == 0 { points.clone() } else { points.iter().rev().copied().collect() };
        for p in ordered {
            // Stack pops are bounded by pushes over this finite collection.
            for _ in 0..hull.len() {
                let n = hull.len();
                if n < start + 2 || orient(hull[n - 2], hull[n - 1], p) > 0 { break; }
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// Even-odd membership with half-open y intervals and crossings strictly right of p.
pub fn in_polygon_odd(poly: &[P], p: P) -> bool {
    let mut inside = false;
    let Some(&last) = poly.last() else { return false };
    let mut a = last;
    for &b in poly {
        if a.y.min(b.y) <= p.y && p.y < a.y.max(b.y) {
            let o = orient(a, b, p);
            if (b.y > a.y && o > 0) || (b.y < a.y && o < 0) {
                inside = !inside;
            }
        }
        a = b;
    }
    inside
}

/// Exact comparison of rationals whose denominators are positive.
pub(crate) fn rational_cmp(a: (i128, i128), b: (i128, i128)) -> Ordering {
    if a.0.signum() != b.0.signum() {
        return a.0.cmp(&b.0);
    }
    let cmp = products([a.0.abs(), b.1, 1], [b.0.abs(), a.1, 1]);
    if a.0 < 0 { cmp.reverse() } else { cmp }
}

/// Compare products without overflowing i128; factors are nonnegative and at most i128::MAX.
pub(crate) fn products(a: [i128; 3], b: [i128; 3]) -> Ordering {
    fn wide(factors: [i128; 3]) -> [i128; 12] {
        const BASE: i128 = 1 << 32;
        let mut out = [0; 12];
        out[0] = 1;
        for factor in factors {
            let digits = [factor % BASE, (factor >> 32) % BASE,
                (factor >> 64) % BASE, factor >> 96];
            let mut next = [0; 12];
            for i in 0..12 {
                for j in 0..4.min(12 - i) {
                    next[i + j] += out[i] * digits[j];
                }
            }
            for i in 0..11 {
                next[i + 1] += next[i] / BASE;
                next[i] %= BASE;
            }
            out = next;
        }
        out.reverse();
        out
    }
    match (a[0].checked_mul(a[1]).and_then(|x| x.checked_mul(a[2])),
        b[0].checked_mul(b[1]).and_then(|x| x.checked_mul(b[2]))) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => wide(a).cmp(&wide(b)),
    }
}

/// Closed segment membership, including a zero length segment.
pub(crate) fn on_segment(a: P, b: P, p: P) -> bool {
    orient(a, b, p) == 0 && fab_elec_core::BBox::new(a, b).contains(p)
}

/// Checked hull vertices: malformed or unrepresentable callback geometry is a blocked input.
pub(crate) fn vertices(hull: &Hull) -> Option<[P; 8]> {
    let [s0, s1, s2, s3, s4, s5, s6, s7] = hull.s.map(i128::from);
    let xy = [(s0, s1 - s0), (s1 - s2, s2), (s2 - s3, s2), (-s4, s3 - s4),
        (-s4, s4 - s5), (s6 - s5, -s6), (s7 - s6, -s6), (s0, s0 - s7)];
    let mut result = [P::ZERO; 8];
    for (v, (x, y)) in result.iter_mut().zip(xy) {
        *v = P::new(Nm::try_from(x).ok()?, Nm::try_from(y).ok()?);
        if NORMALS.iter().zip(hull.s).any(|(&(nx, ny), s)| {
            nx as i128 * x + ny as i128 * y > s as i128
        }) {
            return None;
        }
    }
    if hull.strictly_contains(hull.c) { Some(result) } else { None }
}
