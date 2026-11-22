//! The two scalar types the solver is generic over: `f64` for DC and transient, `C64` for AC.

use std::fmt::Debug;
use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub, SubAssign};

/// What the linear solver needs from a matrix entry.
pub trait Scalar:
    Copy
    + Debug
    + Default
    + PartialEq
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
    + AddAssign
    + SubAssign
    + MulAssign
{
    const ZERO: Self;
    const ONE: Self;
    /// The magnitude pivoting compares.
    fn mag(self) -> f64;
    fn from_f64(v: f64) -> Self;
    /// The slice as f64 (compiled LU plans run on f64 only); None for other scalars.
    fn as_f64(s: &[Self]) -> Option<&[f64]> {
        let _ = s;
        None
    }
    fn as_f64_mut(s: &mut [Self]) -> Option<&mut [f64]> {
        let _ = s;
        None
    }
}

impl Scalar for f64 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    #[inline(always)]
    fn mag(self) -> f64 {
        self.abs()
    }
    #[inline(always)]
    fn from_f64(v: f64) -> Self {
        v
    }
    #[inline(always)]
    fn as_f64(s: &[f64]) -> Option<&[f64]> {
        Some(s)
    }
    #[inline(always)]
    fn as_f64_mut(s: &mut [f64]) -> Option<&mut [f64]> {
        Some(s)
    }
}

/// A complex number in f64 (the kernel keeps its own so it has no dependencies).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C64 {
    pub re: f64,
    pub im: f64,
}

impl C64 {
    pub const I: C64 = C64 { re: 0.0, im: 1.0 };
    pub const ZERO_C: C64 = C64 { re: 0.0, im: 0.0 };
    #[inline(always)]
    pub const fn new(re: f64, im: f64) -> Self {
        C64 { re, im }
    }
    /// `r * e^(i theta)`.
    pub fn polar(r: f64, theta: f64) -> Self {
        C64 { re: r * theta.cos(), im: r * theta.sin() }
    }
    #[inline(always)]
    pub fn norm(self) -> f64 {
        self.re.hypot(self.im)
    }
    #[inline(always)]
    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
    /// The argument in radians, in (-pi, pi].
    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
    #[inline(always)]
    pub fn conj(self) -> Self {
        C64 { re: self.re, im: -self.im }
    }
    #[inline(always)]
    pub fn scale(self, s: f64) -> Self {
        C64 { re: self.re * s, im: self.im * s }
    }
    /// `20 log10 |z|`.
    pub fn db(self) -> f64 {
        20.0 * self.norm().log10()
    }
    pub fn exp(self) -> Self {
        C64::polar(self.re.exp(), self.im)
    }
}

impl Add for C64 {
    type Output = C64;
    #[inline(always)]
    fn add(self, o: C64) -> C64 {
        C64 { re: self.re + o.re, im: self.im + o.im }
    }
}
impl Sub for C64 {
    type Output = C64;
    #[inline(always)]
    fn sub(self, o: C64) -> C64 {
        C64 { re: self.re - o.re, im: self.im - o.im }
    }
}
impl Mul for C64 {
    type Output = C64;
    #[inline(always)]
    fn mul(self, o: C64) -> C64 {
        C64 { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
}
impl Div for C64 {
    type Output = C64;
    /// Smith's algorithm: no overflow for large components.
    #[inline]
    fn div(self, o: C64) -> C64 {
        if o.re.abs() >= o.im.abs() {
            let r = o.im / o.re;
            let d = o.re + o.im * r;
            C64 { re: (self.re + self.im * r) / d, im: (self.im - self.re * r) / d }
        } else {
            let r = o.re / o.im;
            let d = o.re * r + o.im;
            C64 { re: (self.re * r + self.im) / d, im: (self.im * r - self.re) / d }
        }
    }
}
impl Neg for C64 {
    type Output = C64;
    #[inline(always)]
    fn neg(self) -> C64 {
        C64 { re: -self.re, im: -self.im }
    }
}
impl AddAssign for C64 {
    #[inline(always)]
    fn add_assign(&mut self, o: C64) {
        self.re += o.re;
        self.im += o.im;
    }
}
impl SubAssign for C64 {
    #[inline(always)]
    fn sub_assign(&mut self, o: C64) {
        self.re -= o.re;
        self.im -= o.im;
    }
}
impl MulAssign for C64 {
    #[inline(always)]
    fn mul_assign(&mut self, o: C64) {
        *self = *self * o;
    }
}

impl Scalar for C64 {
    const ZERO: Self = C64 { re: 0.0, im: 0.0 };
    const ONE: Self = C64 { re: 1.0, im: 0.0 };
    /// The 1-norm: as good as the modulus for pivot choice and cheaper.
    #[inline(always)]
    fn mag(self) -> f64 {
        self.re.abs() + self.im.abs()
    }
    #[inline(always)]
    fn from_f64(v: f64) -> Self {
        C64 { re: v, im: 0.0 }
    }
}
