//! The plate's test prints, listed in the Tests sidebar in Design and Test prints (each
//! is a feature of its model in the Parts tree too). A test print is a box cut out of the plate
//! to print that piece alone. It holds its place, size, turn and name; what it prints comes from
//! the plate, so nothing else of it can be edited. Each test is a section that folds: its tick
//! (what prints in Test prints), its name and its delete in its bar; unfolded, its fields, and
//! read-only the overrides that reach into its box (they print there as on the original) and what
//! its footing flags. Unfolding a test picks it: its box and its piece show in the view.

use crate::api::*;
use crate::print::format_time;
use crate::ui::keymap::{self, ShortcutId as Key};
use crate::ui::overrides::{self, Override, Target};
use crate::ui::parts::active_part;
use crate::ui::stage::StageAction;
use fab_plate::print_setting;
use makepad_widgets_core::widget_tree::CxWidgetExt;
use makepad_widgets_core::*;
use std::collections::HashMap;

/// What "+ Test cube" says while a part is picked (none picked, it says to pick one).
pub const ADD_TIP: &str = "A test cube on the picked part: only what is inside it prints, to test that piece";
/// The side of a new test cube (mm), when the part is that big.
pub const NEW_SIDE: f64 = 40.0;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // A row's name column, as wide as the Part panel's (`param_panel::LABEL_W` at most), so the
    // fields start at one line in both editors.
    let NamedRow = PropRow{label +: {width: Fill{weight: 0 basis: "42%" max: #(crate::ui::param_panel::LABEL_W)}}}
    // A field's three axes as one row, as the Part panel's Transform: X, Y and Z side by side,
    // each named inside its field.
    let AxesRow = NamedRow{value +: {flow: Right spacing: 4}}
    let Axis = mod.widgets.ValueInput{width: Fill height: Fill min_height: 0}

    // The test prints, each a section that folds: its tick, its name and its delete in the bar,
    // its properties under it (unfolding it picks it: its box and its piece show in the view).
    mod.widgets.FabTestSection = #(FabTestSection::register_widget(vm)){
        grip: false
        margin: mod.widgets.PropSection.margin
        draw_bg: mod.widgets.PropSection.draw_bg
        body: mod.widgets.PropSection.body
        header: mod.widgets.PropSection.header{hdr +: {
            draw_bg +: {
                // The opaque row ground keeps the resting bar dim on the floating card.
                color: fab.color_editor color_hover: fab.color_editor
                selected: instance(0.0) color_selected: uniform(fab.color_selection_bg)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                    sdf.fill(self.color.mix(self.color_hover, self.hover).mix(self.color_selected, self.selected))
                    return sdf.result
                }
            }
            title +: {draw_text +: {
                hover: instance(0.0) color_hover: uniform(fab.color_text_hover)
                get_color: fn() { return self.color.mix(self.color_hover, self.hover) }
            }}
        }}
    }
    mod.widgets.FabTestSectionsBase = #(FabTestSections::register_widget(vm))
    mod.widgets.FabTestSections = set_type_default() do mod.widgets.FabTestSectionsBase{
        width: Fill height: Fit flow: Down
        Test := mod.widgets.FabTestSection{
            header +: {hdr +: {
                title +: {text: ""}
                trail +: {
                    width: Fit height: Fit flow: Right spacing: 2 align: Align{y: 0.5}
                    FabTip{text: "Ticked: it shows, slices and prints in Test prints"
                        tick := mod.widgets.FabCheckBox{width: 16 text: "" a11y: {label: "Print this test"}}
                    }
                    FabTip{text: "Delete this test print"
                        delete := mod.widgets.FabIconButton{
                            width: 24 height: 22
                            a11y: {label: "Delete test print"}
                            draw_icon +: {color: fab.color_text_dim svg: crate_resource("self://resources/icons/trash.svg")}
                        }
                    }
                }
            }}
            body +: {
                flow: Down spacing: theme.prop_row_gap padding: Inset{top: 4 bottom: 8}
                whose := mod.widgets.FabLabelSmall{width: Fill text: ""}
                    name_row := NamedRow{
                        label +: {text: "Name"}
                        value +: {name := mod.widgets.FabInput{width: Fill a11y: {label: "Test print name"}}}
                    }
                    // Position: X, Y and Z side by side, each named inside its field
                    pos := AxesRow{
                        label +: {text: "Position"}
                        value +: {
                            x := Axis{label: "X" a11y: {label: "Test print X"} min: -1000.0 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}
                            y := Axis{label: "Y" a11y: {label: "Test print Y"} min: -1000.0 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}
                            z := Axis{label: "Z" a11y: {label: "Test print Z"} min: -1000.0 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}
                        }
                    }
                    // Size: X, Y and Z side by side, each named inside its field
                    size := AxesRow{
                        label +: {text: "Size"}
                        value +: {
                            sx := Axis{label: "X" a11y: {label: "Test print size X"} min: 0.1 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}
                            sy := Axis{label: "Y" a11y: {label: "Test print size Y"} min: 0.1 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}
                            sz := Axis{label: "Z" a11y: {label: "Test print size Z"} min: 0.1 max: 2000.0 step: 0.5 snap: 5.0 precision: 2 suffix: " mm"}
                        }
                    }
                    // Turn: X, Y and Z side by side, each named inside its field
                    turn := AxesRow{
                        label +: {text: "Turn"}
                        value +: {
                            tx := Axis{label: "X" a11y: {label: "Test print turn X"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}
                            ty := Axis{label: "Y" a11y: {label: "Test print turn Y"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}
                            tz := Axis{label: "Z" a11y: {label: "Test print turn Z"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}
                        }
                    }
                    piece := NamedRow{
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
                    // Pose: X, Y and Z side by side, each named inside its field
                    pose := AxesRow{
                        label +: {text: "Pose"}
                        value +: {
                            px := Axis{label: "X" a11y: {label: "Test print pose X"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}
                            py := Axis{label: "Y" a11y: {label: "Test print pose Y"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}
                            pz := Axis{label: "Z" a11y: {label: "Test print pose Z"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 1 suffix: "°"}
                        }
                    }
                    pose_note := mod.widgets.FabLabelSmall{
                        visible: false
                        width: Fill text: "Laid flat: its own layers. Tests this orientation, not the print's."
                        margin: Inset{left: 10 right: 6 top: 2 bottom: 2}
                    }
                    adhesion := NamedRow{
                        label +: {text: "Adhesion"}
                        value +: {
                            FabTip{text: "A brim of its own round the piece's first layer, at the profile's brim width (Fab's 5 mm where the profile's brim takes none): for a tall piece on a small footing"
                                brim := mod.widgets.FabCheckBox{text: "Brim" a11y: {label: "Test print brim"}}
                            }
                        }
                    }

                // the overrides reaching into its box (the original's), and what its footing flags
                uses := mod.widgets.FabLabelSmall{width: Fill visible: false text: ""}
                flags := mod.widgets.FabLabel{width: Fill visible: false text: "" draw_text +: {color: fab.color_warning}}
            }
        }
        Empty := mod.widgets.FabLabelDim{
            width: Fill text: "No test prints: add a cube on a part to print just that piece of the plate."
            margin: Inset{left: 6 right: 6 top: 4 bottom: 4}
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

/// The clicked or active part on the original plate. In the pieces view, fall back
/// to the picked cube's part or the part held when the whole plate was put away.
pub fn target_part<'a>(state: &'a AppState, under: Option<&str>) -> Option<&'a str> {
    let original = |s: &str| {
        let object = s.split_once('#').map_or(s, |(base, _)| base);
        state.plate.whole_parts().iter().find(|p| p.object == object).map(|p| p.object.as_str())
    };
    under.and_then(original)
        .or_else(|| active_part(state).and_then(|i| original(&state.plate.parts[i].object)))
        .or_else(|| {
            state.plate.whole.as_ref()?;
            picked(state).and_then(|t| original(&t.object))
                .or_else(|| state.plate.whole_active.as_deref().and_then(original))
        })
}

/// Add a cube in the current view and open its sidebar.
pub fn add(cx: &mut Cx, state: &AppState, under: Option<String>) {
    if state.steps.shown.is_some() {
        cx.action(ShellAction::Notice { view: NoticeView::Plate, text: "Test print: return to the end of the part first (Back to the end, or Esc)".into() });
        return;
    }
    match target_part(state, under.as_deref()) {
        Some(object) => {
            if !state.ui.in_mode(PlateMode::Design) {
                cx.action(ShellAction::SetWorkspace(Workspace::Design));
            }
            cx.action(TestPrintAction::Add { object: object.to_owned() });
            cx.action(StageAction::TestsSidebar(true));
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

/// A test's disclosure header and its fields share the feature tree's pick.
#[derive(Script, ScriptHook, Widget)]
pub struct FabTestSection {
    #[deref] section: PropSection,
    #[rust] test_name: String,
    #[rust] selected: bool,
    #[rust] pos_in_set: u32,
    #[rust] set_size: u32,
    #[rust] ink: Option<(bool, bool)>,
}

impl FabTestSection {
    fn set_row(&mut self, name: String, selected: bool, pos_in_set: u32, set_size: u32) {
        self.test_name = name;
        self.selected = selected;
        self.pos_in_set = pos_in_set;
        self.set_size = set_size;
    }
}

impl Widget for FabTestSection {
    fn semantics(&self, cx: &Cx, node: &mut a11y_tree::Node) {
        self.section.semantics(cx, node);
        node.states.set(a11y_tree::States::SELECTABLE, true);
        node.states.set(a11y_tree::States::SELECTED, self.selected);
        node.pos_in_set = self.pos_in_set;
        node.set_size = self.set_size;
        node.actions.insert(a11y_tree::Actions::PRESS | a11y_tree::Actions::SELECT);
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{Actions, ActionResult};
        let open = match request.action {
            Actions::SELECT | Actions::EXPAND => true,
            Actions::COLLAPSE => false,
            Actions::PRESS => !(self.selected && self.section.is_open(cx)),
            _ => return self.section.handle_action(cx, request, scope),
        };
        self.section.set_is_open(cx, open, Animate::Yes);
        if open {
            cx.action(TestPrintAction::Pick { name: self.test_name.clone() });
        } else if self.selected {
            cx.action(ShellAction::PickOverride(None));
        }
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.section.handle_event(cx, event, scope);
    }

    fn visit_cancel(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) -> bool {
        self.section.visit_cancel(visit)
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let ink = (self.selected, self.section.hovered());
        if self.ink != Some(ink) {
            let header = self.section.header();
            if let Some(mut bg) = header.widget(cx, ids!(hdr)).borrow_mut::<View>() {
                bg.draw_bg.set_dyn_instance(cx, id!(selected), &[ink.0 as u8 as f32]);
            }
            if let Some(mut title) = header.label(cx, ids!(hdr.title)).borrow_mut() {
                title.draw_text.set_dyn_instance(cx, id!(hover), &[ink.1 as u8 as f32]);
            }
            self.ink = Some(ink);
        }
        self.section.draw_walk(cx, scope, walk)
    }
}

/// The test prints as sections, made from the `Test` template, one per test and kept. Each
/// section's fields are its own test's: an edit places that test, whichever is picked. Unfolding
/// a section picks its test; a test picked elsewhere (its tree row, the view) unfolds its section.
#[derive(Script, Widget)]
pub struct FabTestSections {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[layout] layout: Layout,
    #[rust] templates: HashMap<LiveId, ScriptObjectRef>,
    #[rust] items: HashMap<LiveId, WidgetRef>,
    /// The sections drawn last: each one's id and its test's name.
    #[rust] shown: Vec<(LiveId, String)>,
    /// Each section's fold as last seen (an opening one picks its test).
    #[rust] open: HashMap<LiveId, bool>,
    /// The name each section's name field was last given: typing in it is left alone till then.
    #[rust] names: HashMap<LiveId, String>,
    /// The test picked at the last draw.
    #[rust] picked: Option<String>,
    #[redraw] #[rust] area: Area,
}

impl ScriptHook for FabTestSections {
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
            self.names.clear();
            self.open.clear();
        }
    }
}

fn test_id(name: &str) -> LiveId {
    LiveId::from_str(&format!("test:{name}"))
}

impl FabTestSections {
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

    /// One section's fields and buttons: its test's edits, whichever test is picked.
    fn test_actions(cx: &mut Cx, w: &WidgetRef, t: &TestPrintView, state: &mut AppState, actions: &Actions) {
        if w.button(cx, ids!(delete)).clicked(actions) {
            cx.action(TestPrintAction::Delete { name: t.name.clone() });
            return;
        }
        if let Some(on) = w.check_box(cx, ids!(tick)).changed(actions) {
            cx.action(TestPrintAction::Tick { name: t.name.clone(), on });
        }
        if let Some(on) = w.check_box(cx, ids!(adhesion.value.brim)).changed(actions) {
            cx.action(TestPrintAction::Brim { name: t.name.clone(), on });
        }
        if w.button(cx, ids!(piece.value.lay_flat)).clicked(actions) {
            // Lay flat acts on the picked test: this one first
            cx.action(TestPrintAction::Pick { name: t.name.clone() });
            cx.action(StageAction::LayFlat(!state.ui.lay_flat));
        }
        // From the placement as it shows: a field committed a moment ago is held there until the
        // plate has it, so the next field's edit keeps it.
        let mut shown = state.ui.test_drag.clone().filter(|d| d.name == t.name).unwrap_or_else(|| TestDrag { name: t.name.clone(), center: t.center, size: t.size, turn: t.turn, pose: t.pose });
        let mut live = None;
        let mut done = None;
        if w.button(cx, ids!(piece.value.as_printed)).clicked(actions) {
            shown.pose = None;
            done = Some(shown.clone());
        }
        for (k, id) in [ids!(pose.value.px), ids!(pose.value.py), ids!(pose.value.pz)].into_iter().enumerate() {
            if let Some(x) = w.value_input(cx, id).ended(actions) {
                let mut pose = shown.pose.unwrap_or([0.0; 3]);
                pose[k] = x;
                shown.pose = Some(pose);
                done = Some(shown.clone());
            }
        }
        let rows: [(&[&[LiveId]; 3], fn(&mut TestDrag) -> &mut [f64; 3]); 3] = [
            (&[ids!(pos.value.x), ids!(pos.value.y), ids!(pos.value.z)], |d| &mut d.center),
            (&[ids!(size.value.sx), ids!(size.value.sy), ids!(size.value.sz)], |d| &mut d.size),
            (&[ids!(turn.value.tx), ids!(turn.value.ty), ids!(turn.value.tz)], |d| &mut d.turn),
        ];
        for (paths, of) in rows {
            for (k, id) in paths.iter().enumerate() {
                let f = w.value_input(cx, id);
                if let Some(x) = f.changed(actions) {
                    of(&mut shown)[k] = x;
                    live = Some(shown.clone());
                }
                if let Some(x) = f.ended(actions) {
                    of(&mut shown)[k] = x;
                    done = Some(shown.clone());
                }
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
        let input = w.text_input(cx, ids!(name_row.value.name));
        if input.returned(actions).is_some() || input.key_focus_lost(actions) {
            let to = input.text().trim().to_string();
            if !to.is_empty() && to != t.name {
                cx.action(TestPrintAction::Rename { name: t.name.clone(), to });
            }
        }
    }

    /// One section as its test stands (a drag's or a field's placement until its edit lands).
    fn sync(&mut self, cx: &mut Cx, id: LiveId, w: &WidgetRef, t: &TestPrintView, state: &AppState) {
        let t = match state.ui.test_drag.as_ref().filter(|d| d.name == t.name) {
            Some(d) => TestPrintView { center: d.center, size: d.size, turn: d.turn, pose: d.pose, ..t.clone() },
            None => t.clone(),
        };
        w.label(cx, ids!(hdr.title)).set_text(cx, &t.name);
        w.check_box(cx, ids!(tick)).set_active(cx, t.on, Animate::No);
        let figures = state.print.test_mode.then(|| state.print.preview.as_ref().filter(|_| !state.print.stale).map(|p| p.pieces.as_slice())).flatten();
        let on = state.plate.whole_parts().iter().find(|p| p.object == t.object).map_or_else(|| t.object.clone(), |p| p.name.clone());
        let whose = match figures.and_then(|f| f.iter().find(|p| p.name == t.name)) {
            Some(p) => format!("On {on}: {}, {:.1} g", format_time(p.seconds), p.grams),
            None => format!("On {on}"),
        };
        w.label(cx, ids!(whose)).set_text(cx, &whose);
        let (c, s, a) = (t.center, t.size, t.turn);
        for (path, v) in [(ids!(pos.value.x), c[0]), (ids!(pos.value.y), c[1]), (ids!(pos.value.z), c[2]),
            (ids!(size.value.sx), s[0]), (ids!(size.value.sy), s[1]), (ids!(size.value.sz), s[2]),
            (ids!(turn.value.tx), a[0]), (ids!(turn.value.ty), a[1]), (ids!(turn.value.tz), a[2])] {
            w.value_input(cx, path).set_value(cx, v);
        }
        let pose = t.pose.unwrap_or([0.0; 3]);
        for (path, v) in [(ids!(pose.value.px), pose[0]), (ids!(pose.value.py), pose[1]), (ids!(pose.value.pz), pose[2])] {
            w.value_input(cx, path).set_value(cx, v);
        }
        w.widget(cx, ids!(piece.value.as_printed)).set_disabled(cx, t.pose.is_none());
        w.widget(cx, ids!(pose_note)).set_visible(cx, t.pose.is_some());
        w.check_box(cx, ids!(adhesion.value.brim)).set_active(cx, t.brim, Animate::No);
        if self.names.get(&id).map(String::as_str) != Some(t.name.as_str()) {
            w.text_input(cx, ids!(name_row.value.name)).set_text(cx, &t.name);
            self.names.insert(id, t.name.clone());
        }
        let used: Vec<String> = uses(state, &t).into_iter().map(|u| u.text).collect();
        w.label(cx, ids!(uses)).set_visible(cx, !used.is_empty());
        w.label(cx, ids!(uses)).set_text(cx, &used.join("\n"));
        let flags = footing_flags(state, &t);
        w.label(cx, ids!(flags)).set_visible(cx, !flags.is_empty());
        w.label(cx, ids!(flags)).set_text(cx, &flags.join("\n"));
    }
}

impl Widget for FabTestSections {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Test prints");
        node.set_size = self.shown.len() as u32;
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Draw(_)) { return; }
        let shown = self.shown.clone();
        for (id, _) in &shown {
            if let Some(w) = self.items.get(id) {
                w.handle_event(cx, event, scope);
            }
        }
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        // Pointer and keyboard folds come from PropSection. The source pick drives the other folds.
        for (id, name) in &shown {
            let Some(w) = self.items.get(id) else { continue };
            let Some(section) = w.borrow::<FabTestSection>() else { continue };
            let open = section.section.is_open(cx);
            let was = self.open.insert(*id, open);
            if was == Some(!open) {
                if open && self.picked.as_deref() != Some(name) {
                    cx.action(TestPrintAction::Pick { name: name.clone() });
                } else if !open && self.picked.as_deref() == Some(name) {
                    cx.action(ShellAction::PickOverride(None));
                }
            }
        }
        let Event::Actions(actions) = event else { return };
        for (id, name) in &shown {
            let Some(w) = self.items.get(id).cloned() else { continue };
            let Some(t) = state.test_prints.iter().find(|t| t.name == *name).cloned() else { continue };
            Self::test_actions(cx, &w, &t, state, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.layout);
        self.shown.clear();
        if let Some(state) = scope.data.get::<AppState>() {
            let picked_name = picked(state).map(|t| t.name.as_str());
            let pick_changed = picked_name != self.picked.as_deref();
            if pick_changed { self.picked = picked_name.map(str::to_owned); }
            if state.test_prints.is_empty() {
                if let Some(w) = self.item(cx, live_id!(empty), live_id!(Empty)) {
                    w.draw_all(cx, &mut Scope::empty());
                }
            }
            let count = state.test_prints.len() as u32;
            for (pos, t) in state.test_prints.iter().enumerate() {
                let id = test_id(&t.name);
                let Some(w) = self.item(cx, id, live_id!(Test)) else { continue };
                if let Some(mut s) = w.borrow_mut::<FabTestSection>() {
                    let selected = picked_name == Some(t.name.as_str());
                    let new = !self.open.contains_key(&id);
                    if new || pick_changed {
                        s.section.set_is_open(cx, selected, if new { Animate::No } else { Animate::Yes });
                        self.open.insert(id, selected);
                    }
                    let position = pos as u32 + 1;
                    if new || pick_changed || s.pos_in_set != position || s.set_size != count {
                        s.set_row(t.name.clone(), selected, position, count);
                    }
                }
                self.sync(cx, id, &w, t, state);
                w.draw_all(cx, &mut Scope::empty());
                self.shown.push((id, t.name.clone()));
            }
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

/// What the test prints come to, next to the whole plate's, while their slice stands (the
/// sidebar's line over the tests).
pub fn totals(state: &AppState) -> Option<String> {
    totals_line(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{AreaKind, GhostBody, MarkedArea};
    use std::sync::Arc;

    fn cube() -> TestPrintView {
        TestPrintView { name: "Neck".into(), object: "figure/body".into(), center: [100.0, 100.0, 80.0], size: [40.0; 3], turn: [0.0; 3], pose: None, lay: None, on: true, brim: false }
    }

    fn pieces_view() -> AppState {
        let mut state = AppState::default();
        state.ui.enter(Workspace::Design);
        state.print.test_mode = true;
        let object = "block/block".to_owned();
        state.plate.whole = Some(Arc::new(vec![PlatePart { object: object.clone(), ..Default::default() }]));
        state.plate.parts = vec![PlatePart { object: format!("{object}#0"), ..Default::default() }];
        state.plate.whole_active = Some(object);
        state
    }

    #[test]
    fn fab_test_section_semantics_reports_selected_and_select_action_is_idempotent() {
        use a11y_tree::{ActionArg, ActionRequest, ActionResult, Actions, Node, NodeId, Role, States};
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut section = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, WindowFamilies::default(), &[makepad_widgets_fab::fab_mod]);
            crate::theme::script_mod(vm);
            crate::ui::script_mod_kit(vm);
            script_mod(vm);
            let value = vm.eval(script! { mod.widgets.FabTestSection{header +: {hdr +: {title +: {text: "Corner"}}}} });
            FabTestSection::script_from_value(vm, value)
        });
        let mut node = Node::default();
        for selected in [false, true] {
            section.set_row("Corner".into(), selected, 1, 2);
            section.section.set_is_open(&mut cx, selected, Animate::No);
            section.semantics(&cx, &mut node);
            assert_eq!((node.role, node.name.as_str()), (Role::DisclosureButton, "Corner"));
            assert_eq!((node.pos_in_set, node.set_size), (1, 2));
            assert!(node.states.contains(States::SELECTABLE));
            assert_eq!(node.states.contains(States::SELECTED), selected);
            assert_eq!(node.states.contains(States::EXPANDED), selected);
            assert_eq!(node.states.contains(States::COLLAPSED), !selected);
            assert!(node.actions.contains(Actions::PRESS | Actions::SELECT));
        }
        for action in [Actions::SELECT, Actions::SELECT, Actions::COLLAPSE] {
            let request = ActionRequest { req: 1, node: NodeId::NONE, action, arg: ActionArg::None };
            let emitted = cx.capture_actions(|cx| {
                assert_eq!(section.handle_action(cx, &request, &mut Scope::empty()), ActionResult::Handled);
            });
            if action == Actions::SELECT {
                assert!(section.section.is_open(&cx));
                assert!(emitted.iter().any(|a| matches!(a.downcast_ref::<TestPrintAction>(), Some(TestPrintAction::Pick { name }) if name == "Corner")));
            } else {
                assert!(!section.section.is_open(&cx));
                assert!(emitted.iter().any(|a| matches!(a.downcast_ref::<ShellAction>(), Some(ShellAction::PickOverride(None)))));
            }
        }
    }

    #[test]
    fn target_part_strips_piece_suffix_and_uses_held_whole_plate() {
        let mut state = pieces_view();
        assert_eq!(target_part(&state, None), Some("block/block"), "held whole-plate pick");
        state.plate.whole_active = None;
        assert_eq!(target_part(&state, Some("block/block#0")), Some("block/block"), "clicked piece");
        assert_eq!(target_part(&state, None), None, "nothing picked");
        let row = crate::ui::code::FeatureRow::test_print("Test 1", "block/block");
        state.source.selected = Some(row.id.clone());
        state.source.features.push(row);
        state.test_prints.push(TestPrintView { name: "Test 1".into(), object: "block/block".into(), ..Default::default() });
        assert_eq!(target_part(&state, None), Some("block/block"), "picked test");
    }

    #[test]
    fn add_emits_no_mode_switch_and_never_names_retired_radio() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut state = pieces_view();
        for tests in [true, false] {
            if !tests {
                state.print.test_mode = false;
                state.plate.parts = Arc::unwrap_or_clone(state.plate.whole.take().unwrap());
                state.plate.whole_active = None;
            }
            let under = (!tests).then(|| "block/block".to_string());
            let actions = cx.capture_actions(|cx| add(cx, &state, under));
            assert_eq!(actions.len(), 2, "Add and open the sidebar are the only actions");
            assert_eq!(actions[0].downcast_ref::<TestPrintAction>(), Some(&TestPrintAction::Add { object: "block/block".into() }));
            assert!(matches!(actions[1].downcast_ref::<StageAction>(), Some(StageAction::TestsSidebar(true))));
        }
        state.steps.shown = Some(0);
        let actions = cx.capture_actions(|cx| add(cx, &state, Some("block/block".into())));
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0].downcast_ref::<ShellAction>(), Some(ShellAction::Notice { text, .. })
            if text.contains("Back to the end") && !text.contains("Whole plate")));
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
