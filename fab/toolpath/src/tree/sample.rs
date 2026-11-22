//! Points along a line at most a spacing apart (R14 §4.4, "sampling a line").
//!
//! - A line shorter than twice the spacing gets one point when one is enough: half its length
//!   from its first stored point, found from its end back and truncated to whole nm. A closed line
//!   (first point = last point) counts its closing segment and is not turned, so on a ring the
//!   point is half the perimeter from wherever the boolean that made the ring started it.
//! - With more points to place, a closed line first turns to start at one end of its longest
//!   vertex-to-vertex distance; the other end is its far end. An open line's far end is its last
//!   point.
//! - The walk takes the start, then the far end when more than one point is asked for or it lies
//!   farther than the spacing from the start.
//! - It then steps to where the line next crosses a circle round the current point, searching
//!   forward. A candidate at least the spacing from every point taken is taken and the circle is
//!   the spacing again. Otherwise the candidate becomes the current point, and the circle is the
//!   spacing less its distance to the nearest taken point, at least 0.1 mm.
//! - Too few points: the spacing shrinks by 10 % and the line is sampled again, while it is still
//!   at least 0.1 mm.

use makepad_fab_slicer::geom::*;

/// The smallest spacing (mm) a line is sampled at, and the smallest step of the walk.
const LEAST_SPACING: f64 = 0.1;
/// One nm in units: the walk's distances are compared to it.
const NM: f64 = SCALE * 1e-6;
/// How much each retry keeps of the spacing.
const RETRY: f64 = 0.9;

/// The points of `line` at most `spacing` (mm) apart, at least `min` of them where the spacing
/// can shrink to give them. The line is closed when its first point is its last.
pub fn sample(line: &[Pt], spacing: f64, min: usize) -> Vec<Pt> {
    let mut s = spacing;
    loop {
        let pts = once(line, s, min);
        if pts.len() >= min || s * RETRY < LEAST_SPACING {
            return pts;
        }
        s *= RETRY;
    }
}

/// A point in units, unrounded while the walk runs.
type P = (f64, f64);

fn p(q: Pt) -> P {
    (q.x as f64, q.y as f64)
}

fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn lerp(a: P, b: P, t: f64) -> P {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn pt(q: P) -> Pt {
    Pt::new(q.0.round() as i64, q.1.round() as i64)
}

/// The point `back` (units) along `path` from its end, walking back.
fn back_from_end(path: &[P], mut back: f64) -> P {
    for w in path.windows(2).rev() {
        let l = dist(w[0], w[1]);
        if back <= l {
            return if l > 0.0 { lerp(w[1], w[0], back / l) } else { w[1] };
        }
        back -= l;
    }
    path[0]
}

/// A closed line turned to start at one end of its longest vertex-to-vertex distance, closed
/// again at that end, and the distance's other end.
fn from_longest(ring: &[P]) -> (Vec<P>, P) {
    let n = ring.len();
    let (mut a, mut b, mut best) = (0, 0, -1.0);
    for i in 0..n {
        for j in i + 1..n {
            let d = dist(ring[i], ring[j]);
            if d > best {
                (a, b, best) = (i, j, d);
            }
        }
    }
    ((0..=n).map(|k| ring[(a + k) % n]).collect(), ring[b])
}

/// Where `path` first leaves the circle of radius `r` round `c`, which lies on segment `seg`:
/// that point and its segment, none when the path ends inside the circle.
fn crossing(path: &[P], seg: usize, c: P, r: f64) -> Option<(P, usize)> {
    for j in seg..path.len() - 1 {
        let (a, b) = (path[j], path[j + 1]);
        if dist(b, c) < r {
            continue;
        }
        // The larger root of |a + t (b − a) − c| = r: the segment starts inside the circle.
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (fx, fy) = (a.0 - c.0, a.1 - c.1);
        let qa = dx * dx + dy * dy;
        let qb = 2.0 * (fx * dx + fy * dy);
        let qc = fx * fx + fy * fy - r * r;
        let t = (-qb + (qb * qb - 4.0 * qa * qc).max(0.0).sqrt()) / (2.0 * qa);
        return Some((lerp(a, b, t.clamp(0.0, 1.0)), j));
    }
    None
}

fn once(line: &[Pt], spacing: f64, min: usize) -> Vec<Pt> {
    if line.is_empty() {
        return Vec::new();
    }
    let pts: Vec<P> = line.iter().map(|&q| p(q)).collect();
    let s = spacing * SCALE;
    // One point (§4.4): half the line's length from its first stored point, as it stands, a
    // closed line's closing segment counted; found from the end back, truncated to whole nm.
    let len: f64 = pts.windows(2).map(|w| dist(w[0], w[1])).sum();
    if min <= 1 && len < 2.0 * s {
        let q = back_from_end(&pts, len / 2.0);
        return vec![Pt::new(q.0 as i64, q.1 as i64)];
    }
    let closed = line.len() > 3 && line[0] == line[line.len() - 1];
    let (path, far) = if closed { from_longest(&pts[..pts.len() - 1]) } else { (pts.clone(), pts[pts.len() - 1]) };
    let mut taken = vec![path[0]];
    if min > 1 || dist(far, path[0]) > s {
        taken.push(far);
    }
    let (mut cur, mut seg, mut r) = (path[0], 0, s);
    while let Some((q, j)) = crossing(&path, seg, cur, r) {
        let near = taken.iter().map(|&t| dist(t, q)).fold(f64::INFINITY, f64::min);
        // To the nearest nm: a candidate on the circle round a taken point is the spacing from it.
        if near >= s - NM {
            taken.push(q);
            r = s;
        } else {
            r = (s - near).max(LEAST_SPACING * SCALE);
        }
        (cur, seg) = (q, j);
    }
    taken.into_iter().map(pt).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(pts: &[(f64, f64)]) -> Vec<Pt> {
        pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect()
    }

    fn mm(p: Pt) -> (f64, f64) {
        ((p.x_mm() * 1000.0).round() / 1000.0, (p.y_mm() * 1000.0).round() / 1000.0)
    }

    fn mms(pts: &[Pt]) -> Vec<(f64, f64)> {
        pts.iter().map(|&p| mm(p)).collect()
    }

    /// §4.4: a short line gets its middle; a long one its start, its far end, then the circle
    /// walk; a candidate too near a taken point moves the circle on by the shortfall.
    #[test]
    fn a_line_is_sampled_by_the_circle_walk() {
        assert_eq!(mms(&sample(&line(&[(0.0, 0.0), (3.0, 0.0)]), 1.78, 1)), vec![(1.5, 0.0)], "under twice the spacing: one point, at half its length");
        // 3.56 is 1.44 from the far end: it moves the circle to 0.34, then 0.68 (from 3.90),
        // then 1.36 (from 4.58), past the end.
        assert_eq!(mms(&sample(&line(&[(0.0, 0.0), (5.0, 0.0)]), 1.78, 1)), vec![(0.0, 0.0), (5.0, 0.0), (1.78, 0.0)], "the start, the far end, then the walk");
        let ten = sample(&line(&[(0.0, 0.0), (10.0, 0.0)]), 1.78, 1);
        assert_eq!(mms(&ten), vec![(0.0, 0.0), (10.0, 0.0), (1.78, 0.0), (3.56, 0.0), (5.34, 0.0), (7.12, 0.0)], "8.90 is too near the far end");
        // The walk measures straight-line distance: round a right angle the candidate is 1.78
        // from the corner's far side, not 1.78 along the line.
        let bend = sample(&line(&[(0.0, 0.0), (1.0, 0.0), (1.0, 5.0)]), 1.78, 1);
        let first = bend[2];
        assert!((first.dist(Pt::from_mm(0.0, 0.0)) / SCALE - 1.78).abs() < 1e-6, "a straight-line circle: {:?}", mm(first));
        assert!((first.x_mm() - 1.0).abs() < 1e-6 && (first.y_mm() - (1.78f64.powi(2) - 1.0).sqrt()).abs() < 1e-6, "on the second segment: {:?}", mm(first));
    }

    /// §4.4: one point on a line is half its length from its first stored point, truncated to
    /// whole nm; on a ring, half the perimeter from its first stored point, the ring not turned to
    /// its longest distance first.
    #[test]
    fn one_point_is_half_the_length_from_the_first_stored_point() {
        let nm = |pts: &[(i64, i64)]| pts.iter().map(|&(x, y)| Pt::new(x, y)).collect::<Vec<_>>();
        assert_eq!(sample(&nm(&[(0, 0), (1001, 0)]), 1.78, 1), nm(&[(500, 0)]), "500.5 nm truncates to 500");
        assert_eq!(sample(&nm(&[(0, 0), (-1001, 0)]), 1.78, 1), nm(&[(-500, 0)]), "−500.5 nm truncates towards zero");
        // A right triangle, 3.414 mm round: half way round from (0, 0) is the hypotenuse's middle.
        // Turned to its longest distance first, it would start at (1, 0) and land at (0, 0.707).
        let tri = line(&[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0)]);
        assert_eq!(mms(&sample(&tri, 1.78, 1)), vec![(0.5, 0.5)], "half the perimeter from the first stored point");
        // The same ring stored from (1, 0) puts its point elsewhere: where it lands follows the
        // first stored point, not the shape.
        let from = line(&[(1.0, 0.0), (0.0, 1.0), (0.0, 0.0), (1.0, 0.0)]);
        assert_eq!(mms(&sample(&from, 1.78, 1)), vec![(0.0, 0.707)], "the ring's first stored point decides");
        // An open line of the same three points has no closing segment: 2.414 mm, so its middle
        // is 1.207 mm from (0, 0), on the hypotenuse 0.207 mm from (1, 0).
        let open = line(&[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]);
        assert_eq!(mms(&sample(&open, 1.78, 1)), vec![(0.854, 0.146)], "an open line counts no closing segment");
    }

    /// §4.4: a closed line with more points to place starts at one end of its longest
    /// vertex-to-vertex distance and takes the other end next.
    #[test]
    fn a_closed_line_starts_on_its_longest_distance() {
        let ring = line(&[(0.0, 1.0), (0.0, 0.0), (4.0, 0.0), (4.0, 1.0), (0.0, 1.0)]);
        let pts = sample(&ring, 1.78, 1);
        assert_eq!(mms(&pts[..2]), vec![(0.0, 1.0), (4.0, 0.0)], "the first longest distance, (0, 1) to (4, 0)");
        assert!(pts.iter().all(|a| pts.iter().all(|b| a == b || a.dist(*b) >= 1.78 * SCALE - 1.0)), "no two points nearer than the spacing: {:?}", mms(&pts));
    }

    /// §4.4: with too few points the spacing shrinks by 10 % at a time while it is at least
    /// 0.1 mm, and more than one point asked for always takes the far end.
    #[test]
    fn too_few_points_shrink_the_spacing() {
        // 1.78 and 1.602 give two points, 1.4418 gives three.
        let three = sample(&line(&[(0.0, 0.0), (3.0, 0.0)]), 1.78, 3);
        assert_eq!(mms(&three), vec![(0.0, 0.0), (3.0, 0.0), (1.442, 0.0)], "the spacing shrinks until three come out");
        let few = sample(&line(&[(0.0, 0.0), (0.05, 0.0)]), 1.78, 3);
        assert_eq!(mms(&few), vec![(0.0, 0.0), (0.05, 0.0)], "not past 0.1 mm: a 0.05 mm line keeps its two ends");
    }
}
