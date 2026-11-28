//! Fab's caption: the window's own title bar carries the app's main
//! controls. `main.rs` hangs [`FabTopBar`] in the window's caption bar; the
//! platform keeps its window buttons, and everything that is not a control
//! (empty space, the title, the state) drags the window. That is why the bar
//! is plain views: the window walks its views to find the controls, and a
//! custom widget would read as one big control. The app runs it through
//! [`handle_actions`] and [`sync`].
//!
//! Left: the cube and the app's menus (File, Edit, View, Help). Centre: the document's name and
//! state (the full name in a tooltip). Right: the requirements chip, the job strip (or the save
//! status while quitting), a reserved slot for the second-window icon (a plate also open in
//! another Fab window), the headset, undo / redo, the workspace menu (it names the shown
//! workspace and lists them all, Cmd+1-9; [`crate::ui::workspace_tabs`]), the AI
//! column and Print. (The code is an editor like any other: its area's selector, or ` to show it.)
//!
//! The slice is a render mode of the plate view (`stage`), not a workspace. Print
//! only opens the printer panel's confirm row (and shows the slice, which slices
//! the plate); the safety-checked start stays there.

use crate::api::*;
use crate::ui::keymap::{self, ShortcutId as Key};
use crate::print::PrintAction;
use crate::ui::menus::{action_for as menu_action_for, edit_menu, file_menu, help_menu, view_menu};
use crate::ui::popover::{menu_picked, open_menu, ui_actions, FabUiAction, MenuItem, MenuPlace};
use crate::ui::editors::Editor;
use crate::ui::shell::ShowPanel;
use crate::ui::workspace_tabs::{FabWorkspaceTabs, WorkspaceTabAction};
use crate::workspaces::{ListOp, Origin, ShownBy, WorkspaceId};
use fab_elec_view::ReqShown;
use makepad_widgets_core::{job_strip::JobStrip, tip::{TipAction, TipWidgetRefExt}};
use makepad_widgets_core::*;

/// The caption's menus: owner id, title and items.
fn menus() -> [(LiveId, &'static str, fn(&AppState) -> Vec<MenuItem>); 4] {
    [
        (live_id!(fab_menu_file), "File", file_menu),
        (live_id!(fab_menu_edit), "Edit", edit_menu),
        (live_id!(fab_menu_view), "View", view_menu),
        (live_id!(fab_menu_help), "Help", |_| help_menu()),
    ]
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // Caption glyphs in the body's ink, lit under the pointer.
    let CapIcon = mod.widgets.FabIconButton{
        width: 28
        height: 26
        padding: Inset{left: 6 right: 6 top: 5 bottom: 5}
    }
    // Between the caption's groups (history, the workspaces, the tools, Print).
    let CapRule = mod.widgets.FabVRule{height: 16 margin: Inset{left: 6 right: 6}}

    // A well in the caption: the field colour, no frame, its edges on device pixels.
    let Well = View{
        show_bg: true
        draw_bg +: {
            pixel: fn() {
                let dpi = self.draw_pass.dpi_factor
                let lo = floor(self.rect_pos * dpi + 0.5) / dpi - self.rect_pos
                let hi = floor((self.rect_pos + self.rect_size) * dpi + 0.5) / dpi - self.rect_pos
                let sdf = Sdf2d.viewport_px(self.pos * self.rect_size, dpi)
                sdf.box(lo.x, lo.y, hi.x - lo.x, hi.y - lo.y, fab.radius_lg)
                sdf.fill(fab.color_field)
                return sdf.result
            }
        }
    }

    // the job strip's slot (the status bar's)
    mod.widgets.FabJobsSlotBase = #(FabJobsSlot::register_widget(vm))

    mod.widgets.FabTopBar = View{
        width: Fill
        height: Fill
        flow: Overlay

        // The right side, against the caption's right end: the job strip, XR, undo and redo, the
        // workspace menu, then the assistant and Print.
        ends := View{
            width: Fill
            height: Fill
            flow: Right
            align: Align{x: 1.0 y: 0.5}
            padding: Inset{right: 8}
            right := View{
                width: Fit
                height: Fill
                flow: Right
                align: Align{x: 1.0 y: 0.5}
                spacing: 2
                // What the project requires, met of all: shown while a project has requirements.
                requirements_tip := FabTip{
                    requirements := mod.widgets.FabButton{
                        visible: false height: 24
                        draw_text +: {
                            failing: instance(0.0) unknown: instance(0.0)
                            get_color: fn() {
                                return fab.color_ok.mix(fab.color_warning, self.unknown).mix(fab.color_error, self.failing)
                            }
                        }
                    }
                }
                // (the job strip is the status bar's: `statusbar`)
                // Always this size, icon or not, so nothing moves when it comes and goes.
                also_slot := View{width: 28 height: 26
                    also_tip := FabTip{ text: "This plate is also open in another Fab window."
                        also := mod.widgets.FabIcon{
                            visible: false
                            margin: Inset{left: 7 top: 5}
                            a11y: {label: "This plate is also open in another Fab window"}
                            draw_icon +: { color: fab.color_warning svg: crate_resource("self://resources/icons/duplicate.svg") }
                        }
                    }
                }
                // Always available: starts the XR server and pairs a device by its PIN.
                xr_caption := mod.widgets.XrCaptionButton{app: "fab" margin: Inset{left: 0 right: 0}}
                // history | the workspace menu (it names the shown one) and its + | the assistant |
                // Print (the printer camera is the Printers editor's)
                FabTip{ text: #(keymap::tooltip(Key::Undo))
                    undo := CapIcon{ a11y: {label: "Undo"} draw_icon +: { svg: crate_resource("self://resources/icons/undo.svg") } }
                }
                FabTip{ text: #(keymap::tooltip(Key::Redo))
                    redo := CapIcon{ a11y: {label: "Redo"} draw_icon +: { svg: crate_resource("self://resources/icons/redo.svg") } }
                }
                CapRule{}
                workspaces := mod.widgets.FabWorkspaceTabs{}
                CapRule{}
                FabTip{ text: #(keymap::tooltip(Key::Chat))
                    chat := CapIcon{ a11y: {label: "AI assistant"} draw_icon +: { svg: crate_resource("self://resources/icons/spark.svg") } }
                }
                // Fab's own settings (how the 3D views take the mouse, ...) in their flyout
                FabTip{ text: "Settings: how the 3D views take the mouse"
                    settings := CapIcon{ a11y: {label: "Settings"} draw_icon +: { svg: crate_resource("self://resources/icons/cog.svg") } }
                }
                CapRule{}
                FabTip{ text: "Print the sliced plate (asks first)"
                    print := mod.widgets.FabButtonAccent{ text: "Print" height: 24 padding: Inset{left: 14 right: 14} }
                }
            }
        }

        // The document's name and state at the centre of the window, under the two sides (it
        // takes no presses: the caption around it still drags the window).
        middle := View{
            width: Fill
            height: Fill
            align: Align{x: 0.5 y: 0.5}
            // centred on the window, not the bar: the bar starts 40 in (past the traffic lights'
            // room), so as much is kept on its right
            padding: Inset{right: 40}
            title := View{
                width: Fit
                height: Fit
                flow: Right
                spacing: 6
                align: Align{y: 0.5}
                doc_name := mod.widgets.FabLabel{
                    text: "Untitled" max_lines: 1 text_overflow: Ellipsis flow: Right
                    hover_actions_enabled: true
                    draw_text +: { text_style: theme.font_bold{ font_size: fab.font_size_title } }
                }
                doc_state := mod.widgets.FabLabelSmall{ text: "" }
            }
        }

        // The menus; the rest up to the right side drags the window.
        starts := View{
            width: Fill
            height: Fill
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 4}
            spacing: 6
            left := View{
                width: Fit
                height: Fill
                flow: Right
                align: Align{x: 0.0 y: 0.5}
                spacing: 2
                cube := FabIcon{
                    margin: Inset{left: 6 right: 4}
                    icon_walk: Walk{ width: 16 height: 16 }
                    draw_icon +: {
                        svg: crate_resource("self://resources/icons/fab.svg")
                        color: fab.color_accent
                    }
                }
                // The app's menus, always in sight (the caption is the app's own).
                menus := View{
                    width: Fit height: Fill flow: Right align: Align{y: 0.5} spacing: 0
                    m_file := mod.widgets.FabMenuButton{ tag: @fab_menu_file owner: @fab_menu_file height: 24 label +: { text: "File" } }
                    m_edit := mod.widgets.FabMenuButton{ tag: @fab_menu_edit owner: @fab_menu_edit height: 24 label +: { text: "Edit" } }
                    m_view := mod.widgets.FabMenuButton{ tag: @fab_menu_view owner: @fab_menu_view height: 24 label +: { text: "View" } }
                    m_help := mod.widgets.FabMenuButton{ tag: @fab_menu_help owner: @fab_menu_help height: 24 label +: { text: "Help" } }
                }
            }
        }
    }
}

/// The job strip's slot: its tooltip names the shown job (its whole line), unless the save's
/// status holds the slot while the app quits (`quit_status`).
#[derive(Script, ScriptHook, Widget)]
struct FabJobsSlot {
    #[deref] view: View,
    #[rust] tip_version: Option<(Option<jobs::JobId>, u64)>,
}

impl Widget for FabJobsSlot {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let tip = self.view.widget(cx, ids!(jobs_tip));
        let strip = tip.child(id!(strip));
        let job = strip.borrow::<JobStrip>().and_then(|s| s.shown_job());
        let version = (job, jobs::board().seq());
        if !tip.child(id!(quit_line)).visible() && self.tip_version != Some(version) {
            let text = job.and_then(|id| jobs::board().get(id)).map_or_else(String::new, |j| j.line());
            tip.as_tip().set_text(&text);
            self.tip_version = Some(version);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// The caption's controls, from the app's action pass (`bar` is the
/// `FabTopBar` view).
pub fn handle_actions(cx: &mut Cx, bar: &WidgetRef, actions: &Actions, state: &AppState) {
    // A plain label can report hover and still let its part of the caption drag the window.
    let name = bar.label(cx, ids!(middle.title.doc_name));
    if let Some(rect) = name.hover_in(actions) {
        cx.widget_action(name.widget_uid(), TipAction::HoverIn(name.text(), rect));
    } else if name.hover_out(actions) {
        cx.widget_action(name.widget_uid(), TipAction::HoverOut);
    }
    workspace_actions(cx, bar, actions, state);
    if bar.button(cx, ids!(right.undo)).clicked(actions) {
        cx.action(ShellAction::Command("undo".into()));
    }
    if bar.button(cx, ids!(right.redo)).clicked(actions) {
        cx.action(ShellAction::Command("redo".into()));
    }
    if bar.button(cx, ids!(right.requirements)).clicked(actions) {
        cx.action(ShowPanel(Editor::Requirements));
    }
    if bar.button(cx, ids!(right.print)).clicked(actions) {
        cx.action(ShowPanel(Editor::Printer));
        // The slice to print is the one the view shows; a plate not sliced yet slices now, and
        // the confirm card says how far it is and what still stands in the way.
        if !state.print.can_send() {
            cx.action(ShellAction::SetRenderMode(RenderMode::Slice));
        }
        cx.action(PrintAction::AskPrint(true));
    }
    if bar.button(cx, ids!(right.chat)).clicked(actions) {
        cx.action(ShellAction::Command("toggle chat".into()));
    }
    // Settings: its flyout under the cog, and what is picked there
    let cog = bar.widget(cx, ids!(right.settings));
    if bar.button(cx, ids!(right.settings)).clicked(actions) {
        open_menu(cx, SETTINGS_MENU, settings_menu(state), cog.area().rect(cx), MenuPlace::Below);
    }
    if let Some(pick) = menu_picked(actions, SETTINGS_MENU) {
        if let Some(scheme) = NavScheme::ALL.into_iter().find(|s| nav_row(*s) == pick) {
            cx.action(crate::ui::stage::StageAction::Nav(scheme));
        } else if pick == live_id!(settings_invert_wheel) {
            cx.action(crate::ui::stage::StageAction::InvertWheel(!state.ui.invert_wheel));
        }
    }
    for action in ui_actions(actions) {
        if let FabUiAction::MenuBarOpen { owner, anchor, press } = action {
            let items = if *owner == live_id!(fab_menu_all) {
                menus().into_iter().map(|(id, title, build)| MenuItem::new(id, title).flyout(build(state))).collect()
            } else if let Some((_, _, build)) = menus().into_iter().find(|(id, _, _)| id == owner) {
                build(state)
            } else {
                continue;
            };
            cx.action(FabUiAction::OpenMenu { owner: *owner, items, anchor: *anchor, place: MenuPlace::Below, menu_bar: true, press: *press });
        }
    }
    for owner in menus().map(|(id, _, _)| id).into_iter().chain([live_id!(fab_menu_all)]) {
        if let Some(action) = menu_picked(actions, owner).and_then(|pick| menu_action_for(state, pick)) {
            cx.action(action);
        }
    }
}

/// The owner of the caption's Settings flyout.
const SETTINGS_MENU: LiveId = live_id!(fab_settings);

/// A 3D navigation scheme's row in Settings.
fn nav_row(scheme: NavScheme) -> LiveId {
    LiveId::from_str(&format!("settings_nav_{}", scheme.key()))
}

/// Settings: how the 3D views take the mouse, as other 3D programs do (each scheme's gestures
/// beside its name), and the wheel's zoom the other way. O(schemes).
fn settings_menu(state: &AppState) -> Vec<MenuItem> {
    let mut items = vec![MenuItem::heading("3D navigation")];
    items.extend(NavScheme::ALL.into_iter().map(|s| MenuItem::new(nav_row(s), s.name()).radio(state.ui.nav == s).note(s.summary())));
    items.push(MenuItem::sep());
    items.push(MenuItem::new(live_id!(settings_invert_wheel), "Invert zoom wheel").checked(state.ui.invert_wheel));
    items
}

/// The owners of the workspaces' menus: a tab's, `+`'s and `⌄`'s.
const TAB_MENU: LiveId = live_id!(fab_ws_tab_menu);
const ADD_MENU: LiveId = live_id!(fab_ws_add_menu);
const ALL_MENU: LiveId = live_id!(fab_ws_all_menu);
/// A tab menu's rows carry the workspace in the id's low half, their command in the high.
const ROW_RENAME: u64 = 0x7773_7231_0000_0000;
const ROW_DUPLICATE: u64 = 0x7773_7232_0000_0000;
const ROW_RESET: u64 = 0x7773_7233_0000_0000;
const ROW_UNDO: u64 = 0x7773_723a_0000_0000;
const ROW_DELETE: u64 = 0x7773_7234_0000_0000;
const ROW_SHOW: u64 = 0x7773_7235_0000_0000;
/// `+`'s default rows carry the origin's place in [`Origin::ALL`].
const ROW_ADD: u64 = 0x7773_7236_0000_0000;
const ROW_KEEP: u64 = 0x7773_7237_0000_0000;
const ROW_BACK: u64 = 0x7773_7238_0000_0000;
const ROW_TEMP: u64 = 0x7773_7239_0000_0000;

fn row_of(id: LiveId) -> (u64, WorkspaceId) {
    (id.0 & !0xffff_ffff, WorkspaceId(id.0 as u32))
}

/// The workspaces' tabs and their menus: a pick shows one, a rename, a move, and the tab's,
/// `+`'s and `⌄`'s rows change the list ([`ShellAction::EditWorkspaces`]).
fn workspace_actions(cx: &mut Cx, bar: &WidgetRef, actions: &Actions, state: &AppState) {
    let list = &state.ui.workspaces;
    for action in actions.iter().filter_map(|a| a.downcast_ref::<WorkspaceTabAction>()) {
        match action {
            WorkspaceTabAction::Pick(id) => cx.action(ShellAction::SwitchWorkspace(*id)),
            WorkspaceTabAction::Dismiss => cx.action(ShellAction::EditWorkspaces(ListOp::Dismiss)),
            WorkspaceTabAction::Rename(id, name) => cx.action(ShellAction::EditWorkspaces(ListOp::Rename(*id, name.clone()))),
            WorkspaceTabAction::Move(id, to) => {
                // `to` is a place among the offered; the row's own place is the one before
                let offered = state.ui.offered_ids();
                let to = offered.get(*to).and_then(|at| list.position(*at)).unwrap_or(list.list.len() - 1);
                cx.action(ShellAction::EditWorkspaces(ListOp::Move(*id, to)));
            }
            WorkspaceTabAction::Menu(id, anchor) => {
                let Some(def) = list.get(*id) else { continue };
                let row = |base: u64, label: &str| MenuItem::new(LiveId(base | id.0 as u64), label);
                let undo = row(ROW_UNDO, "Undo layout change").enabled_if(list.can_undo(*id));
                let items = if Some(*id) == list.temp_id() {
                    vec![row(ROW_KEEP, "Keep"), row(ROW_RENAME, "Rename"), undo, MenuItem::sep(), row(ROW_BACK, "Back")]
                } else {
                    vec![
                        row(ROW_RENAME, "Rename"),
                        row(ROW_DUPLICATE, "Duplicate"),
                        row(ROW_RESET, "Reset").enabled_if(def.origin.is_some()),
                        undo,
                        MenuItem::sep(),
                        row(ROW_DELETE, "Delete").enabled_if(list.list.len() > 1),
                    ]
                };
                open_menu(cx, TAB_MENU, items, *anchor, MenuPlace::Below);
            }
            WorkspaceTabAction::Add(anchor) => {
                let add = |o: &Origin| MenuItem::new(LiveId(ROW_ADD | Origin::ALL.iter().position(|a| a == o).unwrap_or(0) as u64), o.name());
                let general: Vec<MenuItem> = Origin::ALL.iter().filter(|o| !o.electronics() && state.workspace_shown(o.kind())).map(add).collect();
                let electronics: Vec<MenuItem> = Origin::ALL.iter().filter(|o| o.electronics()).map(add).collect();
                let mut items = vec![
                    MenuItem::new(LiveId(ROW_DUPLICATE | list.current.0 as u64), "Duplicate this one").enabled_if(!list.temp_shown()),
                    MenuItem::new(LiveId(ROW_TEMP | list.current.0 as u64), "Temporary layout"),
                    MenuItem::sep(),
                ];
                items.extend(general);
                items.push(MenuItem::sep());
                items.extend(electronics);
                open_menu(cx, ADD_MENU, items, *anchor, MenuPlace::Below);
            }
            WorkspaceTabAction::All(anchor) => {
                let items = list.list.iter().filter(|d| state.ui.offered(d))
                    .map(|d| MenuItem::new(LiveId(ROW_SHOW | d.id.0 as u64), &d.name).radio(d.id == list.current)).collect();
                open_menu(cx, ALL_MENU, items, *anchor, MenuPlace::BelowRight);
            }
        }
    }
    for owner in [TAB_MENU, ADD_MENU, ALL_MENU] {
        let Some(pick) = menu_picked(actions, owner) else { continue };
        let (row, id) = row_of(pick);
        match row {
            ROW_RENAME => {
                if let Some(mut tabs) = bar.widget(cx, ids!(workspaces)).borrow_mut::<FabWorkspaceTabs>() {
                    tabs.begin_rename(cx, id);
                }
            }
            ROW_DUPLICATE => cx.action(ShellAction::EditWorkspaces(ListOp::Duplicate(id))),
            ROW_KEEP if Some(id) == list.temp_id() => cx.action(ShellAction::EditWorkspaces(ListOp::Keep(None))),
            ROW_BACK if Some(id) == list.temp_id() => cx.action(ShellAction::EditWorkspaces(ListOp::Dismiss)),
            ROW_TEMP => cx.action(ShellAction::EditWorkspaces(ListOp::Temporary { from: id, note: String::new(), by: ShownBy::Person })),
            ROW_RESET => cx.action(ShellAction::EditWorkspaces(ListOp::Reset(id))),
            ROW_UNDO => cx.action(ShellAction::EditWorkspaces(ListOp::UndoLayout(id))),
            ROW_DELETE => cx.action(ShellAction::EditWorkspaces(ListOp::Delete(id))),
            ROW_SHOW => cx.action(ShellAction::SwitchWorkspace(id)),
            ROW_ADD => {
                if let Some(origin) = Origin::ALL.get(id.0 as usize) {
                    cx.action(ShellAction::EditWorkspaces(ListOp::Add(*origin)));
                }
            }
            _ => {}
        }
    }
}

/// The temporary layout's tab, shown beside the person's row.
#[derive(Clone, PartialEq, Debug)]
pub struct TempTab {
    pub id: WorkspaceId,
    pub title: String,
    pub ai: bool,
}

/// What the caption shows, read from the state: the app re-syncs the caption when it changes.
/// Cost: a few short string copies per call.
#[derive(Clone, PartialEq, Debug)]
pub struct Caption {
    pub title: String,
    pub info: String,
    /// The offered workspaces in the row's order, and the one shown.
    pub tabs: Vec<(WorkspaceId, String)>,
    pub current: WorkspaceId,
    pub temp: Option<TempTab>,
    /// The shown one is Design's (its code toggle shows).
    pub design: bool,
    pub code: bool,
    /// The plate is also open in another Fab window.
    pub also_open: bool,
}

impl Caption {
    pub fn of(state: &AppState) -> Caption {
        let name = if state.ui.document_name.is_empty() { "Untitled" } else { state.ui.document_name.as_str() };
        // a `*` while the plate's edits are not in its file
        let modified = state.plates.bed().is_some_and(|b| b.modified);
        Caption {
            title: if modified { format!("{name} *") } else { name.to_string() },
            info: state.ui.document_info.clone(),
            tabs: state.ui.workspaces.list.iter().filter(|d| state.ui.offered(d)).map(|d| (d.id, d.name.clone())).collect(),
            current: state.ui.workspaces.current,
            temp: state.ui.workspaces.temp.as_ref().map(|t| TempTab {
                id: t.def.id,
                title: t.def.name.clone(),
                ai: t.by == ShownBy::Ai,
            }),
            design: state.ui.kind() == Workspace::Design,
            code: state.ui.code_shown(),
            also_open: state.ui.also_open,
        }
    }
}

/// The app compares report Arc pointers in its caption update after non-draw events.
/// Only a different report or a reapplied caption changes this button and its tooltip.
pub fn sync_requirements(cx: &mut Cx, bar: &WidgetRef, shown: Option<&ReqShown>) {
    let chip = bar.button(cx, ids!(right.requirements));
    chip.set_visible(cx, shown.is_some());
    if let Some(shown) = shown {
        let report = &shown.report;
        let (met, total) = report.met_count();
        chip.set_text(cx, &format!("Requirements {met}/{total}"));
        bar.tip(cx, ids!(right.requirements_tip)).set_text(&format!("What the project requires: {met} of {total} met"));
        if let Some(mut button) = chip.borrow_mut() {
            button.draw_text.set_dyn_instance(cx, id!(failing), &[u8::from(report.fail > 0) as f32]);
            button.draw_text.set_dyn_instance(cx, id!(unknown), &[u8::from(report.unknown > 0) as f32]);
            button.redraw(cx);
        }
    }
}

/// Pin the save status in the status bar's job slot (`bar` is the `FabStatusBar`). Clearing it
/// restores the strip's ordinary job selection and tooltip.
pub fn quit_status(cx: &mut Cx, bar: &WidgetRef, text: Option<&str>) {
    let slot = bar.widget(cx, ids!(jobs));
    let Some(mut slot) = slot.borrow_mut::<FabJobsSlot>() else { return };
    let tip = slot.view.widget(cx, ids!(jobs_tip));
    let line = tip.child(id!(quit_line));
    line.set_visible(cx, text.is_some());
    tip.child(id!(strip)).set_visible(cx, text.is_none());
    if let Some(text) = text {
        line.set_text(cx, text);
        tip.as_tip().set_text(text);
    } else {
        slot.tip_version = None;
    }
    drop(slot);
    bar.redraw(cx);
}

/// Bring the caption up to date with its model (before a draw).
pub fn sync(cx: &mut Cx, bar: &WidgetRef, caption: &Caption) {
    bar.label(cx, ids!(middle.title.doc_name)).set_text(cx, &caption.title);
    // (what is on the plate is the status bar's to say)
    bar.label(cx, ids!(middle.title.doc_state)).set_text(cx, "");
    bar.widget(cx, ids!(also)).set_visible(cx, caption.also_open);
    if let Some(mut tabs) = bar.widget(cx, ids!(workspaces)).borrow_mut::<FabWorkspaceTabs>() {
        tabs.set(cx, caption.tabs.clone(), caption.current, caption.temp.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_changes_with_each_displayed_input() {
        let mut state = AppState::default();
        let caption = Caption::of(&state);
        assert_eq!(caption.title, "Untitled", "empty document_name uses Untitled");

        state.ui.document_name = "Bracket".into();
        assert_ne!(Caption::of(&state), caption, "document_name changes the caption");

        let caption = Caption::of(&state);
        state.ui.document_info = "1 part".into();
        assert_ne!(Caption::of(&state), caption, "document_info changes the caption");

        let caption = Caption::of(&state);
        state.ui.enter(Workspace::Mechanics);
        assert_ne!(Caption::of(&state), caption, "workspace changes the caption");

        let caption = Caption::of(&state);
        state.ui.arm_shown = !state.ui.arm_shown;
        assert_ne!(Caption::of(&state), caption, "arm_shown changes the caption");

        let caption = Caption::of(&state);
        state.ui.scan_shown = !state.ui.scan_shown;
        assert_ne!(Caption::of(&state), caption, "scan_shown changes the caption");

        let caption = Caption::of(&state);
        let first = state.ui.workspaces.list[0].id;
        state.ui.workspaces.apply(&crate::workspaces::ListOp::Rename(first, "Model".into()));
        assert_ne!(Caption::of(&state), caption, "a workspace renamed changes the caption");

        let caption = Caption::of(&state);
        assert!(state.apply_core(&ShellAction::EditWorkspaces(ListOp::Temporary { from: first, note: "Net VCC".into(), by: ShownBy::Ai })));
        let shown = Caption::of(&state);
        assert_ne!(shown, caption, "a temporary layout changes the caption");
        assert_eq!(shown.temp, Some(TempTab { id: state.ui.workspaces.current, title: "Net VCC".into(), ai: true }));
        assert!(state.apply_core(&ShellAction::EditWorkspaces(ListOp::Dismiss)), "Back closes it");

        let caption = Caption::of(&state);
        assert!(state.apply_core(&ShellAction::ShowCode(!state.ui.code_shown())), "the code toggle changes Design");
        assert_ne!(Caption::of(&state), caption, "code_shown changes the caption");

        state.plates.plates.push(PlateCard {
            id: "plate".into(),
            demo: false,
            group: String::new(),
            name: "Bracket".into(),
            modified: false,
            printer: String::new(),
            when: String::new(),
            file: None,
            note: String::new(),
            why: String::new(),
            picture: None,
        });
        state.plates.on_bed = Some("plate".into());
        let caption = Caption::of(&state);
        state.plates.plates[0].modified = true;
        let modified = Caption::of(&state);
        assert_ne!(modified, caption, "plate modified changes the caption");
        assert_eq!(modified.title, "Bracket *", "plate modified appends the modified mark");
    }
}
