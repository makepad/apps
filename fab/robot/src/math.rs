//! Small fixed-size math for arms: 3-vectors, 3x3 rotations (row-major), rigid poses and the
//! dense solves a 6- or 12-joint system needs. Units are the crate's: mm, kg, s, rad.

pub type V3 = [f64; 3];
pub type M3 = [[f64; 3]; 3];

pub const ZERO: V3 = [0.0; 3];
pub const I3: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[inline]
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
pub fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
#[inline]
pub fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}
#[inline]
pub fn dist(a: V3, b: V3) -> f64 {
    len(sub(a, b))
}
/// `a` scaled to unit length (`a` itself when it has none).
#[inline]
pub fn norm(a: V3) -> V3 {
    let l = len(a);
    if l > 1e-300 {
        scale(a, 1.0 / l)
    } else {
        a
    }
}
#[inline]
pub fn lerp(a: V3, b: V3, t: f64) -> V3 {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}
#[inline]
pub fn mv(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}
/// `mᵀ v`.
#[inline]
pub fn mtv(m: &M3, v: V3) -> V3 {
    [m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2], m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2], m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2]]
}
pub fn mm(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j]))
}
pub fn transpose(a: &M3) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[j][i]))
}
pub fn madd(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[i][j] + b[i][j]))
}
pub fn mscale(a: &M3, s: f64) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[i][j] * s))
}
/// The column `j` of `m`.
#[inline]
pub fn col(m: &M3, j: usize) -> V3 {
    [m[0][j], m[1][j], m[2][j]]
}

/// The rotation by `angle` about the unit `axis` (Rodrigues).
pub fn rot_axis(axis: V3, angle: f64) -> M3 {
    let (s, c) = angle.sin_cos();
    let t = 1.0 - c;
    let [x, y, z] = axis;
    [[t * x * x + c, t * x * y - s * z, t * x * z + s * y], [t * x * y + s * z, t * y * y + c, t * y * z - s * x], [t * x * z - s * y, t * y * z + s * x, t * z * z + c]]
}
pub fn rot_x(a: f64) -> M3 {
    rot_axis([1.0, 0.0, 0.0], a)
}
pub fn rot_y(a: f64) -> M3 {
    rot_axis([0.0, 1.0, 0.0], a)
}
pub fn rot_z(a: f64) -> M3 {
    rot_axis([0.0, 0.0, 1.0], a)
}
/// The rotation vector (axis times angle) of `r`: the small-angle error a solver steps along.
pub fn log_rot(r: &M3) -> V3 {
    let tr = r[0][0] + r[1][1] + r[2][2];
    let c = ((tr - 1.0) * 0.5).clamp(-1.0, 1.0);
    let angle = c.acos();
    let v = [r[2][1] - r[1][2], r[0][2] - r[2][0], r[1][0] - r[0][1]];
    if angle < 1e-9 {
        return scale(v, 0.5);
    }
    if std::f64::consts::PI - angle < 1e-6 {
        // Near a half turn: the axis from the largest diagonal entry of (R + I) / 2.
        let k = (0..3).max_by(|&a, &b| r[a][a].total_cmp(&r[b][b])).unwrap_or(0);
        let mut axis = [0.0; 3];
        for (i, a) in axis.iter_mut().enumerate() {
            *a = (r[i][k] + if i == k { 1.0 } else { 0.0 }) * 0.5;
        }
        return scale(norm(axis), angle);
    }
    scale(v, angle / (2.0 * angle.sin()))
}

/// A rigid placement: `p' = r p + t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub r: M3,
    pub t: V3,
}

impl Default for Pose {
    fn default() -> Self {
        Pose::IDENTITY
    }
}

impl Pose {
    pub const IDENTITY: Pose = Pose { r: I3, t: ZERO };

    pub fn new(r: M3, t: V3) -> Pose {
        Pose { r, t }
    }
    pub fn at(t: V3) -> Pose {
        Pose { r: I3, t }
    }
    /// `self` then `o` in `self`'s frame: `self ∘ o`.
    pub fn mul(&self, o: &Pose) -> Pose {
        Pose { r: mm(&self.r, &o.r), t: add(mv(&self.r, o.t), self.t) }
    }
    pub fn apply(&self, p: V3) -> V3 {
        add(mv(&self.r, p), self.t)
    }
    pub fn rotate(&self, v: V3) -> V3 {
        mv(&self.r, v)
    }
    pub fn inv(&self) -> Pose {
        let rt = transpose(&self.r);
        Pose { r: rt, t: scale(mv(&rt, self.t), -1.0) }
    }
    /// The frame's z axis.
    pub fn z(&self) -> V3 {
        col(&self.r, 2)
    }
    pub fn x(&self) -> V3 {
        col(&self.r, 0)
    }
}

/// A frame whose z axis is `z` (unit) and whose x axis is as near `x_hint` as z allows.
pub fn frame_z(z: V3, x_hint: V3) -> M3 {
    let z = norm(z);
    let mut x = sub(x_hint, scale(z, dot(x_hint, z)));
    if len(x) < 1e-6 {
        let other = if z[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        x = sub(other, scale(z, dot(other, z)));
    }
    let x = norm(x);
    let y = cross(z, x);
    [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]]
}

/// Solve `a x = b` for a symmetric positive definite `a` (n x n, row-major in `a`), in place:
/// `a` holds its Cholesky factor afterwards and `b` the solution. False when `a` is not
/// positive definite.
pub fn cholesky_solve(n: usize, a: &mut [f64], b: &mut [f64]) -> bool {
    if !cholesky(n, a) {
        return false;
    }
    cholesky_back(n, a, b);
    true
}

/// `a` (n x n SPD) into its lower Cholesky factor, in place.
pub fn cholesky(n: usize, a: &mut [f64]) -> bool {
    for j in 0..n {
        let mut d = a[j * n + j];
        for k in 0..j {
            d -= a[j * n + k] * a[j * n + k];
        }
        if d <= 0.0 || !d.is_finite() {
            return false;
        }
        let d = d.sqrt();
        a[j * n + j] = d;
        for i in j + 1..n {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= a[i * n + k] * a[j * n + k];
            }
            a[i * n + j] = s / d;
        }
    }
    true
}

/// Solve with a factor from [`cholesky`].
pub fn cholesky_back(n: usize, l: &[f64], b: &mut [f64]) {
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= l[i * n + k] * b[k];
        }
        b[i] = s / l[i * n + i];
    }
    for i in (0..n).rev() {
        let mut s = b[i];
        for k in i + 1..n {
            s -= l[k * n + i] * b[k];
        }
        b[i] = s / l[i * n + i];
    }
}

/// A deterministic random stream (splitmix64): the same seed gives the same draws on every
/// machine.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9e37_79b9_7f4a_7c15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    /// Uniform in [-1, 1).
    pub fn signed(&mut self) -> f64 {
        2.0 * self.unit() - 1.0
    }
    /// Standard normal (Box-Muller, one of the pair).
    pub fn normal(&mut self) -> f64 {
        let u = self.unit().max(1e-300);
        let v = self.unit();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
    /// A child stream: its draws do not depend on how many the parent makes later.
    pub fn fork(&mut self, salt: u64) -> Rng {
        Rng::new(self.next_u64() ^ salt.wrapping_mul(0xd6e8_feb8_6659_fd93))
    }
}
