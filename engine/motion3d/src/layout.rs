//! Instance layouts: where the copies of an `Instances{...}` node (and the
//! emitter points of particles) start, before any per-instance animation.
//!
//! Every layout is centred on the origin, y up, and deterministic: the
//! random ones take an explicit `seed`, and every instance gets four
//! per-instance randoms in `rand` (0..1) that stay the same for the same
//! `(seed, index)` whatever the count, so an author can key "each instance a
//! little different" without the look jumping when the count changes.

use makepad_geom3d::math::{rand01, Rng};
use makepad_geom3d::mesh::{add3, cross3, len3, normalize_or_up, scale3, sub3, tri_normal_unnormalized, Mesh};
use std::f32::consts::TAU;

/// The golden angle, radians (phyllotaxis and fibonacci spheres).
pub const GOLDEN_ANGLE: f32 = 2.399_963_2;

/// Seed of `rand` for layouts that have none of their own.
const DEFAULT_SEED: u64 = 0x5EED_1A70_u64;

/// One instance's starting point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstanceBase {
    /// World units, relative to the Instances node.
    pub position: [f32; 3],
    /// Unit: outward for circles/spheres/surfaces, +y (up) for grids, lines
    /// and random boxes.
    pub normal: [f32; 3],
    /// 0..count-1, in layout order.
    pub index: u32,
    pub count: u32,
    /// Order along the layout, 0..1 (`index / (count - 1)`; 0 for one
    /// instance) — what staggers and gradients key on.
    pub u: f32,
    /// Four deterministic per-instance randoms in 0..1.
    pub rand: [f32; 4],
}

/// How instances are laid out. Counts are clamped to 1..=1_000_000.
#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    /// A centred lattice of `count[0] x count[1] x count[2]` cells `spacing`
    /// apart (x fastest, then y, then z).
    Grid { count: [u32; 3], spacing: [f32; 3] },
    /// `count` points evenly from `start` to `end` (both included).
    Line { count: u32, start: [f32; 3], end: [f32; 3] },
    /// `count` points on a circle of `radius` in the XZ plane, starting at +x
    /// and turning toward -z (counter-clockwise seen from above).
    Circle { count: u32, radius: f32 },
    /// `count` points evenly on a sphere of `radius` (fibonacci lattice,
    /// from the top down).
    Sphere { count: u32, radius: f32 },
    /// `count` points uniformly random in a box of `size` (centred).
    Cube { count: u32, size: [f32; 3], seed: u64 },
    /// `count` points uniformly random in a ball of `radius`, or on its
    /// surface with `surface: true`.
    Ball { count: u32, radius: f32, seed: u64, surface: bool },
    /// A conical spiral: radius grows 0..`radius` over `turns` turns while y
    /// rises from -height/2 to +height/2 (height 0 = flat).
    Spiral { count: u32, radius: f32, turns: f32, height: f32 },
    /// A helix of constant `radius`, `turns` turns, y from -height/2 to
    /// +height/2.
    Helix { count: u32, radius: f32, turns: f32, height: f32 },
    /// Sunflower seeds in the XZ plane: point i at radius `spacing·√i`,
    /// angle `i·golden angle`.
    Phyllotaxis { count: u32, spacing: f32 },
    /// `count` points on the surface of the supplied mesh, area-weighted,
    /// normals interpolated. Needs `generate(Some(mesh))`; empty without.
    Surface { count: u32, seed: u64 },
    /// Explicit points (normal +y).
    Points(Vec<[f32; 3]>),
}

impl Layout {
    /// Layout names as authored (`Grid{...}`, `Sphere{...}`, ...), for messages.
    pub const NAMES: &'static [&'static str] =
        &["Grid", "Line", "Circle", "Sphere", "Cube", "Ball", "Spiral", "Helix", "Phyllotaxis", "Surface", "Points"];

    /// How many instances the layout makes (Surface: `count`, even without a mesh).
    pub fn count(&self) -> usize {
        let c = |n: u32| n.clamp(1, 1_000_000) as usize;
        match self {
            Layout::Grid { count, .. } => c(count[0]) * c(count[1]) * c(count[2]),
            Layout::Line { count, .. }
            | Layout::Circle { count, .. }
            | Layout::Sphere { count, .. }
            | Layout::Cube { count, .. }
            | Layout::Ball { count, .. }
            | Layout::Spiral { count, .. }
            | Layout::Helix { count, .. }
            | Layout::Phyllotaxis { count, .. }
            | Layout::Surface { count, .. } => c(*count),
            Layout::Points(p) => p.len(),
        }
    }

    fn seed(&self) -> u64 {
        match self {
            Layout::Cube { seed, .. } | Layout::Ball { seed, .. } | Layout::Surface { seed, .. } => *seed,
            _ => DEFAULT_SEED,
        }
    }

    /// The instances. `mesh` is only read by [`Layout::Surface`].
    pub fn generate(&self, mesh: Option<&Mesh>) -> Vec<InstanceBase> {
        let up = [0.0, 1.0, 0.0];
        let n = self.count();
        let mut points: Vec<([f32; 3], [f32; 3])> = Vec::with_capacity(n);
        let frac = |i: usize| if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 };
        match self {
            Layout::Grid { count, spacing } => {
                let c = [count[0].clamp(1, 1_000_000), count[1].clamp(1, 1_000_000), count[2].clamp(1, 1_000_000)];
                let off = |k: usize, i: u32| (i as f32 - (c[k] - 1) as f32 * 0.5) * spacing[k];
                for z in 0..c[2] {
                    for y in 0..c[1] {
                        for x in 0..c[0] {
                            points.push(([off(0, x), off(1, y), off(2, z)], up));
                        }
                    }
                }
            }
            Layout::Line { start, end, .. } => {
                for i in 0..n {
                    points.push((add3(*start, scale3(sub3(*end, *start), frac(i))), up));
                }
            }
            Layout::Circle { radius, .. } => {
                for i in 0..n {
                    let a = TAU * i as f32 / n as f32;
                    let d = [a.cos(), 0.0, -a.sin()];
                    points.push((scale3(d, *radius), d));
                }
            }
            Layout::Sphere { radius, .. } => {
                for i in 0..n {
                    let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
                    let r = (1.0 - y * y).max(0.0).sqrt();
                    let a = i as f32 * GOLDEN_ANGLE;
                    let d = [r * a.cos(), y, -r * a.sin()];
                    points.push((scale3(d, *radius), d));
                }
            }
            Layout::Cube { size, seed, .. } => {
                let mut rng = Rng::new(*seed);
                for _ in 0..n {
                    let p = [rng.range(-0.5, 0.5) * size[0], rng.range(-0.5, 0.5) * size[1], rng.range(-0.5, 0.5) * size[2]];
                    points.push((p, up));
                }
            }
            Layout::Ball { radius, seed, surface, .. } => {
                let mut rng = Rng::new(*seed);
                for _ in 0..n {
                    let p = if *surface { rng.on_sphere() } else { rng.in_sphere() };
                    points.push((scale3(p, *radius), normalize_or_up(p)));
                }
            }
            Layout::Spiral { radius, turns, height, .. } | Layout::Helix { radius, turns, height, .. } => {
                let spiral = matches!(self, Layout::Spiral { .. });
                for i in 0..n {
                    let u = frac(i);
                    let a = TAU * turns * u;
                    let d = [a.cos(), 0.0, -a.sin()];
                    let r = if spiral { radius * u } else { *radius };
                    let p = [d[0] * r, height * (u - 0.5), d[2] * r];
                    points.push((p, d));
                }
            }
            Layout::Phyllotaxis { spacing, .. } => {
                for i in 0..n {
                    let a = i as f32 * GOLDEN_ANGLE;
                    let r = spacing * (i as f32).sqrt();
                    points.push(([r * a.cos(), 0.0, -r * a.sin()], up));
                }
            }
            Layout::Surface { seed, .. } => {
                let Some(mesh) = mesh.filter(|m| !m.is_empty()) else { return Vec::new() };
                points = sample_surface(mesh, n, *seed);
            }
            Layout::Points(p) => points.extend(p.iter().map(|p| (*p, up))),
        }
        let seed = self.seed();
        points
            .into_iter()
            .enumerate()
            .map(|(i, (position, normal))| InstanceBase {
                position,
                normal,
                index: i as u32,
                count: n as u32,
                u: frac(i),
                rand: instance_rand(seed, i as u64),
            })
            .collect()
    }
}

/// The four per-instance randoms of instance `index` (0..1).
/// `index` may be a hash (eval.rs's per-cell picks): its four draws are taken mod 2^64.
pub fn instance_rand(seed: u64, index: u64) -> [f32; 4] {
    let s = seed ^ 0xA5A5_0F0F_1234_5678;
    let at = |k: u64| rand01(s, index.wrapping_mul(4).wrapping_add(k));
    [at(0), at(1), at(2), at(3)]
}

/// `count` area-weighted points on `mesh`'s triangles, with interpolated
/// (or face) normals.
pub fn sample_surface(mesh: &Mesh, count: usize, seed: u64) -> Vec<([f32; 3], [f32; 3])> {
    let tris: Vec<[usize; 3]> = mesh.indices.chunks_exact(3).map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
    let mut cdf = Vec::with_capacity(tris.len());
    let mut total = 0.0f64;
    for t in &tris {
        let n = tri_normal_unnormalized(mesh.positions[t[0]], mesh.positions[t[1]], mesh.positions[t[2]]);
        total += len3(n) as f64 * 0.5;
        cdf.push(total);
    }
    if tris.is_empty() || total <= 0.0 {
        return Vec::new();
    }
    let mut rng = Rng::new(seed);
    let has_normals = mesh.normals.len() == mesh.positions.len();
    (0..count)
        .map(|_| {
            let pick = rng.f32() as f64 * total;
            let ti = cdf.partition_point(|&c| c < pick).min(tris.len() - 1);
            let t = tris[ti];
            let (r1, r2) = (rng.f32().sqrt(), rng.f32());
            let (a, b, c) = (1.0 - r1, r1 * (1.0 - r2), r1 * r2);
            let (p0, p1, p2) = (mesh.positions[t[0]], mesh.positions[t[1]], mesh.positions[t[2]]);
            let p = add3(add3(scale3(p0, a), scale3(p1, b)), scale3(p2, c));
            let n = if has_normals {
                let (n0, n1, n2) = (mesh.normals[t[0]], mesh.normals[t[1]], mesh.normals[t[2]]);
                normalize_or_up(add3(add3(scale3(n0, a), scale3(n1, b)), scale3(n2, c)))
            } else {
                normalize_or_up(cross3(sub3(p1, p0), sub3(p2, p0)))
            };
            (p, n)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_geom3d::mesh::dot3;

    fn centroid(v: &[InstanceBase]) -> [f32; 3] {
        let mut c = [0.0; 3];
        for i in v {
            c = add3(c, i.position);
        }
        scale3(c, 1.0 / v.len() as f32)
    }

    #[test]
    fn counts_and_order() {
        let layouts = [
            (Layout::Grid { count: [4, 3, 2], spacing: [1.0, 1.0, 1.0] }, 24),
            (Layout::Line { count: 5, start: [0.0; 3], end: [4.0, 0.0, 0.0] }, 5),
            (Layout::Circle { count: 12, radius: 2.0 }, 12),
            (Layout::Sphere { count: 500, radius: 1.0 }, 500),
            (Layout::Cube { count: 100, size: [2.0, 2.0, 2.0], seed: 1 }, 100),
            (Layout::Ball { count: 100, radius: 1.0, seed: 1, surface: true }, 100),
            (Layout::Spiral { count: 50, radius: 3.0, turns: 4.0, height: 0.0 }, 50),
            (Layout::Helix { count: 50, radius: 1.0, turns: 3.0, height: 2.0 }, 50),
            (Layout::Phyllotaxis { count: 300, spacing: 0.1 }, 300),
            (Layout::Points(vec![[1.0, 2.0, 3.0]]), 1),
        ];
        for (l, n) in layouts {
            let v = l.generate(None);
            assert_eq!(v.len(), n, "{l:?}");
            assert_eq!(l.count(), n);
            assert!(v.iter().enumerate().all(|(i, b)| b.index == i as u32 && b.count == n as u32));
            if n > 1 {
                assert_eq!(v[0].u, 0.0);
                assert_eq!(v[n - 1].u, 1.0);
            }
            assert!(v.iter().all(|b| (len3(b.normal) - 1.0).abs() < 1e-4));
            assert_eq!(v, l.generate(None), "deterministic");
        }
    }

    #[test]
    fn centred_layouts() {
        for l in [
            Layout::Grid { count: [5, 4, 3], spacing: [0.5, 1.0, 2.0] },
            Layout::Circle { count: 16, radius: 3.0 },
            Layout::Sphere { count: 400, radius: 2.0 },
            Layout::Helix { count: 101, radius: 1.0, turns: 4.0, height: 2.0 },
        ] {
            let c = centroid(&l.generate(None));
            assert!(len3(c) < 0.05, "{l:?} centroid {c:?}");
        }
        let g = Layout::Grid { count: [3, 1, 1], spacing: [2.0, 1.0, 1.0] }.generate(None);
        assert_eq!(g[0].position, [-2.0, 0.0, 0.0]);
        assert_eq!(g[2].position, [2.0, 0.0, 0.0]);
    }

    #[test]
    fn sphere_and_ball_shapes() {
        for b in (Layout::Sphere { count: 200, radius: 2.0 }).generate(None) {
            assert!((len3(b.position) - 2.0).abs() < 1e-4);
            assert!(dot3(b.normal, b.position) > 0.0);
        }
        for b in (Layout::Ball { count: 200, radius: 2.0, seed: 4, surface: false }).generate(None) {
            assert!(len3(b.position) <= 2.0 + 1e-4);
        }
    }

    #[test]
    fn randoms_depend_on_seed_not_count() {
        let a = Layout::Cube { count: 10, size: [1.0; 3], seed: 7 }.generate(None);
        let b = Layout::Cube { count: 20, size: [1.0; 3], seed: 7 }.generate(None);
        let c = Layout::Cube { count: 10, size: [1.0; 3], seed: 8 }.generate(None);
        assert_eq!(a[3].rand, b[3].rand);
        assert_eq!(a[3].position, b[3].position);
        assert_ne!(a[3].rand, c[3].rand);
        assert!(a.iter().all(|i| i.rand.iter().all(|r| (0.0..1.0).contains(r))));
    }

    #[test]
    fn surface_points_lie_on_the_mesh() {
        // A unit quad in the z = 0 plane plus one in the x = 1 plane.
        let mut m = Mesh::new();
        let q = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
        let base = q.iter().map(|p| m.vertex(*p, [0.0, 0.0, 1.0], [0.0, 0.0])).collect::<Vec<_>>();
        m.quad(base[0], base[1], base[2], base[3]);
        let q2 = [[1.0, 0.0, 0.0], [1.0, 0.0, -2.0], [1.0, 1.0, -2.0], [1.0, 1.0, 0.0]];
        let b2 = q2.iter().map(|p| m.vertex(*p, [1.0, 0.0, 0.0], [0.0, 0.0])).collect::<Vec<_>>();
        m.quad(b2[0], b2[1], b2[2], b2[3]);
        let l = Layout::Surface { count: 3000, seed: 3 };
        assert!(l.generate(None).is_empty());
        let v = l.generate(Some(&m));
        assert_eq!(v.len(), 3000);
        let mut on_side = 0;
        for b in &v {
            let p = b.position;
            let on_front = p[2].abs() < 1e-5 && (-1e-5..=1.0 + 1e-5).contains(&p[0]) && (-1e-5..=1.0 + 1e-5).contains(&p[1]);
            let on_right = (p[0] - 1.0).abs() < 1e-5 && (-2.0 - 1e-5..=1e-5).contains(&p[2]) && (-1e-5..=1.0 + 1e-5).contains(&p[1]);
            assert!(on_front || on_right, "{p:?}");
            if on_right && !on_front {
                on_side += 1;
                assert!((b.normal[0] - 1.0).abs() < 1e-4);
            }
        }
        // The side quad has twice the area: about 2/3 of the points.
        let f = on_side as f32 / 3000.0;
        assert!((f - 2.0 / 3.0).abs() < 0.05, "{f}");
        assert_eq!(v, l.generate(Some(&m)));
    }
}
