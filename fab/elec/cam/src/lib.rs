//! Fab electronics manufacturing output (contract C7): plotting a board and its fills through a
//! resolved factory profile (Gerber X2, Excellon, BOM and placement tables, the zip), and the
//! readers for our own outputs used by the round-trip tests. Owned by ECAM.
//!
//! Plotting never refills and never calls the language: fills and the resolved profile come in.
//!
//! C7 signatures (the contract text): `plot`, `export` and `validate_export` take `&[ZoneFill]` (the
//! tiled fills of EROUTE; a pour is the union of a fill's kept polygons, read once) with the board,
//! the circuit and the resolved profile. `CamModel::lattice` is the fill lattice the pours were tiled
//! by. `Validation` checks carry `at` (the place) and `tile` (the lattice tile). A seam fault (the file
//! lacks copper the model has across a tile edge) is an error that blocks the zip. The per-tile
//! verdict cache (skipping unchanged tiles) is a later step and not part of this contract yet.
//!
//! - `image`: `GerberImage`, `DrillImage`, objects with attributes, region maths.
//! - `gerber_write`, `gerber_read`: the layer format with attributes (X2), apertures and macros.
//! - `drill`: Excellon writer and reader.
//! - `profile`: `ResolvedProfile`, `FileSpec`, `OutFile`, `Validation`.
//! - `model`: `CamModel` (per-layer objects, holes, poses, BOM references) and `render`.
//! - `validate`: reads every file back and compares it with the model.

pub mod actions;
pub mod bom_export;
pub mod caps;
pub mod drill;
pub mod drill_export;
pub mod error;
pub mod export;
pub mod gerber_export;
pub mod gerber_read;
pub mod gerber_write;
pub mod image;
pub mod ipc356;
pub mod model;
pub mod package;
pub mod placement_export;
pub mod plot;
pub mod profile;
pub mod tables;
pub mod text;
pub mod validate;

pub use export::{export, plot, read_back, validate_export, Export, ReadBack};
pub use drill::{read_excellon, write_excellon, DrillOpts, Zeros};
pub use error::CamError;
pub use gerber_read::read_gerber;
pub use gerber_write::{write_gerber, GerberOpts};
pub use image::{DrillHole, DrillImage, GItem, Geom, GerberImage, ObjAttrs, Polarity};
pub use model::{render, CamModel, Pose};
pub use profile::{builtin_factories, Capabilities, Check, CheckKind, FileKind, FileSpec, OutFile, ResolvedProfile, Validation, PROFILES};
pub use validate::validate;

#[cfg(test)]
#[path = "actions_tests.rs"]
mod actions_tests;
#[cfg(test)]
#[path = "caps_tests.rs"]
mod caps_tests;
#[cfg(test)]
mod testfill;
#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "export_tests.rs"]
mod export_tests;
#[cfg(test)]
#[path = "alt_profile_tests.rs"]
mod alt_profile_tests;
#[cfg(test)]
#[path = "jlc_tests.rs"]
mod jlc_tests;
#[cfg(test)]
#[path = "arcs_tests.rs"]
mod arcs_tests;
#[cfg(test)]
#[path = "ipc356_tests.rs"]
mod ipc356_tests;
