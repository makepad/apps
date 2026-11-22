//! Geometry queries shared by picking, navigation, tools and culling:
//! rays, hits, frustums, snap results, measurement math.

use crate::model::ids::ElementId;
use makepad_math::{Aabb, Mat4f, Plane, Vec3f, Vec4f};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    pub origin: Vec3f,
    /// Unit length.
    pub dir: Vec3f,
}

impl Ray {
    pub fn new(origin: Vec3f, dir: Vec3f) -> Self {
        Ray {
            origin,
            dir: dir.normalize(),
        }
    }

    pub fn at(&self, t: f32) -> Vec3f {
        self.origin + self.dir * t
    }

    /// Slab test. Returns the entry distance, or `None` when missed.
    pub fn intersect_aabb(&self, b: &Aabb) -> Option<f32> {
        let mut tmin = f32::NEG_INFINITY;
        let mut tmax = f32::INFINITY;
        let o = [self.origin.x, self.origin.y, self.origin.z];
        let d = [self.dir.x, self.dir.y, self.dir.z];
        let mn = [b.min.x, b.min.y, b.min.z];
        let mx = [b.max.x, b.max.y, b.max.z];
        for i in 0..3 {
            if d[i].abs() < 1e-12 {
                if o[i] < mn[i] || o[i] > mx[i] {
                    return None;
                }
            } else {
                let inv = 1.0 / d[i];
                let mut t0 = (mn[i] - o[i]) * inv;
                let mut t1 = (mx[i] - o[i]) * inv;
                if t0 > t1 {
                    std::mem::swap(&mut t0, &mut t1);
                }
                tmin = tmin.max(t0);
                tmax = tmax.min(t1);
                if tmin > tmax {
                    return None;
                }
            }
        }
        if tmax < 0.0 {
            return None;
        }
        Some(tmin.max(0.0))
    }

    /// Möller–Trumbore. Returns `(t, u, v)` for a front- or back-facing hit.
    pub fn intersect_triangle(&self, a: Vec3f, b: Vec3f, c: Vec3f) -> Option<(f32, f32, f32)> {
        let e1 = b - a;
        let e2 = c - a;
        let p = Vec3f::cross(self.dir, e2);
        let det = e1.dot(p);
        if det.abs() < 1e-9 {
            return None;
        }
        let inv = 1.0 / det;
        let s = self.origin - a;
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = Vec3f::cross(s, e1);
        let v = self.dir.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = e2.dot(q) * inv;
        if t <= 0.0 {
            return None;
        }
        Some((t, u, v))
    }

    /// Ray vs. plane `a*x+b*y+c*z+d = 0`.
    pub fn intersect_plane(&self, plane: &Plane) -> Option<f32> {
        let n = Vec3f {
            x: plane.a,
            y: plane.b,
            z: plane.c,
        };
        let denom = n.dot(self.dir);
        if denom.abs() < 1e-9 {
            return None;
        }
        let t = -(n.dot(self.origin) + plane.d) / denom;
        if t < 0.0 {
            None
        } else {
            Some(t)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub element: ElementId,
    /// Index into `Scene::batches` (`u32::MAX`, with `triangle`: a hit on a part's
    /// exact field, which the SDF engine draws, not on a triangle).
    pub batch: u32,
    /// Triangle number inside that batch.
    pub triangle: u32,
    pub t: f32,
    pub point: Vec3f,
    /// Geometric (flat) normal, unit, facing the ray origin.
    pub normal: Vec3f,
    /// Barycentric `(u, v)` of the hit inside the triangle.
    pub bary: [f32; 2],
}

/// Six planes, normals pointing inward. Built from a clip-space matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum {
    pub planes: [Plane; 6],
}

impl Frustum {
    /// Gribb–Hartmann extraction from a column-major view-projection matrix
    /// (`makepad_math::Mat4f` layout).
    pub fn from_view_proj(m: &Mat4f) -> Self {
        let v = &m.v;
        // row i of the matrix: elements v[i], v[i+4], v[i+8], v[i+12]
        let row = |i: usize| Vec4f {
            x: v[i],
            y: v[i + 4],
            z: v[i + 8],
            w: v[i + 12],
        };
        let r0 = row(0);
        let r1 = row(1);
        let r2 = row(2);
        let r3 = row(3);
        let mk = |p: Vec4f| {
            let n = Vec3f {
                x: p.x,
                y: p.y,
                z: p.z,
            };
            let l = n.length().max(1e-12);
            Plane {
                a: p.x / l,
                b: p.y / l,
                c: p.z / l,
                d: p.w / l,
            }
        };
        let add = |a: Vec4f, b: Vec4f| Vec4f {
            x: a.x + b.x,
            y: a.y + b.y,
            z: a.z + b.z,
            w: a.w + b.w,
        };
        let sub = |a: Vec4f, b: Vec4f| Vec4f {
            x: a.x - b.x,
            y: a.y - b.y,
            z: a.z - b.z,
            w: a.w - b.w,
        };
        Frustum {
            planes: [
                mk(add(r3, r0)), // left
                mk(sub(r3, r0)), // right
                mk(add(r3, r1)), // bottom
                mk(sub(r3, r1)), // top
                mk(add(r3, r2)), // near
                mk(sub(r3, r2)), // far
            ],
        }
    }

    /// Conservative box test: true when the box is not fully outside any plane.
    pub fn intersects_aabb(&self, b: &Aabb) -> bool {
        for p in &self.planes {
            let px = if p.a >= 0.0 { b.max.x } else { b.min.x };
            let py = if p.b >= 0.0 { b.max.y } else { b.min.y };
            let pz = if p.c >= 0.0 { b.max.z } else { b.min.z };
            if p.a * px + p.b * py + p.c * pz + p.d < 0.0 {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapKind {
    Vertex,
    EdgeMidpoint,
    Edge,
    Face,
    /// Intersection with the ground plane (Z = 0) when nothing else is hit.
    Ground,
}

impl SnapKind {
    /// Lower wins when two candidates are both inside the snap radius: a
    /// corner beats an edge beats the face it lies on, which is what a
    /// draughtsman expects.
    pub fn priority(self) -> u8 {
        match self {
            SnapKind::Vertex => 0,
            SnapKind::EdgeMidpoint => 1,
            SnapKind::Edge => 2,
            SnapKind::Face => 3,
            SnapKind::Ground => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SnapKind::Vertex => "Vertex",
            SnapKind::EdgeMidpoint => "Midpoint",
            SnapKind::Edge => "Edge",
            SnapKind::Face => "Face",
            SnapKind::Ground => "Ground",
        }
    }
}


#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapHit {
    pub kind: SnapKind,
    pub point: Vec3f,
    pub element: Option<ElementId>,
    /// Unit normal of the snapped face, when there is one.
    pub normal: Option<Vec3f>,
    /// Distance from the cursor in screen pixels.
    pub screen_dist: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapOptions {
    pub vertex: bool,
    pub edge_midpoint: bool,
    pub edge: bool,
    pub face: bool,
    /// Snap search radius in screen pixels.
    pub radius_px: f32,
}

impl Default for SnapOptions {
    fn default() -> Self {
        SnapOptions {
            vertex: true,
            edge_midpoint: true,
            edge: true,
            face: true,
            radius_px: 12.0,
        }
    }
}

/// What a measurement is measuring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MeasureKind {
    #[default]
    Distance,
    Area,
    Angle,
    EdgeLength,
    Radius,
    FaceDistance,
}

/// Straight-line distance.
pub fn distance(a: Vec3f, b: Vec3f) -> f32 {
    (b - a).length()
}

/// Area of a planar polygon (Newell's method). Works for any orientation.
pub fn polygon_area(points: &[Vec3f]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut n = Vec3f::default();
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        n += Vec3f::cross(a, b);
    }
    n.length() * 0.5
}

/// Angle at `vertex` between `a` and `b`, in degrees.
pub fn angle_deg(a: Vec3f, vertex: Vec3f, b: Vec3f) -> f32 {
    let u = (a - vertex).normalize();
    let v = (b - vertex).normalize();
    u.dot(v).clamp(-1.0, 1.0).acos().to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_hits_unit_box() {
        let b = Aabb {
            min: Vec3f {
                x: -1.0,
                y: -1.0,
                z: -1.0,
            },
            max: Vec3f {
                x: 1.0,
                y: 1.0,
                z: 1.0,
            },
        };
        let r = Ray::new(
            Vec3f {
                x: 0.0,
                y: 0.0,
                z: 5.0,
            },
            Vec3f {
                x: 0.0,
                y: 0.0,
                z: -1.0,
            },
        );
        assert!((r.intersect_aabb(&b).unwrap() - 4.0).abs() < 1e-5);
    }

    #[test]
    fn square_area_and_right_angle() {
        let p = [
            Vec3f {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Vec3f {
                x: 2.0,
                y: 0.0,
                z: 0.0,
            },
            Vec3f {
                x: 2.0,
                y: 2.0,
                z: 0.0,
            },
            Vec3f {
                x: 0.0,
                y: 2.0,
                z: 0.0,
            },
        ];
        assert!((polygon_area(&p) - 4.0).abs() < 1e-5);
        assert!((angle_deg(p[1], p[0], p[3]) - 90.0).abs() < 1e-3);
    }
}
