//! Boolean operations on filled outlines: union, intersection, difference
//! and exclusive or of two sets of closed polylines (curves are flattened
//! first, [`crate::geometry::flatten`]).
//!
//! Every edge of both operands is split where it meets any other edge; a
//! piece is kept when the result's inside differs on its two sides, turned
//! so the result lies on its left, and the kept pieces are linked into
//! closed contours. Each operand is filled by the non-zero rule, so
//! self-overlapping outlines (a glyph's overlapping strokes, a path that
//! crosses itself) behave as they draw.

pub type Contour = Vec<[f64; 2]>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    Union,
    Intersection,
    /// The first operand without the second.
    Difference,
    /// Inside exactly one of the two.
    Xor,
}

impl BoolOp {
    pub fn from_id(id: &str) -> Option<BoolOp> {
        Some(match id {
            "union" => BoolOp::Union,
            "intersection" | "intersect" => BoolOp::Intersection,
            "difference" | "subtract" => BoolOp::Difference,
            "xor" | "exclusion" => BoolOp::Xor,
            _ => return None,
        })
    }

    fn keep(self, a: bool, b: bool) -> bool {
        match self {
            BoolOp::Union => a || b,
            BoolOp::Intersection => a && b,
            BoolOp::Difference => a && !b,
            BoolOp::Xor => a != b,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    a: [f64; 2],
    b: [f64; 2],
    /// Which operand (0 or 1) it came from.
    side: u8,
}

fn edges_of(contours: &[Contour], side: u8, out: &mut Vec<Edge>) {
    for c in contours.iter().filter(|c| c.len() >= 3) {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            if a != b && a.iter().chain(&b).all(|v| v.is_finite()) {
                out.push(Edge { a, b, side });
            }
        }
    }
}

/// Non-zero winding of `p` against one operand's edges.
fn winding(edges: &[Edge], side: u8, p: [f64; 2]) -> i32 {
    let mut w = 0;
    for e in edges.iter().filter(|e| e.side == side) {
        let (a, b) = (e.a, e.b);
        if (a[1] <= p[1]) != (b[1] <= p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if x > p[0] {
                w += if b[1] > a[1] { 1 } else { -1 };
            }
        }
    }
    w
}

fn cross(o: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Where segment `e` is cut by segment `f`, as parameters along `e` in
/// (0, 1): one crossing, or the ends of a collinear overlap.
fn cuts(e: &Edge, f: &Edge, eps: f64, out: &mut Vec<f64>) {
    let (p, r) = (e.a, [e.b[0] - e.a[0], e.b[1] - e.a[1]]);
    let (q, s) = (f.a, [f.b[0] - f.a[0], f.b[1] - f.a[1]]);
    let rxs = r[0] * s[1] - r[1] * s[0];
    let qp = [q[0] - p[0], q[1] - p[1]];
    let rr = r[0] * r[0] + r[1] * r[1];
    if rr == 0.0 {
        return;
    }
    let len = rr.sqrt() * (s[0] * s[0] + s[1] * s[1]).sqrt();
    if rxs.abs() <= 1e-12 * len {
        // Parallel: only collinear overlaps cut.
        if (qp[0] * r[1] - qp[1] * r[0]).abs() > eps * rr.sqrt() {
            return;
        }
        for end in [f.a, f.b] {
            let t = ((end[0] - p[0]) * r[0] + (end[1] - p[1]) * r[1]) / rr;
            if t > 1e-9 && t < 1.0 - 1e-9 {
                out.push(t);
            }
        }
        return;
    }
    let t = (qp[0] * s[1] - qp[1] * s[0]) / rxs;
    let u = (qp[0] * r[1] - qp[1] * r[0]) / rxs;
    let (tol_t, tol_u) = (eps / rr.sqrt(), eps / (s[0] * s[0] + s[1] * s[1]).sqrt());
    if t > tol_t && t < 1.0 - tol_t && u > -tol_u && u < 1.0 + tol_u {
        out.push(t);
    }
}

fn bounds(edges: &[Edge]) -> [f64; 4] {
    edges.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, e| {
        [b[0].min(e.a[0]).min(e.b[0]), b[1].min(e.a[1]).min(e.b[1]), b[2].max(e.a[0]).max(e.b[0]), b[3].max(e.a[1]).max(e.b[1])]
    })
}

/// Every edge split at the points where any other edge meets it. A uniform
/// grid of buckets keeps it near-linear for long outlines.
fn split(edges: &[Edge], eps: f64) -> Vec<Edge> {
    let bb = bounds(edges);
    let n = ((edges.len() as f64).sqrt().ceil() as usize).clamp(1, 256);
    let (w, h) = ((bb[2] - bb[0]).max(eps), (bb[3] - bb[1]).max(eps));
    let cell = |x: f64, y: f64| -> (usize, usize) {
        (((x - bb[0]) / w * n as f64).floor().clamp(0.0, (n - 1) as f64) as usize, ((y - bb[1]) / h * n as f64).floor().clamp(0.0, (n - 1) as f64) as usize)
    };
    let mut grid: Vec<Vec<u32>> = vec![Vec::new(); n * n];
    let ranges: Vec<((usize, usize), (usize, usize))> = edges
        .iter()
        .map(|e| {
            let lo = cell(e.a[0].min(e.b[0]) - eps, e.a[1].min(e.b[1]) - eps);
            let hi = cell(e.a[0].max(e.b[0]) + eps, e.a[1].max(e.b[1]) + eps);
            (lo, hi)
        })
        .collect();
    for (i, (lo, hi)) in ranges.iter().enumerate() {
        for gy in lo.1..=hi.1 {
            for gx in lo.0..=hi.0 {
                grid[gy * n + gx].push(i as u32);
            }
        }
    }
    let mut out = Vec::with_capacity(edges.len() * 2);
    let mut seen: Vec<u32> = vec![u32::MAX; edges.len()];
    let mut ts = Vec::new();
    for (i, e) in edges.iter().enumerate() {
        ts.clear();
        let (lo, hi) = ranges[i];
        for gy in lo.1..=hi.1 {
            for gx in lo.0..=hi.0 {
                for &j in &grid[gy * n + gx] {
                    let j = j as usize;
                    if j == i || seen[j] == i as u32 {
                        continue;
                    }
                    seen[j] = i as u32;
                    cuts(e, &edges[j], eps, &mut ts);
                }
            }
        }
        ts.sort_by(f64::total_cmp);
        let mut from = e.a;
        let mut last_t = 0.0;
        for &t in &ts {
            if t - last_t <= 1e-12 {
                continue;
            }
            let p = [e.a[0] + (e.b[0] - e.a[0]) * t, e.a[1] + (e.b[1] - e.a[1]) * t];
            out.push(Edge { a: from, b: p, side: e.side });
            from = p;
            last_t = t;
        }
        out.push(Edge { a: from, b: e.b, side: e.side });
    }
    out
}

/// `op` of the regions `a` and `b` (each filled by the non-zero rule), as
/// closed contours (the result's inside on each contour's left in a y-up
/// frame: counter-clockwise outer boundaries there, holes clockwise; fill
/// the result by the non-zero rule). Contours are polylines; flatten curves
/// before (to the output's tolerance).
pub fn boolean(a: &[Contour], b: &[Contour], op: BoolOp) -> Vec<Contour> {
    let mut edges = Vec::new();
    edges_of(a, 0, &mut edges);
    edges_of(b, 1, &mut edges);
    if edges.is_empty() {
        return Vec::new();
    }
    let bb = bounds(&edges);
    let scale = (bb[2] - bb[0]).abs().max((bb[3] - bb[1]).abs()).max(1e-9);
    let eps = scale * 1e-9;
    let pieces = split(&edges, eps);
    // Snap ends so pieces that meet share a key.
    let q = 1.0 / (scale * 1e-8);
    let key = |p: [f64; 2]| ((p[0] * q).round() as i64, (p[1] * q).round() as i64);
    let probe = scale * 1e-6;
    let inside = |p: [f64; 2]| op.keep(winding(&edges, 0, p) != 0, winding(&edges, 1, p) != 0);
    let mut kept: Vec<([f64; 2], [f64; 2])> = Vec::new();
    let mut have = std::collections::HashSet::new();
    for e in &pieces {
        let d = [e.b[0] - e.a[0], e.b[1] - e.a[1]];
        let len = d[0].hypot(d[1]);
        if len <= eps || key(e.a) == key(e.b) {
            continue;
        }
        let m = [(e.a[0] + e.b[0]) * 0.5, (e.a[1] + e.b[1]) * 0.5];
        let n = [-d[1] / len * probe, d[0] / len * probe];
        let (left, right) = (inside([m[0] + n[0], m[1] + n[1]]), inside([m[0] - n[0], m[1] - n[1]]));
        if left == right {
            continue;
        }
        let (from, to) = if left { (e.a, e.b) } else { (e.b, e.a) };
        // Coincident pieces of the two operands are one boundary.
        if have.insert((key(from), key(to))) {
            kept.push((from, to));
        }
    }
    // Link the pieces into contours: at a point with several ways on, take
    // the sharpest turn to the left, so touching loops come apart cleanly.
    let mut starts: std::collections::HashMap<(i64, i64), Vec<usize>> = std::collections::HashMap::new();
    for (i, (from, _)) in kept.iter().enumerate() {
        starts.entry(key(*from)).or_default().push(i);
    }
    let mut used = vec![false; kept.len()];
    let mut out = Vec::new();
    for first in 0..kept.len() {
        if used[first] {
            continue;
        }
        used[first] = true;
        let mut contour = vec![kept[first].0];
        let mut current = first;
        loop {
            let (from, to) = kept[current];
            let next = starts.get(&key(to)).and_then(|options| {
                options.iter().copied().filter(|&j| !used[j]).max_by(|&i, &j| {
                    let turn = |k: usize| {
                        let d0 = [to[0] - from[0], to[1] - from[1]];
                        let d1 = [kept[k].1[0] - to[0], kept[k].1[1] - to[1]];
                        (d0[0] * d1[1] - d0[1] * d1[0]).atan2(d0[0] * d1[0] + d0[1] * d1[1])
                    };
                    turn(i).total_cmp(&turn(j))
                })
            });
            match next {
                Some(j) => {
                    used[j] = true;
                    contour.push(kept[j].0);
                    current = j;
                }
                None => break,
            }
        }
        // Drop collinear middle points and contours without area.
        let mut clean: Contour = Vec::with_capacity(contour.len());
        for i in 0..contour.len() {
            let (p, c, n) = (contour[(i + contour.len() - 1) % contour.len()], contour[i], contour[(i + 1) % contour.len()]);
            if cross(p, c, n).abs() > eps * scale {
                clean.push(c);
            }
        }
        let area: f64 = (0..clean.len()).map(|i| { let (p, q) = (clean[i], clean[(i + 1) % clean.len()]); p[0] * q[1] - q[0] * p[1] }).sum();
        if clean.len() >= 3 && area.abs() > scale * scale * 1e-12 {
            out.push(clean);
        }
    }
    out
}

/// The signed area of contours (y up: counter-clockwise positive).
pub fn area(contours: &[Contour]) -> f64 {
    contours.iter().map(|c| (0..c.len()).map(|i| { let (p, q) = (c[i], c[(i + 1) % c.len()]); p[0] * q[1] - q[0] * p[1] }).sum::<f64>() * 0.5).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Contour {
        vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]]
    }

    fn circle(cx: f64, cy: f64, r: f64, n: usize) -> Contour {
        (0..n).map(|i| { let a = i as f64 / n as f64 * std::f64::consts::TAU; [cx + r * a.cos(), cy + r * a.sin()] }).collect()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6 * b.abs().max(1.0)
    }

    #[test]
    fn overlapping_squares() {
        let (a, b) = (vec![square(0.0, 0.0, 2.0)], vec![square(1.0, 1.0, 2.0)]);
        assert!(close(area(&boolean(&a, &b, BoolOp::Union)), 7.0));
        assert!(close(area(&boolean(&a, &b, BoolOp::Intersection)), 1.0));
        assert!(close(area(&boolean(&a, &b, BoolOp::Difference)), 3.0));
        assert!(close(area(&boolean(&a, &b, BoolOp::Xor)), 6.0));
        assert_eq!(boolean(&a, &b, BoolOp::Union).len(), 1);
        assert_eq!(boolean(&a, &b, BoolOp::Intersection)[0].len(), 4);
    }

    #[test]
    fn a_hole_and_disjoint_shapes() {
        let (a, b) = (vec![square(0.0, 0.0, 4.0)], vec![square(1.0, 1.0, 2.0)]);
        let hole = boolean(&a, &b, BoolOp::Difference);
        assert_eq!(hole.len(), 2);
        assert!(close(area(&hole), 12.0));
        let far = vec![square(10.0, 0.0, 1.0)];
        assert!(close(area(&boolean(&a, &far, BoolOp::Union)), 17.0));
        assert!(boolean(&a, &far, BoolOp::Intersection).is_empty());
    }

    #[test]
    fn shared_edges_and_winding_direction() {
        // Two squares side by side merge into one rectangle; the second is
        // written clockwise, which non-zero filling does not mind.
        let mut b = square(2.0, 0.0, 2.0);
        b.reverse();
        let u = boolean(&[square(0.0, 0.0, 2.0)], &[b], BoolOp::Union);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].len(), 4);
        assert!(close(area(&u), 8.0));
    }

    #[test]
    fn circles_and_self_overlap() {
        let (a, b) = (vec![circle(0.0, 0.0, 1.0, 256)], vec![circle(1.0, 0.0, 1.0, 256)]);
        let lens = area(&boolean(&a, &b, BoolOp::Intersection));
        // The lens of two unit circles a radius apart: 2π/3 - √3/2.
        assert!((lens - (2.0 * std::f64::consts::PI / 3.0 - 3f64.sqrt() / 2.0)).abs() < 2e-3, "{lens}");
        // One operand made of two overlapping squares (non-zero) unions as their outline.
        let twice = vec![square(0.0, 0.0, 2.0), square(1.0, 0.0, 2.0)];
        assert!(close(area(&boolean(&twice, &[], BoolOp::Union)), 6.0));
    }
}
