//! The Mechanics panel: the live sim's status, every joint's angle and load as it moves (and what
//! its motor pushes, and each string's tension), a row per motor that sets it, and the joint check, run when asked: its progress (a busy line with its stage,
//! bar, time and Cancel) and what it says of each joint. The sim itself runs in the app; this
//! panel shows `AppState::mech`.

use crate::api::{AppState, MechAction, MechCheckView, MechJointView, MechLight, MechPieceView, MechTool, MechView};
use crate::print::trays::DrawFabChip;
use crate::ui::busy::FabBusyLineWidgetExt;
use makepad_widgets_core::*;
use std::collections::HashMap;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabSupportPiecesBase = #(FabSupportPieces::register_widget(vm))
    mod.widgets.FabSupportPieces = set_type_default() do mod.widgets.FabSupportPiecesBase{
        width: Fill
        height: Fit
        draw_chip: mod.draw.DrawFabChip{ radius: 3.0 }
        draw_text: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_dim: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
    }

    mod.widgets.FabMechMotorsBase = #(FabMechMotors::register_widget(vm))
    // One row per motor, as many as the piece has; the panel's body scrolls them. A motor's
    // name stands beside its track while both fit (the names in a column as wide as the longest,
    // the tracks taking the rest), else above it, wrapping: a name is never cut.
    mod.widgets.FabMechMotors = set_type_default() do mod.widgets.FabMechMotorsBase{
        width: Fill height: Fit flow: Down spacing: theme.prop_row_gap
        draw_name: mod.draw.DrawText{
            text_style: theme.font_regular{font_size: theme.prop_font_size line_spacing: 1.0}
        }
        Row := PropRow{
            label +: {width: 0}
            value +: {num := ValueInput{width: Fill height: Fill min_height: 0 track: true precision: 0}}
        }
        Stacked := View{
            width: Fill height: Fit flow: Down spacing: 2
            label := mod.widgets.FabLabelDim{width: Fill height: Fit flow: Right{wrap: true} text: ""}
            value := View{
                width: Fill height: theme.prop_row_height
                num := ValueInput{width: Fill height: Fill min_height: 0 track: true precision: 0}
            }
        }
    }

    mod.widgets.FabMechanicsPanelBase = #(FabMechanicsPanel::register_widget(vm))
    mod.widgets.FabMechanicsPanel = set_type_default() do mod.widgets.FabMechanicsPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        body := mod.widgets.FabScroll{
            flow: Down padding: 10 spacing: 8
            status := mod.widgets.FabLabelDim{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
            verdict := mod.widgets.FabLabel{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
            joints := mod.widgets.FabLabel{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
            motors_title := mod.widgets.FabLabelDim{ width: Fill height: Fit text: "Motors" visible: false }
            motors := mod.widgets.FabMechMotors{}
            // the joint check: what it is doing (or said last) and its button, a fixed slot
            check := mod.widgets.FabBusyLine{ visible: false }
            check_rows := mod.widgets.FabLabel{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
            pieces := mod.widgets.FabSupportPieces{}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabMechanicsPanel {
    #[deref]
    view: View,
}

fn tool_buttons() -> [(&'static [LiveId], MechTool); 5] {
    [
        (ids!(t_grab), MechTool::Grab),
        (ids!(t_throw), MechTool::Throw),
        (ids!(t_pin), MechTool::Pin),
        (ids!(t_dangle), MechTool::Dangle),
        (ids!(t_poke), MechTool::Poke),
    ]
}

pub(crate) fn handle_tools(cx: &mut Cx, tools: &ViewRef, actions: &Actions) {
    for (id, tool) in tool_buttons() {
        if tools.radio_button(cx, id).clicked(actions) {
            cx.action(MechAction::Tool(tool));
        }
    }
}

pub(crate) fn sync_tools(cx: &mut Cx, tools: &ViewRef, tool: MechTool) {
    for (id, value) in tool_buttons() {
        let button = tools.radio_button(cx, id);
        let on = value == tool;
        if button.active(cx) != on {
            button.set_active(cx, on, Animate::No);
        }
    }
}

/// The joint check's busy line when no check runs: what the last one said, or that there is none
/// (or that it is out of date).
pub fn check_line(c: &MechCheckView) -> String {
    match (c.line.is_empty(), c.stale) {
        (true, _) => "Joints not checked".into(),
        (false, true) => format!("{} · out of date: the part or its settings changed", c.line),
        (false, false) => c.line.clone(),
    }
}

const CHECK_TIP: &str = "Checks each joint as designed: the closest the parts come, the layers where a bead could tack them together, and how far it turns before they touch. The view marks what it finds; the sim then uses the measured range.";

/// A force or a torque as the panel reads it: its size, to two figures past the point at most.
fn reading(v: f64) -> String {
    match v.abs() {
        v if v >= 10.0 => format!("{v:.0}"),
        v if v >= 1.0 => format!("{v:.1}"),
        v => format!("{v:.2}"),
    }
}

/// The joint list as the panel shows it, one line each: where each joint is, the force through it and what its
/// motor pushes; then each string's tension.
pub fn joint_lines(m: &MechView) -> String {
    m.joints
        .iter()
        .map(|j| {
            // a slider's travel and place in millimetres, the others' in degrees
            let (at, range) = match j.kind.as_str() {
                "ball" => (format!("{:+.1}°", j.angle.to_degrees()), format!("cone {:.0}°", j.upper.to_degrees())),
                "slider" => (format!("{:+.1} mm", j.angle), format!("{:.1}..{:.1} mm", j.lower, j.upper)),
                "flex" => (format!("{:+.1}°", j.angle.to_degrees()), "springs back".to_string()),
                _ => (format!("{:+.1}°", j.angle.to_degrees()), format!("{:.0}°..{:.0}°", j.lower.to_degrees(), j.upper.to_degrees())),
            };
            let load = j.load.map_or(String::new(), |f| format!("  {} N", reading(f)));
            let unit = if j.kind == "slider" { "N" } else { "N mm" };
            let motor = j.motor.map_or(String::new(), |m| format!("  motor {} of {} {unit}", reading(m.effort), reading(m.limit)));
            format!("{} {}  {at}  {range}{load}{motor}{}", j.name, j.kind, if j.ok { "" } else { "  (check)" })
        })
        .chain(m.strings.iter().enumerate().map(|(i, t)| format!("string {}  {} N", i + 1, reading(*t))))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A motor's row: how its track reads (the panel's units: degrees and millimetres) and goes from
/// the sim's units (radians and millimetres) and back.
#[derive(Clone, Debug, PartialEq)]
struct MotorScale {
    label: String,
    min: f64,
    max: f64,
    step: f64,
    precision: f64,
    suffix: &'static str,
    /// Panel units per sim unit.
    per: f64,
}

impl MotorScale {
    fn of(j: &MechJointView, speed: bool) -> MotorScale {
        let slider = j.kind == "slider";
        let label = if speed { format!("{} speed", j.name) } else { j.name.clone() };
        match (slider, speed) {
            // a drive's track runs a whole travel a second either way, a hinge's a turn a second
            (true, true) => {
                let travel = (j.upper - j.lower).abs();
                MotorScale { label, min: -travel, max: travel, step: 0.5, precision: 1.0, suffix: " mm/s", per: 1.0 }
            }
            (true, false) => MotorScale { label, min: j.lower, max: j.upper, step: 0.5, precision: 1.0, suffix: " mm", per: 1.0 },
            (false, true) => MotorScale { label, min: -360.0, max: 360.0, step: 5.0, precision: 0.0, suffix: "°/s", per: 1f64.to_degrees() },
            (false, false) => {
                // a hinge free all the way round sets any angle of one turn
                let (lower, upper) = (j.lower.to_degrees(), j.upper.to_degrees());
                let (min, max) = if lower.is_finite() && upper.is_finite() && upper - lower < 360.0 { (lower, upper) } else { (-180.0, 180.0) };
                MotorScale { label, min, max, step: 1.0, precision: 0.0, suffix: "°", per: 1f64.to_degrees() }
            }
        }
    }
}

/// The narrowest a track beside its name may be (and the gap between them); narrower, the name
/// goes above it.
const MOTOR_TRACK_MIN: f64 = 90.0;
const MOTOR_NAME_GAP: f64 = 8.0;

/// The piece's motors, a row each: its track sets where a servo goes or how fast a drive turns.
#[derive(Script, Widget)]
pub struct FabMechMotors {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    /// The names' text, measured to choose the rows' layout.
    #[live]
    draw_name: DrawText,
    #[rust]
    templates: HashMap<LiveId, ScriptObjectRef>,
    /// Each row's widget, by its id and template.
    #[rust]
    items: HashMap<(LiveId, LiveId), WidgetRef>,
    /// The rows of the last draw: the joint's index in `MechView::joints`, the row's widget and scale.
    #[rust]
    shown: Vec<(usize, (LiveId, LiveId), MotorScale)>,
    /// What each row's track was last configured with, and its name's width beside it.
    #[rust]
    applied: HashMap<(LiveId, LiveId), (MotorScale, f64)>,
    /// Each name's width on one line.
    #[rust]
    widths: HashMap<String, f64>,
    #[redraw]
    #[rust]
    area: Area,
}

impl ScriptHook for FabMechMotors {
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
            for ((_, template), w) in self.items.iter_mut() {
                if let Some(t) = self.templates.get(template) {
                    let v: ScriptValue = t.as_object().into();
                    w.script_apply(vm, apply, scope, v);
                }
            }
            self.applied.clear();
            self.widths.clear();
        }
    }
}

impl FabMechMotors {
    fn item(&mut self, cx: &mut Cx, key: (LiveId, LiveId)) -> Option<WidgetRef> {
        if let Some(w) = self.items.get(&key) {
            return Some(w.clone());
        }
        // a row in the other layout gives way: one widget a motor, the one in the tree
        self.items.retain(|k, _| k.0 != key.0);
        self.applied.retain(|k, _| k.0 != key.0);
        let value: ScriptValue = self.templates.get(&key.1)?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        // a tree node under the panel: the semantic tree and `ui act` find it
        cx.widget_tree_insert_child(self.uid, key.0, w.clone());
        self.items.insert(key, w.clone());
        Some(w)
    }

    /// A name's width on one line.
    fn width_of(&mut self, cx: &mut Cx2d, name: &str) -> f64 {
        if let Some(w) = self.widths.get(name) {
            return *w;
        }
        let w = self.draw_name.prepare_single_line_run(cx, name).map_or(0.0, |r| r.width_in_lpxs as f64).ceil() + 1.0;
        self.widths.insert(name.to_string(), w);
        w
    }
}

impl Widget for FabMechMotors {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Motors");
        node.set_size = self.shown.len() as u32;
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for (joint, id, scale) in &self.shown {
            let Some(w) = self.items.get(id) else { continue };
            w.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                let num = w.value_input(cx, ids!(num));
                if let Some(v) = num.changed(actions).or_else(|| num.ended(actions)) {
                    cx.action(MechAction::Motor { joint: *joint, target: v / scale.per });
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let rows: Vec<(usize, LiveId, MotorScale, f64)> = state
            .mech
            .joints
            .iter()
            .enumerate()
            .filter_map(|(k, j)| {
                let m = j.motor?;
                let scale = MotorScale::of(j, m.speed);
                Some((k, LiveId::from_str(&scale.label), scale.clone(), m.target * scale.per))
            })
            .collect();
        cx.begin_turtle(walk, self.layout);
        // the names beside the tracks while the longest leaves a track room, else above them
        let names = rows.iter().fold(0.0_f64, |w, (_, _, scale, _)| w.max(self.width_of(cx, &scale.label)));
        let room = cx.turtle().inner_width();
        let beside = room.is_nan() || names + MOTOR_NAME_GAP + MOTOR_TRACK_MIN <= room;
        let (template, name_width) = if beside { (live_id!(Row), names) } else { (live_id!(Stacked), 0.0) };
        self.shown = rows.iter().map(|(k, id, scale, _)| (*k, (*id, template), scale.clone())).collect();
        // another piece's motors go
        let shown: Vec<LiveId> = rows.iter().map(|r| r.1).collect();
        self.items.retain(|k, _| shown.contains(&k.0));
        self.applied.retain(|k, _| shown.contains(&k.0));
        for (_, id, scale, value) in rows {
            let key = (id, template);
            let Some(w) = self.item(cx, key) else { continue };
            let mut num = w.widget(cx, ids!(num));
            let look = (scale.clone(), name_width);
            if self.applied.get(&key) != Some(&look) {
                w.label(cx, ids!(label)).set_text(cx, &scale.label);
                if beside {
                    let mut name = w.widget(cx, ids!(label));
                    script_apply_eval!(cx, name, { width: #(name_width) });
                }
                crate::ui::print_settings::name_control(cx, &num, &format!("{} motor", scale.label), "slider");
                let MotorScale { min, max, step, precision, suffix, .. } = scale.clone();
                let suffix = suffix.to_string();
                script_apply_eval!(cx, num, { min: #(min) max: #(max) step: #(step) precision: #(precision) suffix: #(suffix) });
                self.applied.insert(key, look);
            }
            // a track being dragged shows the drag; else where the motor is set
            let field = w.value_input(cx, ids!(num));
            if !field.is_interacting() && (field.value() - value).abs() > 1e-9 {
                field.set_value(cx, value);
            }
            w.draw_all(cx, &mut Scope::empty());
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

const PIECE_ROW_H: f64 = 34.0;

/// The support pieces and what the check says of each, the ones that won't come out first.
#[derive(Script, ScriptHook, Widget)]
pub struct FabSupportPieces {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_dim: DrawText,
    #[rust]
    text: String,
}

fn worst_first(light: MechLight) -> u8 {
    match light {
        MechLight::Red => 0,
        MechLight::Amber => 1,
        MechLight::Waiting => 2,
        MechLight::Green => 3,
    }
}

fn words(p: &MechPieceView) -> &str {
    if p.words.is_empty() { "checking…" } else { &p.words }
}

impl Widget for FabSupportPieces {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::StaticText;
        makepad_platform::a11y::set_string(&mut node.name, "Support pieces");
        makepad_platform::a11y::set_string(&mut node.value, &self.text);
    }

    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let mut order: Vec<&MechPieceView> = state.mech.pieces.iter().collect();
        order.sort_by_key(|p| worst_first(p.light));
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(order.len() as f64 * PIECE_ROW_H), ..walk });
        self.text.clear();
        let mut y = rect.pos.y;
        for p in order {
            self.draw_chip.spool = 0.0;
            self.draw_chip.empty = 0.0;
            self.draw_chip.fill = match p.light {
                MechLight::Waiting => vec4(0.5, 0.5, 0.5, 1.0),
                MechLight::Green => vec4(0.36, 0.72, 0.42, 1.0),
                MechLight::Amber => vec4(0.878, 0.627, 0.125, 1.0),
                MechLight::Red => vec4(0.878, 0.251, 0.251, 1.0),
            };
            self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.4);
            self.draw_chip.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + 2.0, y + 4.0), size: dvec2(10.0, 10.0) });
            self.draw_text.draw_abs(cx, dvec2(rect.pos.x + 18.0, y + 3.0), &p.label);
            self.draw_dim.draw_abs(cx, dvec2(rect.pos.x + 18.0, y + 17.0), words(p));
            self.text.push_str(&format!("{}: {}\n", p.label, words(p)));
            y += PIECE_ROW_H;
        }
        DrawStep::done()
    }
}

impl Widget for FabMechanicsPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Mechanics");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if self.view.fab_busy_line(cx, ids!(body.check)).acted(actions) {
            cx.action(MechAction::CheckJoints);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(s) = scope.data.get::<AppState>() {
            let m = &s.mech;
            let b = self.view.view(cx, ids!(body));
            let c = &m.check;
            b.widget(cx, ids!(check)).set_visible(cx, c.joints > 0);
            self.view.fab_busy_line(cx, ids!(body.check)).set(cx, c.job, &check_line(c), CHECK_TIP, Some("Check joints"));
            b.label(cx, ids!(check_rows)).set_visible(cx, c.joints > 0 && !c.rows.is_empty());
            b.label(cx, ids!(check_rows)).set_text(cx, &c.rows.join("\n"));
            let status = if m.ready {
                let mut status = format!("{:.2} s", m.time);
                if m.lead.is_some() {
                    status.push_str("  led by the board's bench");
                } else if (m.speed - 1.0).abs() > 1e-6 {
                    status.push_str(&format!(" at {}x", m.speed));
                }
                status
            } else {
                m.status.clone()
            };
            let status = if m.ready && !m.pinned.is_empty() { format!("{status}  {} pinned", m.pinned.len()) } else { status };
            let status = if m.ready && !m.status.is_empty() { format!("{status}  {}", m.status) } else { status };
            b.label(cx, ids!(status)).set_text(cx, &status);
            b.label(cx, ids!(verdict)).set_visible(cx, !m.verdict.is_empty());
            b.label(cx, ids!(verdict)).set_text(cx, &m.verdict);
            b.label(cx, ids!(joints)).set_visible(cx, !m.joints.is_empty() || !m.strings.is_empty());
            b.label(cx, ids!(joints)).set_text(cx, &joint_lines(m));
            b.label(cx, ids!(motors_title)).set_visible(cx, m.joints.iter().any(|j| j.motor.is_some()));
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
