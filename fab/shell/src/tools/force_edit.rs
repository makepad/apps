//! The Force tool's load editor (`FabForceEditor`).
//!
//! The picked load has handles on its glyph. A force has a ring at its arrow's free end, which
//! turns its direction (snapping to the face's normal and to +/-X, Y and Z within 5 degrees;
//! Ctrl or Cmd turns freely), and a square on the shaft, which sets its size. A pressure has a
//! square on the normal; dragged past the face, a push becomes a pull. A torque has a dot at
//! its arc's end, which follows the pointer round the axis; turned back past the start, it
//! turns the other way. The value shows by the handle while it is dragged, and it goes into the
//! part's script on release.
//!
//! A double-click on a load (or on a handle) opens a small card by its glyph to type its size,
//! and a force's X, Y and Z (N, printer frame). A field written (Enter, or leaving it) goes
//! into the part's script at once. Esc or a press outside the card closes it.

use crate::api::*;
use crate::api::PaneId;
use crate::handle_style::*;
use crate::tools::handles::DrawHandlePrim;
use crate::tools::overlay::{glyph_len, torque_frame, PRESSURE_LEN, TORQUE_RADIUS, TORQUE_SWEEP};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    let Num = ValueInput{ width: Fill height: 22 min: 0.0 max: 0.0 step: 1.0 precision: 2 }
    let Axis = Num{ precision: 1 }

    mod.widgets.FabForceEditorBase = #(FabForceEditor::register_widget(vm))
    mod.widgets.FabForceEditor = set_type_default() do mod.widgets.FabForceEditorBase{
        width: Fill
        height: Fill
        draw_prim: mod.draw.DrawHandlePrim{}
        draw_tag: mod.draw.DrawHandlePrim{ kind: #(HandlePrim::RoundedSquare as u32 as f32) }
        draw_text: mod.draw.DrawText{
            color: #xffffff
            text_style: theme.font_bold{ font_size: fab.font_size_ui }
        }
        // floats over the view: its own batch, so the view's draws stay under it
        card: View{
            width: 270 height: Fit flow: Down spacing: 4 padding: 8
            new_batch: true
            cursor: MouseCursor.Default
            show_bg: true
            draw_bg +: {
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                    sdf.fill_keep(vec4(fab.color_float.xyz, 0.94))
                    sdf.stroke(fab.color_float_border, 1.0)
                    return sdf.result
                }
            }
            title := mod.widgets.FabLabel{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
            sized := View{
                width: Fill height: 22 flow: Right spacing: 6 align: Align{y: 0.5}
                size := Num{ label: "Size" }
                unit := mod.widgets.FabLabelDim{ width: 44 height: Fit text: "" }
            }
            xyz := View{
                width: Fill height: 22 flow: Right spacing: 3
                x := Axis{ label: "X" }
                y := Axis{ label: "Y" }
                z := Axis{ label: "Z" }
            }
        }
    }
}

/// A load the editor works on, while the Force tool shows its part: one the script writes at
/// its top, not a fixture.
fn editable(state: &AppState, i: Option<usize>) -> Option<(usize, ForceLoad)> {
    let i = i?;
    let l = state.force.loads.get(i).filter(|l| l.top && l.kind != ForceKind::Fixed)?;
    (state.force_tool() && state.force.scene.is_some() && state.heatmap_part().is_some()).then(|| (i, l.clone()))
}

/// The load the card is open on.
fn open(state: &AppState) -> Option<(usize, ForceLoad)> {
    editable(state, state.force.typing)
}

/// The load with handles: the one picked.
fn handled(state: &AppState) -> Option<(usize, ForceLoad)> {
    editable(state, state.force.focus)
}

/// Grab radius around a grip (points).
const GRAB: f64 = 10.0;
/// How near (degrees) a turned force snaps to the face's normal or an axis.
const SNAP_DEG: f64 = 5.0;

/// A grip of the picked load's handles.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Grip {
    /// A force's direction: the ring at its arrow's free end.
    Turn,
    /// A force's or a pressure's size: the square on its shaft.
    Size,
    /// A torque's: the dot at its arc's end.
    Spin,
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    grip: Grip,
    load: usize,
    /// The load's value when the press came.
    from: [f64; 3],
    /// The value shown now (the script gets it on release).
    value: [f64; 3],
    /// Where the grip is: Size, its distance from the load along the shaft (a pressure's
    /// signed, along the normal); Spin, the arc's angle, unwrapped.
    at: f64,
    /// Turn: the ring on the far half of the sphere about the load.
    back: bool,
    /// Turn: the pointer is off the sphere's outline (coming back on rolls the ring over).
    outside: bool,
    /// Turn: what the direction snapped to.
    snapped: Option<&'static str>,
}

type V3 = [f64; 3];

fn add(a: V3, b: V3) -> V3 { [a[0] + b[0], a[1] + b[1], a[2] + b[2]] }
fn sub(a: V3, b: V3) -> V3 { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn mul(a: V3, k: f64) -> V3 { [a[0] * k, a[1] * k, a[2] * k] }
fn dot(a: V3, b: V3) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn len(a: V3) -> f64 { dot(a, a).sqrt() }
fn unit(a: V3) -> V3 { mul(a, 1.0 / len(a).max(1e-12)) }

/// A point in plate mm to the projector's metres.
fn m(p: V3) -> Vec3f {
    vec3((p[0] * 1e-3) as f32, (p[1] * 1e-3) as f32, (p[2] * 1e-3) as f32)
}

/// The pointer's ray in plate mm: its origin and unit direction.
fn mm_ray(proj: &ViewProjector, at: DVec2) -> (V3, V3) {
    let r = proj.ray(at);
    ([r.origin.x as f64 * 1e3, r.origin.y as f64 * 1e3, r.origin.z as f64 * 1e3], unit([r.dir.x as f64, r.dir.y as f64, r.dir.z as f64]))
}

/// How far along the line through `c` along unit `u` the point nearest the ray lies; None when
/// the ray runs along the line.
fn along(c: V3, u: V3, (o, d): (V3, V3)) -> Option<f64> {
    let w = sub(c, o);
    let b = dot(u, d);
    let denom = 1.0 - b * b;
    (denom > 1e-6).then(|| (b * dot(d, w) - dot(u, w)) / denom)
}

/// `v` to `digits` significant digits.
fn round_to(v: f64, digits: i32) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let k = 10f64.powi(digits - 1 - v.abs().log10().floor() as i32);
    (v * k).round() / k
}

/// A dragged size, rounded as the modifiers say (Ctrl or Cmd: free, Shift: fine) and kept in
/// the kind's range.
fn sized(kind: ForceKind, v: f64, free: bool, fine: bool) -> f64 {
    let v = if free { v } else { round_to(v, if fine { 3 } else { 2 }) };
    kind.sizes().map_or(v, |(lo, hi)| v.clamp(lo, hi))
}

/// A force's direction snapped to the face's normal or an axis within [`SNAP_DEG`], and what it
/// snapped to.
fn snap_dir(dir: V3, normal: V3) -> (V3, Option<&'static str>) {
    let n = unit(normal);
    let to = [
        (mul(n, -1.0), "into the face"),
        (n, "out of the face"),
        ([1.0, 0.0, 0.0], "+X"),
        ([-1.0, 0.0, 0.0], "-X"),
        ([0.0, 1.0, 0.0], "+Y"),
        ([0.0, -1.0, 0.0], "-Y"),
        ([0.0, 0.0, 1.0], "+Z"),
        ([0.0, 0.0, -1.0], "-Z"),
    ];
    let near = SNAP_DEG.to_radians().cos();
    to.into_iter().find(|(c, _)| dot(dir, *c) >= near).map_or((dir, None), |(c, name)| (c, Some(name)))
}

/// A force's size along its direction; a pressure's or a torque's signed value.
fn size_of(l: &ForceLoad) -> f64 {
    if l.kind == ForceKind::Force { l.size() } else { l.value[0] }
}

/// `v` as its field shows it, to `places` decimals.
fn shown(v: f64, places: i32) -> f64 {
    let k = 10f64.powi(places);
    (v * k).round() / k
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabForceEditor {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[rust]
    area: Area,
    #[rust(PaneId::PLATE_ZERO)]
    pub pane: PaneId,
    #[live]
    draw_prim: DrawHandlePrim,
    #[live]
    draw_tag: DrawHandlePrim,
    #[live]
    draw_text: DrawText,
    #[live]
    card: View,
    /// The picked load's grips as last drawn.
    #[rust]
    grips: Vec<(Grip, DVec2)>,
    #[rust]
    hover: Option<Grip>,
    #[rust]
    drag: Option<Drag>,

    /// The card as last drawn: a press outside it closes it.
    #[rust]
    drawn: Option<Rect>,
    /// The load the fields hold, and its value then (refilled when the script changes it).
    #[rust]
    loaded: Option<(usize, [f64; 3])>,
}

impl Widget for FabForceEditor {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Load editor");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.handle_grips(cx, event, scope);
        let Some((i, l)) = scope.data.get::<AppState>().and_then(open) else {
            self.loaded = None;
            return;
        };
        let close = match event {
            Event::MouseDown(e) => self.drawn.is_some_and(|r| !r.contains(e.abs)) && self.grip_at(e.abs).is_none(),
            Event::KeyDown(k) => k.key_code == KeyCode::Escape,
            _ => false,
        };
        if close {
            cx.action(ForceAction::Type(None));
            self.area.redraw(cx);
            return;
        }
        let actions = cx.capture_actions(|cx| self.card.handle_event(cx, event, &mut Scope::empty()));
        let c = &self.card;
        let mut value = None;
        // a field left as it was shown (rounded) keeps the script's value
        if let Some(v) = c.value_input(cx, ids!(sized.size)).ended(&actions).filter(|v| *v != shown(size_of(&l), 2)) {
            value = Some(match l.kind {
                // a force's size along its direction; a pressure or a torque typed with its sign
                ForceKind::Force => l.sized(v.abs()),
                _ => [v, 0.0, 0.0],
            });
        }
        for (k, axis) in [ids!(xyz.x), ids!(xyz.y), ids!(xyz.z)].into_iter().enumerate() {
            if let Some(v) = c.value_input(cx, axis).ended(&actions).filter(|v| *v != shown(l.value[k], 1)) {
                let mut now = value.unwrap_or(l.value);
                now[k] = v;
                value = Some(now);
            }
        }
        if let Some(v) = value.filter(|v| *v != l.value) {
            cx.action(ForceAction::Value(i, v));
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        self.drawn = None;
        self.grips.clear();
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        self.draw_grips(cx, state, rect);
        let Some((i, l)) = open(state) else {
            self.loaded = None;
            return DrawStep::done();
        };
        let proj = ViewProjector::new(*state.view.camera(self.pane), rect);
        let Some(at) = proj.project(vec3((l.centroid[0] * 1e-3) as f32, (l.centroid[1] * 1e-3) as f32, (l.centroid[2] * 1e-3) as f32)) else { return DrawStep::done() };
        let c = &self.card;
        let force = l.kind == ForceKind::Force;
        if self.loaded != Some((i, l.value)) {
            let first = self.loaded.is_none_or(|(was, _)| was != i);
            c.value_input(cx, ids!(sized.size)).set_value(cx, shown(size_of(&l), 2));
            for (k, axis) in [ids!(xyz.x), ids!(xyz.y), ids!(xyz.z)].into_iter().enumerate() {
                c.value_input(cx, axis).set_value(cx, shown(l.value[k], 1));
            }
            if first {
                c.value_input(cx, ids!(sized.size)).begin_edit(cx);
            }
            self.loaded = Some((i, l.value));
        }
        c.label(cx, ids!(title)).set_text(cx, &format!("{} on {:.0} mm\u{b2}", l.kind.label(), l.area));
        c.label(cx, ids!(sized.unit)).set_text(cx, l.kind.unit());
        c.view(cx, ids!(xyz)).set_visible(cx, force);
        // beside its glyph, inside the view
        let pos = at + dvec2(16.0, -16.0);
        let pos = dvec2(pos.x.clamp(rect.pos.x, (rect.pos.x + rect.size.x - 270.0).max(rect.pos.x)), pos.y.clamp(rect.pos.y, (rect.pos.y + rect.size.y - 90.0).max(rect.pos.y)));
        let mut walk = self.card.walk(cx);
        walk.abs_pos = Some(pos);
        let _ = self.card.draw_walk(cx, &mut Scope::empty(), walk);
        self.drawn = Some(self.card.area().rect(cx));
        DrawStep::done()
    }
}

impl FabForceEditor {
    fn grip_at(&self, p: DVec2) -> Option<Grip> {
        self.grips.iter().filter(|(_, at)| (*at - p).length() < GRAB).min_by(|a, b| (a.1 - p).length().total_cmp(&(b.1 - p).length())).map(|(g, _)| *g)
    }

    /// Presses on the picked load's grips, and their drags. Anything else falls through to the
    /// view (picking, placing, orbiting).
    fn handle_grips(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        let Some((i, l)) = handled(state).filter(|(i, _)| self.drag.is_none_or(|d| d.load == *i)) else {
            if self.drag.take().is_some() {
                state.force.held = None;
                crate::ui::redraw(cx, &[ids!(tool_overlay)]);
            }
            self.hover = None;
            state.force.grip = false;
            return;
        };
        match event {
            Event::MouseMove(e) if self.drag.is_none() => {
                let hover = self.grip_at(e.abs);
                state.force.grip = hover.is_some();
                if hover != self.hover {
                    self.hover = hover;
                    self.area.redraw(cx);
                }
                return;
            }
            Event::MouseDown(e) if self.grip_at(e.abs).is_none() => return,
            Event::TouchUpdate(e) if self.drag.is_none() && !e.touches.iter().any(|t| self.grip_at(t.abs).is_some()) => return,
            _ => {}
        }
        let Some(scene) = state.force.scene.clone() else { return };
        let reach = glyph_len(&scene);
        let proj = ViewProjector::new(*state.view.camera(self.pane), self.area.rect(cx));
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                let Some(grip) = self.grip_at(fe.abs) else { return };
                if fe.tap_count >= 2 {
                    cx.action(ForceAction::Type(Some(i)));
                    return;
                }
                let (_, d) = mm_ray(&proj, fe.abs);
                let tail = sub(l.centroid, mul(unit(l.value), reach));
                let at = match grip {
                    Grip::Turn => 0.0,
                    Grip::Size if l.kind == ForceKind::Force => reach * 0.5,
                    Grip::Size => reach * PRESSURE_LEN * if l.value[0] >= 0.0 { 1.0 } else { -1.0 },
                    Grip::Spin => TORQUE_SWEEP * if l.value[0] >= 0.0 { 1.0 } else { -1.0 },
                };
                self.drag = Some(Drag { grip, load: i, from: l.value, value: l.value, at, back: dot(sub(tail, l.centroid), d) > 0.0, outside: false, snapped: None });
                state.force.grip = true;
                self.area.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                let Some(mut drag) = self.drag else { return };
                let ray = mm_ray(&proj, fe.abs);
                let free = fe.modifiers.control || fe.modifiers.logo;
                let c = l.centroid;
                match drag.grip {
                    Grip::Turn => {
                        // the ring on the sphere about the load, on the half it started on; off the
                        // outline it stays on the rim, and coming back on rolls it over
                        let (o, d) = ray;
                        let oc = sub(o, c);
                        let b = dot(d, oc);
                        let disc = b * b - (dot(oc, oc) - reach * reach);
                        let q = if disc < 0.0 {
                            drag.outside = true;
                            add(c, mul(unit(sub(add(o, mul(d, -b)), c)), reach))
                        } else {
                            if drag.outside {
                                drag.outside = false;
                                drag.back = !drag.back;
                            }
                            add(o, mul(d, -b + if drag.back { disc.sqrt() } else { -disc.sqrt() }))
                        };
                        let dir = unit(sub(c, q));
                        let (dir, snapped) = if free { (dir, None) } else { snap_dir(dir, l.normal) };
                        drag.snapped = snapped;
                        drag.value = mul(dir, len(drag.from));
                    }
                    Grip::Size if l.kind == ForceKind::Force => {
                        let Some(s) = along(c, mul(unit(drag.from), -1.0), ray) else { return };
                        drag.at = s.max(0.0);
                        let size = sized(ForceKind::Force, len(drag.from) * drag.at / (reach * 0.5), free, fe.modifiers.shift);
                        drag.value = mul(unit(drag.from), size);
                    }
                    Grip::Size => {
                        let Some(s) = along(c, unit(l.normal), ray) else { return };
                        drag.at = s;
                        let size = sized(l.kind, drag.from[0].abs() * s.abs() / (reach * PRESSURE_LEN), free, fe.modifiers.shift);
                        drag.value = [if s >= 0.0 { size } else { -size }, 0.0, 0.0];
                    }
                    Grip::Spin => {
                        // the pointer on the arc's plane, its angle unwrapped from the last
                        let (o, d) = ray;
                        let axis = unit(l.axis.unwrap_or(l.normal));
                        let den = dot(d, axis);
                        if den.abs() < 1e-6 {
                            return;
                        }
                        let q = add(o, mul(d, dot(sub(c, o), axis) / den));
                        let (t, b) = torque_frame(&l);
                        let a = dot(sub(q, c), b).atan2(dot(sub(q, c), t));
                        let tau = std::f64::consts::TAU;
                        drag.at += (a - drag.at).rem_euclid(tau) - if (a - drag.at).rem_euclid(tau) > std::f64::consts::PI { tau } else { 0.0 };
                        let size = sized(l.kind, drag.from[0].abs() * drag.at.abs() / TORQUE_SWEEP, free, fe.modifiers.shift);
                        drag.value = [if drag.at >= 0.0 { size } else { -size }, 0.0, 0.0];
                    }
                }
                self.drag = Some(drag);
                state.force.held = Some((i, drag.value));
                self.area.redraw(cx);
                crate::ui::redraw(cx, &[ids!(tool_overlay)]);
            }
            Hit::FingerUp(fe) => {
                let Some(drag) = self.drag.take() else { return };
                state.force.held = None;
                state.force.grip = self.grip_at(fe.abs).is_some();
                if !fe.cancelled && drag.value != drag.from {
                    cx.action(ForceAction::Value(drag.load, drag.value));
                }
                self.area.redraw(cx);
                crate::ui::redraw(cx, &[ids!(tool_overlay)]);
            }
            _ => {}
        }
    }

    /// The picked load's handles over its glyph, and while one is dragged its value by it.
    fn draw_grips(&mut self, cx: &mut Cx2d, state: &AppState, rect: Rect) {
        let Some((i, l)) = handled(state) else { return };
        let Some(scene) = state.force.scene.clone() else { return };
        let proj = ViewProjector::new(*state.view.camera(self.pane), rect);
        let reach = glyph_len(&scene);
        let drag = self.drag.filter(|d| d.load == i);
        let value = drag.map_or(l.value, |d| d.value);
        let c = l.centroid;
        let sign = if value[0] >= 0.0 { 1.0 } else { -1.0 };
        let mut grips: Vec<(Grip, V3)> = Vec::new();
        match l.kind {
            ForceKind::Force if len(value) > 0.0 => {
                let dir = unit(value);
                grips.push((Grip::Turn, sub(c, mul(dir, reach))));
                grips.push((Grip::Size, sub(c, mul(dir, drag.filter(|d| d.grip == Grip::Size).map_or(reach * 0.5, |d| d.at)))));
            }
            ForceKind::Pressure => {
                let at = drag.filter(|d| d.grip == Grip::Size).map_or(reach * PRESSURE_LEN * sign, |d| d.at);
                grips.push((Grip::Size, add(c, mul(unit(l.normal), at))));
            }
            ForceKind::Torque => {
                let (t, b) = torque_frame(&l);
                let a = drag.map_or(TORQUE_SWEEP * sign, |d| d.at);
                grips.push((Grip::Spin, add(c, mul(add(mul(t, a.cos()), mul(b, a.sin())), TORQUE_RADIUS * reach))));
            }
            _ => {}
        }
        let held = drag.map(|d| d.grip);
        for (grip, p) in grips {
            let Some(at) = proj.project(m(p)) else { continue };
            let lit = held == Some(grip) || (held.is_none() && self.hover == Some(grip));
            let col = if lit { COL_HOT } else if grip == Grip::Turn { RING } else { SCALE };
            let col = vec4(col[0], col[1], col[2], col[3]);
            let r = if lit { 7.0 * HOVER_SCALE as f64 } else { 7.0 };
            self.draw_prim.color = col;
            match grip {
                // rings, so the arrow head under them still shows
                Grip::Turn | Grip::Spin => {
                    let rim = |k: usize| at + dvec2((k as f64 * std::f64::consts::TAU / 16.0).cos(), (k as f64 * std::f64::consts::TAU / 16.0).sin()) * (r - 1.5);
                    for k in 0..16 {
                        self.draw_prim.set_segment(cx, rim(k), rim(k + 1), 2.5, col);
                    }
                }
                Grip::Size => {
                    self.draw_prim.kind = HandlePrim::RoundedSquare as u32 as f32;
                    let r = r * 0.8;
                    self.draw_prim.draw_abs(cx, Rect { pos: at - dvec2(r, r), size: dvec2(r * 2.0, r * 2.0) });
                }
            }
            self.grips.push((grip, at));
            if let Some(d) = drag.filter(|d| d.grip == grip) {
                let size = if l.kind == ForceKind::Force { len(d.value) } else { d.value[0] };
                let text = match (grip, d.snapped) {
                    (Grip::Turn, Some(to)) => format!("{} {}, {to}", crate::ui::force::size_text(size), l.kind.unit()),
                    (Grip::Turn, None) => format!("{} {}, {:.0}\u{b0} off the face", crate::ui::force::size_text(size), l.kind.unit(), dot(unit(d.value), mul(unit(l.normal), -1.0)).clamp(-1.0, 1.0).acos().to_degrees()),
                    _ => format!("{} {}", crate::ui::force::size_text(size), l.kind.unit()),
                };
                self.label(cx, at, &text);
            }
        }
    }

    fn label(&mut self, cx: &mut Cx2d, at: DVec2, text: &str) {
        let tw = self.draw_text.prepare_single_line_run(cx, text).map(|r| r.width_in_lpxs as f64).unwrap_or(60.0);
        self.draw_tag.color = vec4(LABEL_BG[0], LABEL_BG[1], LABEL_BG[2], LABEL_BG[3]);
        let r = Rect { pos: at + dvec2(14.0, -28.0), size: dvec2(tw + 14.0, 22.0) };
        self.draw_tag.draw_abs(cx, r);
        self.draw_text.draw_abs(cx, r.pos + dvec2(7.0, 5.0), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turned_force_snaps_to_the_face_and_the_axes_and_a_dragged_size_rounds() {
        let up = [0.0, 0.0, 1.0];
        let near = |deg: f64| [deg.to_radians().sin(), 0.0, -deg.to_radians().cos()];
        assert_eq!(snap_dir(near(4.0), up), ([0.0, 0.0, -1.0], Some("into the face")));
        assert_eq!(snap_dir(near(6.0), up).1, None, "6 degrees off turns freely");
        assert_eq!(snap_dir(unit([1.0, 0.03, 0.0]), up).1, Some("+X"));
        // the pointer's ray straight down through (5, 0, 0) is nearest the X axis at 5
        assert!((along([0.0; 3], [1.0, 0.0, 0.0], ([5.0, 0.0, 10.0], [0.0, 0.0, -1.0])).unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(along([0.0; 3], [0.0, 0.0, 1.0], ([5.0, 0.0, 10.0], [0.0, 0.0, -1.0])), None, "a ray along the line");
        assert_eq!(sized(ForceKind::Force, 87.34, false, false), 87.0);
        assert_eq!(sized(ForceKind::Force, 87.34, false, true), 87.3);
        assert_eq!(sized(ForceKind::Force, 0.0, false, false), 0.1, "a force keeps its smallest size");
        assert_eq!(sized(ForceKind::Pressure, 0.4567, true, false), 0.4567, "Ctrl drags freely");
    }
}
