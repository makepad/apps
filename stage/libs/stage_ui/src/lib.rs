//! Stage performance UI.
//!
//! Column layout, deck-tab follow mode, library keyboard and pick rules,
//! and the step grid. This crate draws and remembers view state. It does
//! not own a flow runtime, an instrument, or a deck engine.
//!
//! [`script_mod`] registers this library's widget only. The host calls
//! `makepad_widgets::script_mod` first.

pub mod columns;
pub mod deck_strip;
pub mod deck_tabs;
/// Composes `LiveFrameView` too, as [`music_video`] does.
pub mod layer_stack;
pub mod library;
pub mod library_nav;
/// The tile grid and the pad matrix (the effect picker's, the asset
/// browser's); its host registers it ([`live_grid::script_mod`]) where its
/// look is in place.
pub mod live_grid;
pub mod loop_splat_view;
pub mod recent_searches;
pub mod model;
pub mod mix_surface;
pub mod music_controls;
pub mod music_surface;
/// Composes `LiveFrameView`: the host registers
/// `makepad_live_frame::script_mod` before [`script_mod`].
pub mod music_video;
pub mod music_wave;
pub mod flow_wave;
pub mod page_dock;
pub mod palette_lane;
pub mod palette_row;
pub mod track_list;
pub mod track_menu;

pub use deck_strip::{DeckStrip, DeckStripAction, DeckStripRef, DeckStripState, DeckStripWidgetRefExt, PagedTransportStamp};
pub use layer_stack::{LayerStackAction, LayerStackRow, LayerStackView, LayerStackViewRef, LayerStackViewWidgetRefExt};
pub use library::{LibraryAction, LibraryMode, LibraryRow, LibraryView, LibraryViewRef, LibraryViewWidgetRefExt, SetListAsk, SetListsView};
pub use model::DeckId;

/// Register this library's widgets on `mod.widgets`, after Stage's look
/// (`makepad_look`) they paint with.
///
/// Does not register `makepad_widgets` and does not start a service.
pub fn script_mod(vm: &mut makepad_widgets::ScriptVm) {
    makepad_look::script_mod(vm);
    music_video::script_mod(vm);
    music_controls::script_mod(vm);
    mix_surface::script_mod(vm);
    layer_stack::script_mod(vm);
    music_wave::script_mod(vm);
    flow_wave::script_mod(vm);
    music_surface::script_mod(vm);
    deck_strip::script_mod(vm);
    palette_row::script_mod(vm);
    palette_lane::script_mod(vm);
    library::script_mod(vm);
    loop_splat_view::script_mod(vm);
}
