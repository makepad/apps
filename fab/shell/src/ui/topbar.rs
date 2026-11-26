//! Fab's caption: the window's own title bar carries the app's main
//! controls. `main.rs` hangs [`FabTopBar`] in the window's caption bar; the
//! platform keeps its window buttons, and everything that is not a control
//! (empty space, the title, the state) drags the window. That is why the bar
//! is plain views: the window walks its views to find the controls, and a
//! custom widget would read as one big control. The app runs it through
//! [`handle_actions`] and [`sync`].
//!
//! Left: the cube, the app's menus (File, Edit, View, Help), the document's name and
//! state and undo / redo, followed by the person's workspace tabs (Cmd+1-9) and the
//! temporary layout's tab ([`crate::ui::workspace_tabs`]). Right: requirements, the job strip, a
//! reserved slot for the second-window icon (a plate also open in another Fab window; the slot is
//! always there, so the icon coming and going never moves the tab row), the headset,
//! the code toggle, the AI column and Print. The trailing controls draw first. One rect read
//! drives every fold once the scrolling tabs reach their minimum. The chip shows its count,
//! the document state hides, the workspace tabs show icons, and the menus share one button.
//! The job line then shortens to its status mark. Last, the document name takes the remaining
//! width and ellipsizes, with its full name in the tooltip.
//!
//! The slice is a render mode of the plate view (`stage`), not a workspace. Print
//! only opens the printer panel's confirm row (and shows the slice, which slices
//! the plate); the safety-checked start stays there.

use crate::api::*;
use crate::ui::keymap::{self, ShortcutId as Key};
use crate::ui::job_strip::JOB_LINE_CHARS;
use crate::print::PrintAction;
use crate::ui::menus::{action_for as menu_action_for, edit_menu, file_menu, help_menu, view_menu};
use crate::ui::popover::{menu_picked, open_menu, ui_actions, FabUiAction, MenuItem, MenuPlace};
use crate::ui::dropdown::FabDropdownButton;
use crate::ui::editors::Editor;
use crate::ui::shell::ShowPanel;
use crate::ui::workspace_tabs::{FabWorkspaceTabs, WorkspaceTabAction};
use crate::workspaces::{ListOp, Origin, ShownBy, WorkspaceId};
use fab_elec_view::ReqShown;
use makepad_widgets_core::{job_strip::JobStrip, popover::Popover, tip::{TipAction, TipWidgetRefExt}};
use makepad_widgets_core::width_override::WidthOverride;
use makepad_widgets_core::*;

/// The caption's menus: owner id and items.
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

    // Caption glyphs in the quiet ink, the full ink under the pointer.
    let CapIcon = mod.widgets.FabIconButton{
        width: 28
        height: 26
        padding: Inset{left: 6 right: 6 top: 5 bottom: 5}
        draw_icon +: {color: fab.color_text_dim}
    }

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
                sdf.fill(theme.prop_field)
                return sdf.result
            }
        }
    }

    let CaptionFit = #(FabCaptionFit::register_widget(vm))
    let JobLineFit = #(FabJobLineFit::register_widget(vm))
    let RequirementSpace = #(FabRequirementSpace::register_widget(vm))
    let control_gap = 2

    mod.widgets.FabTopBar = View{
        width: Fill
        height: Fill
        flow: Overlay

        // The right side first, against the caption's right end: the workspaces' row ends where
        // it starts (`FabWorkspaceTabs::follow`), so a job on the strip takes its room from the
        // row, never from the window's edge.
        ends := View{
            width: Fill
            height: Fill
            flow: Right
            align: Align{x: 1.0 y: 0.5}
            padding: Inset{right: 8}
            requirement_space := RequirementSpace{width: Fit height: Fit margin: Inset{right: control_gap}}
            right := View{
                width: Fit
                height: Fill
                flow: Right
                align: Align{x: 1.0 y: 0.5}
                spacing: control_gap
                // Keep the normal gap between the job strip (drawn against this edge) and the icons.
                View{width: 0 height: 0}
                // Always this size, icon or not, so the tab row never moves when it comes and goes.
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
                FabTip{ text: "The code beside the view (`)"
                    code_toggle := mod.widgets.FabIconCheck{ a11y: {label: "Code"} width: 28 height: 26 padding: Inset{left: 6 right: 6 top: 5 bottom: 5} draw_icon +: { svg: crate_resource("self://resources/icons/code.svg") } }
                }
                FabTip{ text: #(keymap::tooltip(Key::Chat))
                    chat := CapIcon{ a11y: {label: "AI assistant"} draw_icon +: { svg: crate_resource("self://resources/icons/spark.svg") } }
                }
                FabTip{ text: "Print the sliced plate (asks first)"
                    print := mod.widgets.FabButtonAccent{ text: "Print" height: 24 padding: Inset{left: 14 right: 14} }
                }
            }
        }

        // Fit reads the trailing controls once. The chip ends just before them.
        fit := CaptionFit{
            jobs_tip := FabTip{wrap_width: 360 height: Fill flow: Right align: Align{y: 0.5}
                // Quit owns the strip's line while the save waits, even when
                // an unrelated job would normally be selected for this slot.
                quit_line := Label{
                    visible: false width: Fit height: Fit
                    draw_text +: {color: fab.color_text text_style: theme.font_regular{font_size: fab.font_size_small}}
                }
                strip := mod.widgets.FabJobStrip{
                    max_chars: #(JOB_LINE_CHARS)
                    line_fit := JobLineFit{width: 0 height: 0}
                    pop +: {line +: {draw_text +: {
                        normal_ink: mod.widgets.ButtonFlat.draw_text.get_color
                        mark: instance(0.0) failed_mark: instance(0.0)
                        get_color: fn() {
                            return self.normal_ink().mix(fab.color_accent.mix(fab.color_error, self.failed_mark), self.mark)
                        }
                    }}}
                }
            }
        }
        chip_row := View{
            width: Fit height: Fill flow: Right align: Align{x: 1.0 y: 0.5}
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
        }

        // The document and history, then the workspaces right after them (as in Blender); the
        // rest up to the right side drags the window.
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
                // The app menus share a button only after the tabs have folded.
                menus := View{
                    width: Fit height: Fill flow: Right align: Align{y: 0.5} spacing: 0
                    m_file := mod.widgets.FabMenuButton{ tag: @fab_menu_file owner: @fab_menu_file height: 24 label +: { text: "File" } }
                    m_edit := mod.widgets.FabMenuButton{ tag: @fab_menu_edit owner: @fab_menu_edit height: 24 label +: { text: "Edit" } }
                    m_view := mod.widgets.FabMenuButton{ tag: @fab_menu_view owner: @fab_menu_view height: 24 label +: { text: "View" } }
                    m_help := mod.widgets.FabMenuButton{ tag: @fab_menu_help owner: @fab_menu_help height: 24 label +: { text: "Help" } }
                }
                compact_menu := mod.widgets.FabMenuButton{ visible: false owner: @fab_menu_all height: 24 label +: {text: "Menu"} }
                title := View{
                    width: Fit
                    height: Fit
                    flow: Right
                    spacing: 6
                    align: Align{y: 0.5}
                    margin: Inset{left: 4 right: 6}
                    doc_name := mod.widgets.FabLabel{
                        text: "Untitled" max_lines: 1 text_overflow: Ellipsis flow: Right
                        hover_actions_enabled: true
                        draw_text +: { text_style: theme.font_bold{ font_size: fab.font_size_title } }
                    }
                    doc_state := mod.widgets.FabLabelSmall{ text: "" }
                }
                FabTip{ text: #(keymap::tooltip(Key::Undo))
                    undo := CapIcon{ a11y: {label: "Undo"} draw_icon +: { svg: crate_resource("self://resources/icons/undo.svg") } }
                }
                FabTip{ text: #(keymap::tooltip(Key::Redo))
                    redo := CapIcon{ a11y: {label: "Redo"} draw_icon +: { svg: crate_resource("self://resources/icons/redo.svg") } }
                }
            }

            // the workspaces: the person's row, the shown one lit
            workspaces := mod.widgets.FabWorkspaceTabs{}
        }
    }
}

#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum CaptionFold {
    Full,
    Count,
    HideState,
    Icons,
    Menu,
}

/// Room after the fixed controls and the minimum scrolling row, before the optional pieces.
fn caption_fold(room: f64, full: f64, count: f64, state: f64, temp: f64, icon: f64) -> CaptionFold {
    if state + temp + full <= room {
        CaptionFold::Full
    } else if state + temp + count <= room {
        CaptionFold::Count
    } else if temp + count <= room {
        CaptionFold::HideState
    } else if icon + count <= room {
        CaptionFold::Icons
    } else {
        CaptionFold::Menu
    }
}

/// Intrinsic width of this caption's expanded left row, independent of its current fold.
/// Its Fit containers are horizontal views and the menu titles' label-and-padding views.
fn expanded_width(cx: &mut Cx2d, widget: &WidgetRef) -> f64 {
    fn row(cx: &mut Cx2d, view: &View) -> f64 {
        let mut widths = view.children.iter().filter(|(id, child)| {
            *id != id!(compact_menu) && (*id == id!(menus) || *id == id!(doc_state) || child.visible())
        }).map(|(_, child)| expanded_width(cx, child));
        let first = widths.next().unwrap_or_default();
        view.walk.margin.width() + view.layout.padding.width() + first
            + widths.map(|width| view.layout.spacing + width).sum::<f64>()
    }
    if let Some(view) = widget.borrow::<View>() {
        return row(cx, &view);
    }
    if let Some(menu) = widget.borrow::<FabDropdownButton>() {
        return row(cx, &menu);
    }
    widget.measure_width(cx, Some(&WidthOverride { visible: Some(true), ..Default::default() })).unwrap_or_default()
}

/// Walk the full chip's width before the job strip. This preserves the original drawing
/// origin of the trailing icons (their SVGs snap to pixels before the group is aligned).
/// The single interactive chip draws later, against the end of this space.
#[derive(Script, ScriptHook, Widget)]
struct FabRequirementSpace {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[redraw] #[rust] area: Area,
    #[rust] chip: WidgetRef,
    #[rust] full: WidthOverride,
    #[rust] width: f64,
}

impl Widget for FabRequirementSpace {
    fn handle_event(&mut self, _: &mut Cx, _: &Event, _: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _: &mut Scope, walk: Walk) -> DrawStep {
        self.width = self.chip.measure_width(cx, Some(&self.full)).unwrap_or_default();
        if self.width > 0.0 {
            cx.walk_turtle(Walk { width: Size::Fixed(self.width), height: Size::Fixed(0.0), ..walk });
        }
        DrawStep::done()
    }
}

/// JobStrip selects and dresses its controls just before its view draws. This zero-walk
/// child runs between those operations, when the line, count and × all belong to this draw.
#[derive(Script, ScriptHook, Widget)]
struct FabJobLineFit {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[redraw] #[rust] area: Area,
    #[rust] pop: WidgetRef,
    #[rust] budget: f64,
    #[rust] natural_width: f64,
    #[rust] unfolded: Option<String>,
    #[rust] hid_more: bool,
}

impl FabJobLineFit {
    fn prepare(&mut self, cx: &mut Cx, budget: f64) {
        self.budget = budget;
        self.natural_width = 0.0;
        // Give JobStrip its own text back for its change detection, without queuing a redraw.
        if let Some(text) = self.unfolded.take() {
            if let Some(mut line) = self.pop.child(id!(line)).borrow_mut::<Button>() {
                line.text.set(&text);
            }
        }
        if self.hid_more {
            self.pop.child(id!(more)).set_visible(cx, true);
            self.hid_more = false;
        }
    }
}

impl Widget for FabJobLineFit {
    fn handle_event(&mut self, _: &mut Cx, _: &Event, _: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _: &mut Scope, _: Walk) -> DrawStep {
        if !self.pop.visible() { return DrawStep::done() }
        let line = self.pop.child(id!(line));
        let more = self.pop.child(id!(more));
        let cancel = self.pop.child(id!(cancel));
        let gap = self.pop.borrow::<Popover>().unwrap().layout.spacing;
        let controls: f64 = [&more, &cancel].into_iter().filter(|w| w.visible())
            .map(|w| w.measure_width(cx, None).unwrap_or_default() + gap).sum();
        let width = line.measure_width(cx, None).unwrap_or_default();
        self.natural_width = width + controls;
        let Some(mut button) = line.borrow_mut::<Button>() else { return DrawStep::done() };
        let mut mark = false;
        if self.natural_width > self.budget {
            let original = button.text();
            let chars: Vec<char> = original.chars().collect();
            let prefix = |n: usize| chars[..n].iter().copied().chain(['…']).collect::<String>();
            let fits = |cx: &mut Cx2d, btn: &mut Button, n: usize| {
                btn.measure_width(cx, Some(&WidthOverride { text: Some(prefix(n)), ..Default::default() }))
                    .is_some_and(|w| w + controls <= self.budget)
            };
            // Twelve readable characters justify a line. Price the real glyphs in the job's font.
            let mut lo = 12.min(chars.len().saturating_sub(1));
            mark = lo < 12 || !fits(cx, &mut button, lo);
            let label = if mark {
                let mut failed = [0.0];
                button.draw_bg.get_instance(cx, id!(failed), &mut failed);
                button.draw_text.set_dyn_instance(cx, id!(failed_mark), &failed);
                if more.visible() {
                    // Its old hit area is gone in this draw. Restoring the model visibility before
                    // the next JobStrip update must not schedule another frame for that old area.
                    more.borrow_mut::<Button>().unwrap().draw_bg.draw_vars.area = Area::Empty;
                    more.set_visible(cx, false);
                    self.hid_more = true;
                }
                "●".to_owned()
            } else {
                let mut hi = chars.len();
                while lo + 1 < hi {
                    let mid = lo + (hi - lo) / 2;
                    if fits(cx, &mut button, mid) { lo = mid } else { hi = mid }
                }
                prefix(lo)
            };
            button.text.set(&label);
            self.unfolded = Some(original);
        }
        button.draw_text.set_dyn_instance(cx, id!(mark), &[u8::from(mark) as f32]);
        DrawStep::done()
    }
}

/// Draw the job strip once against the measured trailing controls, then place the chip and tabs.
#[derive(Script, ScriptHook, Widget)]
struct FabCaptionFit {
    #[deref] view: View,
    #[rust] starts: WidgetRef,
    #[rust] chip_row: WidgetRef,
    #[rust] space: WidgetRef,
    #[rust] count: WidthOverride,
    #[rust] tip_version: Option<(Option<jobs::JobId>, u64)>,
}

impl Widget for FabCaptionFit {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, _: Walk) -> DrawStep {
        let workspaces = self.starts.child(id!(workspaces));
        let Some(mut tabs) = workspaces.borrow_mut::<FabWorkspaceTabs>() else { return DrawStep::done() };
        let bar = cx.turtle().rect();
        let right = tabs.trailing_start(cx, bar);
        let chip = self.chip_row.child(id!(requirements_tip)).child(id!(requirements));
        let Some(space) = self.space.borrow::<FabRequirementSpace>() else { return DrawStep::done() };
        let full = space.width;
        let chip_end = if full > 0.0 { right - space.walk.margin.right } else { right };
        let count = chip.measure_width(cx, Some(&self.count)).unwrap_or_default();
        let left = self.starts.child(id!(left));
        let title = left.child(id!(title));
        let state = title.child(id!(doc_state));
        let state_width = expanded_width(cx, &state) + title.borrow::<View>().unwrap().layout.spacing;
        let starts = self.starts.borrow::<View>().unwrap();
        let tabs_at = bar.pos.x + starts.layout.padding.left + expanded_width(cx, &left) + starts.layout.spacing;
        let (minimum, temp, icon) = tabs.prepare_widths(cx);
        let room = chip_end - tabs_at + state_width - minimum;
        let strip = self.view.widget(cx, ids!(strip));
        let line_fit = strip.child(id!(line_fit));
        let menus = left.child(id!(menus));
        let compact_menu = left.child(id!(compact_menu));
        let menu_saving = expanded_width(cx, &menus) - expanded_width(cx, &compact_menu);
        let job_budget = room + menu_saving - count - icon;
        line_fit.borrow_mut::<FabJobLineFit>().unwrap().prepare(cx, job_budget);
        let jobs_tip = self.view.widget(cx, ids!(jobs_tip));
        cx.begin_turtle(Walk::fixed((right - bar.pos.x).max(0.0), bar.size.y),
            Layout { flow: Flow::right(), align: Align { x: 1.0, y: 0.5 }, ..Default::default() });
        let quit_line = jobs_tip.child(id!(quit_line));
        jobs_tip.draw_all(cx, scope);
        let job_width = cx.turtle().used_width();
        cx.end_turtle();
        let job = strip.borrow::<JobStrip>().and_then(|s| s.shown_job());
        let version = (job, jobs::board().seq());
        if !quit_line.visible() && self.tip_version != Some(version) {
            let text = job.and_then(|id| jobs::board().get(id)).map_or_else(String::new, |j| j.line());
            jobs_tip.as_tip().set_text(&text);
            self.tip_version = Some(version);
        }
        let chip_end = chip_end - job_width;
        // The job budget already reserves the full name with every other control compact.
        // Any remaining deficit comes out of the name. A capped Fit still measures at its
        // natural width, so the next draw can give the name and the earlier folds room again.
        if let Some(mut name) = title.child(id!(doc_name)).borrow_mut::<Label>() {
            let max = (job_width > job_budget).then(|| {
                let natural = name.measure_width(cx, None).unwrap_or_default();
                FitBound::Abs((natural + job_budget - job_width).max(0.0))
            });
            name.walk.width = Size::Fit { min: None, max };
        }
        let natural_job = if !quit_line.visible() { line_fit.borrow::<FabJobLineFit>().unwrap().natural_width } else { job_width };
        let fold = caption_fold(room - natural_job, full, count, state_width, temp, icon);
        let (width, label) = if fold == CaptionFold::Full { (full, &space.full) } else { (count, &self.count) };
        tabs.fit(chip_end - width, fold >= CaptionFold::Icons);
        if let Some(text) = &label.text {
            chip.set_text(cx, text);
        }
        state.set_visible(cx, fold < CaptionFold::HideState);
        menus.set_visible(cx, fold != CaptionFold::Menu);
        compact_menu.set_visible(cx, fold == CaptionFold::Menu);
        self.chip_row.borrow_mut::<View>().unwrap().walk.width = Size::Fixed((chip_end - bar.pos.x).max(0.0));
        DrawStep::done()
    }
}

/// The caption's controls, from the app's action pass (`bar` is the
/// `FabTopBar` view).
pub fn handle_actions(cx: &mut Cx, bar: &WidgetRef, actions: &Actions, state: &AppState) {
    // A plain label can report hover and still let its part of the caption drag the window.
    let name = bar.label(cx, ids!(left.title.doc_name));
    if let Some(rect) = name.hover_in(actions) {
        cx.widget_action(name.widget_uid(), TipAction::HoverIn(name.text(), rect));
    } else if name.hover_out(actions) {
        cx.widget_action(name.widget_uid(), TipAction::HoverOut);
    }
    workspace_actions(cx, bar, actions, state);
    if bar.button(cx, ids!(left.undo)).clicked(actions) {
        cx.action(ShellAction::Command("undo".into()));
    }
    if bar.button(cx, ids!(left.redo)).clicked(actions) {
        cx.action(ShellAction::Command("redo".into()));
    }
    if bar.button(cx, ids!(chip_row.requirements)).clicked(actions) {
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
    if bar.radio_button(cx, ids!(right.code_toggle)).clicked(actions) {
        cx.action(ShellAction::ShowCode(!state.ui.code_shown()));
    }
    if bar.button(cx, ids!(right.chat)).clicked(actions) {
        cx.action(ShellAction::Command("toggle chat".into()));
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
    let chip = bar.button(cx, ids!(chip_row.requirements));
    chip.set_visible(cx, shown.is_some());
    if let Some(shown) = shown {
        let report = &shown.report;
        let (met, total) = report.met_count();
        set_requirement_counts(cx, bar, met, total);
        if let Some(mut button) = chip.borrow_mut() {
            button.draw_text.set_dyn_instance(cx, id!(failing), &[u8::from(report.fail > 0) as f32]);
            button.draw_text.set_dyn_instance(cx, id!(unknown), &[u8::from(report.unknown > 0) as f32]);
            button.redraw(cx);
        }
    }
}

fn set_requirement_counts(cx: &mut Cx, bar: &WidgetRef, met: usize, total: usize) {
    if let Some(mut space) = bar.widget(cx, ids!(requirement_space)).borrow_mut::<FabRequirementSpace>() {
        space.full.text = Some(format!("Requirements {met}/{total}"));
    }
    if let Some(mut fit) = bar.child(id!(fit)).borrow_mut::<FabCaptionFit>() {
        fit.count.text = Some(format!("{met}/{total}"));
    }
    bar.tip(cx, ids!(chip_row.requirements_tip)).set_text(&format!("What the project requires: {met} of {total} met"));
}

/// Pin the save status in the caption's job slot. Clearing it restores
/// the strip's ordinary job selection and tooltip.
pub fn quit_status(cx: &mut Cx, bar: &WidgetRef, text: Option<&str>) {
    let fit = bar.child(id!(fit));
    let Some(mut fit) = fit.borrow_mut::<FabCaptionFit>() else { return };
    let tip = fit.view.widget(cx, ids!(jobs_tip));
    let line = tip.child(id!(quit_line));
    line.set_visible(cx, text.is_some());
    tip.child(id!(strip)).set_visible(cx, text.is_none());
    if let Some(text) = text {
        line.set_text(cx, text);
        tip.as_tip().set_text(text);
    } else {
        fit.tip_version = None;
    }
    drop(fit);
    bar.redraw(cx);
}

/// Bring the caption up to date with its model (before a draw).
pub fn sync(cx: &mut Cx, bar: &WidgetRef, caption: &Caption) {
    bar.label(cx, ids!(left.title.doc_name)).set_text(cx, &caption.title);
    bar.label(cx, ids!(left.title.doc_state)).set_text(cx, &caption.info);
    bar.widget(cx, ids!(also)).set_visible(cx, caption.also_open);
    let space = bar.widget(cx, ids!(requirement_space));
    let chip = bar.widget(cx, ids!(requirements));
    if let Some(mut slot) = space.borrow_mut::<FabRequirementSpace>() {
        slot.chip = chip;
    }
    if let Some(mut fit) = bar.child(id!(fit)).borrow_mut::<FabCaptionFit>() {
        let strip = fit.view.widget(cx, ids!(strip));
        let line_fit = strip.child(id!(line_fit));
        line_fit.borrow_mut::<FabJobLineFit>().unwrap().pop = strip.child(id!(pop));
        if let Some(mut strip) = strip.borrow_mut::<JobStrip>() {
            // The inherited popover is the first child until Fab adds its pre-draw measurement.
            if let Some(index) = strip.child_index(&line_fit) {
                strip.swap_child(0, index);
            }
        }
        fit.space = space;
        fit.starts = bar.child(id!(starts));
        fit.chip_row = bar.child(id!(chip_row));
    }
    let right = bar.widget(cx, ids!(right));
    if let Some(mut tabs) = bar.widget(cx, ids!(workspaces)).borrow_mut::<FabWorkspaceTabs>() {
        tabs.follow(right);
        tabs.set(cx, caption.tabs.clone(), caption.current, caption.temp.clone());
    }
    let code = bar.radio_button(cx, ids!(right.code_toggle));
    code.set_visible(cx, caption.design);
    if code.active(cx) != caption.code {
        code.set_active(cx, caption.code, Animate::No);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledctl_caption_folds_only_as_far_as_it_needs() {
        use makepad_widgets_core::makepad_draw::cx_draw::CxDraw;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let root = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, WindowFamilies::default(), &[makepad_widgets_fab::fab_mod]);
            makepad_xr_stream::serve_script_mod(vm);
            crate::theme::script_mod(vm);
            crate::ui::script_mod_kit(vm);
            crate::ui::workspace_tabs::script_mod(vm);
            crate::ui::job_strip::script_mod(vm);
            super::script_mod(vm);
            // Same space outside Fab's caption as the live macOS window (traffic lights and JIT ring).
            let value = vm.eval(script! {
                use mod.prelude.widgets_internal.*
                use mod.widgets.*
                View{width: Fill height: 38 flow: Right padding: Inset{left: 72 right: 32} caption := FabTopBar{}}
            });
            WidgetRef::script_from_value(vm, value)
        });
        let bar = root.widget(&cx, ids!(caption));
        let chip = bar.widget(&cx, ids!(requirements));
        chip.set_visible(&mut cx, true);
        set_requirement_counts(&mut cx, &bar, 6, 11);
        let mut caption = Caption::of(&AppState::default());
        caption.info = "3 parts · 1 board · 60 solids".into();
        caption.design = false;
        let pass = DrawPass::new(&mut cx);
        let mut list = DrawList2d::new(&mut cx);
        let overlay = makepad_widgets_core::makepad_draw::overlay::Overlay { draw_list: DrawList::new(&mut cx) };
        let mut draw_caption = |cx: &mut Cx, width| {
            let size = dvec2(width, 38.0);
            pass.set_size(cx, size);
            cx.redraw_all();
            let event = std::mem::take(&mut cx.new_draw_event);
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.text_runs = Some(Default::default());
            frame.begin_pass(&pass, Some(1.0));
            list.begin_always(&mut frame);
            overlay.begin(&mut frame);
            frame.begin_root_turtle(size, makepad_widgets_core::Layout::flow_down());
            root.draw_all(&mut frame, &mut Scope::empty());
            overlay.end(&mut frame);
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
            frame.text_runs.take().unwrap()
        };
        let failure = "method no_such_shape not found while rebuilding the enclosure and its mounting plate";
        let mut failed_job = None;
        let long_name = "ledctl enclosure";
        let mut compact_controls = None;
        let name_uid = bar.widget(&cx, ids!(doc_name)).widget_uid().0;
        for (width, temporary, job, name) in [
            (640.0, false, false, "ledctl"), (640.0, true, false, "ledctl"), (1600.0, true, false, "ledctl"),
            (640.0, false, true, "ledctl"), (640.0, true, true, "ledctl"), (640.0, true, true, long_name),
            (1600.0, true, true, long_name), (1600.0, false, true, "ledctl"), (640.0, false, false, "ledctl"),
        ] {
            if job && failed_job.is_none() {
                let rebuild = jobs::JobSpec::new("AI: Rebuilding the plate").start();
                failed_job = Some(rebuild.id());
                rebuild.fail(failure, false);
            } else if !job {
                if let Some(id) = failed_job.take() { jobs::board().dismiss(id); }
            }
            // Design's code toggle beside the temporary tab leaves room for the status mark and ×.
            caption.design = job && temporary;
            caption.title = name.into();
            caption.temp = temporary.then(|| TempTab {
                id: WorkspaceId(99), title: if caption.design { "Design" } else { "Schematic" }.into(), ai: false,
            });
            sync(&mut cx, &bar, &caption);
            let runs = draw_caption(&mut cx, width);
            let drawn_name = runs.runs.iter().find(|run| run.owner == name_uid).expect("document name was drawn");
            assert_eq!(drawn_name.truncated, width == 640.0 && name == long_name, "{name} at {width}");
            let narrow = width == 640.0;
            assert_eq!(chip.text(), if narrow { "6/11" } else { "Requirements 6/11" });
            assert_eq!(bar.widget(&cx, ids!(doc_state)).visible(), !narrow);
            let row = bar.widget(&cx, ids!(workspaces));
            let left = row.area().rect(&cx);
            let right = chip.area().rect(&cx);
            assert!(left.size.x > 0.0 && right.size.x > 0.0 && left.pos.x + left.size.x <= right.pos.x,
                "width {width}, temporary {temporary}: workspace row {left:?}, chip {right:?}");
            let trailing = bar.widget(&cx, ids!(right)).area().rect(&cx);
            assert!(right.pos.x + right.size.x <= trailing.pos.x + 0.001, "chip {right:?}, trailing controls {trailing:?}");
            if job && temporary {
                let title = bar.label(&cx, ids!(doc_name));
                let title_rect = title.area().rect(&cx);
                if narrow {
                    let controls = [id!(cube), id!(compact_menu), id!(undo), id!(redo)]
                        .map(|id| bar.widget(&cx, &[id]).area().rect(&cx));
                    let [cube, menu, undo, redo] = controls;
                    let mut end = 0.0;
                    for rect in [cube, menu, title_rect, undo, redo, left, right] {
                        assert!(rect.size.x > 0.0 && end <= rect.pos.x + 0.001, "{name}: {rect:?} after x={end}");
                        end = rect.pos.x + rect.size.x;
                    }
                    let widths = controls.map(|rect| rect.size.x);
                    if name == long_name {
                        assert_eq!(compact_controls, Some(widths), "the name alone gives up width");
                        let hover = Event::MouseMove(MouseMoveEvent {
                            abs: title_rect.pos + title_rect.size * 0.5, lock_delta: Default::default(),
                            window_id: WindowId(1, 1), modifiers: Default::default(), time: 1.0, handled: Default::default(),
                        });
                        let actions = cx.capture_actions(|cx| root.handle_event(cx, &hover, &mut Scope::empty()));
                        let tips = cx.capture_actions(|cx| handle_actions(cx, &bar, &actions, &AppState::default()));
                        assert_eq!(tips.find_widget_action_cast::<TipAction>(title.widget_uid()),
                            TipAction::HoverIn(long_name.into(), title_rect));
                    } else {
                        compact_controls = Some(widths);
                    }
                }
            }
            if job {
                let line = bar.widget(&cx, ids!(strip.pop.line));
                let cancel = bar.widget(&cx, ids!(strip.pop.cancel));
                let line_rect = line.area().rect(&cx);
                let cancel_rect = cancel.area().rect(&cx);
                assert!(line_rect.size.x > 0.0 && right.pos.x + right.size.x <= line_rect.pos.x + 0.001,
                    "chip {right:?}, job line {line_rect:?}");
                assert!(cancel.visible() && line_rect.pos.x + line_rect.size.x <= cancel_rect.pos.x + 0.001
                    && cancel_rect.pos.x + cancel_rect.size.x <= trailing.pos.x + 0.001,
                    "line {line_rect:?}, × {cancel_rect:?}, trailing controls {trailing:?}");
                if narrow && temporary {
                    assert_eq!(line.text(), "●", "only the status mark fits");
                } else {
                    assert!(line.text().ends_with('…'), "long job line has an ellipsis");
                    if narrow { assert!(line.text().chars().count() < JOB_LINE_CHARS); }
                }
                let tip = bar.tip(&cx, ids!(jobs_tip));
                assert_eq!(tip.borrow().unwrap().text, format!("AI: Rebuilding the plate failed: {failure}"));
                assert!(tip.area().rect(&cx).contains(line_rect.pos + line_rect.size * 0.5), "the job mark is inside its tooltip");
            }
            if temporary {
                let tabs = row.borrow::<FabWorkspaceTabs>().unwrap();
                let tab = tabs.semantics_part_area(live_id!(fab_workspace_temp).0).rect(&cx);
                assert!(tab.size.x > 0.0 && (tab.size.x <= left.size.y) == narrow, "temporary tab {tab:?}");
                let close = tabs.semantics_part_area(live_id!(fab_workspace_temp_close).0);
                assert_eq!(close.is_empty(), narrow, "only the sparkle is drawn in the compact temporary tab");
            }
            // The second-window icon has a reserved slot: nothing else in the caption changes
            // when it comes and goes, so every text run keeps its owner, place and size.
            let kept_tabs = caption.tabs.clone();
            caption.tabs = (1..=4).map(|n| (WorkspaceId(n), format!("Workspace {n}"))).collect();
            sync(&mut cx, &bar, &caption);
            draw_caption(&mut cx, width);
            let off_runs = draw_caption(&mut cx, width);
            let places = |runs: &makepad_widgets_core::makepad_draw::cx_2d::TextRunSink| -> Vec<(u64, f64, f64, f64, f64, bool)> {
                runs.runs.iter().map(|r| (r.owner, r.rect.pos.x, r.rect.pos.y, r.rect.size.x, r.rect.size.y, r.truncated)).collect()
            };
            let also = bar.widget(&cx, ids!(also));
            let slot = bar.widget(&cx, ids!(also_slot));
            let slot_off = slot.area().rect(&cx);
            assert!(!also.visible() && slot_off.size.x == 28.0 && slot_off.size.y == 26.0, "{width}: the slot is reserved with the icon off ({slot_off:?})");
            assert!(off_runs.runs.len() > 4, "{width}: the caption drew its text");
            caption.also_open = true;
            sync(&mut cx, &bar, &caption);
            draw_caption(&mut cx, width);
            let on_runs = draw_caption(&mut cx, width);
            assert!(also.visible(), "{width}, job {job}: the icon shows while the plate is also open elsewhere");
            assert_eq!(slot.area().rect(&cx), slot_off, "{width}, job {job}: the slot changed size or place with the icon on");
            assert!(!also.area().rect(&cx).size.x.is_nan() && also.area().rect(&cx).size.x > 0.0, "{width}: the icon draws inside its slot");
            assert_eq!(places(&on_runs), places(&off_runs), "{width}, temporary {temporary}, job {job}: a text run moved or changed when the icon came");
            caption.tabs = kept_tabs;
            caption.also_open = false;
            sync(&mut cx, &bar, &caption);
        }
        let line = bar.widget(&cx, ids!(jobs_tip.quit_line));
        let line_id = line.widget_uid().0;
        for text in [Some("Saving the plate… 4 s"), Some("Model host stopped. Last edits unsaved. Journal entries kept."), None] {
            quit_status(&mut cx, &bar, text);
            let runs = draw_caption(&mut cx, 1600.0);
            assert_eq!(runs.runs.iter().any(|run| run.owner == line_id), text.is_some());
            if let Some(text) = text { assert_eq!(line.text(), text); }
        }
    }

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
