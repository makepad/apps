//! Hide / isolate / unhide, and solo mode.
//!
//! Thin over `SceneState` (which owns the semantics) so the keys and the
//! Edit menu go through one implementation. Everything here emits actions;
//! nothing mutates `AppState` behind the app's back.
//!
//! * `H` hide selected · `Shift+H` isolate selected · `Alt+H` unhide all
//! * `/` solo: isolate the selection, press again to come back

use crate::api::*;
use makepad_widgets_core::*;

pub fn hide_selected(cx: &mut Cx) {
    cx.action(ShellAction::HideSelected);
}

pub fn isolate_selected(cx: &mut Cx) {
    cx.action(ShellAction::IsolateSelected);
}

pub fn unhide_all(cx: &mut Cx) {
    cx.action(ShellAction::UnhideAll);
}

/// True while an isolation is in force.
pub fn is_solo(state: &AppState) -> bool {
    state.scene_state.isolated.is_some()
}

/// `/` — isolate the selection, or come back if already isolated.
pub fn toggle_solo(cx: &mut Cx, state: &AppState) -> &'static str {
    if is_solo(state) {
        unhide_all(cx);
        "Solo off"
    } else if state.scene_state.selection.is_empty() {
        "Nothing selected to isolate"
    } else {
        isolate_selected(cx);
        "Solo on"
    }
}
