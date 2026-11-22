//! `ModelData` — the plain input of the packed scene build: what a
//! [`crate::document::Document`] projects to and the only thing
//! [`crate::model::Scene::from_model`] consumes. Keep it boring: plain `Vec`s, no
//! lifetimes, no traits, no GPU types.
//!
//! Conventions (the scene layer validates and normalises, it does not guess):
//! * `units.source_to_meters` scales every position and every `transform`
//!   translation. Fab's own scenes are **meters** → `1.0` (the default);
//!   other sources declare their own factor.
//! * `up_axis` declares the source up axis. Y-up sources are rotated on
//!   import so the [`crate::model::Scene`] is always Z-up.
//! * `handedness` is declared, not assumed. Left-handed sources get their
//!   triangle winding flipped on import.
//! * Triangles only, CCW front faces after normalisation, `u32` indices.
//! * `normals`/`uvs` may be empty; missing normals are computed (flat) on
//!   import, missing uvs become `(0,0)`.
//! * Mesh positions are in the element's **local** space; `ElementData::transform`
//!   places them in the world. Parsers that only have world-space meshes set
//!   `transform` to identity.
//! * Every element with geometry references at least one [`MeshRef`] — a mesh
//!   plus its **instance transform** (shared meshes are placed by instance;
//!   drawing meshes without their instance transforms piles them at the
//!   origin). An element without mesh refs is a pure hierarchy node.

use crate::model::ids::{ElementId, MaterialId, MeshId};
use crate::model::units::Units;
use makepad_math::Mat4f;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UpAxis {
    #[default]
    Z,
    Y,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Handedness {
    #[default]
    Right,
    Left,
}

/// A whole parsed file.
#[derive(Clone, Debug, Default)]
pub struct ModelData {
    pub name: String,
    pub source_path: Option<PathBuf>,
    pub units: Units,
    pub up_axis: UpAxis,
    pub handedness: Handedness,
    pub meshes: Vec<MeshData>,
    pub materials: Vec<MaterialData>,
    /// Decoded images stored once; materials reference this array by index.
    pub textures: Vec<TextureData>,
    pub elements: Vec<ElementData>,
}

/// One indexed triangle mesh in element-local space.
#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    /// Same length as `positions` or empty.
    pub normals: Vec<[f32; 3]>,
    /// Same length as `positions` or empty.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle list, 3 indices per triangle.
    pub indices: Vec<u32>,
    /// Contiguous index ranges by material. Covers all of `indices` in order.
    /// Empty means "one submesh, `MaterialId::NONE`".
    pub submeshes: Vec<SubMesh>,
}

/// One placement of a mesh inside an element (instancing). The final local
/// transform of the geometry is `ElementData::transform * MeshRef::transform`.
#[derive(Clone, Debug)]
pub struct MeshRef {
    pub mesh: MeshId,
    pub transform: Mat4f,
    /// Overrides the mesh's own submesh materials for this placement: a mesh
    /// shared between elements can look different on each. `None` = use the
    /// mesh's submeshes.
    pub material: Option<MaterialId>,
}

impl MeshRef {
    pub fn identity(mesh: MeshId) -> MeshRef {
        MeshRef {
            mesh,
            transform: Mat4f::default(),
            material: None,
        }
    }

    pub fn with_material(mesh: MeshId, transform: Mat4f, material: MaterialId) -> MeshRef {
        MeshRef {
            mesh,
            transform,
            material: Some(material),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubMesh {
    pub material: MaterialId,
    pub first_index: u32,
    pub index_count: u32,
}

/// PBR-ish material as far as the scene carries it. Unknown fields keep their
/// defaults; the renderer never needs anything beyond this.
#[derive(Clone, Debug)]
pub struct MaterialData {
    pub name: String,
    /// Linear RGBA, alpha < 1 means transparent.
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    /// Linear RGB emission (radiance scale, 0 = none).
    pub emissive: [f32; 3],
    /// Index of refraction for transmissive materials (glass ≈ 1.5).
    pub ior: f32,
    /// 0 = opaque, 1 = fully transmissive (glass). Distinct from alpha.
    pub transmission: f32,
    pub double_sided: bool,
    /// Index into [`ModelData::textures`].
    pub texture: Option<usize>,
}

impl Default for MaterialData {
    fn default() -> Self {
        MaterialData {
            name: String::new(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            emissive: [0.0; 3],
            ior: 1.45,
            transmission: 0.0,
            double_sided: false,
            texture: None,
        }
    }
}

/// Decoded RGBA8 image, top row first. Shared: a scene rebuilt from the same document
/// texture holds the same pixels (its encode for the lit model is made once).
#[derive(Clone, Debug, Default)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub rgba: std::sync::Arc<[u8]>,
}

#[derive(Clone, Debug, Default)]
pub struct ElementData {
    pub id: ElementId,
    pub name: String,
    /// Local → world (source units; translation is scaled on import).
    pub transform: Mat4f,
    pub meshes: Vec<MeshRef>,
}
