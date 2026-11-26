use std::{
    f64::consts::{FRAC_PI_2, FRAC_PI_4},
    ops::{Add, Mul, Sub},
};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Point(pub f64, pub f64);

impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point(self.0 + o.0, self.1 + o.1)
    }
}
impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point(self.0 - o.0, self.1 - o.1)
    }
}
impl Mul<f64> for Point {
    type Output = Point;
    fn mul(self, k: f64) -> Point {
        Point(self.0 * k, self.1 * k)
    }
}
impl Point {
    pub fn dot(self, p: Point) -> f64 {
        self.0 * p.0 + self.1 * p.1
    }
    pub fn cross(self, p: Point) -> f64 {
        self.0 * p.1 - self.1 * p.0
    }
    pub fn norm2(self) -> f64 {
        self.dot(self)
    }
    pub fn norm(self) -> f64 {
        self.norm2().sqrt()
    }
    pub fn perp(self) -> Point {
        Point(-self.1, self.0)
    }
    pub fn unit(self) -> Option<Point> {
        let len = self.norm();
        (len > 0.0).then(|| self * (1.0 / len))
    }

    // Canonical unoriented axis angle. Range reduction makes a short atan
    // series sufficient and avoids platform-dependent libm approximations.
    pub fn angle(mut self) -> f64 {
        if self.0 < 0.0 {
            self = self * -1.0;
        }
        let y = self.1.abs();
        let steep = y > self.0;
        let r = if steep { self.0 / y } else { y / self.0 };
        let offset = r > std::f64::consts::SQRT_2 - 1.0;
        let x = if offset { (r - 1.0) / (r + 1.0) } else { r };
        let mut power = x;
        let mut angle = x;
        for i in 1..21 {
            power *= -x * x;
            angle += power / (2 * i + 1) as f64;
        }
        if offset {
            angle += FRAC_PI_4;
        }
        if steep {
            angle = FRAC_PI_2 - angle;
        }
        angle.copysign(self.1)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Frame {
    origin: Point,
    span: f64,
    factor: f64,
}

impl Frame {
    pub fn new<'a>(points: impl Iterator<Item = &'a (f64, f64)>) -> Option<Self> {
        let mut lo = Point(f64::INFINITY, f64::INFINITY);
        let mut hi = Point(f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in points {
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            lo = Point(lo.0.min(x), lo.1.min(y));
            hi = Point(hi.0.max(x), hi.1.max(y));
        }
        let mut span = (hi.0 - lo.0).max(hi.1 - lo.1);
        let mut factor = 1.0;
        if span == f64::INFINITY {
            factor = 0.5;
            lo = lo * factor;
            hi = hi * factor;
            span = (hi.0 - lo.0).max(hi.1 - lo.1);
        }
        (span > 0.0).then_some(Self { origin: lo, span, factor })
    }
    pub fn local(self, (x, y): (f64, f64)) -> Point {
        let d = Point(x, y) * self.factor - self.origin;
        Point(d.0 / self.span, d.1 / self.span)
    }
    pub fn world(self, p: Point) -> Option<(f64, f64)> {
        let p = self.origin + p * self.span;
        pair(p.0 / self.factor, p.1 / self.factor)
    }
    pub fn size(self, w: f64, h: f64) -> Option<(f64, f64)> {
        let (w, h) = pair(w * self.span / self.factor, h * self.span / self.factor)?;
        (w > 0.0 && h > 0.0).then_some((w, h))
    }
}

fn pair(x: f64, y: f64) -> Option<(f64, f64)> {
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

pub(super) struct Samples {
    pub points: Vec<Point>,
    pub closed: bool,
}

impl Samples {
    pub fn new(raw: &[(f64, f64)], frame: Frame, allow_closed: bool) -> Option<Self> {
        const COUNT: usize = 160;
        let start = frame.local(*raw.first()?);
        let mut prev = start;
        let mut length = 0.0;
        for &p in &raw[1..] {
            let p = frame.local(p);
            length += (p - prev).norm();
            prev = p;
        }
        if length <= 0.0 {
            return None;
        }
        let gap = (start - prev).norm();
        let closed = allow_closed && gap <= 0.09 * length && gap <= 0.28;
        let spacing =
            if closed { (length + gap) / COUNT as f64 } else { length / (COUNT - 1) as f64 };
        let mut points = Vec::with_capacity(COUNT);
        points.push(start);
        let mut travelled = 0.0;
        prev = start;
        let seam = closed.then_some(&raw[0]);
        for &p in raw[1..].iter().chain(seam) {
            let p = frame.local(p);
            let delta = p - prev;
            let len = delta.norm();
            let end = travelled + len;
            while len > 0.0 && points.len() < COUNT {
                let target = spacing * points.len() as f64;
                if target > end {
                    break;
                }
                points.push(prev + delta * ((target - travelled) / len));
            }
            prev = p;
            travelled = end;
        }
        points.resize(COUNT, prev);
        if !closed {
            points[COUNT - 1] = prev;
        }

        // A three-tap filter removes sample jitter and preserves open endpoints.
        let original = points.clone();
        for i in 0..COUNT {
            if closed || (i > 0 && i + 1 < COUNT) {
                points[i] = original[i] * 0.6
                    + (original[(i + COUNT - 1) % COUNT] + original[(i + 1) % COUNT]) * 0.2;
            }
        }
        Some(Self { points, closed })
    }
}

#[derive(Clone, Copy)]
pub(super) struct Segment {
    start: Point,
    delta: Point,
    inv_len2: f64,
}

impl Segment {
    pub fn new(start: Point, end: Point) -> Self {
        let delta = end - start;
        let len2 = delta.norm2();
        Self { start, delta, inv_len2: if len2 > 0.0 { 1.0 / len2 } else { 0.0 } }
    }
    pub fn project(self, p: Point) -> (f64, f64) {
        let d = p - self.start;
        let t = (d.dot(self.delta) * self.inv_len2).clamp(0.0, 1.0);
        (t, (d - self.delta * t).norm2())
    }
}

// Principal direction of a symmetric covariance matrix. Choose the longer
// of two equivalent eigenvectors to avoid cancellation near an axis.
pub(super) fn principal(xx: f64, xy: f64, yy: f64) -> (Point, f64) {
    let gap = Point(xx - yy, 2.0 * xy).norm();
    let axis =
        if xx >= yy { Point(gap + xx - yy, 2.0 * xy) } else { Point(2.0 * xy, gap + yy - xx) };
    (axis.unit().unwrap_or(Point(1.0, 0.0)), (xx + yy + gap) * 0.5)
}
