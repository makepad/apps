//! The trilinear hexahedron on a cubic cell of side `h`: eight nodes at the corners (x
//! fastest, `corner(i)`), integrated with the 2x2x2 Gauss rule. Gauss point `g` lies in the
//! cell's octant `g` (the same corner bits), so a cell cut by the surface weights each point
//! by the filled share of its own octant: the finite cell method's quadrature on one level of
//! octree subdivision (Parvizian, Duester and Rank, "Finite cell method", Computational
//! Mechanics 41, 2007).
//!
//! Nothing is assembled: the solver applies each cell's stiffness on the fly (strain at each
//! point from the eight nodal displacements, stress by the point's material, nodal forces back
//! by the same gradients), the CPU here and the GPU kernels the same way.
//!
//! Voigt order everywhere: xx, yy, zz, yz, xz, xy, with engineering shear strains.

pub type V3 = [f64; 3];
pub type Mat6 = [[f64; 6]; 6];

/// The Gauss abscissa, 1/sqrt(3).
pub const GP: f64 = 0.577_350_269_189_625_8;

/// Corner `i` of a cell (0 or 1 per axis, x fastest).
pub const fn corner(i: usize) -> [usize; 3] {
    [i & 1, (i >> 1) & 1, (i >> 2) & 1]
}

fn sign(c: usize) -> f64 {
    if c == 0 {
        -1.0
    } else {
        1.0
    }
}

/// The shape functions' gradients of a unit cube at natural point `xi` (each in -1..1):
/// `[node][axis]`. A cell of side `h` divides them by `h`.
pub fn gradients_at(xi: V3) -> [V3; 8] {
    std::array::from_fn(|i| {
        let c = corner(i);
        let s = [sign(c[0]), sign(c[1]), sign(c[2])];
        let f = |k: usize| 1.0 + s[k] * xi[k];
        // dN/dxi = s (1 + s eta)(1 + s zeta) / 8, and dxi/dx = 2 on the unit cube
        [s[0] * f(1) * f(2) / 4.0, s[1] * f(0) * f(2) / 4.0, s[2] * f(0) * f(1) / 4.0]
    })
}

/// [`gradients_at`] at each Gauss point: `[point][node][axis]`.
pub fn gauss_gradients() -> [[V3; 8]; 8] {
    std::array::from_fn(|g| {
        let c = corner(g);
        gradients_at([sign(c[0]) * GP, sign(c[1]) * GP, sign(c[2]) * GP])
    })
}

/// The shape functions at natural point `xi`.
pub fn shape_at(xi: V3) -> [f64; 8] {
    std::array::from_fn(|i| {
        let c = corner(i);
        (0..3).map(|k| 0.5 * (1.0 + sign(c[k]) * xi[k])).product()
    })
}

/// The strain (Voigt) of nodal displacements `u` with node gradients `dn` (already divided by `h`).
pub fn strain(u: &[V3; 8], dn: &[V3; 8]) -> [f64; 6] {
    let mut g = [[0.0; 3]; 3];
    for i in 0..8 {
        for a in 0..3 {
            for b in 0..3 {
                g[a][b] += u[i][a] * dn[i][b];
            }
        }
    }
    [g[0][0], g[1][1], g[2][2], g[1][2] + g[2][1], g[0][2] + g[2][0], g[0][1] + g[1][0]]
}

/// The force stress `s` (Voigt) puts on a node with gradient `d`: s . d.
pub fn traction(s: &[f64; 6], d: V3) -> V3 {
    [
        s[0] * d[0] + s[5] * d[1] + s[4] * d[2],
        s[5] * d[0] + s[1] * d[1] + s[3] * d[2],
        s[4] * d[0] + s[3] * d[1] + s[2] * d[2],
    ]
}

pub fn mul6(d: &Mat6, e: &[f64; 6]) -> [f64; 6] {
    std::array::from_fn(|r| (0..6).map(|c| d[r][c] * e[c]).sum())
}

/// The stiffness block a Gauss point (`d` its material, `w` its weight) adds between a node
/// and itself: column `a` is the force the node's unit displacement along `a` makes.
pub fn self_block(d: &Mat6, dn: V3, w: f64) -> [[f64; 3]; 3] {
    let mut k = [[0.0; 3]; 3];
    for a in 0..3 {
        let mut u = [[0.0; 3]; 8];
        u[0][a] = 1.0;
        let mut one = [[0.0; 3]; 8];
        one[0] = dn;
        let s = mul6(d, &strain(&u, &one));
        let f = traction(&s, dn);
        for b in 0..3 {
            k[b][a] += w * f[b];
        }
    }
    k
}

/// Von Mises equivalent of a stress (Voigt).
pub fn von_mises(s: &[f64; 6]) -> f64 {
    let (a, b, c) = (s[0] - s[1], s[1] - s[2], s[2] - s[0]);
    (0.5 * (a * a + b * b + c * c) + 3.0 * (s[3] * s[3] + s[4] * s[4] + s[5] * s[5])).sqrt()
}

/// The principal stresses (largest first) of a stress (Voigt): the eigenvalues of the
/// symmetric tensor in closed form (O. K. Smith, "Eigenvalues of a symmetric 3x3 matrix",
/// Communications of the ACM 4(4), 1961).
pub fn principal(s: &[f64; 6]) -> [f64; 3] {
    let p1 = s[3] * s[3] + s[4] * s[4] + s[5] * s[5];
    let q = (s[0] + s[1] + s[2]) / 3.0;
    let p2 = (s[0] - q).powi(2) + (s[1] - q).powi(2) + (s[2] - q).powi(2) + 2.0 * p1;
    let p = (p2 / 6.0).sqrt();
    if p < 1e-300 {
        return [q; 3];
    }
    let b = [(s[0] - q) / p, (s[1] - q) / p, (s[2] - q) / p, s[3] / p, s[4] / p, s[5] / p];
    // det of B = [[b0 b5 b4] [b5 b1 b3] [b4 b3 b2]]
    let det = b[0] * (b[1] * b[2] - b[3] * b[3]) - b[5] * (b[5] * b[2] - b[3] * b[4]) + b[4] * (b[5] * b[3] - b[1] * b[4]);
    let r = (det / 2.0).clamp(-1.0, 1.0);
    let phi = r.acos() / 3.0;
    let e1 = q + 2.0 * p * phi.cos();
    let e3 = q + 2.0 * p * (phi + 2.0 * std::f64::consts::PI / 3.0).cos();
    [e1, 3.0 * q - e1 - e3, e3]
}

/// The inverse of a symmetric positive definite 3x3 block (None when singular).
pub fn invert3(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let c = |r: usize, k: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (k1, k2) = ((k + 1) % 3, (k + 2) % 3);
        m[r1][k1] * m[r2][k2] - m[r1][k2] * m[r2][k1]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    if !(det.abs() > 1e-300) {
        return None;
    }
    Some(std::array::from_fn(|r| std::array::from_fn(|k| c(k, r) / det)))
}
