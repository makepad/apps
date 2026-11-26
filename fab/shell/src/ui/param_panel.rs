//! The selected part's parameters as controls, generated from the parameter
//! model ([`crate::params`]): no panel is written per part.
//!
//! [`FabParamPanel`] lists the part's declared parameters first, then the
//! fields of every feature that builds it, grouped by object (groups fold;
//! opening one focuses its feature, so the view's handles show for it). A
//! row's control follows the parameter's kind: a drag number with text entry
//! and its unit for lengths, radii, angles, counts and ratios, a check box
//! for a switch, segment tabs for a choice, X/Y/Z numbers for a point, and
//! [`FabProfileEditor`] for a section or a revolve profile.
//!
//! Hovering a row lights up its handle and the boxes of what it shapes in
//! the view. Dragging previews live (`ParamAction::Preview`, coalesced by the
//! app); letting go or Enter commits one undo step.
//! A step that does not build holds the control at the last value that did and flashes it red.

use crate::api::*;
use crate::params::*;
use crate::ui::parts::active_part;
use makepad_widgets_core::widget_tree::CxWidgetExt;
use makepad_widgets_core::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// The longest a parameter row's label column is (points): the Part panel's Transform rows take
/// the same, so every field of the panel starts at one line. A longer label ends in an ellipsis.
pub const LABEL_W: f64 = 96.0;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabProfileEditorBase = #(FabProfileEditor::register_widget(vm))
    mod.widgets.FabProfileEditor = set_type_default() do mod.widgets.FabProfileEditorBase{
        width: Fill
        height: 150
        draw_bg: mod.draw.DrawColor{ color: #x0000002a }
        draw_line: mod.draw.DrawHandlePrim{}
        draw_text: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
    }

    mod.widgets.FabParamPanelBase = #(FabParamPanel::register_widget(vm))
    mod.widgets.FabParamPanel = set_type_default() do mod.widgets.FabParamPanelBase{
        width: Fill height: Fit flow: Down spacing: 0
        // A group sits on its section's card, its title under the section's title (past the
        // section's icon); its rows and the next group's title follow on the row rhythm.
        Group := PropSection{
            // a grip to move it among its part's groups (never out of them)
            grip: true
            // out over the enclosing section's right inset, so its fields end where the Part's
            // other fields do; its rows start where theirs do (inside the left inset, so a long
            // name's start is never cut at the panel's edge); the sections' gap above it, none
            // below: every bar stands that gap from the next, the Part's next section's too
            margin: Inset{right: 0.0 - theme.prop_pad top: theme.prop_section_gap * 2.0}
            // its bar indented a short level (10 in from the section's rows' start), its rows with it
            header +: {hdr +: {margin: Inset{left: 10.0} padding: Inset{left: theme.prop_label_gap right: theme.prop_pad}}}
            body: View{
                width: Fill height: Fit flow: Down
                // room above and below the rows (a step less than a section's: it is nested);
                // its rows start where its bar does (indented with it)
                padding: Inset{left: 10.0 top: 4 bottom: 6}
                content := View{width: Fill height: Fit flow: Down spacing: theme.prop_row_gap margin: Inset{}}
            }
        }
        // A number's name is inside its field, at its left, the value at its right
        // (`inline_axis`): the row keeps no label column. Every other row's label column is
        // `LABEL_W` wide.
        Num := FabNumberRow{label +: {visible: false width: 0} gap +: {width: 0} value +: {num +: {min: 0.0 max: 0.0 precision: 1}}}
        // A seed: its number, and a button that writes a new one.
        Seed := FabNumberRow{
            label +: {visible: false width: 0} gap +: {width: 0}
            value +: {num +: {min: 0.0 max: 0.0 precision: 0}}
            trail +: {width: Fit reseed := mod.widgets.FabFlatButton{text: "Reseed" a11y: {label: "Reseed"}}}
        }
        // A grown pattern's live step counter, with pause and resume while it grows.
        Growth := PropRow{
            label +: {width: #(LABEL_W)}
            value +: {
                flow: Right align: Align{y: 0.5}
                text := mod.widgets.FabLabelSmall{width: Fill text: "" max_lines: 1}
                pause := mod.widgets.FabFlatButton{text: "Pause" a11y: {label: "Pause growth"}}
            }
        }
        Bool := PropCheckRow{label +: {width: #(LABEL_W)}}
        Choice := PropSegmentedRow{label +: {width: #(LABEL_W)}}
        Pick := PropDropDownRow{label +: {width: #(LABEL_W)}}
        // A field's axes drawn as one row: its name, then the axes' fields side by side (taken from
        // their rows, which are not drawn; each axis named inside its field). See `axis_runs`.
        Axes := PropRow{label +: {visible: false width: 0} gap +: {width: 0} value +: {flow: Right spacing: 4}}
        // A point: one row, its axes side by side, each named inside its field (a 2D one hides Z).
        Point := PropRow{
            label +: {visible: false width: 0} gap +: {width: 0}
            value +: {
                flow: Right spacing: 4
                x := ValueInput{width: Fill height: Fill min_height: 0 label: "X" min: 0.0 max: 0.0 precision: 1}
                y := ValueInput{width: Fill height: Fill min_height: 0 label: "Y" min: 0.0 max: 0.0 precision: 1}
                z := ValueInput{width: Fill height: Fill min_height: 0 label: "Z" min: 0.0 max: 0.0 precision: 1}
            }
        }
        Profile := View{
            width: Fill height: Fit flow: Down spacing: 2
            label := mod.widgets.FabLabelDim{
                width: Fill text: ""
                margin: Inset{left: #(LABEL_W) + theme.prop_label_gap}
            }
            editor := mod.widgets.FabProfileEditor{}
        }
        Derived := PropRow{label +: {width: #(LABEL_W)} value +: {text := mod.widgets.FabLabelSmall{width: Fill text: ""}}}
        Hint := View{
            width: Fill height: Fit
            // (under the fields, which start at the row's left now their names are inside them)
            padding: Inset{left: 4 right: theme.prop_pad top: 4 bottom: 6}
            text := mod.widgets.FabLabelSmall{width: Fill text: ""}
        }
    }

}

// ===========================================================================
// Profile editor
// ===========================================================================

#[derive(Clone, Debug, Default)]
pub enum ProfileEditorAction {
    /// A point moved: the whole profile, live.
    Changed(Vec<[f64; 2]>),
    /// The drag ended.
    Ended(Vec<[f64; 2]>),
    #[default]
    None,
}

/// A 2D profile with draggable points, fitted into its box (equal scale on both axes).
/// Snaps points to half millimetres (Shift: tenths, Ctrl: free).
#[derive(Script, ScriptHook, Widget)]
pub struct FabProfileEditor {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_line: crate::tools::handles::DrawHandlePrim,
    #[live]
    draw_text: DrawText,
    #[rust]
    points: Vec<[f64; 2]>,
    #[rust]
    drag: Option<usize>,
    #[rust]
    hover: Option<usize>,
    /// Model to screen: (origin of model (0,0) in screen, points per mm). Fixed while dragging.
    #[rust]
    map: Option<(DVec2, f64)>,
}

impl FabProfileEditor {
    pub fn set_points(&mut self, cx: &mut Cx, points: &[[f64; 2]]) {
        if self.drag.is_none() && self.points != points {
            self.points = points.to_vec();
            self.draw_bg.redraw(cx);
        }
    }

    fn fit(&self, rect: Rect) -> (DVec2, f64) {
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for p in &self.points {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if self.points.is_empty() {
            return (rect.pos + rect.size * 0.5, 1.0);
        }
        let pad = 14.0;
        let w = (hi[0] - lo[0]).max(1e-3);
        let h = (hi[1] - lo[1]).max(1e-3);
        let s = ((rect.size.x - pad * 2.0) / w).min((rect.size.y - pad * 2.0) / h).max(1e-3);
        let cx = (lo[0] + hi[0]) * 0.5;
        let cy = (lo[1] + hi[1]) * 0.5;
        // Model Y up = screen up.
        (dvec2(rect.pos.x + rect.size.x * 0.5 - cx * s, rect.pos.y + rect.size.y * 0.5 + cy * s), s)
    }

    fn to_screen(map: (DVec2, f64), p: [f64; 2]) -> DVec2 {
        dvec2(map.0.x + p[0] * map.1, map.0.y - p[1] * map.1)
    }

    fn to_model(map: (DVec2, f64), s: DVec2) -> [f64; 2] {
        [(s.x - map.0.x) / map.1, (map.0.y - s.y) / map.1]
    }

    fn nearest(&self, map: (DVec2, f64), at: DVec2) -> Option<usize> {
        self.points.iter().enumerate().map(|(i, p)| (i, (Self::to_screen(map, *p) - at).length())).filter(|(_, d)| *d < 9.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
    }
}

impl Widget for FabProfileEditor {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let uid = self.widget_uid();
        let rect = self.draw_bg.area().rect(cx);
        let map = self.map.unwrap_or_else(|| self.fit(rect));
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverOver(fe) => {
                let h = self.nearest(map, fe.abs);
                if h != self.hover {
                    self.hover = h;
                    self.draw_bg.redraw(cx);
                }
                if h.is_some() {
                    cx.set_cursor(MouseCursor::Hand);
                }
            }
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                self.drag = self.nearest(map, fe.abs);
                self.map = Some(map);
            }
            Hit::FingerMove(fe) => {
                if let (Some(i), Some(map)) = (self.drag, self.map) {
                    let mut q = Self::to_model(map, fe.abs);
                    if !(fe.modifiers.control || fe.modifiers.logo) {
                        let inc = if fe.modifiers.shift { 0.1 } else { 0.5 };
                        q = q.map(|x| snap(x, inc));
                    }
                    if self.points.get(i) != Some(&q) {
                        self.points[i] = q;
                        cx.widget_action(uid, ProfileEditorAction::Changed(self.points.clone()));
                        self.draw_bg.redraw(cx);
                    }
                }
            }
            Hit::FingerUp(_) => {
                if self.drag.take().is_some() {
                    cx.widget_action(uid, ProfileEditorAction::Ended(self.points.clone()));
                }
                self.map = None;
                self.draw_bg.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        let map = self.map.unwrap_or_else(|| self.fit(rect));
        let n = self.points.len();
        let col = vec4(0.40, 0.90, 0.55, 1.0);
        for i in 0..n {
            let a = Self::to_screen(map, self.points[i]);
            let b = Self::to_screen(map, self.points[(i + 1) % n]);
            draw_seg(&mut self.draw_line, cx, a, b, 1.5, vec4(col.x, col.y, col.z, 0.8));
        }
        for i in 0..n {
            let s = Self::to_screen(map, self.points[i]);
            let hot = self.hover == Some(i) || self.drag == Some(i);
            let r = if hot { 5.0 } else { 3.5 };
            self.draw_line.kind = 2.0;
            self.draw_line.color = if hot { vec4(1.0, 1.0, 1.0, 1.0) } else { col };
            self.draw_line.draw_abs(cx, Rect { pos: s - dvec2(r, r), size: dvec2(r * 2.0, r * 2.0) });
        }
        if let Some(i) = self.drag.or(self.hover) {
            if let Some(p) = self.points.get(i) {
                let text = format!("{}: {:.1}, {:.1} mm", i + 1, p[0], p[1]);
                self.draw_text.draw_abs(cx, rect.pos + dvec2(6.0, 4.0), &text);
            }
        }
        DrawStep::done()
    }
}

fn draw_seg(prim: &mut crate::tools::handles::DrawHandlePrim, cx: &mut Cx2d, a: DVec2, b: DVec2, width: f64, color: Vec4f) {
    prim.set_segment(cx, a, b, width, color)
}

impl FabProfileEditorRef {
    pub fn changed(&self, actions: &Actions) -> Option<Vec<[f64; 2]>> {
        match actions.find_widget_action(self.widget_uid())?.cast() {
            ProfileEditorAction::Changed(p) => Some(p),
            _ => None,
        }
    }
    pub fn ended(&self, actions: &Actions) -> Option<Vec<[f64; 2]>> {
        match actions.find_widget_action(self.widget_uid())?.cast() {
            ProfileEditorAction::Ended(p) => Some(p),
            _ => None,
        }
    }
    pub fn set_points(&self, cx: &mut Cx, points: &[[f64; 2]]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_points(cx, points);
        }
    }
}

// ===========================================================================
// The panel
// ===========================================================================

/// What a row's controls show besides their value. A value preview replaces the parameters on
/// every frame of a drag while this stays the same: the controls are configured again (labels,
/// names, ranges: script applies) only when it changes.
#[derive(Clone, Debug, PartialEq)]
enum RowConfig {
    Group { title: String },
    Param { template: LiveId, kind: ParamKind, label: String, unit: String, range: [f64; 4], bounded: bool, reach: bool, choices: Vec<String>, points: usize },
}

impl RowConfig {
    fn of(p: &ParamView) -> Self {
        RowConfig::Param {
            template: template_for(p),
            kind: p.kind,
            label: p.label.clone(),
            unit: p.unit.clone(),
            range: Self::range(p),
            bounded: Self::bounded(p),
            reach: reach_only(p),
            choices: p.choices.clone(),
            points: Self::points(p),
        }
    }

    /// `p` configures the row as this: compared in place, since every row asks on every frame
    /// of a drag (`of` copies the strings).
    fn shows(&self, p: &ParamView) -> bool {
        let RowConfig::Param { template, kind, label, unit, range, bounded, reach, choices, points } = self else { return false };
        *template == template_for(p) && *kind == p.kind && *label == p.label && *unit == p.unit && *range == Self::range(p)
            && *bounded == Self::bounded(p) && *reach == reach_only(p) && *choices == p.choices && *points == Self::points(p)
    }

    fn shows_group(&self, group: &str) -> bool {
        matches!(self, RowConfig::Group { title } if title == group)
    }

    fn range(p: &ParamView) -> [f64; 4] {
        [p.min, p.max, p.step, snap_step(p, p.value.num().unwrap_or(1.0), false)]
    }

    fn bounded(p: &ParamView) -> bool {
        p.declared && p.kind.is_numeric() && p.max > p.min
    }

    fn points(p: &ParamView) -> usize {
        match &p.value { ParamValue::Point(c) => c.len(), _ => 3 }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum RowKind {
    Group(String),
    Param(String),
    Hint,
    /// The Mass and balance row's header.
    Mass,
}

/// A Num row's field as one axis of an [`Axes`] row (`axis` inside it), or in its own row (`None`).
/// Only what changed is set.
fn inline_axis(cx: &mut Cx, row: &WidgetRef, name: &str) {
    if let Some(mut num) = row.value_input(cx, ids!(num)).borrow_mut() {
        let want = name;
        if num.label != want {
            num.label = want.to_string();
            num.redraw(cx);
        }
    }
}

/// A Num row's field, reached through the row's own children rather than the widget tree (an
/// [`Axes`] row draws it, so the tree must keep it under its row; this puts it back there too).
fn row_num(cx: &mut Cx, row: &WidgetRef) -> Option<WidgetRef> {
    let find = |v: &View, name: LiveId| v.children.iter().find(|(id, _)| *id == name).map(|(_, c)| c.clone());
    // (a number row is a `PropRow`, its children its View's)
    let child = |w: &WidgetRef, name: LiveId| w.borrow::<View>().and_then(|v| find(&v, name))
        .or_else(|| w.borrow::<makepad_widgets_core::property::PropRow>().and_then(|r| find(&r, name)));
    let value = child(row, live_id!(value))?;
    let num = child(&value, live_id!(num))?;
    if row.widget(cx, ids!(num)).widget_uid() != num.widget_uid() {
        cx.widget_tree_insert_child(value.widget_uid(), live_id!(num), num.clone());
    }
    Some(num)
}

/// The Mass and balance row's title.
const MASS: &str = "Mass and balance";


#[derive(Script, Widget)]
pub struct FabParamPanel {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    /// The picked print settings volume's rows (its box and its settings) are this panel's
    /// alone: the Overrides editor's; the Parameters editor shows the rest, never a volume's.
    #[live]
    volumes: bool,
    #[rust]
    templates: HashMap<LiveId, ScriptObjectRef>,
    #[rust]
    items: ComponentMap<LiveId, (WidgetRef, LiveId)>,
    /// The key its groups' order is kept by (`widgets::section_reorder`): the shown part's, so
    /// a group moves among its own part's groups only. None until a part shows.
    #[rust]
    order_key: Option<LiveId>,
    /// The groups as last drawn, in order (their ids).
    #[rust]
    group_ids: Vec<LiveId>,
    /// The rows of the last draw: id, what, rect.
    #[rust]
    rows: Vec<(LiveId, RowKind, Rect)>,
    /// Open groups (by name): see [`open_declared`].
    #[rust]
    open: BTreeSet<String>,
    /// The declared groups the shown part has had: each opened once, then is the person's.
    #[rust]
    seen: BTreeSet<String>,
    /// What each row's controls were last configured with.
    #[rust]
    applied: HashMap<LiveId, RowConfig>,
    #[rust]
    capped_seen: HashMap<String, u64>,
    /// The choice rows drawn dimmed (inherited, or not in effect).
    #[rust]
    dimmed: HashSet<LiveId>,
    #[redraw]
    #[rust]
    area: Area,
    #[rust]
    shown_part: Option<String>,
    #[rust]
    step_rows: Option<Arc<StepRows>>,
    #[rust]
    tool_shown: bool,
    #[rust]
    pointer_inside: bool,
    /// The hover tooltip of a row's hint; never inline, so hovering moves nothing.
    #[rust]
    hint: PropHint,
    /// The names the shown part's boxes read (its declared parameters), by the part view they
    /// were taken from: built once per parameter model, not per frame.
    #[rust]
    names: Option<(usize, std::sync::Arc<[makepad_script::syntax::quantity::NamedValue]>)>,
    /// The editor's search (`widgets::filter_sections`), its words lowercased: while it holds
    /// any, only the rows whose group and label hold every word draw, their groups open.
    #[rust]
    filter: Vec<String>,
    /// What the search reads, as the last draw had it: each group's name and its rows' labels.
    #[rust]
    searchable: Vec<(String, Vec<String>)>,
}

impl ScriptHook for FabParamPanel {
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
                    let v: ScriptValue = template.as_object().into();
                    w.script_apply(vm, apply, scope, v);
                }
            }
            self.applied.clear();
            self.dimmed.clear();
        }
    }
}

fn row_id(kind: &RowKind) -> LiveId {
    match kind {
        RowKind::Group(g) => live_id!(group).str_append(g),
        RowKind::Param(k) => live_id!(param).str_append(k),
        RowKind::Hint => live_id!(param_hint),
        RowKind::Mass => live_id!(param_mass),
    }
}

fn template_for(p: &ParamView) -> LiveId {
    if p.kind == ParamKind::Growth {
        return live_id!(Growth);
    }
    if p.derived {
        return live_id!(Derived);
    }
    match p.kind {
        ParamKind::Bool => live_id!(Bool),
        ParamKind::Seed => live_id!(Seed),
        ParamKind::Choice if p.choices.len() <= 4 => live_id!(Choice),
        ParamKind::Choice => live_id!(Pick),
        ParamKind::Point => live_id!(Point),
        ParamKind::Profile => live_id!(Profile),
        _ => live_id!(Num),
    }
}

fn suffix(p: &ParamView) -> String {
    match p.unit.as_str() {
        "" => String::new(),
        "°" => "°".into(),
        u => format!(" {u}"),
    }
}

/// The group of an object's own fields (a feature's literal rows, as `inst/solid` names it):
/// the one that opens when it is focused or picked, and the feature opening it focuses. None
/// for an object with no rows of its own.
fn group_of<'a>(pp: &'a PartParamsView, object: &str) -> Option<&'a str> {
    pp.params.iter().find(|p| !p.declared && p.objects.iter().any(|o| o == object)).map(|p| p.group.as_str())
}

/// Bring the open groups up to a part's declared groups (repeats allowed): a new part starts
/// over, with "Parameters" and each of them open, and a declared group `seen` has not met opens
/// once, so one the AI adds to a part on show comes up open. Past that a group is the person's
/// to fold: a fold stays across draws and rebuilds, until another part is shown.
fn open_declared<'a>(new_part: bool, declared: impl Iterator<Item = &'a str>, seen: &mut BTreeSet<String>, open: &mut BTreeSet<String>) {
    if new_part {
        seen.clear();
        open.clear();
        open.insert("Parameters".into());
    }
    for group in declared {
        if !seen.contains(group) {
            seen.insert(group.to_string());
            open.insert(group.to_string());
        }
    }
}

/// Whether a row is a print settings volume's (its box or its settings): all it shapes is one.
fn in_volume(state: &AppState, p: &ParamView) -> bool {
    !p.objects.is_empty() && p.objects.iter().all(|o| state.params.areas.iter().any(|a| a.kind == AreaKind::Settings && a.object == *o))
}

/// The print settings volume picked (`inst/solid`): its feature's row picked in the tree.
pub fn picked_volume(state: &AppState) -> Option<String> {
    let f = state.source.feature(state.source.selected.as_ref()?)?;
    (f.detail == crate::ui::code::SETTINGS_HERE).then(|| f.object.clone())
}

/// Whether a region's row sets nothing there: its infill pattern where its infill is solid.
fn not_in_effect(state: &AppState, pp: &PartParamsView, p: &ParamView) -> bool {
    let statement = |q: &ParamView| q.key.rsplit_once('.').map(|(s, _)| s.to_string());
    p.setting == Some("sparse_infill_pattern")
        && pp.params.iter().find(|d| d.setting == Some("sparse_infill_density") && statement(d) == statement(p))
            .is_some_and(|d| matches!(FabParamPanel::value_of(state, d), ParamValue::Num(x) if x >= 1.0 - 1e-9))
}

/// The groups of a part's parameters in panel order (declared first), with their rows: the
/// picked print settings volume's size and place (`volumes`: its settings are the Slicer
/// panel's rows), or all but any volume's.
fn grouped<'a>(state: &AppState, params: &'a [ParamView], volumes: bool) -> Vec<(String, Vec<&'a ParamView>)> {
    let mut out: Vec<(String, Vec<&ParamView>)> = Vec::new();
    let picked = if volumes { picked_volume(state) } else { None };
    let all = params.iter().filter(|p| match &picked {
        Some(v) => p.objects.iter().any(|o| o == v) && p.setting.is_none(),
        None => !volumes && !in_volume(state, p),
    });
    for p in all.clone().filter(|p| p.declared).chain(all.clone().filter(|p| !p.declared && !p.detail)).chain(all.filter(|p| p.detail)) {
        match out.iter_mut().find(|(g, _)| *g == p.group) {
            Some((_, list)) => list.push(p),
            None => out.push((p.group.clone(), vec![p])),
        }
    }
    // a box's size and position rows in the plate's order, X, Y, Z (each field where its
    // first row was)
    if volumes {
        for (_, list) in &mut out {
            let field = |p: &ParamView| p.label.rsplit_once(' ').map(|(f, _)| f.to_string());
            let first = |f: &str| list.iter().position(|q| plate_axis(q).is_some() && field(q).as_deref() == Some(f));
            let keys: Vec<(usize, usize)> = list.iter().enumerate().map(|(i, p)| match (plate_axis(p), field(p)) {
                (Some(a), Some(f)) => (first(&f).unwrap_or(i), a.order),
                _ => (i, 0),
            }).collect();
            let mut order: Vec<usize> = (0..list.len()).collect();
            order.sort_by_key(|&i| keys[i]);
            *list = order.into_iter().map(|i| list[i]).collect();
        }
    }
    out
}

/// Prepared once when the edited step or its committed parameter model changes.
struct StepRows {
    part: Arc<PartParamsView>,
    index: usize,
    heading: String,
    params: Vec<ParamView>,
}

impl FabParamPanel {
    fn rows_for_step(&mut self, state: &AppState, part: &Arc<PartParamsView>) -> Option<Arc<StepRows>> {
        let steps = &state.steps;
        let Some(index) = steps.editing.filter(|_| !self.volumes && steps.part.as_ref() == Some(&part.object)) else {
            self.step_rows = None;
            return None;
        };
        if let Some(rows) = &self.step_rows {
            if index == rows.index && Arc::ptr_eq(part, &rows.part) { return Some(rows.clone()); }
        }
        let chip = steps.chips.get(index)?;
        let (instance, _) = part.object.split_once('/')?;
        let feature = format!("{instance}/{}", chip.id);
        let value = chip.value.as_ref().map(|v| format!(" {v}")).unwrap_or_default();
        let heading = format!("Editing step {} of {}: {}{value} · Esc: done", chip.n, steps.chips.len(), chip.verb);
        let mut params: Vec<_> = part.params.iter().filter(|p| p.objects.contains(&feature)).cloned().collect();
        for p in params.iter_mut().filter(|p| p.declared) {
            for other in steps.chips.iter().filter(|s| s.n != chip.n) {
                if p.objects.iter().any(|o| o.split_once('/') == Some((instance, other.id.as_str()))) {
                    if !p.hint.is_empty() { p.hint.push_str(" · "); }
                    p.hint.push_str(&format!("also read by {} (step {})", other.verb, other.n));
                }
            }
        }
        let rows = Arc::new(StepRows { part: part.clone(), index, heading, params });
        self.step_rows = Some(rows.clone());
        Some(rows)
    }

    /// Tell the view a number here is pressed or being typed in.
    fn set_input_active(cx: &mut Cx, scope: &mut Scope, active: bool) {
        if let Some(state) = scope.data.get_mut::<AppState>() {
            if state.params.input_active != active {
                state.params.input_active = active;
                // Editing state affects the part controls and the viewport's handles and quality.
                crate::ui::redraw(cx, &[ids!(tab_part), ids!(tab_slicer), ids!(tab_viewport)]);
            }
        }
    }

    fn item(&mut self, cx: &mut Cx, id: LiveId, template: LiveId) -> Option<WidgetRef> {
        if let Some((w, t)) = self.items.get(&id) {
            if *t == template {
                return Some(w.clone());
            }
        }
        let tref = self.templates.get(&template)?;
        let value: ScriptValue = tref.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        // A tree node under the panel: the semantic tree, the tweaker and `ui act` find it.
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        self.items.insert(id, (w.clone(), template));
        self.applied.remove(&id);
        Some(w)
    }

    fn configure(&mut self, cx: &mut Cx, id: LiveId, w: &WidgetRef, p: &ParamView) {
        if self.applied.get(&id).is_some_and(|c| c.shows(p)) { return; }
        let prec = precision(p.kind, p.step) as f64;
        let label = p.label.clone();
        let suf = suffix(p);
        let (min, max, step) = (p.min, p.max, p.step);
        let snapv = snap_step(p, p.value.num().unwrap_or(1.0), false);
        let bounded = p.declared && p.kind.is_numeric() && p.max > p.min;
        // a range nothing declared scales the drag; a typed number goes past it
        let reach = reach_only(p);
        match template_for(p) {
            t if t == live_id!(Num) || t == live_id!(Seed) => {
                let mut num = w.widget(cx, ids!(num));
                let (mn, mx) = if p.kind == ParamKind::Ratio { (min * 100.0, max * 100.0) } else { (min, max) };
                let st = if p.kind == ParamKind::Ratio { step * 100.0 } else { step };
                let sn = if p.kind == ParamKind::Ratio { snapv * 100.0 } else { snapv };
                let suf = if p.kind == ParamKind::Ratio { " %".to_string() } else { suf };
                // a ratio's decimals are its fraction's: shown in percent, two fewer (15 %, not 15.00 %)
                let prec = if p.kind == ParamKind::Ratio { (prec - 2.0).max(0.0) } else { prec };
                w.label(cx, ids!(label)).set_text(cx, &label);
                crate::ui::print_settings::name_control(cx, &num, &label, "spin_button");
                script_apply_eval!(cx, num, { min: #(mn) max: #(mx) step: #(st) snap: #(sn) precision: #(prec) suffix: #(suf) show_fill: #(bounded) reach_only: #(reach) });
            }
            t if t == live_id!(Bool) => {
                let mut c = w.widget(cx, ids!(check));
                script_apply_eval!(cx, c, { text: #(label) });
            }
            t if t == live_id!(Choice) || t == live_id!(Pick) => {
                w.label(cx, ids!(label)).set_text(cx, &p.label);
                let pick = w.widget(cx, ids!(pick));
                crate::ui::print_settings::name_control(cx, &pick, &p.label, "combo_box");
                // A Choice row's dropdown mirrors its segments (PropChoice); a Pick row has only the dropdown.
                let seg = w.widget(cx, ids!(seg));
                if seg.is_empty() {
                    pick.as_drop_down().set_labels(cx, p.choices.clone());
                } else {
                    crate::ui::print_settings::name_control(cx, &seg, &p.label, "radio_group");
                    if let Some(mut inner) = seg.borrow_mut::<SegmentedControl>() {
                        inner.options = p.choices.clone();
                        inner.option_names = p.choices.clone();
                    }
                }
            }
            t if t == live_id!(Point) => {
                let n = match &p.value { ParamValue::Point(c) => c.len(), _ => 3 };
                w.label(cx, ids!(label)).set_text(cx, &p.label);
                for (i, axis) in [ids!(value.x), ids!(value.y), ids!(value.z)].into_iter().enumerate() {
                    let mut num = w.widget(cx, axis);
                    let name = ["X", "Y", "Z"][i];
                    crate::ui::print_settings::name_control(cx, &num, &format!("{} {name}", p.label), "spin_button");
                    let suf = suf.clone();
                    // the first segment names the point and its axis, the others their axis
                    let inside = if i == 0 { format!("{} {name}", p.label) } else { name.to_string() };
                    script_apply_eval!(cx, num, {min: #(min) max: #(max) step: #(step) snap: #(1.0) precision: #(prec) suffix: #(suf) reach_only: #(reach) label: #(inside)});
                    num.set_visible(cx, i < n);
                }
            }
            t if t == live_id!(Profile) => {
                w.label(cx, ids!(label)).set_text(cx, &format!("{} · drag the points", p.label));
            }
            _ => {
                w.label(cx, ids!(label)).set_text(cx, &p.label);
            }
        }
        self.applied.insert(id, RowConfig::of(p));
    }

    /// A feature's field of several axes comes as one row per axis ("Rotation X", "Rotation Y",
    /// "Rotation Z"; a 2D one's "Position X", "Position Z"): a run of two or three Num rows named
    /// so, their axes in order, with one name before the axis,
    /// draws as one row of that name with the rows' fields side by side ([`Axes`]), each axis named
    /// inside its field. Every other row is as it was. O(rows).
    fn axis_runs(&mut self, cx: &mut Cx, rows: Vec<(LiveId, WidgetRef)>, labels: &[Option<String>]) -> Vec<(LiveId, WidgetRef)> {
        const AXES: [&str; 3] = ["X", "Y", "Z"];
        let split = |l: &Option<String>| l.as_deref().and_then(|l| l.rsplit_once(' ')).map(|(base, axis)| (base.to_string(), axis.to_string()));
        let mut out = Vec::new();
        let mut k = 0;
        while k < rows.len() {
            // a run: two or three of X, Y, Z in that order (a 2D field's X and Z too), the same
            // name before each
            let axis_of = |l: &Option<String>| split(l).and_then(|(base, axis)| AXES.iter().position(|a| *a == axis).map(|i| (base, i)));
            let mut order: Vec<usize> = Vec::new();
            if let Some((base, first)) = axis_of(&labels[k]) {
                order.push(first);
                let mut n = 1;
                while k + n < rows.len() {
                    match axis_of(&labels[k + n]) {
                        Some((b, i)) if b == base && i > *order.last().unwrap() => { order.push(i); n += 1; }
                        _ => break,
                    }
                }
                if n >= 2 {
                    let aid = row_id(&RowKind::Param(format!("axes:{}", rows[k].0 .0)));
                    if let Some(axes) = self.item(cx, aid, live_id!(Axes)) {
                        axes.label(cx, ids!(label)).set_text(cx, &base);
                        // the axes' own fields, side by side (each still its row's: its value, drag and
                        // refusal are the row's; the row itself is not drawn)
                        // the first segment names the field and its axis, the others their axis
                        for (i, (_, w)) in rows[k..k + n].iter().enumerate() {
                            let axis = AXES[order[i]];
                            let name = if i == 0 { format!("{base} {axis}") } else { axis.to_string() };
                            inline_axis(cx, w, &name);
                        }
                        // Each field stays its row's in the widget tree (the row's lookups of `num`
                        // set its value), so it is reached through the row's own children and only
                        // drawn here.
                        let cells: Vec<(LiveId, WidgetRef)> = rows[k..k + n].iter().filter_map(|(id, w)| row_num(cx, w).map(|num| (*id, num))).collect();
                        let value = axes.widget(cx, ids!(value));
                        if let Some(mut view) = value.borrow_mut::<View>() {
                            let same = view.children.len() == cells.len()
                                && view.children.iter().zip(&cells).all(|((a, x), (b, y))| a == b && x.widget_uid() == y.widget_uid());
                            if !same {
                                view.children = cells.iter().cloned().collect();
                                view.redraw(cx);
                            }
                        }
                        out.push((aid, axes));
                        k += n;
                        continue;
                    }
                }
            }
            if let Some(name) = labels[k].as_deref() {
                inline_axis(cx, &rows[k].1, name);
            }
            out.push(rows[k].clone());
            k += 1;
        }
        out
    }

    fn part<'a>(state: &'a AppState) -> Option<&'a std::sync::Arc<PartParamsView>> {
        active_part(state).and_then(|i| state.params.part(i))
    }

    /// A row as this panel shows it: in the volumes panel a print settings box's size and
    /// position read in the plate's axes ([`plate_axis`]), named there, their range turned
    /// with their sign; any other row as it is.
    fn plate_view(&self, p: &ParamView) -> ParamView {
        match plate_axis(p).filter(|_| self.volumes) {
            Some(a) => {
                let (min, max) = if a.sign < 0.0 { (-p.max, -p.min) } else { (p.min, p.max) };
                let [lo, hi] = p.limits;
                let limits = if a.sign < 0.0 { [hi.map(|x| -x), lo.map(|x| -x)] } else { [lo, hi] };
                ParamView { label: a.label, min, max, limits, ..p.clone() }
            }
            None => p.clone(),
        }
    }

    /// What turns a row's written value into the one it shows (and back): -1 for a box's
    /// position along a plate axis its part's runs against, else 1.
    fn plate_sign(&self, p: &ParamView) -> f64 {
        plate_axis(p).filter(|_| self.volumes).map_or(1.0, |a| a.sign)
    }

    /// The value a row shows: a drag in progress, else a value set and not built yet, else the
    /// model's; never the old one while the part rebuilds. A region's print setting its statement
    /// does not write shows what the part prints with there ([`Self::inherited`]).
    fn value_of(state: &AppState, p: &ParamView) -> ParamValue {
        let shown = state.params.shown_value(p);
        match Self::inherited(state, p) {
            Some(own) if shown == p.value => own,
            _ => shown,
        }
    }

    /// What the part prints with where a region's print setting row's statement does not set it
    /// (its own override, the plate's, the profile's), as the row's value; none for a written one
    /// or any other row.
    fn inherited(state: &AppState, p: &ParamView) -> Option<ParamValue> {
        let key = p.setting.filter(|_| p.site.as_ref().is_some_and(|s| s.is_empty()))?;
        let s = fab_plate::print_setting(key)?;
        let part = active_part(state).and_then(|i| state.plate.parts.get(i))?;
        let value = crate::ui::print_settings::part_value(state, part, s);
        match s.kind {
            fab_plate::SettingKind::Choice(list) => {
                let label = list.iter().find(|(v, _)| *v == value)?.1;
                p.choices.iter().position(|c| c == label).map(ParamValue::Choice)
            }
            fab_plate::SettingKind::Number { unit: "%", .. } => fab_plate::setting_number(value).map(|x| ParamValue::Num(x / 100.0)),
            fab_plate::SettingKind::Number { .. } => fab_plate::setting_number(value).map(ParamValue::Num),
            fab_plate::SettingKind::Bool => None,
        }
    }
}

impl Widget for FabParamPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, if self.volumes { "Print settings volumes" } else { "Shape parameters" });
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        if self.tool_shown {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::Status, bounds: makepad_platform::a11y::semantic_rect(self.area.rect(cx)), ..Default::default() };
            makepad_platform::a11y::set_string(&mut node.name, "Cutting tool shown");
            out.push((u64::MAX, None, node));
        }
    }

    fn visit_cancel(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) -> bool {
        for (id, kind, _) in &self.rows {
            if !matches!(kind, RowKind::Param(_)) {
                if let Some((w, _)) = self.items.get(id) { visit(*id, w.clone()); }
            }
        }
        true
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Draw(_)) { return; }
        // a group held by its grip moves among its part's groups, the others sliding aside: first,
        // so the press is the grip's, not its header's fold
        if let (Some(key), true) = (self.order_key, matches!(event, Event::MouseDown(_) | Event::MouseMove(_) | Event::MouseUp(_) | Event::NextFrame(_))) {
            let order: Vec<(LiveId, WidgetRef, WidgetRef)> = self.group_ids.iter()
                .filter_map(|id| self.items.get(id).map(|(w, _)| (*id, w.clone(), w.clone())))
                .collect();
            crate::ui::widgets::section_reorder(cx, event, key, &order, self.area);
        }
        crate::ui::widgets::section_headers(cx, event, self.items.iter().filter(|(_, (_, t))| *t == live_id!(Group)).map(|(_, (w, _))| w));
        let claimed_before = event.pointer_claimed_area();
        let Some(state) = scope.data.get::<AppState>() else { return };
        let outside = match event {
            Event::MouseMove(e) => !self.area.clipped_rect(cx).contains(e.abs),
            Event::MouseDown(e) => !self.area.clipped_rect(cx).contains(e.abs),
            Event::Scroll(e) => !self.area.clipped_rect(cx).contains(e.abs),
            _ => false,
        };
        let leaving = outside && self.pointer_inside;
        if matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::Scroll(_)) {
            self.pointer_inside = !outside;
        }
        // Deliver hover exit and captured drags, including the profile editor outside the panel.
        if outside && !leaving && !state.params.input_active && !cx.fingers.any_areas_captured() {
            if matches!(event, Event::MouseMove(_)) {
                self.hint.clear(cx, self.uid);
                if let Some(state) = scope.data.get_mut::<AppState>() {
                    let owned = self.rows.iter().any(|(_, kind, _)| matches!(kind, RowKind::Param(k) if state.params.hover.as_ref() == Some(k)));
                    if owned && state.params.dragging.is_none() {
                        state.params.hover = None;
                        // Leaving a parameter clears its panel hover and viewport handle highlight.
                        self.area.redraw(cx);
                        crate::ui::redraw(cx, &[ids!(tab_viewport)]);
                    }
                }
            }
            return;
        }
        let Some(pp) = Self::part(state).cloned() else {
            Self::set_input_active(cx, scope, false);
            return;
        };
        let object = &pp.object;
        let actions = cx.capture_actions(|cx| {
            for (id, kind, _) in &self.rows {
                if !matches!(kind, RowKind::Param(_)) {
                    if let Some((w, _)) = self.items.get(id) { w.handle_event(cx, event, scope); }
                }
            }
        });
        // Only an area hover can light a parameter through the panel's input layer.
        if let Event::MouseMove(e) = event {
            let claimed = event.pointer_claimed_area();
            let pointer = if claimed != claimed_before && !claimed.is_empty() {
                Some(e.abs)
            } else {
                match event.hits(cx, self.area) {
                    Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => Some(e.abs),
                    _ => None,
                }
            };
            let key = pointer.and_then(|pos| self.rows.iter().find(|(_, _, r)| r.contains(pos)))
                .and_then(|(_, kind, _)| match kind { RowKind::Param(key) => Some(key.clone()), _ => None });
            let hint = key.as_ref().and_then(|key| {
                let state = scope.data.get::<AppState>()?;
                if state.params.dragging.is_some() { return None; }
                let (id, _, rect) = self.rows.iter().find(|(_, kind, _)| matches!(kind, RowKind::Param(k) if k == key))?;
                let p = match &self.step_rows {
                    Some(rows) => rows.params.iter().find(|p| p.key == *key)?,
                    None => pp.param(key)?,
                };
                // a name its row cut short shows whole first
                let label = self.items.get(id).map(|(w, _)| w.value_input(cx, ids!(num))).filter(|n| n.label_cut()).map(|n| n.label());
                let tip = match label {
                    Some(name) if p.hint.is_empty() => name,
                    Some(name) => format!("{name}\n{}", p.hint),
                    None => p.hint.clone(),
                };
                Some((*id, *rect, tip))
            });
            match hint {
                Some((id, rect, hint)) => self.hint.hover(cx, self.uid, Some((id, rect, hint.as_str()))),
                None => self.hint.clear(cx, self.uid),
            }
            if let Some(state) = scope.data.get_mut::<AppState>() {
                let owned = self.rows.iter().any(|(_, kind, _)| matches!(kind, RowKind::Param(k) if state.params.hover.as_ref() == Some(k)));
                if state.params.dragging.is_none() && (pointer.is_some() || owned) && state.params.hover != key {
                    state.params.hover = key;
                    // Parameter hover appears here and on the viewport's shape handles.
                    self.area.redraw(cx);
                    crate::ui::redraw(cx, &[ids!(tab_viewport)]);
                }
            }
        }
        let later = match event {
            Event::Actions(actions) if actions.iter().any(|a| a.downcast_ref::<WidgetAction>().is_some()) => Some(actions),
            _ => None,
        };
        // Hover and animation events have no value actions to dispatch.
        if actions.is_empty() && later.is_none() && !matches!(event, Event::MouseDown(_) | Event::MouseUp(_) | Event::TouchUpdate(_) | Event::KeyFocus(_) | Event::KeyFocusLost(_) | Event::FingerCancel(_) | Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            return;
        }
        for actions in Some(&actions).into_iter().chain(later).filter(|actions| !actions.is_empty()) {
            for (id, kind, _) in &self.rows {
                let Some((w, _)) = self.items.get(id) else { continue };
                match kind {
                    RowKind::Group(g) => {
                        for action in actions.filter_widget_actions_cast::<FoldHeaderAction>(w.widget_uid()) {
                            let opening = match action {
                                FoldHeaderAction::Opening => true,
                                FoldHeaderAction::Closing => false,
                                _ => continue,
                            };
                            if opening { self.open.insert(g.clone()); } else { self.open.remove(g); }
                            let focus = pp.boxes.iter().find(|b| group_of(&pp, &b.object) == Some(g.as_str()))
                                .map(|b| b.object.clone()).filter(|o| o != object);
                            cx.action(ParamAction::Focus(if opening { focus } else { None }));
                            // Folding changes this panel; feature focus changes viewport handles.
                            self.area.redraw(cx);
                            crate::ui::redraw(cx, &[ids!(tab_viewport)]);
                        }
                    }
                    RowKind::Param(key) => {
                        let Some(p) = pp.param(&key) else { continue };
                        let preview = |cx: &mut Cx, value: ParamValue| cx.action(ParamAction::Preview { object: object.clone(), key: key.clone(), value });
                        let commit = |cx: &mut Cx, value: ParamValue| cx.action(ParamAction::Commit { object: object.clone(), key: key.clone(), value });
                        let pct = |v: f64| if p.kind == ParamKind::Ratio { v / 100.0 } else { v };
                        match p.kind {
                            ParamKind::Growth => {
                                if w.button(cx, ids!(pause)).clicked(&actions) {
                                    if let Some(sim) = scope.data.get::<AppState>().and_then(|st| growing(st, &pp.object, &p.value)) {
                                        sim.set_paused(!sim.paused());
                                        self.area.redraw(cx);
                                    }
                                }
                                continue;
                            }
                            ParamKind::Seed if w.button(cx, ids!(reseed)).clicked(&actions) => {
                                commit(cx, ParamValue::Num(reseeded(p)));
                                continue;
                            }
                            ParamKind::Bool => {
                                if let Some(on) = w.check_box(cx, ids!(check)).changed(&actions) {
                                    commit(cx, ParamValue::Bool(on));
                                }
                            }
                            ParamKind::Choice => {
                                if let Some(i) = w.segmented_control(cx, ids!(seg)).selected(&actions)
                                    .or_else(|| w.drop_down(cx, ids!(pick)).selected(&actions)) {
                                    if Some(&ParamValue::Choice(i)) != Some(&p.value) {
                                        commit(cx, ParamValue::Choice(i));
                                    }
                                }
                            }
                            ParamKind::Point => {
                                let ParamValue::Point(c) = &p.value else { continue };
                                for (i, axis) in [ids!(value.x), ids!(value.y), ids!(value.z)].into_iter().enumerate() {
                                    let num = w.value_input(cx, axis);
                                    let with = |v: f64| {
                                        let mut c = c.clone();
                                        if let Some(x) = c.get_mut(i) {
                                            *x = v;
                                        }
                                        ParamValue::Point(c)
                                    };
                                    if let Some(v) = num.ended(&actions) {
                                        commit(cx, with(v));
                                    } else if let Some(v) = num.changed(&actions) {
                                        preview(cx, with(v));
                                    }
                                }
                            }
                            ParamKind::Profile => {
                                let ed = w.fab_profile_editor(cx, ids!(editor));
                                if let Some(pts) = ed.ended(&actions) {
                                    commit(cx, ParamValue::Profile(pts));
                                } else if let Some(pts) = ed.changed(&actions) {
                                    preview(cx, ParamValue::Profile(pts));
                                }
                            }
                            _ => {
                                let num = w.value_input(cx, ids!(num));
                                // (a box's row shows its value along the plate's axis: back to the
                                // written one)
                                let sign = self.plate_sign(p);
                                if let Some(v) = num.ended(&actions) {
                                    // (a row shown against its written sign takes numbers only: its
                                    // formula would be written with the shown sign)
                                    if let Some((value, code)) = typed_formula(p, actions, num.widget_uid()).filter(|_| sign > 0.0) {
                                        cx.action(ParamAction::Link { object: object.clone(), key: key.clone(), code, value: within_limits(p, pct(sign * value)) });
                                    } else {
                                        commit(cx, ParamValue::Num(within_limits(p, pct(sign * v))));
                                    }
                                } else if let Some(v) = num.changed(&actions) {
                                    preview(cx, ParamValue::Num(within_limits(p, pct(sign * v))));
                                }
                            }
                        }
                    }
                    RowKind::Hint => {}
                    RowKind::Mass => {
                        for action in actions.filter_widget_actions_cast::<FoldHeaderAction>(w.widget_uid()) {
                            let open = match action {
                                FoldHeaderAction::Opening => true,
                                FoldHeaderAction::Closing => false,
                                _ => continue,
                            };
                            cx.action(ParamAction::Weigh { object: object.clone(), open });
                            self.area.redraw(cx);
                        }
                    }
                }
            }
        }
        let active = self.rows.iter().any(|(id, kind, _)| {
            matches!(kind, RowKind::Param(_))
                && self.items.get(id).is_some_and(|(w, _)| {
                    let live = |num: ValueInputRef| num.borrow().is_some_and(|n| n.is_interacting());
                    live(w.value_input(cx, ids!(num))) || [ids!(value.x), ids!(value.y), ids!(value.z)].into_iter().any(|axis| live(w.value_input(cx, axis)))
                })
        });
        Self::set_input_active(cx, scope, active);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.rows.clear();
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        // A ghost that is fading has done its work.
        self.tool_shown = !self.volumes && state.params.ghost.as_ref().is_some_and(|g| g.fade_from.is_none() && !g.bodies.is_empty());
        let Some(pp) = Self::part(state).cloned() else {
            self.step_rows = None;
            cx.begin_turtle(walk, self.layout);
            cx.end_turtle_with_area(&mut self.area);
            return DrawStep::done();
        };
        let new_part = self.shown_part.as_deref() != Some(pp.object.as_str());
        if new_part {
            self.shown_part = Some(pp.object.clone());
        }
        let at = std::sync::Arc::as_ptr(&pp) as usize;
        if self.names.as_ref().is_none_or(|(was, _)| *was != at) {
            self.names = Some((at, named_values(&pp)));
        }
        let names = self.names.as_ref().map(|(_, n)| n.clone()).unwrap_or_else(|| std::sync::Arc::from(Vec::new()));
        open_declared(new_part, pp.params.iter().filter(|p| p.declared).map(|p| p.group.as_str()), &mut self.seen, &mut self.open);
        // A part that declares nothing: its own object's fields.
        if new_part && !pp.params.iter().any(|p| p.declared) {
            if let Some(g) = group_of(&pp, &pp.object) {
                self.open.insert(g.to_string());
            }
        }
        if let Some(g) = state.params.focus.as_deref().and_then(|f| group_of(&pp, f)) {
            self.open.insert(g.to_string());
        }
        cx.begin_turtle(walk, self.layout);
        let editing = self.rows_for_step(state, &pp);
        let mut groups = grouped(state, editing.as_ref().map_or(&pp.params, |rows| &rows.params), self.volumes);
        // in the order they were dragged into (per part), the held one last (drawn over the others)
        let key = LiveId::from_str(&format!("param_groups:{}", pp.object));
        self.order_key = Some(key);
        let held = crate::ui::widgets::held_section(cx, key);
        groups.sort_by_key(|(g, _)| crate::ui::widgets::section_rank(cx, key, row_id(&RowKind::Group(g.clone()))));
        if let Some(i) = groups.iter().position(|(g, _)| Some(row_id(&RowKind::Group(g.clone()))) == held) {
            let g = groups.remove(i);
            groups.push(g);
        }
        self.searchable = groups.iter().map(|(g, ps)| (g.clone(), ps.iter().map(|p| p.label.clone()).collect())).collect();
        // the editor's search: a group whose name holds every word shows whole, else only its
        // rows whose group and label together hold them (so "holes depth" finds Holes' Depth)
        let filtering = !self.filter.is_empty();
        if filtering {
            let words = &self.filter;
            groups = groups.into_iter().filter_map(|(g, ps)| {
                if holds_all(words, &g) { return Some((g, ps)); }
                let ps: Vec<_> = ps.into_iter().filter(|p| holds_all(words, &format!("{g} {}", p.label))).collect();
                (!ps.is_empty()).then_some((g, ps))
            }).collect();
        }
        self.group_ids = groups.iter().map(|(g, _)| row_id(&RowKind::Group(g.clone()))).collect();
        if filtering {
            // (the hints say what the part is; a search shows its matches alone)
        } else if let Some(rows) = &editing {
            if let Some(w) = self.item(cx, live_id!(param_edit_step), live_id!(Hint)) {
                w.label(cx, ids!(text)).set_text(cx, &rows.heading);
                w.draw_all(cx, &mut Scope::empty());
                self.rows.push((live_id!(param_edit_step), RowKind::Hint, w.area().rect(cx)));
            }
        } else if !pp.readout.is_empty() && !self.volumes {
            if let Some(w) = self.item(cx, live_id!(param_readout), live_id!(Hint)) {
                w.label(cx, ids!(text)).set_text(cx, &pp.readout);
                w.draw_all(cx, &mut Scope::empty());
                self.rows.push((live_id!(param_readout), RowKind::Hint, w.area().rect(cx)));
            }
        }
        let grow: Vec<&str> = state.params.grow_lines.iter().filter(|l| l.object == pp.object).map(|l| l.text.as_str()).collect();
        if !grow.is_empty() && !self.volumes && editing.is_none() && !filtering {
            if let Some(w) = self.item(cx, live_id!(param_grow), live_id!(Hint)) {
                w.label(cx, ids!(text)).set_text(cx, &grow.join("\n"));
                w.draw_all(cx, &mut Scope::empty());
                self.rows.push((live_id!(param_grow), RowKind::Hint, w.area().rect(cx)));
            }
        }
        if groups.is_empty() && !self.volumes && editing.is_none() && !filtering {
            if let Some(w) = self.item(cx, live_id!(param_hint), live_id!(Hint)) {
                w.label(cx, ids!(text)).set_text(cx, "This part has no shape parameters (an imported mesh). Ask the AI to rebuild it as a program to tune it here.");
                w.draw_all(cx, &mut Scope::empty());
                self.rows.push((live_id!(param_hint), RowKind::Hint, w.area().rect(cx)));
            }
        }
        for (group, params) in groups {
            let gk = RowKind::Group(group.clone());
            let gid = row_id(&gk);
            // (the picked volume's one group always shows its rows)
            let open = self.volumes || editing.is_some() || filtering || self.open.contains(&group);
            let Some(section) = self.item(cx, gid, live_id!(Group)) else { continue };
            if !self.applied.get(&gid).is_some_and(|c| c.shows_group(&group)) {
                section.label(cx, ids!(hdr.title)).set_text(cx, &group);
                self.applied.insert(gid, RowConfig::Group { title: group.clone() });
            }
            if section.as_prop_section().is_open(cx) != open {
                section.as_prop_section().set_is_open(cx, open, Animate::No);
            }
            let mut children = Vec::new();
            // (each Num row's label, for the axis runs that draw as one row)
            let mut labels: Vec<Option<String>> = Vec::new();
            let start = self.rows.len();
            self.rows.push((gid, gk, Rect::default()));
            // A closing group keeps its rows until the fold animation has run out, so
            // it shrinks smoothly like the fixed sections instead of vanishing.
            if !open && section.as_prop_section().opened(cx) <= 0.001 {
                crate::ui::widgets::section_rows(cx, &section, children);
                draw_at_offset(cx, key, gid, &section);
                self.rows[start].2 = section.widget(cx, ids!(hdr)).area().rect(cx);
                continue;
            }
            for p in params {
                let rk = RowKind::Param(p.key.clone());
                let id = row_id(&rk);
                let Some(w) = self.item(cx, id, template_for(p)) else { continue };
                let shown = self.plate_view(p);
                self.configure(cx, id, &w, &shown);
                let value = match (Self::value_of(state, p), self.plate_sign(p)) {
                    (ParamValue::Num(x), sign) => ParamValue::Num(sign * x),
                    (v, _) => v,
                };
                match (&value, template_for(p)) {
                    (_, t) if t == live_id!(Growth) => {
                        let (text, running) = growth_text(state, &pp.object, &value);
                        w.label(cx, ids!(value.text)).set_text(cx, &text);
                        let pause = w.button(cx, ids!(pause));
                        pause.set_visible(cx, running.is_some());
                        if let Some(paused) = running {
                            pause.set_text(cx, if paused { "Resume" } else { "Pause" });
                        }
                    }
                    (_, t) if t == live_id!(Derived) => {
                        w.label(cx, ids!(value.text)).set_text(cx, &format_value(p, &value));
                    }
                    (ParamValue::Num(x), _) => {
                        let num = w.value_input(cx, ids!(num));
                        let links = p.links && self.plate_sign(p) > 0.0;
                        num.set_names(cx, names.clone(), links);
                        num.set_formula(cx, p.formula.as_deref().filter(|_| links));
                        if !num.borrow().is_some_and(|n| n.is_interacting()) {
                            num.set_value(cx, if p.kind == ParamKind::Ratio { x * 100.0 } else { *x });
                        }
                        // a region's setting it does not write is the part's: shown as inherited
                        let inherited = Self::inherited(state, p).is_some();
                        if let Some(mut n) = num.borrow_mut() {
                            n.inherited = inherited;
                        };
                    }
                    (ParamValue::Bool(b), _) => w.check_box(cx, ids!(check)).set_active(cx, *b, Animate::No),
                    (ParamValue::Choice(i), _) => {
                        w.segmented_control(cx, ids!(seg)).set_selected(cx, *i);
                        w.drop_down(cx, ids!(pick)).set_selected_item(cx, *i);
                        // dim: the part's own (a region's setting it does not write), or not in
                        // effect there (a solid infill's pattern)
                        let dim = Self::inherited(state, p).is_some() || not_in_effect(state, &pp, p);
                        if self.dimmed.contains(&id) != dim {
                            let mut pick = w.widget(cx, ids!(pick));
                            script_apply_eval!(cx, pick, {draw_text +: {color: if #(dim) {mod.theme.prop_ink_dim} else {mod.theme.color_text}}});
                            // (a Pick row is its dropdown alone: it has no segments)
                            let mut seg = w.widget(cx, ids!(seg));
                            if !seg.is_empty() {
                                script_apply_eval!(cx, seg, {draw_text_selected +: {color: if #(dim) {mod.theme.prop_ink_dim} else {mod.theme.color_text}}});
                            }
                            if dim { self.dimmed.insert(id); } else { self.dimmed.remove(&id); }
                        }
                    }
                    (ParamValue::Point(c), _) => {
                        for (i, axis) in [ids!(value.x), ids!(value.y), ids!(value.z)].into_iter().enumerate() {
                            let num = w.value_input(cx, axis);
                            num.set_names(cx, names.clone(), false);
                            if !num.borrow().is_some_and(|n| n.is_interacting()) {
                                num.set_value(cx, c.get(i).copied().unwrap_or(0.0));
                            }
                        }
                    }
                    (ParamValue::Profile(pts), _) => w.fab_profile_editor(cx, ids!(editor)).set_points(cx, pts),
                }
                if let Some(cap) = &state.params.capped {
                    if cap.key == p.key && self.capped_seen.get(&p.key) != Some(&cap.serial) {
                        self.capped_seen.insert(p.key.clone(), cap.serial);
                        if Cx::time_now() - cap.at < 1.0 {
                            match (&cap.hold, template_for(p)) {
                                (ParamValue::Num(x), t) if t == live_id!(Num) => {
                                    let num = w.value_input(cx, ids!(num));
                                    let value = if p.kind == ParamKind::Ratio { x * 100.0 } else { *x };
                                    if cap.flash { num.refuse(cx, value); } else { num.hold(cx, value); }
                                }
                                (ParamValue::Point(c), t) if t == live_id!(Point) => {
                                    for (i, axis) in [ids!(value.x), ids!(value.y), ids!(value.z)].into_iter().enumerate() {
                                        if let Some(x) = c.get(i) {
                                            let num = w.value_input(cx, axis);
                                            if cap.flash { num.refuse(cx, *x); } else { num.hold(cx, *x); }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                children.push((id, w.clone()));
                labels.push((template_for(p) == live_id!(Num)).then(|| p.label.clone()));
                if open { self.rows.push((id, rk, Rect::default())); }
            }
            let children = self.axis_runs(cx, children, &labels);
            crate::ui::widgets::section_rows(cx, &section, children);
            draw_at_offset(cx, key, gid, &section);
            for (id, kind, rect) in &mut self.rows[start..] {
                if let Some((w, _)) = self.items.get(id) {
                    *rect = if matches!(kind, RowKind::Group(_)) { w.widget(cx, ids!(hdr)).area().rect(cx) }
                        // an axis of an Axes row: its field is what shows
                        else if !w.area().is_valid(cx) { w.widget(cx, ids!(num)).area().rect(cx) }
                        else { w.area().rect(cx) };
                }
            }
        }
        if !self.volumes && editing.is_none() && (!filtering || holds_all(&self.filter, MASS)) {
            self.draw_mass(cx, state, &pp);
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

/// Whether `text` holds every one of the search's `words` (lowercased), case aside.
fn holds_all(words: &[String], text: &str) -> bool {
    let text = text.to_lowercase();
    words.iter().all(|w| text.contains(w.as_str()))
}

/// A group drawn at its offset while one of its part's groups moves (`widgets::section_offset`:
/// its top margin, given back by its bottom one, so nothing after it moves with it).
fn draw_at_offset(cx: &mut Cx2d, key: LiveId, id: LiveId, section: &WidgetRef) {
    let offset = crate::ui::widgets::section_offset(cx, key, id);
    let mut walk = section.walk(cx);
    walk.margin.top += offset;
    walk.margin.bottom -= offset;
    section.draw_walk_all(cx, &mut Scope::empty(), walk);
}

impl FabParamPanel {
    /// The editor's search words (none: show all); whether any of its rows holds them all (by
    /// the last draw's rows). The filter is the panel's own: its rows are drawn, not children a
    /// filter could hide one by one.
    pub fn set_filter(&mut self, cx: &mut Cx, words: &[String]) -> bool {
        if self.filter != words {
            self.filter = words.to_vec();
            self.area.redraw(cx);
        }
        words.is_empty() || holds_all(words, MASS) || self.searchable.iter().any(|(group, labels)| {
            holds_all(words, group) || labels.iter().any(|label| holds_all(words, &format!("{group} {label}")))
        })
    }

    /// The Mass and balance row: closed until the person opens it, then the part weighed as the
    /// job prints it (the app asks `fab.measure {what: "mass"}`, as the AI does) and its figures
    /// here, while the view marks its centre of mass.
    fn draw_mass(&mut self, cx: &mut Cx2d, state: &AppState, pp: &PartParamsView) {
        let shown = state.params.mass.as_ref().filter(|m| m.panel && m.object == pp.object);
        let gid = row_id(&RowKind::Mass);
        let Some(section) = self.item(cx, gid, live_id!(Group)) else { return };
        if !self.applied.get(&gid).is_some_and(|c| c.shows_group(MASS)) {
            section.label(cx, ids!(hdr.title)).set_text(cx, MASS);
            self.applied.insert(gid, RowConfig::Group { title: MASS.into() });
        }
        if section.as_prop_section().is_open(cx) != shown.is_some() {
            section.as_prop_section().set_is_open(cx, shown.is_some(), Animate::No);
        }
        let start = self.rows.len();
        self.rows.push((gid, RowKind::Mass, Rect::default()));
        let mut children = Vec::new();
        if shown.is_some() {
            let rows = match shown.and_then(|m| m.figures.as_ref()) {
                Some(f) => f.rows.clone(),
                None => vec![(String::new(), "Weighing…".to_string())],
            };
            for (i, (label, value)) in rows.iter().enumerate() {
                let id = live_id!(param_mass_row).str_append(&i.to_string());
                let Some(w) = self.item(cx, id, live_id!(Derived)) else { continue };
                w.label(cx, ids!(label)).set_text(cx, label);
                w.label(cx, ids!(value.text)).set_text(cx, value);
                children.push((id, w.clone()));
                self.rows.push((id, RowKind::Hint, Rect::default()));
            }
        }
        crate::ui::widgets::section_rows(cx, &section, children);
        section.draw_all(cx, &mut Scope::empty());
        for (id, kind, rect) in &mut self.rows[start..] {
            if let Some((w, _)) = self.items.get(id) {
                *rect = if matches!(kind, RowKind::Mass) { w.widget(cx, ids!(hdr)).area().rect(cx) } else { w.area().rect(cx) };
            }
        }
    }
}

/// The simulation growing the part's `which`-th pattern (a growth row's value), while one does.
fn growing(state: &AppState, object: &str, which: &ParamValue) -> Option<std::sync::Arc<fab_paint_field::pattern::live::Sim>> {
    let k = which.num()? as usize;
    let view = state.plate.parts.iter().find(|p| p.object == object)?.paint_view.as_ref()?;
    view.program.patterns().nth(k)?.sim.clone()
}

/// A growth row's line, and whether its simulation runs (paused or not) for its button.
fn growth_text(state: &AppState, object: &str, which: &ParamValue) -> (String, Option<bool>) {
    let k = which.num().unwrap_or(0.0) as usize;
    let Some(g) = state.plate.parts.iter().find(|p| p.object == object).and_then(|p| p.paint_view.as_ref()).and_then(|v| v.program.patterns().nth(k)) else {
        return ("not yet".into(), None);
    };
    let (step, of) = fab_paint_field::pattern::live::progress(g);
    // where its pattern settled, once it has (a readout: it prints at its steps all the same)
    let settled = g.grid.as_ref().and_then(|grid| grid.settled);
    match (&g.sim, settled) {
        (Some(sim), _) if sim.paused() => (format!("{step} of {of} · paused"), Some(true)),
        (Some(_), Some(at)) if step < of => (format!("{step} of {of} · settled at {at}"), Some(false)),
        (Some(sim), None) if step < of => (format!("{step} of {of} · {:.0}/s", sim.rate()), Some(false)),
        (_, Some(at)) => (format!("{of} steps · settled at {at}"), None),
        _ => (format!("{of} steps"), None),
    }
}

/// A seed other than the row's, in its range.
fn reseeded(p: &ParamView) -> f64 {
    let old = p.value.num().unwrap_or(0.0) as u64;
    let top = if p.max > p.min { p.max as u64 } else { 99_999 };
    let lo = p.min.max(0.0) as u64;
    let span = top.saturating_sub(lo).max(1) + 1;
    let mut x = (Cx::time_now().to_bits() ^ old.wrapping_mul(0x9e37_79b9_7f4a_7c15)).wrapping_mul(0x2545_f491_4f6c_dd1d);
    loop {
        let v = lo + (x >> 17) % span;
        if v != old || span == 1 {
            return v as f64;
        }
        x = x.wrapping_mul(0x2545_f491_4f6c_dd1d).wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared group starts open on a new part; one that appears later opens once; a
    /// group the person folded stays folded across draws, until another part is shown.
    #[test]
    fn declared_groups_open_once_and_folds_stay() {
        let (mut seen, mut open) = (BTreeSet::new(), BTreeSet::new());
        let is_open = |open: &BTreeSet<String>, g: &str| open.contains(g);
        let groups = ["Jellyfish", "Jellyfish", "Cords", "Bell", "Tentacles"];
        open_declared(true, groups.into_iter(), &mut seen, &mut open);
        for g in ["Parameters", "Jellyfish", "Cords", "Bell", "Tentacles"] {
            assert!(is_open(&open, g), "{g} starts open: {open:?}");
        }
        open.remove("Cords");
        open_declared(false, groups.into_iter(), &mut seen, &mut open);
        assert!(!is_open(&open, "Cords"), "a fold stays across draws");
        let later = ["Jellyfish", "Cords", "Bell", "Tentacles", "Fins"];
        open_declared(false, later.into_iter(), &mut seen, &mut open);
        assert!(is_open(&open, "Fins"), "a group that appears later opens");
        assert!(!is_open(&open, "Cords"), "and leaves the fold alone");
        open.remove("Fins");
        open_declared(false, later.into_iter(), &mut seen, &mut open);
        assert!(!is_open(&open, "Fins"), "it opens once");
        open_declared(true, ["Cords"].into_iter(), &mut seen, &mut open);
        assert!(is_open(&open, "Cords") && !is_open(&open, "Bell") && is_open(&open, "Parameters"), "a new part starts over: {open:?}");
    }
}
