//! A part as an exact distance query on the plate.

use makepad_csg_math::Vec3d;
use makepad_csg_sdf::mesh_field::{MeshData, MeshField};
use makepad_csg_sdf::lipschitz::Clearance;
use makepad_csg_sdf::{Sdf3, SdfShape};
use std::sync::Arc;

pub type Affine = [[f64; 4]; 3];

/// The solid in the plate's frame (mm).
pub struct Solid {
    field: Arc<dyn Sdf3 + Send + Sync>,
    /// The mesh on the plate, when the part is only that: a grid is drawn from its triangles.
    pub mesh: Option<Arc<MeshField>>,
    to_field: Affine,
    /// Plate mm per unit of the field.
    scale: f64,
    /// A box holding the solid.
    pub lo: [f64; 3],
    pub hi: [f64; 3],
    /// What its values prove of how far the surface is.
    pub clearance: Clearance,
}

fn apply(m: &Affine, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

/// The inverse of a rigid map with a uniform scale.
fn inverse(m: &Affine) -> (Affine, f64) {
    let scale = (m[0][0] * m[0][0] + m[1][0] * m[1][0] + m[2][0] * m[2][0]).sqrt();
    let k = 1.0 / (scale * scale);
    let r: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| m[j][i] * k));
    let t = [m[0][3], m[1][3], m[2][3]];
    let row = |i: usize| [r[i][0], r[i][1], r[i][2], -(r[i][0] * t[0] + r[i][1] * t[1] + r[i][2] * t[2])];
    ([row(0), row(1), row(2)], scale)
}

impl Solid {
    /// A part that is one distance field: `shape` under `to_plate` (rigid, or with a uniform
    /// scale). `bounds`: its box on the plate, if known.
    pub fn of_field(shape: &SdfShape, to_plate: Affine, bounds: Option<([f64; 3], [f64; 3])>) -> Solid {
        let (lo, hi) = bounds.unwrap_or_else(|| {
            let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
            for c in 0..8 {
                let p = apply(&to_plate, std::array::from_fn(|k| if c & (1 << k) == 0 { shape.min[k] } else { shape.max[k] }));
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            (lo, hi)
        });
        let (to_field, scale) = inverse(&to_plate);
        Solid { field: shape.field(), mesh: None, to_field, scale, lo, hi, clearance: shape.clearance_anywhere().scaled(scale) }
    }

    /// A solid given by its distance on the plate.
    pub fn of_distance(field: Arc<dyn Sdf3 + Send + Sync>, lo: [f64; 3], hi: [f64; 3]) -> Solid {
        Solid { field, mesh: None, to_field: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], scale: 1.0, lo, hi, clearance: Clearance::uniform(1.0) }
    }

    /// A part that has only a mesh, on the plate.
    pub fn of_mesh(positions: Vec<[f64; 3]>, triangles: Vec<[u32; 3]>) -> Solid {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for p in &positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let positions = positions.iter().map(|p| p.map(|v| v as f32)).collect();
        let mesh = Arc::new(MeshField::new(MeshData { positions, triangles }));
        Solid { mesh: Some(mesh.clone()), ..Solid::of_distance(Arc::new(RowOfPoints(mesh)), lo, hi) }
    }

    /// The field's value at each point, in plate mm: below zero inside, as `clearance` proves it
    /// of the distance.
    pub fn values(&self, at: &[[f64; 3]], out: &mut [f64]) {
        if at.is_empty() {
            return;
        }
        let ps: Vec<Vec3d> = at.iter().map(|&p| {
            let q = apply(&self.to_field, p);
            Vec3d::new(q[0], q[1], q[2])
        }).collect();
        self.field.distance_many(&ps, out);
        for v in out.iter_mut() {
            *v *= self.scale;
        }
    }

    /// [`Solid::values`] at points in runs of `per`, each run close together (a brick's points):
    /// a mesh reads a run as one ([`MeshField::signed_in_box`]), the same values.
    pub fn values_in_runs(&self, at: &[[f64; 3]], per: usize, out: &mut [f64]) {
        let Some(mesh) = &self.mesh else { return self.values(at, out) };
        for (run, out) in at.chunks(per).zip(out.chunks_mut(per)) {
            let ps: Vec<[f32; 3]> = run.iter().map(|p| p.map(|v| v as f32)).collect();
            for (o, d) in out.iter_mut().zip(mesh.signed_in_box(&ps)) {
                *o = d as f64;
            }
        }
    }
}

/// A mesh read at runs of nearby points, as a grid's rows and a piece's balls are: each point's
/// sign and search come from the point before it, against tens of microseconds alone.
struct RowOfPoints(Arc<MeshField>);

impl Sdf3 for RowOfPoints {
    fn distance(&self, p: Vec3d) -> f64 {
        self.0.distance(p)
    }
    fn distance_many(&self, ps: &[Vec3d], out: &mut [f64]) {
        let at: Vec<[f32; 3]> = ps.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect();
        for (o, d) in out.iter_mut().zip(self.0.signed_many(&at)) {
            *o = d as f64;
        }
    }
}
