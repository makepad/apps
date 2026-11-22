//! Fab's shell: the document/scene model, the dock of panels around the one
//! plate view, navigation, the part tools, the realtime viewport and the
//! print preview. The app composes it with the format loaders implementing
//! [`model::Loader`].

pub use makepad_widgets_core;
pub use fab_sdf_engine;
pub use fab_sdf_tiles;

pub mod api;
pub mod document;
pub mod firmware_net;
pub mod firmware_transport;
pub mod loader;
pub mod mech_track;
pub mod model;
pub mod model_sources;
pub mod nav;
pub mod params;
pub mod print;
pub use fab_controls::{handle_style, theme};
pub mod tools;
pub mod ui;
pub mod view_prefs;
pub mod workspaces;
pub mod viewport;

pub use api::*;
pub use document::{Document, DocumentBuilder, Edit};
pub use loader::LoadCoordinator;
pub use mech_track::VelocityTrack;
pub use model::Loader;

use makepad_widgets_core::ScriptVm;

/// Register every framework widget and shader in dependency order.
pub fn script_mod(vm: &mut ScriptVm) {
    // The always-available pairing control, the server's frame sink and the mirror.
    makepad_xr_stream::serve_script_mod(vm);
    theme::script_mod(vm);
    // The program's code view and its inline value controls (the Design workspace).
    makepad_code_editor::script_mod(vm);
    makepad_code_values::script_mod(vm);
    ui::script_mod_kit(vm);
    viewport::script_mod(vm);
    nav::script_mod(vm);
    tools::script_mod(vm);
    print::script_mod(vm);
    ui::script_mod(vm);
}
