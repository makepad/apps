//! The view tools: select (box select, the part gizmo), measure and
//! section.
//!
//! `ToolSet` — the active tool's input handler, owned by `FabViewport`, after
//! the navigator in the input chain (`api::ToolController`). It turns pointer
//! and key input into `ShellAction`s; every result it produces lives in
//! `AppState` (`measurements`, `scene_state.section`). In-flight state — the
//! half-placed measurement, a section-handle drag — lives in [`session`],
//! which the overlay reads too.
//!
//! | file | what |
//! |---|---|
//! | `snap.rs` | vertex / midpoint / edge / face snapping for the measure tool |
//! | `measure.rs` | distance / area / angle, units, the 1 mm accuracy gate |
//! | `section.rs` | section planes + section box, handles, animate-in |
//! | `isolate.rs` | H / Shift+H / Alt+H, solo |
//! | `transform.rs` | `FabPartGizmo` — move / turn / scale the selected part |
//! | `overlay.rs` | `FabToolOverlay` — everything the tools draw over the view |
//! | `force_edit.rs` | `FabForceEditor` — a load's typed values, by its glyph |

pub mod annotate;
pub mod areas;
pub mod fine_drag;
pub mod force_edit;
pub mod handles;
pub mod isolate;
pub mod mech_grab;
pub mod measure;
pub mod overlay;
pub mod paint;
pub mod picks;
pub mod sculpt;
pub mod section;
pub mod session;
pub mod snap;
pub mod test_cube;
pub mod transform;

use crate::api::*;
use crate::api::PaneId;
use makepad_widgets_core::*;
use makepad_widgets_core::makepad_platform::thread::{Class, TaskHandle};
use std::collections::VecDeque;
use std::sync::Arc;

/// Pointer travel (points) beyond which a click becomes a drag.
const DRAG_SLOP: f64 = 4.0;

struct Down {
    pos: DVec2,
}

struct EdgeRequest {
    scene: Arc<Scene>,
    proj: ViewProjector,
    hit: SnapHit,
    kind: MeasureKind,
    radius_px: f32,
    units: Units,
}

struct EdgeTask {
    scene: Arc<Scene>,
    kind: MeasureKind,
    task: TaskHandle<Result<Measurement, &'static str>>,
}

pub struct ToolSet {
    pub pane: PaneId,
    down: Option<Down>,
    pending_edges: VecDeque<EdgeRequest>,
    edge_task: Option<EdgeTask>,
}

impl Default for ToolSet {
    fn default() -> Self {
        Self { pane: PaneId::PLATE_ZERO, down: None, pending_edges: VecDeque::new(), edge_task: None }
    }
}

impl ToolSet {
    fn finish_edge(cx: &mut Cx, result: Result<Measurement, &'static str>) {
        match result {
            Ok(m) => {
                session::with(|s| {
                    s.measure.clear();
                    s.hint = format!("{}: {}", measure::kind_label(m.kind), m.label);
                });
                cx.action(ShellAction::AddMeasurement(m));
            }
            Err(message) => session::with(|s| s.hint = message.into()),
        }
    }

    fn poll_edges(&mut self, cx: &mut Cx, state: &AppState) -> bool {
        let mut changed = false;
        if let Some(task) = &mut self.edge_task {
            if let Some(result) = task.task.try_take() {
                if Arc::ptr_eq(&task.scene, &state.scene) && state.tool == Tool::Measure(task.kind) {
                    match result {
                        Ok(result) => Self::finish_edge(cx, result),
                        Err(error) => session::with(|s| s.hint = format!("Edge measurement: {error}")),
                    }
                }
                self.edge_task = None;
                changed = true;
            }
        }
        if self.edge_task.is_none() {
            while self.pending_edges.front().is_some_and(|r| !Arc::ptr_eq(&r.scene, &state.scene) || state.tool != Tool::Measure(r.kind)) {
                self.pending_edges.pop_front();
            }
            if !self.pending_edges.is_empty() {
                let request = self.pending_edges.pop_front().unwrap();
                let scene = request.scene.clone();
                let kind = request.kind;
                match cx.task_pool().submit_named(Class::Visible, "fab-measure-edge", move || {
                    measure::edge_measurement(&request.scene, &request.proj, &request.hit, request.kind, request.radius_px, &request.units)
                }) {
                    Ok(task) => self.edge_task = Some(EdgeTask { scene, kind, task }),
                    Err(error) => {
                        session::with(|s| s.hint = format!("Edge measurement failed: {error}"));
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Snap the cursor, remembering it for the overlay's glyph.
    fn snap_at(&self, state: &AppState, input: &ViewportInput, pos: DVec2) -> Option<SnapHit> {
        let proj = ViewProjector::new(*state.view.camera(self.pane), input.rect);
        let mut opts = state.snap;
        match state.tool {
            Tool::Measure(MeasureKind::FaceDistance) => {
                opts.vertex = false;
                opts.edge_midpoint = false;
                opts.edge = false;
            }
            Tool::Measure(MeasureKind::EdgeLength | MeasureKind::Radius) => {
                opts.vertex = false;
                opts.edge_midpoint = false;
                opts.edge = true;
            }
            _ => {}
        }
        snap::snap(&state.scene, &proj, pos, &opts, &|id| state.is_visible(id))
    }

    /// A section handle within grab range of the pointer.
    fn handle_at(&self, state: &AppState, input: &ViewportInput, pos: DVec2) -> Option<session::SectionHandle> {
        let proj = ViewProjector::new(*state.view.camera(self.pane), input.rect);
        let bounds = state.scene.bounds;
        let mut best: Option<(f64, session::SectionHandle)> = None;
        for (handle, world, _) in section::handles(&state.scene_state.section, &bounds) {
            if let Some(s) = proj.project(world) {
                let d = (s - pos).length();
                if d < 14.0 && best.map_or(true, |b| d < b.0) {
                    best = Some((d, handle));
                }
            }
        }
        best.map(|b| b.1)
    }

    fn start_handle_drag(&self, state: &AppState, input: &ViewportInput, pos: DVec2, handle: session::SectionHandle) -> bool {
        let proj = ViewProjector::new(*state.view.camera(self.pane), input.rect);
        let bounds = state.scene.bounds;
        let Some((world, axis)) = section::handle_anchor(&state.scene_state.section, &bounds, handle) else {
            return false;
        };
        let Some(value) = section::handle_value(&state.scene_state.section, handle) else {
            return false;
        };
        // Screen direction of one metre along the handle axis.
        let (Some(a), Some(b)) = (proj.project(world), proj.project(world + axis)) else {
            return false;
        };
        let d = b - a;
        let len = d.length();
        if len < 1e-6 {
            return false;
        }
        session::with(|s| {
            s.section_drag = Some(session::SectionDrag {
                handle,
                start: pos,
                start_value: value,
                axis_screen: d / len,
                points_per_meter: len,
            });
        });
        true
    }

    fn commit_measure(&self, cx: &mut Cx, state: &mut AppState, points: Vec<Vec3f>, normals: Vec<Vec3f>, kind: MeasureKind) {
        let units = state.units;
        if let Some(m) = measure::commit_with_normals(kind, points, normals, &units) {
            let hint = format!("{}: {}", measure::kind_label(kind), m.label);
            session::with(|s| s.hint = hint);
            cx.action(ShellAction::AddMeasurement(m));
        }
    }

    /// Place one measurement point; commit when the shape is complete.
    fn place_point(&mut self, cx: &mut Cx, state: &mut AppState, hit: SnapHit, proj: &ViewProjector) {
        let kind = match state.tool {
            Tool::Measure(k) => k,
            _ => return,
        };
        if matches!(kind, MeasureKind::EdgeLength | MeasureKind::Radius) {
            let count = hit.element.and_then(|id| state.scene.element(id))
                .map_or(0, |el| el.ranges.iter().map(|r| r.2 as usize).sum::<usize>());
            if count > snap::MAX_SCANNED_INDICES {
                log!("Edge scan of {count} indices queued on the task pool");
                self.pending_edges.push_back(EdgeRequest { scene: state.scene.clone(), proj: *proj, hit, kind, radius_px: state.snap.radius_px, units: state.units });
                session::with(|s| s.hint = "Measuring feature edge…".into());
                self.poll_edges(cx, state);
            } else {
                Self::finish_edge(cx, measure::edge_measurement(&state.scene, proj, &hit, kind, state.snap.radius_px, &state.units));
            }
            return;
        }
        if kind == MeasureKind::FaceDistance && (hit.kind != SnapKind::Face || hit.element.is_none() || hit.normal.is_none()) {
            session::with(|s| s.hint = "Pick a part's face".into());
            return;
        }
        let ready = session::with(|s| {
            if s.measure.kind != kind {
                s.measure.clear();
                s.measure.kind = kind;
            }
            // Closing an area loop by clicking the first point again.
            if kind == MeasureKind::Area && s.measure.points.len() >= 3 {
                if (s.measure.points[0] - hit.point).length() < 1e-3 {
                    s.measure.snaps.clear();
                    return Some((std::mem::take(&mut s.measure.points), std::mem::take(&mut s.measure.normals)));
                }
            }
            s.measure.points.push(hit.point);
            s.measure.snaps.push(hit.kind);
            s.measure.normals.push(hit.normal.unwrap_or_default());
            s.hint = format!(
                "{} point {} · {}",
                measure::kind_label(kind),
                s.measure.points.len(),
                snap::label(hit.kind)
            );
            if s.measure.points.len() >= measure::needed(kind) {
                s.measure.snaps.clear();
                Some((std::mem::take(&mut s.measure.points), std::mem::take(&mut s.measure.normals)))
            } else {
                None
            }
        });
        if let Some((points, normals)) = ready {
            self.commit_measure(cx, state, points, normals, kind);
        }
    }

    /// Enter / right-click while an area loop is open.
    fn close_loop(&self, cx: &mut Cx, state: &mut AppState) -> bool {
        let kind = match state.tool {
            Tool::Measure(k) => k,
            _ => return false,
        };
        let points = session::with(|s| {
            if s.measure.points.len() >= 3 {
                s.measure.snaps.clear();
                s.measure.normals.clear();
                Some(std::mem::take(&mut s.measure.points))
            } else {
                None
            }
        });
        match points {
            Some(p) => {
                self.commit_measure(cx, state, p, Vec::new(), kind);
                true
            }
            None => false,
        }
    }

    fn place_section_from_hit(&self, cx: &mut Cx, state: &AppState, hit: &RayHit) {
        let target = section::single(section::plane_from_hit(hit));
        let bounds = state.scene.bounds;
        let (anim, first) = section::animate_to(&state.scene_state.section, target, &bounds);
        session::with(|s| {
            s.section_anim = Some(anim);
            s.hint = format!("Section from face — drag the handle to offset, {} to flip", crate::ui::keymap::shortcut(crate::ui::keymap::ShortcutId::SectionFlip));
        });
        cx.action(ShellAction::SetSection(first));
    }
}

impl ToolController for ToolSet {
    fn handle(&mut self, cx: &mut Cx, input: &ViewportInput, state: &mut AppState) -> InputResponse {
        if let Some(response) = annotate::handle(cx, input, state, self.pane) {
            self.down = None;
            return response;
        }
        if let Some(response) = paint::handle(cx, input, state) {
            self.down = None;
            return response;
        }
        if let Some(response) = sculpt::handle(cx, input, state) {
            self.down = None;
            return response;
        }
        if let Some(response) = areas::handle(cx, input, state, self.pane) {
            self.down = None;
            return response;
        }
        match input.kind {
            ViewportInputKind::Frame { .. } => {
                let redraw = self.poll_edges(cx, state);
                InputResponse { redraw, wants_frames: self.edge_task.is_some() || !self.pending_edges.is_empty(), ..Default::default() }
            }
            ViewportInputKind::PointerDown {
                button: PointerButton::Primary,
                pos,
                mods,
                ..
            } => {
                if state.ui.lay_flat && mods.shift {
                    self.down = None;
                    return InputResponse::default();
                }
                self.down = Some(Down { pos });
                if state.tool == Tool::Section && !mods.shift {
                    if let Some(handle) = self.handle_at(state, input, pos) {
                        if self.start_handle_drag(state, input, pos, handle) {
                            return InputResponse::consumed();
                        }
                    }
                }
                InputResponse {
                    consumed: true,
                    ..Default::default()
                }
            }

            ViewportInputKind::PointerDown {
                button: PointerButton::Secondary,
                ..
            } => {
                // Right-click closes an open area loop; otherwise it belongs
                // to whoever owns context menus.
                if self.close_loop(cx, state) {
                    InputResponse::consumed()
                } else {
                    InputResponse::default()
                }
            }

            ViewportInputKind::PointerMove { pos, buttons, mods, .. } => {
                let mut resp = InputResponse::default();
                if state.ui.lay_flat && buttons == 0 {
                    resp.cursor = Some(if state.view.hover.is_some_and(|hit| state.plate.part_of(hit.element).is_some()) { MouseCursor::Crosshair } else { MouseCursor::Default });
                }
                // 1. a section handle drag
                let drag = session::with(|s| s.section_drag);
                if let Some(drag) = drag {
                    if buttons & 1 != 0 {
                        let d = pos - drag.start;
                        let along = (d.x * drag.axis_screen.x + d.y * drag.axis_screen.y) / drag.points_per_meter.max(1e-6);
                        let mut value = drag.start_value + along as f32;
                        if mods.control {
                            value = (value * 1000.0).round() * 0.001; // 1 mm snap
                        }
                        let mut section = state.scene_state.section.clone();
                        let limits = state.scene.bounds;
                        section::apply_handle(&mut section, drag.handle, value, &limits);
                        session::with(|s| {
                            s.hint = format!("Section offset {:.1} mm", value * 1000.0);
                            s.section_anim = None;
                        });
                        cx.action(ShellAction::SetSection(section));
                        return InputResponse::consumed();
                    }
                    session::with(|s| s.section_drag = None);
                }

                // 2. snapping preview under the cursor
                match state.tool {
                    Tool::Measure(kind) => {
                        let hit = self.snap_at(state, input, pos);
                        session::with(|s| {
                            s.snap_cursor = hit;
                            s.measure.kind = kind;
                            s.measure.preview = hit;
                        });
                        resp.redraw = true;
                    }
                    Tool::Section => {
                        let hover = self.handle_at(state, input, pos);
                        let changed = session::with(|s| {
                            let c = s.section_hover != hover;
                            s.section_hover = hover;
                            c
                        });
                        if changed {
                            resp.redraw = true;
                            if hover.is_some() {
                                resp.cursor = Some(MouseCursor::Grab);
                            }
                        }
                    }
                    _ => {
                        let had = session::with(|s| {
                            let had = s.snap_cursor.is_some() || s.measure.preview.is_some();
                            s.snap_cursor = None;
                            s.measure.preview = None;
                            had
                        });
                        resp.redraw = had;
                    }
                }
                resp
            }

            ViewportInputKind::PointerUp {
                button: PointerButton::Primary,
                pos,
                mods,
            } => {
                let dragging_handle = session::with(|s| s.section_drag.take()).is_some();
                if dragging_handle {
                    return InputResponse::consumed();
                }
                let Some(down) = self.down.take() else {
                    return InputResponse::default();
                };
                if (pos - down.pos).length() > DRAG_SLOP {
                    return InputResponse::default();
                }

                match state.tool {
                    Tool::Select => {
                        // A click on the pick kind filter's strip is the strip's: the part
                        // under it stays picked.
                        if crate::tools::picks::active(state) && crate::tools::picks::strip(input.rect).iter().any(|(r, _)| r.contains(pos)) {
                            return InputResponse::consumed();
                        }
                        if state.ui.lay_flat && !mods.shift {
                            // A test print's piece shown, picked in place or the ticked ones on the
                            // bed: the click lays the piece, where the app finds the face it meets.
                            let piece = match (state.test_cut_shown(), state.plate.whole.is_some()) {
                                (true, _) => crate::ui::test_prints::picked(state).map(|t| Some(t.name.clone())),
                                (false, true) => Some(None),
                                (false, false) => None,
                            };
                            if let (Some(name), Some(_)) = (piece, input.hit) {
                                let ray = ViewProjector::new(*state.view.camera(self.pane), input.rect).ray(pos);
                                let mm = |v: Vec3f| [v.x as f64 * 1000.0, v.y as f64 * 1000.0, v.z as f64 * 1000.0];
                                cx.action(TestPrintAction::LayFlat { name, from: mm(ray.origin), dir: [ray.dir.x as f64, ray.dir.y as f64, ray.dir.z as f64] });
                                state.ui.lay_flat = false;
                                cx.set_cursor(MouseCursor::Default);
                                crate::ui::redraw(cx, &[ids!(tab_viewport)]);
                                return InputResponse::consumed();
                            }
                            if let Some(hit) = input.hit {
                                if let Some(i) = state.plate.part_of(hit.element) {
                                    cx.action(PartAction::LayFlat {
                                        object: state.plate.parts[i].object.clone(),
                                        normal: [hit.normal.x as f64, hit.normal.y as f64, hit.normal.z as f64],
                                    });
                                    state.ui.lay_flat = false;
                                    cx.set_cursor(MouseCursor::Default);
                                    // Tool drafts and lay-flat controls live in the viewport; committed actions refresh other panels.
                                    crate::ui::redraw(cx, &[ids!(tab_viewport)]);
                                    return InputResponse::consumed();
                                }
                            }
                        }
                        let proj = ViewProjector::new(*state.view.camera(self.pane), input.rect);
                        if let Some(index) = measure::pick(state, &proj, pos) {
                            cx.action(MeasureAction::Select(index));
                            return InputResponse::consumed();
                        }
                        // A click on a part picks the part, on nothing nothing: a print
                        // settings box or override picked before is not any more.
                        let unpick = |cx: &mut Cx| {
                            if state.ui.picked_override.is_some() || crate::ui::param_panel::picked_volume(state).is_some() || crate::ui::test_prints::picked(state).is_some() {
                                cx.action(ShellAction::PickOverride(None));
                            }
                        };
                        match input.hit {
                            Some(hit) => {
                                // Shift in the Edit shape mode adds or takes off a pick, and
                                // keeps the part it is on picked.
                                if mods.shift && crate::tools::picks::active(state) {
                                    cx.action(ShellAction::SelectAdd(hit.element));
                                } else if mods.shift {
                                    cx.action(ShellAction::SelectToggle(hit.element));
                                } else {
                                    cx.action(ShellAction::SelectOnly(hit.element));
                                    unpick(cx);
                                }
                                // Move, Turn and Scale work on what a click picks: the part,
                                // not a feature of it the tree had in focus.
                                if state.params.focus.is_some() && matches!(state.ui.gizmo, GizmoMode::Move | GizmoMode::Rotate | GizmoMode::Scale) {
                                    cx.action(crate::params::ParamAction::Focus(None));
                                }
                            }
                            None => {
                                if !mods.shift {
                                    cx.action(ShellAction::ClearSelection);
                                    unpick(cx);
                                }
                            }
                        }
                        InputResponse::consumed()
                    }
                    Tool::Measure(_) => {
                        if let Some(hit) = self.snap_at(state, input, pos) {
                            let proj = ViewProjector::new(*state.view.camera(self.pane), input.rect);
                            self.place_point(cx, state, hit, &proj);
                        }
                        InputResponse { wants_frames: self.edge_task.is_some() || !self.pending_edges.is_empty(), ..InputResponse::consumed() }
                    }
                    Tool::Section => {
                        match input.hit {
                            Some(hit) => self.place_section_from_hit(cx, state, &hit),
                            None => session::with(|s| s.hint = "Click a part's face to cut there".into()),
                        }
                        InputResponse::consumed()
                    }
                    // Out of the Sliced view its tools draw nothing.
                    Tool::NoTrees | Tool::SettingsHere => InputResponse::default(),
                }
            }

            ViewportInputKind::HoverOut => {
                if state.ui.lay_flat {
                    cx.set_cursor(MouseCursor::Default);
                }
                let had = session::with(|s| {
                    let had = s.snap_cursor.is_some() || s.measure.preview.is_some();
                    s.snap_cursor = None;
                    s.measure.preview = None;
                    had
                });
                InputResponse {
                    redraw: had,
                    ..Default::default()
                }
            }

            ViewportInputKind::KeyDown { key, mods, repeat }
                if !repeat && !mods.control && !mods.logo && !state.ui.in_mode(PlateMode::Mechanics) => {
                match key {
                    KeyCode::Backspace | KeyCode::Delete => {
                        if !delete_measurement(cx, state, key) {
                            return InputResponse::default();
                        }
                    }
                    KeyCode::ReturnKey | KeyCode::NumpadEnter => {
                        if !self.close_loop(cx, state) {
                            return InputResponse::default();
                        }
                    }
                    KeyCode::Escape => {
                        if state.ui.lay_flat {
                            state.ui.lay_flat = false;
                            self.down = None;
                            cx.set_cursor(MouseCursor::Default);
                            // Tool drafts and lay-flat controls live in the viewport; committed actions refresh other panels.
                            crate::ui::redraw(cx, &[ids!(tab_viewport)]);
                            return InputResponse::consumed();
                        }
                        self.pending_edges.clear();
                        if let Some(task) = self.edge_task.take() { task.task.cancel(); }
                        let had = session::with(|s| {
                            let had = !s.measure.is_empty();
                            s.measure.clear();
                            s.measure_drag = None;
                            s.section_drag = None;
                            had
                        });
                        // nothing to let go of: leave the tool, or (in Select) the selection
                        if !had && state.tool != Tool::Select {
                            cx.action(ShellAction::SetTool(Tool::Select));
                        } else if !had {
                            cx.action(ShellAction::ClearSelection);
                            if state.ui.picked_override.is_some() || crate::ui::param_panel::picked_volume(state).is_some() || crate::ui::test_prints::picked(state).is_some() {
                                cx.action(ShellAction::PickOverride(None));
                            }
                        }
                    }
                    _ => return InputResponse::default(),
                }
                InputResponse::consumed()
            }

            _ => InputResponse::default(),
        }
    }
}

/// Widget registration.
pub fn script_mod(vm: &mut ScriptVm) {
    overlay::script_mod(vm);
    transform::script_mod(vm);
    test_cube::script_mod(vm);
    mech_grab::script_mod(vm);
    handles::script_mod(vm);
    force_edit::script_mod(vm);
}

/// The tools' action hook, called from `App::dispatch`.
pub fn apply(_cx: &mut Cx, state: &mut AppState, action: &ShellAction) -> bool {
    match action {
        ShellAction::SelectOnly(_) | ShellAction::SelectToggle(_) | ShellAction::SelectAdd(_)
        | ShellAction::SelectSet(_) | ShellAction::ClearSelection => {
            session::with(|s| {
                s.measure_drag = None;
                s.snap_cursor = None;
            });
            true
        }
        ShellAction::Loaded(_) | ShellAction::SceneUpdated { .. } => {
            if matches!(action, ShellAction::Loaded(_)) {
                // another document: the marks were of the last one
                session::with(|s| s.marker = Default::default());
            }
            session::with(|s| {
                // A queued paint edit finishing must not swallow the next press.
                // The brush validates the retained part's placement before sampling.
                let keep_paint = matches!(action, ShellAction::SceneUpdated { frame: false, .. }) && paint::active(state);
                let draft = keep_paint.then(|| std::mem::take(&mut s.paint));
                let keep_sculpt = matches!(action, ShellAction::SceneUpdated { frame: false, .. }) && sculpt::active(state);
                let sculpt = keep_sculpt.then(|| std::mem::take(&mut s.sculpt));
                s.reset_for_scene();
                // the released stroke is in the plate now (its paint bakes and shows it)
                if let Some(mut draft) = draft { draft.hover = None; draft.pending = None; s.paint = draft; }
                if let Some(mut draft) = sculpt { draft.hover = None; s.sculpt = draft; }
            });
            true
        }
        ShellAction::SetTool(tool) => {
            if *tool != Tool::Select {
                state.ui.lay_flat = false;
            }
            session::with(|s| {
                s.measure.clear();
                s.measure_drag = None;
                s.section_drag = None;
                if let Tool::Measure(k) = tool {
                    s.measure.kind = *k;
                }
                s.hint.clear();
            });
            true
        }
        ShellAction::SetWorkspace(_) if !state.ui.in_mode(PlateMode::Design) => {
            state.ui.lay_flat = false;
            true
        }
        _ => false,
    }
}

/// Used by global keys (Parts or viewport focus) and the viewport's tool input.
/// Backspace's draft undo takes priority over deleting a saved ruler.
pub fn delete_measurement(cx: &mut Cx, state: &AppState, key: KeyCode) -> bool {
    if (key == KeyCode::Backspace || state.selected_measure.is_none()) && matches!(state.tool, Tool::Measure(_)) && session::with(|s| {
        if s.measure.is_empty() { return false; }
        s.measure.undo();
        true
    }) {
        // Tool drafts and lay-flat controls live in the viewport; committed actions refresh other panels.
        crate::ui::redraw(cx, &[ids!(tab_viewport)]);
        return true;
    }
    if let Some(index) = state.selected_measure {
        session::with(|s| s.measure_drag = None);
        cx.action(MeasureAction::Delete(index));
        true
    } else {
        false
    }
}
