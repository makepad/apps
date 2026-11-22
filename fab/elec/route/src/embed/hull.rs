//! Convex octagon parts covering an obstacle inflated for a head's width and clearance.
//!
//! A centreline point on or outside every part, and not `covers`, has `clearance >= need`.
//! Constructing a part costs O(core points); the part count is about the core's length over
//! the inflated radius R. Slack measurements only choose splits, never decide legality.
//! Cache keys must include both `need` and `w`: a width key mid-gesture changes `w`.

use fab_elec_core::dist::core_distance;
use fab_elec_core::geom::{orient, ArcGeom};
use fab_elec_core::shape::Core;
use fab_elec_core::{clearance, BBox, Nm, P, Shape};

/// The eight outward normals, counter-clockwise from +x.
pub const NORMALS: [(i64, i64); 8] = [
    (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1),
];

/// One convex part: { x : NORMALS[k] . x <= s[k] for every k }.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hull {
    pub s: [i64; 8],
    /// The part's reference point for the side rule: the centre of its core part's box.
    /// Half-nm centres truncate toward zero, as in `BBox::center`.
    pub c: P,
    /// Which part of the obstacle (0-based, in construction order).
    pub part: u32,
}

impl Hull {
    /// This part moved by `by`: each support moves by its normal's dot with `by` and the centre by
    /// `by`, which is the moved shape's own part (the centre, a box's truncated midpoint, can differ
    /// by a nanometre where the midpoint's sum is negative and odd).
    pub fn translated(&self, by: P) -> Hull {
        let mut s = self.s;
        for (support, &(nx, ny)) in s.iter_mut().zip(NORMALS.iter()) {
            *support += nx * by.x as i64 + ny * by.y as i64;
        }
        Hull { s, c: self.c.add(by), part: self.part }
    }

    /// Vertex k is where planes k and k+1 (mod 8) meet; counter-clockwise and integer.
    /// Panics if a vertex cannot be represented by `P`; coordinates never wrap or clamp.
    pub fn verts(&self) -> [P; 8] {
        match self.try_verts() {
            Some(vertices) => vertices,
            None => panic!("octagon vertex is outside the Nm coordinate range"),
        }
    }

    /// Strictly inside: N_k . p < s[k] for every k. The boundary is outside (legal for a centreline).
    pub fn strictly_contains(&self, p: P) -> bool {
        NORMALS.iter().zip(self.s).all(|(&(nx, ny), s)| (nx * p.x as i64 + ny * p.y as i64) < s)
    }

    fn try_verts(&self) -> Option<[P; 8]> {
        // Widen before subtracting even for a caller-constructed Hull with arbitrary supports.
        let [s0, s1, s2, s3, s4, s5, s6, s7] = self.s.map(i128::from);
        let xy = [
            (s0, s1 - s0), (s1 - s2, s2), (s2 - s3, s2), (-s4, s3 - s4),
            (-s4, s4 - s5), (s6 - s5, -s6), (s7 - s6, -s6), (s0, s0 - s7),
        ];
        let mut vertices = [P::ZERO; 8];
        for (v, (x, y)) in vertices.iter_mut().zip(xy) {
            *v = P::new(Nm::try_from(x).ok()?, Nm::try_from(y).ok()?);
        }
        debug_assert!((0..8).all(|k| orient(vertices[k], vertices[(k + 1) % 8], vertices[(k + 2) % 8]) >= 0));
        Some(vertices)
    }
}

/// The parts of `shape` for a head `w` wide keeping `need` from it.
/// Nonpositive `need` is passable and returns no parts.
///
/// Requires nonnegative shape dimensions and head width. Panics if the resulting vertices
/// do not fit `P` (a shape at the edge of the coordinate range).
pub fn hull_parts(shape: &Shape, need: Nm, w: Nm) -> Vec<Hull> {
    if need <= 0 {
        return Vec::new();
    }
    let (core, core_radius) = shape.core();
    assert!(w >= 0 && core_radius >= 0.0, "hull widths and radii must be nonnegative");
    // Twice every core radius is an exactly representable integer, including odd track widths.
    let twice_radius = (2.0 * core_radius) as i64 + 2 * need as i64 + w as i64;
    let r = (twice_radius + 1) / 2;
    let mut parts = Vec::new();
    match &core {
        Core::Point(p) => append(&mut parts, vec![make_part(&[*p], r)]),
        Core::Seg(a, b) => split_segment(*a, *b, &core, r, &mut parts),
        Core::Arc(a, mid, b) => {
            if let Some(arc) = ArcGeom::new(*a, *mid, *b) {
                split(
                    &core, r, &mut parts,
                    |n| {
                        let sagitta = arc.radius * (1.0 - (arc.sweep.abs() / (2.0 * n as f64)).cos());
                        r.checked_add(sagitta.ceil() as i64)?.checked_add(1)
                    },
                    |i, n| {
                        if i == 0 { *a } else if i == n { *b } else { arc.at(i as f64 / n as f64) }
                    },
                );
            } else {
                // Shape::core normally already turns a degenerate arc into a segment.
                split_segment(*a, *b, &core, r, &mut parts);
            }
        }
        Core::Convex(points) => {
            let part = make_part(points, r);
            if within_bound(&part, &core, r) {
                append(&mut parts, vec![part]);
            } else {
                split_ring(points, &core, r, &mut parts);
            }
        }
        Core::Set(set) => {
            for polygon in set.polygons() {
                split_ring(&polygon.outer, &core, r, &mut parts);
                for hole in &polygon.holes {
                    split_ring(hole, &core, r, &mut parts);
                }
            }
        }
    }
    parts
}

/// Whether a centreline point breaks clearance through the shape's filled interior,
/// which boundary parts do not cover (filled polygons, convex cores split to their edges).
/// Uses the same clearance predicate for all shapes, including their boundary bands.
pub fn covers(shape: &Shape, need: Nm, w: Nm, p: P) -> bool {
    clearance(&Shape::Segment { a: p, b: p, w }, shape) < need
}

fn make_part(points: &[P], r: i64) -> Hull {
    let d = diagonal(r);
    let mut s = [i64::MIN; 8];
    for &p in points {
        for (k, &(nx, ny)) in NORMALS.iter().enumerate() {
            s[k] = s[k].max(nx * p.x as i64 + ny * p.y as i64);
        }
    }
    for (k, support) in s.iter_mut().enumerate() {
        *support += if k % 2 == 0 { r } else { d };
    }
    Hull { s, c: BBox::of_points(points.iter().copied()).center(), part: 0 }
}

/// ceil(R sqrt(2)), using a binary integer square root followed by an exact adjustment.
fn diagonal(r: i64) -> i64 {
    // Radii come from i32 dimensions, or from split's representable arc inflation.
    let r = r as i128;
    let square = 2 * r * r;
    let (mut lo, mut hi) = (r, 2 * r);
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if mid * mid <= square { lo = mid; } else { hi = mid; }
    }
    (lo + i128::from(lo * lo < square)) as i64
}

fn within_bound(part: &Hull, core: &Core<'_>, r: i64) -> bool {
    let Some(vertices) = part.try_verts() else { return false };
    let corner = r as f64 * ((4.0 - 2.0 * 2.0_f64.sqrt()).sqrt() - 1.0);
    let bound = 2.0 * corner + 2.0;
    vertices.iter().all(|&v| core_distance(&Core::Point(v), core) - r as f64 <= bound)
}

fn append(parts: &mut Vec<Hull>, new: Vec<Hull>) {
    for mut part in new {
        assert!(part.try_verts().is_some(), "octagon vertex is outside the Nm coordinate range");
        part.part = parts.len() as u32;
        parts.push(part);
    }
}

fn split_ring(points: &[P], core: &Core<'_>, r: i64, parts: &mut Vec<Hull>) {
    for (&a, &b) in points.iter().zip(points.iter().cycle().skip(1)) {
        if a != b {
            split_segment(a, b, core, r, parts);
        }
    }
}

fn split_segment(a: P, b: P, core: &Core<'_>, r: i64, parts: &mut Vec<Hull>) {
    split(core, r, parts, |n| Some(r + i64::from(n > 1)), |i, n| {
        // Round the absolute interpolated coordinates, with exact halves away from zero.
        // Each point comes directly from the endpoints, so rounding never accumulates.
        let coord = |a: Nm, b: Nm| {
            let value = a as i128 * (n - i) as i128 + b as i128 * i as i128;
            let n = n as i128;
            let rounded = if value >= 0 { (value + n / 2) / n } else { -((-value + n / 2) / n) };
            rounded as Nm
        };
        P::new(coord(a.x, b.x), coord(a.y, b.y))
    });
}

fn split(
    core: &Core<'_>, r: i64, parts: &mut Vec<Hull>,
    radius: impl Fn(usize) -> Option<i64>, point: impl Fn(usize, usize) -> P,
) {
    const LIMIT: usize = 1 << 20;
    let mut n = 1;
    loop {
        debug_assert!(n < LIMIT, "octagon subdivision reached its integer-range limit");
        // Oversized early arc sagittae can become representable after subdivision.
        if let Some(r_part) = radius(n).filter(|&r| r <= Nm::MAX as i64) {
            let candidates: Vec<_> = (0..n).map(|i| make_part(&[point(i, n), point(i + 1, n)], r_part)).collect();
            if candidates.iter().all(|p| within_bound(p, core, r)) || n == LIMIT {
                append(parts, candidates);
                return;
            }
        } else if n == LIMIT {
            panic!("octagon inflation is outside the Nm coordinate range");
        }
        n *= 2;
    }
}
