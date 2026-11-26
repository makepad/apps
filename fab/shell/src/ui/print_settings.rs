//! The slicer settings panel, every row generated from the plate's settings table. With
//! nothing selected it edits the plate's global set; with objects selected the same rows are
//! those objects' overrides of it, each marked, with a reset back to global; with a print
//! settings box picked, the settings a box can set, its own marked and the rest its object's.
//! Edits go out as [`PartAction`]s (a box's as its rows' [`ParamAction`]s): code is the truth
//! (`settings(...)` and `place(..., settings: ...)` in the project, `m.settings` in the part's
//! script), so undo, saving and the AI see them like any other edit.

use crate::api::{AppState, PartAction, PlatePart};
use crate::params::{ParamAction, ParamValue, ParamView};
use crate::print::PrintState;
use crate::ui::parts::{active_part, selected_parts};
use crate::ui::popover::{dropdown_clicked, menu_picked, open_menu, MenuItem, MenuPlace};
use fab_plate::{checked_value, print_setting, setting_number, setting_on, PrintSetting, SettingGroup, SettingKind, SettingScope, PRINT_SETTINGS};
use makepad_widgets_core::widget_tree::CxWidgetExt;
use makepad_widgets_core::*;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const COPY_MENU: LiveId = live_id!(fab_slicer_copy);

/// Choices up to this many sit as tabs in the row; more open a menu.
const TAB_CHOICES: usize = 4;
const MORE_PATTERNS: &str = "More patterns…";

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    let Reset = PropTrailButton{
        visible: false
        draw_icon +: { svg: crate_resource("self://resources/icons/refresh.svg") }
    }
    let Setting = View{width: Fill height: Fit flow: Down spacing: theme.prop_row_gap}

    mod.widgets.FabSlicerToolsBase = #(FabSlicerTools::register_widget(vm))
    mod.widgets.FabSlicerTools = set_type_default() do mod.widgets.FabSlicerToolsBase{
        width: Fill height: theme.prop_row_height flow: Right spacing: 4
        align: Align{y: 0.5}
    }

    mod.widgets.FabSlicerRowsBase = #(FabSlicerRows::register_widget(vm))
    mod.widgets.FabSlicerRows = set_type_default() do mod.widgets.FabSlicerRowsBase{
        width: Fill height: Fit flow: Down spacing: 0
        padding: Inset{left: theme.prop_pad right: theme.prop_pad top: 2 bottom: 12}
        Head := PropSection{
            body: View{
                width: Fill height: Fit flow: Down
                padding: Inset{top: 2 bottom: 6}
                content := View{width: Fill height: Fit flow: Down spacing: theme.prop_row_gap margin: Inset{left: theme.prop_pad right: theme.prop_pad}}
            }
        }
        Mode := PropRow{
            height: Fit margin: Inset{top: 6 bottom: 2}
            value +: {
                height: Fit
                title := mod.widgets.FabLabelSmall{
                    width: Fill padding: Inset{}
                    align: Align{x: 0.0 y: 0.5}
                }
            }
        }
        Num := Setting{
            row := FabNumberRow{
                value := mod.widgets.FabTip{width: Fill text: ""
                    // The sheet gives every number field a taller minimum than a property row (and the row's
                    // own height is fixed): the field takes the row's height, so its text centres in it.
                    num := ValueInput{width: Fill height: Fill min_height: 0}
                }
                trail +: {width: theme.prop_trail_width FabTip{text: "Remove the override: back to what it inherits" reset := Reset{}}}
            }
        }
        Bool := Setting{
            row := PropCheckRow{
                value := mod.widgets.FabTip{width: Fill text: ""
                    check := PropCheckBox{}
                }
                trail +: {width: theme.prop_trail_width FabTip{text: "Remove the override: back to what it inherits" reset := Reset{}}}
            }
        }
        Choice := Setting{
            row := PropSegmentedRow{
                value := mod.widgets.FabTip{width: Fill text: ""
                    choices := PropChoice{}
                }
                trail +: {width: theme.prop_trail_width FabTip{text: "Remove the override: back to what it inherits" reset := Reset{}}}
            }
        }
        Pick := Setting{
            row := PropDropDownRow{
                value := mod.widgets.FabTip{width: Fill text: ""
                    pick := PropDropDown{}
                }
                trail +: {width: theme.prop_trail_width FabTip{text: "Remove the override: back to what it inherits" reset := Reset{}}}
            }
        }
        // A pick whose short list says what each choice is for: its menu is as wide as those
        // words, not the field (the menu stays inside the window).
        PickNotes := Setting{
            row := PropDropDownRow{
                value := mod.widgets.FabTip{width: Fill text: ""
                    pick := PropDropDown{popup_menu: mod.widgets.PopupMenu{width: Fit}}
                }
                trail +: {width: theme.prop_trail_width FabTip{text: "Remove the override: back to what it inherits" reset := Reset{}}}
            }
        }
    }

    mod.widgets.FabSlicerPanelBase = #(FabSlicerPanel::register_widget(vm))
    mod.widgets.FabSlicerPanel = set_type_default() do mod.widgets.FabSlicerPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: { color: fab.color_editor }
        head := View{
            width: Fill height: Fit flow: Down spacing: 4
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: 8 bottom: 8}
            scope := PropSegmented{
                options: ["Global" "Object"]
                option_names: ["Global settings" "Object settings"]
            }
            title := mod.widgets.FabLabel{width: Fill text: "" draw_text +: {text_style: theme.font_bold{font_size: fab.font_size_ui}}}
            note := mod.widgets.FabLabelSmall{width: Fill text: ""}
            tools := mod.widgets.FabSlicerTools{
                search := mod.widgets.FabSearch{
                    width: Fill{min: 80}
                    row +: {padding: Inset{left: 6 right: 6} input +: {min_height: 0}}
                }
                FabTip{text: "Show advanced settings"
                    advanced := PropCheckBox{
                        width: Fit text: "Advanced"
                        label_walk +: {width: Fit}
                        a11y: {label: "Advanced"}
                    }
                }
                FabTip{text: "Give other objects this object's settings"
                    copy := mod.widgets.FabDropdownButton{
                        a11y: {label: "Copy settings to"} tag: @fab_slicer_copy owner: @fab_slicer_copy
                        height: theme.prop_row_height padding: Inset{left: 6 right: 6} spacing: 4
                        label := View{width: Fit height: Fill flow: Right align: Align{y: 0.5}
                            word := mod.widgets.FabLabel{text: "Copy to…"}
                            glyph := View{visible: false width: 14 height: Fill padding: Inset{top: 3 bottom: 3}
                                mod.widgets.FabIcon{
                                    width: 14 height: 14 icon_walk: Walk{width: 14 height: 14}
                                    draw_icon +: {svg: crate_resource("self://resources/icons/duplicate.svg")}
                                }
                            }
                        }
                        chevron_slot +: {
                            width: theme.prop_chevron padding: Inset{top: 6 bottom: 6}
                            chevron +: {width: theme.prop_chevron height: theme.prop_chevron icon_walk: Walk{width: theme.prop_chevron height: theme.prop_chevron}}
                        }
                    }
                }
                FabTip{text: "Every setting here back to what it inherits"
                    clear := mod.widgets.FabButton{
                        text: "Reset all" a11y: {label: "Reset all"}
                        height: theme.prop_row_height padding: Inset{left: 6 right: 6} spacing: 4
                        icon_walk: Walk{width: 14 height: Fill margin: Inset{top: 3 bottom: 3}}
                        draw_icon +: {svg: crate_resource("self://resources/icons/refresh.svg") color: fab.color_text}
                    }
                }
            }
        }
        body := mod.widgets.FabScroll{
            rows := mod.widgets.FabSlicerRows{}
        }
    }
}

#[derive(Script, Widget)]
pub struct FabSlicerTools {
    #[deref]
    view: View,
    #[rust]
    compact: Option<bool>,
}

impl ScriptHook for FabSlicerTools {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if !apply.is_eval() { self.compact = None; }
    }
}

impl Widget for FabSlicerTools {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let width = cx.peek_walk_turtle(walk).size.x;
        let copy = self.view.widget(cx, ids!(copy));
        let clear = self.view.widget(cx, ids!(clear));
        let full = width_override::WidthOverride { visible: Some(true), ..Default::default() };
        // Price the full faces even while compact, so widening restores their words.
        let needed = (|| {
            let mut needed = 80.0 + 4.0 + self.view.widget(cx, ids!(advanced)).measure_width(cx, None)?;
            if copy.visible() {
                let label = copy.widget(cx, ids!(label.word)).measure_width(cx, Some(&full))?;
                let chevron = copy.widget(cx, ids!(chevron_slot)).measure_width(cx, None)?;
                needed += 4.0 + label + 12.0 + 4.0 + chevron;
            }
            if clear.visible() {
                let full = width_override::WidthOverride { text: Some("Reset all".into()), ..full };
                needed += 4.0 + clear.measure_width(cx, Some(&full))?;
            }
            Some(needed)
        })();
        let compact = needed.is_none_or(|needed| needed > width);
        if self.compact != Some(compact) {
            copy.widget(cx, ids!(label.word)).set_visible(cx, !compact);
            copy.widget(cx, ids!(label.glyph)).set_visible(cx, compact);
            clear.set_text(cx, if compact { "" } else { "Reset all" });
            self.compact = Some(compact);
        }
        // View defers the search Fill until the following Fit faces are measured.
        self.view.draw_walk(cx, scope, walk)
    }
}

/// The name a setting's control answers to: its mode's or its group's, and its label.
pub fn control_name(s: &PrintSetting) -> String {
    if !s.mode.is_empty() {
        if s.mode == s.label { s.label.into() } else { format!("{} {}", s.mode, s.label) }
    } else {
        format!("{} {}", s.group.title(), s.label)
    }
}

struct SettingIds {
    row: LiveId,
    mode: Option<LiveId>,
    name: String,
}

/// Table-indexed ids and accessible names, shared by every panel instance.
fn setting_ids() -> &'static [SettingIds] {
    static IDS: OnceLock<Vec<SettingIds>> = OnceLock::new();
    IDS.get_or_init(|| PRINT_SETTINGS.iter().enumerate().map(|(i, s)| SettingIds {
        row: LiveId::from_str(&format!("setting.{}", s.key)),
        mode: (!s.mode.is_empty() && (i == 0 || PRINT_SETTINGS[i - 1].mode != s.mode)).then(|| LiveId::from_str(&format!("setting-mode:{}", s.mode))),
        name: control_name(s),
    }).collect())
}

fn head_id(group: SettingGroup) -> LiveId {
    LiveId::from_str(&format!("setting-group:{}", group.title()))
}

/// What the rows edit: the plate's global set, these parts' overrides of it, or a print
/// settings box's.
#[derive(Clone, Debug, PartialEq)]
enum Editing {
    Global,
    /// Part indices, the active one first.
    Parts(Vec<usize>),
    /// A print settings box (`inst/solid`) on plate part `part`: the settings a box sets
    /// ([`fab_plate::REGION_SETTINGS`]); one it does not set is the part's.
    Box { object: String, part: usize },
}

/// Whether a print settings box's row is written in its statement (else it shows its part's).
fn written(p: &ParamView) -> bool {
    p.site.as_ref().is_some_and(|s| !s.is_empty())
}

/// A box row's value as the setting's text (`"100%"`, `"gyroid"`, `"3"`).
fn setting_text(s: &PrintSetting, p: &ParamView, v: &ParamValue) -> Option<String> {
    match (s.kind, v) {
        (SettingKind::Choice(list), ParamValue::Choice(i)) => {
            let label = p.choices.get(*i)?;
            list.iter().find(|(_, l)| *l == label.as_str()).map(|(v, _)| v.to_string())
        }
        (SettingKind::Number { unit: "%", .. }, ParamValue::Num(x)) => Some(format!("{}%", (x * 1000.0).round() / 10.0)),
        (SettingKind::Number { .. }, ParamValue::Num(x)) => Some(format!("{x}")),
        _ => None,
    }
}

/// A setting's text as a box row's value.
fn param_value(s: &PrintSetting, p: &ParamView, value: &str) -> Option<ParamValue> {
    match s.kind {
        SettingKind::Choice(list) => {
            let label = list.iter().find(|(v, _)| *v == value)?.1;
            p.choices.iter().position(|c| c == label).map(ParamValue::Choice)
        }
        SettingKind::Number { unit: "%", .. } => setting_number(value).map(|x| ParamValue::Num(x / 100.0)),
        SettingKind::Number { .. } => setting_number(value).map(ParamValue::Num),
        SettingKind::Bool => None,
    }
}

fn editing(state: &AppState, show_global: bool) -> Editing {
    if show_global {
        return Editing::Global;
    }
    let mut parts = selected_parts(state);
    if let Some(active) = active_part(state) {
        parts.retain(|p| *p != active);
        parts.insert(0, active);
    }
    if parts.is_empty() { Editing::Global } else { Editing::Parts(parts) }
}

/// A setting as the plate gives it to every object: the project's, else the profile's, else the table's.
pub fn global_value<'a>(state: &'a AppState, s: &'a PrintSetting) -> &'a str {
    state.plate.settings.iter().find(|(k, _)| k == s.key).map(|(_, v)| v.as_str())
        .or_else(|| state.print.base_values.get(s.key).map(String::as_str).filter(|v| !v.is_empty() && *v != "nil"))
        .unwrap_or(s.default)
}

/// A setting as `part` prints with it: its own override, else the plate's ([`global_value`]).
pub fn part_value<'a>(state: &'a AppState, part: &'a PlatePart, s: &'a PrintSetting) -> &'a str {
    part.print.iter().find(|(k, _)| k == s.key).map(|(_, v)| v.as_str()).unwrap_or_else(|| global_value(state, s))
}

/// The number a row shows: profiles may say a width as a share of the nozzle and an
/// acceleration as a share of the default.
fn shown_number(print: &PrintState, s: &PrintSetting, value: &str) -> f64 {
    let n = setting_number(value).unwrap_or(0.0);
    if !value.trim().ends_with('%') {
        return n;
    }
    let of = |key: &str, fallback: f64| print.base_values.get(key).and_then(|v| setting_number(v)).unwrap_or(fallback);
    match s.kind {
        SettingKind::Number { unit: "mm", .. } => n / 100.0 * of("nozzle_diameter", 0.4),
        SettingKind::Number { unit: "mm/s²", .. } => n / 100.0 * of("default_acceleration", 500.0),
        _ => n,
    }
}

pub(crate) fn same_value(print: &PrintState, s: &PrintSetting, a: &str, b: &str) -> bool {
    match s.kind {
        SettingKind::Number { .. } => (shown_number(print, s, a) - shown_number(print, s, b)).abs() < 1e-9,
        SettingKind::Bool => setting_on(s, a) == setting_on(s, b),
        SettingKind::Choice(_) => a == b,
    }
}

fn number_precision(step: f64) -> usize {
    if step >= 1.0 { 0 } else if step >= 0.1 { 1 } else { 2 }
}

/// A dragged or typed number as the project keeps it: on its step, with no float tail.
fn number_text(s: &PrintSetting, v: f64) -> Option<String> {
    let SettingKind::Number { step, .. } = s.kind else { return None };
    let on_step: f64 = format!("{:.*}", number_precision(step), v).parse().ok()?;
    checked_value(s.key, &on_step.to_string()).ok()
}

/// Does the query find this setting?
fn matches_query(s: &PrintSetting, query: &str) -> bool {
    let has = |t: &str| t.to_lowercase().contains(query);
    has(s.label) || has(s.hint) || has(s.key) || has(s.mode) || has(s.group.title())
}

fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// Overrides on a part (the parts list shows this too).
/// How many of the part's own settings differ from what it would inherit (one the same as the
/// plate's is no override, though an older plate may still carry it).
pub fn override_count(state: &AppState, part: &PlatePart) -> usize {
    fab_plate::own_settings(&part.print).filter(|(k, v)| print_setting(k).is_none_or(|s| !same_value(&state.print, s, v, global_value(state, s)))).count()
}

// ===========================================================================
// The panel: which set it edits, the search, the fold, the result
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabSlicerPanel {
    #[deref]
    view: View,
    /// The Global tab, chosen by hand while objects are selected.
    #[rust]
    show_global: bool,
    #[rust]
    query: String,
    #[rust]
    advanced: bool,
    /// The selection the Global tab was chosen under: a new selection goes back to its object.
    #[rust]
    selection_seen: Vec<usize>,
    /// A setting's field to scroll into view once it is drawn (an override's row picked).
    #[rust]
    reveal: Option<String>,
}

impl FabSlicerPanel {
    fn sync_head(&mut self, cx: &mut Cx, state: &AppState) {
        let selected = selected_parts(state);
        if selected != self.selection_seen {
            self.selection_seen = selected.clone();
            self.show_global = false;
        }
        // A print settings box picked: the rows are the box's, its size and place its fields
        // under Overrides.
        let boxed = crate::ui::param_panel::picked_volume(state).filter(|_| state.ui.in_mode(crate::api::PlateMode::Design))
            .and_then(|object| Some((crate::ui::overrides::volume_part(state, &object)?, object)));
        let edit = match &boxed {
            Some((part, object)) => Editing::Box { object: object.clone(), part: *part },
            None => editing(state, self.show_global),
        };
        let (title, note) = match &edit {
            Editing::Box { object, part } => {
                let solid = object.split_once('/').map_or(object.as_str(), |(_, s)| s);
                let on = state.plate.parts.get(*part).map_or("its object", |p| p.name.as_str());
                (format!("{}, on {on}", crate::ui::overrides::box_name(solid)),
                    format!("What the box sets is marked, each with a reset. The rest are {on}'s: set one to override it in the box."))
            }
            Editing::Global => {
                let n = state.plate.settings.len();
                ("All objects".to_string(), match (n, selected.is_empty()) {
                    (0, true) => "The profile's settings. Select an object to override them for it.".to_string(),
                    (0, false) => "The profile's settings.".to_string(),
                    (n, _) => format!("{} on the profile: each is under Overrides.", plural(n, "change")),
                })
            }
            Editing::Parts(parts) => {
                let total: usize = parts.iter().filter_map(|i| state.plate.parts.get(*i)).map(|p| override_count(state, p)).sum();
                let title = match parts.as_slice() {
                    [one] => state.plate.parts.get(*one).map(|p| p.name.clone()).unwrap_or_default(),
                    many => format!("{} objects", many.len()),
                };
                (title, if total == 0 { "Follows the global settings.".to_string() } else { format!("{} of the global settings: each is under Overrides.", plural(total, "override")) })
            }
        };
        let v = &self.view;
        v.label(cx, ids!(head.title)).set_text(cx, &title);
        v.label(cx, ids!(head.note)).set_text(cx, &note);
        let is_box = boxed.is_some();
        v.widget(cx, ids!(head.scope)).set_visible(cx, !selected.is_empty() && !is_box);
        let global = matches!(edit, Editing::Global);
        v.segmented_control(cx, ids!(head.scope)).set_selected(cx, if global { 0 } else { 1 });
        let any = match &edit {
            Editing::Global => !state.plate.settings.is_empty(),
            Editing::Parts(parts) => parts.iter().filter_map(|i| state.plate.parts.get(*i)).any(|p| fab_plate::own_settings(&p.print).next().is_some()),
            Editing::Box { .. } => false,
        };
        v.widget(cx, ids!(head.tools.clear)).set_visible(cx, any && !is_box);
        // Copying takes one object with something to copy, and another object to take it.
        let can_copy = matches!(&edit, Editing::Parts(p) if p.len() == 1) && state.plate.parts.len() > 1;
        v.widget(cx, ids!(head.tools.copy)).set_visible(cx, can_copy && !is_box);
        if let Some(mut rows) = v.widget(cx, ids!(body.rows)).borrow_mut::<FabSlicerRows>() {
            rows.filter = Filter { edit, query: self.query.clone(), advanced: self.advanced };
        }
    }
}

impl Widget for FabSlicerPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Slicer settings");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        // an override's row picked (in the Overrides editor): its setting's field scrolled into
        // view, its group open; the shell brings this editor forward
        if let Some(crate::ui::overrides::RevealSetting(key)) = actions.iter().rev().find_map(|a| a.downcast_ref::<crate::ui::overrides::RevealSetting>()) {
            self.reveal = Some(key.to_string());
            self.show_global = false;
            if let Some(mut rows) = self.view.widget(cx, ids!(body.rows)).borrow_mut::<FabSlicerRows>() {
                rows.unfold(key);
            }
            self.view.redraw(cx);
        }
        if scope.data.get::<AppState>().is_some_and(|s| !s.ui.on_screen(crate::ui::editors::Editor::Slicer)) {
            // off screen: none of its controls can be pressed
            return;
        }
        let Some(state) = scope.data.get::<AppState>() else { return };
        let v = &self.view;
        if let Some(i) = v.segmented_control(cx, ids!(head.scope)).selected(actions) {
            self.show_global = i == 0;
            self.view.redraw(cx);
        }
        if let Some(text) = self.view.text_input(cx, ids!(head.tools.search.row.input)).changed(actions) {
            self.query = text.trim().to_lowercase();
            self.view.redraw(cx);
        }
        if let Some(on) = self.view.check_box(cx, ids!(head.tools.advanced)).changed(actions) {
            self.advanced = on;
            self.view.redraw(cx);
        }
        let edit = editing(state, self.show_global);
        if self.view.button(cx, ids!(head.tools.clear)).clicked(actions) {
            match &edit {
                Editing::Global => cx.action(PartAction::ClearPlateSettings),
                Editing::Parts(parts) => cx.action(PartAction::ClearPrintSettings { objects: parts.iter().filter_map(|i| state.plate.parts.get(*i)).map(|p| p.object.clone()).collect() }),
                // (a box hides the button)
                Editing::Box { .. } => {}
            }
        }
        if let Editing::Parts(parts) = &edit {
            if let (Some(anchor), [one]) = (dropdown_clicked(actions, COPY_MENU), parts.as_slice()) {
                let mut items = vec![MenuItem::new(LiveId(u64::MAX), "Every other object"), MenuItem::sep()];
                items.extend(state.plate.parts.iter().enumerate().filter(|(i, _)| i != one).map(|(i, p)| MenuItem::new(LiveId(i as u64 + 1), &p.name)));
                open_menu(cx, COPY_MENU, items, anchor, MenuPlace::Below);
            }
            if let (Some(pick), [one]) = (menu_picked(actions, COPY_MENU), parts.as_slice()) {
                let to: Vec<String> = state.plate.parts.iter().enumerate()
                    .filter(|(i, _)| i != one && (pick == LiveId(u64::MAX) || pick == LiveId(*i as u64 + 1)))
                    .map(|(_, p)| p.object.clone()).collect();
                if let (Some(from), false) = (state.plate.parts.get(*one), to.is_empty()) {
                    cx.action(PartAction::CopyPrintSettings { from: from.object.clone(), to });
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            self.sync_head(cx, state);
        }
        let step = self.view.draw_walk(cx, scope, walk);
        if let Some(key) = &self.reveal {
            let rect = self.view.widget(cx, ids!(body.rows)).borrow::<FabSlicerRows>().and_then(|rows| rows.row_rect(cx, key));
            if let Some(rect) = rect {
                self.reveal = None;
                if let Some(mut body) = self.view.view(cx, ids!(body)).borrow_mut() {
                    body.scroll_into_view_abs(cx, rect);
                }
            }
        }
        step
    }
}

// ===========================================================================
// The rows
// ===========================================================================

#[derive(Clone, Debug, PartialEq)]
struct Filter {
    edit: Editing,
    query: String,
    advanced: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Filter { edit: Editing::Global, query: String::new(), advanced: false }
    }
}

/// How a row reads right now (its control's look is reapplied only when this changes).
#[derive(Clone, Debug, PartialEq)]
struct RowLook {
    set: bool,
    mixed: bool,
    locked: bool,
    expanded: bool,
    /// The actual choices behind the displayed indices. The short list adds More after them.
    choices: Vec<(&'static str, &'static str)>,
}

enum RowEdit {
    Set(String),
    More,
}

#[derive(Script, Widget)]
pub struct FabSlicerRows {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[rust]
    templates: HashMap<LiveId, ScriptObjectRef>,
    #[rust]
    items: ComponentMap<LiveId, (WidgetRef, LiveId)>,
    /// What shows, in order; the index into the table for a setting's row.
    #[rust]
    rows: Vec<(LiveId, Option<usize>)>,
    #[rust]
    filter: Filter,
    #[rust]
    folded: HashSet<SettingGroup>,
    #[rust]
    synced: Option<(Filter, u64, u64, u64, Vec<SettingGroup>)>,
    #[rust]
    applied: HashMap<LiveId, RowLook>,
    /// Rows showing their full choice list, until the corresponding dropdown closes.
    #[rust]
    expanded: HashMap<usize, DropDownRef>,
    /// The hover tooltip of a group header's note; never inline, so hovering moves nothing.
    #[rust]
    hint: PropHint,
    #[redraw]
    #[rust]
    area: Area,
}

impl ScriptHook for FabSlicerRows {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.templates.clear();
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let (Some(id), Some(t)) = (kv.key.as_id(), kv.value.as_object()) {
                            self.templates.insert(id, vm.new_object_ref(t));
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            for (_, (w, t)) in self.items.iter_mut() {
                if let Some(template) = self.templates.get(t) {
                    w.script_apply(vm, apply, scope, template.as_object().into());
                }
            }
            self.applied.clear();
            self.synced = None;
        }
    }
}

/// Semantics read the retained source, so give each generated control its own source object.
pub(crate) fn name_control(cx: &mut Cx, w: &WidgetRef, name: &str, role: &str) {
    let mut w = w.clone();
    let name = name.to_string();
    let role = role.to_string();
    cx.with_vm(|vm| {
        let source = w.script_source();
        let script = script! { __script_source__{a11y: {label: #(name) role: #(role)}} };
        let value = vm.eval_with_source(script, source);
        w.script_apply(vm, &Apply::New, &mut Scope::empty(), value);
    });
}

impl FabSlicerRows {
    fn item(&mut self, cx: &mut Cx, id: LiveId, template: LiveId) -> Option<WidgetRef> {
        if let Some((w, _)) = self.items.get(&id) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&template)?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        self.items.insert(id, (w.clone(), template));
        Some(w)
    }

    /// A field's rect as drawn, for a setting shown here.
    pub fn row_rect(&self, cx: &Cx, key: &str) -> Option<Rect> {
        let id = setting_ids()[PRINT_SETTINGS.iter().position(|s| s.key == key)?].row;
        let (w, _) = self.items.get(&id).filter(|_| self.rows.iter().any(|(r, _)| *r == id))?;
        Some(w.area().rect(cx)).filter(|r| r.size.y > 0.0)
    }

    /// Open the group of a setting.
    pub fn unfold(&mut self, key: &str) {
        if let Some(s) = print_setting(key) {
            self.folded.remove(&s.group);
        }
    }

    fn parts<'a>(&self, state: &'a AppState) -> Vec<&'a PlatePart> {
        match &self.filter.edit {
            Editing::Global => Vec::new(),
            Editing::Parts(parts) => parts.iter().filter_map(|i| state.plate.parts.get(*i)).collect(),
            Editing::Box { part, .. } => state.plate.parts.get(*part).into_iter().collect(),
        }
    }

    /// The picked box's row for a setting (its first statement's); none unless a box is edited.
    fn box_row<'a>(&self, state: &'a AppState, s: &PrintSetting) -> Option<&'a ParamView> {
        let Editing::Box { object, part } = &self.filter.edit else { return None };
        state.params.part(*part)?.params.iter().find(|p| p.setting == Some(s.key) && p.objects.iter().any(|o| o == object))
    }

    /// The choices a row offers, as (value, label).
    fn choices(&self, state: &AppState, s: &PrintSetting, current: Option<&str>, expanded: bool) -> Vec<(&'static str, &'static str)> {
        let SettingKind::Choice(list) = s.kind else { return Vec::new() };
        let allowed = match self.box_row(state, s) {
            Some(p) => list.iter().copied().filter(|(_, l)| p.choices.iter().any(|c| c == l)).collect(),
            None => list.to_vec(),
        };
        if s.common.is_empty() || expanded {
            return allowed;
        }
        let mut short: Vec<_> = s.common.iter().filter_map(|(v, _)| allowed.iter().find(|(a, _)| a == v).copied()).collect();
        if let Some(&choice) = allowed.iter().find(|(v, _)| Some(*v) == current) {
            if !short.contains(&choice) { short.push(choice); }
        }
        short
    }

    /// Is this setting overridden here? (Any selected part, the project's global set, the box.)
    fn is_set(&self, state: &AppState, s: &PrintSetting) -> bool {
        match &self.filter.edit {
            Editing::Global => state.plate.settings.iter().any(|(k, _)| k == s.key),
            Editing::Parts(_) => self.parts(state).iter().any(|p| p.print.iter().any(|(k, _)| k == s.key)),
            Editing::Box { .. } => self.box_row(state, s).is_some_and(written),
        }
    }

    /// The value shown, and whether the selected parts disagree.
    fn shown_value<'a>(&self, state: &'a AppState, s: &'a PrintSetting) -> (String, bool) {
        if let Some(p) = self.box_row(state, s).filter(|p| written(p)) {
            return (setting_text(s, p, &state.params.shown_value(p)).unwrap_or_default(), false);
        }
        let parts = self.parts(state);
        match parts.split_first() {
            None => (global_value(state, s).to_string(), false),
            Some((first, rest)) => {
                let v = part_value(state, first, s);
                (v.to_string(), rest.iter().any(|p| !same_value(&state.print, s, part_value(state, p, s), v)))
            }
        }
    }

    fn configure(&mut self, cx: &mut Cx, index: usize, w: &WidgetRef, look: RowLook) {
        let ids = &setting_ids()[index];
        let s = &PRINT_SETTINGS[index];
        let id = ids.row;
        if self.applied.get(&id) == Some(&look) {
            return;
        }
        let first = !self.applied.contains_key(&id);
        let relabel = self.applied.get(&id).is_none_or(|old| old.expanded != look.expanded || old.choices != look.choices);
        if first {
            let reset = w.widget(cx, ids!(reset));
            name_control(cx, &reset, &format!("Remove override: {}", ids.name), "button");
        }
        let label = s.label.split(" (0 =").next().unwrap_or(s.label).to_string();
        let name = ids.name.as_str();
        if first || self.applied.get(&id).is_none_or(|old| old.locked != look.locked) {
            // A setting only the plate sets says why in its tooltip while objects are edited.
            let hint = match s.scope {
                SettingScope::Plate(why) if look.locked => format!("{}\n\n{why}", s.hint),
                _ => s.hint.to_string(),
            };
            let mut tip = w.widget(cx, ids!(row.value));
            script_apply_eval!(cx, tip, {text: #(hint)});
        }
        if first {
            let indent = if s.needs.is_empty() { 0.0 } else { 12.0 };
            let mut row = w.clone();
            script_apply_eval!(cx, row, {margin: mod.turtle.Inset{left: #(indent)}});
        }
        let inherited = !look.set;
        let marked = look.set;
        let mixed = look.mixed;
        let dim = inherited || mixed;
        let tint = if marked { 1.0 } else { 0.0 };
        match s.kind {
            SettingKind::Bool => {
                let mut check = w.widget(cx, ids!(check));
                if first { name_control(cx, &check, name, "check_box"); }
                script_apply_eval!(cx, check, {
                    text: #(label)
                    draw_bg +: {marked: #(tint)}
                    draw_text +: {color: if #(dim) {mod.theme.prop_ink_dim} else {mod.theme.color_text}}
                });
                check.set_disabled(cx, look.locked);
            }
            SettingKind::Number { min, max, step, unit } => {
                w.label(cx, ids!(row.label)).set_text(cx, &label);
                let suffix = if unit.is_empty() { String::new() } else { format!(" {unit}") };
                let precision = number_precision(step) as f64;
                let mut num = w.widget(cx, ids!(num));
                if first { name_control(cx, &num, name, "spin_button"); }
                script_apply_eval!(cx, num, {
                    min: #(min) max: #(max) step: #(step) precision: #(precision) suffix: #(suffix)
                    marked: #(marked) mixed: #(mixed) inherited: #(inherited)
                });
                w.value_input(cx, ids!(num)).set_read_only(cx, look.locked);
            }
            SettingKind::Choice(table) => {
                w.label(cx, ids!(row.label)).set_text(cx, &label);
                let mut pick = w.widget(cx, ids!(pick));
                if first {
                    name_control(cx, &pick, name, "combo_box");
                }
                if relabel {
                    let dropdown = pick.as_drop_down();
                    let mut labels: Vec<String> = look.choices.iter().map(|(_, l)| l.to_string()).collect();
                    if !s.common.is_empty() {
                        if look.expanded {
                            dropdown.set_face_labels(cx, Vec::new());
                        } else {
                            dropdown.set_face_labels(cx, labels.clone());
                            for ((value, _), label) in look.choices.iter().zip(&mut labels) {
                                if let Some((_, note)) = s.common.iter().find(|(v, _)| v == value) {
                                    label.push_str(": ");
                                    label.push_str(note);
                                }
                            }
                            labels.push(MORE_PATTERNS.into());
                        }
                    }
                    dropdown.set_labels(cx, labels);
                }
                script_apply_eval!(cx, pick, {
                    marked: #(marked) mixed: #(mixed)
                    draw_text +: {color: if #(dim) {mod.theme.prop_ink_dim} else {mod.theme.color_text}}
                });
                pick.set_disabled(cx, look.locked);
                if table.len() <= TAB_CHOICES && s.common.is_empty() {
                    let mut seg = w.widget(cx, ids!(seg));
                    if first { name_control(cx, &seg, name, "radio_group"); }
                    if let Some(mut inner) = seg.borrow_mut::<SegmentedControl>().filter(|_| relabel) {
                        inner.options = look.choices.iter().map(|(_, l)| l.to_string()).collect();
                        inner.option_names = look.choices.iter().map(|(_, l)| format!("{name} {l}")).collect();
                    }
                    script_apply_eval!(cx, seg, {
                        marked: #(marked) mixed: #(mixed)
                        draw_text_selected +: {color: if #(dim) {mod.theme.prop_ink_dim} else {mod.theme.color_text}}
                    });
                    seg.set_disabled(cx, look.locked);
                }
            }
        }
        self.applied.insert(id, look);
    }

    fn head(&mut self, cx: &mut Cx, group: SettingGroup, overrides: usize) -> Option<WidgetRef> {
        let id = head_id(group);
        let w = self.item(cx, id, live_id!(Head))?;
        w.label(cx, ids!(hdr.title)).set_text(cx, group.title());
        let count = if overrides == 0 { String::new() } else { plural(overrides, "override") };
        w.label(cx, ids!(hdr.count)).set_text(cx, &count);
        w.widget(cx, ids!(hdr.count)).set_visible(cx, overrides > 0);
        let open = !self.folded.contains(&group);
        if w.as_prop_section().is_open(cx) != open { w.as_prop_section().set_is_open(cx, open, Animate::No); }
        self.rows.push((id, None));
        Some(w)
    }

    fn sync_row(&mut self, cx: &mut Cx, state: &AppState, index: usize, w: &WidgetRef) {
        let s = &PRINT_SETTINGS[index];
        let (value, mixed) = self.shown_value(state, s);
        let plate_only = matches!(s.scope, SettingScope::Plate(_));
        let locked = plate_only && !matches!(self.filter.edit, Editing::Global);
        let set = !locked && self.is_set(state, s);
        let expanded = !s.common.is_empty() && self.expanded.contains_key(&index);
        let choices = self.choices(state, s, (!mixed).then_some(value.as_str()), expanded);
        let selected = choices.iter().position(|(v, _)| *v == value);
        // A retained unknown pattern, or one the box cannot set, has no selected menu entry.
        let mixed = mixed || (!s.common.is_empty() && selected.is_none());
        self.configure(cx, index, w, RowLook { set, mixed, locked, expanded, choices });
        w.widget(cx, ids!(reset)).set_visible(cx, set);
        match s.kind {
            SettingKind::Bool => w.check_box(cx, ids!(check)).set_state(cx,
                if mixed { CheckState::Mixed } else if setting_on(s, &value) { CheckState::On } else { CheckState::Off }, Animate::No),
            SettingKind::Number { .. } => {
                let num = w.value_input(cx, ids!(num));
                if !num.borrow().is_some_and(|n| n.is_interacting()) {
                    let number = shown_number(&state.print, s, &value);
                    num.set_value(cx, number);
                    if let Some(mut n) = num.borrow_mut() {
                        n.inherited = !set;
                        n.mixed = mixed;
                        n.placeholder = if number == 0.0 && s.label.contains("(0 =") { "Auto".into() } else { String::new() };
                    }
                }
            }
            SettingKind::Choice(_) => {
                if let Some(i) = selected {
                    w.segmented_control(cx, ids!(seg)).set_selected(cx, i);
                    w.drop_down(cx, ids!(pick)).set_selected_item(cx, i);
                }
            }
        }
    }

    /// Reconcile the rows only when what they show changes.
    fn sync(&mut self, cx: &mut Cx2d, state: &AppState) {
        // Dismissing the menu emits no Select action. Its face holds the pointer while open.
        let expanded = self.expanded.len();
        self.expanded.retain(|_, pick| {
            if cx.sweep_lock_area() == Some(pick.area()) { return true; }
            // Another overlay can hold the pointer above an open menu.
            let mut node = a11y_tree::Node::default();
            if let Some(inner) = pick.borrow() { inner.semantics(cx, &mut node); }
            node.states.contains(a11y_tree::States::EXPANDED)
        });
        if self.expanded.len() != expanded {
            self.synced = None;
        }
        let folded: Vec<SettingGroup> = SettingGroup::ALL.into_iter().filter(|g| self.folded.contains(g)).collect();
        // a box's rows come with the part's parameters, not the plate's revision
        let boxed = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for s in PRINT_SETTINGS.iter().filter(|s| fab_plate::REGION_SETTINGS.contains(&s.key)) {
                if let Some(p) = self.box_row(state, s) {
                    (written(p), format!("{:?} {:?}", state.params.shown_value(p), p.choices)).hash(&mut h);
                }
            }
            h.finish()
        };
        let key = (self.filter.clone(), state.plate.revision, state.print.base_revision, boxed, folded);
        if self.synced.as_ref() == Some(&key) {
            return;
        }
        for (_, (w, _)) in self.items.iter() { w.set_visible(cx, false); }
        self.rows.clear();
        let searching = !self.filter.query.is_empty();
        // a box: the settings it can set, every one (none is advanced there)
        let in_box = matches!(self.filter.edit, Editing::Box { .. });
        for group in SettingGroup::ALL {
            let in_group: Vec<usize> = (0..PRINT_SETTINGS.len()).filter(|i| PRINT_SETTINGS[*i].group == group).collect();
            let visible: Vec<usize> = in_group.iter().copied().filter(|i| {
                let s = &PRINT_SETTINGS[*i];
                if in_box && self.box_row(state, s).is_none() { return false; }
                let parent_on = fab_plate::needs_met(s, &|p| self.shown_value(state, p).0);
                if !parent_on { return false; }
                if searching { return matches_query(s, &self.filter.query); }
                in_box || self.is_set(state, s) || !s.advanced || self.filter.advanced
            }).collect();
            // Searching hides what does not match; a group with nothing to show goes.
            if visible.is_empty() {
                for i in &in_group {
                    if let Some((w, _)) = self.items.get(&setting_ids()[*i].row) {
                        w.set_visible(cx, false);
                    }
                }
                continue;
            }
            let overrides = in_group.iter().filter(|i| self.is_set(state, &PRINT_SETTINGS[**i])).count();
            if searching { self.folded.remove(&group); }
            let Some(section) = self.head(cx, group, overrides) else { continue };
            section.set_visible(cx, true);
            let start = self.rows.len();
            for index in visible {
                let s = &PRINT_SETTINGS[index];
                let ids = &setting_ids()[index];
                if let Some(mode) = ids.mode {
                    if s.label != s.mode {
                        if let Some(w) = self.item(cx, mode, live_id!(Mode)) {
                            w.set_visible(cx, true);
                            w.label(cx, ids!(value.title)).set_text(cx, s.mode);
                            self.rows.push((mode, None));
                        }
                    }
                }
                let template = match s.kind {
                    SettingKind::Bool => live_id!(Bool),
                    SettingKind::Number { .. } => live_id!(Num),
                    SettingKind::Choice(c) if c.len() <= TAB_CHOICES && s.common.is_empty() => live_id!(Choice),
                    SettingKind::Choice(_) if !s.common.is_empty() => live_id!(PickNotes),
                    SettingKind::Choice(_) => live_id!(Pick),
                };
                let Some(w) = self.item(cx, ids.row, template) else { continue };
                w.set_visible(cx, true);
                self.sync_row(cx, state, index, &w);
                self.rows.push((ids.row, Some(index)));
            }
            let children = self.rows[start..].iter().filter_map(|(id, _)| self.items.get(id).map(|(w, _)| (*id, w.clone()))).collect();
            crate::ui::widgets::section_rows(cx, &section, children);
        }
        self.synced = Some(key);
    }

    /// One row's new value, if this event gave it one.
    fn edited(&self, cx: &mut Cx, actions: &Actions, index: usize, w: &WidgetRef) -> Option<RowEdit> {
        let s = &PRINT_SETTINGS[index];
        match s.kind {
            SettingKind::Bool => w.check_box(cx, ids!(check)).changed(actions).map(|v| if v { "1".into() } else { "0".into() }),
            SettingKind::Number { .. } => w.value_input(cx, ids!(num)).ended(actions).and_then(|v| number_text(s, v)),
            SettingKind::Choice(_) => {
                let i = w.segmented_control(cx, ids!(seg)).selected(actions).or_else(|| w.drop_down(cx, ids!(pick)).selected(actions))?;
                let look = self.applied.get(&setting_ids()[index].row)?;
                if !s.common.is_empty() && !look.expanded && i == look.choices.len() {
                    return Some(RowEdit::More);
                }
                look.choices.get(i).map(|(v, _)| v.to_string())
            }
        }.map(RowEdit::Set)
    }

    /// Send a row's edit. A value equal to what it would inherit is no override at all. It
    /// goes out even when the plate shows that already: an edit still on its way may not.
    fn commit(&self, cx: &mut Cx, state: &AppState, s: &PrintSetting, value: Option<String>) {
        match &self.filter.edit {
            Editing::Global => {
                let base = state.print.base_values.get(s.key).map(String::as_str).filter(|v| !v.is_empty() && *v != "nil").unwrap_or(s.default);
                let value = value.filter(|v| !same_value(&state.print, s, v, base));
                cx.action(PartAction::SetPlateSetting { key: s.key.into(), value });
            }
            Editing::Parts(_) => {
                let objects: Vec<String> = self.parts(state).iter().map(|p| p.object.clone()).collect();
                let value = value.filter(|v| !same_value(&state.print, s, v, global_value(state, s)));
                cx.action(PartAction::SetPrintSetting { objects, key: s.key.into(), value });
            }
            // a box's setting: written in its statement, or (its part's own value, or a reset)
            // taken out of it
            Editing::Box { part, .. } => {
                let (Some(p), Some(owner)) = (self.box_row(state, s), state.params.part(*part)) else { return };
                let own = state.plate.parts.get(*part).map(|pp| part_value(state, pp, s));
                match value.filter(|v| own.is_none_or(|o| !same_value(&state.print, s, v, o))) {
                    Some(v) => {
                        if let Some(value) = param_value(s, p, &v) {
                            cx.action(ParamAction::Commit { object: owner.object.clone(), key: p.key.clone(), value });
                        }
                    }
                    None if written(p) => cx.action(PartAction::ResetAreaSetting { key: p.key.clone() }),
                    None => {}
                }
            }
        }
    }
}

impl Widget for FabSlicerRows {
    fn visit_cancel(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) -> bool {
        for group in SettingGroup::ALL {
            let id = head_id(group);
            if let Some((w, _)) = self.items.get(&id) { if w.visible() { visit(id, w.clone()); } }
        }
        true
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let claimed_before = event.pointer_claimed_area();
        for group in SettingGroup::ALL {
            if let Some((w, _)) = self.items.get(&head_id(group)) {
                if w.visible() { w.handle_event(cx, event, scope); }
            }
        }
        if let Event::MouseMove(e) = event {
            let claimed = event.pointer_claimed_area();
            // A claim made by a header is already an accepted hit in this subtree.
            let pointer = if claimed != claimed_before && !claimed.is_empty() {
                Some(e.abs)
            } else {
                match event.hits(cx, self.area) {
                    Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => Some(e.abs),
                    _ => None,
                }
            };
            let over = pointer.and_then(|pos| SettingGroup::ALL.into_iter().filter(|g| !g.note().is_empty()).find_map(|g| {
                let (w, _) = self.items.get(&head_id(g)).filter(|(w, _)| w.visible())?;
                let rect = w.widget(cx, ids!(hdr)).area().rect(cx);
                rect.contains(pos).then_some((head_id(g), rect, g.note()))
            }));
            self.hint.hover(cx, self.uid, over);
        }
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        for group in SettingGroup::ALL {
            if let Some((w, _)) = self.items.get(&head_id(group)) {
                for action in actions.filter_widget_actions_cast::<FoldHeaderAction>(w.widget_uid()) {
                    match action {
                        FoldHeaderAction::Opening => { self.folded.remove(&group); }
                        FoldHeaderAction::Closing => { self.folded.insert(group); }
                        _ => continue,
                    }
                    self.redraw(cx);
                }
            }
        }
        let mut more = None;
        for (id, index) in &self.rows {
            let Some(index) = index else { continue };
            let s = &PRINT_SETTINGS[*index];
            let Some((w, _)) = self.items.get(id) else { continue };
            if w.button(cx, ids!(reset)).clicked(actions) {
                self.commit(cx, state, s, None);
                continue;
            }
            if matches!(s.scope, SettingScope::Plate(_)) && !matches!(self.filter.edit, Editing::Global) {
                continue;
            }
            match self.edited(cx, actions, *index, w) {
                Some(RowEdit::Set(value)) => self.commit(cx, state, s, Some(value)),
                Some(RowEdit::More) => more = Some((*index, w.clone())),
                None => {}
            }
        }
        if let Some((index, w)) = more {
            let dropdown = w.drop_down(cx, ids!(pick));
            self.expanded.insert(index, dropdown.clone());
            self.sync_row(cx, state, index, &w);
            if let Some(mut pick) = dropdown.borrow_mut() { pick.set_active(cx); };
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        self.sync(cx, state);
        cx.begin_turtle(walk, self.layout);
        for group in SettingGroup::ALL {
            if let Some((w, _)) = self.items.get(&head_id(group)) {
                if w.visible() { w.draw_all(cx, &mut Scope::empty()); }
            }
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}
