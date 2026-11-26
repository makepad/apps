//! The Electronics device editor. Its viewport keeps its camera when the dock parks the body.

use crate::api::{aabb_empty, aabb_is_empty, aabb_union, Aabb, AppState, Camera, DeviceRestore, ElementId, PaneId, PaneLease, PaneMode, PanePoses, PaneRole, PoseSetId, PresetView, ShellAction, TraceProgress};
use crate::model::{aabb_union_point, state::pose_apply};
use crate::tools::section::box_corners;
use crate::tools::transform::mm_to_world;
use crate::viewport::FabViewport;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // the toolbar's segments and buttons: no faces of their own at rest
    let Segment = mod.widgets.FabSegmentTab{
        height: 22
        padding: Inset{left: 14 right: 14 top: 0 bottom: 0}
        draw_bg +: {color: vec4(0.0) color_focus: vec4(0.0)}
    }
    let ViewButton = mod.widgets.FabFlatButton{
        height: 22
        padding: Inset{left: 8 right: 8 top: 0 bottom: 0}
        draw_text +: {color: fab.color_text_dim}
    }

    mod.widgets.FabDevice3dBase = #(FabDevice3d::register_widget(vm))
    mod.widgets.FabDevice3d = set_type_default() do mod.widgets.FabDevice3dBase{
        width: Fill height: Fill flow: Down
        event_order: EventOrder.Up
        // the editor's toolbar, as every editor's (its area's selector at the left end): how the
        // product shows (assembled or as a kit), the camera's presets, the case and the shot
        editor_bar := mod.widgets.FabEditorBar{
            assembled := Segment{text: "Assembled"}
            kit := Segment{text: "Kit"}
            mod.widgets.FabVRule{}
            reset := mod.widgets.FabFlatButton{
                text: "Reset" height: 22
                padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
                draw_text +: {color: fab.color_text_dim}
            }
            iso := ViewButton{text: "Iso"}
            top := ViewButton{text: "Top"}
            front := ViewButton{text: "Front"}
            mod.widgets.FabVRule{}
            case := Segment{
                text: "Case" independent: true
                padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
                draw_bg +: {color_active: fab.color_button_active}
                draw_text +: {color_active: fab.color_text_on_accent}
            }
            // the rare product shot: this view traced, the everyday view raster
            shot := Segment{
                text: "Product shot" independent: true
                padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
                draw_bg +: {color_active: fab.color_button_active}
                draw_text +: {color_active: fab.color_text_on_accent}
            }
        }
        body := View{
            width: Fill height: Fill flow: Overlay
            device_viewport := FabViewport{selects_parts: true}
            corners := View{
                new_batch: true
                width: Fill height: Fit flow: Right
                padding: 8
                // the instant shows only in the recorder
                readout := View{width: Fill height: Fit}
                right := View{
                    width: Fit height: Fit flow: Down spacing: 4
                    align: Align{x: 1.0}
                    // the shot's progress while it traces
                    shot_row := mod.widgets.FabFloat{
                        visible: false
                        padding: Inset{left: 8 right: 8 top: 4 bottom: 4}
                        shot_progress := mod.widgets.FabLabel{text: ""}
                    }
                    // the build instructions: the plan's steps one at a time
                    steps := mod.widgets.FabFloat{
                        flow: Right spacing: 2
                        padding: Inset{left: 2 right: 2 top: 2 bottom: 2}
                        align: Align{y: 0.5}
                        prev := ViewButton{text: "Back"}
                        step_label := mod.widgets.FabLabel{text: "" padding: Inset{left: 6 right: 6}}
                        next := ViewButton{text: "Next"}
                    }
                }
            }
        }
    }
}

#[derive(Default)]
enum Framing {
    #[default]
    Pending,
    Restoring(Camera),
    /// The framing set, its view size, and the device position it framed (`DeviceView::position`).
    WaitingForCase(Camera, DVec2, f64),
    Framed(Camera, DVec2, f64),
    Manual,
}

#[derive(Script, Widget)]
pub struct FabDevice3d {
    #[deref]
    view: View,
    #[rust]
    lease: Option<PaneLease>,
    #[rust]
    framing: Framing,
    #[rust]
    /// Kit, Case, the build step, the step names (address, count), and the pane's mode and trace
    /// progress last shown.
    shown_controls: Option<(bool, bool, Option<usize>, usize, usize, PaneMode, TraceProgress)>,
    /// The side the framing looks from: a preset, or the editor's own three-quarter view.
    #[rust]
    look: Option<PresetView>,
}

impl ScriptHook for FabDevice3d {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if !apply.is_eval() {
            self.shown_controls = None;
        }
    }
}

impl FabDevice3d {
    fn pane(&mut self, cx: &Cx, state: &mut AppState) -> Option<PaneId> {
        if let Some(lease) = &self.lease {
            return Some(lease.id());
        }
        let child = self.view.widget(cx, ids!(device_viewport));
        let mut viewport = child.borrow_mut::<FabViewport>()?;
        let lease = state.lease_pane(PaneRole::Device);
        let pane = lease.id();
        viewport.set_pane(&lease);
        state.set_pane_poses(pane, PanePoses::Set(PoseSetId::DEVICE));
        state.device.pane = Some(pane);
        self.lease = Some(lease);
        Some(pane)
    }

    pub fn park(&mut self, cx: &mut Cx) {
        if let Some(lease) = &self.lease {
            // a product shot ends with its view: shown again, the view is raster
            cx.action(ShellAction::SetPaneMode { pane: lease.id(), mode: PaneMode::Raster });
            if let Some(mut viewport) = self.view.widget(cx, ids!(device_viewport)).borrow_mut::<FabViewport>() {
                viewport.park_widget(cx);
            }
            lease.park();
        }
    }

    fn set_framing(&mut self, state: &mut AppState, framing: Framing) {
        let placed = matches!(framing, Framing::Manual);
        if state.device.placed != placed {
            state.device.placed = placed;
        }
        // A switch or quit before usable bounds arrive still saves the requested camera.
        state.device.restore = match framing {
            Framing::Restoring(camera) => Some(DeviceRestore::Camera(camera)),
            _ => None,
        };
        self.framing = framing;
    }

    fn frame(&mut self, state: &mut AppState, pane: PaneId, size: DVec2) {
        if size.x <= 1.0 || size.y <= 1.0 { return; }
        match self.framing {
            Framing::Manual => return,
            Framing::WaitingForCase(ref camera, framed_size, at) | Framing::Framed(ref camera, framed_size, at) => {
                if !same_placement(state.view.camera(pane), camera) {
                    self.set_framing(state, Framing::Manual);
                    return;
                }
                let resized = (size.x - framed_size.x).abs() > 1.0 || (size.y - framed_size.y).abs() > 1.0;
                let case_arrived = matches!(self.framing, Framing::WaitingForCase(..)) && !state.device.case.is_empty();
                // a framing nobody moved follows the device between Kit and Assembled, so each
                // ends framed whole (the kit's laid-out board too)
                let moved = state.device.position != at;
                if !resized && !case_arrived && !moved { return; }
            }
            Framing::Pending | Framing::Restoring(_) => {}
        }
        let has_case = !state.device.case.is_empty();
        let bounds = if has_case { device_bounds(state, pane) } else { state.frame_all_bounds() };
        if aabb_is_empty(&bounds) { return; }
        let aspect = (size.x / size.y) as f32;
        if let Framing::Restoring(camera) = self.framing {
            if camera.sees(bounds, aspect) {
                *state.view.camera_mut(pane) = camera;
                self.set_framing(state, Framing::Manual);
                return;
            }
        }
        let (sin_el, cos_el) = 35.0_f32.to_radians().sin_cos();
        let (sin_az, cos_az) = 30.0_f32.to_radians().sin_cos();
        let camera = state.view.camera_mut(pane);
        *camera = Camera::default();
        match self.look {
            None => camera.eye = camera.target + vec3(-sin_az * cos_el, -cos_az * cos_el, sin_el),
            Some(preset) => {
                let (along, up) = preset.look_dir_and_up();
                camera.up = up;
                camera.eye = camera.target - along;
            }
        }
        camera.frame_bounds(&bounds, aspect);
        fit_projected_box(camera, &bounds, aspect);
        let at = state.device.position;
        let framing = if has_case { Framing::Framed(*camera, size, at) } else { Framing::WaitingForCase(*camera, size, at) };
        self.set_framing(state, framing);
    }

    fn sync_controls(&mut self, cx: &mut Cx, state: &AppState, pane: PaneId) {
        let device = &state.device;
        let (kit, step, n) = (device.kit, device.step, device.steps.len());
        let case = !state.view.pane(pane).case_hidden;
        let (mode, trace) = (state.view.pane(pane).mode, state.view.pane(pane).trace);
        let shown = (kit, case, step, device.steps.as_ptr() as usize, n, mode, trace);
        if self.shown_controls != Some(shown) {
            self.shown_controls = Some(shown);
            let traced = mode == PaneMode::Traced;
            for (id, active) in [(ids!(assembled), step.is_none() && !kit), (ids!(kit), step.is_none() && kit), (ids!(case), case), (ids!(shot), traced)] {
                self.view.radio_button(cx, id).set_active(cx, active, Animate::No);
            }
            if let Some(mut viewport) = self.view.widget(cx, ids!(device_viewport)).borrow_mut::<FabViewport>() {
                viewport.set_pane_mode(cx, mode);
            }
            self.view.widget(cx, ids!(shot_row)).set_visible(cx, traced);
            if traced {
                let mut text = if trace.samples >= trace.target {
                    format!("Product shot · {} samples", trace.samples)
                } else {
                    format!("Tracing · {} of {} samples", trace.samples, trace.target)
                };
                for (on, note) in [(trace.stale, "stale"), (trace.fields_not_traced, "fields not traced"), (trace.bodies_not_traced, "bodies not traced"), (trace.unavailable, "unavailable")] {
                    if on {
                        text.push_str(" · ");
                        text.push_str(note);
                    }
                }
                self.view.label(cx, ids!(shot_progress)).set_text(cx, &text);
            }
            self.view.widget(cx, ids!(steps)).set_visible(cx, n > 0);
            let text = match step {
                Some(k) => {
                    let k = k.min(n.saturating_sub(1));
                    format!("Step {} of {n} · {}", k + 1, device.steps.get(k).map_or("", String::as_str))
                }
                None => format!("{n} steps"),
            };
            self.view.label(cx, ids!(step_label)).set_text(cx, &text);
            self.view.widget(cx, ids!(device_viewport)).redraw(cx);
        }
    }
}

fn same_placement(current: &Camera, framed: &Camera) -> bool {
    if current.target != framed.target || current.up != framed.up || current.fov_y_deg != framed.fov_y_deg
        || current.ortho != framed.ortho || current.ortho_height != framed.ortho_height {
        return false;
    }
    // Fitting orthographic clip planes can slide the eye along the view line.
    if current.eye == framed.eye { return true; }
    let delta = current.forward() - framed.forward();
    delta.x.abs() <= 1e-5 && delta.y.abs() <= 1e-5 && delta.z.abs() <= 1e-5
        && (current.ortho || (current.distance() - framed.distance()).abs() <= 1e-6 * framed.distance())
}

fn fit_projected_box(camera: &mut Camera, bounds: &Aabb, aspect: f32) {
    let corners = box_corners(bounds);
    let forward = camera.forward();
    let mut distance = camera.distance();
    let mut min_distance = 0.0_f32;
    'fit: for step in 0..5 {
        let mut projected = aabb_empty();
        for corner in corners {
            let Some(ndc) = camera.project(corner, aspect) else { break 'fit };
            projected = aabb_union_point(&projected, ndc);
            if step == 0 {
                // The target stays fixed. Keep even an off-centre projected box inside the viewport.
                let depth = (corner - camera.eye).dot(forward);
                let edge_depth = depth * ndc.x.abs().max(ndc.y.abs());
                min_distance = min_distance.max(distance - depth + edge_depth.max(2.0 * camera.near));
            }
        }
        // NDC runs from -1 to 1, so a span of 1.6 fills 80% of the area.
        let span = projected.max - projected.min;
        let scale = span.x.max(span.y) / 1.6;
        if scale <= 0.0 { break; }
        let next = (distance * scale).max(min_distance);
        if (next - distance).abs() < 1e-4 * distance { break; }
        distance = next;
        camera.eye = camera.target - forward * distance;
        camera.fit_clip_planes(bounds, aspect);
    }
    camera.ortho_height = 2.0 * distance * (camera.fov_y_deg.to_radians() * 0.5).tan();
}

fn device_bounds(state: &AppState, pane: PaneId) -> Aabb {
    let mut bounds = aabb_empty();
    let mut include = |id: Option<ElementId>, b: Aabb| {
        if aabb_is_empty(&b) { return; }
        if let Some(pose) = id.and_then(|id| state.pose_in(pane, id)) {
            for x in [b.min.x, b.max.x] {
                for y in [b.min.y, b.max.y] {
                    for z in [b.min.z, b.max.z] {
                        bounds = aabb_union_point(&bounds, pose_apply(&pose, vec3(x, y, z)));
                    }
                }
            }
        } else {
            bounds = aabb_union(&bounds, &b);
        }
    };
    for &id in state.device.case.iter() {
        if let Some(element) = state.scene.element(id) {
            include(Some(id), element.bounds);
        }
    }
    for part in state.plate.parts.iter().filter(|part| part.is_board()) {
        include(part.element, Aabb { min: mm_to_world(part.min_mm), max: mm_to_world(part.max_mm) });
    }
    bounds
}

impl Widget for FabDevice3d {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Some(state) = scope.data.get_mut::<AppState>() {
            let Some(pane) = self.pane(cx, state) else { return };
            self.sync_controls(cx, state, pane);
            if state.view.pane(pane).mode == PaneMode::Traced {
                // a traced view held or moving draws coarse until it rests (as the stage's panes)
                let moving = self.view.widget(cx, ids!(device_viewport)).borrow::<FabViewport>().is_some_and(|viewport| viewport.trace_interacting());
                if let Some(p) = state.view.panes.get_mut(pane.0 as usize).and_then(Option::as_mut) {
                    p.interacting = p.pointer_held || moving;
                }
            }
            if let Event::Actions(actions) = event {
                if self.view.radio_button(cx, ids!(assembled)).clicked(actions) {
                    cx.action(ShellAction::SetDeviceKit(false));
                }
                if self.view.radio_button(cx, ids!(kit)).clicked(actions) {
                    cx.action(ShellAction::SetDeviceKit(true));
                }
                // Back and Next walk the steps put on: Kit (none), each step, the last (assembled)
                let n = state.device.steps.len();
                let done = state.device.step.map_or(if state.device.kit { 0 } else { n }, |k| (k + 1).min(n));
                for (id, to) in [(ids!(prev), done.saturating_sub(1)), (ids!(next), (done + 1).min(n))] {
                    if self.view.button(cx, id).clicked(actions) && n > 0 && to != done {
                        cx.action(if to == 0 { ShellAction::SetDeviceKit(true) } else { ShellAction::SetDeviceStep(Some(to - 1)) });
                    }
                }
                if self.view.radio_button(cx, ids!(shot)).clicked(actions) {
                    let mode = if state.view.pane(pane).mode == PaneMode::Traced { PaneMode::Raster } else { PaneMode::Traced };
                    cx.action(ShellAction::SetPaneMode { pane, mode });
                }
                if self.view.radio_button(cx, ids!(case)).clicked(actions) {
                    cx.action(ShellAction::SetPaneCase { pane, shown: state.view.pane(pane).case_hidden });
                }
                if self.view.button(cx, ids!(reset)).clicked(actions) {
                    self.set_framing(state, Framing::Pending);
                    self.view.redraw(cx);
                }
                for (id, look) in [(ids!(iso), None), (ids!(top), Some(PresetView::Top)), (ids!(front), Some(PresetView::Front))] {
                    if self.view.button(cx, id).clicked(actions) {
                        self.look = look;
                        self.set_framing(state, Framing::Pending);
                        self.view.redraw(cx);
                    }
                }
            }
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get_mut::<AppState>() {
            let Some(pane) = self.pane(cx, state) else { return DrawStep::done() };
            self.sync_controls(cx, state, pane);
            if let Some(restore) = state.device.restore.take() {
                let framing = match restore {
                    DeviceRestore::Frame => Framing::Pending,
                    DeviceRestore::Camera(camera) => Framing::Restoring(camera),
                };
                self.set_framing(state, framing);
            }
            if !matches!(self.framing, Framing::Manual) {
                // the view under the toolbar
                let size = cx.peek_walk_turtle(walk).size - dvec2(0.0, crate::ui::area_notch::TOOLBAR_H);
                self.frame(state, pane, size);
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
