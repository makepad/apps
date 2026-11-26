//! Motion graphics for Stage's edits (EDITS.md §10.2, §10.3): text, shape,
//! SVG, image, video, shader, 3D and UI animations written as Splash
//! documents, so the AI Hub, Claude Code and people can author them, and
//! evaluated as a pure function of time like everything else in an edit.
//!
//! ```text
//! let doc = MotionDoc::load(source)?;          // typed, with teaching diagnostics
//! let doc = doc.with_inputs(&params)?;          // an edit's Graphic{params: …}, ids as `LiveId::from_str("name")`
//! let doc = MotionDoc::load_with(source, &params, ctx)?;  // beat grid, onsets, sections, voice timings, audio features
//! let frame = doc.eval(t);                      // MotionFrame: the resolved draw list at t
//! // Many instances of one document (a HUD's panes): each holds its own inputs.
//! let mut mine = doc.new_inputs();
//! let lap = doc.input_index(LiveId::from_str("lap")).unwrap();   // resolved once
//! doc.write_input(&mut mine, lap, InputValue::Number(3.0))?;     // by index: no reload
//! let frame = doc.eval_with(t, &mut mine);
//! ```
//!
//! This crate is pure (no GPU, no UI, no clocks, no I/O): the documents,
//! their evaluation and the frame. `makepad-motion-draw` draws a frame into
//! an offscreen target with Makepad's GPU vector, SVG and text renderers,
//! runtime-compiled shaders, the render kit, glTF scenes and the widget
//! tree (vj_fx presets and Scene3D behind its features);
//! `makepad-stage-video-edit` uses [`timing`] for musical time in edits.
//! The `scene3d` feature lowers Seq3D and Text3D layers through motion3d.

/// Overlays bound to an app run's recorded data (lane M4).
pub mod align;
pub mod bind;
pub mod builtin;
mod chart;
pub mod catalog;
/// 2D character rigs, poses, walks and lip sync (lane CH1).
pub mod character;
pub mod draw2d;
pub mod icons;
pub mod inspect;
mod eval;
pub mod features;
/// Every field the Motion and 3D readers accept, from the readers.
pub mod field_registry;
/// A document's font values (`Font{…}`), checked against the document font table.
pub mod font_spec;
/// AudioFeatures as JSON (`makepad-motion --audio <file.json>`; lane CH1).
mod features_json;
pub mod frame;
pub mod geometry;
/// Pure FK, constraints and CPU skinning shared by Bones and Character.
pub mod rig;
/// The component library: the document root and every layer type.
pub mod layers;
pub mod math;
mod load;
pub mod model;
pub mod modules;
#[cfg(feature = "scene3d")]
mod kernel2d;
/// A frame's 3D scene loaded into motion3d.
#[cfg(feature = "scene3d")]
pub mod scene;
/// Source-preserving edits (the editor's timeline, inspector and viewer; lane M5).
pub mod palette;
pub mod paths;
pub mod pieces;
/// Pure data plotting: ticks, adaptive curves, implicit contours and arrows.
pub mod plotting;
/// Union, intersection, difference and exclusive or of filled outlines.
pub mod boolean;
pub mod edit;
pub mod structure;
pub mod lane;
pub mod patch;
/// Procedural motion design: truchet tilings, slit-scan slices, EaseGraph, ParticleText, Bounce and onion skins, GridFloor, Metaballs (lane FX2).
pub mod procedural;
/// A layer's `effects` and the camera checked against the render kit.
pub mod render_effects;
/// Every value a document writes, as editors see it (lane LV1).
pub mod schema;
mod resolve;
pub mod signals;
/// 3D kinetic typography: `Text3D` layers and their presets (lane T3).
pub mod text3d;
pub mod timing;
pub mod crowd;
pub mod lyrics;
pub mod style;
pub mod templates;
/// Typography functions documents call (`text_layout`, `text_path`, `fit_text`, `stroke_path`, …).
mod type_fns;
pub use type_fns::{text_outline_contours, text_width};
pub mod typography;
#[cfg(feature = "scene3d")]
pub mod upgrade;
mod forms;
mod value;
pub use value::{editable, editable_as};
mod obj;
mod lower;
mod live;
mod build;

pub use chart::{chart_checks, ChartIssue, ChartSeverity};
pub use makepad_tween::named::{Curve, Ease};
pub use eval::{Polyline, format_escalate, format_escalate_exp, format_number, format_time, point_along, sample_track, unit_ranges};
pub use features::{AudioFeatures, AudioSignal, FeatureCurve, FeaturePhone, FeatureWord, MusicFeatures, VoiceFeatures, WordState};
pub use frame::{Affine, CameraFrame, EffectFrame, EffectValue, Item, ItemKind, KineticFrame, LightFrame, Local, MaterialFrame, MotionFrame, NodeFrame, PlateFrame, TextUnit, WidgetValue};
pub use load::{check_path_data, RESERVED_UNIFORMS};
pub use model::*;
pub use resolve::uses_musical_time;
pub use signals::{LiveSignals, SignalSource};
pub use timing::{BeatGrid, Length, Onset, Section, TimeExpr, TimeRef, Timing};
pub use layers::layer_types;
pub use value::{DOC_CODE_FILE, DOC_FILE, PRELUDE};
/// The identifier inputs are named by (`in(@name)` is `LiveId::from_str("name")`).
pub use makepad_script::LiveId;

/// Name this crate's directory to a VM, so its resources resolve as
/// `crate_resource("makepad_motion:resources/…")` (its icons and modules). A crate is otherwise known to a VM
/// only through a `script_mod!` of its own, and this pure crate has none.
///
/// The crates the VM knows (`makepad_widgets`, once the widget library is
/// registered) are also where the document fonts' files are read from
/// without a VM (`makepad_doc_fonts::register_root`), so call it after the
/// widgets. The bundled display faces are the font table's own crate's.
pub fn register_resources(vm: &mut makepad_script::ScriptVm) {
    font_spec::font_roots();
    vm.register_crate_manifest("makepad_motion", env!("CARGO_MANIFEST_DIR"));
    vm.register_crate_manifest("makepad_doc_fonts", makepad_doc_fonts::DIR);
    for (name, dir) in vm.crate_manifests().iter() {
        makepad_doc_fonts::register_root(name, dir);
    }
}

/// The share of a document's width inside its side margins (6% each
/// side): what `wrap: @safe` and `fit: @safe` mean, and the width
/// `inspect` holds text to.
pub const SAFE_WIDTH: f64 = 0.88;



pub mod tree;
