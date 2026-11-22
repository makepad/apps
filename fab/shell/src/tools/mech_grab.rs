//! Mechanics' pointer tools: hold, throw, pin, dangle or poke a part. A drag
//! follows the camera-facing plane through the picked point; the wheel moves
//! that plane nearer or farther. Only Throw adds release velocity. The sim is
//! the app's: this widget only picks and sends `MechAction`s. A press off
//! every part goes on to the view (orbit, pan).

use crate::api::*;
use crate::api::PaneId;
use crate::handle_style::{GRAB_HALO, GRAB_BALL, PIN_HALO, PIN_STEM, PIN_BALL};
use crate::model::ElementId;
use crate::model::state::pose_apply;
use crate::viewport::pick_ray;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabMechGrabBase = #(FabMechGrab::register_widget(vm))
    mod.widgets.FabMechGrab = set_type_default() do mod.widgets.FabMechGrabBase{
        width: Fill
        height: Fill
        draw_prim: mod.draw.DrawGizmoPrim{}
    }
}

#[derive(Clone, Copy, Debug)]
struct Hold {
    element: ElementId,
    tool: MechTool,
    /// The drag plane: a point on it and its normal (printer frame, metres).
    origin: Vec3f,
    normal: Vec3f,
    /// The target now (printer frame, metres).
    target: Vec3f,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabMechGrab {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_prim: crate::tools::transform::DrawGizmoPrim,
    #[rust]
    area: Area,
    #[rust(PaneId::PLATE_ZERO)]
    pub pane: PaneId,
    #[rust]
    hold: Option<Hold>,
    #[rust]
    velocity: crate::VelocityTrack,
    /// Each grabbable part's and support piece's name and where it shows on screen (for semantics).
    #[rust]
    targets: Vec<(String, DVec2, bool)>,
}

fn active(state: &AppState) -> bool {
    state.ui.in_mode(PlateMode::Mechanics) && state.mech.ready
}

impl FabMechGrab {
    pub(crate) fn is_holding(&self) -> bool {
        self.hold.is_some()
    }

    fn ray(&self, cx: &Cx, state: &AppState, at: DVec2) -> crate::model::query::Ray {
        ViewProjector::new(*state.view.camera(self.pane), self.area.rect(cx)).ray(at)
    }

    /// The plate part under a screen point and where on it (printer frame, metres).
    fn part_at(&self, cx: &Cx, state: &AppState, at: DVec2) -> Option<(ElementId, Vec3f, Vec3f)> {
        if !self.area.clipped_rect(cx).contains(at) {
            return None;
        }
        let ray = self.ray(cx, state, at);
        let hit = pick_ray(state, self.pane, &ray)?;
        // anything but the printer: a part, or one link of a chain in the Mechanics scene
        if state.plate.is_context(hit.element) {
            return None;
        }
        Some((hit.element, hit.point, ray.dir))
    }

    fn on_plane(&self, cx: &Cx, state: &AppState, at: DVec2, h: &Hold) -> Option<Vec3f> {
        let ray = self.ray(cx, state, at);
        let d = ray.dir.dot(h.normal);
        if d.abs() < 1e-6 {
            return None;
        }
        let t = (h.origin - ray.origin).dot(h.normal) / d;
        (t > 0.0).then(|| ray.origin + ray.dir * t)
    }

    fn cursor(tool: MechTool) -> MouseCursor {
        match tool {
            MechTool::Grab | MechTool::Throw | MechTool::Dangle => MouseCursor::Hand,
            MechTool::Pin => MouseCursor::Crosshair,
            MechTool::Poke => MouseCursor::Arrow,
        }
    }

    fn release(&mut self, cx: &mut Cx, velocity: Option<[f64; 3]>) {
        if self.hold.take().is_some() {
            self.velocity = Default::default();
            cx.action(MechAction::Release { velocity });
            cx.set_cursor(MouseCursor::Default);
            // Grab feedback and mechanics readouts share the simulated state; selection actions refresh other panels.
            crate::ui::redraw(cx, &[ids!(viewport), ids!(mech_grab), ids!(tab_mechanics), ids!(xr)]);
        }
    }

    fn element_center(state: &AppState, element: ElementId) -> Option<Vec3f> {
        if !state.is_drawn(element) {
            return None;
        }
        let bounds = &state.scene.element(element)?.bounds;
        if aabb_is_empty(bounds) {
            return None;
        }
        let center = aabb_center(bounds);
        Some(state.scene_state.pose(element).map_or(center, |pose| pose_apply(&pose, center)))
    }

    fn disc(&mut self, cx: &mut Cx2d, at: DVec2, radius: f64, color: [f32; 4]) {
        self.draw_prim.disc(cx, Rect { pos: at - dvec2(radius, radius), size: dvec2(radius * 2.0, radius * 2.0) }, color);
    }

    fn held_marker(&mut self, cx: &mut Cx2d, at: DVec2) {
        self.disc(cx, at, 10.0, GRAB_HALO);
        self.disc(cx, at, 5.0, GRAB_BALL);
    }

    fn pin_marker(&mut self, cx: &mut Cx2d, at: DVec2) {
        self.disc(cx, at, 9.0, PIN_HALO);
        // A pin head and a short stem, using the same outlined discs as the grab.
        for y in [0.0, -2.0, -4.0, -6.0] {
            self.disc(cx, at + dvec2(0.0, y), 2.2, PIN_STEM);
        }
        self.disc(cx, at - dvec2(0.0, 10.0), 5.5, PIN_BALL);
    }
}

fn point(v: Vec3f) -> [f64; 3] {
    [v.x as f64, v.y as f64, v.z as f64]
}

impl Widget for FabMechGrab {
    /// Each part as a grab target, so a test or an automation client can take hold of one.
    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (i, (name, at, selected)) in self.targets.iter().enumerate() {
            let r = 8.0;
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                bounds: makepad_platform::a11y::semantic_rect(Rect { pos: *at - dvec2(r, r), size: dvec2(r * 2.0, r * 2.0) }),
                ..Default::default()
            };
            makepad_platform::a11y::set_string(&mut node.name, &format!("Grab {name}"));
            node.states.set(a11y_tree::States::SELECTED, *selected);
            out.push((i as u64 + 1, None, node));
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get::<AppState>() else { return };
        let resetting = matches!(event, Event::Actions(actions) if actions.iter().any(|a| matches!(a.downcast_ref::<MechAction>(), Some(MechAction::Reset | MechAction::Scene(_) | MechAction::Piece(_)))));
        if !active(state) || resetting || matches!(event, Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            if self.hold.is_some() {
                log!("fab mech: let go of a hold: active {} resetting {resetting} focus lost {}", active(state), matches!(event, Event::WindowLostFocus(_)));
            }
            self.release(cx, None);
            return;
        }
        if self.hold.is_some_and(|h| h.tool != state.mech.tool) {
            self.release(cx, None);
        }
        // Filter BEFORE hits: hits captures a mouse button even if a later
        // FingerDown guard rejects it. Navigation keeps all non-primary presses.
        let picked = if let Event::MouseDown(e) = event {
            if !e.button.is_primary() || e.modifiers.shift || e.handled.get() != Area::Empty || self.hold.is_some() {
                return;
            }
            // a press outside its own viewport (another Plate's, a rail) is not its to take
            if !self.area.clipped_rect(cx).contains(e.abs) {
                return;
            }
            let Some(hit) = self.part_at(cx, state, e.abs) else {
                log!("fab mech: a press at {:?} found no part (viewport {:?})", e.abs, self.area.clipped_rect(cx));
                return;
            };
            Some(hit)
        } else {
            None
        };
        if let Event::MouseMove(e) = event {
            if self.hold.is_none() && (cx.fingers.is_mouse_held_outside(&[self.area]) || self.part_at(cx, state, e.abs).is_none()) {
                return;
            }
        }
        if let Event::Scroll(se) = event {
            if let Some(h) = self.hold {
                // Scale depth by wheel travel, so a trackpad and wheel stay
                // continuous at any camera distance and cannot cross the eye.
                let depth = (h.origin - state.view.camera(self.pane).eye).dot(h.normal * -1.0);
                let next_depth = depth * (se.scroll.y as f32 * 0.002).exp();
                let mut next = Hold { origin: h.origin + h.normal * (depth - next_depth), ..h };
                if next_depth.is_finite() && next_depth > 0.0 {
                    if let Some(target) = self.on_plane(cx, state, se.abs, &next) {
                        next.target = target;
                        self.hold = Some(next);
                        self.velocity.push(se.time, target);
                        cx.action(MechAction::Drag { point: point(target) });
                        // Grab feedback and mechanics readouts share the simulated state; selection actions refresh other panels.
                        crate::ui::redraw(cx, &[ids!(viewport), ids!(mech_grab), ids!(tab_mechanics), ids!(xr)]);
                    }
                }
                se.handled_x.set(true);
                se.handled_y.set(true);
            }
            return;
        }
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                let Some((element, at, dir)) = picked.or_else(|| self.part_at(cx, state, fe.abs)) else {
                    // A touch has no MouseDown pre-check. Give a miss back to navigation.
                    event.unhandle(cx, &self.area);
                    return;
                };
                log!("fab mech: a press took element {element:?} at {at:?}");
                cx.action(if fe.modifiers.shift { ShellAction::SelectToggle(element) } else { ShellAction::SelectOnly(element) });
                cx.set_key_focus(self.area);
                match state.mech.tool {
                    MechTool::Pin => cx.action(MechAction::Pin { element, point: point(at) }),
                    MechTool::Poke => cx.action(MechAction::Poke { element, point: point(at), dv: point(dir * 1.5) }),
                    tool @ (MechTool::Grab | MechTool::Throw | MechTool::Dangle) => {
                        // A camera-facing plane, including for an off-centre pick.
                        let normal = (state.view.camera(self.pane).eye - state.view.camera(self.pane).target).normalize();
                        self.hold = Some(Hold { element, tool, origin: at, normal, target: at });
                        self.velocity = Default::default();
                        self.velocity.push(fe.time, at);
                        cx.action(if tool == MechTool::Dangle {
                            MechAction::Dangle { element, point: point(at) }
                        } else {
                            MechAction::Grab { element, point: point(at), hand: false }
                        });
                    }
                }
                cx.set_cursor(if self.hold.is_some() { MouseCursor::Grabbing } else { Self::cursor(state.mech.tool) });
                // Grab feedback and mechanics readouts share the simulated state; selection actions refresh other panels.
                crate::ui::redraw(cx, &[ids!(viewport), ids!(mech_grab), ids!(tab_mechanics), ids!(xr)]);
            }
            Hit::FingerMove(fe) => {
                let Some(h) = self.hold else { return };
                cx.set_cursor(MouseCursor::Grabbing);
                if let Some(p) = self.on_plane(cx, state, fe.abs, &h) {
                    self.hold = Some(Hold { target: p, ..h });
                    self.velocity.push(fe.time, p);
                    cx.action(MechAction::Drag { point: point(p) });
                    // Grab feedback and mechanics readouts share the simulated state; selection actions refresh other panels.
                    crate::ui::redraw(cx, &[ids!(viewport), ids!(mech_grab), ids!(tab_mechanics), ids!(xr)]);
                }
            }
            Hit::FingerUp(fe) if fe.device.is_primary_hit() => {
                if let Some(h) = self.hold {
                    let target = self.on_plane(cx, state, fe.abs, &h).unwrap_or(h.target);
                    // Include the release timestamp so holding still before letting
                    // go ages out an earlier flick in the shared 40 ms tracker.
                    self.velocity.push(fe.time, target);
                    if !fe.cancelled && target != h.target {
                        cx.action(MechAction::Drag { point: point(target) });
                    }
                    let velocity = (h.tool == MechTool::Throw && !fe.cancelled).then(|| point(self.velocity.velocity()));
                    self.release(cx, velocity);
                }
            }
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => cx.set_cursor(Self::cursor(state.mech.tool)),
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        self.targets.clear();
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        if !active(state) {
            return DrawStep::done();
        }
        let proj = ViewProjector::new(*state.view.camera(self.pane), rect);
        for (i, p) in state.plate.parts.iter().enumerate() {
            if state.part_unposed(i) {
                continue;
            }
            let c = p.center();
            let mut w = vec3(c[0] as f32 * 0.001, c[1] as f32 * 0.001, c[2] as f32 * 0.001);
            if let (Some(e), Some(m)) = (p.element, state.scene_state.moves.as_ref()) {
                if let Some(a) = m.get(&e) {
                    w = vec3(a[0][0] * w.x + a[0][1] * w.y + a[0][2] * w.z + a[0][3], a[1][0] * w.x + a[1][1] * w.y + a[1][2] * w.z + a[1][3], a[2][0] * w.x + a[2][1] * w.y + a[2][2] * w.z + a[2][3]);
                }
            }
            if let Some(s) = proj.project(w) {
                let selected = p.element.is_some_and(|e| state.scene_state.selection.set.contains(&e));
                self.targets.push((p.name.clone(), s, selected));
            }
        }
        for piece in &state.mech.pieces {
            let Some(e) = state.scene.elements.iter().find(|e| e.name == piece.name).map(|e| e.id) else { continue };
            let [x, y, z] = piece.grip;
            let at = state.scene_state.pose(e).map_or(vec3(x, y, z), |pose| pose_apply(&pose, vec3(x, y, z)));
            if let Some(s) = proj.project(at) {
                self.targets.push((piece.label.clone(), s, state.scene_state.selection.set.contains(&e)));
            }
        }
        for element in &state.mech.pinned {
            if let Some(at) = Self::element_center(state, *element).and_then(|p| proj.project(p)) {
                self.pin_marker(cx, at);
            }
        }
        // The state also covers a hold from another input device. Show the
        // body's centre as well as the desktop target when it is held here.
        if let Some(element) = state.mech.grabbed_element {
            if let Some(at) = Self::element_center(state, element).and_then(|p| proj.project(p)) {
                self.held_marker(cx, at);
            }
        }
        if let Some(h) = self.hold {
            if state.mech.grabbed_element == Some(h.element) || state.mech.grabbed_element.is_none() {
                if let Some(at) = proj.project(h.target) {
                    self.held_marker(cx, at);
                }
            }
        }
        DrawStep::done()
    }
}
