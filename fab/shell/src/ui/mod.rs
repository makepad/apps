//! Fab's UI: the header, the dock of panels around the one plate view
//! (`shell.rs`), the plate view and its overlays (`stage.rs`), the parts
//! panels (`parts.rs`), the camera, the caption's job strip, and the control kit
//! every panel is built from (icons, buttons, drag-numbers, menus, popovers,
//! the file browser, the keymap help, the command palette).

pub mod about;
pub mod arm;
pub mod scan;
pub mod busy;
pub mod force;
pub mod command_palette;
pub mod device3d;
pub mod dropdown;
pub mod elec_layouts;
pub mod editors;
pub mod icons;
pub mod camera;
pub mod code;
pub mod keymap;
pub mod library_float;
pub mod library_panel;
pub mod checks_panel;
pub mod requirements_panel;
pub mod bom_panel;
pub mod board_controls;
pub mod journal_panel;
pub mod review_strip;
pub mod area_notch;
pub mod areas;
pub mod mech_actions;
pub mod mechanics;
pub mod menu;
pub mod menus;
pub mod model_sources;
pub mod param_panel;
pub mod parts;
pub mod plates;
pub mod print_settings;
pub mod progress_card;
pub mod popover;
pub mod shell;
pub mod slice_panel;
pub mod stage;
pub mod steps;
pub mod job_strip;
pub mod topbar;
pub mod workspace_tabs;
pub mod workspace_tool;
pub mod overrides;
pub mod strength;
pub mod test_prints;
pub mod widgets;

use makepad_widgets_core::*;
use makepad_widgets_data::dock::*;
use makepad_xr_stream::XrDesktopFreeze;

/// The control kit (icons + styled base widgets + the controls that are
/// ours). Registered right after the theme so every module's `script_mod!`
/// can use `FabButton`, `FabIcon`, `ValueInput`… .
pub fn script_mod_kit(vm: &mut ScriptVm) {
    icons::script_mod(vm);
    widgets::script_mod(vm);
    dropdown::script_mod(vm);
    popover::script_mod(vm);
    busy::script_mod(vm);
}

/// The panels and the shell. Registered after every other module so it can
/// place their widgets.
pub fn script_mod(vm: &mut ScriptVm) {
    workspace_tabs::script_mod(vm);
    job_strip::script_mod(vm);
    topbar::script_mod(vm);
    param_panel::script_mod(vm);
    code::script_mod(vm);
    steps::script_mod(vm);
    overrides::script_mod(vm);
    strength::script_mod(vm);
    test_prints::script_mod(vm);
    print_settings::script_mod(vm);
    parts::script_mod(vm);
    plates::script_mod(vm);
    mechanics::script_mod(vm);
    arm::script_mod(vm);
    scan::script_mod(vm);
    force::script_mod(vm);
    mech_actions::script_mod(vm);
    progress_card::script_mod(vm);
    stage::script_mod(vm);
    device3d::script_mod(vm);
    slice_panel::script_mod(vm);
    keymap::script_mod(vm);
    about::script_mod(vm);
    model_sources::script_mod(vm);
    command_palette::script_mod(vm);
    library_float::script_mod(vm);
    library_panel::script_mod(vm);
    checks_panel::script_mod(vm);
    requirements_panel::script_mod(vm);
    bom_panel::script_mod(vm);
    board_controls::script_mod(vm);
    journal_panel::script_mod(vm);
    review_strip::script_mod(vm);
    area_notch::script_mod(vm);
    shell::script_mod(vm);
}

/// Invalidate sibling widgets after the current event releases their widget borrows.
#[derive(Clone, Debug)]
pub struct Redraw(pub &'static [&'static [LiveId]]);

pub fn redraw(cx: &mut Cx, paths: &'static [&'static [LiveId]]) {
    cx.action(Redraw(paths));
}

/// The Plate viewport that draws `pane`: what the app reads a view's picture or size from
/// names its pane, never the first `viewport` in the tree. O(Plate views); not per frame.
pub fn viewport_of(cx: &Cx, root: &WidgetRef, pane: crate::api::PaneId) -> WidgetRef {
    root.widgets(cx, &[ids!(viewport)]).iter()
        .find(|w| w.borrow::<crate::viewport::FabViewport>().is_some_and(|v| v.pane() == pane))
        .cloned()
        .unwrap_or_default()
}

/// Dock tab ids also name their chrome; invalidate the body explicitly. A tab records its
/// body's list again, not every list inside it: the body reads the state as it draws, and
/// what changed there redraws its own list in that draw (a part panel section that stayed the
/// same keeps its recording). A repeatable editor's tab id reaches every instance's tab, and
/// a widget path every widget it names (each Plate's `viewport`): a hidden one only marks its
/// list.
///
/// While a headset's interaction holds the desktop ([`XrDesktopFreeze`]) its window paints
/// nothing, so it records nothing either: only the headset's view and the viewport redraw
/// (the viewport keeps its picture; its draw publishes the fields and paint the eyes draw).
/// The panels and the plate tab's overlays wait for the release, which redraws the window.
pub fn redraw_now(cx: &mut Cx, root: &WidgetRef, paths: &[&[LiveId]]) {
    let frozen = XrDesktopFreeze::active(cx);
    for &path in paths {
        if frozen && !matches!(path, [id] if *id == live_id!(xr) || *id == live_id!(viewport)) {
            continue;
        }
        if let Some(editor) = editors::Editor::all().find(|editor| path.len() == 1 && editor.tab() == path[0]) {
            let dock = root.dock(cx, ids!(dock));
            // the Part's editor is the Plate views' sidebar: each Plate body's
            if editor == editors::Editor::Part {
                let plate = editors::Editor::Viewport.kind();
                let bodies: Vec<WidgetRef> = dock.borrow_mut().map(|mut dock| {
                    dock.items().values().filter(|(kind, _)| *kind == plate).map(|(_, body)| body.clone()).collect()
                }).unwrap_or_default();
                for body in bodies {
                    body.widget(cx, ids!(part_side)).redraw(cx);
                }
            }
            if editor.repeatable() {
                let tabs: Vec<LiveId> = dock.borrow_mut().map(|mut dock| {
                    dock.items().iter().filter(|(_, (kind, _))| *kind == editor.kind()).map(|(tab, _)| *tab).collect()
                }).unwrap_or_default();
                for tab in tabs {
                    dock.redraw_tab(cx, tab);
                }
            } else {
                dock.redraw_tab(cx, path[0]);
            }
        } else {
            for widget in root.widgets(cx, &[path]).iter() {
                widget.redraw(cx);
            }
        }
    }
}

/// [`redraw_now`] for the callers that redraw every frame (a playhead step, an electronics
/// drag or scene): a path inside the Plate view redraws in each Plate body through a lookup in
/// that body, a cached first match; `xr` is the window's own. A window-wide walk for every match,
/// as `redraw_now` does, costs some 15 us per path on Fab's tree (under the JIT, against ~1 us
/// for a first match). The same freeze rule. Per call: a lookup per path and Plate body, and a
/// short list of the bodies.
pub fn redraw_plates(cx: &mut Cx, root: &WidgetRef, paths: &[&[LiveId]]) {
    let frozen = XrDesktopFreeze::active(cx);
    let plate = editors::Editor::Viewport.kind();
    let bodies: Vec<WidgetRef> = root.dock(cx, ids!(dock)).borrow_mut().map(|mut dock| {
        dock.items().values().filter(|(kind, _)| *kind == plate).map(|(_, body)| body.clone()).collect()
    }).unwrap_or_default();
    for &path in paths {
        if frozen && !matches!(path, [id] if *id == live_id!(xr) || *id == live_id!(viewport)) {
            continue;
        }
        if matches!(path, [id] if *id == live_id!(xr)) {
            root.widget(cx, path).redraw(cx);
            continue;
        }
        for body in &bodies {
            body.widget(cx, path).redraw(cx);
        }
    }
}
