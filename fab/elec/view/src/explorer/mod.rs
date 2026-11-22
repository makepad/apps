//! The Circuit tab's pane: the schematic that draws itself (ARCHITECTURE 5.4, E2.2 to E2.6).
//!
//! - `glyphs`: the primitives the sheet is drawn from: part symbols (from the part's symbol, or
//!   the family glyph for small parts), rail and label tags, junctions, bus entries.
//! - `scene`: retained sheet geometry, text, picking and semantic nodes.
//! - `view`: the circuit explorer widget, its navigation and assembly motion.
//! - `scrub`: a drag on a value chip steps the value through its preferred-number series.
//! - `layout_job`: the sheet is laid out on a worker, the latest request winning.
//! - `liveix`: sheet wiring trees and static indices into the live bench's row textures.
//! - `targets`: what the pointer aims at for an edit gesture (pins, wire ends, tags, wires, canvas).
//! - `value_edit`: a value edited as text (the inspector's value field).
//! - `inspector`: the card for the part a click selects: its value, edited in place, and its facts.
//! - `tools`: the pane's tool strip (Select, Wire, No-connect, Probe), armed one gesture at a time.

pub mod body_source;
pub mod glyphs;
pub mod scene;
pub mod view;
pub mod scrub;
mod drag;
mod erc_marks;
mod gesture;
mod font_metrics;
pub mod layout_job;
pub mod liveix;
pub mod targets;
mod value_edit;
mod inspector;
pub mod tools;
pub mod reveal_view;
