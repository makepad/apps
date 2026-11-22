// Ported from TiNRS Psychedelic Router 0.1.0 (src/organic, src/core, src/topology).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! The organic routing method (`@organic`): the reference's guarded energy-descent relaxer and
//! its export copper, ported function by function, joined to Fab's board model and judged by
//! Fab's own checks (`verify`, `drc`).
//!
//! Inside this module everything is the reference's: mm, `f32` where it says float and `f64`
//! where it says double, its constants, its Y-down frame and its index-based board model
//! (`model::OBoard`). Fab's types meet it in `adapt` only: nm to mm once on the way in, mm to
//! nm (rounded once) on the way out.
//!
//! Files map one to one onto reference files (the port ledger keeps file:line provenance):
//! - `geom`: core/Geometry.h;
//! - `model`: the subset of core/Board.h the relaxer reads;
//! - `padgeom`: core/PadGeometry.h (pad frames, holes, distances);
//! - `copper`: organic/CopperGeometry.h (segment, box and pad gaps; net connectivity);
//! - `diffpairs`: core/DiffPairs.h/.cpp (sampled coupled length and pair membership);
//! - `biarc`: core/Biarc.h (arc pairs and sampled biarc splines);
//! - `arcfit`: core/ArcFit.h/.cpp (arc and octilinear export, with an injected violation counter);
//! - `grid`: organic/SpatialGrid.h;
//! - `settings`: the settings of organic/OrganicRelaxer.h;
//! - `engine`: the relaxer's Engine (organic/OrganicRelaxer.cpp): indices, contacts, pair evaluation;
//! - `relax`: the relaxation of a step (organic/OrganicRelaxer.cpp): node state, sweeps, resampling;
//! - `relaxer`: the step driver and the curve export (organic/OrganicRelaxer.cpp);
//! - `spline`: the exported curves (organic/OrganicRelaxer.cpp);
//! - `teardrop_sdf`: distance-field teardrops (organic/TeardropSdf.h/.cpp);
//! - `neckdown`: neckdown infrastructure and trace widths (organic/NeckdownTeardrop.cpp);
//! - `neckdown_td`: teardrops and the combined export pass (re-exported through `neckdown`);
//! - `opt`: the finish passes (src/optimize/) and what they share: `Guard`, `drcgeom`, `hops`,
//!   `pairguard`;
//! - `simplify`: topology/NetGraphSimplifier.cpp (loop removal, dangling vias);
//! - `adapt` (Fab's): `OBoard` from a `Board`, a `RuleMatrix` and an `RStack`;
//! - `pour`: a spanning forest through settled pour fills, with one shared segment legality test;
//! - `job` (Fab's): the `@organic` method: grid route, relax, verify, lift back to tracks.
//!
//! Determinism: no hash-order iteration, no wall clock in any decision, libm only outside
//! decisions; same input, same bytes, any thread count.

pub mod adapt;
pub mod arcfit;
pub mod biarc;
pub mod copper;
pub mod diffpairs;
pub mod engine;
pub mod geom;
pub mod grid;
pub mod job;
mod judge;
pub mod model;
pub mod neckdown;
mod neckdown_td;
pub mod opt;
pub mod padgeom;
pub mod pour;
pub mod relax;
pub mod relaxer;
pub mod settings;
pub mod simplify;
pub mod spline;
pub mod teardrop_sdf;
