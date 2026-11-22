//! Fab electronics language (contract C5): evaluation of a project's Splash documents to a
//! `Design`, the two-way source map, the writer of board statements, span splices for design
//! ops and resolved factory profiles. Owned by ELANG.
//!
//! - `source`: the project's documents as text (`Sources`, `Doc`, `DocKind`), library documents
//!   (`Libs`) and the loader of a project folder (`load_dir`). Positions are byte offsets.
//! - `map`: `SourceMap`, the two-way map between any `Sel` and the source range that made it,
//!   and `SourceRange`.
//! - `eval`: `evaluate(sources, libs)` and `evaluate_from(prev, ..)`: the design, its source map
//!   and each unit's state (built, or kept at its last good lowering with located errors).
//! - `splice`: the text side of a design op (`SpanEdit`), which keeps the map current by the
//!   edit's length delta.
//! - `write`: a board as canonical board statements.
//!
//! Errors are `fab_elec_core::Diag`s located by `Sel::Source { doc, start, end }`; `Sources::describe`
//! writes such a location as `file:line:col`.
//!
//! Cost: nothing here runs per frame. Evaluation and splicing run on the host's worker on
//! snapshots (`Sources` clones share each document's text).

mod bench;
mod bench_splice;
pub mod bench_view;
pub mod board_doc;
pub mod board_splice;
mod catalog;
mod circuit;
mod circuit_splice;
pub use circuit_splice::models_doc_name;
pub mod eseries;
pub mod eval;
mod hint_splice;
pub mod led;
pub mod map;
mod pin_splice;
pub mod prims;
pub mod resolve;
mod series_splice;
pub mod source;
pub mod splice;
pub mod sys_actions;
pub mod sys_doc;
mod sys_splice;
pub mod sys_sim;
pub mod units;
pub mod value_site;
mod verify;
pub mod write;

pub use bench_view::{BenchEdit, BenchView, bench_views};
pub use bench::stimulus_unit;
pub use catalog::{blocks_alone, catalog, defs_catalog, Catalog, CatalogEntry, CatalogKind, CatalogPin, CatalogSource, Miniature, ProjectDefs};
pub use circuit::Phases;
pub use eval::{
    BenchOut, BenchPanel, BenchProbe, BenchScope, PanelEntry, PanelGroup, EvalError, Evaluated, NO_CANCEL, SysEval, SystemLowered, UnitEval, UnitState, check_system, evaluate,
    evaluate_from, relower_system, std_libs,
};
pub use fab_elec_core::{Diag, DocId};
pub use map::{BenchKey, BenchSite, Input, Origin, PinSite, SourceMap, SourceRange, ValueSite};
pub use pin_splice::{part_template, part_templates, PartTemplates};
pub use source::{load_dir, part_assets, Doc, DocKind, Libs, Sources, SYSTEM_DOC};
pub use splice::{splice, SpanEdit};
pub use verify::{Verified, splice_verified, text_verified, with_files};
pub use write::write;
