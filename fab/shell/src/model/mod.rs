//! Document and scene data shared by loaders, tools and the viewport.
//!
//! Sits between format loaders (plain data out) and the viewport renderer
//! (`crate::viewport`). Everything in this module is **pure data**: no GPU
//! handles, no `Cx`, no threads. A [`Scene`] is built once on a loader thread
//! (or the model worker) from a [`ModelData`] and then shared as `Arc<Scene>`
//! with the UI thread. Mutable per-session state (selection, visibility,
//! section planes) lives in [`SceneState`] and is versioned with a
//! `revision` counter so consumers can diff cheaply.
//!
//! ## Coordinate law
//! Inside this crate and everything downstream: **right-handed, Z up, meters**.
//! `ModelData` declares the source units and up-axis; `Scene::from_model`
//! normalises. Nothing after that ever converts again.
//!
//! ## Boundary contract
//! * Loader → model: [`crate::document::Document`], projected to [`ModelData`].
//! * Model → viewport: [`Scene`] (`batches` are GPU-ready CPU buffers), [`SceneState`],
//!   [`query::Ray`]/[`query::RayHit`]/[`query::Frustum`].
//! * Model → tools: units formatting, snapping and measurement geometry.

pub mod batch;
pub mod bounds;
pub mod bvh;
pub mod fixture;
pub mod ids;
pub mod model;
pub mod overlap;
pub mod query;
pub mod scene;
pub mod source;
pub(crate) mod spread;
pub mod state;
pub mod units;

pub use batch::{ElementRange, RenderBatch, Vertex, VERTEX_STRIDE};
pub use bounds::{aabb_center, aabb_empty, aabb_extent, aabb_is_empty, aabb_radius, aabb_union, aabb_union_point, aabb_transform};
pub use bvh::{Bvh, PickOptions};
pub use ids::{ElementId, MaterialId, MeshId};
pub use model::{
    ElementData, Handedness, MaterialData, MeshData, MeshRef, ModelData, SubMesh, TextureData,
    UpAxis,
};
pub use overlap::{OverlapRecord, OverlapReport, DEFAULT_COPLANAR_TOL_M};
pub use query::{
    angle_deg, distance, polygon_area, Frustum, MeasureKind, Ray, RayHit,
    SnapHit, SnapKind, SnapOptions,
};
pub use scene::{
    Element, Material, Scene, ScenePart, SceneStats, MAX_BATCH_VERTICES, TARGET_BATCH_TRIANGLES,
};
pub use source::{LoadCancel, LoadError, LoadProgress, Loader};
pub use state::{PoseSet, SceneState, SectionPlane, SectionState, Selection};
pub use units::{LengthUnit, Units};

pub use makepad_math;
