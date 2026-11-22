// Ported from TiNRS Psychedelic Router 0.1.0: src/core/Geometry.h.
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Vectors, boxes, segments, circles and the curve evaluators, in `f32` mm as the reference
//! has them. The port's frame is the reference's: Y down (the adapter flips once on the way in
//! and once on the way out).
//!
//! Porting rules kept here so every ported file follows them:
//! - `Vec2` has no `PartialEq`: the reference's `==` is the approximate `approx_eq` (1e-5);
//!   an exact comparison is written on the fields.
//! - `std::min`, `std::max` and `std::clamp` on floats are `std_min`, `std_max` and
//!   `std_clamp` below (the same answers for NaN and signed zeros, no panic on an empty range).

use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Sub, SubAssign};

pub const K_PI: f32 = std::f32::consts::PI;

/// `std::min(a, b)`: `b < a ? b : a`.
#[inline]
pub fn std_min<T: PartialOrd>(a: T, b: T) -> T {
    if b < a {
        b
    } else {
        a
    }
}

/// `std::max(a, b)`: `a < b ? b : a`.
#[inline]
pub fn std_max<T: PartialOrd>(a: T, b: T) -> T {
    if a < b {
        b
    } else {
        a
    }
}

/// `std::clamp(v, lo, hi)`: `v < lo ? lo : hi < v ? hi : v`.
#[inline]
pub fn std_clamp<T: PartialOrd>(v: T, lo: T, hi: T) -> T {
    if v < lo {
        lo
    } else if hi < v {
        hi
    } else {
        v
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Vec2 {
        Vec2 { x, y }
    }
    /// The reference's `operator==`: both coordinates within 1e-5.
    #[inline]
    pub fn approx_eq(self, o: Vec2) -> bool {
        (self.x - o.x).abs() < 1e-5 && (self.y - o.y).abs() < 1e-5
    }
    #[inline]
    pub fn length_squared(self) -> f32 {
        self.x * self.x + self.y * self.y
    }
    #[inline]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }
    #[inline]
    pub fn normalized(self) -> Vec2 {
        let l = self.length();
        if l > 1e-6 {
            self / l
        } else {
            Vec2::new(0.0, 0.0)
        }
    }
    #[inline]
    pub fn perpendicular(self) -> Vec2 {
        Vec2::new(-self.y, self.x)
    }
    #[inline]
    pub fn dot(self, o: Vec2) -> f32 {
        self.x * o.x + self.y * o.y
    }
    #[inline]
    pub fn cross(self, o: Vec2) -> f32 {
        self.x * o.y - self.y * o.x
    }
    #[inline]
    pub fn distance_to(self, o: Vec2) -> f32 {
        (self - o).length()
    }
    #[inline]
    pub fn distance_squared_to(self, o: Vec2) -> f32 {
        (self - o).length_squared()
    }
    #[inline]
    pub fn lerp(a: Vec2, b: Vec2, t: f32) -> Vec2 {
        a + (b - a) * t
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    #[inline]
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    #[inline]
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f32> for Vec2 {
    type Output = Vec2;
    #[inline]
    fn mul(self, s: f32) -> Vec2 {
        Vec2::new(self.x * s, self.y * s)
    }
}

impl Div<f32> for Vec2 {
    type Output = Vec2;
    /// As the reference: multiply by the reciprocal.
    #[inline]
    fn div(self, s: f32) -> Vec2 {
        let inv = 1.0 / s;
        Vec2::new(self.x * inv, self.y * inv)
    }
}

impl AddAssign for Vec2 {
    #[inline]
    fn add_assign(&mut self, o: Vec2) {
        self.x += o.x;
        self.y += o.y;
    }
}

impl SubAssign for Vec2 {
    #[inline]
    fn sub_assign(&mut self, o: Vec2) {
        self.x -= o.x;
        self.y -= o.y;
    }
}

impl MulAssign<f32> for Vec2 {
    #[inline]
    fn mul_assign(&mut self, s: f32) {
        self.x *= s;
        self.y *= s;
    }
}

impl DivAssign<f32> for Vec2 {
    #[inline]
    fn div_assign(&mut self, s: f32) {
        let inv = 1.0 / s;
        self.x *= inv;
        self.y *= inv;
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { x, y, z }
    }
}

/// An axis-aligned box; the default is the empty box (min 1e9, max -1e9).
#[derive(Copy, Clone, Debug)]
pub struct Box2 {
    pub min: Vec2,
    pub max: Vec2,
}

impl Default for Box2 {
    fn default() -> Box2 {
        Box2 { min: Vec2::new(1e9, 1e9), max: Vec2::new(-1e9, -1e9) }
    }
}

impl Box2 {
    #[inline]
    pub const fn new(min: Vec2, max: Vec2) -> Box2 {
        Box2 { min, max }
    }
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.min.x <= self.max.x && self.min.y <= self.max.y
    }
    #[inline]
    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }
    #[inline]
    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }
    #[inline]
    pub fn expand(&mut self, p: Vec2) {
        self.min.x = std_min(self.min.x, p.x);
        self.min.y = std_min(self.min.y, p.y);
        self.max.x = std_max(self.max.x, p.x);
        self.max.y = std_max(self.max.y, p.y);
    }
    #[inline]
    pub fn expand_box(&mut self, b: &Box2) {
        self.expand(b.min);
        self.expand(b.max);
    }
    #[inline]
    pub fn pad(&mut self, margin: f32) {
        self.min.x -= margin;
        self.min.y -= margin;
        self.max.x += margin;
        self.max.y += margin;
    }
    #[inline]
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }
    #[inline]
    pub fn overlaps(&self, o: &Box2) -> bool {
        self.max.x >= o.min.x && self.min.x <= o.max.x && self.max.y >= o.min.y && self.min.y <= o.max.y
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Segment2 {
    pub p0: Vec2,
    pub p1: Vec2,
}

impl Segment2 {
    #[inline]
    pub const fn new(a: Vec2, b: Vec2) -> Segment2 {
        Segment2 { p0: a, p1: b }
    }
    #[inline]
    pub fn length(&self) -> f32 {
        (self.p1 - self.p0).length()
    }
    #[inline]
    pub fn length_squared(&self) -> f32 {
        (self.p1 - self.p0).length_squared()
    }
    pub fn closest_point(&self, pt: Vec2) -> Vec2 {
        let ab = self.p1 - self.p0;
        let ab2 = ab.length_squared();
        if ab2 < 1e-8 {
            return self.p0;
        }
        let t = std_clamp((pt - self.p0).dot(ab) / ab2, 0.0, 1.0);
        self.p0 + ab * t
    }
    /// `distanceTo(const Vec2&)`.
    pub fn distance_to_point(&self, pt: Vec2) -> f32 {
        (self.closest_point(pt) - pt).length()
    }
    /// `distanceTo(const Segment2&)`.
    pub fn distance_to_seg(&self, o: &Segment2) -> f32 {
        if self.crosses_strictly(o, 1e-6) {
            return 0.0;
        }
        if self.intersects(o).is_some() {
            return 0.0;
        }
        let d1 = self.distance_to_point(o.p0);
        let d2 = self.distance_to_point(o.p1);
        let d3 = o.distance_to_point(self.p0);
        let d4 = o.distance_to_point(self.p1);
        // std::min({d1, d2, d3, d4}): the first smallest
        std_min(std_min(std_min(d1, d2), d3), d4)
    }
    /// `intersects(o, &hit)`: the intersection point when the segments meet (parallel and
    /// collinear segments never do).
    pub fn intersects(&self, o: &Segment2) -> Option<Vec2> {
        let r = self.p1 - self.p0;
        let s = o.p1 - o.p0;
        let rxs = r.cross(s);
        let qp = o.p0 - self.p0;
        let qpxr = qp.cross(r);
        if rxs.abs() < 1e-8 {
            return None;
        }
        let t = qp.cross(s) / rxs;
        let u = qpxr / rxs;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            Some(self.p0 + r * t)
        } else {
            None
        }
    }
    /// Proper crossing: interiors intersect transversally. Touching endpoints, T-junctions and
    /// collinear overlaps are not reported. The reference's default `eps` is 1e-6.
    pub fn crosses_strictly(&self, o: &Segment2, eps: f32) -> bool {
        let r = self.p1 - self.p0;
        let s = o.p1 - o.p0;
        let d1 = r.cross(o.p0 - self.p0);
        let d2 = r.cross(o.p1 - self.p0);
        let d3 = s.cross(self.p0 - o.p0);
        let d4 = s.cross(self.p1 - o.p0);
        let ab = (d1 > eps && d2 < -eps) || (d1 < -eps && d2 > eps);
        let cd = (d3 > eps && d4 < -eps) || (d3 < -eps && d4 > eps);
        ab && cd
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Circle2 {
    pub center: Vec2,
    pub radius: f32,
}

impl Circle2 {
    #[inline]
    pub const fn new(c: Vec2, r: f32) -> Circle2 {
        Circle2 { center: c, radius: r }
    }
    #[inline]
    pub fn contains(&self, p: Vec2) -> bool {
        (p - self.center).length_squared() <= self.radius * self.radius
    }
    #[inline]
    pub fn distance_to(&self, p: Vec2) -> f32 {
        std_max(0.0, (p - self.center).length() - self.radius)
    }
}

/// Catmull-Rom spline evaluator for smooth organic trace curve interpolation.
pub fn catmull_rom(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let t2 = t * t;
    let t3 = t2 * t;
    (p1 * 2.0 + (p2 - p0) * t + (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * t2 + (p0 * -1.0 + p1 * 3.0 - p2 * 3.0 + p3) * t3) * 0.5
}

/// Cubic Bezier interpolation.
pub fn cubic_bezier(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let u = 1.0 - t;
    let tt = t * t;
    let uu = u * u;
    let uuu = uu * u;
    let ttt = tt * t;
    p0 * uuu + p1 * (3.0 * uu * t) + p2 * (3.0 * u * tt) + p3 * ttt
}
