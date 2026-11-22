//! Test and benchmark scenes: a handful of boxes ([`parts`]) for the
//! geometry, picking, snapping and measuring tests, and a large instanced
//! model ([`synthetic_model`]) for the performance gates. Metres, Z up,
//! right-handed — what every scene build normalises to. The boxes are
//! metre-sized so the tests' tolerances are the ones the tools promise.

use crate::model::ids::{ElementId, MaterialId, MeshId};
use crate::model::model::*;
use crate::model::units::{LengthUnit, Units};
use makepad_math::Mat4f;

struct Builder {
    model: ModelData,
}

impl Builder {
    fn material(&mut self, name: &str, rgba: [f32; 4], rough: f32) -> MaterialId {
        self.model.materials.push(MaterialData {
            name: name.into(),
            base_color: rgba,
            roughness: rough,
            transmission: if rgba[3] < 1.0 { 0.5 } else { 0.0 },
            ..Default::default()
        });
        MaterialId::from_index(self.model.materials.len() - 1)
    }

    /// Axis-aligned box mesh from `min` to `max`, flat normals, one submesh.
    fn box_mesh(&mut self, min: [f32; 3], max: [f32; 3], material: MaterialId) -> MeshId {
        let mut positions = Vec::with_capacity(24);
        let mut normals = Vec::with_capacity(24);
        let mut uvs = Vec::with_capacity(24);
        let mut indices = Vec::with_capacity(36);
        // (normal, u axis, v axis)
        let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
            ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
            ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
            ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
            ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        ];
        let c = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let h = [
            (max[0] - min[0]) * 0.5,
            (max[1] - min[1]) * 0.5,
            (max[2] - min[2]) * 0.5,
        ];
        for (n, u, v) in faces {
            let base = positions.len() as u32;
            for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let p = [
                    c[0] + (n[0] + u[0] * su + v[0] * sv) * h[0],
                    c[1] + (n[1] + u[1] * su + v[1] * sv) * h[1],
                    c[2] + (n[2] + u[2] * su + v[2] * sv) * h[2],
                ];
                positions.push(p);
                normals.push(n);
                uvs.push([(su + 1.0) * 0.5, (sv + 1.0) * 0.5]);
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        let count = indices.len() as u32;
        self.model.meshes.push(MeshData {
            positions,
            normals,
            uvs,
            indices,
            submeshes: vec![SubMesh {
                material,
                first_index: 0,
                index_count: count,
            }],
        });
        MeshId::from_index(self.model.meshes.len() - 1)
    }

    fn part(&mut self, name: &str, min: [f32; 3], max: [f32; 3], material: MaterialId) -> ElementId {
        let mesh = self.box_mesh(min, max, material);
        let id = ElementId::from_index(self.model.elements.len());
        self.model.elements.push(ElementData {
            id,
            name: name.into(),
            transform: Mat4f::default(),
            meshes: vec![MeshRef::identity(mesh)],
        });
        id
    }
}

/// Four boxes standing on z = 0: a wide base with a post on it, a block
/// beside them and a translucent cover (the one transparent material).
pub fn parts() -> ModelData {
    let mut b = Builder {
        model: ModelData {
            name: "Parts".into(),
            units: Units {
                source_to_meters: 1.0,
                display: LengthUnit::Millimeter,
                precision: 1,
            },
            ..Default::default()
        },
    };
    let grey = b.material("Grey", [0.55, 0.56, 0.58, 1.0], 0.6);
    let orange = b.material("Orange", [0.85, 0.35, 0.08, 1.0], 0.5);
    let clear = b.material("Clear", [0.80, 0.90, 0.95, 0.35], 0.0);
    b.part("base", [-3.0, -2.0, 0.0], [3.0, 2.0, 0.6], grey);
    b.part("post", [-0.5, -0.5, 0.6], [0.5, 0.5, 3.6], orange);
    b.part("block", [-6.0, -1.0, 0.0], [-4.0, 1.0, 2.0], grey);
    b.part("cover", [3.5, -1.5, 0.0], [5.5, 1.5, 0.2], clear);
    b.model
}

/// A plate of chainmail the way Mechanics shows it: `links` instances of one
/// ring mesh (flat top and bottom, ~670 triangles), each its own element, in
/// two layers whose rings overlap their neighbours' bounds, on a printer bed
/// (`fab:bed`) their bottoms rest on. Metres, a 3 mm ring of 0.8 mm bar.
pub fn mail_model(links: usize) -> ModelData {
    let mut b = Builder {
        model: ModelData {
            name: format!("Mail of {links}"),
            units: Units { source_to_meters: 1.0, display: LengthUnit::Millimeter, precision: 1 },
            ..Default::default()
        },
    };
    let steel = b.material("Filament", [0.6, 0.62, 0.65, 1.0], 0.45);
    b.part("fab:bed", [-0.02, -0.02, -0.006], [0.3, 0.3, 0.0], steel);
    let ring = MeshId::from_index(b.model.meshes.len());
    b.model.meshes.push(ring_mesh(84, 0.0022, 0.003, 0.0008));
    let side = (links as f32).sqrt().ceil().max(1.0) as usize;
    for i in 0..links {
        let (x, y) = (i % side, i / side);
        let mut t = Mat4f::default();
        t.v[12] = x as f32 * 0.0045;
        t.v[13] = y as f32 * 0.0045;
        // Alternate rings ride a layer up (0.4 mm over the lower ones' tops).
        t.v[14] = if (x + y) % 2 == 0 { 0.0 } else { 0.0012 };
        let id = ElementId::from_index(b.model.elements.len());
        b.model.elements.push(ElementData { id, name: format!("mail~{i}"), transform: t, meshes: vec![MeshRef::identity(ring)] });
    }
    b.model
}

/// A ring of rectangular section about z: radii `r0..r1`, height `h`, `seg` steps round, the
/// top and bottom flat.
fn ring_mesh(seg: usize, r0: f32, r1: f32, h: f32) -> MeshData {
    let mut m = MeshData::default();
    // Per step: four corners of the section, each face with its own normals (flat sides).
    let faces: [([usize; 2], [f32; 3]); 4] = [([0, 1], [0.0, 0.0, -1.0]), ([1, 2], [1.0, 0.0, 0.0]), ([2, 3], [0.0, 0.0, 1.0]), ([3, 0], [-1.0, 0.0, 0.0])];
    let corner = |k: usize, a: f32| -> [f32; 3] {
        let (r, z) = [(r0, 0.0), (r1, 0.0), (r1, h), (r0, h)][k];
        [r * a.cos(), r * a.sin(), z]
    };
    for (pair, n) in faces {
        let base = m.positions.len() as u32;
        for i in 0..=seg {
            let a = i as f32 / seg as f32 * std::f32::consts::TAU;
            for k in pair {
                m.positions.push(corner(k, a));
                let nn = if n[2] != 0.0 { n } else { [n[0] * a.cos(), n[0] * a.sin(), 0.0] };
                m.normals.push(nn);
            }
        }
        for i in 0..seg as u32 {
            let (a, b, c, d) = (base + 2 * i, base + 2 * i + 1, base + 2 * i + 2, base + 2 * i + 3);
            m.indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    m
}

/// The large scene the performance gates run against (5 M triangles,
/// interactive, sub-ms pick): a few thousand elements on a grid sharing a
/// handful of meshes through instance transforms.
///
/// `target_triangles` is met to within one mesh; `elements` is exact.
pub fn synthetic_model(target_triangles: usize, elements: usize) -> ModelData {
    let elements = elements.max(1);
    let per_element = (target_triangles / elements).max(2);
    // A sphere of `seg × seg` quads is `2 · seg²` triangles.
    let seg = ((per_element as f32 / 2.0).sqrt().round() as usize).max(1);
    const MESHES: usize = 8;
    const ROWS: usize = 6;

    let mut model = ModelData {
        name: format!("Synthetic {elements} × {}", 2 * seg * seg),
        units: Units {
            source_to_meters: 1.0,
            display: LengthUnit::Meter,
            precision: 2,
        },
        ..Default::default()
    };
    for m in 0..MESHES {
        model
            .meshes
            .push(sphere_mesh(seg, 0.8 + 0.05 * m as f32));
    }

    let side = (elements as f32).sqrt().ceil().max(1.0) as usize;
    for i in 0..elements {
        let (gx, gy) = ((i % side) as f32 * 3.0, (i / side) as f32 * 3.0);
        let mut t = Mat4f::default();
        t.v[12] = gx;
        t.v[13] = gy;
        t.v[14] = (i % ROWS) as f32 * 4.0;
        model.elements.push(ElementData {
            id: ElementId::from_index(i),
            name: format!("Part {i}"),
            transform: t,
            meshes: vec![MeshRef::identity(MeshId::from_index(i % MESHES))],
        });
    }
    model
}

/// UV sphere with `seg × seg` quads and per-vertex normals.
fn sphere_mesh(seg: usize, radius: f32) -> MeshData {
    let mut m = MeshData::default();
    let n = seg.max(1);
    for iv in 0..=n {
        let v = iv as f32 / n as f32;
        let phi = v * std::f32::consts::PI;
        for iu in 0..=n {
            let u = iu as f32 / n as f32;
            let theta = u * std::f32::consts::TAU;
            let d = [
                phi.sin() * theta.cos(),
                phi.sin() * theta.sin(),
                phi.cos(),
            ];
            m.positions.push([d[0] * radius, d[1] * radius, d[2] * radius]);
            m.normals.push(d);
            m.uvs.push([u, v]);
        }
    }
    let row = n + 1;
    for iv in 0..n {
        for iu in 0..n {
            let a = (iv * row + iu) as u32;
            let (b, c, d) = (a + 1, a + row as u32, a + row as u32 + 1);
            m.indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_model_hits_its_size() {
        let m = synthetic_model(120_000, 60);
        assert_eq!(m.elements.len(), 60);
        let tris: usize = m
            .elements
            .iter()
            .flat_map(|e| e.meshes.iter())
            .map(|r| m.meshes[r.mesh.index()].indices.len() / 3)
            .sum();
        // Within one mesh of the target.
        assert!(tris > 100_000 && tris < 140_000, "{tris}");
        for mesh in &m.meshes {
            assert_eq!(mesh.normals.len(), mesh.positions.len());
            assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.positions.len()));
        }
    }

    #[test]
    fn the_parts_are_well_formed() {
        let m = parts();
        assert_eq!(m.elements.len(), 4);
        for e in &m.elements {
            for mref in &e.meshes {
                let mesh = &m.meshes[mref.mesh.index()];
                assert_eq!(mesh.indices.len() % 3, 0);
                assert_eq!(mesh.normals.len(), mesh.positions.len());
                assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.positions.len()));
            }
        }
    }
}
