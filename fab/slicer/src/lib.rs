//! Fab's slicer geometry.
//!
//! - `geom`: integer-grid points, rings, regions (1 unit = 1 nm).
//! - `grid`: the shared spatial index of segments (exact nearest, box, winding queries).
//! - `clip`: exact polygon booleans with fill rules.
//! - `offset`: grow/shrink with miter, square or round joins.
//! - `mesh`: triangle meshes from STL and 3MF.
//! - `instance`: copies of one mesh at other XY positions (planned once).
//! - `slice`: mesh → per-layer regions.
//! - `paint`: colour paint → per-layer filament regions.
//! - `sdf`: layer contours straight from a distance field (no mesh).
//! - `source`: the contour-source trait the toolpath stage slices through.
//! - `embed`: items printed into a part: its regions less their cavities.
//! - `xml`: the small XML reader the 3MF parts need.

pub mod clip;
pub mod embed;
pub mod geom;
pub mod grid;
pub mod instance;
pub mod mesh;
pub mod nonplanar;
pub mod offset;
pub mod paint;
pub mod par;
pub mod sdf;
pub mod sdf_paint;
pub mod slice;
pub mod source;
pub mod xml;

pub use geom::{scaled, unscaled, BBox, ExPolygon, Polygon, Polyline, Pt, SCALE};
