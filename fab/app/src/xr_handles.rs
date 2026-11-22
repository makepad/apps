//! Shape handles in the headset: the selected part's parameter handles (the same ones the
//! desktop view shows, from [`fab::params`]) taken at the hand's tip with a controller's trigger or grip, or
//! a hand's pinch ([`makepad_xr_ui::input::grab`]). Free tips light the handle in reach. Plain state stepped once per headset frame before the part grabs ([`XrHandles::step`]):
//! a hand that holds a handle is masked from the part grab and the navigation.
//!
//! While held, the hand drives the value through the same drag math as the desktop
//! ([`fab::params::drag_value`] with the hand as a point), snapped to round values (the trigger
//! half pressed: fine steps), with a haptic tick on every snap point passed and a stronger one
//! when a face of another feature is met. The value goes out as a preview every frame it changes
//! (the app's coalescing slot: the mesh and the live slice follow the newest) and commits as one
//! undo step on release. A label floats by the hand.

use crate::xr_edit::{self, Affine};
use fab::params::*;
use makepad_widgets::makepad_platform::event::xr::{XrController, XrHand, XrState};
use makepad_widgets::*;
use makepad_xr_nav::XrNavView;
use makepad_xr_ui::haptic::{self, Haptics};
use makepad_xr_ui::input::{grab, tip, Buttons};

/// How near a hand's tip must be to a handle's grip to take it (room metres).
pub const REACH_M: f32 = 0.035;

/// Visual radius derives from the existing reach sphere, in room metres (independent of
/// model zoom). Angular size is steady around arm's length; clamps keep close/far grips usable.
pub fn ball_radius(distance: f32, reach: f32) -> f32 {
    reach * 0.2 * (distance / 0.6).clamp(0.7, 1.5)
}

#[derive(Clone, Debug)]
struct Hold {
    hand: usize,
    object: String,
    key: String,
    grip: HandleGrip,
    /// The grip point minus the hand when taken (printer mm): no jump on taking it.
    offset: V3,
    value: ParamValue,
    previewed: bool,
    snapped_edge: bool,
}

/// The per-frame answer.
#[derive(Clone, Debug, Default)]
pub struct HandlesOut {
    /// A new live value (object, parameter key, value; send as a preview).
    pub preview: Option<(String, String, ParamValue)>,
    /// The release: commit (object, parameter key, value) as one undo step.
    pub commit: Option<(String, String, ParamValue)>,
    /// An abandoned preview: restore this object's committed value.
    pub cancel: Option<String>,
    /// Hands holding a handle: no part grab, no navigation for them.
    pub consumed: [bool; 2],
    /// A snap per step passed, a limit on meeting another feature's face, a grab on taking it.
    pub haptics: Haptics,
    /// The value label: the point it stands by (room metres; the chip places itself beside it,
    /// `xr::chip_card`) and what.
    pub label: Option<(Vec3f, String)>,
    /// The handle a free hand is near (it lights up).
    pub near: Option<String>,
    /// The hands whose tip is on a handle's grip (or holds one).
    pub touch: [bool; 2],
    /// The exact grip chosen by the existing nearest-in-reach test, including profile index.
    pub grip: Option<(HandleGrip, V3)>,
}

#[derive(Default)]
pub struct XrHandles {
    hold: Option<Hold>,
    grip_down: [bool; 2],
    count_down: [bool; 2],
}

/// Where a handle's grips are (printer mm): the handle's one grip, or each profile point.
fn grips(p: &ParamView, view_dir: V3, eye: V3, scale: f32) -> Vec<(HandleGrip, V3)> {
    match &p.handle {
        Some(ParamHandle::Profile { points, .. }) => (0..points.len()).map(|i| (HandleGrip { point: i }, grip_point(p.handle.as_ref().unwrap(), HandleGrip { point: i }, view_dir))).collect(),
        Some(ParamHandle::Count { at, axis }) => count_grips(*at, *axis, eye, scale).into_iter().enumerate().map(|(point, at)| (HandleGrip { point }, at)).collect(),
        None => Vec::new(),
        Some(h) => vec![(HandleGrip::default(), grip_point(h, HandleGrip::default(), view_dir))],
    }
}

fn hand_point(c: &XrController, h: &XrHand, down: bool) -> Option<Vec3f> {
    grab(c, h, down).map(|p| p.position)
}

fn fine(c: &XrController) -> bool {
    c.active() && c.trigger > 0.2 && c.trigger < 0.55
}

impl XrHandles {
    /// The parameter a hand holds.
    pub fn held_key(&self) -> Option<&str> {
        self.hold.as_ref().map(|h| h.key.as_str())
    }

    /// The hand holding a handle.
    pub fn held_hand(&self) -> Option<usize> {
        self.hold.as_ref().map(|h| h.hand)
    }

    /// One frame. `pp` is the selected part's parameters (printer mm on the plate), `shown`
    /// the ones with handles showing, `head` the eye (room metres) for ring grips and labels,
    /// `delta` the hands' move of the part not yet in its plate (the part and its handles are
    /// drawn moved by it).
    #[allow(clippy::too_many_arguments)]
    pub fn step(&mut self, state: &XrState, view: &XrNavView, pp: Option<&PartParamsView>, shown: &[&ParamView], head: Vec3f, delta: &Affine, buttons: &Buttons, out: &mut HandlesOut) {
        *out = HandlesOut::default();
        let ctrl = [&state.left_controller, &state.right_controller];
        let hand = [&state.left_hand, &state.right_hand];
        let mm_per_m = 1000.0 / view.scale.max(1e-6) as f64;
        // room metres -> the part's printer mm as its plate has it
        let back = xr_edit::inverse(delta);
        let printer_from_room = |view: &XrNavView, r: Vec3f| xr_edit::apply(&back, xr_edit::printer_from_room(view, r));
        let eye = printer_from_room(view, head);
        let downs: [Option<Vec3f>; 2] = [hand_point(ctrl[0], hand[0], self.grip_down[0]), hand_point(ctrl[1], hand[1], self.grip_down[1])];
        let pressed = [downs[0].is_some() && !self.grip_down[0], downs[1].is_some() && !self.grip_down[1]];
        let released = [downs[0].is_none() && self.grip_down[0], downs[1].is_none() && self.grip_down[1]];
        self.grip_down = [downs[0].is_some(), downs[1].is_some()];
        for i in 0..2 {
            if self.count_down[i] { out.consumed[i] = true; }
            if !self.grip_down[i] { self.count_down[i] = false; }
        }

        // the hold: follow the hand, release commits
        if let Some(mut hold) = self.hold.take() {
            let i = hold.hand;
            out.consumed[i] = true;
            out.touch[i] = true;
            let param = pp.filter(|pp| pp.object == hold.object).and_then(|pp| pp.param(&hold.key)).filter(|p| p.handle.is_some());
            if released[i] || param.is_none() {
                if hold.previewed {
                    if released[i] {
                        out.commit = Some((hold.object, hold.key, hold.value));
                    } else {
                        out.cancel = Some(hold.object);
                    }
                }
                return;
            }
            let (p, pp) = (param.unwrap(), pp.unwrap());
            if let (Some(at), Some(h)) = (downs[i], &p.handle) {
                let hp = printer_from_room(view, at);
                let target = add(hp, hold.offset);
                let view_dir = unit(sub(eye, target));
                out.grip = Some((hold.grip, grip_point(h, hold.grip, view_dir)));
                if let Some(raw) = drag_value(h, &p.value, hold.grip, &DragPointer::point(target), view_dir) {
                    let is_fine = fine(ctrl[i]);
                    let mut edge = false;
                    let value = match raw {
                        ParamValue::Num(x) => {
                            let now = p.value.num().unwrap_or(x);
                            let tol = 0.004 * mm_per_m; // 4 mm of hand travel
                            let e = edge_snap(h, now, x, &pp.boxes, &p.objects, tol);
                            edge = e.is_some();
                            ParamValue::Num(clamp_value(p, e.unwrap_or_else(|| snap(x, snap_step(p, x, is_fine)))))
                        }
                        ParamValue::Point(c) => ParamValue::Point(c.iter().map(|x| snap(*x, if is_fine { 0.1 } else { 1.0 })).collect()),
                        ParamValue::Profile(mut pts) => {
                            if let Some(q) = pts.get_mut(hold.grip.point) {
                                *q = q.map(|x| snap(x, if is_fine { 0.1 } else { 0.5 }));
                            }
                            ParamValue::Profile(pts)
                        }
                        other => other,
                    };
                    if value != hold.value {
                        // a tick per snap point passed; a stronger one on meeting a face
                        out.haptics.add(i, if edge && !hold.snapped_edge { haptic::LIMIT } else { haptic::SNAP });
                        hold.value = value.clone();
                        hold.previewed = true;
                        out.preview = Some((hold.object.clone(), hold.key.clone(), value));
                    }
                    hold.snapped_edge = edge;
                }
                out.label = Some((at, label(p, &hold.value, hold.grip)));
            }
            self.hold = Some(hold);
            return;
        }

        // free hands: the nearest grip in reach lights up, a grip press takes it
        let Some(pp) = pp else { return };
        let reach_mm = REACH_M as f64 * mm_per_m;
        for i in 0..2 {
            let at = tip(ctrl[i], hand[i]).map(|p| p.position).or(downs[i]);
            let Some(at) = at else { continue };
            if out.consumed[i] { continue; }
            let hp = printer_from_room(view, at);
            let mut best: Option<(f64, &ParamView, HandleGrip, V3)> = None;
            for p in shown {
                let view_dir = unit(sub(eye, hp));
                for (g, gp) in grips(p, view_dir, eye, view.scale) {
                    let d = length(sub(gp, hp));
                    if d <= reach_mm && best.is_none_or(|b| d < b.0) {
                        best = Some((d, p, g, gp));
                    }
                }
            }
            let Some((_, p, g, gp)) = best else { continue };
            out.near = Some(p.key.clone());
            out.grip = Some((g, gp));
            out.label = Some((xr_edit::room_from_printer(view, xr_edit::apply(delta, gp)), label(p, &p.value, g)));
            out.touch[i] = true;
            if pressed[i] && !buttons.claimed(i) {
                // Touch at the index tip; carry a pinching hand at its midpoint without a jump.
                let at = downs[i].unwrap();
                let grab_hp = printer_from_room(view, at);
                out.consumed[i] = true;
                out.haptics.add(i, haptic::GRAB);
                out.label = Some((at, label(p, &p.value, g)));
                if matches!(&p.handle, Some(ParamHandle::Count { .. })) {
                    if let Some(value) = p.value.num() {
                        let next = clamp_value(p, value + if g.point == 0 { 1.0 } else { -1.0 });
                        if next != value { out.commit = Some((pp.object.clone(), p.key.clone(), ParamValue::Num(next))); }
                    }
                    self.count_down[i] = true;
                    return;
                }
                self.hold = Some(Hold { hand: i, object: pp.object.clone(), key: p.key.clone(), grip: g, offset: sub(gp, grab_hp), value: p.value.clone(), previewed: false, snapped_edge: false });
                return;
            }
        }
    }

}

fn label(p: &ParamView, v: &ParamValue, g: HandleGrip) -> String {
    match v {
        ParamValue::Profile(pts) => pts.get(g.point).map(|q| format!("{} {}: {:.1}, {:.1} mm", p.label, g.point + 1, q[0], q[1])).unwrap_or_default(),
        _ => format!("{}  {}", p.label, format_value(p, v)),
    }
}

/// The count buttons use the same metric as their interaction balls. Grip indices are + / −.
fn count_grips(at: V3, axis: V3, eye: V3, scale: f32) -> [V3; 2] {
    let distance = length(sub(eye, at)) as f32 * scale * 0.001;
    let spacing = ball_radius(distance, REACH_M) as f64 * 4.0 * 1000.0 / scale.max(1e-6) as f64;
    [add(at, mul(unit(axis), spacing)), add(at, mul(unit(axis), -spacing))]
}

fn color(c: fab::handle_style::Color) -> Vec4f { vec4(c[0], c[1], c[2], c[3]) }

/// Shared desktop ink, with the capture pulse expressed only in the outline.
pub fn shape_style(base: fab::handle_style::Color, state: fab::handle_style::HandleState, time: f64) -> makepad_xr_ui::XrShapeStyle {
    use fab::handle_style::*;
    let active = state == HandleState::Active;
    let pulse = if active { 0.5 + 0.5 * ((time * std::f64::consts::TAU * 1.5).sin() as f32) } else { 0.0 };
    makepad_xr_ui::XrShapeStyle {
        color: color(state.color(base)),
        outline: if active { vec4(1.0, 1.0, 1.0, 0.65 + 0.35 * pulse) } else { vec4(0.0, 0.0, 0.0, OUTLINE_ALPHA) },
        outline_px: OUTLINE_PX + pulse,
    }
}

/// Assemble the desktop definitions as room-space surfaces. Parameter locations and the
/// grab radius stay in the interaction model; only stroke/ball thickness has a visual scale.
#[allow(clippy::too_many_arguments)]
pub fn draw_handles(shapes: &mut makepad_xr_ui::XrShapes, state: Option<&fab::AppState>, edit: &crate::xr_edit::XrEdit, handles: &XrHandles, out: &HandlesOut, view: &XrNavView, transform: Mat4f, head: Vec3f, time: f64) {
    use fab::handle_style::*;
    let (Some(state), Some(i)) = (state, edit.selected.iter().position(|s| *s)) else { return };
    let Some(pp) = state.params.part(i).map(|pp| state.params.shown_part(pp)) else { return };
    let delta = edit.deltas.get(i).copied().unwrap_or(xr_edit::IDENTITY);
    let eye = xr_edit::apply(&xr_edit::inverse(&delta), xr_edit::printer_from_room(view, head));
    let room = |p: V3| {
        let p = xr_edit::apply(&delta, p);
        let v = transform.transform_vec4(vec4((p[0] * 0.001) as f32, (p[2] * 0.001) as f32, (-p[1] * 0.001) as f32, 1.0));
        vec3f(v.x, v.y, v.z)
    };
    let direction = |at: V3, v: V3| (room(add(at, v)) - room(at)).normalize();
    let radius = |p: Vec3f| ball_radius((p - head).length(), REACH_M);
    // Tool previews retain all edges, with continuous round strokes instead of cube beads.
    if let Some(g) = &state.params.ghost {
        let fade = g.fade_from.map_or(1.0, |t| 1.0 - ((time - t) / 0.4).clamp(0.0, 1.0)) as f32;
        if fade > 0.0 {
            for body in &g.bodies {
                let col = if body.subtract { [0.95, 0.30, 0.25, 0.85 * fade] } else { [0.30, 0.55, 1.0, 0.85 * fade] };
                for edge in &body.edges {
                    let (a, b) = (room(body.positions[edge[0] as usize]), room(body.positions[edge[1] as usize]));
                    shapes.segment(a, b, radius((a + b) * 0.5) * 0.08, shape_style(col, HandleState::Idle, time));
                }
            }
        }
    }
    for p in fab::tools::handles::shown_params(state, &pp) {
        let Some(h) = &p.handle else { continue };
        let base = fab::tools::handles::handle_shape(h).color();
        let visual = if handles.held_key() == Some(p.key.as_str()) { HandleState::Active } else if out.near.as_deref() == Some(p.key.as_str()) || state.params.hover.as_deref() == Some(p.key.as_str()) { HandleState::Hover } else { HandleState::Idle };
        let style = shape_style(base, visual, time);
        // Keep the drawn grip at the point the hit test selected, especially for a radius
        // whose eye-facing grip changes with the approach direction.
        let grip_at = |g: HandleGrip| {
            if handles.held_key() == Some(p.key.as_str()) || out.near.as_deref() == Some(p.key.as_str()) {
                if let Some((_, at)) = out.grip.filter(|(picked, _)| *picked == g) { return at; }
            }
            grip_point(h, g, unit(sub(eye, grip_point(h, g, eye))))
        };
        let gp = room(grip_at(HandleGrip::default()));
        let r = radius(gp) * visual.scale();
        let stroke = r * 0.18;
        match h {
            ParamHandle::Linear { from, to, .. } => {
                let (a, b) = (room(*from), room(*to));
                let d = (b - a).normalize();
                let mut guide = style;
                guide.color.w *= 0.45;
                shapes.segment(a, b, stroke * 0.5, guide);
                shapes.segment(b, b + d * r * 2.2, stroke, style);
                shapes.cone(b + d * r * 1.8, b + d * r * 3.2, r * 0.65, style);
                shapes.sphere(gp, r, style);
            }
            ParamHandle::Radius { center, axis, radius: rad, .. } => {
                let c = room(*center);
                let rr = (room(add(*center, mul(perp(*axis), *rad))) - c).length();
                let mut ring = style; ring.color.w *= 0.85;
                shapes.ring(c, direction(*center, *axis), rr, stroke, ring);
                shapes.sphere(gp, r, style);
            }
            ParamHandle::Angle { center, axis, from, radius: rad, angle, .. } => {
                let c = room(*center);
                // grip_point defines the fallback radius too: the arc ends at the pick ball.
                let start = room(add(*center, mul(unit(*from), rad.max(1.0))));
                shapes.arc(c, direction(*center, *axis), start - c, (start - c).length(), angle.to_radians() as f32, stroke, style);
                let mut guide = style; guide.color.w *= 0.5;
                shapes.segment(c, start, stroke * 0.5, guide);
                shapes.segment(c, gp, stroke * 0.5, guide);
                shapes.sphere(gp, r, style);
            }
            ParamHandle::Point { .. } => shapes.sphere(gp, r, style),
            ParamHandle::Profile { origin, u, v, points } => {
                let at = |q| room(profile_point(*origin, *u, *v, q));
                if !points.is_empty() {
                    let mut prev = at(*points.last().unwrap());
                    let mut line = shape_style(base, HandleState::Idle, time); line.color.w *= 0.7;
                    for q in points { let next = at(*q); shapes.segment(prev, next, stroke * 0.6, line); prev = next; }
                }
                for (point, q) in points.iter().enumerate() {
                    let a = at(*q);
                    let s = if out.grip.is_some_and(|(g, _)| g.point == point) { visual } else { HandleState::Idle };
                    shapes.sphere(a, radius(a) * s.scale(), shape_style(base, s, time));
                }
            }
            ParamHandle::Count { at, axis } => {
                for (point, p_at) in count_grips(*at, *axis, eye, view.scale).into_iter().enumerate() {
                    let a = room(p_at);
                    let s = if out.grip.is_some_and(|(g, _)| g.point == point) { visual } else { HandleState::Idle };
                    let value = p.value.num().unwrap_or(0.0);
                    let disabled = clamp_value(p, value + if point == 0 { 1.0 } else { -1.0 }) == value;
                    let s = if disabled { HandleState::Disabled } else { s };
                    let r = radius(a) * s.scale();
                    let n = (head - a).normalize();
                    let x = Vec3f::cross(if n.y.abs() < 0.9 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) }, n).normalize();
                    let y = Vec3f::cross(n, x);
                    let badge = a + y * r * 2.2;
                    shapes.sphere(a, r, shape_style(base, s, time));
                    shapes.disc(badge, n, r, r * 0.2, shape_style(COUNT_BG, s, time));
                    let ink = if s == HandleState::Hover || s == HandleState::Active { COUNT_INK_HOT } else { base };
                    let ink = shape_style(ink, if disabled { HandleState::Disabled } else { HandleState::Idle }, time);
                    let front = badge + n * r * 0.3;
                    shapes.segment(front - x * r * 0.55, front + x * r * 0.55, r * 0.10, ink);
                    if point == 0 { shapes.segment(front - y * r * 0.55, front + y * r * 0.55, r * 0.10, ink); }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xr_edit::room_from_printer;
    use makepad_xr_nav::{ClipBox, XrNavView};

    fn view() -> XrNavView {
        XrNavView { clip: ClipBox { center: vec3f(0.0, 0.9, -0.6), yaw: 0.0, half: vec3f(0.5, 0.3, 0.5) }, origin: vec3f(-0.175, 0.9, -0.44), yaw: 0.0, scale: 1.0 }
    }

    /// A controller with its tip at `at`.
    fn held(at: Vec3f, grip: f32) -> XrController {
        let mut c = XrController { buttons: XrController::ACTIVE, ..Default::default() };
        c.grip_pose.orientation = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        c.aim_pose = c.grip_pose;
        makepad_xr_ui::input::place_tip(&mut c, at);
        c.grip = grip;
        c
    }

    fn state(r: XrController) -> XrState {
        let mut s = XrState { right_controller: r, ..Default::default() };
        s.head_pose.orientation = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        s
    }

    #[test]
    fn either_grab_control_on_a_height_handle_drags_snaps_ticks_and_commits_once() {
        for use_trigger in [false, true] {
            let held = |at, down| {
                let mut c = held(at, down);
                if use_trigger {
                    c.trigger = c.grip;
                    c.grip = 0.0;
                }
                c
            };
            // A 70 mm vase height handle: the arrow tip at printer (100, 100, 70), moving 1 mm per mm.
            let p = ParamView {
                key: "vase_height".into(),
                label: "Height".into(),
                kind: ParamKind::Length,
                unit: "mm".into(),
                value: ParamValue::Num(70.0),
                min: 30.0,
                max: 200.0,
                step: 0.5,
                declared: true,
                handle: Some(ParamHandle::Linear { from: [100.0, 100.0, 0.0], to: [100.0, 100.0, 70.0], rate: 1.0 }),
                objects: vec!["vase".into()],
                ..Default::default()
            };
            let pp = PartParamsView { readout: String::new(), object: "vase".into(), params: vec![p.clone()], boxes: Vec::new(), wires: Vec::new() };
            let v = view();
            let tip = room_from_printer(&v, [100.0, 100.0, 70.0]);
            let head = vec3f(0.0, 1.6, 0.5);
            let mut hs = XrHandles::default();
            let mut out = HandlesOut::default();
            let shown = [&pp.params[0]];
            // reach without gripping: it lights
            hs.step(&state(held(tip + vec3f(0.01, 0.0, 0.0), 0.0)), &v, Some(&pp), &shown, head, &xr_edit::IDENTITY, &Buttons::default(), &mut out);
            assert_eq!(out.near.as_deref(), Some("vase_height"));
            // grip 1 cm beside the tip: taken, no jump
            hs.step(&state(held(tip + vec3f(0.01, 0.0, 0.0), 1.0)), &v, Some(&pp), &shown, head, &xr_edit::IDENTITY, &Buttons::default(), &mut out);
            assert!(hs.held_key().is_some() && out.consumed[1] && out.preview.is_none());
            // up 12.3 mm (room Y is printer Z): snapped to 82, with a tick
            hs.step(&state(held(tip + vec3f(0.01, 0.0123, 0.0), 1.0)), &v, Some(&pp), &shown, head, &xr_edit::IDENTITY, &Buttons::default(), &mut out);
            assert_eq!(out.preview, Some(("vase".into(), "vase_height".into(), ParamValue::Num(82.0))));
            assert!(out.haptics.0[1].is_some() && out.label.as_ref().is_some_and(|l| l.1.contains("82")));
            // release: one commit
            hs.step(&state(held(tip, 0.0)), &v, Some(&pp), &shown, head, &xr_edit::IDENTITY, &Buttons::default(), &mut out);
            assert_eq!(out.commit, Some(("vase".into(), "vase_height".into(), ParamValue::Num(82.0))));
            assert!(!hs.held_key().is_some());
        }
    }
}
