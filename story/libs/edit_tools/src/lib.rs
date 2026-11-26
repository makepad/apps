//! The agent loop over edits and motion documents: the
//! commands a person or an AI agent runs while writing a cut, each with a
//! short human report and a machine-readable one (`strict_json::Value`).
//!
//! - [`inspect`]: load diagnostics, missing media and revisions, unknown or
//!   non-seekable effects, layers outside the frame, text overflow, 9:16
//!   safe-zone violations, empty and overlapping spans, gaps, the clip limit.
//! - [`timeline`]: scenes, tracks, blocks and audio as text or JSON.
//! - [`effect_strip`]: an effect or transition (vj_fx or the render kit)
//!   over a test card, as frames for a strip.
//! - [`audio`]: per-segment loudness (BS.1770), true peak, clipping and
//!   silence of the cut's mixdown.
//! - [`grab`]: frames of the cut on the real GPU through the renderer's
//!   engine (a host drives it from its draw, like `CutThumbnail`), and
//!   [`image`]: PNG, labelled contact sheets and onion-skin motion trails
//!   made from them; [`exposure`]: each grabbed frame's clipped and
//!   near-black shares, mean luma and range, from the pixels read back;
//!   [`coverage`]: each 3D scene node's share of its scene and screen box.
//!
//! Pure commands never touch a GPU or a clock; `audio::analyze` and
//! `inspect` with a resolver read files and belong on a worker.
//! A motion document is inspected and laid out by itself ([`motion`]).

pub mod audio;
pub mod coverage;
pub mod effect_strip;
pub mod exposure;
pub mod grab;
pub mod image;
pub mod input;
pub mod inspect;
pub mod motion;
pub mod screen_crop;
pub mod timeline;

pub use audio::{analyze, analyze_mix, AudioOptions, AudioReport};
pub use effect_strip::{EffectSpec, EffectStrip, StripOptions};
pub use exposure::{Exposure, ExposureSummary};
pub use grab::{onion_picture, strip_picture, FrameGrabber, GrabbedFrame};
pub use image::{encode_png, Picture};
pub use input::{parse_time, DocKind, Document};
pub use inspect::{inspect, inspect_text, InspectOptions, InspectReport, Issue, Severity};
pub use timeline::{timeline, Timeline};
