//! Points, boxes, angles and paths with arcs, on the integer nanometre grid.
//!
//! The board frame is fixed here once for every electronics crate: X right, Y UP, the origin at
//! the lower left of the board outline's bounding box. Fab's 3D scene puts the board in its X / -Z
//! plane with the thickness along +Y (`elec/mech` does that mapping). Formats with a Y-down frame
//! flip at their reader and writer only.

/// One nanometre. `i32` covers +-2.147 m, more than any board or panel.
pub type Nm = i32;

pub const NM_PER_MM: f64 = 1_000_000.0;
pub const NM_PER_UM: f64 = 1_000.0;
pub const NM_PER_MIL: f64 = 25_400.0;
pub const NM_PER_INCH: f64 = 25_400_000.0;

/// Millimetres to nm, rounded to the nearest nm.
#[inline]
pub fn mm(v: f64) -> Nm {
    (v * NM_PER_MM).round() as Nm
}
/// Micrometres to nm.
#[inline]
pub fn um(v: f64) -> Nm {
    (v * NM_PER_UM).round() as Nm
}
/// Thousandths of an inch to nm (exact for whole mils).
#[inline]
pub fn mil(v: f64) -> Nm {
    (v * NM_PER_MIL).round() as Nm
}
/// Metres (the base unit Splash length literals evaluate to: `1.5mm` is 0.0015) to nm.
#[inline]
pub fn metres(v: f64) -> Nm {
    (v * 1e9).round() as Nm
}
/// nm to millimetres.
#[inline]
pub fn to_mm(v: Nm) -> f64 {
    v as f64 / NM_PER_MM
}

/// A point (or a vector) in the board frame.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct P {
    pub x: Nm,
    pub y: Nm,
}

impl P {
    pub const ZERO: P = P { x: 0, y: 0 };
    #[inline]
    pub const fn new(x: Nm, y: Nm) -> P {
        P { x, y }
    }
    /// A point given in millimetres.
    #[inline]
    pub fn mm(x: f64, y: f64) -> P {
        P::new(mm(x), mm(y))
    }
    #[inline]
    pub fn add(self, o: P) -> P {
        P::new(self.x + o.x, self.y + o.y)
    }
    #[inline]
    pub fn sub(self, o: P) -> P {
        P::new(self.x - o.x, self.y - o.y)
    }
    #[inline]
    pub fn neg(self) -> P {
        P::new(-self.x, -self.y)
    }
    /// Dot product, exact.
    #[inline]
    pub fn dot(self, o: P) -> i64 {
        self.x as i64 * o.x as i64 + self.y as i64 * o.y as i64
    }
    /// Cross product `self x o`, exact: positive when `o` is counter-clockwise of `self`.
    #[inline]
    pub fn cross(self, o: P) -> i64 {
        self.x as i64 * o.y as i64 - self.y as i64 * o.x as i64
    }
    /// Squared length, exact.
    #[inline]
    pub fn len2(self) -> i64 {
        self.dot(self)
    }
    #[inline]
    pub fn len(self) -> f64 {
        (self.len2() as f64).sqrt()
    }
    #[inline]
    pub fn dist(self, o: P) -> f64 {
        o.sub(self).len()
    }
    /// Squared distance, exact (i128: no overflow anywhere in the i32 range).
    #[inline]
    pub fn dist2(self, o: P) -> i128 {
        let dx = o.x as i128 - self.x as i128;
        let dy = o.y as i128 - self.y as i128;
        dx * dx + dy * dy
    }
    /// Rotated about the origin, counter-clockwise; exact for quarter turns, otherwise rounded to
    /// the nearest nm.
    pub fn rotated(self, a: Angle) -> P {
        match a.quarter_turns() {
            Some(0) => self,
            Some(1) => P::new(-self.y, self.x),
            Some(2) => P::new(-self.x, -self.y),
            Some(3) => P::new(self.y, -self.x),
            _ => {
                let (s, c) = a.sin_cos();
                let (x, y) = (self.x as f64, self.y as f64);
                P::new((x * c - y * s).round() as Nm, (x * s + y * c).round() as Nm)
            }
        }
    }
    /// Rotated about `about`.
    pub fn rotated_about(self, about: P, a: Angle) -> P {
        self.sub(about).rotated(a).add(about)
    }
    /// Mirrored across the vertical line `x = axis_x` (a part flipped to the bottom side is
    /// mirrored in X, seen from the top).
    #[inline]
    pub fn mirrored_x(self, axis_x: Nm) -> P {
        P::new(2 * axis_x - self.x, self.y)
    }
    /// The point as f64 nm.
    #[inline]
    pub fn f(self) -> (f64, f64) {
        (self.x as f64, self.y as f64)
    }
    /// From f64 nm, rounded to the nearest nm.
    #[inline]
    pub fn from_f(x: f64, y: f64) -> P {
        P::new(x.round() as Nm, y.round() as Nm)
    }
}

/// Sign of the turn `a -> b -> c`: positive left (counter-clockwise), exact.
#[inline]
pub fn orient(a: P, b: P, c: P) -> i128 {
    let abx = b.x as i128 - a.x as i128;
    let aby = b.y as i128 - a.y as i128;
    let acx = c.x as i128 - a.x as i128;
    let acy = c.y as i128 - a.y as i128;
    abx * acy - aby * acx
}

/// An axis-aligned box, inclusive on both ends. `BBox::EMPTY` (min > max) contains nothing and
/// is the identity of `union`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct BBox {
    pub min: P,
    pub max: P,
}

impl Default for BBox {
    fn default() -> Self {
        BBox::EMPTY
    }
}

impl BBox {
    pub const EMPTY: BBox = BBox { min: P { x: Nm::MAX, y: Nm::MAX }, max: P { x: Nm::MIN, y: Nm::MIN } };
    #[inline]
    pub fn new(a: P, b: P) -> BBox {
        BBox { min: P::new(a.x.min(b.x), a.y.min(b.y)), max: P::new(a.x.max(b.x), a.y.max(b.y)) }
    }
    #[inline]
    pub fn point(p: P) -> BBox {
        BBox { min: p, max: p }
    }
    pub fn of_points(pts: impl IntoIterator<Item = P>) -> BBox {
        pts.into_iter().fold(BBox::EMPTY, |b, p| b.with(p))
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }
    #[inline]
    pub fn with(self, p: P) -> BBox {
        BBox { min: P::new(self.min.x.min(p.x), self.min.y.min(p.y)), max: P::new(self.max.x.max(p.x), self.max.y.max(p.y)) }
    }
    #[inline]
    pub fn union(self, o: BBox) -> BBox {
        if o.is_empty() {
            return self;
        }
        if self.is_empty() {
            return o;
        }
        BBox { min: P::new(self.min.x.min(o.min.x), self.min.y.min(o.min.y)), max: P::new(self.max.x.max(o.max.x), self.max.y.max(o.max.y)) }
    }
    /// Grown by `d` on every side (shrunk for negative `d`; may become empty).
    #[inline]
    pub fn inflate(self, d: Nm) -> BBox {
        if self.is_empty() {
            return self;
        }
        BBox { min: P::new(self.min.x.saturating_sub(d), self.min.y.saturating_sub(d)), max: P::new(self.max.x.saturating_add(d), self.max.y.saturating_add(d)) }
    }
    /// Overlap or touch.
    #[inline]
    pub fn intersects(&self, o: &BBox) -> bool {
        !self.is_empty() && !o.is_empty() && self.min.x <= o.max.x && o.min.x <= self.max.x && self.min.y <= o.max.y && o.min.y <= self.max.y
    }
    #[inline]
    pub fn contains(&self, p: P) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }
    #[inline]
    pub fn contains_box(&self, o: &BBox) -> bool {
        o.is_empty() || (self.contains(o.min) && self.contains(o.max))
    }
    #[inline]
    pub fn width(&self) -> i64 {
        if self.is_empty() { 0 } else { self.max.x as i64 - self.min.x as i64 }
    }
    #[inline]
    pub fn height(&self) -> i64 {
        if self.is_empty() { 0 } else { self.max.y as i64 - self.min.y as i64 }
    }
    #[inline]
    pub fn center(&self) -> P {
        P::new(((self.min.x as i64 + self.max.x as i64) / 2) as Nm, ((self.min.y as i64 + self.max.y as i64) / 2) as Nm)
    }
    /// Squared distance from `p` to the box (0 inside), exact.
    pub fn dist2_to(&self, p: P) -> i128 {
        let dx = if p.x < self.min.x { self.min.x as i128 - p.x as i128 } else if p.x > self.max.x { p.x as i128 - self.max.x as i128 } else { 0 };
        let dy = if p.y < self.min.y { self.min.y as i128 - p.y as i128 } else if p.y > self.max.y { p.y as i128 - self.max.y as i128 } else { 0 };
        dx * dx + dy * dy
    }
}

/// An angle in micro-degrees, normalised to `[0, 360 degrees)`. Integer so that rotations
/// compare, hash and round-trip exactly (`90deg` is exactly a quarter turn); one micro-degree is
/// 17 nm of arc at one metre. Counter-clockwise positive in the board frame.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct Angle(i32);

impl Angle {
    pub const UNITS_PER_DEG: i64 = 1_000_000;
    pub const FULL: i64 = 360 * Self::UNITS_PER_DEG;
    pub const ZERO: Angle = Angle(0);
    pub const QUARTER: Angle = Angle(90_000_000);
    pub const HALF: Angle = Angle(180_000_000);

    /// From micro-degrees, normalised.
    #[inline]
    pub fn from_udeg(v: i64) -> Angle {
        Angle(v.rem_euclid(Self::FULL) as i32)
    }
    /// From degrees, rounded to the nearest micro-degree.
    pub fn deg(v: f64) -> Angle {
        Angle::from_udeg((v * Self::UNITS_PER_DEG as f64).round() as i64)
    }
    /// From radians (Splash's `deg` literals evaluate to radians).
    pub fn rad(v: f64) -> Angle {
        Angle::deg(v.to_degrees())
    }
    #[inline]
    pub fn udeg(self) -> i32 {
        self.0
    }
    #[inline]
    pub fn degrees(self) -> f64 {
        self.0 as f64 / Self::UNITS_PER_DEG as f64
    }
    #[inline]
    pub fn radians(self) -> f64 {
        self.degrees().to_radians()
    }
    #[inline]
    pub fn add(self, o: Angle) -> Angle {
        Angle::from_udeg(self.0 as i64 + o.0 as i64)
    }
    #[inline]
    pub fn sub(self, o: Angle) -> Angle {
        Angle::from_udeg(self.0 as i64 - o.0 as i64)
    }
    #[inline]
    pub fn neg(self) -> Angle {
        Angle::from_udeg(-(self.0 as i64))
    }
    /// `Some(0..=3)` when the angle is a whole number of quarter turns.
    #[inline]
    pub fn quarter_turns(self) -> Option<u8> {
        if self.0 % 90_000_000 == 0 { Some((self.0 / 90_000_000) as u8) } else { None }
    }
    /// `(sin, cos)`, exact for quarter turns.
    pub fn sin_cos(self) -> (f64, f64) {
        match self.quarter_turns() {
            Some(0) => (0.0, 1.0),
            Some(1) => (1.0, 0.0),
            Some(2) => (0.0, -1.0),
            Some(3) => (-1.0, 0.0),
            _ => self.radians().sin_cos(),
        }
    }
}

/// One step of a path, from the previous point (or the path's start).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Seg {
    /// A straight line to `P`.
    Line(P),
    /// A circular arc through `mid` to `end`. The three points (previous, mid, end) define it, as
    /// in the board formats; `mid` is any point on the arc strictly between the ends. A
    /// degenerate arc (collinear points) is drawn and measured as the straight line.
    Arc { mid: P, end: P },
}

impl Seg {
    #[inline]
    pub fn end(&self) -> P {
        match *self {
            Seg::Line(p) => p,
            Seg::Arc { end, .. } => end,
        }
    }
}

/// Where on a track's centreline a junction end sits: segment `seg` of the host's path, `frac` of the way along it in fixed point (0 the segment's start, `OnTrack::ONE` its end; along an arc, the fraction of its sweep).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct OnTrack {
    pub seg: u32,
    pub frac: u32,
}

impl OnTrack {
    pub const ONE: u32 = 1 << 31;

    /// The nearest position to `p` on one segment; `mid` makes it an arc, unless collinear.
    pub fn along(seg: u32, a: P, mid: Option<P>, b: P, p: P) -> OnTrack {
        if p == a { return OnTrack { seg, frac: 0 }; }
        if p == b { return OnTrack { seg, frac: Self::ONE }; }
        if let Some(g) = mid.and_then(|m| ArcGeom::new(a, m, b)) {
            let angle = (p.y as f64 - g.center.1).atan2(p.x as f64 - g.center.0);
            let sweep = (if g.sweep > 0.0 { angle - g.start } else { g.start - angle }).rem_euclid(std::f64::consts::TAU);
            let frac = if sweep <= g.sweep.abs() {
                (sweep / g.sweep.abs() * Self::ONE as f64).round() as u32
            } else if p.dist2(a) <= p.dist2(b) { 0 } else { Self::ONE };
            return OnTrack { seg, frac: frac.min(Self::ONE) };
        }
        let (dx, dy) = (b.x as i128 - a.x as i128, b.y as i128 - a.y as i128);
        let len2 = dx * dx + dy * dy;
        let dot = ((p.x as i128 - a.x as i128) * dx + (p.y as i128 - a.y as i128) * dy).clamp(0, len2);
        let frac = if len2 == 0 { 0 } else { ((dot * Self::ONE as i128 + len2 / 2) / len2) as u32 };
        OnTrack { seg, frac }
    }
}

/// An open polyline of lines and arcs (a track's centre line).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Path {
    pub start: P,
    pub segs: Vec<Seg>,
}

impl Path {
    pub fn new(start: P) -> Path {
        Path { start, segs: Vec::new() }
    }
    pub fn line(mut self, to: P) -> Path {
        self.segs.push(Seg::Line(to));
        self
    }
    pub fn arc(mut self, mid: P, end: P) -> Path {
        self.segs.push(Seg::Arc { mid, end });
        self
    }
    pub fn end(&self) -> P {
        self.segs.last().map(|s| s.end()).unwrap_or(self.start)
    }
    /// The point at `at`, clamping the segment to the path's end and the fraction to `ONE`.
    pub fn point_on(&self, at: OnTrack) -> P {
        let i = at.seg as usize;
        let Some(s) = self.segs.get(i) else { return self.end() };
        let a = if i == 0 { self.start } else { self.segs[i - 1].end() };
        let b = s.end();
        let frac = at.frac.min(OnTrack::ONE);
        if frac == 0 { return a; }
        if frac == OnTrack::ONE { return b; }
        if let Seg::Arc { mid, .. } = *s {
            if let Some(g) = ArcGeom::new(a, mid, b) {
                return g.at(frac as f64 / OnTrack::ONE as f64);
            }
        }
        let one = OnTrack::ONE as i128;
        let lerp = |a: Nm, b: Nm| {
            let n = a as i128 * (one - frac as i128) + b as i128 * frac as i128;
            ((n + n.signum() * (one / 2)) / one) as Nm
        };
        P::new(lerp(a.x, b.x), lerp(a.y, b.y))
    }
    /// The nearest point on the centreline and its position; the first minimum wins.
    pub fn locate(&self, p: P) -> (OnTrack, P) {
        let mut best = (OnTrack::default(), self.start);
        let mut distance = i128::MAX;
        for (i, (a, s)) in self.steps().enumerate() {
            let mid = match s { Seg::Line(_) => None, Seg::Arc { mid, .. } => Some(mid) };
            let at = OnTrack::along(i as u32, a, mid, s.end(), p);
            let point = self.point_on(at);
            let d = p.dist2(point);
            if d < distance {
                best = (at, point);
                distance = d;
            }
        }
        best
    }
    /// The steps with their start point: `(from, seg)`.
    pub fn steps(&self) -> impl Iterator<Item = (P, Seg)> + '_ {
        let mut at = self.start;
        self.segs.iter().map(move |s| {
            let from = at;
            at = s.end();
            (from, *s)
        })
    }
    /// The box of every point of the path (arcs included, not only their end points).
    pub fn bbox(&self) -> BBox {
        let mut b = BBox::point(self.start);
        for (from, s) in self.steps() {
            b = match s {
                Seg::Line(p) => b.with(p),
                Seg::Arc { mid, end } => b.union(arc_bbox(from, mid, end)),
            };
        }
        b
    }
    /// The length along the path in nm (arcs by their true length).
    pub fn length(&self) -> f64 {
        self.steps()
            .map(|(from, s)| match s {
                Seg::Line(p) => from.dist(p),
                Seg::Arc { mid, end } => ArcGeom::new(from, mid, end).map(|g| g.length()).unwrap_or_else(|| from.dist(end)),
            })
            .sum()
    }
    pub fn translated(&self, by: P) -> Path {
        Path {
            start: self.start.add(by),
            segs: self
                .segs
                .iter()
                .map(|s| match *s {
                    Seg::Line(p) => Seg::Line(p.add(by)),
                    Seg::Arc { mid, end } => Seg::Arc { mid: mid.add(by), end: end.add(by) },
                })
                .collect(),
        }
    }
    /// The same points mapped by `f` (rotation, mirror). A mirror keeps arcs valid: an arc is its
    /// three points, so no direction flag has to change.
    pub fn mapped(&self, f: impl Fn(P) -> P) -> Path {
        Path {
            start: f(self.start),
            segs: self
                .segs
                .iter()
                .map(|s| match *s {
                    Seg::Line(p) => Seg::Line(f(p)),
                    Seg::Arc { mid, end } => Seg::Arc { mid: f(mid), end: f(end) },
                })
                .collect(),
        }
    }
}

/// A closed contour of lines and arcs: a `Path` whose last step ends at its start. Board outlines
/// and cutouts keep their arcs (Gerber G02/G03, exact edge clearance); `to_ring` polygonises.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Contour {
    pub path: Path,
}

impl Contour {
    /// A closed contour; the closing line back to the start is added when the path does not
    /// already end there.
    pub fn new(mut path: Path) -> Contour {
        if path.end() != path.start {
            path.segs.push(Seg::Line(path.start));
        }
        Contour { path }
    }
    /// An axis-aligned rectangle from `min` to `max`, with corner radius `radius` (0: sharp),
    /// counter-clockwise from the bottom edge.
    pub fn rect(min: P, max: P, radius: Nm) -> Contour {
        let r = radius.max(0).min(((max.x - min.x) / 2).min((max.y - min.y) / 2));
        if r == 0 {
            return Contour::new(Path::new(min).line(P::new(max.x, min.y)).line(max).line(P::new(min.x, max.y)));
        }
        // the arc mid points sit at 45 degrees on each corner circle
        let k = r as f64 * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
        let k = k.round() as Nm;
        let p = Path::new(P::new(min.x + r, min.y))
            .line(P::new(max.x - r, min.y))
            .arc(P::new(max.x - k, min.y + k), P::new(max.x, min.y + r))
            .line(P::new(max.x, max.y - r))
            .arc(P::new(max.x - k, max.y - k), P::new(max.x - r, max.y))
            .line(P::new(min.x + r, max.y))
            .arc(P::new(min.x + k, max.y - k), P::new(min.x, max.y - r))
            .line(P::new(min.x, min.y + r))
            .arc(P::new(min.x + k, min.y + k), P::new(min.x + r, min.y));
        Contour::new(p)
    }
    /// A full circle, as two half arcs.
    pub fn circle(c: P, r: Nm) -> Contour {
        Contour::new(Path::new(P::new(c.x + r, c.y)).arc(P::new(c.x, c.y + r), P::new(c.x - r, c.y)).arc(P::new(c.x, c.y - r), P::new(c.x + r, c.y)))
    }
    pub fn bbox(&self) -> BBox {
        self.path.bbox()
    }
    /// The contour as a polygon ring; arcs become chords whose vertices lie on the arc, at most
    /// `max_err` inside it. Orientation follows the contour's.
    pub fn to_ring(&self, max_err: Nm) -> Vec<P> {
        let mut out = vec![self.path.start];
        for (from, s) in self.path.steps() {
            match s {
                Seg::Line(p) => out.push(p),
                Seg::Arc { mid, end } => match ArcGeom::new(from, mid, end) {
                    Some(g) => g.chord_points(max_err, &mut out),
                    None => out.push(end),
                },
            }
        }
        if out.len() > 1 && out.last() == out.first() {
            out.pop();
        }
        out
    }
}

/// The circle an arc lies on, from its three points; in f64 nm.
#[derive(Copy, Clone, Debug)]
pub struct ArcGeom {
    pub center: (f64, f64),
    pub radius: f64,
    /// Start angle in radians, measured at the centre.
    pub start: f64,
    /// Signed sweep in radians (positive counter-clockwise), `0 < |sweep| < 2 pi`.
    pub sweep: f64,
    pub a: P,
    pub b: P,
}

impl ArcGeom {
    /// `None` when the three points are collinear (a straight line) or coincide.
    pub fn new(a: P, mid: P, b: P) -> Option<ArcGeom> {
        let o = orient(a, mid, b);
        if o == 0 {
            return None;
        }
        let (ax, ay) = a.f();
        let (mx, my) = mid.f();
        let (bx, by) = b.f();
        // circumcentre, computed relative to `a` for precision
        let (ux, uy) = (mx - ax, my - ay);
        let (vx, vy) = (bx - ax, by - ay);
        let d = 2.0 * (ux * vy - uy * vx);
        let uu = ux * ux + uy * uy;
        let vv = vx * vx + vy * vy;
        let cx = (vy * uu - uy * vv) / d;
        let cy = (ux * vv - vx * uu) / d;
        let center = (ax + cx, ay + cy);
        let radius = (cx * cx + cy * cy).sqrt();
        let ang = |x: f64, y: f64| (y - center.1).atan2(x - center.0);
        let start = ang(ax, ay);
        let end = ang(bx, by);
        let tau = std::f64::consts::TAU;
        // counter-clockwise when a -> mid -> b turns left
        let sweep = if o > 0 { (end - start).rem_euclid(tau) } else { -((start - end).rem_euclid(tau)) };
        Some(ArcGeom { center, radius, start, sweep, a, b })
    }
    pub fn length(&self) -> f64 {
        self.radius * self.sweep.abs()
    }
    /// True when the direction at angle `t` (radians, at the centre) lies within the sweep.
    pub fn contains_angle(&self, t: f64) -> bool {
        let tau = std::f64::consts::TAU;
        if self.sweep >= 0.0 {
            (t - self.start).rem_euclid(tau) <= self.sweep
        } else {
            (self.start - t).rem_euclid(tau) <= -self.sweep
        }
    }
    /// The point at sweep fraction `f` (0 = start, 1 = end), rounded.
    pub fn at(&self, f: f64) -> P {
        let t = self.start + self.sweep * f;
        P::from_f(self.center.0 + self.radius * t.cos(), self.center.1 + self.radius * t.sin())
    }
    /// Number of chords so that each chord's sagitta is at most `max_err` (at least one).
    pub fn chords_for(&self, radius: f64, max_err: Nm) -> usize {
        let e = (max_err.max(1) as f64).min(radius);
        if radius <= 0.0 {
            return 1;
        }
        // sagitta e = r (1 - cos(theta / 2)) -> theta = 2 acos(1 - e / r)
        let theta = 2.0 * (1.0 - e / radius).clamp(-1.0, 1.0).acos();
        if theta <= 0.0 {
            return 1;
        }
        ((self.sweep.abs() / theta).ceil() as usize).max(1)
    }
    /// Pushes the chord vertices after the start, ending exactly at `b`.
    pub fn chord_points(&self, max_err: Nm, out: &mut Vec<P>) {
        let n = self.chords_for(self.radius, max_err);
        for i in 1..n {
            out.push(self.at(i as f64 / n as f64));
        }
        out.push(self.b);
    }
}

/// The bounding box of the arc `a -> mid -> b`, including the extreme points it sweeps over.
pub fn arc_bbox(a: P, mid: P, b: P) -> BBox {
    let mut bb = BBox::point(a).with(b).with(mid);
    if let Some(g) = ArcGeom::new(a, mid, b) {
        for k in 0..4 {
            let t = k as f64 * std::f64::consts::FRAC_PI_2;
            if g.contains_angle(t) {
                let (s, c) = t.sin_cos();
                bb = bb.with(P::from_f(g.center.0 + g.radius * c, g.center.1 + g.radius * s));
            }
        }
        // rounding of the extreme point is at most half a nm each way
        bb = bb.inflate(1);
    }
    bb
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Units, angles, rotation and arc geometry in one setup: a 3-4-5 triangle and a quarter arc.
    #[test]
    fn units_angles_and_arcs() {
        assert_eq!(mm(1.0), 1_000_000, "mm");
        assert_eq!(mil(1.0), 25_400, "mil");
        assert_eq!(metres(0.0015), 1_500_000, "metres as Splash length literals give them");
        assert_eq!(Angle::deg(-90.0), Angle::deg(270.0), "angles normalise");
        assert_eq!(Angle::rad(std::f64::consts::FRAC_PI_2), Angle::QUARTER, "radians to a quarter turn");
        let p = P::mm(3.0, 4.0);
        assert_eq!(p.rotated(Angle::QUARTER), P::mm(-4.0, 3.0), "quarter turns are exact");
        assert_eq!(p.rotated(Angle::deg(45.0)).rotated(Angle::deg(315.0)), p, "a full turn in two steps comes back");
        assert_eq!(p.len(), 5_000_000.0, "length");
        // the quarter arc from (r,0) via 45deg to (0,r) about the origin, r = 10 mm
        let r = mm(10.0);
        let k = (r as f64 * std::f64::consts::FRAC_1_SQRT_2).round() as Nm;
        let g = ArcGeom::new(P::new(r, 0), P::new(k, k), P::new(0, r)).expect("arc");
        assert!((g.radius - r as f64).abs() < 1.0, "radius from three points");
        assert!((g.sweep - std::f64::consts::FRAC_PI_2).abs() < 1e-6, "counter-clockwise quarter sweep");
        let bb = arc_bbox(P::new(r, 0), P::new(k, k), P::new(0, r));
        assert!(bb.max.x >= r && bb.max.y >= r && bb.min.x <= 0 && bb.min.y <= 0, "box holds the arc");
        let reverse = ArcGeom::new(P::new(0, r), P::new(k, k), P::new(r, 0)).expect("arc");
        assert!(reverse.sweep < 0.0, "the reverse arc is clockwise");
        assert!(ArcGeom::new(P::ZERO, P::new(1, 1), P::new(2, 2)).is_none(), "collinear points are no arc");
        // chord vertices stay within the error
        let mut pts = vec![g.a];
        g.chord_points(mm(0.005), &mut pts);
        assert_eq!(*pts.last().unwrap(), g.b, "chords end at the arc end");
        for w in pts.windows(2) {
            let m = P::new((w[0].x + w[1].x) / 2, (w[0].y + w[1].y) / 2);
            assert!(r as f64 - m.len() <= mm(0.005) as f64 + 2.0, "sagitta within max_err");
        }
        // a rounded rectangle contour: closed, its box is the rectangle, its length has the arcs
        let c = Contour::rect(P::ZERO, P::mm(50.0, 30.0), mm(2.0));
        assert_eq!(c.path.end(), c.path.start, "closed");
        let b = c.bbox();
        assert!(b.min.x >= -1 && b.min.y >= -1 && b.max.x <= mm(50.0) + 1 && b.max.y <= mm(30.0) + 1, "box");
        let expect = 2.0 * (46.0 + 26.0) * 1e6 + std::f64::consts::TAU * 2e6;
        assert!((c.path.length() - expect).abs() < 50.0, "perimeter with arcs: {}", c.path.length());
    }
}
