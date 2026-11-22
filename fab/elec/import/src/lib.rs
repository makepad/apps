//! Fab electronics part import (EPARTS, `local/plans/part-models.md`).
//!
//! One pipeline from a part source (a supplier's part service, a CAD library, a model file) to one runtime
//! format: a Splash part whose body references one `.glb` (`BodySource::Mesh`). Mesh formats other
//! than glTF (OBJ with MTL, VRML) are read once, at import, into a [`Mesh`], placed in the part's
//! body frame and written out as `.glb` ([`glb::write_glb`]); the runtime reads only `.glb`
//! ([`glb::read_glb`]).
//!
//! Readers never panic on their input: a shape or field they cannot take is an [`Issue`] that names
//! the line and the field, either the refusal of the whole file (`Err`) or a warning carried with the
//! result ([`MeshRead::warnings`]). Nothing silently drops or guesses geometry.
//!
//! Cost: import only, once per part, on a worker; linear in the input's size (triangulation is
//! quadratic in one polygon's corner count, and CAD polygons are small). Nothing here runs per frame.

pub mod cache;
pub mod easyeda;
pub mod fit;
pub mod glb;
pub mod kicad3d;
pub mod library;
pub mod mesh;
pub mod obj;
pub mod part;
pub mod wrl;

pub use mesh::{Affine, Material, Mesh, MeshBuilder, Normals, Prim};

/// A located problem in an input: the 1-based line it is on (0 when the format has no lines, as a
/// `.glb`), the statement or field it concerns, and what is wrong.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Issue {
    pub line: u32,
    pub field: String,
    pub message: String,
}

impl Issue {
    pub fn new(line: u32, field: impl Into<String>, message: impl Into<String>) -> Self {
        Issue { line, field: field.into(), message: message.into() }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line > 0 {
            write!(f, "line {}: {}: {}", self.line, self.field, self.message)
        } else {
            write!(f, "{}: {}", self.field, self.message)
        }
    }
}

/// What a mesh reader gives: the mesh in the file's own units and axes, and what it skipped or
/// approximated, each located.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct MeshRead {
    pub mesh: Mesh,
    pub warnings: Vec<Issue>,
}
