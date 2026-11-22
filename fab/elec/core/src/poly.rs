//! Polygon sets on the nm grid: booleans and offsets over `libs/i_overlay`.
//!
//! Booleans run in i_overlay's integer core on our coordinates as they are (1 unit = 1 nm), so
//! no scaling and no rounding of the input; constructed intersection points are rounded to the
//! grid. Offsets run in its outline builder at a fixed scale of 1 (again the nm grid).
//!
//! Invariant: a `PolySet` is always normalised: no self-intersections or overlaps, outer rings
//! counter-clockwise (Y up) and holes clockwise, no repeated or collinear vertices, each ring
//! starting at its lowest-then-leftmost vertex, holes and polygons in sorted order. Equal
//! regions built the same way compare equal and hash the same.

use crate::geom::{orient, Angle, BBox, Nm, P};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::Overlay;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::mesh::outline::offset::OutlineOffset;
use i_overlay::mesh::style::{LineJoin, OutlineStyle};

/// One polygon: an outer ring and the holes in it.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Polygon {
    pub outer: Vec<P>,
    pub holes: Vec<Vec<P>>,
}

/// Disjoint polygons with holes (see the module invariant).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct PolySet {
    polys: Vec<Polygon>,
}

type IShape = Vec<Vec<IntPoint<i32>>>;

fn to_int(ring: &[P]) -> Vec<IntPoint<i32>> {
    ring.iter().map(|p| IntPoint::new(p.x, p.y)).collect()
}

fn from_int(ring: &[IntPoint<i32>]) -> Vec<P> {
    ring.iter().map(|p| P::new(p.x, p.y)).collect()
}

/// Twice the signed area of a ring (positive counter-clockwise), exact.
pub fn ring_area2(ring: &[P]) -> i128 {
    let n = ring.len();
    if n < 3 {
        return 0;
    }
    let mut a = 0i128;
    for i in 0..n {
        let p = ring[i];
        let q = ring[(i + 1) % n];
        a += p.x as i128 * q.y as i128 - q.x as i128 * p.y as i128;
    }
    a
}

/// Rotates a ring to start at its lowest-then-leftmost vertex.
fn rotate_to_min(ring: &mut [P]) {
    if let Some((i, _)) = ring.iter().enumerate().min_by_key(|(_, p)| (p.y, p.x)) {
        ring.rotate_left(i);
    }
}

/// Removes repeated and collinear vertices.
fn simplify_ring(ring: Vec<P>) -> Vec<P> {
    let mut pts: Vec<P> = Vec::with_capacity(ring.len());
    for p in ring {
        if pts.last() != Some(&p) {
            pts.push(p);
        }
    }
    while pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    let mut changed = true;
    while changed && pts.len() >= 3 {
        changed = false;
        let n = pts.len();
        let mut keep = Vec::with_capacity(n);
        for i in 0..n {
            let a = pts[(i + n - 1) % n];
            let b = pts[i];
            let c = pts[(i + 1) % n];
            if orient(a, b, c) != 0 {
                keep.push(b);
            } else {
                changed = true;
            }
        }
        pts = keep;
    }
    pts
}

impl PolySet {
    pub fn new() -> PolySet {
        PolySet::default()
    }

    /// From i_overlay's output (outer CCW, holes CW), normalised.
    fn from_shapes(shapes: Vec<IShape>) -> PolySet {
        let mut polys = Vec::with_capacity(shapes.len());
        for shape in shapes {
            let mut rings = shape.into_iter().map(|r| simplify_ring(from_int(&r))).filter(|r| r.len() >= 3);
            let Some(mut outer) = rings.next() else { continue };
            if ring_area2(&outer) < 0 {
                outer.reverse();
            }
            rotate_to_min(&mut outer);
            let mut holes: Vec<Vec<P>> = rings
                .map(|mut h| {
                    if ring_area2(&h) > 0 {
                        h.reverse();
                    }
                    rotate_to_min(&mut h);
                    h
                })
                .collect();
            holes.sort();
            polys.push(Polygon { outer, holes });
        }
        polys.sort_by(|a, b| a.outer.cmp(&b.outer));
        PolySet { polys }
    }

    fn shapes(&self) -> Vec<IShape> {
        self.polys
            .iter()
            .map(|p| {
                let mut s = Vec::with_capacity(1 + p.holes.len());
                s.push(to_int(&p.outer));
                s.extend(p.holes.iter().map(|h| to_int(h)));
                s
            })
            .collect()
    }

    fn op(a: &[IShape], b: &[IShape], rule: OverlayRule, fill: FillRule) -> PolySet {
        if a.is_empty() && b.is_empty() {
            return PolySet::new();
        }
        let out = Overlay::<i32>::with_shapes(a, b).overlay(rule, fill);
        PolySet::from_shapes(out)
    }

    /// The region inside a ring of any orientation or self-overlap, by the non-zero rule (a ring
    /// that winds round twice is still filled once).
    pub fn from_ring_nonzero(ring: Vec<P>) -> PolySet {
        if ring.len() < 3 {
            return PolySet::new();
        }
        let mut ring = ring;
        if ring_area2(&ring) < 0 {
            ring.reverse();
        }
        PolySet::op(&[vec![to_int(&ring)]], &[], OverlayRule::Subject, FillRule::NonZero)
    }

    /// The region of rings combined by the even-odd rule: nested rings alternate between
    /// filled and hole, whatever their orientation (how board outlines with cutouts and imported
    /// polygons are read).
    pub fn from_rings_even_odd(rings: &[Vec<P>]) -> PolySet {
        let shape: IShape = rings.iter().filter(|r| r.len() >= 3).map(|r| to_int(r)).collect();
        if shape.is_empty() {
            return PolySet::new();
        }
        PolySet::op(&[shape], &[], OverlayRule::Subject, FillRule::EvenOdd)
    }

    /// An axis-aligned rectangle.
    pub fn rect(b: BBox) -> PolySet {
        if b.is_empty() || b.width() == 0 || b.height() == 0 {
            return PolySet::new();
        }
        PolySet::from_ring_nonzero(vec![b.min, P::new(b.max.x, b.min.y), b.max, P::new(b.min.x, b.max.y)])
    }

    pub fn is_empty(&self) -> bool {
        self.polys.is_empty()
    }
    /// Each polygon as a set of its own (islands of a fill), in order.
    pub fn split(&self) -> Vec<PolySet> {
        self.polys.iter().map(|p| PolySet { polys: vec![p.clone()] }).collect()
    }
    pub fn polygons(&self) -> &[Polygon] {
        &self.polys
    }
    /// Every ring, outer rings and holes (holes clockwise).
    pub fn rings(&self) -> impl Iterator<Item = &[P]> {
        self.polys.iter().flat_map(|p| std::iter::once(p.outer.as_slice()).chain(p.holes.iter().map(|h| h.as_slice())))
    }
    pub fn vertex_count(&self) -> usize {
        self.rings().map(|r| r.len()).sum()
    }
    pub fn bbox(&self) -> BBox {
        BBox::of_points(self.polys.iter().flat_map(|p| p.outer.iter().copied()))
    }
    /// Area in nm^2.
    pub fn area(&self) -> f64 {
        self.rings().map(|r| ring_area2(r) as f64).sum::<f64>() / 2.0
    }
    /// True when `p` is inside or on the boundary.
    pub fn contains(&self, p: P) -> bool {
        self.polys.iter().any(|poly| {
            let bb = BBox::of_points(poly.outer.iter().copied());
            if !bb.contains(p) {
                return false;
            }
            match ring_winding(&poly.outer, p) {
                Winding::On => true,
                Winding::Out => false,
                Winding::In => poly.holes.iter().all(|h| ring_winding(h, p) != Winding::In),
            }
        })
    }
    /// `contains` for many points on the horizontal line `y` (a scan row): the edges whose closed
    /// y-range holds `y` are collected once, O(V); `RowContains::contains(x)` then visits only
    /// those, O(k) for the k edges on the line, and answers as `contains(P::new(x, y))` does.
    pub fn row(&self, y: Nm) -> RowContains {
        let on_row = |ring: &[P]| -> Vec<(P, P)> {
            ring_edges(ring).filter(|(a, b)| a.y.min(b.y) <= y && y <= a.y.max(b.y)).collect()
        };
        let polys = self
            .polys
            .iter()
            .filter_map(|poly| {
                let outer = on_row(&poly.outer);
                let holes = || poly.holes.iter().map(|h| on_row(h)).filter(|edges| !edges.is_empty()).collect();
                (!outer.is_empty()).then(|| RowPoly { outer, holes: holes() })
            })
            .collect();
        RowContains { y, polys }
    }

    pub fn union(&self, o: &PolySet) -> PolySet {
        PolySet::op(&self.shapes(), &o.shapes(), OverlayRule::Union, FillRule::NonZero)
    }
    pub fn difference(&self, o: &PolySet) -> PolySet {
        if self.is_empty() {
            return PolySet::new();
        }
        if o.is_empty() || !self.bbox().intersects(&o.bbox()) {
            return self.clone();
        }
        PolySet::op(&self.shapes(), &o.shapes(), OverlayRule::Difference, FillRule::NonZero)
    }
    pub fn intersection(&self, o: &PolySet) -> PolySet {
        if self.is_empty() || o.is_empty() || !self.bbox().intersects(&o.bbox()) {
            return PolySet::new();
        }
        PolySet::op(&self.shapes(), &o.shapes(), OverlayRule::Intersect, FillRule::NonZero)
    }
    pub fn xor(&self, o: &PolySet) -> PolySet {
        PolySet::op(&self.shapes(), &o.shapes(), OverlayRule::Xor, FillRule::NonZero)
    }
    /// The union of many sets in one pass (cheaper than folding `union`).
    pub fn union_all<'a>(sets: impl IntoIterator<Item = &'a PolySet>) -> PolySet {
        let subj: Vec<IShape> = sets.into_iter().flat_map(|s| s.shapes()).collect();
        if subj.is_empty() {
            return PolySet::new();
        }
        PolySet::op(&subj, &[], OverlayRule::Subject, FillRule::NonZero)
    }

    /// Grown by `delta` (shrunk when negative) with round joins. Join vertices lie on the true
    /// offset and the chords between them cut inside it by at most `max_err`: a caller that
    /// needs a result containing the exact offset grows by `delta + max_err`.
    pub fn offset(&self, delta: Nm, max_err: Nm) -> PolySet {
        if self.is_empty() || delta == 0 {
            return self.clone();
        }
        let shapes: Vec<Vec<Vec<[f64; 2]>>> =
            self.rings_by_polygon().map(|rings| rings.iter().map(|r| r.iter().map(|p| [p.x as f64, p.y as f64]).collect()).collect()).collect();
        let r = (delta as f64).abs();
        // i_overlay's round join steps by an angle (its "ratio" is arc length over radius) and
        // takes floor(turn / step) steps, so a step can come out up to twice the one asked for:
        // ask for half the angle whose chord has sagitta e (2 acos(1 - e / r)), and every chord
        // stays within e of the true arc (inscribed, so an offset never reaches past delta + 0
        // and never falls short of delta - e)
        let e = (max_err.max(1) as f64).min(r);
        let step = (1.0 - e / r).clamp(-1.0, 1.0).acos().clamp(1e-3, 0.25 * std::f64::consts::PI);
        let style = OutlineStyle::new(delta as f64).line_join(LineJoin::Round(step));
        let Ok(out) = shapes.outline_fixed_scale(&style, 1.0) else {
            // coordinates past the fixed-scale range (> 1 m): the floating adapter scales them
            return PolySet::from_float(shapes.outline(&style));
        };
        PolySet::from_float(out)
    }

    fn from_float(shapes: Vec<Vec<Vec<[f64; 2]>>>) -> PolySet {
        let ints: Vec<IShape> = shapes
            .into_iter()
            .map(|s| s.into_iter().map(|r| r.into_iter().map(|[x, y]| IntPoint::new(x.round() as i32, y.round() as i32)).collect()).collect())
            .collect();
        // rounding can touch neighbours: one union pass restores the invariant
        PolySet::op(&ints, &[], OverlayRule::Subject, FillRule::NonZero)
    }

    fn rings_by_polygon(&self) -> impl Iterator<Item = Vec<&[P]>> {
        self.polys.iter().map(|p| std::iter::once(p.outer.as_slice()).chain(p.holes.iter().map(|h| h.as_slice())).collect())
    }

    /// Every vertex mapped by `f`, then normalised again. `f` must keep orientation (a mirror
    /// goes through `mirrored_x`).
    fn mapped(&self, f: impl Fn(P) -> P, reverse: bool) -> PolySet {
        let shapes: Vec<IShape> = self
            .polys
            .iter()
            .map(|p| {
                std::iter::once(&p.outer)
                    .chain(p.holes.iter())
                    .map(|r| {
                        let mut v: Vec<IntPoint<i32>> = r.iter().map(|q| {
                            let m = f(*q);
                            IntPoint::new(m.x, m.y)
                        }).collect();
                        if reverse {
                            v.reverse();
                        }
                        v
                    })
                    .collect()
            })
            .collect();
        PolySet::op(&shapes, &[], OverlayRule::Subject, FillRule::NonZero)
    }

    /// Moved by `by`; exact, and the invariant needs no new pass.
    pub fn translated(&self, by: P) -> PolySet {
        let mut s = self.clone();
        for p in &mut s.polys {
            p.outer.iter_mut().for_each(|q| *q = q.add(by));
            p.holes.iter_mut().flatten().for_each(|q| *q = q.add(by));
        }
        s
    }
    pub fn rotated_about(&self, about: P, a: Angle) -> PolySet {
        if a == Angle::ZERO {
            return self.clone();
        }
        self.mapped(|p| p.rotated_about(about, a), false)
    }
    pub fn mirrored_x(&self, axis_x: Nm) -> PolySet {
        self.mapped(|p| p.mirrored_x(axis_x), true)
    }
}

/// A `PolySet`'s containment test on one scan row (`PolySet::row`).
#[derive(Clone, Debug)]
pub struct RowContains {
    y: Nm,
    polys: Vec<RowPoly>,
}

/// One polygon's edges on the row: its outer ring's and each hole's that has any.
#[derive(Clone, Debug)]
struct RowPoly {
    outer: Vec<(P, P)>,
    holes: Vec<Vec<(P, P)>>,
}

impl RowContains {
    /// True when `(x, y)` is inside or on the boundary, as `PolySet::contains` answers.
    pub fn contains(&self, x: Nm) -> bool {
        let p = P::new(x, self.y);
        self.polys.iter().any(|poly| match edges_winding(poly.outer.iter().copied(), p) {
            Winding::On => true,
            Winding::Out => false,
            Winding::In => poly.holes.iter().all(|h| edges_winding(h.iter().copied(), p) != Winding::In),
        })
    }
}

#[derive(PartialEq, Eq, Debug)]
pub(crate) enum Winding {
    In,
    Out,
    On,
}

fn ring_edges(ring: &[P]) -> impl Iterator<Item = (P, P)> + '_ {
    let n = ring.len();
    (0..n).map(move |i| (ring[i], ring[(i + 1) % n]))
}

/// Point in ring by crossing count, exact; a point on an edge is `On`.
fn ring_winding(ring: &[P], p: P) -> Winding {
    edges_winding(ring_edges(ring), p)
}

/// The crossing count over a ring's edges. Only edges whose closed y-range holds `p.y` can
/// touch `p` or cross its row, so a row's edges alone give the same answer.
pub(crate) fn edges_winding(edges: impl Iterator<Item = (P, P)>, p: P) -> Winding {
    let mut inside = false;
    for (a, b) in edges {
        if orient(a, b, p) == 0 && p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y) {
            return Winding::On;
        }
        if (a.y > p.y) != (b.y > p.y) {
            // the edge crosses the horizontal line through p: is the crossing right of p?
            let o = orient(a, b, p);
            if (o > 0) == (b.y > a.y) {
                inside = !inside;
            }
        }
    }
    if inside { Winding::In } else { Winding::Out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::mm;

    fn sq(x0: f64, y0: f64, s: f64) -> PolySet {
        PolySet::rect(BBox::new(P::mm(x0, y0), P::mm(x0 + s, y0 + s)))
    }

    /// Booleans, offsets, containment and normal form on two overlapping squares.
    #[test]
    fn booleans_offsets_and_normal_form() {
        let a = sq(0.0, 0.0, 10.0);
        let b = sq(5.0, 5.0, 10.0);
        let u = a.union(&b);
        assert_eq!(u.polygons().len(), 1, "union is one polygon");
        assert!((u.area() - 175e12).abs() < 1.0, "union area {}", u.area());
        let i = a.intersection(&b);
        assert_eq!(i, sq(5.0, 5.0, 5.0), "intersection is the overlap square, normalised identically");
        let d = a.difference(&b);
        assert!((d.area() - 75e12).abs() < 1.0, "difference area");
        let x = a.xor(&b);
        assert!((x.area() - 150e12).abs() < 1.0, "xor area");
        assert_eq!(a.union(&b), b.union(&a), "union commutes in normal form");
        // a hole: big minus small inside it
        let ring = sq(0.0, 0.0, 20.0).difference(&sq(5.0, 5.0, 5.0));
        assert_eq!(ring.polygons()[0].holes.len(), 1, "a hole");
        assert!(ring_area2(&ring.polygons()[0].holes[0]) < 0, "holes are clockwise");
        assert!(ring_area2(&ring.polygons()[0].outer) > 0, "outers are counter-clockwise");
        assert!(ring.contains(P::mm(1.0, 1.0)) && !ring.contains(P::mm(7.0, 7.0)), "containment honours holes");
        assert!(ring.contains(P::mm(0.0, 3.0)), "boundary counts as inside");
        // a scan row answers as `contains`, on rows through the hole, along its edges and corners
        let both = ring.union(&sq(30.0, 2.0, 4.0));
        for y in [0.0, 2.0, 3.0, 5.0, 7.5, 10.0, 20.0, 21.0] {
            let row = both.row(mm(y));
            for x in (-4..=140).map(|i| i as f64 * 0.25) {
                assert_eq!(row.contains(mm(x)), both.contains(P::mm(x, y)), "row and point tests agree at ({x}, {y})");
            }
        }
        // even-odd nesting (an outline with a cutout) gives the same region
        let eo = PolySet::from_rings_even_odd(&[
            sq(0.0, 0.0, 20.0).polygons()[0].outer.clone(),
            sq(5.0, 5.0, 5.0).polygons()[0].outer.clone(),
        ]);
        assert_eq!(eo, ring, "even-odd rings equal the difference");
        // offsets: grow by 1 mm with round corners, area 100 + 40 + pi, within the chord error
        let g = a.offset(mm(1.0), mm(0.001));
        let exact = (100.0 + 40.0 + std::f64::consts::PI) * 1e12;
        assert!(g.area() <= exact && g.area() > exact - 0.01e12, "grown area {} vs {}", g.area(), exact);
        // every vertex of the grown outline is at the offset and every chord within the error
        // (the round join's step rounding once let a chord sag past it)
        let ring = &g.polygons()[0].outer;
        let to_square = |p: P| {
            let dx = (0 - p.x).max(p.x - mm(10.0)).max(0) as f64;
            let dy = (0 - p.y).max(p.y - mm(10.0)).max(0) as f64;
            (dx * dx + dy * dy).sqrt()
        };
        for (k, p) in ring.iter().enumerate() {
            let q = ring[(k + 1) % ring.len()];
            let mid = P::new(((p.x as i64 + q.x as i64) / 2) as Nm, ((p.y as i64 + q.y as i64) / 2) as Nm);
            assert!((to_square(*p) - 1e6).abs() <= 1.0, "a grown vertex lies at the offset: {}", to_square(*p));
            assert!(to_square(mid) >= 1e6 - 1e3 - 1.0, "a chord stays within the error of the offset: {}", to_square(mid));
        }
        let s = a.offset(-mm(1.0), mm(0.001));
        assert_eq!(s, sq(1.0, 1.0, 8.0), "shrinking a square keeps it square");
        assert!(a.offset(-mm(6.0), mm(0.001)).is_empty(), "shrinking past half the width empties it");
        // transforms
        assert_eq!(a.translated(P::mm(5.0, 5.0)).intersection(&b), sq(5.0, 5.0, 10.0), "translate");
        let m = a.mirrored_x(0);
        assert_eq!(m, PolySet::rect(BBox::new(P::mm(-10.0, 0.0), P::mm(0.0, 10.0))), "mirror keeps orientation valid");
        let r = a.rotated_about(P::ZERO, Angle::QUARTER);
        assert_eq!(r, PolySet::rect(BBox::new(P::mm(-10.0, 0.0), P::mm(0.0, 10.0))), "quarter turn");
        assert_eq!(PolySet::union_all([&a, &b, &i]), u, "union of many");
    }
}
