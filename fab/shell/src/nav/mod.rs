//! `Navigator`: the camera controller behind the plate view (owned by
//! `FabViewport`), implementing `api::NavController`:
//!
//! * **Orbit** — MMB drags, Shift+MMB pans, primary+MMB dollies, and the wheel
//!   zooms *to the cursor*. the primary modifier with LMB dollies;
//!   Alt+LMB pans in Select. Shift+LMB
//!   drags orbit in every tool, starting on a part or off it (a Shift click
//!   without travel still reaches the tool). Turntable by
//!   default (up-locked, so twenty mixed drags cannot tilt the horizon),
//!   trackball on request. An orbit picks its pivot as it starts: on an object
//!   the view shows whole, the object's centre, so it turns in place; on an
//!   object zoomed into (`FRAMED_VIEW_SHARE`) or on the plate, the point under
//!   the pointer; off both, the selection's centre, else the plate's. That
//!   point keeps its pixel while the orbit (and its coast) lasts. Zoom never
//!   moves it; pan takes the view along.
//!   Releasing a flick keeps the view coasting for a moment.
//! * **Presets** — numpad 1/3/7 (+Ctrl for the opposite), 9 isometric, 5
//!   orthographic, 2/4/6/8 step 15°, Home frames the model, `.` frames the
//!   selection. Laptops have no numpad, so the top-row digits do the same.
//!   Every move is an eased 100–250 ms glide that lands exactly on its mark.
//!
//! Never: draws into the 3D pass, touches `SceneState`, or knows about any
//! widget other than its own gizmo.

pub mod gizmo;
pub mod orbit;

use crate::api::*;
use crate::model::aabb_union_point;
use crate::model::state::pose_apply;
use crate::viewport::sdf::instance_of;
use makepad_widgets_core::*;
use orbit::CameraAnim;

/// Camera glides: a numpad orbit step (the same as `fab.anim_fast`, the
/// UI's motion token), and a frame / preset move.
const ANIM_FAST: f32 = 0.10;
const ANIM_SLOW: f32 = 0.25;

/// A flick keeps coasting for about a fifth of a second, then stops. Long
/// enough to feel like momentum, short enough never to feel out of control.
const INERTIA_TAU: f32 = 0.11;
/// Below this the coast has visually stopped (layout points per second).
const INERTIA_MIN: f64 = 40.0;
const INERTIA_MAX: f64 = 6000.0;
/// A drag has to be a flick, not a nudge, to coast at all.
const INERTIA_START: f64 = 120.0;
/// Pointer travel (points) before a held primary/secondary button turns
/// from a click into a drag — under it the press stays a click for the
/// select tool or the context menu.
pub(crate) const DRAG_THRESHOLD_PT: f64 = 4.0;

/// An object fits the view while its box on screen is no wider and no taller than this share of
/// the view: an orbit that starts on it turns about its centre. Past it the view is zoomed into
/// the object, and the orbit turns about the point pressed, as on the plate. Frame Selected leaves
/// an object's box at up to 1/1.05 of the view (its corners lie on the sphere it fits), so the
/// share is the whole view: whatever framing shows whole turns in place.
pub(crate) const FRAMED_VIEW_SHARE: f64 = 1.0;

/// What a press that may turn into an orbit is on.
#[derive(Clone, Copy, Debug)]
struct Press {
    /// The surface point: a part's, else the plate's face.
    point: Vec3f,
    /// The part (scene element) the point is on; `None` on the plate.
    part: Option<ElementId>,
}

/// Where the object `element` belongs to shows: the box corners of each of its solids (every solid
/// its object script makes, `instance/solid`; just `element` when it is no plate part, such as a
/// chain link in Mechanics), each moved with its pose in `pane`.
fn object_corners(state: &AppState, pane: PaneId, element: ElementId) -> Vec<Vec3f> {
    let plate = &state.plate;
    let elements: Vec<ElementId> = match plate.part_of(element) {
        Some(i) => {
            let instance = instance_of(&plate.parts[i].object);
            plate.parts.iter().filter(|p| instance_of(&p.object) == instance).filter_map(|p| p.element).filter(|e| *e == element || state.is_visible(*e)).collect()
        }
        None => vec![element],
    };
    let mut corners = Vec::with_capacity(elements.len() * 8);
    for e in elements {
        let Some(b) = state.scene.element(e).map(|el| el.bounds).filter(|b| !aabb_is_empty(b)) else {
            continue;
        };
        let pose = state.pose_in(pane, e);
        for i in 0..8 {
            let c = vec3(if i & 1 == 0 { b.min.x } else { b.max.x }, if i & 2 == 0 { b.min.y } else { b.max.y }, if i & 4 == 0 { b.min.z } else { b.max.z });
            corners.push(pose.map_or(c, |p| pose_apply(&p, c)));
        }
    }
    corners
}

/// The centre of the object `element` belongs to while the view in `rect` shows it whole
/// ([`FRAMED_VIEW_SHARE`]); `None` when it is larger (the view is zoomed into it) or some of it
/// is behind the eye.
fn framed_centre(state: &AppState, pane: PaneId, rect: Rect, element: ElementId) -> Option<Vec3f> {
    let corners = object_corners(state, pane, element);
    if corners.is_empty() {
        return None;
    }
    let proj = ViewProjector::new(*state.view.camera(pane), rect);
    let (mut lo, mut hi) = (dvec2(f64::INFINITY, f64::INFINITY), dvec2(f64::NEG_INFINITY, f64::NEG_INFINITY));
    let mut world = aabb_empty();
    for c in &corners {
        let at = proj.project(*c)?;
        (lo.x, lo.y, hi.x, hi.y) = (lo.x.min(at.x), lo.y.min(at.y), hi.x.max(at.x), hi.y.max(at.y));
        world = aabb_union_point(&world, *c);
    }
    let size = hi - lo;
    (size.x <= rect.size.x * FRAMED_VIEW_SHARE && size.y <= rect.size.y * FRAMED_VIEW_SHARE).then(|| aabb_center(&world))
}

/// One numpad-orbit step, in the same screen units the drag path uses.
fn orbit_step_points() -> f32 {
    (15.0f32).to_radians() / orbit::ORBIT_SENS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drag {
    Orbit,
    Pan,
    Dolly,
}

pub struct Navigator {
    pub pane: PaneId,
    last_aspect: f32,
    last_rect: Rect,
    opening: Option<Aabb>,
    /// The camera was put back from the plate's view file, or kept from the plate before: the
    /// next frame frames the plate if, at this view's own aspect, it sees nothing of it.
    check_camera: bool,
    drag: Option<Drag>,
    /// A primary/secondary button held but not yet dragged past
    /// `DRAG_THRESHOLD_PT`: the emulated three-button mouse (Fab's
    /// option, on by default here — Mac trackpads have no middle button).
    /// Bare LMB drag orbits, Alt+LMB and RMB drag pan, primary+LMB dollies.
    pending: Option<(Drag, DVec2, Option<Press>)>,
    /// Pointer travel waiting for the next frame. Camera transforms consume
    /// this once per frame; input callbacks only add physical pointer delta.
    move_accum: DVec2,
    /// Total travel from `drag_origin`. Re-evaluating from the press camera
    /// makes even trackball motion independent of how frames divide the path.
    drag_total: DVec2,
    drag_origin: Option<Camera>,
    /// The release velocity (points per second): pointer travel per frame, filtered
    /// ([`Self::sample_drag_velocity`]).
    drag_vel: DVec2,
    /// The frame time the velocity filter has come to; None before a drag's first frame.
    vel_time: Option<f64>,
    /// The last frame found the drag's pointer still: the drag asks for no more frames, and the
    /// filter takes the rest's frames that did not come when the pointer moves or lets go.
    resting: bool,
    /// The refresh interval while frames come one after another (seconds): what one frame's
    /// pointer travel spans after a rest.
    cadence: f64,
    /// A coast released from a rest starts at the frame after the release, which slows it by the
    /// rest first: the frame time the rest counts from ([`Self::coast_after_rest`]).
    coast_rest: Option<f64>,
    inertia: Option<(Drag, DVec2)>,
    /// Frames the navigation asked for after one it took (the description's `nav_frames`).
    frames_asked: u64,
    /// The point the current orbit (drag or coast) turns about.
    pivot: Option<Vec3f>,
    anim: Option<CameraAnim>,
    /// Fab's auto-perspective: orbiting away from an axis view by hand
    /// leaves orthographic behind.
    auto_persp: bool,
    /// Laptops have no numpad; the top-row digits drive the preset views too.
    emulate_numpad: bool,
    /// The person's last switch of projection: the camera before it and the one it gave. Switched
    /// straight back (the view untouched since), the camera before comes back to the bit: worked
    /// back from the orthographic height in f32 the eye can land a step off.
    switched: Option<(Camera, Camera)>,
}

impl Default for Navigator {
    fn default() -> Self {
        Navigator {
            pane: PaneId::PLATE_ZERO,
            last_aspect: 0.0,
            last_rect: Rect::default(),
            opening: None,
            check_camera: false,
            drag: None,
            pending: None,
            move_accum: DVec2::default(),
            drag_total: DVec2::default(),
            drag_origin: None,
            drag_vel: DVec2::default(),
            vel_time: None,
            resting: false,
            cadence: 1.0 / 60.0,
            coast_rest: None,
            inertia: None,
            frames_asked: 0,
            pivot: None,
            anim: None,
            auto_persp: false,
            emulate_numpad: true,
            switched: None,
        }
    }
}

impl Navigator {
    /// An opening is a layout operation, not an animation from the previous document.
    pub(crate) fn open(&mut self, bounds: Aabb) {
        self.stop_motion();
        self.opening = Some(bounds);
    }

    /// The next frame checks the camera once, at the view's own aspect ([`Self::prepare_frame`]).
    pub(crate) fn check_camera(&mut self) {
        self.check_camera = true;
    }

    /// Called with the viewport's actual rectangle before recording its first picture.
    pub(crate) fn prepare_frame(&mut self, state: &mut AppState, rect: Rect) {
        self.last_rect = rect;
        self.last_aspect = (rect.size.x / rect.size.y) as f32;
        if std::mem::take(&mut self.check_camera) && self.opening.is_none() {
            self.opening = state.restored_frame(self.pane, self.last_aspect);
        }
        if let Some(bounds) = self.opening.take() {
            // The window keeps its projection from one document to the next.
            let mut camera = Camera { ortho: state.view.camera(self.pane).ortho, ..Camera::default() };
            camera.frame_bounds(&bounds, self.last_aspect);
            self.glide(state, camera, 0.0, None);
        }
        // A resin printer's stack hangs from its plate as it prints: while the Slice render
        // mode shows it, the camera turns over (the plate on top), and back after.
        let hang = state.print.resin_hangs();
        let to = self.heading(state);
        if to.hung != hang {
            let mut to = to;
            (to.hung, to.up) = (hang, -to.up);
            let preset = state.view.pane(self.pane).preset;
            self.glide(state, to, ANIM_SLOW, preset);
        }
    }

    /// Where the camera is headed: a glide's end while one is on its way, else where it stands. A
    /// move that builds on the camera (a framing, a preset view, a turn by a step) builds on this,
    /// so one asked during a glide ends where it would after the glide: Frame Selected pressed
    /// during the View: Front glide framed along the glide's direction of the moment, degrees off
    /// the front.
    fn heading(&self, state: &AppState) -> Camera {
        self.anim.as_ref().map_or(*state.view.camera(self.pane), |a| a.to)
    }

    /// Whatever the camera was doing on its own stops.
    /// A drag is in flight and nothing glides: the camera moves only on the pointer's own moves,
    /// each of which redraws the view, so a resting pointer needs no frames.
    pub fn dragging(&self) -> bool {
        self.drag.is_some() && self.anim.is_none() && self.inertia.is_none()
    }

    /// The navigation asks for the next frame: a glide or a coast runs, or a drag has travel to
    /// apply or has not yet found its pointer resting. A drag whose pointer rests asks for none
    /// (it is still in flight: [`NavController::is_animating`]); its next move or release asks.
    pub fn needs_frames(&self) -> bool {
        self.anim.is_some()
            || self.inertia.is_some()
            || (self.drag.is_some() && (!self.resting || self.move_accum != DVec2::default()))
    }

    /// Frames the navigation asked for after one it took (the view's description's `nav_frames`).
    pub fn frames_asked(&self) -> u64 {
        self.frames_asked
    }

    pub fn stop_motion(&mut self) {
        self.opening = None;
        self.anim = None;
        self.inertia = None;
    }

    fn aspect(&self) -> f32 {
        if self.last_aspect > 0.0 {
            self.last_aspect
        } else {
            16.0 / 9.0
        }
    }

    fn rect_height(&self) -> f32 {
        let h = self.last_rect.size.y as f32;
        if h > 1.0 {
            h
        } else {
            1000.0
        }
    }

    /// Publish navigation to this pane and, when it moved, its link source.
    fn commit(&self, state: &mut AppState, camera: Camera, preset: Option<PresetView>) {
        let pane = state.view.pane_mut(self.pane);
        let write_back = pane.link.source().is_some() && pane.camera != camera;
        pane.camera = camera;
        pane.preset = preset;
        if write_back {
            state.view.write_back(self.pane);
        }
    }

    /// Start (or, with `dur <= 0`, immediately finish) a camera move.
    fn glide(
        &mut self,
        state: &mut AppState,
        to: Camera,
        dur: f32,
        preset: Option<PresetView>,
    ) {
        self.inertia = None;
        self.pivot = None;
        let from = *state.view.camera(self.pane);
        if dur <= 1e-4 {
            self.anim = None;
            self.commit(state, to, preset);
            return;
        }
        // Show the first eased frame as the key is pressed.
        let mut anim = CameraAnim::new(from, to, dur, preset);
        let (camera, done) = anim.step(0.0);
        self.anim = (!done).then_some(anim);
        self.commit(state, camera, preset);
    }

    /// Turn to the selection's centre (else the plate's) and orbit about it from where the
    /// camera stands: the way back to orbiting a thing after the pivot ended up next to the eye.
    pub fn orbit_around_selection(&mut self, state: &mut AppState) {
        let b = state.selection_bounds().unwrap_or(state.scene.bounds);
        if aabb_is_empty(&b) {
            return;
        }
        let mut to = self.heading(state);
        let centre = aabb_center(&b);
        if (to.eye - centre).length() < orbit::MIN_DISTANCE * 10.0 {
            return;
        }
        to.target = centre;
        let preset = state.view.pane(self.pane).preset;
        self.glide(state, to, ANIM_SLOW, preset);
    }

    fn apply_orbit(&self, cam: &mut Camera, style: OrbitStyle, dx: f32, dy: f32) {
        match style {
            OrbitStyle::Trackball => orbit::orbit_trackball(cam, dx, dy),
            OrbitStyle::Turntable => orbit::orbit_turntable(cam, dx, dy),
        }
    }

    /// Start a drag. An ORBIT picks its pivot ([`Self::orbit_pivot`]); the camera does not move or
    /// turn for it.
    fn begin_drag_at(&mut self, drag: Drag, state: &mut AppState, press: Option<Press>) {
        self.pivot = (drag == Drag::Orbit).then(|| self.orbit_pivot(state, press));
        self.begin_drag(drag, *state.view.camera(self.pane));
    }

    /// What an orbit that starts on `press` turns about. On an object the view shows whole, the
    /// object's centre; on an object the view is zoomed into, or on the plate, the point pressed;
    /// off both, the selection's centre, else the plate's.
    fn orbit_pivot(&self, state: &AppState, press: Option<Press>) -> Vec3f {
        match press.filter(|p| p.point.is_finite()) {
            Some(press) => press.part.and_then(|e| framed_centre(state, self.pane, self.last_rect, e)).unwrap_or(press.point),
            None => {
                let b = state.selection_bounds().unwrap_or(state.scene.bounds);
                if aabb_is_empty(&b) {
                    state.view.camera(self.pane).target
                } else {
                    aabb_center(&b)
                }
            }
        }
    }

    /// Orbit `cam` about the current pivot (the look point when there is none).
    fn orbit_cam(&self, cam: &mut Camera, style: OrbitStyle, dx: f32, dy: f32) {
        match self.pivot {
            Some(p) => orbit::orbit_about(cam, p, style, dx, dy),
            None => self.apply_orbit(cam, style, dx, dy),
        }
    }

    fn begin_drag(&mut self, drag: Drag, camera: Camera) {
        self.drag = Some(drag);
        self.drag_origin = Some(camera);
        self.drag_total = DVec2::default();
        self.move_accum = DVec2::default();
        self.drag_vel = DVec2::default();
        self.vel_time = None;
        self.resting = false;
    }

    /// Apply one cumulative drag sample from the camera at pointer-down.
    /// No frame-time term belongs here: these are layout points, not a rate.
    fn apply_drag_total(&self, state: &mut AppState, drag: Drag) {
        let mut cam = self
            .drag_origin
            .unwrap_or_else(|| *state.view.camera(self.pane));
        let dx = self.drag_total.x as f32;
        let dy = self.drag_total.y as f32;
        let preset = state.view.pane(self.pane).preset;
        let keep = match drag {
            Drag::Orbit => {
                self.break_auto_ortho(&mut cam, preset);
                self.orbit_cam(&mut cam, state.view.orbit_style, dx, dy);
                None
            }
            Drag::Pan => {
                orbit::pan(&mut cam, dx, dy, self.rect_height());
                preset
            }
            Drag::Dolly => {
                orbit::dolly(&mut cam, 1.01f32.powf(dy), None);
                preset
            }
        };
        self.commit(state, cam, keep);
    }

    fn flush_drag_motion(&mut self, state: &mut AppState) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        let delta = std::mem::take(&mut self.move_accum);
        if delta.x == 0.0 && delta.y == 0.0 {
            return false;
        }
        self.drag_total += delta;
        self.apply_drag_total(state, drag);
        true
    }

    /// Orbiting by hand out of an axis view returns to perspective, exactly
    /// as Fab's auto-perspective preference does.
    fn break_auto_ortho(&self, cam: &mut Camera, preset: Option<PresetView>) {
        if self.auto_persp && preset.is_some() && cam.ortho {
            orbit::set_ortho(cam, false);
        }
    }

    // -----------------------------------------------------------------
    // orbit mode
    // -----------------------------------------------------------------

    fn handle_orbit(
        &mut self,
        cx: &mut Cx,
        input: &ViewportInput,
        state: &mut AppState,
    ) -> InputResponse {
        if state.picks.menu.is_some() {
            self.pending = None;
            if matches!(input.kind, ViewportInputKind::PointerDown { .. } | ViewportInputKind::PointerMove { .. } | ViewportInputKind::PointerUp { .. }) {
                return InputResponse { consumed: true, ..Default::default() };
            }
        }
        match input.kind {
            ViewportInputKind::PointerDown {
                button,
                pos,
                mods,
                tap_count,
            } => {
                self.inertia = None;
                self.pivot = None;
                self.pending = None;
                // What the press is on: a part's surface, else the plate's face (drawn or not, so
                // from under the bed too); an orbit picks its pivot from it.
                let press = match input.hit {
                    Some(hit) => Some(Press { point: hit.point, part: Some(hit.element) }),
                    None => state.plate.bed_point(&ViewProjector::new(*state.view.camera(self.pane), input.rect).ray(pos)).map(|point| Press { point, part: None }),
                };
                let shift_orbit = button == PointerButton::Primary && mods.shift && !mods.alt && !mods.control && !mods.logo;
                if crate::tools::annotate::owns_input(state, input) || crate::tools::paint::owns_input(state, input) || crate::tools::sculpt::owns_input(state, input) {
                    // A brush press stops camera motion and leaves the drag to the tool.
                    self.anim = None;
                    return InputResponse::default();
                }
                let middle = button == PointerButton::Middle;
                // Emulated three-button mouse: a bare LMB press may become an
                // orbit, Alt+LMB / RMB a pan, primary+LMB a dolly — only once
                // the pointer travels; until then it is a click for the tool
                // or the context menu, so nothing is consumed here. Only in
                // the select tool: measure/section own their LMB.
                if shift_orbit {
                    // Shift+LMB orbits in every tool; a click without travel stays the tool's.
                    self.pending = Some((Drag::Orbit, DVec2::default(), press));
                } else if !middle && tap_count < 2 && state.tool == Tool::Select {
                    let kind = if button == PointerButton::Secondary || mods.alt {
                        Drag::Pan
                    } else if mods.is_primary() {
                        Drag::Dolly
                    } else {
                        Drag::Orbit
                    };
                    // What the press is on: the orbit's pivot comes from it once the press turns into one.
                    self.pending = Some((kind, DVec2::default(), press));
                }
                if middle {
                    self.anim = None;
                    let drag = if mods.shift {
                        Drag::Pan
                    } else if mods.is_primary() {
                        Drag::Dolly
                    } else {
                        Drag::Orbit
                    };
                    self.begin_drag_at(drag, state, press);
                    return InputResponse {
                        consumed: true,
                        redraw: false,
                        cursor: Some(MouseCursor::Grabbing),
                        wants_frames: true,
                        ..Default::default()
                    };
                }
                // Double-click on a surface moves the pivot there without
                // turning the camera. Deliberately *not* consumed: the same
                // click also selects the part. The Force tool's double-click
                // opens a load's editor instead.
                let force = state.force_tool() && state.heatmap_part().is_some();
                if button == PointerButton::Primary && tap_count >= 2 && !mods.shift && !force {
                    if let Some(hit) = input.hit {
                        let mut to = *state.view.camera(self.pane);
                        orbit::recenter(&mut to, hit.point);
                        let preset = state.view.pane(self.pane).preset;
                        self.glide(state, to, ANIM_SLOW, preset);
                        return InputResponse {
                            consumed: false,
                            redraw: true,
                            wants_frames: true,
                            ..Default::default()
                        };
                    }
                }
                InputResponse::default()
            }
            ViewportInputKind::PointerMove { delta, buttons, .. } => {
                let mut initial_delta = None;
                if self.drag.is_none() {
                    if let Some((kind, travel, press)) = self.pending.as_mut() {
                        if buttons == 0 {
                            // The release went elsewhere (a menu took it).
                            self.pending = None;
                            return InputResponse::default();
                        }
                        *travel += delta;
                        if travel.length() < DRAG_THRESHOLD_PT {
                            return InputResponse::default();
                        }
                        let (kind, press) = (*kind, *press);
                        initial_delta = Some(*travel);
                        self.pending = None;
                        self.anim = None;
                        self.begin_drag_at(kind, state, press);
                    }
                }
                if self.drag.is_none() {
                    return InputResponse::default();
                }
                // `initial_delta` already contains the threshold-crossing
                // event; established drags add this event here.
                self.move_accum += initial_delta.unwrap_or(delta);
                InputResponse {
                    consumed: true,
                    redraw: false,
                    wants_frames: true,
                    ..Default::default()
                }
            }
            ViewportInputKind::PointerUp { .. } => {
                self.pending = None;
                self.flush_drag_motion(state);
                let Some(drag) = self.drag.take() else {
                    return InputResponse::default();
                };
                let v = self.drag_vel;
                // A release from a rest: the rest's frames did not come, so the coast slows by
                // them at the next frame ([`Self::coast_after_rest`]), which may stop it.
                self.coast_rest = if self.resting { self.vel_time } else { None };
                self.move_accum = DVec2::default();
                self.drag_total = DVec2::default();
                self.drag_origin = None;
                self.drag_vel = DVec2::default();
                self.vel_time = None;
                self.resting = false;
                let speed = v.length();
                if matches!(drag, Drag::Orbit | Drag::Pan) && speed > INERTIA_START {
                    self.inertia = Some((drag, if self.coast_rest.is_some() { v } else { coast_velocity(v) }));
                }
                if self.inertia.is_none() {
                    self.pivot = None;
                }
                InputResponse {
                    consumed: true,
                    redraw: true,
                    cursor: Some(MouseCursor::Default),
                    wants_frames: self.inertia.is_some(),
                    ..Default::default()
                }
            }
            ViewportInputKind::Scroll { delta, pos, mods } => {
                // The brush takes Shift+scroll (its size) before the camera pans.
                if crate::tools::paint::owns_scroll(state, &mods) || crate::tools::sculpt::owns_scroll(state, &mods) {
                    return InputResponse::default();
                }
                self.inertia = None;
                self.pivot = None;
                self.anim = None;
                let preset = state.view.pane(self.pane).preset;
                let rect_h = input.rect.size.y as f32;
                let mut cam = *state.view.camera(self.pane);
                if mods.shift {
                    orbit::pan(&mut cam, 0.0, -delta.y as f32, rect_h);
                } else if mods.control {
                    orbit::pan(&mut cam, -delta.y as f32, 0.0, rect_h);
                } else {
                    // Positive scroll is away from the model, everywhere in
                    // this repo. Dolly about a point on the cursor ray, which
                    // leaves that point on exactly the same pixel.
                    let steps = (delta.y as f32 / 80.0).clamp(-1.5, 1.5);
                    if steps.abs() < 1e-4 {
                        return InputResponse::default();
                    }
                    let anchor = match input.hit {
                        Some(hit) => hit.point,
                        None => {
                            let proj = ViewProjector::new(cam, input.rect);
                            orbit::anchor_on_pivot_plane(&cam, &proj.ray(pos))
                        }
                    };
                    orbit::dolly(&mut cam, 1.25f32.powf(steps), Some(anchor));
                }
                self.commit(state, cam, preset);
                InputResponse::consumed()
            }
            ViewportInputKind::KeyDown { key, mods, repeat } => {
                self.handle_orbit_key(cx, state, key, mods, repeat)
            }
            _ => InputResponse::default(),
        }
    }

    fn handle_orbit_key(
        &mut self,
        cx: &mut Cx,
        state: &mut AppState,
        key: KeyCode,
        mods: KeyModifiers,
        repeat: bool,
    ) -> InputResponse {
        if repeat {
            // Held preset keys would fight their own animation.
            return InputResponse::default();
        }
        if mods.logo || mods.alt || mods.shift {
            return InputResponse::default();
        }
        if let Some(preset) = self.preset_for_key(key, mods) {
            self.preset(cx, state, preset, true);
            return InputResponse {
                consumed: true,
                redraw: true,
                wants_frames: true,
                ..Default::default()
            };
        }
        if mods.control {
            return InputResponse::default();
        }
        if let Some((dx, dy)) = self.orbit_step_for_key(key) {
            let style = state.view.orbit_style;
            let preset = state.view.pane(self.pane).preset;
            let mut to = self.heading(state);
            self.break_auto_ortho(&mut to, preset);
            self.apply_orbit(&mut to, style, dx, dy);
            self.glide(state, to, ANIM_FAST, None);
            return InputResponse {
                consumed: true,
                redraw: true,
                wants_frames: true,
                ..Default::default()
            };
        }
        match key {
            KeyCode::Numpad5 | KeyCode::Key5 if self.numpad_key(key) => {
                let ortho = !state.view.camera(self.pane).ortho;
                self.set_ortho(cx, state, ortho);
                InputResponse::consumed()
            }
            KeyCode::Home => {
                let b = state.frame_all_bounds();
                self.frame(cx, state, b, true);
                InputResponse {
                    consumed: true,
                    redraw: true,
                    wants_frames: true,
                    ..Default::default()
                }
            }
            // O: orbit around the selection (else the plate) from where the camera is.
            KeyCode::KeyO => {
                self.orbit_around_selection(state);
                InputResponse {
                    consumed: true,
                    redraw: true,
                    wants_frames: true,
                    ..Default::default()
                }
            }
            KeyCode::Period | KeyCode::NumpadDecimal => {
                let b = state
                    .selection_bounds()
                    .unwrap_or_else(|| state.frame_all_bounds());
                self.frame(cx, state, b, true);
                InputResponse {
                    consumed: true,
                    redraw: true,
                    wants_frames: true,
                    ..Default::default()
                }
            }
            _ => InputResponse::default(),
        }
    }

    /// True when this key counts as its numpad twin (top-row emulation).
    fn numpad_key(&self, key: KeyCode) -> bool {
        match key {
            KeyCode::Numpad0
            | KeyCode::Numpad1
            | KeyCode::Numpad2
            | KeyCode::Numpad3
            | KeyCode::Numpad4
            | KeyCode::Numpad5
            | KeyCode::Numpad6
            | KeyCode::Numpad7
            | KeyCode::Numpad8
            | KeyCode::Numpad9 => true,
            KeyCode::Key0
            | KeyCode::Key1
            | KeyCode::Key2
            | KeyCode::Key3
            | KeyCode::Key4
            | KeyCode::Key5
            | KeyCode::Key6
            | KeyCode::Key7
            | KeyCode::Key8
            | KeyCode::Key9 => self.emulate_numpad,
            _ => false,
        }
    }

    fn preset_for_key(&self, key: KeyCode, mods: KeyModifiers) -> Option<PresetView> {
        if !self.numpad_key(key) || mods.logo || mods.alt || mods.shift {
            return None;
        }
        let flip = mods.control;
        match key {
            KeyCode::Numpad1 | KeyCode::Key1 => Some(if flip {
                PresetView::Back
            } else {
                PresetView::Front
            }),
            KeyCode::Numpad3 | KeyCode::Key3 => Some(if flip {
                PresetView::Left
            } else {
                PresetView::Right
            }),
            KeyCode::Numpad7 | KeyCode::Key7 => Some(if flip {
                PresetView::Bottom
            } else {
                PresetView::Top
            }),
            KeyCode::Numpad9 | KeyCode::Key9 if !mods.control => Some(PresetView::Isometric),
            _ => None,
        }
    }

    fn orbit_step_for_key(&self, key: KeyCode) -> Option<(f32, f32)> {
        if !self.numpad_key(key) {
            return None;
        }
        let s = orbit_step_points();
        match key {
            KeyCode::Numpad4 | KeyCode::Key4 => Some((-s, 0.0)),
            KeyCode::Numpad6 | KeyCode::Key6 => Some((s, 0.0)),
            KeyCode::Numpad8 | KeyCode::Key8 => Some((0.0, -s)),
            KeyCode::Numpad2 | KeyCode::Key2 => Some((0.0, s)),
            _ => None,
        }
    }

    // -----------------------------------------------------------------
    // per-frame
    // -----------------------------------------------------------------

    /// One frame's pointer travel (`accum`) into the release velocity. Exponential filtering is
    /// cadence-independent (0.5 blend at 60 Hz, with the equivalent blend at other rates), and a
    /// frame without travel pulls the velocity toward zero. A resting pointer asks for no frames,
    /// so when frames come again (the pointer moves, or something else draws) the filter first
    /// takes the rest's frames that did not come, as it would have one by one, and the travel
    /// spans the last of them (one refresh interval).
    fn sample_drag_velocity(&mut self, accum: DVec2, dt: f32, time: f64) {
        let dt = match self.vel_time.map(|from| (time - from).max(0.0)) {
            Some(span) if self.resting => {
                let step = span.min(self.cadence);
                self.drag_vel *= rest_decay(span - step);
                step
            }
            Some(_) => {
                if dt > 1e-4 {
                    self.cadence = dt as f64;
                }
                dt as f64
            }
            None => dt as f64,
        };
        if dt > 1e-4 {
            let inst = accum / dt;
            self.drag_vel += (inst - self.drag_vel) * (1.0 - rest_decay(dt));
        }
        self.vel_time = Some(time);
        self.resting = accum == DVec2::default();
    }

    /// A coast released from a rest (`coast_rest`), at the first frame after the release: the
    /// rest's frames before the release slow it as the filter would have (the rest ends a refresh
    /// interval before this frame); too slow then, it does not coast. Returns the step this frame
    /// takes: one refresh interval, not the time since the frame before the rest.
    fn coast_after_rest(&mut self, time: f64, dt: f32) -> f32 {
        let Some(from) = self.coast_rest.take() else { return dt };
        let Some((drag, v)) = self.inertia else { return dt };
        let v = v * rest_decay((time - from - self.cadence).max(0.0));
        if v.length() > INERTIA_START {
            self.inertia = Some((drag, coast_velocity(v)));
        } else {
            self.inertia = None;
            self.pivot = None;
        }
        dt.min(self.cadence as f32)
    }

    fn tick(&mut self, state: &mut AppState, dt: f32, time: f64) -> InputResponse {
        let mut dt = dt.clamp(0.0, 0.1);
        let mut redraw = false;
        let mut wants = false;

        // Camera motion consumes physical pointer travel once. Frame time is
        // used only to estimate release velocity, never to scale the drag.
        if self.drag.is_some() {
            let accum = self.move_accum;
            if self.flush_drag_motion(state) {
                redraw = true;
            }
            self.sample_drag_velocity(accum, dt, time);
            // A resting pointer asks for no frames: its next move or its release asks again.
            wants = !self.resting;
        }
        dt = self.coast_after_rest(time, dt);

        if let Some(anim) = &mut self.anim {
            let (cam, done) = anim.step(dt);
            let preset = anim.preset;
            self.commit(state, cam, preset);
            if done {
                self.anim = None;
            } else {
                wants = true;
            }
            redraw = true;
        } else if let Some((drag, vel)) = self.inertia {
            let step = vel * dt as f64;
            let style = state.view.orbit_style;
            let preset = state.view.pane(self.pane).preset;
            let rect_h = self.rect_height();
            let mut cam = *state.view.camera(self.pane);
            let keep = match drag {
                Drag::Orbit => {
                    self.break_auto_ortho(&mut cam, preset);
                    self.orbit_cam(&mut cam, style, step.x as f32, step.y as f32);
                    None
                }
                _ => {
                    orbit::pan(&mut cam, step.x as f32, step.y as f32, rect_h);
                    preset
                }
            };
            self.commit(state, cam, keep);
            let decay = (-dt / INERTIA_TAU).exp() as f64;
            let next = vel * decay;
            if next.length() < INERTIA_MIN {
                self.inertia = None;
                self.pivot = None;
            } else {
                self.inertia = Some((drag, next));
                wants = true;
            }
            redraw = true;
        }

        if wants || self.needs_frames() {
            self.frames_asked += 1;
        }
        InputResponse {
            consumed: false,
            redraw,
            wants_frames: wants,
            ..Default::default()
        }
    }
}

/// What the release-velocity filter keeps of the velocity over `secs` without pointer travel:
/// half per frame at 60 Hz.
fn rest_decay(secs: f64) -> f64 {
    let tau = (1.0f64 / 60.0) / std::f64::consts::LN_2;
    (-secs / tau).exp()
}

/// A release velocity as a coast takes it: at most `INERTIA_MAX`.
fn coast_velocity(v: DVec2) -> DVec2 {
    let speed = v.length();
    if speed > INERTIA_MAX { v * (INERTIA_MAX / speed) } else { v }
}

impl NavController for Navigator {
    fn handle(&mut self, cx: &mut Cx, input: &ViewportInput, state: &mut AppState) -> InputResponse {
        self.last_rect = input.rect;
        if input.rect.size.y > 1.0 {
            self.last_aspect = (input.rect.size.x / input.rect.size.y) as f32;
        }
        match input.kind {
            // Escape stops any glide or drag in flight.
            ViewportInputKind::KeyDown { key: KeyCode::Escape, .. } if self.is_animating() => {
                self.anim = None;
                self.inertia = None;
                self.drag = None;
                self.drag_origin = None;
                self.move_accum = DVec2::default();
                self.pivot = None;
                InputResponse {
                    consumed: true,
                    redraw: true,
                    cursor: Some(MouseCursor::Default),
                    ..Default::default()
                }
            }
            ViewportInputKind::Frame { dt, time } => self.tick(state, dt, time),
            _ => self.handle_orbit(cx, input, state),
        }
    }

    fn frame(
        &mut self,
        _cx: &mut Cx,
        state: &mut AppState,
        bounds: Aabb,
        animate: bool,
    ) {
        if aabb_is_empty(&bounds) {
            return;
        }
        let aspect = self.aspect();
        let preset = state.view.pane(self.pane).preset;
        let mut to = self.heading(state);
        to.frame_bounds(&bounds, aspect);
        self.glide(
            state,
            to,
            if animate { ANIM_SLOW } else { 0.0 },
            preset,
        );
    }

    fn preset(
        &mut self,
        _cx: &mut Cx,
        state: &mut AppState,
        preset: PresetView,
        animate: bool,
    ) {
        let cam = self.heading(state);
        let was = state.view.pane(self.pane).preset;
        let mut to = orbit::preset_camera(&cam, preset, self.auto_persp);
        // Auto-perspective the other way round: leaving an axis view for a
        // free angle gives the perspective back, but only when it was the
        // axis view that took it away — a deliberate `5` is left alone.
        if self.auto_persp
            && preset == PresetView::Isometric
            && to.ortho
            && matches!(was, Some(p) if p != PresetView::Isometric)
        {
            orbit::set_ortho(&mut to, false);
        }
        self.glide(
            state,
            to,
            if animate { ANIM_SLOW } else { 0.0 },
            Some(preset),
        );
    }

    fn frame_from(&mut self, _cx: &mut Cx, state: &mut AppState, bounds: Aabb, look: Vec3f, up: Vec3f) {
        if aabb_is_empty(&bounds) {
            return;
        }
        let mut to = self.heading(state);
        to.eye = to.target - look.normalize() * to.distance().clamp(orbit::MIN_DISTANCE, orbit::MAX_DISTANCE);
        to.up = up;
        to.frame_bounds(&bounds, self.aspect());
        self.glide(state, to, ANIM_SLOW, None);
    }

    /// The person's choice of projection (View › Orthographic, its key, the view bar's button):
    /// kept for the next start, the camera fitted to the scene at once, so a pick before the next
    /// frame starts behind it too. Switched straight back, the view is the one before, exactly
    /// ([`Self::switched`]). Cost: one camera copied and compared per switch.
    fn set_ortho(&mut self, _cx: &mut Cx, state: &mut AppState, ortho: bool) {
        let preset = state.view.pane(self.pane).preset;
        let was = *state.view.camera(self.pane);
        if was.ortho == ortho {
            return;
        }
        // the clip planes aside, as each frame fits them
        let untouched = |a: &Camera, b: &Camera| Camera { near: b.near, far: b.far, ..*a } == *b;
        let mut cam = match self.switched.take() {
            Some((before, gave)) if before.ortho == ortho && untouched(&gave, &was) => before,
            _ => {
                let mut cam = was;
                orbit::set_ortho(&mut cam, ortho);
                cam
            }
        };
        cam.fit_clip_planes(&state.scene.bounds, self.aspect());
        self.switched = Some((was, cam));
        crate::view_prefs::save_ortho(ortho);
        // A glide on its way goes on in the new projection: its next step would take the old
        // one back. Else the projection changes in a glide of its own (a narrowing perspective
        // between the two: `orbit::lerp_camera`).
        if let Some(anim) = &mut self.anim {
            orbit::set_ortho(&mut anim.from, ortho);
            orbit::set_ortho(&mut anim.to, ortho);
            self.commit(state, cam, preset);
        } else {
            self.glide(state, cam, ANIM_SLOW, preset);
        }
    }

    fn orbit_by(&mut self, _cx: &mut Cx, state: &mut AppState, dx: f32, dy: f32) {
        self.anim = None;
        let style = state.view.orbit_style;
        let preset = state.view.pane(self.pane).preset;
        let mut cam = *state.view.camera(self.pane);
        self.break_auto_ortho(&mut cam, preset);
        self.apply_orbit(&mut cam, style, dx, dy);
        self.commit(state, cam, None);
    }

    fn is_animating(&self) -> bool {
        self.anim.is_some() || self.inertia.is_some() || self.drag.is_some()
    }
}
script_mod! {
    use mod.prelude.fab.*
    #(gizmo::script_mod(vm))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Element;

    const TEST_RECT: Rect = Rect {
        pos: DVec2 { x: 0.0, y: 0.0 },
        size: DVec2 { x: 960.0, y: 640.0 },
    };

    fn send(
        nav: &mut Navigator,
        cx: &mut Cx,
        state: &mut AppState,
        kind: ViewportInputKind,
    ) {
        nav.handle(
            cx,
            &ViewportInput {
                rect: TEST_RECT,
                kind,
                hit: None,
            },
            state,
        );
    }

    fn camera_near(a: Camera, b: Camera) {
        let err = (a.eye - b.eye)
            .length()
            .max((a.target - b.target).length())
            .max((a.up - b.up).length());
        assert!(err < 2e-4, "camera mismatch {err}: {a:?} != {b:?}");
        assert_eq!(a.ortho, b.ortho);
        assert!((a.ortho_height - b.ortho_height).abs() < 2e-4);
    }

    #[test]
    fn opening_camera_uses_declared_parts_and_final_view_size() {
        let parts = [
            PlatePart { object: "mesh".into(), min_mm: [-80.0, 20.0, -10.0], max_mm: [40.0, 80.0, 60.0], ..Default::default() },
            PlatePart { object: "lattice".into(), min_mm: [300.0, 270.0, 0.0], max_mm: [410.0, 370.0, 180.0], ..Default::default() },
        ];
        let rect = Rect { size: dvec2(581.0, 938.0), ..TEST_RECT };
        let mut cameras = Vec::new();
        for order in [[0, 1], [1, 0]] {
            for resized in [false, true] {
                let mut cx = Cx::new(Box::new(|_, _| {}));
                let mut state = AppState::default();
                state.plate.bed_max = [350.0, 320.0];
                let mut nav = Navigator::default();
                if resized {
                    // A frame at the old size precedes the opening request.
                    send(&mut nav, &mut cx, &mut state, ViewportInputKind::Frame { dt: 0.0, time: 0.0 });
                }
                for i in order {
                    state.plate.parts.push(parts[i].clone());
                    // The display representation can lag the document: only the latest
                    // arriving part has a field/mesh box in this scene.
                    let mut scene = Scene::empty();
                    scene.bounds = Aabb {
                        min: crate::tools::transform::mm_to_world(parts[i].min_mm),
                        max: crate::tools::transform::mm_to_world(parts[i].max_mm),
                    };
                    state.update_scene(std::sync::Arc::new(scene), true);
                }
                let bounds = state.frame_all_bounds();
                nav.open(bounds);
                nav.handle(&mut cx, &ViewportInput {
                    rect, kind: ViewportInputKind::Frame { dt: 1.0, time: 1.0 }, hit: None,
                }, &mut state);
                nav.prepare_frame(&mut state, rect);
                cameras.push(*state.view.camera(nav.pane));
                // Once opened, redraws and resizes preserve the person's navigation.
                state.view.camera_mut(nav.pane).target.x += 0.1;
                let moved = *state.view.camera(nav.pane);
                nav.prepare_frame(&mut state, TEST_RECT);
                assert_eq!(*state.view.camera(nav.pane), moved, "layout must consume an opening only once");
                nav.open(bounds);
                nav.stop_motion();
                nav.prepare_frame(&mut state, rect);
                assert_eq!(*state.view.camera(nav.pane), moved, "switching to Monitor cancels an unshown opening");
            }
        }
        for camera in &cameras[1..] {
            assert_eq!(*camera, cameras[0], "part arrival order and pre-frame resize must not change the opening camera: {cameras:?}");
        }
        let mut expected = Camera::default();
        expected.frame_bounds(&Aabb {
            min: crate::tools::transform::mm_to_world([-80.0, 0.0, -10.0]),
            max: crate::tools::transform::mm_to_world([410.0, 370.0, 180.0]),
        }, (rect.size.x / rect.size.y) as f32);
        assert_eq!(cameras[0], expected, "the opening view includes all declared extents and fits the final viewport");
    }

    fn drag_path(frame_stride: usize, drag: Drag, style: OrbitStyle, pane: PaneId) -> Camera {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut state = AppState::default();
        state.view.orbit_style = style;
        let mut nav = Navigator::default();
        nav.pane = pane;
        let mods = KeyModifiers {
            shift: drag == Drag::Pan,
            control: drag == Drag::Dolly && !cfg!(target_vendor = "apple"),
            logo: drag == Drag::Dolly && cfg!(target_vendor = "apple"),
            ..Default::default()
        };
        send(
            &mut nav,
            &mut cx,
            &mut state,
            ViewportInputKind::PointerDown {
                button: PointerButton::Middle,
                pos: dvec2(300.0, 240.0),
                mods,
                tap_count: 1,
            },
        );
        let dt = frame_stride as f32 / 120.0;
        for i in 0..120 {
            let delta = dvec2(0.45 + (i % 7) as f64 * 0.015, 0.18 - (i % 5) as f64 * 0.01);
            send(
                &mut nav,
                &mut cx,
                &mut state,
                ViewportInputKind::PointerMove {
                    pos: dvec2(300.0, 240.0),
                    delta,
                    mods,
                    buttons: 4,
                },
            );
            if (i + 1) % frame_stride == 0 {
                send(
                    &mut nav,
                    &mut cx,
                    &mut state,
                    ViewportInputKind::Frame {
                        dt,
                        time: (i + 1) as f64 / 120.0,
                    },
                );
            }
        }
        *state.view.camera(pane)
    }

    /// A press on the plate where no part is turns the orbit about that point of the plate, as a
    /// press on a part turns it about the part's: from above, and from under the bed, where the
    /// plate is not drawn. A press off the plate keeps the fallback (here the look point).
    #[test]
    fn a_press_on_the_plate_turns_the_orbit_about_that_point() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let pane = PaneId::PRIMARY;
        let at = vec3(0.1, 0.08, 0.0);
        let press_and_turn = |state: &mut AppState, cx: &mut Cx, pos: DVec2| {
            let mut nav = Navigator::default();
            let mods = KeyModifiers::default();
            send(&mut nav, cx, state, ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, mods, tap_count: 1 });
            for _ in 0..4 {
                send(&mut nav, cx, state, ViewportInputKind::PointerMove { pos, delta: dvec2(6.0, -4.0), mods, buttons: 1 });
            }
            send(&mut nav, cx, state, ViewportInputKind::Frame { dt: 1.0 / 60.0, time: 1.0 });
            nav.pivot.expect("an orbit's pivot")
        };
        for under in [false, true] {
            let mut state = AppState::default();
            state.plate.bed_max = [350.0, 320.0];
            let cam = state.view.camera_mut(pane);
            (cam.target, cam.eye) = (vec3(0.175, 0.16, 0.0), vec3(0.6, -0.3, if under { -0.35 } else { 0.35 }));
            assert_eq!(state.view.camera(pane).under_bed(), under);
            let before = *state.view.camera(pane);
            let pos = ViewProjector::new(before, TEST_RECT).project(at).expect("the point in view");
            let pivot = press_and_turn(&mut state, &mut cx, pos);
            assert!((pivot - at).length() < 1e-5, "under {under}: the pivot {pivot:?}, the press on the plate at {at:?}");
            let after = ViewProjector::new(*state.view.camera(pane), TEST_RECT).project(at).expect("the point in view");
            assert!(state.view.camera(pane).eye != before.eye && (after - pos).length() < 0.5, "under {under}: the plate's point left its pixel: {pos:?} -> {after:?}");
        }
        // Off the plate, over its back edge: the look point.
        let mut state = AppState::default();
        state.plate.bed_max = [350.0, 320.0];
        let cam = state.view.camera_mut(pane);
        (cam.target, cam.eye) = (vec3(0.175, 0.16, 0.0), vec3(0.6, -0.3, 0.35));
        let target = state.view.camera(pane).target;
        let pos = ViewProjector::new(*state.view.camera(pane), TEST_RECT).project(vec3(0.1, 0.4, 0.0)).expect("in view");
        assert_eq!(press_and_turn(&mut state, &mut cx, pos), target);
    }

    /// An orbit that starts on an object the view shows whole turns about the object's centre (of
    /// both solids of a two-solid object, not the other object beside it), and that centre keeps
    /// its pixel while the point pressed moves. Zoomed into the object with the wheel, or pressed
    /// on the plate beside it, the orbit turns about the point pressed. The edge: an object's box
    /// at the framed share of the view still turns about its centre, a hair larger does not.
    #[test]
    fn an_orbit_on_an_object_in_view_turns_about_its_centre() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let pane = PaneId::PRIMARY;
        let mut state = AppState::default();
        state.plate.bed_max = [350.0, 320.0];
        let solids = [
            ("bracket/body", vec3(0.10, 0.10, 0.0), vec3(0.13, 0.12, 0.02)),
            ("bracket/pin", vec3(0.13, 0.105, 0.0), vec3(0.14, 0.115, 0.03)),
            ("knob", vec3(0.25, 0.20, 0.0), vec3(0.29, 0.24, 0.04)),
        ];
        let mut scene = Scene::empty();
        for (i, (object, min, max)) in solids.iter().enumerate() {
            let id = ElementId::from_index(i);
            scene.elements.push(Element { id, name: object.to_string(), bounds: aabb_empty(), ranges: Vec::new(), triangle_count: 0 });
            scene.set_field_bounds(id, Aabb { min: *min, max: *max });
            state.plate.parts.push(PlatePart { object: object.to_string(), element: Some(id), printed: true, ..Default::default() });
        }
        state.scene = std::sync::Arc::new(scene);
        let bracket = Aabb { min: vec3(0.10, 0.10, 0.0), max: vec3(0.14, 0.12, 0.03) };
        let centre = aabb_center(&bracket);
        // On the body's top face, well off the bracket's centre.
        let pressed = vec3(0.105, 0.105, 0.02);
        let on_body = RayHit { element: ElementId::from_index(0), batch: u32::MAX, triangle: u32::MAX, t: 0.1, point: pressed, normal: vec3(0.0, 0.0, 1.0), bary: [0.0, 0.0] };
        let input = |kind, hit| ViewportInput { rect: TEST_RECT, kind, hit };
        let orbit = |state: &mut AppState, cx: &mut Cx, pos: DVec2, hit: Option<RayHit>| {
            let mut nav = Navigator::default();
            let mods = KeyModifiers::default();
            nav.handle(cx, &input(ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, mods, tap_count: 1 }, hit), state);
            for _ in 0..4 {
                nav.handle(cx, &input(ViewportInputKind::PointerMove { pos, delta: dvec2(6.0, -4.0), mods, buttons: 1 }, hit), state);
            }
            nav.handle(cx, &input(ViewportInputKind::Frame { dt: 1.0 / 60.0, time: 1.0 }, None), state);
            nav.pivot.expect("an orbit's pivot")
        };
        let pixel = |cam: Camera, p: Vec3f| ViewProjector::new(cam, TEST_RECT).project(p).expect("in view");
        let near = |a: Vec3f, b: Vec3f| (a - b).length() < 1e-5;

        // Framed as Frame Selected frames it: the bracket turns in place.
        let mut framed = Camera::default();
        framed.frame_bounds(&bracket, (TEST_RECT.size.x / TEST_RECT.size.y) as f32);
        *state.view.camera_mut(pane) = framed;
        let pos = pixel(framed, pressed);
        let pivot = orbit(&mut state, &mut cx, pos, Some(on_body));
        assert!(near(pivot, centre), "a framed object: the pivot {pivot:?}, the bracket's centre {centre:?}");
        let (stay, moved) = ((pixel(*state.view.camera(pane), centre) - pixel(framed, centre)).length(), (pixel(*state.view.camera(pane), pressed) - pos).length());
        assert!(stay < 0.5 && moved > 2.0, "the centre keeps its pixel (moved {stay} pt) while the pressed point turns ({moved} pt)");

        // Pressed on the plate beside it, in the same view: that point of the plate.
        *state.view.camera_mut(pane) = framed;
        let on_plate = vec3(0.12, 0.13, 0.0);
        let pivot = orbit(&mut state, &mut cx, pixel(framed, on_plate), None);
        assert!(near(pivot, on_plate), "the plate: the pivot {pivot:?}, the press at {on_plate:?}");

        // Zoomed in to the pressed point with the wheel: that point.
        *state.view.camera_mut(pane) = framed;
        let mut nav = Navigator::default();
        for _ in 0..3 {
            nav.handle(&mut cx, &input(ViewportInputKind::Scroll { delta: dvec2(0.0, -120.0), pos, mods: KeyModifiers::default() }, Some(on_body)), &mut state);
        }
        let bounds = state.scene.bounds;
        state.view.camera_mut(pane).fit_clip_planes(&bounds, (TEST_RECT.size.x / TEST_RECT.size.y) as f32);
        let pivot = orbit(&mut state, &mut cx, pos, Some(on_body));
        assert!(near(pivot, pressed), "zoomed into the object: the pivot {pivot:?}, the press at {pressed:?}");

        // The edge, looking straight down orthographically: the bracket is 40 mm wide, so at an
        // ortho height of `fit` its box spans the framed share of the view's width exactly.
        let fit = 0.04 / ((TEST_RECT.size.x / TEST_RECT.size.y) * FRAMED_VIEW_SHARE) as f32;
        for (height, framed) in [(fit * 1.01, true), (fit * 0.99, false)] {
            *state.view.camera_mut(pane) = Camera { eye: centre + vec3(0.0, 0.0, 0.5), target: centre, up: vec3(0.0, 1.0, 0.0), ortho: true, ortho_height: height, ..Camera::default() };
            let pos = pixel(*state.view.camera(pane), pressed);
            let pivot = orbit(&mut state, &mut cx, pos, Some(on_body));
            assert!(near(pivot, if framed { centre } else { pressed }), "ortho height {height} (fits at {fit}): the pivot {pivot:?}");
        }
    }

    /// A drag whose pointer rests asks for no frames, yet its release coasts as the velocity
    /// filtered every frame would have it: a flick coasts; a release after a pause of two
    /// refreshes coasts at a quarter of that (half per refresh), after half a second not at all;
    /// a flick after a rest coasts as fast as one without it. Frames come only when the
    /// navigation asks for one, as the view delivers them.
    #[test]
    fn a_resting_drag_asks_no_frames_and_coasts_as_frame_by_frame() {
        /// A middle-button orbit at 60 Hz, each refresh's pointer travel in points (0: still),
        /// released after the last: its coast's speed and the frames it took while still.
        fn orbit(cx: &mut Cx, travel: &[f64]) -> (Option<f64>, usize) {
            let (mods, pos) = (KeyModifiers::default(), dvec2(300.0, 240.0));
            let (mut state, mut nav) = (AppState::default(), Navigator::default());
            send(&mut nav, cx, &mut state, ViewportInputKind::PointerDown { button: PointerButton::Middle, pos, mods, tap_count: 1 });
            let (mut due, mut last, mut still_frames) = (true, 0.0, 0);
            for (i, &dx) in travel.iter().chain([0.0].iter()).enumerate() {
                let time = (i + 1) as f64 / 60.0;
                if i == travel.len() {
                    send(&mut nav, cx, &mut state, ViewportInputKind::PointerUp { button: PointerButton::Middle, pos, mods });
                    due = nav.needs_frames();
                } else if dx != 0.0 {
                    send(&mut nav, cx, &mut state, ViewportInputKind::PointerMove { pos, delta: dvec2(dx, 0.0), mods, buttons: 4 });
                    due = true;
                }
                if due {
                    send(&mut nav, cx, &mut state, ViewportInputKind::Frame { dt: (time - last).min(0.1) as f32, time });
                    (due, last) = (nav.needs_frames(), time);
                    still_frames += usize::from(dx == 0.0 && i < travel.len());
                }
            }
            (nav.inertia.map(|(_, v)| v.length()), still_frames)
        }
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let flick = [20.0; 6];
        let (coast, _) = orbit(&mut cx, &flick);
        let coast = coast.expect("a flick coasts");
        let paused = |rest: usize| [&flick[..], &vec![0.0; rest]].concat();
        let (short, still) = orbit(&mut cx, &paused(2));
        let short = short.expect("a release two refreshes after a flick still coasts");
        assert!((short - coast / 4.0).abs() < 0.01 * coast && still == 1, "two still refreshes: a coast of {short} for the flick's {coast} (a quarter expected), {still} frames taken still");
        let (long, still) = orbit(&mut cx, &paused(30));
        assert!(long.is_none() && still == 1, "half a second still: a coast of {long:?}, and {still} frames taken still (only the one that finds the pointer resting)");
        let (again, _) = orbit(&mut cx, &[&paused(30)[..], &flick[..]].concat());
        let again = again.expect("a flick after a rest coasts");
        assert!((again - coast).abs() < 0.01 * coast, "a flick after a rest coasts at {again}, without one at {coast}");
    }

    #[test]
    fn pointer_path_is_independent_of_frame_cadence() {
        for (drag, style) in [
            (Drag::Orbit, OrbitStyle::Turntable),
            (Drag::Orbit, OrbitStyle::Trackball),
            (Drag::Pan, OrbitStyle::Turntable),
            (Drag::Dolly, OrbitStyle::Turntable),
        ] {
            camera_near(drag_path(1, drag, style, PaneId::PRIMARY), drag_path(10, drag, style, PaneId::PRIMARY));
        }
    }

    #[test]
    fn numpad_emulation_maps_the_top_row() {
        let nav = Navigator::default();
        let plain = KeyModifiers::default();
        let ctrl = KeyModifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(
            nav.preset_for_key(KeyCode::Key1, plain),
            Some(PresetView::Front)
        );
        assert_eq!(
            nav.preset_for_key(KeyCode::Numpad1, plain),
            Some(PresetView::Front)
        );
        assert_eq!(
            nav.preset_for_key(KeyCode::Key1, ctrl),
            Some(PresetView::Back)
        );
        assert_eq!(
            nav.preset_for_key(KeyCode::Key7, plain),
            Some(PresetView::Top)
        );
        assert_eq!(
            nav.preset_for_key(KeyCode::Key3, ctrl),
            Some(PresetView::Left)
        );
        assert_eq!(nav.preset_for_key(KeyCode::KeyW, plain), None);
        // 2/4/6/8 step, they are not presets.
        assert_eq!(nav.preset_for_key(KeyCode::Key2, plain), None);
        assert!(nav.orbit_step_for_key(KeyCode::Key2).is_some());
        assert!(nav.orbit_step_for_key(KeyCode::Key1).is_none());
    }

    #[test]
    fn orbit_step_is_fifteen_degrees() {
        let mut cam = Camera::default();
        cam.target = vec3(0.0, 0.0, 0.0);
        cam.eye = vec3(10.0, 0.0, 0.0);
        cam.up = orbit::WORLD_UP;
        let before = cam.forward();
        let (dx, dy) = Navigator::default()
            .orbit_step_for_key(KeyCode::Numpad6)
            .unwrap();
        orbit::orbit_turntable(&mut cam, dx, dy);
        let angle = before.dot(cam.forward()).clamp(-1.0, 1.0).acos().to_degrees();
        assert!((angle - 15.0).abs() < 0.05, "stepped {angle} degrees");
    }

    /// A camera move, its frames at 60 a second: `n` of them, or (none) until the camera is still.
    fn run_frames(nav: &mut Navigator, cx: &mut Cx, state: &mut AppState, time: &mut f64, n: Option<usize>) {
        let mut k = 0;
        while n.map_or(nav.is_animating(), |n| k < n) {
            *time += 1.0 / 60.0;
            send(nav, cx, state, ViewportInputKind::Frame { dt: 1.0 / 60.0, time: *time });
            k += 1;
        }
    }

    type Move<'a> = &'a dyn Fn(&mut Navigator, &mut Cx, &mut AppState);

    /// `first`, then `second` asked `frames` frames into the glide `first` starts (none: once the
    /// camera is still), from an angle above the front right: the camera once all is still.
    fn one_after_another(cx: &mut Cx, first: Move, second: Move, frames: Option<usize>, pane: PaneId) -> Camera {
        let mut state = AppState::default();
        state.plate.bed_max = [350.0, 320.0];
        let cam = state.view.camera_mut(pane);
        (cam.target, cam.eye) = (vec3(0.175, 0.16, 0.015), vec3(0.67, -0.39, 0.38));
        let mut nav = Navigator::default();
        nav.pane = pane;
        let mut time = 0.0;
        // The view's size first, as a drawn view gives it.
        send(&mut nav, cx, &mut state, ViewportInputKind::Frame { dt: 0.0, time });
        first(&mut nav, cx, &mut state);
        run_frames(&mut nav, cx, &mut state, &mut time, frames);
        second(&mut nav, cx, &mut state);
        run_frames(&mut nav, cx, &mut state, &mut time, None);
        *state.view.camera(pane)
    }

    /// A move that builds on the camera, asked while a glide is on its way, builds on where the
    /// glide ends: it ends as it would once the glide is over. Frame Selected pressed during the
    /// View: Front glide framed the part along the glide's direction of the moment, degrees off
    /// the front; View: Front during a framing's glide took only part of the framing's look
    /// point and distance; a key's turn by a step during another's turned less than two steps.
    #[test]
    fn a_move_during_a_glide_ends_where_it_would_after_the_glide() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let pane = PaneId::PRIMARY;
        let part = Aabb { min: vec3(0.159, 0.144, 0.0), max: vec3(0.191, 0.176, 0.019) };
        let front = |nav: &mut Navigator, cx: &mut Cx, state: &mut AppState| nav.preset(cx, state, PresetView::Front, true);
        let framed = |nav: &mut Navigator, cx: &mut Cx, state: &mut AppState| nav.frame(cx, state, part, true);
        let step = |nav: &mut Navigator, cx: &mut Cx, state: &mut AppState| {
            send(nav, cx, state, ViewportInputKind::KeyDown { key: KeyCode::Numpad6, mods: KeyModifiers::default(), repeat: false });
        };
        let cases: [(&str, Move, Move); 3] = [
            ("Frame Selected during the View: Front glide", &front, &framed),
            ("View: Front during the Frame Selected glide", &framed, &front),
            ("a step during a step's glide", &step, &step),
        ];
        for (what, first, second) in cases {
            let still = one_after_another(&mut cx, first, second, None, pane);
            for frames in [0, 3, 9] {
                let during = one_after_another(&mut cx, first, second, Some(frames), pane);
                let off = (during.eye - still.eye).length().max((during.target - still.target).length()).max((during.up - still.up).length());
                assert!(off < 2e-4, "{what}, {frames} frames into it, ended {off} m off: {during:?}, once still {still:?}");
            }
        }
        // Framed after View: Front, the view looks straight along the front.
        let still = one_after_another(&mut cx, &front, &framed, Some(3), pane);
        let look = (still.target - still.eye).normalize();
        assert!(look.y > 0.99999, "framed during the View: Front glide, the view looks along {look:?}");
    }

    /// Orthographic and back is the view it was, close in as far as a view goes too, the eye
    /// stood back behind the scene in between; and the projection chosen during a glide holds
    /// to the glide's end, where the glide and then the choice would have left it.
    #[test]
    fn a_projection_toggle_comes_back_exactly_and_holds_through_a_glide() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let pane = PaneId::PRIMARY;
        let part = Aabb { min: vec3(0.159, 0.144, 0.0), max: vec3(0.191, 0.176, 0.019) };
        let look = |c: &Camera| (c.target - c.eye).normalize();
        for d in [orbit::MIN_DISTANCE, 0.3, 4.0] {
            let mut state = AppState::default();
            state.plate.bed_max = [350.0, 320.0];
            std::sync::Arc::make_mut(&mut state.scene).bounds = part;
            let target = vec3(0.175, 0.16, 0.015);
            let cam = state.view.camera_mut(pane);
            (cam.target, cam.eye) = (target, target + vec3(0.5, -0.55, 0.36).normalize() * d);
            let mut nav = Navigator::default();
            send(&mut nav, &mut cx, &mut state, ViewportInputKind::Frame { dt: 0.0, time: 0.0 });
            let was = *state.view.camera(pane);
            nav.set_ortho(&mut cx, &mut state, true);
            let ortho = *state.view.camera(pane);
            nav.set_ortho(&mut cx, &mut state, false);
            let back = *state.view.camera(pane);
            let off = (back.eye - was.eye).length().max((back.target - was.target).length());
            assert!(!back.ortho && off < 1e-6 * d.max(0.1), "{d} m off: perspective came back {off} m off: {was:?} -> {back:?}");
            nav.set_ortho(&mut cx, &mut state, true);
            let again = *state.view.camera(pane);
            let turn = (look(&again) - look(&ortho)).length();
            assert!(again.ortho && (again.ortho_height - ortho.ortho_height).abs() <= ortho.ortho_height * 1e-6 && turn < 1e-6, "{d} m off: orthographic came back {} m high and {turn} turned: {ortho:?} -> {again:?}", again.ortho_height);
        }
        let framed = |nav: &mut Navigator, cx: &mut Cx, state: &mut AppState| nav.frame(cx, state, part, true);
        let ortho = |nav: &mut Navigator, cx: &mut Cx, state: &mut AppState| nav.set_ortho(cx, state, true);
        let still = one_after_another(&mut cx, &framed, &ortho, None, pane);
        assert!(still.ortho);
        for frames in [0, 3, 9] {
            let during = one_after_another(&mut cx, &framed, &ortho, Some(frames), pane);
            let off = (during.target - still.target).length().max((look(&during) - look(&still)).length()).max((during.ortho_height - still.ortho_height).abs());
            assert!(during.ortho && off < 2e-4, "orthographic {frames} frames into the Frame Selected glide ended {off} off: {during:?}, once still {still:?}");
        }
    }
}
