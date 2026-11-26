//! The tweak panel's widgets that need Motion: [`font_picker::FontPicker`]
//! (and the panel's font row, [`panel_font::PanelFont`]) and the effects stack
//! ([`effects::PanelEffects`]). The panel kit they sit on is the open
//! `makepad-value-pickers`.

pub mod doc;
pub mod effects;
pub mod font_picker;
pub mod panel_font;

pub use doc::{motion_doc, panel_files, MotionDoc};
pub use font_picker::*;

use makepad_widgets::*;

/// The font row's paths, for the panel's tree (search and filters).
fn own_paths(w: &WidgetRef) -> Option<Vec<String>> {
    w.borrow::<panel_font::PanelFont>().map(|x| vec![x.path.clone()])
}

/// What a panel's isolate needs to name these widgets (`makepad_editor_panel::register`'s
/// `more`): the widgets, and the isolate's prelude made again with them.
pub fn isolate_mod(vm: &mut ScriptVm) {
    script_mod(vm);
    makepad_widgets::script_eval!(vm, {
        mod.prelude.widgets = {
            ..mod.prelude.widgets_header,
            theme: mod.theme,
            ..mod.widgets,
        }
    });
}

/// Register the widgets (after the panel kit's `script_mod`).
pub fn script_mod(vm: &mut ScriptVm) {
    font_picker::script_mod(vm);
    panel_font::script_mod(vm);
    effects::script_mod(vm);
    makepad_value_pickers::panel::tree::register_own(own_paths);
}
