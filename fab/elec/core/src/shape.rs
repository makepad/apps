//! The shapes all copper, mask, paste, silk and courtyard geometry is made of.

use crate::geom::{arc_bbox, ArcGeom, Angle, BBox, Nm, P};
use crate::poly::PolySet;

/// A filled shape on one layer.
///
/// Widths and radii are full widths (`Segment::w` is the track width), never half widths.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Shape {
    /// A disc.
    Circle { c: P, r: Nm },
    /// A straight track: the segment `a`-`b` with round ends, `w` wide.
    Segment { a: P, b: P, w: Nm },
    /// An arc track through `a`, `mid`, `b` with round ends, `w` wide (collinear points: a
    /// straight segment).
    Arc { a: P, mid: P, b: P, w: Nm },
    /// A rectangle centred at `c` with half sizes `half`, rotated by `rot` about `c`, corners
    /// rounded with radius `corner` (0 sharp, at most the smaller half size).
    Rect { c: P, half: P, rot: Angle, corner: Nm },
    /// A stadium (oblong pad): centred at `c`, half sizes `half`, rotated by `rot`; the shorter
    /// side is fully round.
    Oval { c: P, half: P, rot: Angle },
    /// Polygons with holes, integer vertices.
    Poly(PolySet),
}

/// A shape as a core and a radius: the shape is every point within `radius` of the core.
/// Distances between shapes are distances between cores minus both radii.
#[derive(Clone, Debug)]
pub enum Core<'a> {
    Point(P),
    Seg(P, P),
    /// The arc through `a`, `mid`, `b` (never collinear; those become `Seg`).
    Arc(P, P, P),
    /// A convex polygon, counter-clockwise, 3 or more vertices (a sharp or rounded rectangle's
    /// inner rectangle).
    Convex(Vec<P>),
    /// A polygon set, as filled regions.
    Set(&'a PolySet),
}

impl Shape {
    /// The shape as core plus radius (f64 nm: half an odd width is a half nm).
    pub fn core(&self) -> (Core<'_>, f64) {
        match self {
            Shape::Circle { c, r } => (Core::Point(*c), *r as f64),
            Shape::Segment { a, b, w } => (if a == b { Core::Point(*a) } else { Core::Seg(*a, *b) }, *w as f64 / 2.0),
            Shape::Arc { a, mid, b, w } => {
                let core = if ArcGeom::new(*a, *mid, *b).is_some() { Core::Arc(*a, *mid, *b) } else if a == b { Core::Point(*a) } else { Core::Seg(*a, *b) };
                (core, *w as f64 / 2.0)
            }
            Shape::Rect { c, half, rot, corner } => {
                let r = (*corner).max(0).min(half.x.min(half.y));
                let (hx, hy) = (half.x - r, half.y - r);
                let core = rect_core(*c, hx, hy, *rot);
                (core, r as f64)
            }
            Shape::Oval { c, half, rot } => {
                let r = half.x.min(half.y);
                (rect_core(*c, half.x - r, half.y - r, *rot), r as f64)
            }
            Shape::Poly(s) => (Core::Set(s), 0.0),
        }
    }

    /// The box of every point of the shape.
    pub fn bbox(&self) -> BBox {
        let up = |r: f64| r.ceil() as Nm;
        match self {
            Shape::Circle { c, r } => BBox::point(*c).inflate(*r),
            Shape::Segment { a, b, w } => BBox::new(*a, *b).inflate(up(*w as f64 / 2.0)),
            Shape::Arc { a, mid, b, w } => arc_bbox(*a, *mid, *b).inflate(up(*w as f64 / 2.0)),
            Shape::Rect { .. } | Shape::Oval { .. } => {
                let (core, r) = self.core();
                core_bbox(&core).inflate(up(r))
            }
            Shape::Poly(s) => s.bbox(),
        }
    }

    pub fn translated(&self, by: P) -> Shape {
        match self {
            Shape::Circle { c, r } => Shape::Circle { c: c.add(by), r: *r },
            Shape::Segment { a, b, w } => Shape::Segment { a: a.add(by), b: b.add(by), w: *w },
            Shape::Arc { a, mid, b, w } => Shape::Arc { a: a.add(by), mid: mid.add(by), b: b.add(by), w: *w },
            Shape::Rect { c, half, rot, corner } => Shape::Rect { c: c.add(by), half: *half, rot: *rot, corner: *corner },
            Shape::Oval { c, half, rot } => Shape::Oval { c: c.add(by), half: *half, rot: *rot },
            Shape::Poly(s) => Shape::Poly(s.translated(by)),
        }
    }

    /// Rotated counter-clockwise by `a` about `about`.
    pub fn rotated_about(&self, about: P, a: Angle) -> Shape {
        let f = |p: P| p.rotated_about(about, a);
        match self {
            Shape::Circle { c, r } => Shape::Circle { c: f(*c), r: *r },
            Shape::Segment { a: p, b, w } => Shape::Segment { a: f(*p), b: f(*b), w: *w },
            Shape::Arc { a: p, mid, b, w } => Shape::Arc { a: f(*p), mid: f(*mid), b: f(*b), w: *w },
            Shape::Rect { c, half, rot, corner } => Shape::Rect { c: f(*c), half: *half, rot: rot.add(a), corner: *corner },
            Shape::Oval { c, half, rot } => Shape::Oval { c: f(*c), half: *half, rot: rot.add(a) },
            Shape::Poly(s) => Shape::Poly(s.rotated_about(about, a)),
        }
    }

    /// Mirrored across the vertical line `x = axis_x` (a part placed on the bottom side).
    pub fn mirrored_x(&self, axis_x: Nm) -> Shape {
        let f = |p: P| p.mirrored_x(axis_x);
        match self {
            Shape::Circle { c, r } => Shape::Circle { c: f(*c), r: *r },
            Shape::Segment { a, b, w } => Shape::Segment { a: f(*a), b: f(*b), w: *w },
            Shape::Arc { a, mid, b, w } => Shape::Arc { a: f(*a), mid: f(*mid), b: f(*b), w: *w },
            Shape::Rect { c, half, rot, corner } => Shape::Rect { c: f(*c), half: *half, rot: rot.neg(), corner: *corner },
            Shape::Oval { c, half, rot } => Shape::Oval { c: f(*c), half: *half, rot: rot.neg() },
            Shape::Poly(s) => Shape::Poly(s.mirrored_x(axis_x)),
        }
    }

    /// The shape as polygons that CONTAIN it: curved edges are replaced by edges outside the
    /// curve, at most `max_err` (+1 nm of rounding) away. Containment is what knock-outs (zone
    /// fill, mask openings) need to stay legal.
    pub fn to_polyset(&self, max_err: Nm) -> PolySet {
        let (core, r) = self.core();
        let r = if r > 0.0 { r + 1.0 } else { 0.0 };
        match core {
            Core::Set(s) => s.clone(),
            Core::Point(c) if r > 0.0 => PolySet::from_ring_nonzero(circle_ring(c, r, max_err)),
            Core::Point(_) => PolySet::new(),
            Core::Seg(a, b) => PolySet::from_ring_nonzero(convex_offset(&[a, b], r, max_err)),
            Core::Convex(pts) => PolySet::from_ring_nonzero(convex_offset(&pts, r, max_err)),
            Core::Arc(a, mid, b) => PolySet::from_ring_nonzero(arc_track_ring(a, mid, b, r, max_err)),
        }
    }
}

fn rect_core(c: P, hx: Nm, hy: Nm, rot: Angle) -> Core<'static> {
    if hx == 0 && hy == 0 {
        return Core::Point(c);
    }
    let f = |x: Nm, y: Nm| P::new(x, y).rotated(rot).add(c);
    if hy == 0 {
        return Core::Seg(f(-hx, 0), f(hx, 0));
    }
    if hx == 0 {
        return Core::Seg(f(0, -hy), f(0, hy));
    }
    Core::Convex(vec![f(-hx, -hy), f(hx, -hy), f(hx, hy), f(-hx, hy)])
}

/// The box of a core.
pub fn core_bbox(core: &Core<'_>) -> BBox {
    match core {
        Core::Point(p) => BBox::point(*p),
        Core::Seg(a, b) => BBox::new(*a, *b),
        Core::Arc(a, m, b) => arc_bbox(*a, *m, *b),
        Core::Convex(pts) => BBox::of_points(pts.iter().copied()),
        Core::Set(s) => s.bbox(),
    }
}

/// Angle step for a circumscribed polygon of radius `r` whose edges stay within `max_err` of
/// the circle: `r / cos(step / 2) - r <= max_err`.
fn circumscribed_step(r: f64, max_err: Nm) -> f64 {
    let e = max_err.max(1) as f64;
    let step = 2.0 * (r / (r + e)).clamp(-1.0, 1.0).acos();
    step.clamp(1e-3, std::f64::consts::FRAC_PI_2)
}

/// Pushes a circumscribed arc about `c` from angle `t0` sweeping `sweep` (> 0) radians: the
/// tangent points at both ends lie on the circle, the vertices between lie outside it.
fn push_arc(c: (f64, f64), r: f64, t0: f64, sweep: f64, max_err: Nm, out: &mut Vec<P>) {
    let at = |t: f64, rr: f64| P::from_f(c.0 + rr * t.cos(), c.1 + rr * t.sin());
    out.push(at(t0, r));
    if sweep <= 0.0 {
        return;
    }
    let n = (sweep / circumscribed_step(r, max_err)).ceil().max(1.0) as usize;
    let step = sweep / n as f64;
    let rr = r / (step / 2.0).cos();
    for k in 0..n {
        out.push(at(t0 + (k as f64 + 0.5) * step, rr));
    }
    out.push(at(t0 + sweep, r));
}

fn circle_ring(c: P, r: f64, max_err: Nm) -> Vec<P> {
    let n = (std::f64::consts::TAU / circumscribed_step(r, max_err)).ceil().max(4.0) as usize;
    let step = std::f64::consts::TAU / n as f64;
    let rr = r / (step / 2.0).cos();
    (0..n).map(|k| P::from_f(c.x as f64 + rr * (k as f64 * step).cos(), c.y as f64 + rr * (k as f64 * step).sin())).collect()
}

/// A convex polygon (counter-clockwise; two points: a segment) grown by `r` with round corners.
fn convex_offset(pts: &[P], r: f64, max_err: Nm) -> Vec<P> {
    let n = pts.len();
    let mut out = Vec::new();
    let normal = |a: P, b: P| {
        let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
        // outward of a counter-clockwise polygon: the edge direction turned clockwise
        dy.atan2(dx) - std::f64::consts::FRAC_PI_2
    };
    for i in 0..n {
        let prev = pts[(i + n - 1) % n];
        let cur = pts[i];
        let next = pts[(i + 1) % n];
        let t0 = normal(prev, cur);
        let t1 = normal(cur, next);
        let sweep = (t1 - t0).rem_euclid(std::f64::consts::TAU);
        // a segment's two "edges" are the same line both ways: each end turns half a circle
        let sweep = if n == 2 { std::f64::consts::PI } else { sweep };
        push_arc(cur.f(), r, t0, sweep, max_err, &mut out);
    }
    out
}

/// The ring of an arc track: outer arc, the end cap, inner arc back, the start cap.
fn arc_track_ring(a: P, mid: P, b: P, r: f64, max_err: Nm) -> Vec<P> {
    let g = ArcGeom::new(a, mid, b).expect("arc core is never collinear");
    // walk counter-clockwise: swap the ends of a clockwise arc
    let (start, sweep) = if g.sweep >= 0.0 { (g.start, g.sweep) } else { (g.start + g.sweep, -g.sweep) };
    let c = g.center;
    let end_pt = |t: f64| (c.0 + g.radius * t.cos(), c.1 + g.radius * t.sin());
    let mut out = Vec::new();
    let pi = std::f64::consts::PI;
    // outer arc
    push_arc(c, g.radius + r, start, sweep, max_err, &mut out);
    // end cap: half a circle about the end, from outward to inward
    push_arc(end_pt(start + sweep), r, start + sweep, pi, max_err, &mut out);
    // inner arc, backwards, chords inside the inner edge (vertices on it): the ring still holds
    // the track
    let ri = g.radius - r;
    if ri > 0.0 {
        let n = g.chords_for(ri, max_err);
        for k in 0..=n {
            let t = start + sweep - sweep * k as f64 / n as f64;
            out.push(P::from_f(c.0 + ri * t.cos(), c.1 + ri * t.sin()));
        }
    } else {
        out.push(P::from_f(c.0, c.1));
    }
    // start cap: from inward to outward
    push_arc(end_pt(start), r, start + pi, pi, max_err, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::mm;

    /// Boxes, transforms and containing polygons of one of each shape.
    #[test]
    fn boxes_transforms_and_polygons() {
        let shapes = [
            Shape::Circle { c: P::mm(1.0, 1.0), r: mm(0.3) },
            Shape::Segment { a: P::mm(0.0, 0.0), b: P::mm(5.0, 0.0), w: mm(0.25) },
            Shape::Arc { a: P::mm(10.0, 0.0), mid: P::mm(7.0710678, 7.0710678), b: P::mm(0.0, 10.0), w: mm(0.2) },
            Shape::Rect { c: P::mm(2.0, 3.0), half: P::mm(0.6, 0.4), rot: Angle::deg(30.0), corner: mm(0.1) },
            Shape::Oval { c: P::mm(-2.0, 1.0), half: P::mm(1.0, 0.5), rot: Angle::QUARTER },
        ];
        for s in &shapes {
            let bb = s.bbox();
            let poly = s.to_polyset(mm(0.002));
            let pb = poly.bbox();
            assert!(!poly.is_empty(), "{s:?} polygonises");
            // the containing polygon reaches the box and stays within max_err + rounding of it
            assert!(pb.min.x <= bb.min.x + 2 && pb.max.x >= bb.max.x - 2, "{s:?}: polygon spans the box in x ({pb:?} vs {bb:?})");
            let slack = mm(0.002) + 3;
            assert!(pb.min.x >= bb.min.x - slack && pb.min.y >= bb.min.y - slack && pb.max.x <= bb.max.x + slack && pb.max.y <= bb.max.y + slack, "{s:?}: polygon within max_err of the box ({pb:?} vs {bb:?})");
            let t = s.translated(P::mm(1.0, 2.0)).translated(P::mm(-1.0, -2.0));
            assert_eq!(&t, s, "{s:?}: translation round trip");
            let m = s.mirrored_x(mm(3.0)).mirrored_x(mm(3.0));
            assert_eq!(&m, s, "{s:?}: mirror twice is identity");
        }
        // a rotated rectangle's box: a 90deg turn swaps its sides exactly
        let r = Shape::Rect { c: P::ZERO, half: P::mm(2.0, 1.0), rot: Angle::ZERO, corner: 0 };
        let rb = r.rotated_about(P::ZERO, Angle::QUARTER).bbox();
        assert_eq!((rb.min, rb.max), (P::mm(-1.0, -2.0), P::mm(1.0, 2.0)), "quarter-turned rectangle box");
        // area of the containing polygon of a disc is a little above pi r^2
        let disc = Shape::Circle { c: P::ZERO, r: mm(1.0) }.to_polyset(mm(0.001));
        let a = disc.area();
        let exact = std::f64::consts::PI * 1e12;
        assert!(a >= exact && a < exact * 1.003, "disc area {a} vs {exact}");
    }
}
