//! Integer-grid 2D geometry: points, rings, regions and polylines.
//!
//! One unit is one nanometre (`SCALE` units per millimetre). Every
//! predicate (orientation, containment, intersection) is exact, done in
//! `i128`; only constructed points (intersections, offsets) are rounded to
//! the grid.

pub const SCALE: f64 = 1_000_000.0;

#[inline]
pub fn scaled(mm: f64) -> i64 {
    (mm * SCALE).round() as i64
}

#[inline]
pub fn unscaled(v: i64) -> f64 {
    v as f64 / SCALE
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct Pt {
    pub x: i64,
    pub y: i64,
}

impl Pt {
    #[inline]
    pub const fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }
    #[inline]
    pub fn from_mm(x: f64, y: f64) -> Self {
        Self::new(scaled(x), scaled(y))
    }
    #[inline]
    pub fn x_mm(self) -> f64 {
        unscaled(self.x)
    }
    #[inline]
    pub fn y_mm(self) -> f64 {
        unscaled(self.y)
    }
    #[inline]
    pub fn sub(self, o: Pt) -> Pt {
        Pt::new(self.x - o.x, self.y - o.y)
    }
    #[inline]
    pub fn add(self, o: Pt) -> Pt {
        Pt::new(self.x + o.x, self.y + o.y)
    }
    #[inline]
    pub fn dist(self, o: Pt) -> f64 {
        let dx = (self.x - o.x) as f64;
        let dy = (self.y - o.y) as f64;
        (dx * dx + dy * dy).sqrt()
    }
    #[inline]
    pub fn dist2(self, o: Pt) -> i128 {
        let dx = (self.x - o.x) as i128;
        let dy = (self.y - o.y) as i128;
        dx * dx + dy * dy
    }
    /// Rotate about the origin.
    pub fn rotated(self, cos: f64, sin: f64) -> Pt {
        let x = self.x as f64;
        let y = self.y as f64;
        Pt::new((x * cos - y * sin).round() as i64, (x * sin + y * cos).round() as i64)
    }
}

/// `cross(b - a, c - a)`: positive when a→b→c turns left.
#[inline]
pub fn orient(a: Pt, b: Pt, c: Pt) -> i128 {
    let abx = (b.x - a.x) as i128;
    let aby = (b.y - a.y) as i128;
    let acx = (c.x - a.x) as i128;
    let acy = (c.y - a.y) as i128;
    abx * acy - aby * acx
}

#[inline]
pub fn cross(a: Pt, b: Pt) -> i128 {
    a.x as i128 * b.y as i128 - a.y as i128 * b.x as i128
}

#[inline]
pub fn dot(a: Pt, b: Pt) -> i128 {
    a.x as i128 * b.x as i128 + a.y as i128 * b.y as i128
}

/// Counter-clockwise hull, without collinear or closing points.
pub fn convex_hull(pts: &mut Vec<Pt>) -> Vec<Pt> {
    pts.sort_unstable();
    pts.dedup();
    if pts.len() < 3 { return pts.clone(); }
    let mut hull = Vec::with_capacity(pts.len() * 2);
    for &p in pts.iter() {
        while hull.len() >= 2 && orient(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0 { hull.pop(); }
        hull.push(p);
    }
    let lower = hull.len();
    for &p in pts.iter().rev().skip(1) {
        while hull.len() > lower && orient(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0 { hull.pop(); }
        hull.push(p);
    }
    hull.pop();
    hull
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BBox {
    pub min: Pt,
    pub max: Pt,
}

impl BBox {
    pub fn empty() -> Self {
        Self {
            min: Pt::new(i64::MAX, i64::MAX),
            max: Pt::new(i64::MIN, i64::MIN),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }
    pub fn add(&mut self, p: Pt) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }
    pub fn merge(&mut self, o: &BBox) {
        if !o.is_empty() {
            self.add(o.min);
            self.add(o.max);
        }
    }
    pub fn of(pts: &[Pt]) -> Self {
        let mut b = Self::empty();
        for p in pts {
            b.add(*p);
        }
        b
    }
    pub fn overlaps(&self, o: &BBox) -> bool {
        self.min.x <= o.max.x && o.min.x <= self.max.x && self.min.y <= o.max.y && o.min.y <= self.max.y
    }
    pub fn center(&self) -> Pt {
        Pt::new((self.min.x + self.max.x) / 2, (self.min.y + self.max.y) / 2)
    }
}

/// A closed ring; the closing edge from the last point back to the first is
/// implicit. Outer contours run counter-clockwise, holes clockwise.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polygon {
    pub pts: Vec<Pt>,
}

impl Polygon {
    pub fn new(pts: Vec<Pt>) -> Self {
        Self { pts }
    }
    pub fn rect(min: Pt, max: Pt) -> Self {
        Self::new(vec![min, Pt::new(max.x, min.y), max, Pt::new(min.x, max.y)])
    }
    pub fn len(&self) -> usize {
        self.pts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pts.is_empty()
    }
    /// Twice the signed area, exact.
    pub fn area2(&self) -> i128 {
        let n = self.pts.len();
        let mut a = 0i128;
        for i in 0..n {
            a += cross(self.pts[i], self.pts[(i + 1) % n]);
        }
        a
    }
    /// Signed area in mm².
    pub fn area_mm2(&self) -> f64 {
        self.area2() as f64 * 0.5 / (SCALE * SCALE)
    }
    pub fn is_ccw(&self) -> bool {
        self.area2() > 0
    }
    pub fn reverse(&mut self) {
        self.pts.reverse();
    }
    pub fn bbox(&self) -> BBox {
        BBox::of(&self.pts)
    }
    pub fn perimeter(&self) -> f64 {
        let n = self.pts.len();
        (0..n).map(|i| self.pts[i].dist(self.pts[(i + 1) % n])).sum()
    }
    /// Winding number of `p` (points on the boundary count by the half-open
    /// rule, consistently with neighbouring rings).
    pub fn winding(&self, p: Pt) -> i32 {
        winding_of_ring(&self.pts, p)
    }
    pub fn contains(&self, p: Pt) -> bool {
        self.winding(p) != 0
    }
    /// The ring as an open path that starts and ends at point `i`.
    pub fn split_at(&self, i: usize) -> Polyline {
        let n = self.pts.len();
        let mut pts = Vec::with_capacity(n + 1);
        for k in 0..=n {
            pts.push(self.pts[(i + k) % n]);
        }
        Polyline { pts }
    }
}

pub fn winding_of_ring(pts: &[Pt], p: Pt) -> i32 {
    let n = pts.len();
    let mut w = 0;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        if a.y <= p.y {
            if b.y > p.y && orient(a, b, p) > 0 {
                w += 1;
            }
        } else if b.y <= p.y && orient(a, b, p) < 0 {
            w -= 1;
        }
    }
    w
}

/// A region: one counter-clockwise contour and its clockwise holes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExPolygon {
    pub contour: Polygon,
    pub holes: Vec<Polygon>,
}

impl ExPolygon {
    pub fn new(contour: Polygon) -> Self {
        Self { contour, holes: Vec::new() }
    }
    pub fn area_mm2(&self) -> f64 {
        self.contour.area_mm2() + self.holes.iter().map(|h| h.area_mm2()).sum::<f64>()
    }
    pub fn area2(&self) -> i128 {
        self.contour.area2() + self.holes.iter().map(|h| h.area2()).sum::<i128>()
    }
    pub fn bbox(&self) -> BBox {
        self.contour.bbox()
    }
    pub fn contains(&self, p: Pt) -> bool {
        self.contour.winding(p) + self.holes.iter().map(|h| h.winding(p)).sum::<i32>() != 0
    }
    pub fn rings(&self) -> impl Iterator<Item = &Polygon> {
        std::iter::once(&self.contour).chain(self.holes.iter())
    }
    pub fn translate(&mut self, d: Pt) {
        for r in std::iter::once(&mut self.contour).chain(self.holes.iter_mut()) {
            for p in &mut r.pts {
                *p = p.add(d);
            }
        }
    }
}

pub fn to_polygons(ex: &[ExPolygon]) -> Vec<Polygon> {
    let mut out = Vec::new();
    for e in ex {
        out.push(e.contour.clone());
        out.extend(e.holes.iter().cloned());
    }
    out
}

pub fn total_area_mm2(ex: &[ExPolygon]) -> f64 {
    ex.iter().map(|e| e.area_mm2()).sum()
}

/// Regions without pinches: a ring that passes a vertex twice (a zero-width
/// neck, as a traced zero set, a union or a mesh section can make) is split
/// there into rings that do not. A piece turning the ring's way stays what the
/// ring was; a piece turning the other way is the other kind (a contour's lobe
/// turned back is a hole in it, a hole's is an island in the hole). Pieces of
/// zero area go, and a hole goes into the smallest contour around it.
/// Regions with no pinch come back as they were.
pub fn split_pinches(regions: Vec<ExPolygon>) -> Vec<ExPolygon> {
    // the vertices a ring passes more than once
    let repeats = |r: &Polygon| {
        let mut pts = r.pts.clone();
        pts.sort_unstable();
        let mut twice: Vec<Pt> = pts.windows(2).filter(|w| w[0] == w[1]).map(|w| w[0]).collect();
        twice.dedup();
        twice
    };
    let pinched = |e: &ExPolygon| e.rings().any(|r| !repeats(r).is_empty());
    if !regions.iter().any(pinched) {
        return regions;
    }
    let mut out = Vec::with_capacity(regions.len());
    for e in regions {
        let marks: Vec<Vec<Pt>> = e.rings().map(repeats).collect();
        if marks.iter().all(|m| m.is_empty()) {
            out.push(e);
            continue;
        }
        let (mut contours, mut holes) = (Vec::new(), Vec::new());
        for (ring, marks) in e.rings().zip(&marks) {
            for piece in split_ring_at(&ring.pts, marks) {
                match piece.area2() {
                    a if a > 0 => contours.push(ExPolygon::new(piece)),
                    a if a < 0 => holes.push(piece),
                    _ => {}
                }
            }
        }
        for h in holes {
            // A point inside the hole's first edge: on no other ring (they meet only at vertices).
            let (a, b) = (h.pts[0], h.pts[1]);
            let probe = Pt::new((a.x + b.x) / 2, (a.y + b.y) / 2);
            if let Some(c) = contours.iter_mut().filter(|c| c.contour.contains(probe)).min_by_key(|c| c.contour.area2()) {
                c.holes.push(h);
            }
        }
        out.extend(contours);
    }
    out
}

/// A ring cut at each of the `marks` vertices it passes again: each closed lobe in the order
/// it closes (starting at its vertex), then what remains. Only a marked vertex closes a lobe;
/// a lobe of fewer than three points is a doubled vertex or a spike going out and back.
pub fn split_ring_at(pts: &[Pt], marks: &[Pt]) -> Vec<Polygon> {
    let mut marks = marks.to_vec();
    marks.sort_unstable();
    marks.dedup();
    let mut pieces = Vec::new();
    let mut stack: Vec<Pt> = Vec::with_capacity(pts.len());
    // the marked vertices on the stack and their positions
    let mut at: Vec<(Pt, usize)> = Vec::new();
    for &p in pts {
        if marks.binary_search(&p).is_err() {
            stack.push(p);
            continue;
        }
        if let Some(&(_, i)) = at.iter().find(|m| m.0 == p) {
            let mut lobe = vec![p];
            lobe.extend(stack.drain(i + 1..));
            at.retain(|m| m.1 <= i);
            pieces.push(Polygon::new(lobe));
        } else {
            at.push((p, stack.len()));
            stack.push(p);
        }
    }
    pieces.push(Polygon::new(stack));
    pieces
}

pub fn bbox_of(ex: &[ExPolygon]) -> BBox {
    let mut b = BBox::empty();
    for e in ex {
        b.merge(&e.bbox());
    }
    b
}

/// An open path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polyline {
    pub pts: Vec<Pt>,
}

impl Polyline {
    pub fn new(pts: Vec<Pt>) -> Self {
        Self { pts }
    }
    pub fn length(&self) -> f64 {
        self.pts.windows(2).map(|w| w[0].dist(w[1])).sum()
    }
    pub fn first(&self) -> Pt {
        self.pts[0]
    }
    pub fn last(&self) -> Pt {
        *self.pts.last().unwrap()
    }
    pub fn reverse(&mut self) {
        self.pts.reverse();
    }
    /// Shorten the end by `d` (units).
    pub fn clip_end(&mut self, mut d: f64) {
        while d > 0.0 && self.pts.len() >= 2 {
            let n = self.pts.len();
            let a = self.pts[n - 2];
            let b = self.pts[n - 1];
            let l = a.dist(b);
            if l > d {
                let t = (l - d) / l;
                self.pts[n - 1] = lerp(a, b, t);
                return;
            }
            d -= l;
            self.pts.pop();
        }
    }
}

#[inline]
pub fn lerp(a: Pt, b: Pt, t: f64) -> Pt {
    Pt::new(
        (a.x as f64 + (b.x - a.x) as f64 * t).round() as i64,
        (a.y as f64 + (b.y - a.y) as f64 * t).round() as i64,
    )
}

/// Remove consecutive duplicates and collinear interior vertices of a ring.
pub fn clean_ring(pts: &mut Vec<Pt>) {
    pts.dedup();
    while pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    // Each pass decides on the ring as it was before the pass, then
    // compacts it in place.
    let mut drop: Vec<bool> = Vec::new();
    while pts.len() >= 3 {
        let n = pts.len();
        drop.clear();
        drop.extend((0..n).map(|i| {
            let a = pts[(i + n - 1) % n];
            let b = pts[i];
            let c = pts[(i + 1) % n];
            orient(a, b, c) == 0 && dot(b.sub(a), c.sub(b)) >= 0
        }));
        let kept = drop.iter().filter(|d| !**d).count();
        if kept == n {
            break;
        }
        if kept < 3 {
            pts.clear();
            return;
        }
        let mut w = 0;
        for i in 0..n {
            if !drop[i] {
                pts[w] = pts[i];
                w += 1;
            }
        }
        pts.truncate(w);
    }
}

/// Douglas-Peucker simplification of a ring at `tol` units; keeps at least
/// a triangle.
pub fn simplify_ring(pts: &[Pt], tol: f64) -> Vec<Pt> {
    let n = pts.len();
    if n < 4 || tol <= 0.0 {
        return pts.to_vec();
    }
    // Split at the point farthest from point 0 so both halves are open.
    let mut far = 0;
    let mut best = 0i128;
    for i in 1..n {
        let d = pts[0].dist2(pts[i]);
        if d > best {
            best = d;
            far = i;
        }
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[far] = true;
    dp_mark(pts, 0, far, tol, &mut keep);
    let mut tail: Vec<Pt> = pts[far..].to_vec();
    tail.push(pts[0]);
    let mut keep_tail = vec![false; tail.len()];
    let tl = tail.len();
    dp_mark(&tail, 0, tl - 1, tol, &mut keep_tail);
    for (k, kk) in keep_tail.iter().enumerate().take(tl - 1).skip(1) {
        if *kk {
            keep[far + k] = true;
        }
    }
    let out: Vec<Pt> = pts.iter().zip(keep.iter()).filter(|(_, k)| **k).map(|(p, _)| *p).collect();
    if out.len() < 3 {
        pts.to_vec()
    } else {
        out
    }
}

pub fn simplify_polyline(pts: &[Pt], tol: f64) -> Vec<Pt> {
    let n = pts.len();
    if n < 3 || tol <= 0.0 {
        return pts.to_vec();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    dp_mark(pts, 0, n - 1, tol, &mut keep);
    pts.iter().zip(keep.iter()).filter(|(_, k)| **k).map(|(p, _)| *p).collect()
}

fn dp_mark(pts: &[Pt], i0: usize, i1: usize, tol: f64, keep: &mut [bool]) {
    let mut stack = vec![(i0, i1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let pa = pts[a];
        let pb = pts[b];
        let len = pa.dist(pb);
        let mut best = -1.0;
        let mut bi = a;
        for (i, p) in pts.iter().enumerate().take(b).skip(a + 1) {
            let d = if len == 0.0 {
                pa.dist(*p)
            } else {
                (orient(pa, pb, *p) as f64).abs() / len
            };
            if d > best {
                best = d;
                bi = i;
            }
        }
        if best > tol {
            keep[bi] = true;
            stack.push((a, bi));
            stack.push((bi, b));
        }
    }
}

/// Whether segments `ab` and `cd` cross or come within `within` units of each other.
pub fn segments_within(a: Pt, b: Pt, c: Pt, d: Pt, within: f64) -> bool {
    let side = |o: i128| o.signum();
    let crossing = side(orient(a, b, c)) * side(orient(a, b, d)) < 0 && side(orient(c, d, a)) * side(orient(c, d, b)) < 0;
    crossing || dist_point_segment(a, c, d) <= within || dist_point_segment(b, c, d) <= within || dist_point_segment(c, a, b) <= within || dist_point_segment(d, a, b) <= within
}

/// Distance from `p` to segment `ab` in units.
pub fn dist_point_segment(p: Pt, a: Pt, b: Pt) -> f64 {
    let ab = b.sub(a);
    let ap = p.sub(a);
    // Differences within ±2^31: the dot products fit in i64, and i64 → f64
    // rounds exactly as i128 → f64 does.
    const LIM: i64 = 1 << 31;
    let t = if ab.x.abs() < LIM && ab.y.abs() < LIM && ap.x.abs() < LIM && ap.y.abs() < LIM {
        let l2 = ab.x * ab.x + ab.y * ab.y;
        if l2 == 0 {
            return p.dist(a);
        }
        (ap.x * ab.x + ap.y * ab.y) as f64 / l2 as f64
    } else {
        let l2 = dot(ab, ab);
        if l2 == 0 {
            return p.dist(a);
        }
        dot(ap, ab) as f64 / l2 as f64
    };
    if t <= 0.0 {
        p.dist(a)
    } else if t >= 1.0 {
        p.dist(b)
    } else {
        let q = lerp(a, b, t);
        p.dist(q)
    }
}
