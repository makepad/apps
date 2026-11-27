//! The plate's print overrides, every one listed in the Overrides editor (a box is a feature of
//! its model in the Features tree too). An override is a setting the plate
//! changes from the profile, a setting a part changes from the plate's (part-wide), or a print
//! settings box (`m.settings` on a volume of the part's script) with the settings it writes. A
//! row says what it is and whose, and is deleted by its button: a setting goes back to what it
//! inherits, a box's statement goes, each one undo step. A click picks it, the one pick there
//! is: the Slicer panel then shows its settings (a box's, its object's, the plate's), its own
//! marked, the rest as inherited, and a setting's field scrolled into view.

use crate::api::*;
use crate::params::*;
use crate::ui::param_panel::picked_volume;
use crate::ui::editors::Editor;
use crate::ui::parts::active_part;
use crate::ui::popover::{menu_picked, open_menu, MenuPlace};
use crate::ui::print_settings::{global_value, same_value};
use crate::ui::shell::ShowPanel;
use fab_plate::{print_setting, setting_number, setting_on, PrintSetting, SettingKind};
use makepad_widgets_core::tip::TipWidgetExt;
use makepad_widgets_core::widget_tree::CxWidgetExt;
use makepad_widgets_core::*;
use std::collections::HashMap;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabOverrideListBase = #(FabOverrideList::register_widget(vm))
    mod.widgets.FabOverrideList = set_type_default() do mod.widgets.FabOverrideListBase{
        width: Fill height: Fit flow: Down spacing: 0
        Row := View{
            width: Fill height: Fit flow: Right spacing: 4
            align: Align{y: 0.5}
            padding: Inset{left: 10 right: 4 top: 4 bottom: 4}
            cursor: MouseCursor.Hand
            show_bg: true
            draw_bg +: {
                selected: instance(0.0)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                    sdf.fill(fab.color_editor.mix(fab.color_selection_bg, self.selected))
                    sdf.rect(0.0, 0.0, 2.0 * self.selected, self.rect_size.y)
                    sdf.fill(fab.color_accent)
                    return sdf.result
                }
            }
            text := View{
                width: Fill height: Fit flow: Down spacing: 1
                what := mod.widgets.FabLabel{width: Fill text: "" max_lines: 2}
                whose := mod.widgets.FabLabelSmall{width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis}
            }
            FabTip{text: "Delete: a setting goes back to what it inherits, a box goes"
                delete := mod.widgets.FabIconButton{
                    width: 24 height: 22
                    a11y: {label: "Delete override"}
                    draw_icon +: {color: fab.color_text_dim svg: crate_resource("self://resources/icons/trash.svg")}
                }
            }
        }
        Empty := mod.widgets.FabLabelDim{
            width: Fill text: "No overrides: every object prints with the global settings."
            margin: Inset{left: 10 right: 6 top: 4 bottom: 4}
        }
    }

    // The Overrides editor: every override on the plate (each delete-able, a click picks it), Add
    // for a print settings volume, and the picked volume's size and place (its settings are the
    // Slicer's rows).
    mod.widgets.FabOverridesPanelBase = #(FabOverridesPanel::register_widget(vm))
    mod.widgets.FabOverridesPanel = set_type_default() do mod.widgets.FabOverridesPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector (and this editor's tools, when it has any)
        editor_bar := mod.widgets.FabEditorBar{}
        body := mod.widgets.FabScroll{
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: #(crate::ui::widgets::PANEL_TOP) bottom: 6}
            list := mod.widgets.FabOverrideList{}
            add_row := View{
                width: Fill height: Fit flow: Right
                padding: Inset{left: 10 right: 6 top: 4 bottom: 4}
                add_tip := FabTip{text: #(ADD_VOLUME_TIP)
                    add := mod.widgets.FabButton{
                        text: "+ Settings volume" a11y: {label: "Add print settings volume"}
                        height: theme.prop_row_height padding: Inset{left: 8 right: 8}
                    }
                }
            }
            volumes := mod.widgets.FabParamPanel{volumes: true}
        }
    }
}

const VOLUME_MENU: LiveId = live_id!(fab_overrides_volume);

/// The add button's tip while a part is picked (none picked, it says to pick one).
const ADD_VOLUME_TIP: &str = "A print settings volume with its own infill and wall loops: a box, sphere, cylinder or capsule on the picked part, or one that follows the picked feature";

/// The Overrides editor. Per draw: the add button's state, O(1); the list draws its rows.
#[derive(Script, ScriptHook, Widget)]
pub struct FabOverridesPanel {
    #[deref]
    view: View,
}

impl Widget for FabOverridesPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Overrides");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        if !state.ui.on_screen(Editor::Overrides) {
            // off screen: none of its controls can be pressed
            return;
        }
        if self.view.button(cx, ids!(body.add)).clicked(actions) {
            let anchor = self.view.widget(cx, ids!(body.add)).area().rect(cx);
            open_menu(cx, VOLUME_MENU, crate::ui::stage::volume_menu(state), anchor, MenuPlace::Below);
        }
        if let Some(pick) = menu_picked(actions, VOLUME_MENU) {
            crate::ui::stage::volume_picked(cx, state, pick);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            // a volume goes on the picked part; none picked, the add button waits for one
            let has_part = active_part(state).is_some();
            self.view.widget(cx, ids!(body.add)).set_disabled(cx, !has_part);
            self.view.tip(cx, ids!(body.add_tip)).set_text(if has_part { ADD_VOLUME_TIP } else { crate::ui::stage::SELECT_A_PART });
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// What an override is set on.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A setting the plate changes from the profile.
    Plate { key: String },
    /// A setting a part changes from the plate's (its placement's `settings`).
    Part { object: String, key: String },
    /// A print settings box (`inst/solid`).
    Box { object: String },
}

/// One override as its rows show it.
#[derive(Clone, Debug, PartialEq)]
pub struct Override {
    /// Stable across rebuilds: its rows' identity.
    pub id: String,
    pub target: Target,
    /// What it is: `Infill density 100 % (global 15 %)`, `Print settings box 1: Infill density 100 %`.
    pub what: String,
    /// Whose it is: `All objects`, or the object's name.
    pub whose: String,
    /// The plate part it belongs to; none for the plate's own.
    pub part: Option<usize>,
}

/// A setting's value as a person reads it: on or off, a choice's name, a number with its unit.
pub fn value_text(s: &PrintSetting, value: &str) -> String {
    match s.kind {
        SettingKind::Bool => (if setting_on(s, value) { "on" } else { "off" }).into(),
        SettingKind::Choice(list) => list.iter().find(|(v, _)| *v == value).map_or_else(|| value.to_string(), |(_, label)| label.to_string()),
        SettingKind::Number { unit, .. } => match setting_number(value) {
            Some(x) if value.trim().ends_with('%') => format!("{x} %"),
            Some(x) if unit.is_empty() => format!("{x}"),
            Some(x) => format!("{x} {unit}"),
            None => value.to_string(),
        },
    }
}

/// A print settings volume's name: `settings1` reads `Print settings box 1`, `sphere_settings1`
/// `Print settings sphere 1`, and one named for a part what it is on: `left_arm_settings` reads
/// `Left arm`, `left_arm_settings2` `Left arm 2`.
pub fn box_name(solid: &str) -> String {
    let digits = solid.trim_end_matches(|c: char| c.is_ascii_digit());
    let number = &solid[digits.len()..];
    let shape = [("settings", "box"), ("sphere_settings", "sphere"), ("cylinder_settings", "cylinder"), ("capsule_settings", "capsule")];
    if let (false, Some((_, shape))) = (number.is_empty(), shape.iter().find(|(stem, _)| *stem == digits)) {
        return format!("Print settings {shape} {number}");
    }
    let Some(on) = digits.strip_suffix("_settings").filter(|on| !on.is_empty()) else { return solid.replace('_', " ") };
    let mut words = on.replace('_', " ");
    if let Some(first) = words.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    if number.is_empty() { words } else { format!("{words} {number}") }
}

/// What a print settings box on part `part` writes: each setting's label and value.
pub fn box_settings(state: &AppState, part: usize, area: &MarkedArea) -> Vec<(String, String)> {
    state.params.part(part).map(|pp| {
        pp.params.iter().filter(|p| p.setting.is_some() && p.site.as_ref().is_some_and(|s| !s.is_empty()) && p.objects.iter().any(|o| *o == area.object))
            .filter_map(|p| Some((print_setting(p.setting?)?.label.to_string(), row_text(p)))).collect()
    }).unwrap_or_default()
}

/// The instance whose script holds the part's boxes.
fn instance_of(part: &PlatePart) -> &str {
    if part.script.is_empty() { part.object.split('/').next().unwrap_or(&part.object) } else { part.script.as_str() }
}

/// The plate part a print settings box (`inst/solid`) is on.
pub fn volume_part(state: &AppState, object: &str) -> Option<usize> {
    let (inst, _) = object.split_once('/')?;
    state.plate.whole_parts().iter().position(|p| instance_of(p) == inst)
}

/// The plate's overrides: the plate's own, then each part's part-wide settings and its boxes.
pub fn overrides(state: &AppState) -> Vec<Override> {
    let mut out = Vec::new();
    for (key, value) in &state.plate.settings {
        let Some(s) = print_setting(key) else { continue };
        let base = state.print.base_values.get(s.key).map(String::as_str).filter(|v| !v.is_empty() && *v != "nil").unwrap_or(s.default);
        let (v, b) = (value_text(s, value), value_text(s, base));
        out.push(Override {
            id: format!("plate:{key}"),
            target: Target::Plate { key: key.clone() },
            what: format!("{} {v} (profile {b})", s.label),
            whose: "All objects".into(),
            part: None,
        });
    }
    for (i, part) in state.plate.whole_parts().iter().enumerate() {
        for (key, value) in fab_plate::own_settings(&part.print) {
            let Some(s) = print_setting(key) else { continue };
            let global = global_value(state, s);
            let (v, g) = (value_text(s, value), value_text(s, global));
            let against = if same_value(&state.print, s, value, global) { "same as global: no effect".to_string() } else { format!("global {g}") };
            out.push(Override {
                id: format!("part:{}:{key}", part.object),
                target: Target::Part { object: part.object.clone(), key: key.clone() },
                what: format!("{} {v} ({against})", s.label),
                whose: part.name.clone(),
                part: Some(i),
            });
        }
        let inst = instance_of(part);
        for area in state.params.areas.iter().filter(|a| a.kind == AreaKind::Settings && a.object.split_once('/').is_some_and(|(i, _)| i == inst)) {
            let Some((_, solid)) = area.object.split_once('/') else { continue };
            let name = box_name(solid);
            let written: Vec<String> = box_settings(state, i, area).into_iter().map(|(label, value)| format!("{label} {value}")).collect();
            let keys = if written.is_empty() { "the object's own settings".to_string() } else { written.join(" · ") };
            out.push(Override {
                id: format!("box:{}", area.object),
                target: Target::Box { object: area.object.clone() },
                what: format!("{name}: {keys}"),
                whose: part.name.clone(),
                part: Some(i),
            });
        }
    }
    out
}

/// A box's setting row's value as its line shows it.
fn row_text(p: &ParamView) -> String {
    match &p.value {
        ParamValue::Num(x) if p.kind == ParamKind::Ratio => format!("{} %", (x * 1000.0).round() / 10.0),
        ParamValue::Num(x) => format!("{}", x.round()),
        ParamValue::Choice(i) => p.choices.get(*i).cloned().unwrap_or_default(),
        _ => String::new(),
    }
}

/// Whether the override is the one picked: a box picked in the view or the tree; a setting
/// whose row was clicked, while its object (the plate's: nothing) is selected and no box is.
pub fn is_picked(state: &AppState, o: &Override) -> bool {
    let setting = || state.ui.picked_override.as_deref() == Some(o.id.as_str()) && picked_volume(state).is_none();
    match &o.target {
        Target::Box { object } => picked_volume(state).as_deref() == Some(object.as_str()),
        Target::Part { .. } => setting() && o.part.is_some() && crate::ui::parts::active_part(state) == o.part,
        Target::Plate { .. } => setting() && crate::ui::parts::active_part(state).is_none(),
    }
}

/// Whether an override's id ([`Override::id`]) is a print settings box's.
pub fn is_box_id(id: &str) -> bool {
    id.starts_with("box:")
}

/// A setting's field in the Slicer to bring into view: scrolled to, its group open (the Slicer
/// comes forward, [`ShowPanel`]).
#[derive(Clone, Debug)]
pub struct RevealSetting(pub &'static str);

/// Pick an override, the one pick there is: a box (in the view, its part selected), or a
/// setting (its object selected, the plate's: nothing; any box picked is not), its field
/// brought into view in the Slicer.
pub fn pick(cx: &mut Cx, state: &AppState, o: &Override) {
    // the selection first: a selection unpicks any override
    match &o.target {
        Target::Box { object } => cx.action(PartAction::PickArea { object: object.clone() }),
        Target::Part { .. } => {
            if let Some(e) = o.part.and_then(|i| state.plate.parts.get(i)).and_then(|p| p.element) {
                cx.action(ShellAction::SelectOnly(e));
            }
        }
        Target::Plate { .. } => cx.action(ShellAction::SelectSet(Vec::new())),
    }
    cx.action(ShellAction::PickOverride(Some(o.id.clone())));
    if let Target::Part { key, .. } | Target::Plate { key } = &o.target {
        if let Some(s) = print_setting(key) {
            cx.action(ShowPanel(Editor::Slicer));
            cx.action(RevealSetting(s.key));
        }
    }
}

/// Delete an override: a setting goes back to what it inherits (the plate's to the profile, a
/// part's to the plate's), a box's statement goes; one undo step.
pub fn delete(cx: &mut Cx, o: &Override) {
    cx.action(match &o.target {
        Target::Plate { key } => PartAction::SetPlateSetting { key: key.clone(), value: None },
        Target::Part { object, key } => PartAction::SetPrintSetting { objects: vec![object.clone()], key: key.clone(), value: None },
        Target::Box { object } => PartAction::DeleteArea { object: object.clone() },
    });
}

#[derive(Script, Widget)]
pub struct FabOverrideList {
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
    items: HashMap<LiveId, WidgetRef>,
    /// The overrides of the last draw, each with its row's id and whether it is picked.
    #[rust]
    shown: Vec<(Override, LiveId, bool)>,
    /// What each row was last given (its lines and whether it is picked).
    #[rust]
    applied: HashMap<LiveId, (String, String, bool)>,
    #[redraw]
    #[rust]
    area: Area,
}

impl ScriptHook for FabOverrideList {
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
            for (id, w) in self.items.iter_mut() {
                let template = if *id == live_id!(empty) { live_id!(Empty) } else { live_id!(Row) };
                if let Some(t) = self.templates.get(&template) {
                    let v: ScriptValue = t.as_object().into();
                    w.script_apply(vm, apply, scope, v);
                }
            }
            self.applied.clear();
        }
    }
}

impl FabOverrideList {
    fn item(&mut self, cx: &mut Cx, id: LiveId, template: LiveId) -> Option<WidgetRef> {
        if let Some(w) = self.items.get(&id) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&template)?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        self.items.insert(id, w.clone());
        Some(w)
    }
}

fn row_key(o: &Override) -> u64 {
    LiveId::from_str(&o.id).0
}

impl Widget for FabOverrideList {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Overrides");
        node.set_size = self.shown.len() as u32;
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (pos, (o, id, picked)) in self.shown.iter().enumerate() {
            let rect = self.items.get(id).map(|w| w.area().rect(cx)).unwrap_or_default();
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::ListItem,
                bounds: makepad_platform::a11y::semantic_rect(rect),
                pos_in_set: pos as u32 + 1,
                set_size: self.shown.len() as u32,
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::SELECT,
                ..Default::default()
            };
            makepad_platform::a11y::set_string(&mut node.name, &o.what);
            makepad_platform::a11y::set_string(&mut node.description, &o.whose);
            node.states.set(a11y_tree::States::SELECTABLE, true);
            node.states.set(a11y_tree::States::SELECTED, *picked);
            out.push((row_key(o), None, node));
        }
    }

    fn semantics_part_area(&self, key: u64) -> Area {
        self.shown.iter().find(|(o, _, _)| row_key(o) == key).and_then(|(_, id, _)| self.items.get(id)).map_or(Area::Empty, |w| w.area())
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some((o, _, _)) = self.shown.iter().find(|(o, _, _)| row_key(o) == key).cloned() else { return ActionResult::Unsupported };
        let Some(state) = scope.data.get::<AppState>() else { return ActionResult::Unhandled };
        if request.action == Actions::PRESS || request.action == Actions::SELECT {
            pick(cx, state, &o);
            return ActionResult::Handled;
        }
        ActionResult::Unhandled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let mut picked = None;
        let mut deleted = None;
        for (o, id, _) in &self.shown {
            let Some(w) = self.items.get(id) else { continue };
            w.handle_event(cx, event, scope);
            // a click and an assistive press both arrive as the button's action
            if let Event::Actions(actions) = event {
                if w.button(cx, ids!(delete)).clicked(actions) {
                    deleted = Some(o.clone());
                    continue;
                }
            }
            match event.hits(cx, w.area()) {
                Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Hand),
                Hit::FingerUp(fe) if fe.is_over && !fe.cancelled && fe.device.is_primary_hit() && fe.was_tap() => picked = Some(o.clone()),
                _ => {}
            }
        }
        if let Some(o) = deleted {
            delete(cx, &o);
        } else if let (Some(o), Some(state)) = (picked, scope.data.get::<AppState>()) {
            pick(cx, state, &o);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        self.shown = overrides(state).into_iter().map(|o| {
            let on = is_picked(state, &o);
            let id = LiveId::from_str(&o.id);
            (o, id, on)
        }).collect();
        cx.begin_turtle(walk, self.layout);
        if self.shown.is_empty() {
            if let Some(w) = self.item(cx, live_id!(empty), live_id!(Empty)) {
                w.draw_all(cx, &mut Scope::empty());
            }
        }
        for k in 0..self.shown.len() {
            let (o, id, on) = self.shown[k].clone();
            let Some(w) = self.item(cx, id, live_id!(Row)) else { continue };
            let look = (o.what.clone(), o.whose.clone(), on);
            if self.applied.get(&id) != Some(&look) {
                w.label(cx, ids!(what)).set_text(cx, &look.0);
                w.label(cx, ids!(whose)).set_text(cx, &look.1);
                let mut row = w.clone();
                let selected = if on { 1.0 } else { 0.0 };
                script_apply_eval!(cx, row, {draw_bg +: {selected: #(selected)}});
                self.applied.insert(id, look);
            }
            w.draw_all(cx, &mut Scope::empty());
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A part with two settings of its own (the supports the same as the plate's) and a print
    /// settings box: one row for the plate's setting and three for the part, each saying what
    /// it is against what it inherits.
    #[test]
    fn the_rows_name_each_override_against_what_it_inherits() {
        let mut state = AppState::default();
        state.plate.settings = vec![("enable_support".into(), "1".into())];
        state.plate.parts = vec![PlatePart {
            object: "figure/body".into(),
            script: "figure".into(),
            name: "Figure".into(),
            print: vec![("enable_support".into(), "1".into()), ("sparse_infill_density".into(), "100%".into())],
            ..Default::default()
        }];
        state.params.areas = Arc::new(vec![MarkedArea { object: "figure/settings1".into(), kind: AreaKind::Settings, ..Default::default() }]);
        let rows = overrides(&state);
        let what: Vec<(&str, &str)> = rows.iter().map(|o| (o.what.as_str(), o.whose.as_str())).collect();
        assert_eq!(what, [
            ("Supports on (profile off)", "All objects"),
            ("Supports on (same as global: no effect)", "Figure"),
            ("Density 100 % (global 20 %)", "Figure"),
            ("Print settings box 1: the object's own settings", "Figure"),
        ]);
        assert_eq!(rows[3].target, Target::Box { object: "figure/settings1".into() });
    }

    /// A row's delete: a setting goes back to what it inherits, a box goes; one action each.
    #[test]
    fn a_rows_delete_is_one_action() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let part = Override { id: "part:figure/body:enable_support".into(), target: Target::Part { object: "figure/body".into(), key: "enable_support".into() }, what: String::new(), whose: String::new(), part: Some(0) };
        let block = Override { id: "box:figure/settings1".into(), target: Target::Box { object: "figure/settings1".into() }, ..part.clone() };
        let actions = cx.capture_actions(|cx| {
            delete(cx, &part);
            delete(cx, &block);
        });
        let sent: Vec<_> = actions.iter().filter_map(|a| a.downcast_ref::<PartAction>()).collect();
        assert_eq!(sent, [
            &PartAction::SetPrintSetting { objects: vec!["figure/body".into()], key: "enable_support".into(), value: None },
            &PartAction::DeleteArea { object: "figure/settings1".into() },
        ]);
    }

    #[test]
    fn a_box_reads_as_its_number_or_its_given_name() {
        assert_eq!(box_name("settings1"), "Print settings box 1");
        assert_eq!(box_name("settings12"), "Print settings box 12");
        assert_eq!(box_name("sphere_settings2"), "Print settings sphere 2");
        assert_eq!(box_name("left_arm_settings"), "Left arm");
        assert_eq!(box_name("left_arm_settings2"), "Left arm 2");
        assert_eq!(box_name("hinge"), "hinge");
        assert_eq!(box_name("neck_core"), "neck core");
        assert_eq!(box_name("solid_core"), "solid core");
    }

    /// A part's own setting the same as the plate's is listed (so it can go) but no override in
    /// the counts.
    #[test]
    fn a_setting_the_same_as_the_global_is_not_counted() {
        let mut state = AppState::default();
        state.plate.settings = vec![("enable_support".into(), "1".into())];
        let part = PlatePart { print: vec![("enable_support".into(), "1".into()), ("sparse_infill_density".into(), "100%".into())], ..Default::default() };
        assert_eq!(crate::ui::print_settings::override_count(&state, &part), 1);
        state.plate.settings.clear();
        assert_eq!(crate::ui::print_settings::override_count(&state, &part), 2);
    }
}
