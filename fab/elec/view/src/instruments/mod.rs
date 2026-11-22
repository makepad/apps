//! The instruments (ARCHITECTURE 5.21): the digital phosphor oscilloscope. `acquire` is the sink
//! on every simulation member (trigger, then each acquisition window rasterised as its samples
//! arrive), `phosphor` the hit frames, the span rasteriser and the waveform-weighted persistence,
//! `feed` the member workers and the merger that hands the UI one frame at a time,
//! `phosphor_view` the GPU's accumulation into a 32-bit float picture and its grading, `scope`
//! the signal/meter lane around that screen (labelled axes, trigger markers, answers) and
//! its settings model.
//! `axis` lays out linear and logarithmic ticks and engineering labels without a drawing context.
//! `look` holds the instruments' tokens: channel hues, text levels and rules over the canvas tokens.
//! `scrub` draws the record around a paused instant and shares the live capture's measurements.
//! `sources` is the host's evaluated-bench seam; `drive_lane` shows each stub's driven pins and
//! emits duty, frequency and mode edits for the host to splice into its bench source.

pub mod acquire;
pub mod axis;
pub mod bench;
pub mod controls_panel;
pub mod bench_setup;
pub mod device;
pub mod device_feed;
pub mod decoders;
pub mod delay;
pub mod distribution;
mod distribution_view;
pub mod feed;
pub mod drive_lane;
pub mod events;
pub mod fw_events;
pub mod fw_lane;
pub mod gesture;
pub mod intake_worker;
pub mod logic;
pub mod logic_view;
pub mod look;
pub mod panel_model;
pub mod phosphor;
pub mod phosphor_view;
pub mod reticle;
pub mod scope;
mod semantics;
pub mod scope_host;
pub mod scrub;
pub mod simfeed;
pub mod sources;
pub mod span;
pub mod spectrum;
/// Catalog tools and asynchronous capture storage.
pub mod tools;
pub mod trigger_ui;
pub mod trend;
pub mod source_bar;

use makepad_widgets_core::*;

/// Registers the instruments after `live` (shader ramps) and `trace` (the meter spark).
pub fn script_mod(vm: &mut ScriptVm) {
    crate::live::tag::script_mod(vm);
    phosphor_view::script_mod(vm);
    crate::live::peek_scope::script_mod(vm);
    distribution_view::script_mod(vm);
    scope::script_mod(vm);
    drive_lane::script_mod(vm);
    fw_lane::script_mod(vm);
    crate::live::strip::script_mod(vm);
    source_bar::script_mod(vm);
    controls_panel::script_mod(vm);
    bench_setup::script_mod(vm);
    bench::script_mod(vm);
    logic_view::script_mod(vm);
}
