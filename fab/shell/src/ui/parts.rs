//! The user's parts: [`FabPartsPanel`] lists every placed part of the plate
//! (never the printer's own geometry) with its filament colour, size and a
//! warning when it cannot print; [`FabPartPanel`] edits the selected part:
//! position, turn, scale, its parameters, print colour, and the plate tools
//! (drop to the plate, frame it).
//!
//! Parts and annotations edit through [`PartAction`]s and [`MeasureAction`]s;
//! the app makes each edit one undoable model transaction.

use crate::ui::code::TreeFold;
use crate::api::*;
use crate::ui::keymap::{self, ShortcutId as Key};
use crate::model::state::{part_axes, pose_stretch_about, pose_turn_shift, Pose};
use crate::print::colour_map::{FabColorLink, PartPick};
use crate::print::trays::DrawFabChip;
use crate::tools::transform::{mm_to_world, shown_part};
use crate::ui::popover::{dropdown_clicked, menu_picked, open_menu, MenuItem, MenuPlace};
use std::collections::{HashMap, HashSet};
use makepad_widgets_core::*;
use makepad_widgets_core::widget_tree::CxWidgetExt;

const ADD_MENU: LiveId = live_id!(fab_parts_add);
const PART_MENU: LiveId = live_id!(fab_parts_context);
const ORIGIN_FIELD_MAX: f64 = 400.0;
/// How far a row of the parts tree sits in per level.
const TREE_STEP: f64 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Part,
    /// An object whose bodies print as one: its bodies are the part rows under it.
    Object,
    Script,
    AnnotationGroup,
    Measurement(usize),
}

/// One row of the expanded parts tree. Parts keep their model identity;
/// measurements use the document indices accepted by `MeasureAction`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub key: u64,
    pub parent: Option<u64>,
    pub kind: RowKind,
    pub name: String,
    /// Annotation value and kind; part details are derived from the live plate.
    pub detail: String,
    pub parts: Vec<usize>,
    /// The base instance's first part for a script node, or the child itself.
    /// Unused by annotation rows.
    pub rename: usize,
    /// Zero for a part row; the number of linked placements for a script node.
    pub instances: usize,
    pub pos_in_set: u32,
    pub set_size: u32,
}

/// Annotation groups share the tree's layout, collapse state and row template.
/// Titles identify groups; callers supply the child identities and contents.
pub struct AnnotationGroup {
    pub title: String,
    pub rows: Vec<Row>,
}

#[derive(Clone, Copy)]
enum RowCommand {
    Select,
    Delete,
}

impl Row {
    fn is_branch(&self) -> bool {
        matches!(self.kind, RowKind::Object | RowKind::Script | RowKind::AnnotationGroup)
    }

    fn is_part(&self) -> bool {
        matches!(self.kind, RowKind::Part | RowKind::Object | RowKind::Script)
    }

    fn excluded(&self, plate: &PlateInfo) -> bool {
        self.is_part() && !self.parts.iter().any(|i| plate.parts.get(*i).is_some_and(|p| p.printed || p.insert || p.is_board()))
    }

    /// A row of boards only: shown for reference, never printed; its eye shows or hides them.
    fn is_board(&self, plate: &PlateInfo) -> bool {
        self.is_part() && !self.parts.is_empty() && self.parts.iter().all(|i| plate.parts.get(*i).is_some_and(|p| p.is_board()))
    }

    fn eye_state(&self, plate: &PlateInfo) -> Option<bool> {
        if !self.is_part() { return None; }
        if self.is_board(plate) {
            return Some(self.parts.iter().filter_map(|i| plate.parts.get(*i)).any(|p| !p.hidden));
        }
        let mut parts = self.parts.iter().filter_map(|i| plate.parts.get(*i)).filter(|p| !p.insert && !p.is_board());
        let first = parts.next()?;
        Some(first.printed || parts.any(|p| p.printed))
    }

    /// The eye edits the placements that print (not inserts, not boards); a mixed script row
    /// switches those all off. A board row's shows or hides its boards (they never print).
    fn eye_action(&self, plate: &PlateInfo) -> Option<PartAction> {
        let on = self.eye_state(plate)?;
        let board = self.is_board(plate);
        let objects = self.parts.iter().filter_map(|i| plate.parts.get(*i)).filter(|p| !p.insert && p.is_board() == board).map(|p| p.object.clone()).collect();
        Some(if board { PartAction::SetHidden { objects, hidden: on } } else { PartAction::SetPrint { objects, print: !on } })
    }

    fn detail_text(&self, plate: &PlateInfo) -> String {
        let p = &plate.parts[self.rename];
        if self.kind == RowKind::Part && p.insert {
            // O(parts) host lookup per insert row when drawing refreshes the cached details;
            // the list already scans its parts per draw.
            let host = plate.parts.iter().find(|host| host.object == p.insert_into);
            let host = host.map_or(p.insert_into.as_str(), |host| host.name.as_str());
            // Printed into its host, the print pausing to place it; melted into it after the
            // print (a heat-set insert); else it only rides there.
            return if p.embed {
                format!("Printed into {host}")
            } else if !p.melt.is_empty() {
                format!("{} insert, melted into {host} after the print", p.melt)
            } else {
                format!("Rides in {host}")
            };
        }
        if self.kind == RowKind::Object { bodies_text(&plate.parts, &self.parts) }
        else if self.instances > 0 { format!("{} linked instances · {} parts", self.instances, self.parts.len()) }
        else if p.is_board() { format!("Board, not printed · {}", size_text(p)) }
        else { size_text(p) }
    }

    fn measurement_action(&self, command: RowCommand) -> Option<MeasureAction> {
        let RowKind::Measurement(index) = self.kind else { return None };
        Some(match command {
            RowCommand::Select => MeasureAction::Select(index),
            RowCommand::Delete => MeasureAction::Delete(index),
        })
    }

    fn annotation_selected(&self, state: &AppState) -> bool {
        matches!(self.kind, RowKind::Measurement(index) if state.selected_measure == Some(index))
    }

    fn rename_label(&self) -> &'static str {
        match self.kind {
            RowKind::Measurement(_) => "Rename measurement",
            RowKind::Object => "Rename object",
            _ => "Rename part",
        }
    }
}

/// The row `key` names in `tree`.
fn row_of(tree: &[Row], key: u64) -> Option<&Row> {
    tree.iter().find(|r| r.key == key)
}

/// How many rows `row` stands under: 0 at the root.
fn depth(tree: &[Row], row: &Row) -> u32 {
    std::iter::successors(row.parent, |&k| row_of(tree, k).and_then(|r| r.parent)).count() as u32
}

/// Whether a row `row` stands under is folded: it is not shown.
fn folded(tree: &[Row], collapsed: &HashSet<u64>, row: &Row) -> bool {
    std::iter::successors(row.parent, |&k| row_of(tree, k).and_then(|r| r.parent)).any(|k| collapsed.contains(&k))
}

/// A linked placement's row: its script's, or one right under it.
fn linked(tree: &[Row], row: &Row) -> bool {
    row.kind == RowKind::Script || row.parent.and_then(|k| row_of(tree, k)).is_some_and(|p| p.kind == RowKind::Script)
}

fn measurement_name(measurement: &Measurement) -> &str {
    measurement.name.as_deref().filter(|name| !name.trim().is_empty()).unwrap_or(&measurement.label)
}

fn measurement_group(measurements: &[Measurement]) -> AnnotationGroup {
    AnnotationGroup {
        title: "Measurements".into(),
        rows: measurements.iter().enumerate().map(|(index, measurement)| Row {
            key: LiveId::from_str(&format!("measurement:{index}")).0,
            parent: None,
            kind: RowKind::Measurement(index),
            name: measurement_name(measurement).into(),
            detail: format!("{} · {}", crate::tools::measure::kind_label(measurement.kind), measurement.label),
            parts: Vec::new(),
            rename: 0,
            instances: 0,
            pos_in_set: 0,
            set_size: 0,
        }).collect(),
    }
}

/// Scripts and their children retain first appearance order. Multiple objects from
/// one placement stay flat; only distinct linked placements introduce a script node.
/// Nonempty annotation groups follow all parts, in the supplied order.
pub fn tree_rows(plate: &PlateInfo, annotations: &[AnnotationGroup]) -> Vec<Row> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut scripts = HashMap::new();
    // in their declared order: the plate lists its printed parts first, so a part switched off
    // would otherwise drop to the end (ties, a placed object's several bodies, as listed)
    let mut order: Vec<usize> = (0..plate.parts.len()).collect();
    order.sort_by_key(|&i| (plate.parts[i].order, i));
    for (i, p) in order.into_iter().map(|i| (i, &plate.parts[i])) {
        let script = if p.script.is_empty() { p.object.split('/').next().unwrap_or(&p.object) } else { &p.script };
        let group = *scripts.entry(script).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[group].push(i);
    }
    let mut rows = Vec::new();
    for parts in groups {
        let first = &plate.parts[parts[0]];
        let instances = parts.iter().map(|i| plate.parts[*i].link).collect::<HashSet<_>>().len();
        let parent = if instances > 1 {
            let key = LiveId::from_str(&format!("script:{}", first.script)).0;
            let file = first.file.rsplit(['/', '\\']).next().unwrap_or_default();
            let stem = file.strip_suffix(".cad.splash").unwrap_or_else(|| file.rsplit_once('.').map_or(file, |(stem, _)| stem));
            let rename = parts.iter().copied().find(|i| plate.parts[*i].link == 0).unwrap_or(parts[0]);
            rows.push(Row { key, parent: None, kind: RowKind::Script, name: if stem.is_empty() { first.name.clone() } else { stem.into() }, detail: String::new(), parts: parts.clone(), rename, instances, pos_in_set: 0, set_size: 0 });
            Some(key)
        } else { None };
        // An object whose bodies print as one stands where its first body is, its bodies under it.
        let mut items: Vec<(Option<&(String, String)>, Vec<usize>)> = Vec::new();
        for i in parts {
            match plate.parts[i].body_of.as_ref() {
                Some(of) => match items.iter_mut().find(|(o, _)| o.is_some_and(|o| o.0 == of.0)) {
                    Some(item) => item.1.push(i),
                    None => items.push((Some(of), vec![i])),
                },
                None => items.push((None, vec![i])),
            }
        }
        let part_row = |i: usize, parent: Option<u64>, pos: usize, count: u32| {
            let p = &plate.parts[i];
            Row { key: LiveId::from_str(&format!("part:{}", p.object)).0, parent, kind: RowKind::Part, name: p.name.clone(), detail: String::new(), parts: vec![i], rename: i, instances: 0, pos_in_set: pos as u32 + 1, set_size: count }
        };
        let count = items.len() as u32;
        for (pos, (of, bodies)) in items.into_iter().enumerate() {
            match of {
                Some((id, name)) => {
                    let key = LiveId::from_str(&format!("object:{id}")).0;
                    rows.push(Row { key, parent, kind: RowKind::Object, name: name.clone(), detail: String::new(), parts: bodies.clone(), rename: bodies[0], instances: 0, pos_in_set: pos as u32 + 1, set_size: count });
                    let n = bodies.len() as u32;
                    rows.extend(bodies.into_iter().enumerate().map(|(k, i)| part_row(i, Some(key), k, n)));
                }
                None => rows.push(part_row(bodies[0], parent, pos, count)),
            }
        }
    }
    for group in annotations.iter().filter(|group| !group.rows.is_empty()) {
        let key = LiveId::from_str(&format!("annotation:{}", group.title)).0;
        let count = group.rows.len() as u32;
        rows.push(Row { key, parent: None, kind: RowKind::AnnotationGroup, name: group.title.clone(), detail: format!("{count} item{}", if count == 1 { "" } else { "s" }), parts: Vec::new(), rename: 0, instances: 0, pos_in_set: 0, set_size: 0 });
        for (pos, child) in group.rows.iter().enumerate() {
            let mut child = child.clone();
            child.parent = Some(key);
            child.pos_in_set = pos as u32 + 1;
            child.set_size = count;
            rows.push(child);
        }
    }
    let roots = rows.iter().filter(|r| r.parent.is_none()).count() as u32;
    let mut pos = 0;
    for row in &mut rows {
        if row.parent.is_none() {
            pos += 1;
            row.pos_in_set = pos;
            row.set_size = roots;
        }
    }
    rows
}

pub(crate) fn duplicate_menu(has: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::new(live_id!(part_duplicate_linked), "Duplicate (linked)").key(&keymap::shortcut(Key::DuplicateLinked)).enabled_if(has),
        MenuItem::new(live_id!(part_duplicate_copy), "Duplicate as copy").key(&keymap::shortcut(Key::DuplicateCopy)).enabled_if(has),
    ]
}

/// The "Material" flyout: the printer's trays by nozzle; a pick prints the whole part from
/// that tray (in the print colour mapped there, or a new one).
pub(crate) fn material_menu(state: &AppState, objects: &[String]) -> MenuItem {
    let m = &state.print.colors;
    let current = {
        let mut pos = objects.iter().filter_map(|o| state.plate.parts.iter().find(|p| p.object == *o)).map(|p| m.row(&p.print_color).and_then(|r| r.pos));
        let first = pos.next().flatten();
        if pos.all(|p| p == first) { first } else { None }
    };
    let has = !objects.is_empty() && !state.slice_blocks_edits() && !state.ui.in_mode(PlateMode::Mechanics);
    MenuItem::new(live_id!(part_material), "Material").flyout(crate::print::colours::tray_menu(m, current)).enabled_if(has)
}

pub(crate) fn part_menu(state: &AppState, objects: &[String], has: bool) -> Vec<MenuItem> {
    let mut items = vec![material_menu(state, objects), MenuItem::sep()];
    // Inserts only: printed into their part, the print pausing to place them, or riding in it.
    // A heat-set insert melts in after the print: it has no such choice.
    let inserts: Vec<&PlatePart> = objects.iter().filter_map(|o| state.plate.parts.iter().find(|p| p.object == *o)).collect();
    if !inserts.is_empty() && inserts.iter().all(|p| p.insert && p.melt.is_empty()) {
        let on = inserts.iter().all(|p| p.embed);
        items.push(MenuItem::new(live_id!(part_embed), "Print into its part (pause to place)").checked(on).enabled_if(has && !state.slice_blocks_edits()));
        items.push(MenuItem::sep());
    }
    items.extend(duplicate_menu(has));
    items.extend([
        MenuItem::new(live_id!(part_delete), "Delete").key(&keymap::shortcut(Key::Delete)).enabled_if(has),
        MenuItem::new(live_id!(part_delete_script), "Delete script (all linked instances)").enabled_if(has),
    ]);
    items
}

/// Print these parts (or one part's painted material) from a tray: the edit that says so.
pub(crate) fn assign_tray(state: &AppState, objects: Vec<String>, pos: crate::print::colours::Position, paint: Vec<[f32; 3]>) -> PartAction {
    let m = &state.print.colors;
    // The colour already printing from there (a used one first); none: a new one.
    let color = m.rows.iter().filter(|r| r.pos == Some(pos)).max_by_key(|r| r.used).map(|r| r.name.clone());
    PartAction::AssignTray { objects, color, printer: m.layout.printer.clone(), side: crate::print::colours::head_word(&m.layout.heads, pos.head), tray: pos.tray_text(), paint }
}

/// Clone/copy each selected placement once, even when a script emits several parts.
pub fn duplicate_selected(cx: &mut Cx, state: &AppState, copy: bool) {
    let objects = selected_parts(state).into_iter().map(|i| state.plate.parts[i].object.clone()).collect::<Vec<_>>();
    part_menu_action(cx, state, &objects, if copy { live_id!(part_duplicate_copy) } else { live_id!(part_duplicate_linked) });
}

/// One selection, one action and one undo step; the worker resolves and orders placements.
pub fn delete_selected(cx: &mut Cx, state: &AppState) {
    let objects = selected_parts(state).into_iter().map(|i| state.plate.parts[i].object.clone()).collect::<Vec<_>>();
    part_menu_action(cx, state, &objects, live_id!(part_delete));
}

pub(crate) fn part_menu_action(cx: &mut Cx, state: &AppState, objects: &[String], pick: LiveId) -> bool {
    if let Some(pos) = crate::print::colours::tray_pick(&state.print.colors, pick) {
        if !objects.is_empty() && !state.slice_blocks_edits() && !state.ui.in_mode(PlateMode::Mechanics) {
            cx.action(assign_tray(state, objects.to_vec(), pos, Vec::new()));
        }
        return true;
    }
    if pick == live_id!(part_embed) {
        let inserts: Vec<&PlatePart> = objects.iter().filter_map(|o| state.plate.parts.iter().find(|p| p.object == *o && p.insert && p.melt.is_empty())).collect();
        if !inserts.is_empty() && !state.slice_blocks_edits() {
            let embed = !inserts.iter().all(|p| p.embed);
            cx.action(PartAction::SetEmbed { objects: inserts.iter().map(|p| p.object.clone()).collect(), embed });
        }
        return true;
    }
    let linked = pick == live_id!(part_duplicate_linked);
    let copy = pick == live_id!(part_duplicate_copy);
    let delete = pick == live_id!(part_delete);
    let script = pick == live_id!(part_delete_script);
    if !(linked || copy || delete || script) { return false; }
    if state.ui.in_mode(PlateMode::Mechanics) { return true; }
    // Deletion stays available in a slice; the app explains a read-only Monitor view.
    if delete || script {
        cx.action(if script { PartAction::DeleteScripts { objects: objects.to_vec() } }
            else { PartAction::DeleteParts { objects: objects.to_vec() } });
        return true;
    }
    if state.slice_blocks_edits() { return true; }
    let by_object: HashMap<_, _> = state.plate.parts.iter().map(|p| (p.object.as_str(), p)).collect();
    let targets: Vec<_> = objects.iter().filter_map(|object| by_object.get(object.as_str()).copied()).collect();
    let mut seen = HashSet::new();
    for part in targets {
        let object = &part.object;
        let instance = object.split('/').next().unwrap_or(object);
        if !seen.insert(instance) { continue; }
        let object = object.clone();
        cx.action(if copy { PartAction::DuplicateCopy { object } } else { PartAction::DuplicateLinked { object } });
    }
    true
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // List and tool glyphs in the quiet ink, the full ink under the pointer.
    let ToolIcon = mod.widgets.FabIconButton{
        width: 24
        height: 22
        draw_icon +: {color: fab.color_text_dim}
    }

    let EyeIcon = ToolIcon{}

    // A row's fold chevron, the feature tree's: right while folded, down while open. The whole
    // 18 x 22 slot takes the press; only the small chevron draws (no face on hover or press).
    let FoldIcon = mod.widgets.FabIconButton{
        width: 18
        height: 22
        icon_walk: Walk{width: 10 height: 10}
        draw_bg +: {pixel: fn() { return vec4(0.0) }}
        draw_icon +: {color: fab.color_text_dim}
    }

    // A Transform number row: its label column the Parameters rows' (`param_panel::LABEL_W`), so
    // the Part panel's fields all start at one line.
    let TransformRow = PropNumberRow{
        label +: {width: #(crate::ui::param_panel::LABEL_W)}
    }

    mod.widgets.FabPartsPanelBase = #(FabPartsPanel::register_widget(vm))
    mod.widgets.FabPartsPanel = set_type_default() do mod.widgets.FabPartsPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        draw_shade: mod.draw.FabScrollShade{}
        // the toolbar: the area's editor selector, then the filter
        tools := mod.widgets.FabEditorBar{
            // narrows the list to the parts whose names hold every word typed (Add, Feature and
            // Shape stand in the 3D view's bar)
            filter := mod.widgets.FabSearch{
                width: Fill{min: 60}
                row +: { input +: { empty_text: "Filter" a11y: {label: "Filter parts"} } }
            }
        }
        mod.widgets.FabHr{}
        empty := View{
            width: Fill
            height: Fit
            padding: Inset{left: 14 right: 14 top: 14 bottom: 14}
            visible: false
            mod.widgets.FabLabelDim{ width: Fill text: "No parts yet. Add a shape, drop an STL, 3MF or glTF file on the window, or ask the AI to design one." }
        }
        list := PortalList{
            width: Fill
            height: Fill
            flow: Down
            auto_tail: false
            // A press picks a row; the list scrolls with the wheel and its bar, never by a drag.
            drag_scrolling: false
            // An open part's boolean tree, right under its row (the outliner's features).
            Features := mod.widgets.FabFeatureTree{ margin: Inset{right: 2} }
            Row := View{
                width: Fill
                height: 34
                flow: Right
                align: Align{x: 0.0 y: 0.5}
                padding: Inset{left: 8 right: 6 top: 0 bottom: 0}
                spacing: 6
                cursor: MouseCursor.Hand
                show_bg: true
                draw_bg +: {
                    selected: instance(0.0)
                    hover: instance(0.0)
                    // selection as the feature rows show it: a rounded box inset in the row (hover
                    // lights the name instead, `text.title.name`'s `hot`)
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                        sdf.fill(fab.color_editor)
                        sdf.box(4.0, 1.0, self.rect_size.x - 8.0, self.rect_size.y - 2.0, 3.0)
                        sdf.fill(vec4(fab.color_selection_bg.xyz, 0.0).mix(fab.color_selection_bg, self.selected))
                        return sdf.result
                    }
                }
                animator: Animator{
                    hover: {
                        default: @off
                        off: AnimatorState{
                            from: {all: Forward {duration: fab.anim_fast}}
                            apply: { draw_bg: {hover: 0.0} text: {title: {name: {draw_text: {hot: 0.0}}}} }
                        }
                        on: AnimatorState{
                            from: {all: Snap}
                            apply: { draw_bg: {hover: 1.0} text: {title: {name: {draw_text: {hot: 1.0}}}} }
                        }
                    }
                }
                // The fold's slot is kept on every row, so the swatches line up.
                fold := View{
                    width: 18 height: Fill flow: Overlay align: Align{x: 0.5 y: 0.5}
                    branch := FoldIcon{ visible: false a11y: {label: "Expand"} draw_icon +: {svg: crate_resource("self://resources/icons/chevron_right.svg")} }
                    branch_open := FoldIcon{ visible: false a11y: {label: "Collapse"} draw_icon +: {svg: crate_resource("self://resources/icons/chevron_down.svg")} }
                }
                linked := View{
                    width: 14 height: 14 visible: false
                    FabIconSmall{ icon_walk: Walk{width: 14 height: 14} draw_icon +: {svg: crate_resource("self://resources/icons/link.svg")} }
                }
                text := View{
                    width: Fill
                    height: Fit
                    flow: Down
                    spacing: 2
                    // The name and its rename field share one line of fixed height, the field's
                    // text where the name's was, so renaming moves nothing in the row.
                    title := View{
                        width: Fill
                        height: 16
                        flow: Overlay
                        align: Align{y: 0.5}
                        // a part's name in bold: the parts stand apart from the features under them
                        name := mod.widgets.FabLabel{ width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis
                            draw_text +: { text_style: theme.font_bold{font_size: fab.font_size_ui} row_alpha: instance(1.0) hot: instance(0.0) get_color: fn() { return vec4(mix(self.color.rgb, theme.color_text_hover.rgb, self.hot), self.color.a * self.row_alpha) } }
                        }
                        rename := mod.widgets.FabInput{
                            width: Fill height: Fill visible: false
                            margin: Inset{left: -4}
                            padding: Inset{left: 4 right: 4 top: 0 bottom: 0}
                        }
                    }
                    // the dimensions, quieter than the name
                    size := mod.widgets.FabLabelSmall{ width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis
                        draw_text +: { color: fab.color_text_muted row_alpha: instance(1.0) get_color: fn() { return vec4(self.color.rgb, self.color.a * self.row_alpha) } }
                    }
                }
                // The engine that builds the part (Exact, Field, Mesh) as a pill; its tip says what
                // that engine is good at (`PartEngine::tip`).
                engine_tip := FabTip{
                    engine := View{
                        width: Fit
                        height: 18
                        padding: Inset{left: 5 right: 5}
                        align: Align{x: 0.5 y: 0.5}
                        cursor: MouseCursor.Hand
                        show_bg: true
                        draw_bg +: {
                            pixel: fn() {
                                let dpi = self.draw_pass.dpi_factor
                                let lo = floor(self.rect_pos * dpi + 0.5) / dpi - self.rect_pos
                                let hi = floor((self.rect_pos + self.rect_size) * dpi + 0.5) / dpi - self.rect_pos
                                let sdf = Sdf2d.viewport_px(self.pos * self.rect_size, dpi)
                                sdf.box(lo.x, lo.y, hi.x - lo.x, hi.y - lo.y, fab.radius)
                                sdf.fill(theme.prop_field)
                                return sdf.result
                            }
                        }
                        label := mod.widgets.FabLabelSmall{ text: "" draw_text +: { color: fab.color_text_muted } }
                    }
                }
                // The part's colour, its edges on device pixels, with its nozzle's letter, left of the eye;
                // its tip says which nozzle and tray that is (`RowDisplay::tip`).
                swatch_tip := FabTip{
                    swatch := View{
                        width: 18
                        height: 18
                        // (a cursor of its own claims the pointer, which the tip around it needs)
                        cursor: MouseCursor.Hand
                        show_bg: true
                        draw_bg +: {
                            color: instance(vec4(0.8, 0.5, 0.2, 1.0))
                            pixel: fn() {
                                let dpi = self.draw_pass.dpi_factor
                                let lo = floor(self.rect_pos * dpi + 0.5) / dpi - self.rect_pos
                                let hi = floor((self.rect_pos + self.rect_size) * dpi + 0.5) / dpi - self.rect_pos
                                let sdf = Sdf2d.viewport_px(self.pos * self.rect_size, dpi)
                                sdf.box(lo.x, lo.y, hi.x - lo.x, hi.y - lo.y, fab.radius)
                                sdf.fill(self.color)
                                return sdf.result
                            }
                        }
                        align: Align{x: 0.5 y: 0.5}
                        slot := mod.widgets.FabLabelSmall{ text: "1" draw_text +: { color: #xffffffd0 } }
                    }
                }
                eye := EyeIcon{
                    a11y: {label: "Hide part"}
                    draw_icon +: {svg: crate_resource("self://resources/icons/eye.svg")}
                }
                eye_off := EyeIcon{
                    visible: false
                    a11y: {label: "Show part"}
                    draw_icon +: {svg: crate_resource("self://resources/icons/eye_off.svg")}
                }
                warn := View{
                    visible: false
                    width: 14
                    height: 14
                    FabIconSmall{
                        icon_walk: Walk{ width: 14 height: 14 }
                        draw_icon +: { color: fab.color_warning svg: crate_resource("self://resources/icons/warning.svg") }
                    }
                }
            }
        }
        footer := View{
            width: Fill
            height: Fit
            padding: Inset{left: 10 right: 10 top: 6 bottom: 8}
            totals := mod.widgets.FabLabelSmall{ width: Fill text: "" }
        }
    }

    mod.widgets.FabColorRowsBase = #(FabColorRows::register_widget(vm))
    mod.widgets.FabColorRows = set_type_default() do mod.widgets.FabColorRowsBase{
        width: Fill height: Fit flow: Down spacing: 2
        Row := mod.widgets.FabColorLink{}
    }

    mod.widgets.FabPaintPaletteBase = #(FabPaintPalette::register_widget(vm))
    mod.widgets.FabPaintPalette = set_type_default() do mod.widgets.FabPaintPaletteBase{
        width: Fill height: Fit
        color_error: fab.color_error
        draw_chip: mod.draw.DrawFabChip{ radius: 3.0 }
        draw_text: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small - 2.0 }
        }
    }

    mod.widgets.FabBrushDialBase = #(FabBrushDial::register_widget(vm))
    mod.widgets.FabBrushDial = set_type_default() do mod.widgets.FabBrushDialBase{
        width: Fill height: 44
        draw_chip: mod.draw.DrawFabChip{}
        draw_text: mod.draw.DrawText{
            color: fab.color_text_muted
            text_style: theme.font_regular{ font_size: fab.font_size_small - 1.0 }
        }
    }

    mod.widgets.FabPatternDialBase = #(FabPatternDial::register_widget(vm))
    mod.widgets.FabPatternDial = set_type_default() do mod.widgets.FabPatternDialBase{
        width: Fill height: 44
        draw_chip: mod.draw.DrawFabChip{}
        draw_text: mod.draw.DrawText{
            color: fab.color_text_muted
            text_style: theme.font_regular{ font_size: fab.font_size_small - 1.0 }
        }
    }

    mod.widgets.FabPartPanelBase = #(FabPartPanel::register_widget(vm))
    mod.widgets.FabPartPanel = set_type_default() do mod.widgets.FabPartPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        // Nothing selected: the plate at a glance.
        // the toolbar: the area's editor selector (and this editor's tools, when it has any); as
        // the Plate view's sidebar (`stage`'s `part_side`): its title, the filter and its close
        editor_bar := mod.widgets.FabEditorBar{
            title := mod.widgets.FabLabel{ visible: false text: "Part" draw_text +: { text_style: theme.font_bold{font_size: fab.font_size_ui} } }
            // filters the rows below by name (a section's title shows it whole)
            search := mod.widgets.FabSearch{width: Fill{min: 80} row +: {input +: {empty_text: "Filter"}}}
            side_close := ToolIcon{ visible: false a11y: {label: "Close part"} draw_icon +: { svg: crate_resource("self://resources/icons/close.svg") } }
        }
        none := PropSection{
            // (one section alone: nothing to move it among)
            grip: false
            margin: Inset{left: theme.prop_pad right: theme.prop_pad bottom: theme.prop_section_gap}
            header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/grid.svg")}} title +: {text: "Plate"}}}
            body +: {
                plate_summary := mod.widgets.FabLabel{width: Fill text: "" a11y: {label: "Plate summary"}}
                mod.widgets.FabLabelDim{width: Fill text: "Select a part on the plate or in the list to edit it."}
            }
        }
        body := mod.widgets.FabScroll{
            visible: false
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: #(crate::ui::widgets::PANEL_TOP)}
            // The sections draw in lists of their own: a section records again only when what
            // it shows changed (its setters, or the panel's key of `rest`). The part's name and
            // size are the parts list's (rename there).
            rest := View{width: Fill height: Fit flow: Down new_batch: true
                // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
                transform_slot := View{width: Fill height: Fit
                    transform := PropSection{
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/move.svg")}} title +: {text: "Transform"}}}
                        body +: {
                            // what Position measures from
                            anchor_row := TransformRow{
                                label +: {text: "Anchor"}
                                value +: {num +: {visible: false} anchor := PropDropDown{width: Fill labels: ["Origin" "Centre" "Corner"] a11y: {label: "Position anchor"}}}
                            }
                            // X and Y side by side, each its axis inside its field
                            xy := TransformRow{
                                margin: Inset{top: theme.prop_row_gap}
                                label +: {text: "Position"}
                                value +: {
                                    flow: Right spacing: 4
                                    num +: {a11y: {label: "X"} label: "X" min: -50.0 max: #(ORIGIN_FIELD_MAX) step: 0.5 snap: 5.0 precision: 1 suffix: " mm"}
                                    num_y := ValueInput{width: Fill height: Fill min_height: 0 a11y: {label: "Y"} label: "Y" min: -50.0 max: #(ORIGIN_FIELD_MAX) step: 0.5 snap: 5.0 precision: 1 suffix: " mm"}
                                }
                            }
                            rs := View{width: Fill height: Fit flow: Down spacing: theme.prop_row_gap margin: Inset{top: theme.prop_row_gap}
                                rot := TransformRow{
                                    label +: {text: "Turn"}
                                    value +: {num +: {a11y: {label: "Turn"} min: -360.0 max: 360.0 step: 1.0 snap: 15.0 precision: 0 suffix: "°"}}
                                }
                                // X, Y and Z side by side, the proportions lock after them
                                scale := TransformRow{
                                    label +: {text: "Scale"}
                                    value +: {
                                        flow: Right spacing: 4
                                        num +: {a11y: {label: "Scale X"} label: "X" min: 1.0 max: 1000.0 step: 1.0 snap: 10.0 precision: 1 suffix: " %"}
                                        num_y := ValueInput{width: Fill height: Fill min_height: 0 a11y: {label: "Scale Y"} label: "Y" min: 1.0 max: 1000.0 step: 1.0 snap: 10.0 precision: 1 suffix: " %"}
                                        num_z := ValueInput{width: Fill height: Fill min_height: 0 a11y: {label: "Scale Z"} label: "Z" min: 1.0 max: 1000.0 step: 1.0 snap: 10.0 precision: 1 suffix: " %"}
                                    }
                                    trail +: {width: 30 padding: Inset{left: 4} lock := mod.widgets.FabIconCheck{ a11y: {label: "Keep proportions"} width: Fill height: Fill padding: Inset{left: 4 right: 4 top: 3 bottom: 3} draw_icon +: { svg: crate_resource("self://resources/icons/link.svg") } }}
                                }
                            }
                            // The part's actions as a light strip (duplicate and delete are the parts list's menu;
                            // Drop to plate and Frame selected are the 3D view's toolbar's).
                            buttons := View{
                                width: Fill height: Fit flow: Right spacing: 2 margin: Inset{top: 4}
                                align: Align{x: 0.0 y: 0.5}
                                FabTip{text: "Make printable"
                                    make_printable := mod.widgets.FabActionButton{
                                        visible: false
                                        a11y: {label: "Make printable"}
                                        draw_icon +: {svg: crate_resource("self://resources/icons/cube.svg")}
                                    }
                                }
                            }
                        }
                    }
                }
                // The part's shape parameters, then each feature's fields, in groups that fold
                // ([`crate::ui::param_panel::FabParamPanel`], which was the Parameters editor's).
                // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
                params_slot := View{width: Fill height: Fit
                    params := PropSection{
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/shape.svg")}} title +: {text: "Parameters"}}}
                        // (its groups keep the sections' gap between them: no room of its own)
                        body +: {padding: Inset{left: theme.prop_pad right: theme.prop_pad top: 0 bottom: 0} panel := mod.widgets.FabParamPanel{}}
                    }
                }
                // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
                colours_slot := View{width: Fill height: Fit
                    colours := PropSection{
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/palette.svg")}} title +: {text: "Colours"}}}
                        body +: {rows := mod.widgets.FabColorRows{}}
                    }
                }
                // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
                issues_slot := View{width: Fill height: Fit
                    issues := PropSection{
                        visible: false
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/printer.svg")}} title +: {text: "Printability"}}}
                        body +: {list := mod.widgets.FabLabel{width: Fill text: "" draw_text +: {color: fab.color_warning}}}
                    }
                }
            }
        }
    }
}

/// The plate's Add menu (its button stands in the 3D view's bar): new parts (the primitives, an
/// import, from a picture). The demos are the Plates grid's; what changes a part is Modify's. Handled for the
/// app (`main`), wherever the button is, so a second 3D view raises no second menu.
pub fn part_add_menu(cx: &mut Cx, actions: &Actions) {
    if let Some(anchor) = dropdown_clicked(actions, ADD_MENU) {
        let items = vec![
            MenuItem::new(live_id!(add_box), "Box").svg("self://resources/icons/add_box.svg"),
            MenuItem::new(live_id!(add_cylinder), "Cylinder").svg("self://resources/icons/add_cylinder.svg"),
            MenuItem::new(live_id!(add_sphere), "Sphere").svg("self://resources/icons/add_sphere.svg"),
            MenuItem::new(live_id!(add_cone), "Cone").svg("self://resources/icons/add_cylinder.svg"),
            MenuItem::new(live_id!(add_torus), "Torus").svg("self://resources/icons/radio_off.svg"),
            MenuItem::sep(),
            // tiles: short labels (the import's formats and key are in its dialog and the keymap)
            MenuItem::new(live_id!(add_import), "Import…").svg("self://resources/icons/import.svg"),
            MenuItem::new(live_id!(add_picture), "Colour picture…").svg("self://resources/icons/palette.svg"),
            MenuItem::new(live_id!(add_image), "From image…").svg("self://resources/icons/spark.svg"),
        ];
        open_menu(cx, ADD_MENU, items, anchor, MenuPlace::Below);
    }
    if let Some(pick) = menu_picked(actions, ADD_MENU) {
        let shape = [(live_id!(add_box), "box"), (live_id!(add_cylinder), "cylinder"), (live_id!(add_sphere), "sphere"), (live_id!(add_cone), "cone"), (live_id!(add_torus), "torus")]
            .iter()
            .find(|(id, _)| *id == pick)
            .map(|(_, s)| *s);
        if let Some(shape) = shape {
            cx.action(PartAction::Add { shape: shape.into() });
        } else if pick == live_id!(add_import) {
            cx.action(ShellAction::ImportDialog);
        } else if pick == live_id!(add_picture) {
            cx.action(ShellAction::ColourPictureDialog);
        } else if pick == live_id!(add_image) {
            cx.action(ShellAction::ImageToPartDialog);
        }
    }
}

pub fn selected_parts(state: &AppState) -> Vec<usize> {
    state.plate.parts.iter().enumerate().filter(|(_, p)| p.element.is_some_and(|e| state.scene_state.selection.set.contains(&e))).map(|(i, _)| i).collect()
}

/// The part the part panel edits: the primary selection's.
pub fn active_part(state: &AppState) -> Option<usize> {
    state.scene_state.selection.active.and_then(|e| state.plate.part_of(e)).or_else(|| state.plate.parts.iter().position(|p| p.element.is_some_and(|e| state.scene_state.selection.contains(e))))
}

/// The plate in a few lines: its parts and filaments, its footprint, and the print when sliced.
fn plate_summary(state: &AppState) -> String {
    let parts = &state.plate.parts;
    if parts.is_empty() {
        return "Nothing on the plate yet.".into();
    }
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for p in parts {
        for k in 0..3 {
            lo[k] = lo[k].min(p.min_mm[k]);
            hi[k] = hi[k].max(p.max_mm[k]);
        }
    }
    let colours = state.print.colors.rows.iter().filter(|r| r.used).count();
    let mut out = format!(
        "{} part{} in {} colour{}\n{:.0} × {:.0} × {:.0} mm",
        parts.len(), if parts.len() == 1 { "" } else { "s" },
        colours, if colours == 1 { "" } else { "s" },
        hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2],
    );
    // a grown part that cannot grow looks done: it says so without being selected
    for line in &state.params.grow_lines {
        out.push('\n');
        out.push_str(&line.text);
    }
    let p = &state.print;
    if let Some(pv) = p.preview.as_ref().filter(|_| !p.stale) {
        // An estimate ahead of the playing preview says its totals meanwhile.
        let ahead = p.estimate.filter(|_| pv.total <= 0.0 && pv.filament_use.is_empty());
        out.push_str(&format!("\nPrint {} · {} layers", crate::print::format_time(ahead.map_or(pv.total, |e| e.seconds)), pv.max_layer() + 1));
        let grams: f64 = ahead.map_or_else(|| pv.filament_use.iter().map(|f| f.grams).sum(), |e| e.grams);
        if grams > 0.0 {
            out.push_str(&format!(" · {grams:.1} g"));
        }
    }
    out
}

fn size_text(p: &PlatePart) -> String {
    let s = p.size();
    format!("{:.1} × {:.1} × {:.1} mm", s[0], s[1], s[2])
}

/// An object whose bodies print as one: how many it has, and the size of all of them.
fn bodies_text(parts: &[PlatePart], bodies: &[usize]) -> String {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in bodies.iter().filter_map(|&i| parts.get(i)) {
        for k in 0..3 {
            lo[k] = lo[k].min(p.min_mm[k]);
            hi[k] = hi[k].max(p.max_mm[k]);
        }
    }
    format!("{} bodies as one · {:.1} × {:.1} × {:.1} mm", bodies.len(), hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2])
}

// ====================================================================// Parts list
// ====================================================================
#[derive(Default)]
struct RowDisplay {
    size: String,
    selected: bool,
    issue: bool,
    excluded: bool,
    eye: Option<bool>,
    color: [f32; 4],
    slot: String,
    /// The swatch's tooltip: the nozzle and tray its colour prints from.
    tip: String,
    /// The engine pill's word and tooltip (bodies built by different engines: "Mixed").
    engine: &'static str,
    engine_tip: &'static str,
    rect: Option<Rect>,
    /// The row as drawn, which its node stands where it shows ([`Widget::semantics_part_area`]).
    area: Area,
}

enum RenameTarget {
    Part(String),
    Measurement(usize),
}

#[derive(Script, Widget)]
pub struct FabPartsPanel {
    #[deref]
    view: View,
    /// The shadow under the toolbar while the list is scrolled down.
    #[live]
    draw_shade: DrawQuad,
    #[rust]
    revision: Option<u64>,
    #[rust]
    display_revision: Option<((u64, u64), u64)>,
    #[rust]
    measurement_labels: Vec<(Option<String>, String, MeasureKind)>,
    #[rust]
    tree: Vec<Row>,
    #[rust]
    order: Vec<usize>,
    #[rust]
    visible_rows: Vec<usize>,
    #[rust]
    rows: Vec<RowDisplay>,
    #[rust]
    collapsed: HashSet<u64>,
    /// An eye or a fold was pressed (it toggles on the press): the click that ends that press is no second
    /// toggle, whichever row's eye it lands on (a hidden part moves in the list under the
    /// pointer); a click without a press (a semantic press) toggles.
    #[rust]
    eye_pressed: Option<u64>,
    /// The filter as typed: a row shows when its name, or one of its group's or its children's,
    /// holds every word (folds are ignored meanwhile, so no match hides).
    #[rust]
    filter: String,
    /// Part rows whose feature tree the person closed (every tree starts open).
    #[rust]
    closed_trees: HashSet<u64>,
    /// A part's tree opening or closing: how far it is open (0..1) and where it heads; a tree
    /// still closing keeps its place in the list until it is shut.
    #[rust]
    reveal: HashMap<u64, (f64, f64)>,
    #[rust]
    reveal_frame: NextFrame,
    #[rust]
    reveal_last: Option<f64>,
    /// Part rows with a feature tree, and the instance it is of (as last drawn).
    #[rust]
    tree_of: HashMap<u64, String>,
    /// The list's items: a row of `tree` (its index), or an open part's feature tree (`true`).
    #[rust]
    items: Vec<(usize, bool)>,
    #[rust]
    focused: Option<u64>,
    #[rust]
    rename: Option<(u64, RenameTarget, String)>,
    #[rust]
    rename_focus: bool,
    #[rust]
    context_objects: Vec<String>,
    #[rust]
    context_row: Option<u64>,
}

impl FabPartsPanel {
    fn focus_row(&mut self, cx: &mut Cx, scope: &mut Scope, key: u64) {
        self.focused = Some(key);
        cx.set_key_focus(self.view.area());
        if let Some(state) = scope.data.get_mut::<AppState>() {
            state.ui.parts_key = self.view.area();
        }
    }

    fn select_row(&mut self, cx: &mut Cx, scope: &mut Scope, row: &Row, toggle: bool) {
        self.focus_row(cx, scope, row.key);
        if row.kind == RowKind::AnnotationGroup {
            self.toggle_row(cx, row.key);
            return;
        }
        if let Some(action) = row.measurement_action(RowCommand::Select) {
            cx.action(action);
            return;
        }
        let Some(state) = scope.data.get::<AppState>() else { return };
        let elements: Vec<_> = row.parts.iter().filter_map(|i| state.plate.parts.get(*i).and_then(|p| p.element)).collect();
        let ids = if toggle {
            let remove = elements.iter().all(|e| state.scene_state.selection.set.contains(e));
            let targets: HashSet<_> = elements.iter().copied().collect();
            let mut others: Vec<_> = state.scene_state.selection.set.iter().copied().filter(|e| !targets.contains(e)).collect();
            if remove {
                if let Some(active) = state.scene_state.selection.active {
                    if let Some(i) = others.iter().position(|e| *e == active) { others.swap(0, i); }
                }
                others
            } else {
                let mut ids = elements;
                ids.extend(others);
                ids
            }
        } else { elements };
        cx.action(ShellAction::SelectSet(ids));
        // the part is the selection now: a feature picked in its tree is let go
        cx.action(crate::ui::code::SourceAction::Select { id: String::new(), reveal: false });
    }

    fn begin_rename(&mut self, cx: &mut Cx, scope: &mut Scope, row: &Row) {
        let Some(state) = scope.data.get::<AppState>() else { return };
        if state.slice_blocks_edits() || state.ui.in_mode(PlateMode::Mechanics) { return; }
        let (target, name) = match row.kind {
            RowKind::AnnotationGroup => return,
            RowKind::Measurement(index) => {
                let Some(measurement) = state.measurements.get(index) else { return };
                (RenameTarget::Measurement(index), measurement_name(measurement).to_string())
            }
            RowKind::Part | RowKind::Script => {
                let Some(part) = state.plate.parts.get(row.rename) else { return };
                (RenameTarget::Part(part.object.clone()), part.name.clone())
            }
            // its name is its placement's: its bodies are named by it
            RowKind::Object => {
                let Some(part) = state.plate.parts.get(row.rename) else { return };
                (RenameTarget::Part(part.object.clone()), row.name.clone())
            }
        };
        self.rename = Some((row.key, target, name));
        self.rename_focus = true;
        self.view.redraw(cx);
    }

    /// A fold drag (`TreeFold`) moved to `p`: the part rows it reaches here, with children, take
    /// its state.
    fn fold_drag(&mut self, cx: &mut Cx, p: DVec2) {
        let Some(open) = cx.global::<TreeFold>().open else { return };
        let list = self.view.portal_list(cx, ids!(list));
        // the part rows shown, in their order down the list (an open part's tree is its own)
        let mut rows: Vec<(usize, Rect)> = Vec::new();
        for &pos in &self.visible_rows {
            let Some(&(i, false)) = self.items.get(pos) else { continue };
            if let Some((_, item)) = list.get_item(pos) {
                rows.push((i, item.area().rect(cx)));
            }
        }
        rows.sort_by(|a, b| a.1.pos.y.total_cmp(&b.1.pos.y));
        let Some(h) = rows.iter().position(|(_, r)| r.contains(p)) else { return };
        let keys: Vec<String> = rows.iter().map(|(i, _)| format!("p{}", self.tree[*i].key)).collect();
        for k in TreeFold::reach(cx, &keys, h) {
            let i = rows[k].0;
            if self.expanded(&self.tree[i]) == Some(!open) {
                let key = self.tree[i].key;
                self.toggle_row(cx, key);
            }
        }
    }

    fn toggle_row(&mut self, cx: &mut Cx, key: u64) {
        if self.tree_of.contains_key(&key) && !row_of(&self.tree, key).is_some_and(Row::is_branch) {
            // closed: it opens; open: it closes
            let open = self.closed_trees.remove(&key);
            if !open { self.closed_trees.insert(key); }
            // it slides open or shut from where it is now
            let to = if open { 1.0 } else { 0.0 };
            self.reveal.entry(key).or_insert((1.0 - to, to)).1 = to;
            self.reveal_frame = cx.new_next_frame();
        } else if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.view.redraw(cx);
    }

    /// Whether `row`'s children (a branch's rows, a part's feature tree) show.
    fn expanded(&self, row: &Row) -> Option<bool> {
        if row.is_branch() {
            Some(!self.collapsed.contains(&row.key))
        } else if self.tree_of.contains_key(&row.key) {
            Some(!self.closed_trees.contains(&row.key))
        } else {
            None
        }
    }

    fn open_row_menu(&mut self, cx: &mut Cx, scope: &mut Scope, row: &Row, anchor: Rect) {
        if row.kind == RowKind::AnnotationGroup { return; }
        let Some(state) = scope.data.get::<AppState>() else { return };
        if let RowKind::Measurement(index) = row.kind {
            let has = index < state.measurements.len() && !state.slice_blocks_edits() && !state.ui.in_mode(PlateMode::Mechanics);
            let items = vec![
                MenuItem::new(live_id!(part_rename), "Rename").key(&keymap::shortcut(Key::Rename)).enabled_if(has),
                MenuItem::new(live_id!(annotation_delete), "Delete").key(&keymap::shortcut(Key::Delete)).enabled_if(has),
            ];
            self.context_objects.clear();
            self.context_row = Some(row.key);
            self.select_row(cx, scope, row, false);
            open_menu(cx, PART_MENU, items, anchor, MenuPlace::At);
            return;
        }
        let selected: HashSet<_> = selected_parts(state).into_iter().collect();
        let selected_row = row.parts.iter().all(|i| selected.contains(i));
        self.context_objects = if selected_row {
            selected_parts(state).into_iter().map(|i| state.plate.parts[i].object.clone()).collect()
        } else {
            row.parts.iter().filter_map(|i| state.plate.parts.get(*i)).map(|p| p.object.clone()).collect()
        };
        let has = !self.context_objects.is_empty() && !state.ui.in_mode(PlateMode::Mechanics);
        let mut items = part_menu(state, &self.context_objects, has);
        items.push(MenuItem::sep());
        items.push(MenuItem::new(live_id!(part_rename), "Rename").key(&keymap::shortcut(Key::Rename)).enabled_if(has && !state.slice_blocks_edits()));
        if !selected_row { self.select_row(cx, scope, row, false); }
        else { self.focus_row(cx, scope, row.key); }
        self.context_row = Some(row.key);
        open_menu(cx, PART_MENU, items, anchor, MenuPlace::At);
    }

    fn sync_tree(&mut self, state: &AppState) {
        // Compare just the displayed metadata, never clone points/normals per frame.
        // Measurement edits can arrive without a change to the plate's revision.
        let measurements_changed = self.measurement_labels.len() != state.measurements.len()
            || self.measurement_labels.iter().zip(&state.measurements).any(|((name, label, kind), m)| {
                name != &m.name || label != &m.label || *kind != m.kind
            });
        if self.revision == Some(state.plate.revision) && !measurements_changed { return; }
        if self.measurement_labels.len() > state.measurements.len() {
            // Deletion shifts document indices. Do not let an open editor or menu
            // act on the measurement that inherited the old row's index.
            if self.rename.as_ref().is_some_and(|(_, target, _)| matches!(target, RenameTarget::Measurement(_))) {
                self.rename = None;
            }
            if self.tree.iter().any(|row| Some(row.key) == self.context_row && matches!(row.kind, RowKind::Measurement(_))) {
                self.context_row = None;
            }
            if self.tree.iter().any(|row| Some(row.key) == self.focused && matches!(row.kind, RowKind::Measurement(_))) {
                self.focused = None;
            }
        }
        self.display_revision = None;
        self.tree = tree_rows(&state.plate, &[measurement_group(&state.measurements)]);
        self.revision = Some(state.plate.revision);
        if measurements_changed {
            self.measurement_labels = state.measurements.iter().map(|m| (m.name.clone(), m.label.clone(), m.kind)).collect();
        }
        let keys: HashSet<_> = self.tree.iter().map(|r| r.key).collect();
        self.collapsed.retain(|key| keys.contains(key));
        if self.rename.as_ref().is_some_and(|(key, _, _)| !keys.contains(key)) { self.rename = None; }
        if self.focused.is_some_and(|key| !keys.contains(&key)) { self.focused = None; }
    }
}

impl ScriptHook for FabPartsPanel {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if !apply.is_eval() {
            self.revision = None;
            self.display_revision = None;
        }
    }
}

impl Widget for FabPartsPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Tree;
        makepad_platform::a11y::set_string(&mut node.name, "Parts");
        node.set_size = self.tree.iter().filter(|r| r.parent.is_none()).count() as u32;
        node.states.set(a11y_tree::States::MULTISELECTABLE, true);
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for &i in &self.order {
            let (row, shown) = (&self.tree[i], &self.rows[i]);
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::TreeItem,
                bounds: makepad_platform::a11y::semantic_rect(shown.rect.unwrap_or_default()),
                level: depth(&self.tree, row) + 1,
                pos_in_set: row.pos_in_set,
                set_size: row.set_size,
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::FOCUS,
                ..Default::default()
            };
            makepad_platform::a11y::set_string(&mut node.name, &row.name);
            let mut description = shown.size.clone();
            if row.is_part() && row.kind != RowKind::Script && linked(&self.tree, row) { description.push_str(", linked instance"); }
            if shown.issue { description.push_str(", has printability issues"); }
            if shown.excluded { description.push_str(", excluded from print"); }
            makepad_platform::a11y::set_string(&mut node.description, &description);
            node.states = a11y_tree::States::FOCUSABLE;
            if row.kind != RowKind::AnnotationGroup {
                node.actions = node.actions | a11y_tree::Actions::SELECT | a11y_tree::Actions::SHOW_MENU;
                node.states.set(a11y_tree::States::SELECTABLE, true);
            }
            if row.is_part() { node.actions = node.actions | a11y_tree::Actions::ADD_SELECTION; }
            node.states.set(a11y_tree::States::SELECTED, shown.selected);
            node.states.set(a11y_tree::States::FOCUSED, self.focused == Some(row.key) && cx.keyboard.key_focus() == self.view.area());
            node.states.set(a11y_tree::States::OFFSCREEN, shown.rect.is_none());
            if let Some(open) = self.expanded(row) {
                node.actions = node.actions | a11y_tree::Actions::EXPAND | a11y_tree::Actions::COLLAPSE;
                node.states.set(a11y_tree::States::EXPANDED, open);
                node.states.set(a11y_tree::States::COLLAPSED, !open);
            }
            out.push((row.key, row.parent, node));
        }
    }

    fn semantics_part_area(&self, key: u64) -> Area {
        // a drawn row: one the list scrolled past an edge is offscreen
        self.visible_rows.iter().filter_map(|&pos| self.items.get(pos)).filter(|(_, tree)| !tree).find(|(i, _)| self.tree[*i].key == key).map_or(Area::Empty, |(i, _)| self.rows[*i].area)
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(i) = self.order.iter().copied().find(|i| self.tree[*i].key == key) else { return ActionResult::Unsupported };
        let row = self.tree[i].clone();
        if request.action == Actions::SELECT || request.action == Actions::PRESS || request.action == Actions::ADD_SELECTION {
            self.select_row(cx, scope, &row, request.action == Actions::ADD_SELECTION);
        } else if request.action == Actions::FOCUS {
            self.focus_row(cx, scope, key);
        } else if request.action == Actions::SHOW_MENU {
            self.open_row_menu(cx, scope, &row, self.rows[i].rect.unwrap_or_default());
        } else if self.expanded(&row).is_some() && (request.action == Actions::EXPAND || request.action == Actions::COLLAPSE) {
            if self.expanded(&row) != Some(request.action == Actions::EXPAND) { self.toggle_row(cx, key); }
        } else {
            return ActionResult::Unhandled;
        }
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        match event {
            Event::MouseUp(_) => cx.global::<TreeFold>().open = None,
            Event::MouseMove(e) => self.fold_drag(cx, e.abs),
            _ => {}
        }
        if let Event::KeyDown(e) = event {
            if e.key_code == KeyCode::F2 && !e.is_repeat && cx.keyboard.key_focus() == self.view.area() {
                if let Some(row) = self.tree.iter().find(|r| Some(r.key) == self.focused).cloned() {
                    self.begin_rename(cx, scope, &row);
                }
            }
        }
        if let Event::NextFrame(ne) = event {
            if ne.set.contains(&self.reveal_frame) && !self.reveal.is_empty() {
                let dt = self.reveal_last.map_or(0.0, |t| (ne.time - t).clamp(0.0, 0.05));
                self.reveal_last = Some(ne.time);
                let step = dt / crate::ui::code::fold_s();
                for (now, to) in self.reveal.values_mut() {
                    *now = if *to > *now { (*now + step).min(*to) } else { (*now - step).max(*to) };
                }
                // a tree that got where it was going needs no entry (closed_trees says the rest)
                self.reveal.retain(|_, (now, to)| now != to);
                if self.reveal.is_empty() { self.reveal_last = None; } else { self.reveal_frame = cx.new_next_frame(); }
                self.view.redraw(cx);
            }
        }
        // An eye's press-time toggle skips the click that ends that press, and only that one: the
        // mark lasts through the release's own actions (`RELEASED`), then goes, so a release
        // that clicks nothing (its button hid as it toggled) leaves no skip behind.
        const RELEASED: u64 = u64::MAX;
        match event {
            Event::Actions(_) => {}
            Event::MouseUp(_) if self.eye_pressed.is_some() => self.eye_pressed = Some(RELEASED),
            _ if self.eye_pressed == Some(RELEASED) => self.eye_pressed = None,
            _ => {}
        }
        let Event::Actions(actions) = event else { return };
        let list = self.view.portal_list(cx, ids!(list));
        for visible in 0..self.visible_rows.len() {
            let pos = self.visible_rows[visible];
            // an open part's tree handles its own events (the list hands them on)
            let Some(&(i, false)) = self.items.get(pos) else { continue };
            let key = self.tree[i].key;
            if let Some((_, item)) = list.get_item(pos) {
                let input = item.text_input(cx, ids!(text.rename));
                if self.rename.as_ref().is_some_and(|(key, _, _)| *key == self.tree[i].key) {
                    if input.escaped(actions) {
                        self.rename = None;
                        self.focus_row(cx, scope, key);
                        self.view.redraw(cx);
                    } else if input.returned(actions).is_some() || input.key_focus_lost(actions) {
                        let name = input.text().trim().to_string();
                        if let Some((_, target, old)) = self.rename.take() {
                            if name != old {
                                match target {
                                    RenameTarget::Part(object) if !name.is_empty() => cx.action(PartAction::Rename { object, name }),
                                    RenameTarget::Measurement(index) => cx.action(MeasureAction::Rename(index, name)),
                                    _ => {}
                                }
                            }
                        }
                        if input.returned(actions).is_some() { self.focus_row(cx, scope, key); }
                        self.view.redraw(cx);
                    }
                }
                let fold_pressed = item.button(cx, ids!(branch)).pressed(actions) || item.button(cx, ids!(branch_open)).pressed(actions);
                let fold_clicked = item.button(cx, ids!(branch)).clicked(actions) || item.button(cx, ids!(branch_open)).clicked(actions);
                // a fold toggles as it is pressed (its click after is skipped, as an eye's is)
                if fold_pressed || (fold_clicked && self.eye_pressed.take().is_none()) {
                    if fold_pressed { self.eye_pressed = Some(key); }
                    self.focus_row(cx, scope, key);
                    self.toggle_row(cx, key);
                    // a drag on from the press folds or unfolds the rows it crosses the same way
                    if fold_pressed {
                        let open = self.expanded(&self.tree[i]).unwrap_or(true);
                        TreeFold::start(cx, open, format!("p{key}"));
                    }
                } else if item.button(cx, ids!(eye)).pressed(actions) || item.button(cx, ids!(eye_off)).pressed(actions)
                    || ((item.button(cx, ids!(eye)).clicked(actions) || item.button(cx, ids!(eye_off)).clicked(actions)) && self.eye_pressed.take().is_none())
                {
                    // the eye toggles as it is pressed
                    if item.button(cx, ids!(eye)).pressed(actions) || item.button(cx, ids!(eye_off)).pressed(actions) {
                        self.eye_pressed = Some(key);
                    }
                    if let Some(state) = scope.data.get::<AppState>() {
                        if !state.slice_blocks_edits() && !state.ui.in_mode(PlateMode::Mechanics) {
                            if let Some(action) = self.tree[i].eye_action(&state.plate) { cx.action(action); }
                        }
                    }
                } else if let Some(down) = item.as_view().finger_down(actions).filter(|d| d.device.is_primary_hit()) {
                    // a row is picked as it is pressed
                    let row = self.tree[i].clone();
                    self.select_row(cx, scope, &row, down.modifiers.shift || down.modifiers.is_primary());
                    if down.tap_count == 2 { self.begin_rename(cx, scope, &row); }
                } else if let Some(up) = item.as_view().finger_up(actions) {
                    if !up.cancelled && up.is_over && !up.is_sweep
                        && up.device.mouse_button().is_some_and(|b| b.is_secondary())
                        && (up.abs - up.abs_start).length() < crate::nav::DRAG_THRESHOLD_PT
                    {
                        let row = self.tree[i].clone();
                        self.open_row_menu(cx, scope, &row, Rect { pos: up.abs, size: dvec2(0.0, 0.0) });
                    }
                }
            }
        }
        if self.eye_pressed == Some(RELEASED) {
            self.eye_pressed = None;
        }
        if let Some(pick) = menu_picked(actions, PART_MENU) {
            if pick == live_id!(part_rename) {
                if let Some(row) = self.tree.iter().find(|r| Some(r.key) == self.context_row).cloned() {
                    self.begin_rename(cx, scope, &row);
                }
            } else if pick == live_id!(annotation_delete) {
                if let Some(state) = scope.data.get::<AppState>() {
                    if !state.slice_blocks_edits() && !state.ui.in_mode(PlateMode::Mechanics) {
                        if let Some(action) = self.tree.iter().find(|r| Some(r.key) == self.context_row).and_then(|row| row.measurement_action(RowCommand::Delete)) {
                            cx.action(action);
                        }
                    }
                }
            } else if let Some(state) = scope.data.get::<AppState>() {
                part_menu_action(cx, state, &self.context_objects, pick);
            }
        }
        if let Some(text) = self.view.text_input(cx, ids!(tools.filter.row.input)).changed(actions) {
            self.filter = text;
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return self.view.draw_walk(cx, scope, walk) };
        self.sync_tree(state);
        self.order.clear();
        if self.filter.trim().is_empty() {
            self.order.extend(self.tree.iter().enumerate().filter(|(_, r)| !folded(&self.tree, &self.collapsed, r)).map(|(i, _)| i));
        } else {
            // a row that matches keeps its groups above it and its rows under it: O(rows · depth)
            let words: Vec<String> = self.filter.split_whitespace().map(|w| w.to_lowercase()).collect();
            let hit = |r: &Row| { let name = r.name.to_lowercase(); words.iter().all(|w| name.contains(w.as_str())) };
            let mut keep: HashSet<u64> = HashSet::new();
            for r in self.tree.iter().filter(|r| hit(r)) {
                keep.insert(r.key);
                keep.extend(std::iter::successors(r.parent, |&k| row_of(&self.tree, k).and_then(|p| p.parent)));
            }
            let under_hit = |r: &Row| std::iter::successors(r.parent, |&k| row_of(&self.tree, k).and_then(|p| p.parent)).any(|k| row_of(&self.tree, k).is_some_and(|p| hit(p)));
            self.order.extend(self.tree.iter().enumerate().filter(|(_, r)| keep.contains(&r.key) || under_hit(r)).map(|(i, _)| i));
        }
        // A part's feature tree: its instance's (an object's bodies are one instance: its row
        // has none, nor have they), when that instance has rows.
        self.tree_of.clear();
        for row in &self.tree {
            if row.kind != RowKind::Part || row.parent.and_then(|k| row_of(&self.tree, k)).is_some_and(|p| p.kind == RowKind::Object) { continue; }
            let Some(p) = state.plate.parts.get(row.rename) else { continue };
            let Some((instance, _)) = p.object.split_once('/') else { continue };
            let has = if instance == state.source.instance { !state.source.features.is_empty() } else { state.source.trees.get(instance).is_some_and(|t| !t.is_empty()) };
            if has { self.tree_of.insert(row.key, instance.to_string()); }
        }
        // (a part whose tree is being read again keeps it closed: kept while its row stays)
        let rows: HashSet<u64> = self.tree.iter().map(|r| r.key).collect();
        self.closed_trees.retain(|key| rows.contains(key));
        self.reveal.retain(|key, _| self.tree_of.contains_key(key));
        self.items.clear();
        for &i in &self.order {
            self.items.push((i, false));
            let key = self.tree[i].key;
            let open = self.tree_of.contains_key(&key) && !self.closed_trees.contains(&key);
            if open || self.reveal.get(&key).is_some_and(|(now, _)| *now > 0.0) { self.items.push((i, true)); }
        }
        let parts = &state.plate.parts;
        let revision = ((state.plate.revision, state.field_loop_marks), state.print.revision);
        if self.display_revision.is_none_or(|r| r.0 != revision.0) {
            self.rows = self.tree.iter().map(|row| {
                if !row.is_part() {
                    return RowDisplay {
                        size: row.detail.clone(),
                        selected: row.annotation_selected(state),
                        ..Default::default()
                    };
                }
                let p = &parts[row.rename];
                let [r, g, b] = crate::ui::popover::display_rgb([p.color[0], p.color[1], p.color[2]]);
                let mixed = row.parts.iter().any(|i| parts[*i].engine != p.engine);
                RowDisplay {
                    size: row.detail_text(&state.plate),
                    selected: false,
                    issue: row.parts.iter().any(|i| parts[*i].blocking || !parts[*i].issues.is_empty()),
                    excluded: row.excluded(&state.plate),
                    eye: row.eye_state(&state.plate),
                    color: [r, g, b, 1.0],
                    slot: String::new(),
                    tip: String::new(),
                    engine: if mixed { "Mixed" } else { p.engine.label() },
                    engine_tip: if mixed { "Its bodies are built by different engines: each body's pill names its own" } else { p.engine.tip() },
                    rect: None,
                    area: Area::Empty,
                }
            }).collect();
            self.view.view(cx, ids!(empty)).set_visible(cx, self.tree.is_empty());
            let issues = parts.iter().filter(|p| p.blocking || !p.issues.is_empty()).count();
            let mut totals = format!("{} part{}", parts.len(), if parts.len() == 1 { "" } else { "s" });
            if issues > 0 { totals.push_str(&format!(" · {issues} with issues")); }
            if !state.plate.machine.is_empty() { totals.push_str(&format!(" · {}", state.plate.machine)); }
            self.view.label(cx, ids!(footer.totals)).set_text(cx, &totals);
        }
        if self.display_revision != Some(revision) {
            self.display_revision = Some(revision);
            for (row, shown) in self.tree.iter().zip(&mut self.rows) {
                if !row.is_part() { continue; }
                let p = &parts[row.rename];
                let slot = if row.parts.iter().all(|i| parts[*i].print_color == p.print_color) {
                    state.print.colors.row(&p.print_color).and_then(|r| r.pos).map_or_else(String::new, |pos| crate::print::colours::head_mark(&state.print.colors.layout.heads[pos.head]))
                } else { "-".into() };
                if shown.slot != slot {
                    shown.slot.clear();
                    shown.slot.push_str(&slot);
                }
                shown.tip = if slot == "-" {
                    "Its bodies print in more than one colour: see each in the Part panel's Colours".to_string()
                } else {
                    // its colour, then the nozzle and tray it prints from
                    colour_tip(state, &p.print_color)
                };
            }
        }
        // A feature picked in a part's open tree is what is selected: its part's row does not show
        // it too (folded, the part's row is all that can show it).
        let feature_picked = state.source.selected.is_some();
        for (row, shown) in self.tree.iter().zip(&mut self.rows) {
            shown.selected = if row.is_part() {
                let in_tree = feature_picked && !self.closed_trees.contains(&row.key) && self.tree_of.get(&row.key).is_some_and(|i| *i == state.source.instance);
                !in_tree && row.parts.iter().all(|i| parts[*i].element.is_some_and(|e| state.scene_state.selection.contains(e)))
            } else {
                row.annotation_selected(state)
            };
            shown.rect = None;
            shown.area = Area::Empty;
        }
        self.visible_rows.clear();
        let old_area = self.view.area();
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = step.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else { continue };
            list.set_item_range(cx, 0, self.items.len());
            while let Some(pos) = list.next_visible_item(cx) {
                let Some(&(i, tree)) = self.items.get(pos) else { continue };
                self.visible_rows.push(pos);
                if tree {
                    // under its part's text: the row's indent, past the swatch
                    let indent = 10.0 + TREE_STEP * depth(&self.tree, &self.tree[i]) as f64 + 14.0;
                    let item = list.item(cx, pos, live_id!(Features));
                    let key = self.tree[i].key;
                    let open = self.reveal.get(&key).map_or(if self.closed_trees.contains(&key) { 0.0 } else { 1.0 }, |(now, _)| *now);
                    if let (Some(instance), Some(mut t)) = (self.tree_of.get(&key), item.borrow_mut::<crate::ui::code::FabFeatureTree>()) {
                        t.show_instance(instance, indent, open);
                    }
                    item.draw_all(cx, scope);
                    continue;
                }
                let (row, shown) = (&self.tree[i], &mut self.rows[i]);
                let item = list.item(cx, pos, live_id!(Row));
                let sel = if shown.selected { 1.0 } else { 0.0 };
                let indent = 10.0 + TREE_STEP * depth(&self.tree, row) as f64;
                if let Some(mut view) = item.as_view().borrow_mut() {
                    view.layout.padding.left = indent;
                    view.draw_bg.set_dyn_instance(cx, live_id!(selected), &[sel]);
                }
                let open = if row.is_branch() { Some(!self.collapsed.contains(&row.key)) } else if self.tree_of.contains_key(&row.key) { Some(!self.closed_trees.contains(&row.key)) } else { None };
                item.widget(cx, ids!(branch)).set_visible(cx, open == Some(false));
                item.widget(cx, ids!(branch_open)).set_visible(cx, open == Some(true));
                item.view(cx, ids!(linked)).set_visible(cx, row.is_part() && linked(&self.tree, row));
                let [r, g, b, _] = shown.color;
                let sw = item.view(cx, ids!(swatch));
                sw.set_visible(cx, row.is_part());
                if let Some(mut sw) = sw.borrow_mut() {
                    sw.draw_bg.set_dyn_instance(cx, live_id!(color), &[r, g, b, 1.0]);
                }
                item.label(cx, ids!(swatch.slot)).set_text(cx, &shown.slot);
                item.widget(cx, ids!(engine_tip)).set_visible(cx, row.is_part());
                item.label(cx, ids!(engine.label)).set_text(cx, shown.engine);
                if let Some(mut tip) = item.widget(cx, ids!(engine_tip)).borrow_mut::<makepad_widgets_core::tip::Tip>() {
                    if tip.text != shown.engine_tip { tip.text = shown.engine_tip.to_string(); }
                }
                if let Some(mut tip) = item.widget(cx, ids!(swatch_tip)).borrow_mut::<makepad_widgets_core::tip::Tip>() {
                    tip.text.clone_from(&shown.tip);
                }
                let alpha = if shown.excluded { 0.4 } else { 1.0 };
                for path in [ids!(text.name), ids!(text.size)] {
                    if let Some(mut label) = item.label(cx, path).borrow_mut() {
                        label.draw_text.set_dyn_instance(cx, live_id!(row_alpha), &[alpha]);
                    }
                }
                item.label(cx, ids!(text.name)).set_text(cx, &row.name);
                item.label(cx, ids!(text.size)).set_text(cx, &shown.size);
                item.view(cx, ids!(warn)).set_visible(cx, shown.issue);
                item.widget(cx, ids!(eye)).set_visible(cx, shown.eye == Some(true));
                item.widget(cx, ids!(eye_off)).set_visible(cx, shown.eye == Some(false));
                let renaming = self.rename.as_ref().is_some_and(|(key, _, _)| *key == row.key);
                item.widget(cx, ids!(text.name)).set_visible(cx, !renaming);
                if renaming {
                    let input = item.widget(cx, ids!(text.rename));
                    crate::ui::print_settings::name_control(cx, &input, row.rename_label(), "text_field");
                }
                item.widget(cx, ids!(text.rename)).set_visible(cx, renaming);
                let focus = renaming && self.rename_focus;
                if focus {
                    let input = item.text_input(cx, ids!(text.rename));
                    input.set_text(cx, &self.rename.as_ref().unwrap().2);
                }
                item.draw_all(cx, &mut Scope::empty());
                if focus {
                    let input = item.text_input(cx, ids!(text.rename));
                    input.set_key_focus(cx);
                    if let Some(mut input) = input.borrow_mut() { input.select_all(cx); }
                    self.rename_focus = false;
                }
                shown.rect = Some(item.area().rect(cx));
                shown.area = item.area();
            }
        }
        if let Some(state) = scope.data.get_mut::<AppState>() {
            state.ui.refresh_plate_keys(old_area, self.view.area());
        }
        // the list scrolled down: a shadow along its top, under the toolbar
        let list = self.view.portal_list(cx, ids!(list));
        let (first_id, first_scroll) = (list.first_id(), list.scroll_position());
        crate::ui::widgets::draw_scroll_shade(cx, &mut self.draw_shade, list.area().rect(cx), first_id, first_scroll);
        DrawStep::done()
    }
}

// ====================================================================// Print colour chips
// ====================================================================
/// A print colour's tooltip: its name and its tray's.
fn colour_tip(state: &AppState, name: &str) -> String {
    let m = &state.print.colors;
    let Some(r) = m.row(name) else { return name.to_string() };
    if r.unassigned() {
        return format!("{}\nNo tray yet: pick one in the part's Colours list; it prints as the first material until then", r.name);
    }
    match &r.tray {
        Some(t) => format!("{}\n{}", r.name, crate::print::colours::tray_tip(t, &m.layout)),
        None => r.name.clone(),
    }
}

/// The print colours a part prints with, as the palette has them: its own (its `color:`)
/// first, then each painted one (fills and strokes) once, with the painted materials (colour
/// values) that print in it. A painted material without a colour of its own prints in the
/// part's and moves with it.
pub(crate) fn part_colours(m: &crate::print::colours::Mapping, p: &PlatePart) -> Vec<(crate::print::colours::Row, Vec<[f32; 3]>)> {
    let mut colours: Vec<(String, Vec<[f32; 3]>)> = vec![(p.print_color.clone(), Vec::new())];
    for c in p.paint.iter().flat_map(|paint| paint.colors.iter().skip(1)) {
        let Some(name) = c.color.clone().filter(|n| *n != p.print_color) else { continue };
        match colours.iter_mut().find(|(n, _)| *n == name) {
            Some((_, rgbs)) => rgbs.push(c.rgb),
            None => colours.push((name, vec![c.rgb])),
        }
    }
    colours.into_iter().filter_map(|(n, rgbs)| m.row(&n).map(|r| (r.clone(), rgbs))).collect()
}

/// The Part panel's Colours list: one [`FabColorLink`] row per colour the part prints with
/// ([`part_colours`]), base first; each row's slot is the dropdown that picks its tray.
#[derive(Script, Widget)]
pub struct FabColorRows {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[layout] layout: Layout,
    #[rust] template: Option<ScriptObjectRef>,
    #[rust] rows: Vec<WidgetRef>,
    #[redraw] #[rust] area: Area,
}

impl ScriptHook for FabColorRows {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if kv.key.as_id() == Some(live_id!(Row)) {
                            if let Some(row) = kv.value.as_object() { self.template = Some(vm.new_object_ref(row)); }
                        }
                    }
                });
            }
        }
        if apply.is_reload() { self.rows.clear(); }
    }
}

impl Widget for FabColorRows {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Part colours");
    }

    fn visit_cancel(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) -> bool {
        for (i, w) in self.rows.iter().enumerate() { visit(LiveId(i as u64 + 1), w.clone()); }
        true
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for row in &mut self.rows { row.handle_event(cx, event, scope); }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let (colours, base, object, objects) = match scope.data.get::<AppState>().and_then(|state| Some((state, active_part(state)?))) {
            Some((state, k)) => {
                let part = &state.plate.parts[k];
                // The base colour's pick prints every selected part from the tray, as the part
                // menu's does; a painted colour's only this part's materials.
                let mut objects: Vec<String> = selected_parts(state).into_iter().map(|i| state.plate.parts[i].object.clone()).collect();
                if !objects.contains(&part.object) {
                    objects = vec![part.object.clone()];
                }
                (part_colours(&state.print.colors, part), part.print_color.clone(), part.object.clone(), objects)
            }
            None => (Vec::new(), String::new(), String::new(), Vec::new()),
        };
        cx.begin_turtle(walk, self.layout);
        self.rows.truncate(colours.len());
        for (i, (row, paint)) in colours.into_iter().enumerate() {
            if self.rows.len() <= i {
                let Some(template) = &self.template else { break };
                let value = template.as_object().into();
                let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
                cx.widget_tree_insert_child(self.uid, LiveId(i as u64 + 1), w.clone());
                self.rows.push(w);
            }
            let w = &self.rows[i];
            if let Some(mut link) = w.borrow_mut::<FabColorLink>() {
                link.base = row.name == base;
                link.part = Some(PartPick { objects: if link.base { objects.clone() } else { vec![object.clone()] }, paint });
                link.row = Some(row);
            }
            w.draw_all(cx, scope);
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

/// The brush's compact swatch row: the print colours (identity is the name, so two whites in
/// two materials are two swatches), each with its nozzle under it.
#[derive(Script, ScriptHook, Widget)]
pub struct FabPaintPalette {
    #[uid] uid: WidgetUid,
    #[walk] walk: Walk,
    #[live] color_error: Vec4f,
    #[live] #[redraw] draw_chip: DrawFabChip,
    #[live] draw_text: DrawText,
    /// A swatch's side.
    #[live(14.0)] chip: f64,
    /// The swatches centred in the width (the rail's narrow strip), else from the left.
    #[live(true)] centred: bool,
    /// Each swatch: its rect and its index in `PlateInfo::colors`.
    #[rust] chips: Vec<(Rect, usize)>,
    #[rust] current: Option<String>,
    #[rust] hint: PropHint,
    #[rust] area: Area,
}

/// After a pick: the plate view's paint row draws in a list of its own that the view's redraws
/// reuse, so the swatch ring, the brush dial's disc and the support toggles redraw only when asked.
fn repaint_brush_row(cx: &mut Cx) {
    crate::ui::redraw(cx, &[ids!(model_ui.context.paint_bar)]);
}

impl Widget for FabPaintPalette {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::RadioGroup;
        makepad_platform::a11y::set_string(&mut node.name, "Brush colour");
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (i, (rect, _)) in self.chips.iter().enumerate() {
            let node = a11y_tree::Node {
                role: a11y_tree::Role::Radio,
                bounds: makepad_platform::a11y::semantic_rect(*rect),
                pos_in_set: i as u32 + 1,
                set_size: self.chips.len() as u32,
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::SELECT,
                ..Default::default()
            };
            out.push((i as u64 + 1, None, node));
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        if request.action != Actions::PRESS && request.action != Actions::SELECT { return ActionResult::Unhandled; }
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some((_, index)) = key.checked_sub(1).and_then(|i| self.chips.get(i as usize)) else { return ActionResult::Unsupported };
        cx.action(crate::ui::stage::StageAction::PaintColor(*index));
        repaint_brush_row(cx);
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let uid = self.uid;
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let over = self.chips.iter().find(|(r, _)| r.contains(e.abs)).copied();
                match (over, scope.data.get::<AppState>()) {
                    (Some((r, index)), Some(state)) => {
                        let name = state.plate.colors.get(index).map(|c| c.name.as_str()).unwrap_or("");
                        let text = colour_tip(state, name);
                        self.hint.hover(cx, uid, Some((LiveId(index as u64 + 1), r, text.as_str())));
                    }
                    _ => self.hint.clear(cx, uid),
                }
            }
            Hit::FingerHoverOut(_) | Hit::Nothing if matches!(event, Event::MouseMove(_)) => self.hint.clear(cx, uid),
            Hit::FingerUp(fe) if fe.is_over && !fe.cancelled => {
                if let Some((_, index)) = self.chips.iter().find(|(r, _)| r.contains(fe.abs)) {
                    self.hint.clear(cx, uid);
                    cx.action(crate::ui::stage::StageAction::PaintColor(*index));
                    repaint_brush_row(cx);
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.chips.clear();
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        // painting a support channel, no colour is the brush's
        self.current = state.ui.paint_name.clone().filter(|_| state.ui.paint_target == PaintTarget::Color);
        let m = &state.print.colors;
        // The used colours first, then the project's others.
        let mut colors: Vec<(usize, &crate::print::colours::Row)> = Vec::new();
        for used in [true, false] {
            for (i, c) in state.plate.colors.iter().enumerate() {
                if let Some(r) = m.row(&c.name).filter(|r| r.used == used && (used || !r.implicit)) {
                    colors.push((i, r));
                }
            }
        }
        // Square swatches with the nozzle mark under each, as many to a row as fit.
        let chip = self.chip;
        let gap = (chip * 0.22).round();
        let cell_h = chip + 11.0;
        // A Fit width (a row of tools across the view) takes every swatch in one row.
        let fit = walk.width.is_fit();
        let width = if fit { colors.len().max(1) as f64 * (chip + gap) - gap + 4.0 } else { cx.turtle().next_walk_width(walk.width, walk.margin) };
        let columns = (((width + gap) / (chip + gap)).floor().max(1.0) as usize).min(colors.len().max(1));
        let grid_w = columns as f64 * (chip + gap) - gap;
        let mut walk = walk;
        if fit {
            walk.width = Size::Fixed(width);
        }
        walk.height = Size::Fixed(colors.len().div_ceil(columns) as f64 * (cell_h + gap) + 1.0);
        let rect = cx.walk_turtle(walk);
        // clear of the active swatch's outline on the left
        let left = if self.centred { ((width - grid_w) * 0.5).max(0.0) } else { 2.0 };
        for (k, (index, row)) in colors.into_iter().enumerate() {
            let r = Rect { pos: rect.pos + dvec2(left + (k % columns) as f64 * (chip + gap), 1.0 + (k / columns) as f64 * (cell_h + gap)), size: dvec2(chip, chip) };
            self.draw_chip.spool = 0.0;
            self.draw_chip.ring_width = 1.0;
            if self.current.as_deref() == Some(row.name.as_str()) {
                // The active colour: a white outline round its swatch.
                self.draw_chip.fill = vec4(1.0, 1.0, 1.0, 1.0);
                self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_chip.draw_abs(cx, Rect { pos: r.pos - dvec2(2.0, 2.0), size: r.size + dvec2(4.0, 4.0) });
            }
            let [r0, g0, b0] = crate::ui::popover::display_rgb([row.shown[0], row.shown[1], row.shown[2]]);
            self.draw_chip.fill = vec4(r0, g0, b0, 1.0);
            self.draw_chip.ring = if row.unassigned() { self.color_error } else { vec4(0.0, 0.0, 0.0, 0.5) };
            self.draw_chip.ring_width = if row.unassigned() { 1.5 } else { 1.0 };
            self.draw_chip.draw_abs(cx, r);
            let mark = row.pos.map_or_else(|| "–".into(), |p| crate::print::colours::head_mark(&m.layout.heads[p.head]));
            let tw = self.draw_text.prepare_single_line_run(cx, &mark).map(|t| t.width_in_lpxs as f64).unwrap_or(6.0);
            self.draw_text.draw_abs(cx, dvec2(r.pos.x + (chip - tw) * 0.5, r.pos.y + chip + 1.0), &mark);
            self.chips.push((Rect { pos: r.pos, size: dvec2(chip, cell_h) }, index));
        }
        cx.add_rect_area(&mut self.area, rect);
        DrawStep::done()
    }
}

/// A brush strip dial: diameter as a disc, fill angle as two rays, sculpt strength, or (`force`)
/// a load's size. Drag it up or down, or scroll over it; the number sits under it.
#[derive(Script, ScriptHook, Widget)]
pub struct FabBrushDial {
    #[uid] uid: WidgetUid,
    #[walk] walk: Walk,
    #[live] #[redraw] draw_chip: DrawFabChip,
    #[live] draw_text: DrawText,
    #[live] angle: bool,
    #[live] sculpt: bool,
    #[live] strength: bool,
    #[live] force: bool,
    #[live(true)] #[visible] visible: bool,
    #[rust] drag: Option<(f64, f64)>,
    /// A picked load's size while a drag moves it (written into the script on release).
    #[rust] dragged: Option<f64>,
    #[rust] hover: bool,
    #[rust] area: Area,
}

/// The brush diameters the dial spans (mm), drawn as a log scale of discs.
const DIAL_MIN_MM: f64 = 0.2;
const DIAL_MAX_MM: f64 = 200.0;

/// What the Force strip's dial sizes: the load picked (one the script writes at its top, not a
/// fixture), else the next load of the strip's kind. Its kind, its size and the load's index.
fn force_target(state: &AppState) -> (ForceKind, f64, Option<usize>) {
    let picked = state.force.focus.and_then(|i| Some((i, state.force.loads.get(i)?))).filter(|(_, l)| l.top && l.kind != ForceKind::Fixed);
    match picked {
        Some((i, l)) => (l.kind, l.size(), Some(i)),
        None => (state.ui.force_kind, state.ui.force_size[state.ui.force_kind as usize], None),
    }
}

impl FabBrushDial {
    /// A load size to `v` (its kind's range): a picked load's at once (`now`) or once the drag
    /// ends, the next load's at once.
    fn set_force(&mut self, cx: &mut Cx, state: &AppState, v: f64, now: bool) {
        let (kind, _, picked) = force_target(state);
        let Some((lo, hi)) = kind.sizes() else { return };
        let v = v.clamp(lo, hi);
        match picked {
            Some(i) if now => {
                self.dragged = None;
                cx.action(ForceAction::Value(i, state.force.loads[i].sized(v)));
            }
            Some(_) => self.dragged = Some(v),
            None => cx.action(crate::ui::stage::StageAction::ForceSize(kind, v)),
        }
    }

    fn value(&self, state: &AppState) -> f64 {
        if self.force {
            return self.dragged.unwrap_or(force_target(state).1);
        }
        if self.strength { state.ui.sculpt_strength } else if self.angle { state.ui.paint_angle } else if self.sculpt { state.ui.sculpt_radius * 2.0 } else { state.ui.paint_radius * 2.0 }
    }

    fn set(&self, cx: &mut Cx, value: f64) {
        if self.strength {
            cx.action(crate::ui::stage::StageAction::SculptStrength(value.clamp(0.1, 1.0)));
        } else if self.angle {
            cx.action(crate::ui::stage::StageAction::PaintAngle(value.clamp(1.0, 180.0)));
        } else if self.sculpt {
            cx.action(crate::ui::stage::StageAction::SculptRadius((value * 0.5).clamp(crate::tools::paint::MIN_RADIUS, crate::tools::paint::MAX_RADIUS)));
        } else {
            cx.action(crate::ui::stage::StageAction::PaintRadius((value * 0.5).clamp(crate::tools::paint::MIN_RADIUS, crate::tools::paint::MAX_RADIUS)));
        }
    }
}

impl FabBrushDial {
    /// The Force strip's dial: a drag scales the size (up grows it), a wheel notch by a tenth.
    fn force_event(&mut self, cx: &mut Cx, event: &Event, state: &AppState, value: f64) {
        let sized = force_target(state).0.sizes().is_some();
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(_) => {
                self.hover = true;
                if sized {
                    cx.set_cursor(MouseCursor::Hand);
                }
                self.area.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hover = false;
                self.area.redraw(cx);
            }
            Hit::FingerDown(fe) if sized => {
                self.drag = Some((fe.abs.y, value));
                self.area.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                if let Some((y0, v0)) = self.drag {
                    self.set_force(cx, state, v0 * ((y0 - fe.abs.y) * 0.03).exp(), false);
                    self.area.redraw(cx);
                }
            }
            Hit::FingerUp(_) => {
                self.drag = None;
                if let Some(v) = self.dragged {
                    self.set_force(cx, state, v, true);
                }
                self.area.redraw(cx);
            }
            Hit::FingerScroll(fe) if sized => {
                let notches = (-fe.scroll.y / 40.0).clamp(-3.0, 3.0);
                self.set_force(cx, state, value * 1.1f64.powf(notches), true);
                event.set_scroll_handled(Vec2Index::X);
                event.set_scroll_handled(Vec2Index::Y);
                self.area.redraw(cx);
            }
            _ => {}
        }
    }
}

impl Widget for FabBrushDial {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, if self.force { "Load size" } else if self.strength { "Sculpt strength" } else if self.angle { "Smooth fill angle" } else { "Brush diameter" });
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get::<AppState>() else { return };
        let value = self.value(state);
        if self.force {
            self.force_event(cx, event, state, value);
            return;
        }
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(_) => {
                self.hover = true;
                cx.set_cursor(MouseCursor::Hand);
                self.area.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hover = false;
                self.area.redraw(cx);
            }
            Hit::FingerDown(fe) => {
                self.drag = Some((fe.abs.y, value));
                cx.set_cursor(MouseCursor::Hand);
                self.area.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                if let Some((y0, v0)) = self.drag {
                    let up = y0 - fe.abs.y;
                    self.set(cx, if self.strength { v0 + up * 0.01 } else if self.angle { v0 + up } else { v0 * (up * 0.03).exp() });
                    self.area.redraw(cx);
                }
            }
            Hit::FingerUp(_) => {
                self.drag = None;
                self.area.redraw(cx);
            }
            Hit::FingerScroll(fe) => {
                // Scroll up (negative) grows it, the way Shift+scroll sizes the brush in the view.
                let notches = (-fe.scroll.y / 40.0).clamp(-3.0, 3.0);
                self.set(cx, if self.strength { value + notches * 0.05 } else if self.angle { value + notches * 2.0 } else { value * 1.1f64.powf(notches) });
                // Consume the wheel even at a limit so the viewport cannot zoom.
                event.set_scroll_handled(Vec2Index::X);
                event.set_scroll_handled(Vec2Index::Y);
                self.area.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible { return DrawStep::done(); }
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let value = self.value(state);
        let rect = cx.walk_turtle(walk);
        let hot = self.hover || self.drag.is_some();
        let well = Rect { pos: rect.pos, size: dvec2(rect.size.x, 30.0) };
        self.draw_chip.spool = 0.0;
        let force = self.force.then(|| force_target(state).0);
        if let Some(kind) = force {
            let t = kind.sizes().map_or(0.0, |(lo, hi)| ((value / lo).max(1.0).ln() / (hi / lo).ln()).clamp(0.0, 1.0));
            self.draw_chip.disc = (5.0 + t * 21.0) as f32;
            self.draw_chip.wedge = 0.0;
            self.draw_chip.fill = if kind.sizes().is_some() { vec4(0.8, 0.8, 0.85, 1.0) } else { vec4(0.4, 0.4, 0.45, 1.0) };
            self.draw_chip.ring = if hot { vec4(1.0, 1.0, 1.0, 1.0) } else { vec4(0.0, 0.0, 0.0, 0.55) };
        } else if self.angle {
            self.draw_chip.disc = 1.0;
            self.draw_chip.wedge = value.to_radians() as f32;
            self.draw_chip.fill = if hot { vec4(1.0, 1.0, 1.0, 1.0) } else { vec4(0.8, 0.8, 0.85, 1.0) };
        } else {
            let t = if self.strength { ((value - 0.1) / 0.9).clamp(0.0, 1.0) } else { ((value / DIAL_MIN_MM).max(1.0).ln() / (DIAL_MAX_MM / DIAL_MIN_MM).ln()).clamp(0.0, 1.0) };
            // A support brush shows the tint it paints, not the colour.
            let [r, g, b] = if self.sculpt { [0.8, 0.8, 0.85] } else { crate::ui::popover::display_rgb(state.ui.paint_target.tint().unwrap_or(state.ui.paint_color)) };
            self.draw_chip.disc = (5.0 + t * 21.0) as f32;
            self.draw_chip.wedge = 0.0;
            self.draw_chip.fill = vec4(r, g, b, 1.0);
            self.draw_chip.ring = if hot { vec4(1.0, 1.0, 1.0, 1.0) } else { vec4(0.0, 0.0, 0.0, 0.55) };
        }
        self.draw_chip.draw_abs(cx, well);
        self.draw_chip.disc = 0.0;
        self.draw_chip.wedge = 0.0;
        let text = if let Some(kind) = force {
            match kind.sizes() {
                None => "-".to_string(),
                Some(_) if hot => format!("{}{}", crate::ui::force::size_text(value), kind.unit().replace(' ', "")),
                Some(_) => crate::ui::force::size_text(value),
            }
        } else if self.strength {
            format!("{value:.2}")
        } else if self.angle {
            format!("{value:.0}\u{b0}")
        } else if hot {
            format!("{}mm", crate::tools::paint::diameter_text(value))
        } else {
            crate::tools::paint::diameter_text(value)
        };
        let tw = self.draw_text.prepare_single_line_run(cx, &text).map(|r| r.width_in_lpxs as f64).unwrap_or(16.0);
        self.draw_text.color = if hot { vec4(1.0, 1.0, 1.0, 1.0) } else { vec4(0.62, 0.62, 0.68, 1.0) };
        self.draw_text.draw_abs(cx, dvec2(rect.pos.x + (rect.size.x - tw) * 0.5, rect.pos.y + 31.0), &text);
        cx.add_rect_area(&mut self.area, rect);
        DrawStep::done()
    }
}

/// One of the Pattern fill's settings in the paint strip, by `which`: 0 its kind (a click for
/// the next), 1 its size, 2 its seed, 3 its sharpness, 4 its balance (each dragged up or down,
/// or scrolled). The value sits under it.
#[derive(Script, ScriptHook, Widget)]
pub struct FabPatternDial {
    #[uid] uid: WidgetUid,
    #[walk] walk: Walk,
    #[live] #[redraw] draw_chip: DrawFabChip,
    #[live] draw_text: DrawText,
    #[live] which: usize,
    #[live(true)] #[visible] visible: bool,
    /// Where a press began and the pattern then, and whether it has moved off a click.
    #[rust] drag: Option<(f64, PaintPattern, bool)>,
    #[rust] hover: bool,
    #[rust] area: Area,
}

impl FabPatternDial {
    /// The pattern with this dial's setting moved by `up` points of drag (or a tenth of a wheel
    /// notch per point) from `from`.
    fn moved(&self, from: PaintPattern, up: f64) -> PaintPattern {
        let mut p = from;
        match self.which {
            1 => p.scale = from.scale * (up * 0.03).exp(),
            2 => p.seed = (from.seed as f64 + (up / 8.0).round()).clamp(1.0, PaintPattern::SEEDS as f64) as u32,
            3 => p.sharp = from.sharp + up * 0.01,
            4 => p.balance = from.balance + up * 0.01,
            _ => {}
        }
        p.held()
    }

    fn label(&self, p: &PaintPattern, hot: bool) -> String {
        match self.which {
            0 => p.kind.label().to_string(),
            1 if hot => format!("{}mm", crate::tools::paint::diameter_text(p.scale)),
            1 => crate::tools::paint::diameter_text(p.scale),
            2 => format!("#{}", p.seed),
            3 => format!("{:.2}", p.sharp),
            _ => format!("{:+.2}", p.balance),
        }
    }
}

impl Widget for FabPatternDial {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, ["Pattern kind", "Pattern size", "Pattern seed", "Pattern sharpness", "Pattern balance"][self.which.min(4)]);
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get::<AppState>() else { return };
        let pattern = state.ui.paint_pattern;
        let set = |cx: &mut Cx, p: PaintPattern| cx.action(crate::ui::stage::StageAction::PaintPattern(p));
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(_) => {
                self.hover = true;
                cx.set_cursor(MouseCursor::Hand);
                self.area.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hover = false;
                self.area.redraw(cx);
            }
            Hit::FingerDown(fe) => {
                self.drag = Some((fe.abs.y, pattern, false));
                self.area.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                if let Some((y0, from, moved)) = self.drag {
                    let up = y0 - fe.abs.y;
                    let moved = moved || up.abs() > 3.0;
                    self.drag = Some((y0, from, moved));
                    if moved && self.which != 0 {
                        set(cx, self.moved(from, up));
                    }
                    self.area.redraw(cx);
                }
            }
            Hit::FingerUp(_) => {
                // the kind steps on a click
                if let Some((_, from, false)) = self.drag {
                    if self.which == 0 {
                        let kind = from.kind.next();
                        set(cx, PaintPattern { kind, sharp: kind.sharp(), ..from });
                    }
                }
                self.drag = None;
                self.area.redraw(cx);
            }
            Hit::FingerScroll(fe) if self.which != 0 => {
                let notches = (-fe.scroll.y / 40.0).clamp(-3.0, 3.0);
                set(cx, self.moved(pattern, notches * if self.which == 2 { 8.0 } else { 3.2 }));
                event.set_scroll_handled(Vec2Index::X);
                event.set_scroll_handled(Vec2Index::Y);
                self.area.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible { return DrawStep::done(); }
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let p = state.ui.paint_pattern;
        let rect = cx.walk_turtle(walk);
        let hot = self.hover || self.drag.is_some();
        let well = Rect { pos: rect.pos, size: dvec2(rect.size.x, 30.0) };
        // how far along its range the setting is, drawn as the disc's size
        let t = match self.which {
            1 => (p.scale / PaintPattern::SCALE.0).ln() / (PaintPattern::SCALE.1 / PaintPattern::SCALE.0).ln(),
            2 => p.seed as f64 / PaintPattern::SEEDS as f64,
            3 => p.sharp,
            4 => 0.5 + 0.5 * p.balance,
            _ => 0.5,
        };
        let [r, g, b] = if self.which == 0 { crate::ui::popover::display_rgb(state.ui.paint_color) } else { [0.8, 0.8, 0.85] };
        self.draw_chip.spool = 0.0;
        self.draw_chip.wedge = 0.0;
        self.draw_chip.disc = (5.0 + t.clamp(0.0, 1.0) * 21.0) as f32;
        self.draw_chip.fill = vec4(r, g, b, 1.0);
        self.draw_chip.ring = if hot { vec4(1.0, 1.0, 1.0, 1.0) } else { vec4(0.0, 0.0, 0.0, 0.55) };
        self.draw_chip.draw_abs(cx, well);
        self.draw_chip.disc = 0.0;
        let text = self.label(&p, hot);
        let tw = self.draw_text.prepare_single_line_run(cx, &text).map(|r| r.width_in_lpxs as f64).unwrap_or(16.0);
        self.draw_text.color = if hot { vec4(1.0, 1.0, 1.0, 1.0) } else { vec4(0.62, 0.62, 0.68, 1.0) };
        self.draw_text.draw_abs(cx, dvec2(rect.pos.x + (rect.size.x - tw) * 0.5, rect.pos.y + 31.0), &text);
        cx.add_rect_area(&mut self.area, rect);
        DrawStep::done()
    }
}

// ====================================================================// Part properties
// ====================================================================
#[derive(Script, ScriptHook, Widget)]
pub struct FabPartPanel {
    #[deref]
    view: View,
    /// What the position fields measure from: the part's origin, its footprint's centre or corner.
    #[rust]
    anchor: PositionAnchor,
    #[rust]
    position_shown: Option<(usize, u64, PositionAnchor, Pose)>,
    #[rust]
    shown_xy: [f64; 2],
    /// What `rest` shows besides its setters' values, as it last recorded: the part and its
    /// plate (which of its sections and rows show, its painted colours) and the print colours
    /// (the colour row and the trays read them as they draw).
    #[rust]
    rest_shown: Option<(String, u64, crate::print::colours::Mapping)>,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum PositionAnchor {
    #[default]
    Origin,
    Centre,
    Corner,
}

impl PositionAnchor {
    fn xy(self, part: &PlatePart) -> [f64; 2] {
        let [x, y, _] = match self {
            Self::Origin => part.origin,
            Self::Centre => part.center(),
            Self::Corner => part.min_mm,
        };
        [x, y]
    }

    /// The origin move for an edit to one of the panel's two position fields.
    pub fn place_field(self, part: &PlatePart, axis: usize, value: f64) -> PartAction {
        let mut at = [part.origin[0], part.origin[1]];
        at[axis] = value + (at[axis] - self.xy(part)[axis]);
        PartAction::Place { object: part.object.clone(), at, rotate: part.rotate }
    }
}

/// The scale an edit of the Scale field of `axis` (0 X, 1 Y, 2 Z) to `percent` sets, from
/// `now`: with the lock, all three by the same factor (a uniform scale stays uniform); without
/// it, that axis alone. Each factor kept within 0.01..=10.
fn scale_by_field(now: [f64; 3], axis: usize, percent: f64, locked: bool) -> [f64; 3] {
    if locked {
        let f = (percent / 100.0) / now[axis].max(1e-6);
        now.map(|k| (k * f).clamp(0.01, 10.0))
    } else {
        let mut new = now;
        new[axis] = (percent / 100.0).clamp(0.01, 10.0);
        new
    }
}

impl Widget for FabPartPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Part");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::Actions(actions) = event {
            if self.view.text_input(cx, ids!(editor_bar.search.row.input)).changed(actions).is_some() {
                self.view.redraw(cx);
            }
        }
        // a section held by its grip moves with the pointer, the others sliding aside
        // (`section_reorder`): first, and with its animation's frames too
        if matches!(event, Event::MouseDown(_) | Event::MouseMove(_) | Event::MouseUp(_) | Event::NextFrame(_)) {
            let order: Vec<(LiveId, WidgetRef, WidgetRef)> = [(live_id!(transform_slot), live_id!(transform)), (live_id!(params_slot), live_id!(params)), (live_id!(colours_slot), live_id!(colours)), (live_id!(issues_slot), live_id!(issues))]
                .into_iter().map(|(slot, id)| (slot, self.view.widget(cx, &[id]), self.view.widget(cx, &[slot]))).collect();
            crate::ui::widgets::section_reorder(cx, event, live_id!(part_sections), &order, self.view.area());
        }
        // the headers first: a press reads the fold it is about to turn
        if matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::MouseUp(_) | Event::MouseLeave(_) | Event::ClearHover) {
            let sections = [ids!(none), ids!(transform), ids!(params), ids!(colours), ids!(issues)].map(|path| self.view.widget(cx, path));
            crate::ui::widgets::section_headers(cx, event, &sections);
        }
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        if !state.ui.on_screen(crate::ui::editors::Editor::Part) && !state.ui.part_side_shown {
            // off screen: none of its controls can be pressed
            return;
        }
        let Some(index) = active_part(state) else { return };
        let p = &state.plate.parts[index];
        let object = &p.object;
        let v = &self.view;
        if state.slice_blocks_edits() { return; }
        if v.radio_button(cx, ids!(body.transform.rs.scale.trail.lock)).clicked(actions) {
            state.ui.scale_locked = !state.ui.scale_locked;
            crate::ui::redraw(cx, &[ids!(tab_part)]);
        }
        if p.imported_mesh && v.button(cx, ids!(body.transform.buttons.make_printable)).clicked(actions) {
            cx.action(PartAction::MakePrintable { object: object.clone() });
        }
        let o = p.origin;
        let x = v.value_input(cx, ids!(body.transform.xy.value.num));
        let y = v.value_input(cx, ids!(body.transform.xy.value.num_y));
        let scales = [
            v.value_input(cx, ids!(body.transform.rs.scale.value.num)),
            v.value_input(cx, ids!(body.transform.rs.scale.value.num_y)),
            v.value_input(cx, ids!(body.transform.rs.scale.value.num_z)),
        ];
        // While a number scrubs, the part follows through the placement preview.
        let about = mm_to_world([o[0], o[1], p.min_mm[2]]);
        let zero = vec3(0.0, 0.0, 0.0);
        let pose = if let Some(nx) = x.changed(actions) {
            Some(pose_turn_shift(zero, 0.0, mm_to_world([nx - self.anchor.xy(p)[0], 0.0, 0.0])))
        } else if let Some(ny) = y.changed(actions) {
            Some(pose_turn_shift(zero, 0.0, mm_to_world([0.0, ny - self.anchor.xy(p)[1], 0.0])))
        } else if let Some(r) = v.value_input(cx, ids!(body.transform.rs.rot.value.num)).changed(actions) {
            Some(pose_turn_shift(about, (r - p.rotate) as f32, zero))
        } else {
            scales.iter().enumerate().find_map(|(axis, field)| {
                let value = field.changed(actions)?;
                let new = scale_by_field(p.scale, axis, value, state.ui.scale_locked);
                Some(pose_stretch_about(about, &part_axes(p.turn), std::array::from_fn(|i| (new[i] / p.scale[i].max(1e-6)) as f32)))
            })
        };
        if let (Some(pose), Some(e)) = (pose, p.element) {
            state.scene_state.set_nudge(Some((e, pose)));
            // The nudge appears in the viewport and these placement controls.
            crate::ui::redraw(cx, &[ids!(tab_viewport), ids!(tab_part)]);
        }
        let p = &state.plate.parts[index];
        if let Some(nx) = x.ended(actions) {
            cx.action(self.anchor.place_field(p, 0, nx));
        }
        if let Some(ny) = y.ended(actions) {
            cx.action(self.anchor.place_field(p, 1, ny));
        }
        if let Some(choice) = v.drop_down(cx, ids!(body.transform.anchor)).changed(actions) {
            self.anchor = match choice { 1 => PositionAnchor::Centre, 2 => PositionAnchor::Corner, _ => PositionAnchor::Origin };
            // A footprint may extend beyond the origin field's range. Equal limits give
            // ValueInput an unbounded coordinate with the same scrub step and snap.
            for field in [x, y] {
                if let Some(mut f) = field.borrow_mut() {
                    f.max = if self.anchor == PositionAnchor::Origin { ORIGIN_FIELD_MAX } else { f.min };
                }
            }
            crate::ui::redraw(cx, &[ids!(tab_part)]);
        }
        if let Some(r) = v.value_input(cx, ids!(body.transform.rs.rot.value.num)).ended(actions) {
            cx.action(PartAction::Place { object: object.clone(), at: [o[0], o[1]], rotate: r });
        }
        for (axis, field) in scales.iter().enumerate() {
            if let Some(value) = field.ended(actions) {
                cx.action(PartAction::Scale { object: object.clone(), scale: scale_by_field(p.scale, axis, value, state.ui.scale_locked) });
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // the sections in the order they were dragged into
        let rest = self.view.widget(cx, ids!(rest));
        crate::ui::widgets::apply_section_order(cx, live_id!(part_sections), &rest);
        if let Some(state) = scope.data.get::<AppState>() {
            // As it shows now: a part being placed reads where it is, not where it was.
            let part = active_part(state).map(|i| (i, shown_part(state, &state.plate.parts[i])));
            self.view.widget(cx, ids!(none)).set_visible(cx, part.is_none());
            if part.is_none() {
                self.view.label(cx, ids!(none.plate_summary)).set_text(cx, &plate_summary(state));
            }
            // (a widget, not a View: the body is a scroll box with a shadow, `FabScroll`)
            self.view.widget(cx, ids!(body)).set_visible(cx, part.is_some());
            if let Some((index, p)) = part {
                let v = &self.view;
                v.button(cx, ids!(body.transform.buttons.make_printable)).set_visible(cx, p.imported_mesh);
                let can_edit = !state.slice_blocks_edits();
                for id in [ids!(body.transform.xy), ids!(body.transform.rs), ids!(body.transform.anchor_row)] {
                    v.widget(cx, id).set_visible(cx, can_edit);
                }
                let position = (index, state.plate.revision, self.anchor, state.scene_state.nudge_of(p.element));
                if self.position_shown != Some(position) {
                    self.shown_xy = self.anchor.xy(&p);
                    self.position_shown = Some(position);
                }
                // Each setter leaves a field being edited alone and takes the value on release.
                v.value_input(cx, ids!(body.transform.xy.value.num)).set_value(cx, self.shown_xy[0]);
                v.value_input(cx, ids!(body.transform.xy.value.num_y)).set_value(cx, self.shown_xy[1]);
                v.value_input(cx, ids!(body.transform.rs.rot.value.num)).set_value(cx, p.rotate);
                v.value_input(cx, ids!(body.transform.rs.scale.value.num)).set_value(cx, p.scale[0] * 100.0);
                v.value_input(cx, ids!(body.transform.rs.scale.value.num_y)).set_value(cx, p.scale[1] * 100.0);
                v.value_input(cx, ids!(body.transform.rs.scale.value.num_z)).set_value(cx, p.scale[2] * 100.0);
                let lock = v.radio_button(cx, ids!(body.transform.rs.scale.trail.lock));
                if lock.active(cx) != state.ui.scale_locked {
                    lock.set_active(cx, state.ui.scale_locked, Animate::No);
                }
                let mut issues = p.issues.clone();
                if p.blocking && issues.is_empty() {
                    issues.push("Off the printable volume or out of a nozzle's reach.".into());
                }
                v.widget(cx, ids!(body.issues)).set_visible(cx, !issues.is_empty());
                v.label(cx, ids!(body.issues.list)).set_text(cx, &issues.join("\n"));
                let same = self.rest_shown.as_ref().is_some_and(|(object, revision, colors)| *object == p.object && *revision == state.plate.revision && *colors == state.print.colors);
                if !same {
                    self.rest_shown = Some((p.object.clone(), state.plate.revision, state.print.colors.clone()));
                    v.view(cx, ids!(body.rest)).redraw(cx);
                }
            }
        }
        // the toolbar's search filters the sections' rows (`filter_sections`)
        let query = self.view.text_input(cx, ids!(editor_bar.search.row.input)).text();
        let sections = [ids!(transform), ids!(params), ids!(colours), ids!(issues)].map(|path| self.view.widget(cx, path));
        crate::ui::widgets::filter_sections(cx, live_id!(part_sections), &query, &sections);
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    /// The Part panel's Scale: locked, one axis's edit scales all three by its factor (a uniform
    /// scale stays uniform, a stretched one keeps its proportions); unlocked, that axis alone.
    #[test]
    fn a_scale_field_scales_one_axis_or_all_three_by_the_lock() {
        assert_eq!(scale_by_field([1.0; 3], 0, 150.0, true), [1.5; 3], "locked uniform scale stays uniform");
        assert_eq!(scale_by_field([1.0, 0.5, 2.0], 1, 100.0, true), [2.0, 1.0, 4.0], "locked stretched scale keeps its proportions");
        assert_eq!(scale_by_field([1.0; 3], 2, 50.0, false), [1.0, 1.0, 0.5], "unlocked scale changes only the edited axis");
        assert_eq!(scale_by_field([1.0, 1.0, 9.0], 0, 200.0, true), [2.0, 2.0, 10.0], "locked scale clamps each factor");
    }

    #[test]
    fn delete_menu_dispatches_a_whole_selection_as_one_action_even_under_a_slice() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut state = AppState::default();
        state.print.play.active = true;
        state.ui.enter(Workspace::Monitor);
        let objects = vec!["bracket/body".into(), "bracket~1/body".into()];
        // Keep the action visible to the app so Monitor can explain the restriction.
        let actions = cx.capture_actions(|cx| {
            assert!(part_menu_action(cx, &state, &objects, live_id!(part_delete)));
        });
        let deletions: Vec<_> = actions.iter().filter_map(|a| a.downcast_ref::<PartAction>()).collect();
        assert_eq!(deletions, [&PartAction::DeleteParts { objects }]);
    }


    fn part(script: &str, link: u32, name: &str) -> PlatePart {
        let instance = if link == 0 { script.to_string() } else { format!("{script}~{link}") };
        PlatePart { printed: true, object: format!("{instance}/body"), script: script.into(), link, file: format!("{script}.cad.splash"), name: name.into(), ..Default::default() }
    }

    #[test]
    fn the_tree_keeps_the_declared_order_whatever_prints() {
        // the plate lists its printed parts first: "wheel", switched off, comes after "body"
        let mut wheel = part("wheel", 0, "Wheel");
        wheel.order = (0, 0);
        wheel.printed = false;
        let mut body = part("body", 0, "Body");
        body.order = (1, 0);
        let plate = PlateInfo { parts: vec![body, wheel], ..Default::default() };
        let names: Vec<String> = tree_rows(&plate, &[]).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["Wheel", "Body"], "declared order, not printed first");
    }

    #[test]
    fn row_eye_dispatches_print_for_one_clone_or_the_whole_script() {
        let mut plate = PlateInfo { parts: vec![part("bracket", 0, "Base"), part("bracket", 1, "Clone")], ..Default::default() };
        plate.parts[1].printed = false;
        let rows = tree_rows(&plate, &[]);
        assert_eq!(rows[1].eye_action(&plate), Some(PartAction::SetPrint { objects: vec!["bracket/body".into()], print: false }));
        assert_eq!(rows[2].eye_action(&plate), Some(PartAction::SetPrint { objects: vec!["bracket~1/body".into()], print: true }));
        assert_eq!(rows[0].eye_action(&plate), Some(PartAction::SetPrint { objects: vec!["bracket/body".into(), "bracket~1/body".into()], print: false }));
        plate.parts[0].printed = false;
        assert_eq!(rows[0].eye_action(&plate), Some(PartAction::SetPrint { objects: vec!["bracket/body".into(), "bracket~1/body".into()], print: true }));
    }

    /// A board row (its part linked `board:<name>`) never prints: undimmed, said so, and its eye
    /// shows or hides it; the script row's eye prints the case alone; View › Show Board reads it.
    #[test]
    fn a_board_rows_eye_shows_or_hides_it_and_never_prints_it() {
        let case = part("box", 0, "Case");
        let mut board = part("box", 1, "Board");
        board.printed = false;
        board.max_mm = [40.0, 74.0, 4.8];
        board.linked = Some(LinkedPlace { key: "board:demo".into(), plate_from_frame: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]] });
        let mut state = AppState::default();
        state.plate = PlateInfo { parts: vec![case, board], ..Default::default() };
        let rows = tree_rows(&state.plate, &[]);
        let row = &rows[2];
        assert!(!row.excluded(&state.plate), "a board is not dimmed as left out of the print");
        assert_eq!(row.detail_text(&state.plate), "Board, not printed · 40.0 × 74.0 × 4.8 mm", "the board says it is not printed");
        assert_eq!(row.eye_state(&state.plate), Some(true), "a shown board's eye is open");
        assert_eq!(row.eye_action(&state.plate), Some(PartAction::SetHidden { objects: vec!["box~1/body".into()], hidden: true }), "its eye hides it");
        assert_eq!(rows[0].eye_action(&state.plate), Some(PartAction::SetPrint { objects: vec!["box/body".into()], print: false }), "the script row's eye prints the case alone");
        assert_eq!(state.board_shown(), Some(true), "Show Board is on");
        state.plate.parts[1].hidden = true;
        assert_eq!(row.eye_state(&state.plate), Some(false), "a hidden board's eye is shut");
        assert_eq!(row.eye_action(&state.plate), Some(PartAction::SetHidden { objects: vec!["box~1/body".into()], hidden: false }), "its eye shows it again");
        assert_eq!(state.board_shown(), Some(false), "Show Board is off");
        state.plate.parts.pop();
        assert_eq!(state.board_shown(), None, "a plate without a board has no Show Board");
    }

    #[test]
    fn an_insert_row_rides_in_its_host_and_has_no_eye() {
        let host = part("holder", 0, "Holder");
        let mut insert = part("holder", 1, "Nut");
        insert.printed = false;
        insert.insert = true;
        insert.insert_into = host.object.clone();
        let mut plate = PlateInfo { parts: vec![host, insert], ..Default::default() };
        let rows = tree_rows(&plate, &[]);
        let row = &rows[2];
        assert_eq!(row.detail_text(&plate), "Rides in Holder", "the insert names its host's display name");
        plate.parts[1].embed = true;
        assert_eq!(tree_rows(&plate, &[])[2].detail_text(&plate), "Printed into Holder", "an item printed into its host says so");
        plate.parts[1].embed = false;
        plate.parts[1].melt = "M3".into();
        assert_eq!(tree_rows(&plate, &[])[2].detail_text(&plate), "M3 insert, melted into Holder after the print", "a heat-set insert says when it goes in");
        plate.parts[1].melt.clear();
        assert!(!row.excluded(&plate), "the insert is neither dimmed nor described as excluded");
        assert_eq!(row.eye_state(&plate), None, "the insert has no eye control");
        assert_eq!(row.eye_action(&plate), None, "the insert cannot toggle printing");
        assert_eq!(rows[0].eye_action(&plate), Some(PartAction::SetPrint { objects: vec!["holder/body".into()], print: false }), "the group eye skips the insert");
        plate.parts[0].printed = false;
        assert!(rows[1].excluded(&plate), "an ordinary hidden part still dims");
        assert!(!rows[0].excluded(&plate), "a group with a visible insert stays undimmed");
        assert_eq!(rows[0].eye_state(&plate), Some(false), "the group eye reflects only its toggleable parts");
        assert_eq!(rows[0].eye_action(&plate), Some(PartAction::SetPrint { objects: vec!["holder/body".into()], print: true }), "the group eye restores only its hidden part");
    }

    #[test]
    fn single_instance_stays_flat() {
        let plate = PlateInfo { parts: vec![part("bracket", 0, "Bracket")], ..Default::default() };
        let rows = tree_rows(&plate, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Bracket");
        assert_eq!(rows[0].parts, vec![0]);
        assert_eq!(rows[0].parent, None);
        assert_eq!(rows[0].instances, 0);
    }

    #[test]
    fn linked_placements_nest_under_their_script() {
        // A clone can precede the base in the plate; node rename still targets the base.
        let plate = PlateInfo { parts: vec![part("bracket", 2, "Bracket 3"), part("bracket", 0, "Bracket")], ..Default::default() };
        let rows = tree_rows(&plate, &[]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "bracket");
        assert_eq!(rows[0].instances, 2);
        assert_eq!(rows[0].parts, vec![0, 1]);
        assert_eq!(rows[0].rename, 1);
        assert_eq!(rows[1].parent, Some(rows[0].key));
        assert_eq!(rows[2].parent, Some(rows[0].key));
        assert_eq!(rows[1].parts, vec![0]);
        assert_eq!(rows[2].parts, vec![1]);
        assert_eq!((rows[1].pos_in_set, rows[2].pos_in_set, rows[2].set_size), (1, 2, 2));
    }

    #[test]
    fn an_independent_copy_is_its_own_root() {
        let plate = PlateInfo { parts: vec![part("bracket", 0, "Bracket"), part("bracket_2", 0, "Bracket copy"), part("bracket", 1, "Bracket 2")], ..Default::default() };
        let rows = tree_rows(&plate, &[]);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].parts, vec![0, 2]);
        assert_eq!(rows[3].name, "Bracket copy");
        assert_eq!(rows[3].parent, None);
        assert_eq!(rows[3].parts, vec![1]);
        assert_eq!(rows[0].set_size, 2);
    }

    #[test]
    fn ordering_and_identity_survive_rename_and_unrelated_insertions() {
        let mut plate = PlateInfo { parts: vec![part("z", 0, "Zed"), part("a", 0, "A"), part("z", 1, "Zed 2")], ..Default::default() };
        let before = tree_rows(&plate, &[]);
        assert_eq!(before.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["z", "Zed", "Zed 2", "A"]);
        assert_eq!(tree_rows(&plate, &[]), before);
        plate.parts[0].name = "Renamed".into();
        plate.parts.insert(0, part("new", 0, "New"));
        let after = tree_rows(&plate, &[]);
        assert_eq!(after[1..].iter().map(|r| r.key).collect::<Vec<_>>(), before.iter().map(|r| r.key).collect::<Vec<_>>());
        assert_eq!(after[2].name, "Renamed");
        assert_eq!(after.iter().map(|r| r.key).collect::<HashSet<_>>().len(), after.len());
    }

    #[test]
    fn multiple_objects_in_one_placement_do_not_look_linked() {
        let mut other = part("assembly", 0, "Lid");
        other.object = "assembly/lid".into();
        let plate = PlateInfo { parts: vec![part("assembly", 0, "Box"), other], ..Default::default() };
        let rows = tree_rows(&plate, &[]);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.parent.is_none() && r.instances == 0));
    }

    /// A body of `instance`'s object "bust", which prints its bodies as one.
    fn body(script: &str, link: u32, solid: &str) -> PlatePart {
        let mut p = part(script, link, &format!("bust {solid}"));
        let instance = p.object.split('/').next().unwrap().to_string();
        p.object = format!("{instance}/{solid}");
        p.body_of = Some((format!("{instance}/bust"), "bust".into()));
        p
    }

    #[test]
    fn an_object_printing_its_bodies_as_one_holds_them_under_it() {
        let plate = PlateInfo { parts: vec![body("hair", 0, "head"), body("hair", 0, "lock"), body("hair", 0, "other"), part("bracket", 0, "Bracket")], ..Default::default() };
        let rows = tree_rows(&plate, &[]);
        let shown: Vec<(RowKind, &str, u32)> = rows.iter().map(|r| (r.kind, r.name.as_str(), depth(&rows, r))).collect();
        assert_eq!(shown, vec![(RowKind::Object, "bust", 0), (RowKind::Part, "bust head", 1), (RowKind::Part, "bust lock", 1), (RowKind::Part, "bust other", 1), (RowKind::Part, "Bracket", 0)]);
        let object = &rows[0];
        assert!(object.is_branch() && object.is_part() && !linked(&rows, object));
        assert_eq!((object.parts.clone(), object.rename, object.pos_in_set, object.set_size), (vec![0, 1, 2], 0, 1, 2));
        for (k, row) in rows[1..4].iter().enumerate() {
            assert_eq!((row.parent, row.parts.clone(), row.pos_in_set, row.set_size), (Some(object.key), vec![k], k as u32 + 1, 3), "body {k}");
            assert!(!linked(&rows, row));
        }
        // The object's eye switches all of its bodies, a body's that body.
        assert_eq!(object.eye_action(&plate), Some(PartAction::SetPrint { objects: vec!["hair/head".into(), "hair/lock".into(), "hair/other".into()], print: false }));
        assert_eq!(rows[2].eye_action(&plate), Some(PartAction::SetPrint { objects: vec!["hair/lock".into()], print: false }));
        // Folded, its bodies are not shown; the rest is.
        let collapsed = HashSet::from([object.key]);
        assert_eq!(rows.iter().map(|r| folded(&rows, &collapsed, r)).collect::<Vec<_>>(), vec![false, true, true, true, false]);
        assert_eq!(bodies_text(&plate.parts, &object.parts), "3 bodies as one · 0.0 × 0.0 × 0.0 mm");
    }

    #[test]
    fn linked_objects_of_bodies_nest_under_their_script() {
        let plate = PlateInfo { parts: vec![body("hair", 0, "head"), body("hair", 0, "lock"), body("hair", 1, "head"), body("hair", 1, "lock")], ..Default::default() };
        let rows = tree_rows(&plate, &[]);
        let shown: Vec<(RowKind, u32)> = rows.iter().map(|r| (r.kind, depth(&rows, r))).collect();
        assert_eq!(shown, vec![(RowKind::Script, 0), (RowKind::Object, 1), (RowKind::Part, 2), (RowKind::Part, 2), (RowKind::Object, 1), (RowKind::Part, 2), (RowKind::Part, 2)]);
        assert_ne!(rows[1].key, rows[4].key, "each placement's object is a row of its own");
        assert_eq!((rows[1].parts.clone(), rows[4].parts.clone()), (vec![0, 1], vec![2, 3]));
        assert!(linked(&rows, &rows[1]) && !linked(&rows, &rows[2]), "the object is the linked placement, its bodies are its");
        // Folding the script hides both objects and their bodies.
        let collapsed = HashSet::from([rows[0].key]);
        assert!(rows[1..].iter().all(|r| folded(&rows, &collapsed, r)));
    }

    fn measurement(name: Option<&str>, kind: MeasureKind, label: &str) -> Measurement {
        Measurement {
            name: name.map(str::to_string),
            kind,
            points: Vec::new(),
            value: 0.0,
            label: label.into(),
            circle: None,
            gap: None,
            normals: Vec::new(),
        }
    }

    #[test]
    fn empty_measurements_do_not_add_a_group() {
        let plate = PlateInfo { parts: vec![part("bracket", 0, "Bracket")], ..Default::default() };
        assert_eq!(tree_rows(&plate, &[measurement_group(&[])]), tree_rows(&plate, &[]));
        assert!(tree_rows(&PlateInfo::default(), &[measurement_group(&[])]).is_empty());
    }

    #[test]
    fn measurements_follow_all_parts_with_their_values_and_kinds() {
        let plate = PlateInfo {
            parts: vec![part("bracket", 0, "Bracket"), part("bracket", 1, "Bracket 2"), part("copy", 0, "Copy")],
            ..Default::default()
        };
        let measurements = vec![
            measurement(Some("Width"), MeasureKind::Distance, "25.0 mm"),
            measurement(None, MeasureKind::Angle, "90.0°"),
        ];
        let parts = tree_rows(&plate, &[]);
        let rows = tree_rows(&plate, &[measurement_group(&measurements)]);
        assert_eq!(rows[..parts.len()].iter().map(|r| r.key).collect::<Vec<_>>(), parts.iter().map(|r| r.key).collect::<Vec<_>>());
        let group = &rows[parts.len()];
        assert_eq!(group.kind, RowKind::AnnotationGroup);
        assert_eq!(group.name, "Measurements");
        assert!(group.is_branch());
        assert_eq!((group.parent, group.pos_in_set, group.set_size), (None, 3, 3));
        let children = &rows[parts.len() + 1..];
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name, "Width");
        assert_eq!(children[0].detail, "Distance · 25.0 mm");
        assert_eq!(children[1].name, "90.0°");
        assert_eq!(children[1].detail, "Angle · 90.0°");
        for (index, row) in children.iter().enumerate() {
            assert_eq!(row.kind, RowKind::Measurement(index));
            assert_eq!(row.parent, Some(group.key));
            assert_eq!((row.pos_in_set, row.set_size), (index as u32 + 1, 2));
            assert!(row.parts.is_empty());
            assert!(!row.is_branch());
        }
    }

    #[test]
    fn annotation_groups_share_ordering_and_keep_row_identity_on_rename() {
        let mut measurements = vec![measurement(None, MeasureKind::Radius, "R 5.00 mm")];
        let first = tree_rows(&PlateInfo::default(), &[measurement_group(&measurements)]);
        measurements[0].name = Some("Bore".into());
        let mut group = measurement_group(&measurements);
        group.title = "Dimensions".into();
        let rows = tree_rows(&PlateInfo::default(), &[AnnotationGroup { title: "Empty".into(), rows: Vec::new() }, group]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Dimensions");
        assert_eq!(rows[0].set_size, 1);
        assert_eq!(rows[1].name, "Bore");
        assert_eq!(rows[1].key, first[1].key);
        assert_eq!(rows[1].parent, Some(rows[0].key));
    }

    #[test]
    fn measurement_rows_map_selection_and_delete_to_document_indices() {
        let mut state = AppState::default();
        let element = ElementId(7);
        let mut printed_part = part("bracket", 0, "Bracket");
        printed_part.element = Some(element);
        state.plate.parts.push(printed_part);
        state.measurements = vec![
            measurement(None, MeasureKind::Distance, "10.0 mm"),
            measurement(Some("Height"), MeasureKind::Distance, "20.0 mm"),
        ];
        let rows = tree_rows(&state.plate, &[measurement_group(&state.measurements)]);
        assert!(rows.iter().all(|row| !row.annotation_selected(&state)));
        for row in &rows[..2] {
            assert_eq!(row.measurement_action(RowCommand::Select), None);
            assert_eq!(row.measurement_action(RowCommand::Delete), None);
        }
        for (index, row) in rows[2..].iter().enumerate() {
            assert_eq!(row.measurement_action(RowCommand::Select), Some(MeasureAction::Select(index)));
            assert_eq!(row.measurement_action(RowCommand::Delete), Some(MeasureAction::Delete(index)));
        }
        state.apply_core(&ShellAction::SelectSet(vec![element]));
        let Some(MeasureAction::Select(index)) = rows[3].measurement_action(RowCommand::Select) else { panic!("measurement selection") };
        assert!(crate::tools::measure::select(&mut state, index));
        assert!(selected_parts(&state).is_empty());
        assert!(!rows[1].annotation_selected(&state));
        assert!(!rows[2].annotation_selected(&state));
        assert!(rows[3].annotation_selected(&state));
        state.apply_core(&ShellAction::SelectSet(vec![element]));
        assert_eq!(state.selected_measure, None);
        assert_eq!(selected_parts(&state), vec![0]);
    }
}
