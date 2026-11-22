//! Traces (ARCHITECTURE 5.21): what the instruments draw and measure. A trace window is reduced
//! to its pixel columns (`columns`, from the solver's pyramid in `store`), drawn as anti-aliased
//! capsules (`draw`) in the `TraceView` widget (`view`) with its cursors (`cursor`).
//! The scope's measurements and triggers are in `fab_elec_bench` (`measure`, `trigger`).
//! `eye` measures the eye opening of an NRZ record.

pub mod colcache;
pub mod columns;
pub mod cursor;
pub mod draw;
pub mod eye;
pub mod spark;
pub mod store;
pub mod take_window;
pub mod view;

use makepad_widgets_core::*;

/// Registers the trace shaders and widgets.
pub fn script_mod(vm: &mut ScriptVm) {
    draw::script_mod(vm);
    view::script_mod(vm);
    spark::script_mod(vm);
}
