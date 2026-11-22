//! The picked test print's box, with plate-axis moves and face handles that keep the
//! opposite face fixed. A drag previews the box until its single placement edit arrives.

use crate::api::*;
use crate::api::PaneId;
use crate::handle_style::{AXIS_X, AXIS_Y, AXIS_Z, COL_HOT as HOT, MOVE_FREE, RING};
use super::fine_drag::{readout_rect, Fine, SNAP_DEG, SNAP_MM};
use super::transform::{mm_to_world, seg_dist, swept_deg, DrawGizmoPrim};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabTestCubeBase = #(FabTestCube::register_widget(vm))
    mod.widgets.FabTestCube = set_type_default() do mod.widgets.FabTestCubeBase{
        width: Fill
        height: Fill
        draw_prim: mod.draw.DrawGizmoPrim{}
        draw_text: mod.draw.DrawText{
            color: #xffffff
            text_style: theme.font_bold{ font_size: fab.font_size_ui }
        }
        draw_tag: mod.draw.DrawGizmoPrim{ kind: 2.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Handle {
    MoveX,
    MoveY,
    MoveZ,
    MoveFree,
    Turn,
    Size(usize, f64),
}

#[derive(Clone)]
struct Drag {
    handle: Handle,
    start: TestPrintView,
    shown: TestDrag,
    start_plate: [f64; 2],
    start_z: f64,
    fine: [Fine; 3],
    pointer: DVec2,
}

struct Shapes {
    name: String,
    centre: DVec2,
    tips: [Option<DVec2>; 3],
    faces: [Option<DVec2>; 6],
    ring: Vec<DVec2>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabTestCube {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_prim: DrawGizmoPrim,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_tag: DrawGizmoPrim,
    #[rust]
    area: Area,
    #[rust(PaneId::PLATE_ZERO)]
    pub pane: PaneId,
    #[rust]
    shapes: Option<Shapes>,
    #[rust]
    hover: Option<Handle>,
    #[rust]
    drag: Option<Drag>,
}

fn moved(center: [f64; 3], delta: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| center[k] + delta[k])
}

fn normal(turn: [f64; 3], axis: usize, sign: f64) -> [f64; 3] {
    crate::model::state::turn_matrix(turn).map(|row| sign * row[axis])
}

fn face_dragged(center: [f64; 3], mut size: [f64; 3], turn: [f64; 3], axis: usize, sign: f64, along: f64) -> ([f64; 3], [f64; 3]) {
    let side = (size[axis] + along).max(1.0);
    let delta = side - size[axis];
    size[axis] = side;
    (moved(center, normal(turn, axis, sign).map(|n| n * delta * 0.5)), size)
}

fn face_center(center: [f64; 3], size: [f64; 3], turn: [f64; 3], axis: usize, sign: f64) -> [f64; 3] {
    moved(center, normal(turn, axis, sign).map(|n| n * size[axis] * 0.5))
}

/// The face's position along its outward normal, measured from the plate origin.
fn face_position(center: [f64; 3], size: [f64; 3], turn: [f64; 3], axis: usize, sign: f64) -> f64 {
    let n = normal(turn, axis, sign);
    (0..3).map(|k| center[k] * n[k]).sum::<f64>() + size[axis] * 0.5
}

fn face_handle(k: usize) -> Handle {
    Handle::Size(k / 2, if k % 2 == 0 { 1.0 } else { -1.0 })
}

impl FabTestCube {
    fn wanted(state: &AppState) -> Option<&TestPrintView> {
        if !state.ui.in_mode(PlateMode::Design) || state.slice_blocks_edits() || state.tool != Tool::Select {
            return None;
        }
        crate::ui::test_prints::picked(state)
    }

    fn shown(state: &AppState, cube: &TestPrintView) -> TestPrintView {
        let mut cube = cube.clone();
        if let Some(d) = state.ui.test_drag.as_ref().filter(|d| d.name == cube.name) {
            cube.center = d.center;
            cube.size = d.size;
            cube.turn = d.turn;
            cube.pose = d.pose;
        }
        cube
    }

    fn projector(&self, cx: &Cx, state: &AppState) -> ViewProjector {
        ViewProjector::new(*state.view.camera(self.pane), self.area.rect(cx))
    }

    /// The plate point (mm) under a screen point, on the plane at `z_mm`.
    fn plate_at(&self, cx: &Cx, state: &AppState, screen: DVec2, z_mm: f64) -> Option<[f64; 2]> {
        let ray = self.projector(cx, state).ray(screen);
        let z = z_mm as f32 * 0.001;
        if ray.dir.z.abs() < 1e-6 {
            return None;
        }
        let t = (z - ray.origin.z) / ray.dir.z;
        if t <= 0.0 {
            return None;
        }
        let p = ray.origin + ray.dir * t;
        Some([p.x as f64 * 1000.0, p.y as f64 * 1000.0])
    }

    /// Where along the line through `c` along the unit `n` (mm) the pointer's ray passes closest.
    fn along_line(&self, cx: &Cx, state: &AppState, screen: DVec2, c: [f64; 3], n: [f64; 3]) -> Option<f64> {
        let ray = self.projector(cx, state).ray(screen);
        let o = [ray.origin.x as f64 * 1000.0, ray.origin.y as f64 * 1000.0, ray.origin.z as f64 * 1000.0];
        let d = [ray.dir.x as f64, ray.dir.y as f64, ray.dir.z as f64];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let w = [o[0] - c[0], o[1] - c[1], o[2] - c[2]];
        let (dd, nd) = (dot(d, d), dot(n, d));
        let denom = dd - nd * nd;
        if denom < 1e-9 {
            return None;
        }
        Some((dd * dot(w, n) - nd * dot(w, d)) / denom)
    }

    fn line(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, width: f64, color: [f32; 4]) {
        let pad = width + 2.0;
        let lo = dvec2(a.x.min(b.x) - pad, a.y.min(b.y) - pad);
        let hi = dvec2(a.x.max(b.x) + pad, a.y.max(b.y) + pad);
        self.draw_prim.kind = 0.0;
        self.draw_prim.width = width as f32;
        self.draw_prim.a = vec2((a.x - lo.x) as f32, (a.y - lo.y) as f32);
        self.draw_prim.b = vec2((b.x - lo.x) as f32, (b.y - lo.y) as f32);
        self.draw_prim.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_prim.draw_abs(cx, Rect { pos: lo, size: hi - lo });
    }

    fn dot(&mut self, cx: &mut Cx2d, c: DVec2, r: f64, color: [f32; 4], square: bool) {
        self.draw_prim.kind = if square { 2.0 } else { 1.0 };
        self.draw_prim.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_prim.draw_abs(cx, Rect { pos: c - dvec2(r, r), size: dvec2(r * 2.0, r * 2.0) });
    }

    fn arrow(&mut self, cx: &mut Cx2d, from: DVec2, to: DVec2, color: [f32; 4]) {
        self.line(cx, from, to, 2.5, color);
        let d = to - from;
        let len = d.length().max(1e-6);
        let u = d / len;
        let n = dvec2(-u.y, u.x);
        let back = to - u * 11.0;
        self.line(cx, to, back + n * 5.5, 2.5, color);
        self.line(cx, to, back - n * 5.5, 2.5, color);
    }

    /// A drag's value by the pointer ([`readout_rect`]).
    fn readout(&mut self, cx: &mut Cx2d, pointer: DVec2, text: &str) {
        let Some(run) = self.draw_text.prepare_single_line_run(cx, text) else { return };
        let tw = run.width_in_lpxs as f64;
        self.draw_tag.color = vec4(0.05, 0.06, 0.07, 0.85);
        self.draw_tag.kind = 2.0;
        let r = readout_rect(pointer, dvec2(tw + 14.0, 22.0), self.area.rect(cx));
        self.draw_tag.draw_abs(cx, r);
        self.draw_text.draw_abs(cx, r.pos + dvec2(7.0, 5.0), text);
    }

    fn colour(&self, h: Handle, base: [f32; 4]) -> [f32; 4] {
        if self.hover == Some(h) || self.drag.as_ref().is_some_and(|d| d.handle == h) {
            HOT
        } else {
            base
        }
    }

    fn build_shapes(&self, cx: &Cx, state: &AppState, cube: &TestPrintView) -> Option<Shapes> {
        let proj = self.projector(cx, state);
        let c = cube.center;
        let centre = proj.project(mm_to_world(c))?;
        let ppm = proj.points_per_meter_at(mm_to_world(c));
        if ppm <= 0.0 { return None; }
        let corners = cube.corners();
        // At least 90 points, and beyond the box with room for the arrowhead.
        let tips = std::array::from_fn(|axis| {
            let extent = corners.iter().map(|p| (p[axis] - c[axis]).abs()).fold(0.0, f64::max);
            let mut p = c;
            p[axis] += (90.0 / ppm * 1000.0).max(extent + 16.0 / ppm * 1000.0);
            proj.project(mm_to_world(p))
        });
        let faces = std::array::from_fn(|k| {
            let Handle::Size(axis, sign) = face_handle(k) else { unreachable!() };
            proj.project(mm_to_world(face_center(c, cube.size, cube.turn, axis, sign)))
        });
        let r = 0.6 * cube.size[0].max(cube.size[1]);
        let foot = corners.iter().map(|p| p[2]).fold(f64::MAX, f64::min);
        let ring = (0..=64).map(|i| {
            let a = i as f64 / 64.0 * std::f64::consts::TAU;
            proj.project(mm_to_world([c[0] + r * a.cos(), c[1] + r * a.sin(), foot]))
        }).collect::<Option<Vec<_>>>()?;
        Some(Shapes { name: cube.name.clone(), centre, tips, faces, ring })
    }

    fn handle_at(&self, state: &AppState, p: DVec2) -> Option<Handle> {
        let cube = Self::wanted(state)?;
        let s = self.shapes.as_ref().filter(|s| s.name == cube.name)?;
        for (k, at) in s.faces.iter().enumerate() {
            if at.is_some_and(|at| (p - at).length() < 9.0) { return Some(face_handle(k)); }
        }
        for (at, h) in s.tips.iter().zip([Handle::MoveX, Handle::MoveY, Handle::MoveZ]) {
            if at.is_some_and(|at| (p - at).length() < 10.0) { return Some(h); }
        }
        if (p - s.centre).length() < 11.0 { return Some(Handle::MoveFree); }
        s.ring.windows(2).any(|w| seg_dist(p, w[0], w[1]) < 7.0).then_some(Handle::Turn)
    }

    pub(crate) fn handle_viewport_input(&mut self, cx: &mut Cx, input: &ViewportInput, state: &AppState) {
        if self.drag.is_some() { return; }
        let hover = match input.kind {
            ViewportInputKind::PointerMove { pos, buttons: 0, .. } => self.handle_at(state, pos),
            ViewportInputKind::HoverOut => None,
            _ => return,
        };
        if hover != self.hover {
            self.hover = hover;
            self.draw_prim.redraw(cx);
        }
        if hover.is_some() { cx.set_cursor(MouseCursor::Hand); }
    }
}

impl Widget for FabTestCube {
    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let Some(s) = &self.shapes else { return };
        let mut push = |key: u64, name: &str, at: DVec2, r: f64| {
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                bounds: makepad_platform::a11y::semantic_rect(Rect { pos: at - dvec2(r, r), size: dvec2(r * 2.0, r * 2.0) }),
                ..Default::default()
            };
            makepad_platform::a11y::set_string(&mut node.name, name);
            out.push((key, None, node));
        };
        push(1, "Test cube: move freely", s.centre, 8.0);
        for (k, name) in ["Test cube: move along X", "Test cube: move along Y", "Test cube: move along Z"].into_iter().enumerate() {
            if let Some(p) = s.tips[k] { push(2 + k as u64, name, p, 6.0); }
        }
        if let Some(p) = s.ring.first() { push(5, "Test cube: turn", *p, 6.0); }
        for (k, name) in ["Test cube: size +X", "Test cube: size -X", "Test cube: size +Y", "Test cube: size -Y", "Test cube: size +Z", "Test cube: size -Z"].into_iter().enumerate() {
            if let Some(p) = s.faces[k] { push(6 + k as u64, name, p, 6.0); }
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        let target = Self::wanted(state);
        let invalid = self.drag.as_ref().is_some_and(|d| target.is_none_or(|t| t.name != d.start.name));
        if invalid || matches!(event, Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            if self.drag.take().is_some() {
                state.ui.test_drag = None;
                crate::ui::redraw(cx, &[ids!(viewport), ids!(test_cube), ids!(tests)]);
            }
            self.hover = None;
            return;
        }
        if target.is_none() {
            self.hover = None;
            return;
        }
        // Hover stays the view's; only a drag of ours follows the pointer.
        if matches!(event, Event::MouseMove(_)) && self.drag.is_none() { return; }
        // Other buttons and Cmd/Ctrl's dolly pass through to the view, and with Lay flat on a press
        // is the lay's, on the piece under the handles.
        if let Event::MouseDown(fe) = event {
            if !fe.button.is_primary() || fe.modifiers.control || fe.modifiers.logo || state.ui.lay_flat || self.handle_at(state, fe.abs).is_none() { return; }
        }
        if let Event::TouchUpdate(e) = event {
            if self.drag.is_none() && (state.ui.lay_flat || !e.touches.iter().any(|t| self.handle_at(state, t.abs).is_some())) { return; }
        }
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                let Some(handle) = self.handle_at(state, fe.abs) else { return };
                let Some(cube) = Self::wanted(state) else { return };
                let start = Self::shown(state, cube);
                let c = start.center;
                let foot = start.corners().iter().map(|p| p[2]).fold(f64::MAX, f64::min);
                let (start_plate, start_z) = match handle {
                    Handle::MoveZ => {
                        let Some(z) = self.along_line(cx, state, fe.abs, c, [0.0, 0.0, 1.0]) else { return };
                        ([c[0], c[1]], z)
                    }
                    Handle::Size(axis, sign) => {
                        let Some(t) = self.along_line(cx, state, fe.abs, c, normal(start.turn, axis, sign)) else { return };
                        ([c[0], c[1]], t)
                    }
                    _ => {
                        let z = if handle == Handle::Turn { foot } else { c[2] };
                        let Some(p) = self.plate_at(cx, state, fe.abs, z) else { return };
                        (p, z)
                    }
                };
                let fine = match handle {
                    Handle::Turn => [Fine::new(start.turn[2]); 3],
                    Handle::Size(axis, sign) => [Fine::new(face_position(c, start.size, start.turn, axis, sign)); 3],
                    _ => c.map(Fine::new),
                };
                let shown = TestDrag { name: start.name.clone(), center: c, size: start.size, turn: start.turn, pose: start.pose };
                self.drag = Some(Drag { handle, start, shown, start_plate, start_z, fine, pointer: fe.abs });
            }
            Hit::FingerMove(fe) => {
                let Some(mut d) = self.drag.take() else { return };
                d.pointer = fe.abs;
                let c = d.start.center;
                match d.handle {
                    Handle::MoveX | Handle::MoveY | Handle::MoveFree => {
                        if let Some(p) = self.plate_at(cx, state, fe.abs, c[2]) {
                            let mut delta = [0.0; 3];
                            for k in 0..2 {
                                if (k == 0 && d.handle == Handle::MoveY) || (k == 1 && d.handle == Handle::MoveX) { continue; }
                                delta[k] = d.fine[k].value(c[k] + p[k] - d.start_plate[k], &fe.modifiers, SNAP_MM) - c[k];
                            }
                            d.shown.center = moved(c, delta);
                        }
                    }
                    Handle::MoveZ => {
                        if let Some(z) = self.along_line(cx, state, fe.abs, c, [0.0, 0.0, 1.0]) {
                            d.shown.center[2] = d.fine[2].value(c[2] + z - d.start_z, &fe.modifiers, SNAP_MM);
                        }
                    }
                    Handle::Turn => {
                        if let Some(p) = self.plate_at(cx, state, fe.abs, d.start_z) {
                            // the turn about the plate's Z is the last of its three
                            let turn = swept_deg(d.start_plate, p, [c[0], c[1]]);
                            d.shown.turn[2] = d.fine[0].value(d.start.turn[2] + turn, &fe.modifiers, SNAP_DEG);
                        }
                    }
                    Handle::Size(axis, sign) => {
                        let n = normal(d.start.turn, axis, sign);
                        if let Some(along) = self.along_line(cx, state, fe.abs, c, n).map(|t| t - d.start_z) {
                            let pos = face_position(c, d.start.size, d.start.turn, axis, sign);
                            let along = d.fine[0].value(pos + along, &fe.modifiers, SNAP_MM) - pos;
                            (d.shown.center, d.shown.size) = face_dragged(c, d.start.size, d.start.turn, axis, sign, along);
                        }
                    }
                }
                state.ui.test_drag = Some(d.shown.clone());
                self.drag = Some(d);
                crate::ui::redraw(cx, &[ids!(viewport), ids!(test_cube), ids!(tests)]);
            }
            Hit::FingerUp(fe) => {
                let Some(d) = self.drag.take() else { return };
                let p = d.shown;
                let changed = p.center != d.start.center || p.size != d.start.size || p.turn != d.start.turn;
                if fe.cancelled || !changed {
                    state.ui.test_drag = None;
                } else {
                    state.ui.test_drag = Some(p.clone());
                    cx.action(TestPrintAction::Place { name: p.name, center: p.center, size: p.size, turn: p.turn, pose: p.pose });
                }
                crate::ui::redraw(cx, &[ids!(viewport), ids!(test_cube), ids!(tests)]);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        self.shapes = None;
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let Some(cube) = Self::wanted(state) else { return DrawStep::done() };
        let cube = Self::shown(state, cube);
        let Some(s) = self.build_shapes(cx, state, &cube) else { return DrawStep::done() };
        let proj = self.projector(cx, state);
        let corners = cube.corners();
        for (i, j) in [(0, 1), (1, 2), (2, 3), (3, 0), (4, 5), (5, 6), (6, 7), (7, 4), (0, 4), (1, 5), (2, 6), (3, 7)] {
            if let (Some(a), Some(b)) = (proj.project(mm_to_world(corners[i])), proj.project(mm_to_world(corners[j]))) {
                self.line(cx, a, b, 1.5, HOT);
            }
        }
        let col = self.colour(Handle::Turn, RING);
        for w in s.ring.windows(2) { self.line(cx, w[0], w[1], 2.5, col); }
        for (k, h) in [Handle::MoveX, Handle::MoveY, Handle::MoveZ].into_iter().enumerate() {
            if let Some(t) = s.tips[k] {
                let col = self.colour(h, [AXIS_X, AXIS_Y, AXIS_Z][k]);
                self.arrow(cx, s.centre, t, col);
            }
        }
        let col = self.colour(Handle::MoveFree, MOVE_FREE);
        self.dot(cx, s.centre, 7.0, col, true);
        for (k, p) in s.faces.iter().enumerate() {
            if let Some(p) = p {
                let col = self.colour(face_handle(k), [AXIS_X, AXIS_Y, AXIS_Z][k / 2]);
                self.dot(cx, *p, 5.5, col, true);
            }
        }
        self.shapes = Some(s);
        if let Some(d) = &self.drag {
            let text = match d.handle {
                Handle::Turn => format!("{:.1} °", cube.turn[2]),
                Handle::Size(_, _) => format!("{:.2} x {:.2} x {:.2} mm", cube.size[0], cube.size[1], cube.size[2]),
                _ => format!("X {:.2}  Y {:.2}  Z {:.2} mm", cube.center[0], cube.center[1], cube.center[2]),
            };
            let pointer = d.pointer;
            self.readout(cx, pointer, &text);
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_changes_each_coordinate_by_its_delta() {
        assert_eq!(moved([1.0, 2.0, 3.0], [-2.0, 0.5, 4.0]), [-1.0, 2.5, 7.0]);
    }

    #[test]
    fn face_keeps_the_opposite_face_at_zero_and_thirty_degrees_and_at_minimum() {
        let center = [3.7, -2.3, 12.5];
        let size = [10.0, 20.0, 25.0];
        for turn in [[0.0; 3], [0.0, 0.0, 30.0], [20.0, -15.0, 30.0]] {
            for axis in 0..3 {
                for sign in [-1.0, 1.0] {
                    let opposite = face_center(center, size, turn, axis, -sign);
                    let face = face_center(center, size, turn, axis, sign);
                    for along in [4.25, -2.5, -100.0] {
                        let (c, s) = face_dragged(center, size, turn, axis, sign, along);
                        let back = face_center(c, s, turn, axis, -sign);
                        let front = face_center(c, s, turn, axis, sign);
                        let delta = along.max(1.0 - size[axis]);
                        let expected = moved(face, normal(turn, axis, sign).map(|n| n * delta));
                        for k in 0..3 {
                            assert!((back[k] - opposite[k]).abs() < 1e-12, "opposite face moved: turn {turn:?}, axis {axis}, sign {sign}, along {along}");
                            assert!((front[k] - expected[k]).abs() < 1e-12, "dragged face did not follow its normal");
                            assert_eq!(s[k], if k == axis { (size[k] + along).max(1.0) } else { size[k] }, "only the dragged side changes, with a 1 mm minimum");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fine_snap_lands_the_face_on_a_whole_millimetre() {
        let center = [3.7, -2.3, 12.5];
        let size = [10.3, 20.7, 25.9];
        for turn in [[0.0; 3], [0.0, 0.0, 30.0], [20.0, -15.0, 30.0]] {
            for axis in 0..3 {
                for sign in [-1.0, 1.0] {
                    let pos = face_position(center, size, turn, axis, sign);
                    let mut fine = Fine::new(pos);
                    let snapped = fine.value_with(pos + 2.37, false, true, SNAP_MM);
                    let (c, s) = face_dragged(center, size, turn, axis, sign, snapped - pos);
                    let actual = face_position(c, s, turn, axis, sign);
                    assert!((actual - actual.round()).abs() < 1e-12, "face position must snap, not its change or the side length");
                    assert!((actual - snapped).abs() < 1e-12, "face must reach Fine's snapped position");
                }
            }
        }
    }
}
