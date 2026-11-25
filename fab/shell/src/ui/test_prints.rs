//! The plate's test prints, listed in the Test prints editor (each is a feature of its model in the
//! Features tree too). A test print is a box cut out of the plate to print that piece alone. It
//! holds its place, size, turn and name; what it prints comes from the plate, so nothing else of
//! it can be edited. A row has its tick (what prints when the test prints are chosen), its name
//! and its delete; under it, read-only, the overrides that reach into the box, for they print
//! there as on the original, each a click away from its row in the Overrides list. A click on a
//! row picks that one: its fields show below the list, its box and its piece in the view.

use crate::api::*;
use crate::print::{format_time, PrintAction};
use crate::ui::keymap::{self, ShortcutId as Key};
use crate::ui::overrides::{self, Override, Target};
use crate::ui::parts::active_part;
use crate::ui::editors::Editor;
use crate::ui::shell::ShowPanel;
use crate::ui::stage::StageAction;
use fab_plate::print_setting;
use makepad_widgets_core::tip::TipWidgetExt;
use makepad_widgets_core::widget_tree::CxWidgetExt;
use makepad_widgets_core::*;
use std::collections::HashMap;

/// What "+ Test cube" says while a part is picked (none picked, it says to pick one).
const ADD_TIP: &str = "A test cube on the picked part: only what is inside it prints, to test that piece";
/// The side of a new test cube (mm), when the part is that big.
pub const NEW_SIDE: f64 = 40.0;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabTestPrintListBase = #(FabTestPrintList::register_widget(vm))
    mod.widgets.FabTestPrintList = set_type_default() do mod.widgets.FabTestPrintListBase{
        width: Fill height: Fit flow: Down spacing: 0
        Row := View{
            width: Fill height: Fit flow: Right spacing: 4
            align: Align{y: 0.5}
            padding: Inset{left: 10 right: 4 top: 2 bottom: 2}
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
            FabTip{text: "Ticked: it shows, slices and prints when the test prints are chosen"
                tick := mod.widgets.FabCheckBox{width: 16 text: "" a11y: {label: "Print this test"}}
            }
            text := View{
                width: Fill height: Fit flow: Down spacing: 1
                name := mod.widgets.FabLabel{width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis}
                whose := mod.widgets.FabLabelSmall{width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis}
            }
            FabTip{text: "Delete this test print"
                delete := mod.widgets.FabIconButton{
                    width: 24 height: 22
                    a11y: {label: "Delete test print"}
                    draw_icon +: {color: fab.color_text_dim svg: crate_resource("self://resources/icons/trash.svg")}
                }
            }
        }
        Use := View{
            width: Fill height: Fit
            padding: Inset{left: 32 right: 6 top: 1 bottom: 3}
            cursor: MouseCursor.Hand
            what := mod.widgets.FabLabelSmall{width: Fill text: "" max_lines: 2}
        }
        Empty := mod.widgets.FabLabelDim{
            width: Fill text: "No test prints: add a cube to print just that piece of the plate."
            margin: Inset{left: 10 right: 6 top: 4 bottom: 4}
        }
    }

    mod.widgets.FabTestPrintsBase = #(FabTestPrints::register_widget(vm))
    mod.widgets.FabTestPrints = set_type_default() do mod.widgets.FabTestPrintsBase{
        width: Fill height: Fit flow: Down
        section := View{
            width: Fill height: Fit flow: Down
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: 6 bottom: 6}
            mode := View{
                width: Fill height: Fit flow: Right spacing: 2
                padding: Inset{left: 10 right: 6 top: 2 bottom: 4}
                a11y: {role: "radio_group" label: "What the Slicer slices"}
                mod.widgets.FabTip{text: "Slice and print the plate, as it is"
                    whole := mod.widgets.FabSegmentTab{text: "Whole plate" a11y: {label: "Whole plate"}}
                }
                mod.widgets.FabTip{text: "Slice and print only the ticked test prints, on the bed where they will print"
                    tests := mod.widgets.FabSegmentTab{text: "Test prints" a11y: {label: "Test prints"}}
                }
            }
            list := mod.widgets.FabTestPrintList{}
            totals := mod.widgets.FabLabelSmall{
                visible: false
                width: Fill text: "" margin: Inset{left: 10 right: 6 top: 2 bottom: 2}
            }
            add_row := View{
                width: Fill height: Fit flow: Right
                padding: Inset{left: 10 right: 6 top: 4 bottom: 4}
                add_tip := FabTip{text: #(ADD_TIP)
                    add := mod.widgets.FabButton{
                        text: "+ Test cube" a11y: {label: "Add test cube"}
                        height: theme.prop_row_height padding: Inset{left: 8 right: 8}
                    }
                }
            }
            fields := View{
                visible: false
                width: Fill height: Fit flow: Down spacing: theme.prop_row_gap
                name_row := PropRow{
                    label +: {text: "Name"}
                    value +: {name := mod.widgets.FabInput{width: Fill a11y: {label: "Test print name"}}}
                }
                x := PropNumberRow{
                    label +: {text: "Position X"}
                    value +: {num +: {a11y: {label: "Test print X"} min: -1000.0 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}}
                }
                y := PropNumberRow{
                    label +: {text: "Y"}
                    value +: {num +: {a11y: {label: "Test print Y"} min: -1000.0 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}}
                }
                z := PropNumberRow{
                    label +: {text: "Z"}
                    value +: {num +: {a11y: {label: "Test print Z"} min: -1000.0 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}}
                }
                sx := PropNumberRow{
                    label +: {text: "Size X"}
                    value +: {num +: {a11y: {label: "Test print size X"} min: 0.1 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}}
                }
                sy := PropNumberRow{
                    label +: {text: "Y"}
                    value +: {num +: {a11y: {label: "Test print size Y"} min: 0.1 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}}
                }
                sz := PropNumberRow{
                    label +: {text: "Z"}
                    value +: {num +: {a11y: {label: "Test print size Z"} min: 0.1 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}}
                }
                tx := PropNumberRow{
                    label +: {text: "Turn X"}
                    value +: {num +: {a11y: {label: "Test print turn X"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}}
                }
                ty := PropNumberRow{
                    label +: {text: "Y"}
                    value +: {num +: {a11y: {label: "Test print turn Y"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}}
                }
                tz := PropNumberRow{
                    label +: {text: "Z"}
                    value +: {num +: {a11y: {label: "Test print turn Z"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}}
                }
                piece := PropRow{
                    height: Fit
                    label +: {text: "Piece"}
                    value +: {
                        height: Fit flow: Right{wrap: true} spacing: 4
                        FabTip{text: "Print the piece as it stands in the print, on the print's layers"
                            as_printed := mod.widgets.FabButton{
                                text: "As printed" a11y: {label: "Test print as printed"}
                                height: theme.prop_row_height padding: Inset{left: 8 right: 8}
                            }
                        }
                        FabTip{text: #(format!("Lay flat: click a surface of the piece, or a cut face, to turn it face down onto the bed ({} cancels)", keymap::shortcut(Key::Cancel)))
                            lay_flat := mod.widgets.FabButton{
                                text: "Lay flat" a11y: {label: "Test print lay flat"}
                                height: theme.prop_row_height padding: Inset{left: 8 right: 8}
                            }
                        }
                    }
                }
                px := PropNumberRow{
                    label +: {text: "Pose X"}
                    value +: {num +: {a11y: {label: "Test print pose X"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}}
                }
                py := PropNumberRow{
                    label +: {text: "Y"}
                    value +: {num +: {a11y: {label: "Test print pose Y"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}}
                }
                pz := PropNumberRow{
                    label +: {text: "Z"}
                    value +: {num +: {a11y: {label: "Test print pose Z"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}}
                }
                pose_note := mod.widgets.FabLabelSmall{
                    visible: false
                    width: Fill text: "Laid flat: its own layers. Tests this orientation, not the print's."
                    margin: Inset{left: 10 right: 6 top: 2 bottom: 2}
                }
                adhesion := PropRow{
                    label +: {text: "Adhesion"}
                    value +: {
                        FabTip{text: "A brim of its own round the piece's first layer, at the profile's brim width (Fab's 5 mm where the profile's brim takes none): for a tall piece on a small footing"
                            brim := mod.widgets.FabCheckBox{text: "Brim" a11y: {label: "Test print brim"}}
                        }
                    }
                }
            }
        }
    }

    // The Test prints editor: the list, its "+ Test cube" and the picked one's fields, scrolled.
    mod.widgets.FabTestPrintsPanel = View{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector; the tests scroll under it
        editor_bar := mod.widgets.FabEditorBar{}
        body := mod.widgets.FabScroll{
            tests := mod.widgets.FabTestPrints{}
        }
    }
}

/// An override reaching into a test print's box, as its row reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Use {
    pub text: String,
    /// Its row in the Overrides list: a click goes there.
    pub of: Override,
}

/// The overrides that reach into `t`, each setting on a line of its own: the part-wide settings
/// of the parts it reaches, and the print settings boxes that reach into it on those parts. They
/// are the original's; a test print has none of its own. The plate's own settings reach
/// everything, so they say nothing here.
pub fn uses(state: &AppState, t: &TestPrintView) -> Vec<Use> {
    let reached = |i: usize| state.plate.whole_parts().get(i).is_some_and(|p| t.reaches(p.min_mm, p.max_mm));
    let mut out = Vec::new();
    for o in overrides::overrides(state) {
        let Some(part) = o.part.filter(|i| reached(*i)).and_then(|i| state.plate.whole_parts().get(i)) else { continue };
        match &o.target {
            Target::Plate { .. } => {}
            Target::Part { key, .. } => {
                let Some(s) = print_setting(key) else { continue };
                let Some((_, value)) = fab_plate::own_settings(&part.print).find(|(k, _)| k == key) else { continue };
                out.push(Use { text: format!("Uses: {}: {} = {}", part.name, s.label, overrides::value_text(s, value)), of: o.clone() });
            }
            Target::Box { object } => {
                let Some(area) = state.params.areas.iter().find(|a| a.object == *object) else { continue };
                let points = || area.bodies.iter().flat_map(|b| b.positions.iter());
                let lo = std::array::from_fn(|k| points().map(|q| q[k]).fold(f64::MAX, f64::min));
                let hi = std::array::from_fn(|k| points().map(|q| q[k]).fold(f64::MIN, f64::max));
                if !t.reaches(lo, hi) {
                    continue;
                }
                let name = overrides::box_name(object.split_once('/').map_or(object.as_str(), |(_, s)| s));
                let written = o.part.map(|i| overrides::box_settings(state, i, area)).unwrap_or_default();
                for (label, value) in &written {
                    out.push(Use { text: format!("Uses: {name}: {label} = {value}"), of: o.clone() });
                }
                if written.is_empty() {
                    out.push(Use { text: format!("Uses: {name}: the object's own settings"), of: o.clone() });
                }
            }
        }
    }
    out
}

/// The test print picked (its feature's row picked in the tree).
pub fn picked(state: &AppState) -> Option<&TestPrintView> {
    let f = state.source.feature(state.source.selected.as_ref()?)?;
    (f.detail == crate::ui::code::TEST_PRINT).then(|| state.test_prints.iter().find(|t| t.name == f.name()))?
}

/// What the test prints come to, next to the whole plate's, while their slice stands.
fn totals_line(state: &AppState) -> Option<String> {
    let p = &state.print;
    let shown = p.test_mode.then(|| p.preview.as_ref().filter(|s| !p.stale && !s.pieces.is_empty())).flatten()?;
    let grams: f64 = shown.filament_use.iter().map(|f| f.grams).sum();
    let whole = match p.whole_figures {
        Some((seconds, g)) => format!("Whole plate: {}, {g:.1} g", format_time(seconds)),
        None => "Whole plate: not sliced".to_string(),
    };
    Some(format!("All of them: {}, {grams:.1} g. {whole}", format_time(shown.total)))
}

/// The picked test print as it shows: a drag's or a field's placement, until its edit lands.
pub fn shown(state: &AppState) -> Option<TestPrintView> {
    let t = picked(state)?;
    Some(match state.ui.test_drag.as_ref().filter(|d| d.name == t.name) {
        Some(d) => TestPrintView { center: d.center, size: d.size, turn: d.turn, pose: d.pose, ..t.clone() },
        None => t.clone(),
    })
}

/// The test prints' rows in the Features tree: those of the script it shows, after its own.
pub fn show_rows(state: &mut AppState) {
    state.source.features.retain(|f| f.detail != crate::ui::code::TEST_PRINT);
    let prefix = format!("{}/", state.source.instance);
    let rows: Vec<_> = state.test_prints.iter().filter(|t| t.object.starts_with(&prefix)).map(|t| crate::ui::code::FeatureRow::test_print(&t.name, &t.object)).collect();
    state.source.features.extend(rows);
    if state.source.selected.as_ref().is_some_and(|id| state.source.feature(id).is_none()) {
        state.source.selected = None;
    }
}

/// "+ Test cube": a true cube in the middle of the part under the click, or else the picked one;
/// with none, nothing but the status saying why. The Test prints editor, which lists them, comes
/// forward; the view stays as it is.
pub fn add(cx: &mut Cx, state: &AppState, under: Option<String>) {
    if state.plate.whole.is_some() {
        cx.action(ShellAction::Notice { view: NoticeView::Plate, text: "Test print: a cube goes on the model, choose Whole plate".into() });
        return;
    }
    match under.or_else(|| active_part(state).map(|i| state.plate.parts[i].object.clone())) {
        Some(object) => {
            if !state.ui.in_mode(PlateMode::Design) {
                cx.action(ShellAction::SetWorkspace(Workspace::Design));
            }
            cx.action(TestPrintAction::Add { object });
            cx.action(ShowPanel(Editor::TestPrints));
        }
        None => cx.action(ShellAction::Notice { view: NoticeView::Plate, text: format!("Test print: {}, the cube goes on it", crate::ui::stage::SELECT_A_PART) }),
    }
}

/// What the test slice flags of `t`'s piece standing on the bed (a tall piece on a small
/// footing), as Fab's checks word it, and what brim it has.
pub fn footing_flags(state: &AppState, t: &TestPrintView) -> Vec<String> {
    let p = &state.print;
    let Some(figures) = p.preview.as_ref().filter(|_| p.test_mode && !p.stale).and_then(|pv| pv.pieces.iter().find(|f| f.name == t.name)) else { return Vec::new() };
    p.report().issues.iter().filter(|i| i.kind == "plate_contact" && i.object.as_ref().is_some_and(|o| figures.objects.contains(o))).map(|i| {
        let brim = if t.brim {
            "it has a brim of its own"
        } else if figures.brim_grams > 0.0 {
            "the profile's brim rings it"
        } else {
            "give it a brim in its fields"
        };
        format!("{}: {brim}", i.message)
    }).collect()
}

enum Item {
    Test(TestPrintView, bool),
    Use(Use),
    /// What the slice flags of its piece's footing, the test print's name with it.
    Flag(String, String),
}

impl Item {
    /// Stable across rebuilds: its row's identity.
    fn id(&self, test: &str, k: usize) -> LiveId {
        match self {
            Item::Test(..) => LiveId::from_str(&format!("test:{test}")),
            Item::Use(_) => LiveId::from_str(&format!("test:{test}:{k}")),
            Item::Flag(..) => LiveId::from_str(&format!("test:{test}:flag{k}")),
        }
    }
}

#[derive(Script, Widget)]
pub struct FabTestPrintList {
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
    /// The rows of the last draw, each with its row's id.
    #[rust]
    shown: Vec<(Item, LiveId)>,
    /// What each row was last given.
    #[rust]
    applied: HashMap<LiveId, (String, String, bool, bool)>,
    #[redraw]
    #[rust]
    area: Area,
}

impl ScriptHook for FabTestPrintList {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.templates.clear();
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, value: ScriptValue) {
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
            self.items.clear();
            self.applied.clear();
        }
    }
}

impl FabTestPrintList {
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

impl Widget for FabTestPrintList {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Test prints");
        node.set_size = self.shown.iter().filter(|(i, _)| matches!(i, Item::Test(..))).count() as u32;
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (pos, (item, id)) in self.shown.iter().enumerate() {
            let rect = self.items.get(id).map(|w| w.area().rect(cx)).unwrap_or_default();
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::ListItem,
                bounds: makepad_platform::a11y::semantic_rect(rect),
                pos_in_set: pos as u32 + 1,
                set_size: self.shown.len() as u32,
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::SELECT,
                ..Default::default()
            };
            match item {
                Item::Test(t, on) => {
                    makepad_platform::a11y::set_string(&mut node.name, &t.name);
                    node.states.set(a11y_tree::States::SELECTABLE, true);
                    node.states.set(a11y_tree::States::SELECTED, *on);
                }
                Item::Use(u) => makepad_platform::a11y::set_string(&mut node.name, &u.text),
                Item::Flag(_, text) => makepad_platform::a11y::set_string(&mut node.name, text),
            }
            out.push((id.0, None, node));
        }
    }

    fn semantics_part_area(&self, key: u64) -> Area {
        self.items.get(&LiveId(key)).map_or(Area::Empty, |w| w.area())
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some((item, _)) = self.shown.iter().find(|(_, id)| id.0 == key) else { return ActionResult::Unsupported };
        let Some(state) = scope.data.get::<AppState>() else { return ActionResult::Unhandled };
        if request.action == Actions::PRESS || request.action == Actions::SELECT {
            match item {
                Item::Test(t, _) => cx.action(TestPrintAction::Pick { name: t.name.clone() }),
                Item::Use(u) => overrides::pick(cx, state, &u.of),
                Item::Flag(name, _) => cx.action(TestPrintAction::Pick { name: name.clone() }),
            }
            return ActionResult::Handled;
        }
        ActionResult::Unhandled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let mut go = None;
        for (item, id) in &self.shown {
            let Some(w) = self.items.get(id) else { continue };
            w.handle_event(cx, event, scope);
            if let (Event::Actions(actions), Item::Test(t, _)) = (event, item) {
                // a click and an assistive press both arrive as the button's action
                if w.button(cx, ids!(delete)).clicked(actions) {
                    cx.action(TestPrintAction::Delete { name: t.name.clone() });
                    continue;
                }
                if let Some(on) = w.check_box(cx, ids!(tick)).changed(actions) {
                    cx.action(TestPrintAction::Tick { name: t.name.clone(), on });
                    continue;
                }
            }
            match event.hits(cx, w.area()) {
                Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Hand),
                Hit::FingerUp(fe) if fe.is_over && !fe.cancelled && fe.device.is_primary_hit() && fe.was_tap() => go = Some(*id),
                _ => {}
            }
        }
        match go.and_then(|id| self.shown.iter().find(|(_, i)| *i == id)) {
            Some((Item::Test(t, _), _)) => cx.action(TestPrintAction::Pick { name: t.name.clone() }),
            Some((Item::Use(u), _)) => {
                if let Some(state) = scope.data.get::<AppState>() {
                    overrides::pick(cx, state, &u.of);
                }
            }
            Some((Item::Flag(name, _), _)) => cx.action(TestPrintAction::Pick { name: name.clone() }),
            None => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let picked_name = picked(state).map(|t| t.name.clone());
        self.shown.clear();
        for t in &state.test_prints {
            let on = picked_name.as_deref() == Some(t.name.as_str());
            let item = Item::Test(t.clone(), on);
            let id = item.id(&t.name, 0);
            self.shown.push((item, id));
            for (k, u) in uses(state, t).into_iter().enumerate() {
                let item = Item::Use(u);
                let id = item.id(&t.name, k);
                self.shown.push((item, id));
            }
            for (k, text) in footing_flags(state, t).into_iter().enumerate() {
                let item = Item::Flag(t.name.clone(), text);
                let id = item.id(&t.name, k);
                self.shown.push((item, id));
            }
        }
        let figures = state.print.test_mode.then(|| state.print.preview.as_ref().filter(|_| !state.print.stale).map(|p| p.pieces.as_slice())).flatten();
        let name_of = |object: &str| state.plate.whole_parts().iter().find(|p| p.object == object).map_or_else(|| object.to_string(), |p| p.name.clone());
        let looks: Vec<(String, String, bool, bool)> = self.shown.iter().map(|(item, _)| match item {
            Item::Test(t, on) => {
                let sliced = figures.and_then(|f| f.iter().find(|p| p.name == t.name));
                let whose = match sliced {
                    Some(p) => format!("{}, {:.1} g", format_time(p.seconds), p.grams),
                    None => format!("{:.0} x {:.0} x {:.0} mm, on {}", t.size[0], t.size[1], t.size[2], name_of(&t.object)),
                };
                (t.name.clone(), whose, *on, t.on)
            }
            Item::Use(u) => (u.text.clone(), String::new(), false, false),
            Item::Flag(_, text) => (text.clone(), String::new(), false, false),
        }).collect();
        cx.begin_turtle(walk, self.layout);
        if self.shown.is_empty() {
            if let Some(w) = self.item(cx, live_id!(empty), live_id!(Empty)) {
                w.draw_all(cx, &mut Scope::empty());
            }
        }
        for (k, look) in looks.into_iter().enumerate() {
            let id = self.shown[k].1;
            let is_test = matches!(self.shown[k].0, Item::Test(..));
            let Some(w) = self.item(cx, id, if is_test { live_id!(Row) } else { live_id!(Use) }) else { continue };
            if self.applied.get(&id) != Some(&look) {
                if is_test {
                    w.label(cx, ids!(name)).set_text(cx, &look.0);
                    w.label(cx, ids!(whose)).set_text(cx, &look.1);
                    w.check_box(cx, ids!(tick)).set_active(cx, look.3, Animate::No);
                    let mut row = w.clone();
                    let selected = if look.2 { 1.0 } else { 0.0 };
                    script_apply_eval!(cx, row, {draw_bg +: {selected: #(selected)}});
                } else {
                    w.label(cx, ids!(what)).set_text(cx, &look.0);
                }
                self.applied.insert(id, look);
            }
            w.draw_all(cx, &mut Scope::empty());
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

/// The list, its "+ Test cube" and the picked one's fields.
#[derive(Script, ScriptHook, Widget)]
pub struct FabTestPrints {
    #[deref]
    view: View,
    /// The name its field was last given: typing in it is left alone until the name changes.
    #[rust]
    name_shown: Option<String>,
}

impl Widget for FabTestPrints {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        let v = &self.view;
        if v.button(cx, ids!(section.add_row.add)).clicked(actions) {
            add(cx, state, None);
        }
        if v.radio_button(cx, ids!(section.mode.whole)).clicked(actions) {
            cx.action(PrintAction::TestMode(false));
        }
        if v.radio_button(cx, ids!(section.mode.tests)).clicked(actions) {
            cx.action(PrintAction::TestMode(true));
        }
        let Some(t) = picked(state).cloned() else { return };
        if let Some(on) = v.check_box(cx, ids!(section.fields.adhesion.value.brim)).changed(actions) {
            cx.action(TestPrintAction::Brim { name: t.name.clone(), on });
        }
        if v.button(cx, ids!(section.fields.piece.value.lay_flat)).clicked(actions) {
            cx.action(StageAction::LayFlat(!state.ui.lay_flat));
        }
        // From the placement as it shows: a field committed a moment ago is held there until the
        // plate has it, so the next field's edit keeps it.
        let mut shown = state.ui.test_drag.clone().filter(|d| d.name == t.name).unwrap_or_else(|| TestDrag { name: t.name.clone(), center: t.center, size: t.size, turn: t.turn, pose: t.pose });
        let fields = [ids!(section.fields.x.value.num), ids!(section.fields.y.value.num), ids!(section.fields.z.value.num)];
        let sizes = [ids!(section.fields.sx.value.num), ids!(section.fields.sy.value.num), ids!(section.fields.sz.value.num)];
        let mut live = None;
        let mut done = None;
        if v.button(cx, ids!(section.fields.piece.value.as_printed)).clicked(actions) {
            shown.pose = None;
            done = Some(shown.clone());
        }
        for (k, id) in [ids!(section.fields.px.value.num), ids!(section.fields.py.value.num), ids!(section.fields.pz.value.num)].into_iter().enumerate() {
            if let Some(x) = v.value_input(cx, id).ended(actions) {
                let mut pose = shown.pose.unwrap_or([0.0; 3]);
                pose[k] = x;
                shown.pose = Some(pose);
                done = Some(shown.clone());
            }
        }
        for (k, id) in fields.into_iter().enumerate() {
            let f = v.value_input(cx, id);
            if let Some(x) = f.changed(actions) {
                shown.center[k] = x;
                live = Some(shown.clone());
            }
            if let Some(x) = f.ended(actions) {
                shown.center[k] = x;
                done = Some(shown.clone());
            }
        }
        for (k, id) in sizes.into_iter().enumerate() {
            let f = v.value_input(cx, id);
            if let Some(x) = f.changed(actions) {
                shown.size[k] = x;
                live = Some(shown.clone());
            }
            if let Some(x) = f.ended(actions) {
                shown.size[k] = x;
                done = Some(shown.clone());
            }
        }
        for (k, id) in [ids!(section.fields.tx.value.num), ids!(section.fields.ty.value.num), ids!(section.fields.tz.value.num)].into_iter().enumerate() {
            let f = v.value_input(cx, id);
            if let Some(x) = f.changed(actions) {
                shown.turn[k] = x;
                live = Some(shown.clone());
            }
            if let Some(x) = f.ended(actions) {
                shown.turn[k] = x;
                done = Some(shown.clone());
            }
        }
        if let Some(drag) = live {
            state.ui.test_drag = Some(drag);
            crate::ui::redraw(cx, &[ids!(viewport), ids!(test_cube)]);
        }
        if let Some(d) = done {
            // Held as the gizmo's drop is, until its edit lands (`test_place`).
            state.ui.test_drag = Some(d.clone());
            cx.action(TestPrintAction::Place { name: d.name.clone(), center: d.center, size: d.size, turn: d.turn, pose: d.pose });
        }
        let input = v.text_input(cx, ids!(section.fields.name_row.value.name));
        if input.returned(actions).is_some() || input.key_focus_lost(actions) {
            let to = input.text().trim().to_string();
            if !to.is_empty() && to != t.name {
                cx.action(TestPrintAction::Rename { name: t.name, to });
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            let v = &self.view;
            let has_part = active_part(state).is_some();
            v.radio_button(cx, ids!(section.mode.whole)).set_active(cx, !state.print.test_mode, Animate::No);
            v.radio_button(cx, ids!(section.mode.tests)).set_active(cx, state.print.test_mode, Animate::No);
            let totals = totals_line(state);
            v.widget(cx, ids!(section.totals)).set_visible(cx, totals.is_some());
            if let Some(line) = totals {
                v.label(cx, ids!(section.totals)).set_text(cx, &line);
            }
            v.widget(cx, ids!(section.add_row.add)).set_disabled(cx, !has_part);
            v.tip(cx, ids!(section.add_row.add_tip)).set_text(if has_part { ADD_TIP } else { crate::ui::stage::SELECT_A_PART });
            let t = shown(state);
            v.widget(cx, ids!(section.fields)).set_visible(cx, t.is_some());
            if let Some(t) = t {
                let (c, s, a) = (t.center, t.size, t.turn);
                v.value_input(cx, ids!(section.fields.x.value.num)).set_value(cx, c[0]);
                v.value_input(cx, ids!(section.fields.y.value.num)).set_value(cx, c[1]);
                v.value_input(cx, ids!(section.fields.z.value.num)).set_value(cx, c[2]);
                v.value_input(cx, ids!(section.fields.sx.value.num)).set_value(cx, s[0]);
                v.value_input(cx, ids!(section.fields.sy.value.num)).set_value(cx, s[1]);
                v.value_input(cx, ids!(section.fields.sz.value.num)).set_value(cx, s[2]);
                v.value_input(cx, ids!(section.fields.tx.value.num)).set_value(cx, a[0]);
                v.value_input(cx, ids!(section.fields.ty.value.num)).set_value(cx, a[1]);
                v.value_input(cx, ids!(section.fields.tz.value.num)).set_value(cx, a[2]);
                let pose = t.pose.unwrap_or([0.0; 3]);
                v.value_input(cx, ids!(section.fields.px.value.num)).set_value(cx, pose[0]);
                v.value_input(cx, ids!(section.fields.py.value.num)).set_value(cx, pose[1]);
                v.value_input(cx, ids!(section.fields.pz.value.num)).set_value(cx, pose[2]);
                v.widget(cx, ids!(section.fields.piece.value.as_printed)).set_disabled(cx, t.pose.is_none());
                v.widget(cx, ids!(section.fields.pose_note)).set_visible(cx, t.pose.is_some());
                v.check_box(cx, ids!(section.fields.adhesion.value.brim)).set_active(cx, t.brim, Animate::No);
                if self.name_shown.as_deref() != Some(t.name.as_str()) {
                    v.text_input(cx, ids!(section.fields.name_row.value.name)).set_text(cx, &t.name);
                    self.name_shown = Some(t.name.clone());
                }
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{AreaKind, GhostBody, MarkedArea};
    use std::sync::Arc;

    fn cube() -> TestPrintView {
        TestPrintView { name: "Neck".into(), object: "figure/body".into(), center: [100.0, 100.0, 80.0], size: [40.0; 3], turn: [0.0; 3], pose: None, lay: None, on: true, brim: false }
    }

    fn figure(state: &mut AppState) {
        state.plate.parts = vec![
            PlatePart { object: "figure/body".into(), script: "figure".into(), name: "Figure".into(), min_mm: [80.0, 80.0, 0.0], max_mm: [120.0, 120.0, 120.0], print: vec![("wall_loops".into(), "4".into())], ..Default::default() },
            PlatePart { object: "stand/stand".into(), script: "stand".into(), name: "Stand".into(), min_mm: [200.0, 200.0, 0.0], max_mm: [220.0, 220.0, 20.0], print: vec![("sparse_infill_density".into(), "50%".into())], ..Default::default() },
        ];
    }

    /// The Overrides row a box's area makes, at those bounds.
    fn area(object: &str, lo: [f64; 3], hi: [f64; 3]) -> MarkedArea {
        MarkedArea {
            object: object.into(),
            kind: AreaKind::Settings,
            bodies: vec![GhostBody { positions: vec![lo, hi], ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn a_row_lists_only_the_overrides_that_reach_into_its_box() {
        let mut state = AppState::default();
        figure(&mut state);
        state.plate.settings = vec![("enable_support".into(), "1".into())];
        state.params.areas = Arc::new(vec![
            area("figure/settings1", [95.0, 95.0, 70.0], [105.0, 105.0, 90.0]),
            area("figure/settings2", [90.0, 90.0, 10.0], [110.0, 110.0, 20.0]),
        ]);
        let texts: Vec<String> = uses(&state, &cube()).into_iter().map(|u| u.text).collect();
        assert_eq!(texts, ["Uses: Figure: Wall loops = 4", "Uses: Print settings box 1: the object's own settings"]);
        // A click on one goes to that override's row.
        let u = uses(&state, &cube());
        assert_eq!(u[1].of.target, Target::Box { object: "figure/settings1".into() });
    }

    #[test]
    fn a_turned_box_reaches_what_its_corners_do_not_cover() {
        let t = TestPrintView { center: [0.0, 0.0, 0.0], size: [10.0; 3], turn: [0.0, 0.0, 45.0], ..cube() };
        // Inside the unturned box's corner, outside the turned one.
        assert!(!t.reaches([4.5, 4.5, -1.0], [5.0, 5.0, 1.0]));
        assert!(t.reaches([6.0, -1.0, -1.0], [7.0, 1.0, 1.0]));
        assert!(!t.reaches([-1.0, -1.0, 6.0], [1.0, 1.0, 7.0]), "above it");
        // Tilted, its top corner reaches where its side did not.
        let tilted = TestPrintView { turn: [45.0, 0.0, 0.0], ..t };
        assert!(tilted.reaches([-1.0, -1.0, 6.0], [1.0, 1.0, 7.0]));
    }
}
