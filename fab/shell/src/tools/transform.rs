//! The part gizmo: move, turn and scale the selected part right on the plate, or the feature
//! of it in focus (a row of the Features tree, a feature picked in the view).
//!
//! Drawn in the 2D pass over the view from the part's box projected through
//! the view camera, at the part's origin (what its `at` places, what it turns and scales
//! about: where the Part panel's Position is). Move has an X and a Y arrow and a centre
//! square (free on the plate); Turn is a ring on the plate around it; Scale is a corner
//! handle (the whole part, evenly) and a handle along each of the part's own axes (that axis
//! alone: its `place` scale by axis). A press on a handle captures the pointer (the view never
//! orbits from it); anywhere else the press goes on to the view.
//!
//! A drag is continuous; Shift goes at a tenth of the speed and Cmd (Ctrl) snaps the value to
//! whole millimetres, 5° or 5 % ([`super::fine_drag`]); the value shows by the pointer.
//!
//! While a move, a turn or a scale drags, the part shows at its new pose through
//! the placement preview (`SceneState::nudge`: the element table's pose and the
//! field's transform, no geometry rebuilt; a field part scaled by axis is drawn by its
//! field stretched). The gizmo is drawn at that same pose, so it stays on the part
//! through the drag and after it, until the committed plate has the part
//! there. Letting go commits one `PartAction`, which the app makes one
//! undoable model transaction.
//!
//! A feature gets the same gizmo at its own origin (what its `at` places, what it turns and
//! scales about), with a Z arrow too: a feature is not dropped onto the bed. Its drag is its
//! placement in its script ([`ParamAction::Place`]): previewed live like a dragged value (a
//! distance-field part re-bakes only what the change reaches), one undo step when let go. Its
//! box shows at the dragged pose meanwhile. A cutter's ghost is a grip too, in every mode: a
//! press on it moves the cutter freely on the plate.

use crate::api::*;
use crate::api::PaneId;
use crate::handle_style::{AXIS_X, AXIS_Y, AXIS_Z, COL_BOX, RING, SCALE, COL_HOT as HOT, MOVE_FREE};
use crate::params::{AreaKind, ObjectBox, ParamAction, PlaceGesture};
use crate::model::state::{part_axes, pose_apply, pose_mul, pose_scale_about, pose_scale_turn, pose_stretch_about, pose_turn_shift, Pose, POSE_IDENTITY};
use crate::ui::parts::active_part;
use super::fine_drag::{readout_rect, Fine, SNAP_DEG, SNAP_MM, SNAP_SCALE};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    /// One gizmo primitive in a quad: a segment from `a` to `b` (quad-local
    /// points) with a round cap, a disc (`kind` 1) or a square (`kind` 2).
    mod.draw.DrawGizmoPrim = mod.std.set_type_default() do #(DrawGizmoPrim::script_shader(vm)){
        ..mod.draw.DrawQuad
        a: vec2(0.0, 0.0)
        b: vec2(0.0, 0.0)
        width: 2.0
        kind: 0.0
        color: vec4(1.0, 1.0, 1.0, 1.0)
        pixel: fn() {
            let p = self.pos * self.rect_size
            if self.kind > 1.5 {
                let sdf = Sdf2d.viewport(p)
                sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, 1.5)
                sdf.fill_keep(vec4(self.color.xyz, self.color.w * 0.35))
                sdf.stroke(self.color, 1.5)
                return sdf.result
            }
            if self.kind > 0.5 {
                let sdf = Sdf2d.viewport(p)
                let c = self.rect_size * 0.5
                sdf.circle(c.x, c.y, min(c.x, c.y) - 1.0)
                sdf.fill_keep(self.color)
                sdf.stroke(vec4(0.0, 0.0, 0.0, 0.55), 1.0)
                return sdf.result
            }
            let pa = p - self.a
            let ba = self.b - self.a
            let h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.0001), 0.0, 1.0)
            let d = length(pa - ba * h) - self.width * 0.5
            let aa = 0.75
            let alpha = clamp(0.5 - d / aa, 0.0, 1.0)
            return vec4(self.color.xyz * self.color.w * alpha, self.color.w * alpha)
        }
    }

    mod.widgets.FabPartGizmoBase = #(FabPartGizmo::register_widget(vm))
    mod.widgets.FabPartGizmo = set_type_default() do mod.widgets.FabPartGizmoBase{
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

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawGizmoPrim {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub a: Vec2f,
    #[live]
    pub b: Vec2f,
    #[live]
    pub width: f32,
    #[live]
    pub kind: f32,
    #[live]
    pub color: Vec4f,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Handle {
    MoveX,
    MoveY,
    /// A feature's height (a part sits on the bed).
    MoveZ,
    MoveFree,
    Turn,
    /// The whole part, evenly.
    Scale,
    /// Along the part's own axis 0 (X), 1 (Y) or 2 (Z) alone.
    ScaleAxis(u8),
    /// A cutter's ghost: a free move on the plate.
    Ghost,
}

#[derive(Clone, Debug)]
struct Drag {
    handle: Handle,
    part: usize,
    /// The plate part's object.
    object: String,
    /// The feature dragged (its box when the drag began); none: the part.
    feature: Option<ObjectBox>,
    /// Plate point (mm) under the press, and the screen point.
    start_plate: [f64; 2],
    start_screen: DVec2,
    /// A feature's Z drag: the height under the press along the vertical through its origin.
    start_z: f64,
    /// The placement preview the drag starts from (a committed move the plate has not caught
    /// up with yet, else identity); the drag's pose is its own on top of it.
    base: Pose,
    /// What it turns and scales about (mm): its origin (a part's on the plate at its bottom).
    pivot: [f64; 3],
    rotate: f64,
    scale: [f64; 3],
    /// The part's own axes (columns, in the plate's frame): what a scale by axis goes along.
    axes: [[f32; 3]; 3],
    /// The gizmo's centre on screen at the press (it scales about it), and a scale handle's
    /// direction from it there (unit).
    centre: DVec2,
    along: DVec2,
    /// What the drag has done so far: its move, its turn (degrees), its factor along each of the
    /// part's axes.
    offset: [f64; 3],
    turn: f64,
    by: [f64; 3],
    /// The values the pointer sets, fine and snapped ([`Fine`]): X, Y, Z of a move, the turn, a
    /// scale by axis (or the whole part's, in 0).
    fine: [Fine; 3],
    /// Where the pointer is: the value shows by it.
    pointer: DVec2,
    /// A feature's gesture shown so far (none: nothing previewed yet).
    sent: Option<PlaceGesture>,
}

/// The screen shapes of the gizmo for the current frame.
#[derive(Default, Clone)]
struct Shapes {
    centre: DVec2,
    x_tip: Option<DVec2>,
    y_tip: Option<DVec2>,
    z_tip: Option<DVec2>,
    ring: Vec<DVec2>,
    scale: Option<DVec2>,
    /// The scale handles along the part's own X, Y, Z (a part's only).
    scale_axes: [Option<DVec2>; 3],
}

/// What the gizmo works on: the active part, or the feature of it in focus.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    part: usize,
    feature: Option<ObjectBox>,
}

/// A feature's gesture is in its script and its rebuild on the way: the gizmo stays at the
/// pose let go until the plate has the feature there (or the edit is refused).
#[derive(Clone, Debug)]
struct Settling {
    feature: ObjectBox,
    gesture: PlaceGesture,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabPartGizmo {
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
    #[rust]
    settling: Option<Settling>,
    /// The mode the shapes were built for.
    #[rust]
    mode: GizmoMode,
}

impl DrawGizmoPrim {
    /// A filled disc of `color` in `rect` (for other tools' markers).
    pub(crate) fn disc(&mut self, cx: &mut Cx2d, rect: Rect, color: [f32; 4]) {
        self.kind = 1.0;
        self.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_abs(cx, rect);
    }
}

/// What a part turns and scales about, where its gizmo sits: its origin, on the plate at the
/// part's bottom.
fn pivot(part: &PlatePart) -> [f64; 3] {
    [part.origin[0], part.origin[1], part.min_mm[2]]
}

pub(crate) fn mm_to_world(p: [f64; 3]) -> Vec3f {
    vec3(p[0] as f32 * 0.001, p[1] as f32 * 0.001, p[2] as f32 * 0.001)
}

/// `part` where it shows: its box and origin moved with its placement preview's pose (the
/// gizmo's frame is the drawn part's, during a drag and until the committed plate has it).
pub(crate) fn shown_part(state: &AppState, part: &PlatePart) -> PlatePart {
    let pose = state.scene_state.nudge_of(part.element);
    if pose == POSE_IDENTITY {
        return part.clone();
    }
    let moved = |p: [f64; 3]| {
        let q = pose_apply(&pose, mm_to_world(p));
        [q.x as f64 * 1000.0, q.y as f64 * 1000.0, q.z as f64 * 1000.0]
    };
    // its factors along its own axes and its turn, from the pose and the part's own turn
    let axes = part_axes(part.turn);
    let (by, turn) = pose_scale_turn(&pose, &axes);
    let mut shown = part.clone();
    // The box moves with its bottom centre and takes the scale, not the turn (it stays the
    // size it is while it turns): the box round its corners stretched about that point.
    let c = part.center();
    let anchor = [c[0], c[1], part.min_mm[2]];
    let at = moved(anchor);
    let s = pose_stretch_about(vec3(0.0, 0.0, 0.0), &axes, by);
    let (mut lo, mut hi) = (at, at);
    for corner in 0..8 {
        let d: [f64; 3] = std::array::from_fn(|k| if corner & (1 << k) == 0 { part.min_mm[k] } else { part.max_mm[k] } - anchor[k]);
        for k in 0..3 {
            let q = at[k] + (0..3).map(|j| s[k][j] as f64 * d[j]).sum::<f64>();
            lo[k] = lo[k].min(q);
            hi[k] = hi[k].max(q);
        }
    }
    (shown.min_mm, shown.max_mm) = (lo, hi);
    for k in 0..3 {
        shown.scale[k] *= by[k] as f64;
    }
    shown.origin = moved(part.origin);
    shown.rotate += turn as f64;
    shown.turn[2] += turn as f64;
    shown
}

/// `b` at a feature gesture's pose: its origin and its eight corners.
fn posed(b: &ObjectBox, g: Option<PlaceGesture>) -> ([f64; 3], [[f64; 3]; 8]) {
    let o = b.origin;
    let corner = |i: usize| -> [f64; 3] { [if i & 1 == 0 { b.min[0] } else { b.max[0] }, if i & 2 == 0 { b.min[1] } else { b.max[1] }, if i & 4 == 0 { b.min[2] } else { b.max[2] }] };
    let at = |q: [f64; 3]| -> [f64; 3] {
        match g {
            None => q,
            Some(PlaceGesture::Move(d)) => [q[0] + d[0], q[1] + d[1], q[2] + d[2]],
            Some(PlaceGesture::Turn(t)) => {
                let (s, c) = t.to_radians().sin_cos();
                let (x, y) = (q[0] - o[0], q[1] - o[1]);
                [o[0] + c * x - s * y, o[1] + s * x + c * y, q[2]]
            }
            Some(PlaceGesture::Scale(f)) => std::array::from_fn(|k| o[k] + (q[k] - o[k]) * f),
        }
    };
    (at(o), std::array::from_fn(|i| at(corner(i))))
}

/// An edit of `b`'s placement is on its way (set, its plate not in yet): the gizmo waits for
/// it, so the next gesture starts from where this one put it.
fn in_flight(state: &AppState, b: &ObjectBox) -> bool {
    state.params.pending.iter().any(|(k, _)| k.strip_prefix(b.object.as_str()).is_some_and(|r| r.starts_with('.')))
}

/// The focused feature of part `part` (not the part's own solid).
fn focused_feature(state: &AppState, part: usize) -> Option<&ObjectBox> {
    let focus = state.params.focus.as_ref()?;
    let pp = state.params.part(part)?;
    if pp.object == *focus {
        return None;
    }
    pp.object_box(focus)
}

pub(crate) fn seg_dist(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let ap = p - a;
    let t = ((ap.x * ab.x + ap.y * ab.y) / (ab.x * ab.x + ab.y * ab.y).max(1e-9)).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

impl FabPartGizmo {
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

    /// What the gizmo works on now. A feature has its handles in Move, Turn and Scale, and in
    /// Select and Edit shape (whose handles are its parameters') its cutter's ghost only.
    fn wanted(state: &AppState) -> Option<Target> {
        if crate::ui::test_prints::picked(state).is_some() { return None; }
        if state.slice_blocks_edits() || matches!(state.ui.gizmo, GizmoMode::Paint | GizmoMode::Sculpt | GizmoMode::Force) || state.tool != Tool::Select {
            return None;
        }
        let part = active_part(state)?;
        match (focused_feature(state, part), state.ui.gizmo) {
            // a print settings box has a size, not a scale: its Size rows and handles set it
            (Some(b), GizmoMode::Scale) if state.params.areas.iter().any(|a| a.kind == AreaKind::Settings && a.object == b.object) => None,
            // nor has one that follows a feature a place: it is where the feature is
            (Some(b), _) if state.params.areas.iter().any(|a| a.follows && a.object == b.object) => None,
            (Some(b), _) => Some(Target { part, feature: Some(b.clone()) }),
            (None, GizmoMode::Move | GizmoMode::Rotate | GizmoMode::Scale) => Some(Target { part, feature: None }),
            _ => None,
        }
    }

    /// The focused cutter's ghost under `p`: a grip. Not while a shape handle has the pointer.
    fn on_ghost(&self, cx: &Cx, state: &AppState, feature: &ObjectBox, p: DVec2) -> bool {
        let mine = |k: &str| k == feature.object || k.strip_prefix(feature.object.as_str()).is_some_and(|r| r.starts_with('.'));
        let Some(g) = state.params.ghost.as_ref().filter(|g| g.fade_from.is_none() && mine(&g.key)) else { return false };
        if state.params.hover.is_some() && matches!(state.ui.gizmo, GizmoMode::Off | GizmoMode::Shape) {
            return false;
        }
        let proj = self.projector(cx, state);
        let inside = |t: [DVec2; 3]| {
            let side = |a: DVec2, b: DVec2| (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
            let (d0, d1, d2) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
            (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
        };
        g.bodies.iter().any(|body| {
            let pts: Vec<Option<DVec2>> = body.positions.iter().map(|q| proj.project(mm_to_world(*q))).collect();
            body.triangles.iter().take(6000).any(|t| match (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]) {
                (Some(a), Some(b), Some(c)) => inside([a, b, c]),
                _ => false,
            })
        })
    }

    fn handle_at(&self, cx: &Cx, state: &AppState, p: DVec2) -> Option<Handle> {
        let target = Self::wanted(state)?;
        if target.feature.as_ref().is_some_and(|f| in_flight(state, f)) {
            return None;
        }
        let handle = self.shapes.as_ref().and_then(|s| match state.ui.gizmo {
            GizmoMode::Move => {
                if (p - s.centre).length() < 11.0 {
                    return Some(Handle::MoveFree);
                }
                [(s.x_tip, Handle::MoveX), (s.y_tip, Handle::MoveY), (s.z_tip, Handle::MoveZ)].into_iter().find_map(|(tip, h)| tip.filter(|t| seg_dist(p, s.centre, *t) < 7.0).map(|_| h))
            }
            GizmoMode::Rotate => s.ring.windows(2).any(|w| seg_dist(p, w[0], w[1]) < 7.0).then_some(Handle::Turn),
            GizmoMode::Scale => {
                let axis = s.scale_axes.iter().enumerate().find_map(|(k, t)| t.filter(|t| (p - *t).length() < 9.0).map(|_| Handle::ScaleAxis(k as u8)));
                axis.or_else(|| s.scale.filter(|h| (p - *h).length() < 10.0).map(|_| Handle::Scale))
            }
            GizmoMode::Off | GizmoMode::Shape | GizmoMode::Paint | GizmoMode::Sculpt | GizmoMode::Force => None,
        });
        handle.or_else(|| target.feature.as_ref().filter(|f| self.on_ghost(cx, state, f, p)).map(|_| Handle::Ghost))
    }

    /// The handles about `base` (mm) for a box of `size` with its far corner at `corner`, and
    /// the scale handles along a part's own `axes`.
    fn build_shapes(&self, cx: &Cx, state: &AppState, base: [f64; 3], size: [f64; 3], corner: [f64; 3], with_z: bool, axes: Option<&[[f32; 3]; 3]>) -> Option<Shapes> {
        let proj = self.projector(cx, state);
        let centre = proj.project(mm_to_world(base))?;
        let ppm = proj.points_per_meter_at(mm_to_world(base));
        if ppm <= 0.0 {
            return None;
        }
        // Arrows about 90 points long whatever the zoom.
        let len_mm = 90.0 / ppm * 1000.0;
        let x_tip = proj.project(mm_to_world([base[0] + len_mm, base[1], base[2]]));
        let y_tip = proj.project(mm_to_world([base[0], base[1] + len_mm, base[2]]));
        let z_tip = if with_z { proj.project(mm_to_world([base[0], base[1], base[2] + len_mm])) } else { None };
        let r = (size[0].hypot(size[1]) * 0.5 + 6.0).max(len_mm * 0.45);
        let ring = (0..=64)
            .filter_map(|i| {
                let a = i as f64 / 64.0 * std::f64::consts::TAU;
                proj.project(mm_to_world([base[0] + r * a.cos(), base[1] + r * a.sin(), base[2]]))
            })
            .collect();
        let scale = proj.project(mm_to_world(corner));
        // A little shorter than the arrows: the corner handle stays clear of them.
        let scale_axes = std::array::from_fn(|k| {
            let a = axes?;
            proj.project(mm_to_world(std::array::from_fn(|i| base[i] + a[i][k] as f64 * len_mm * 0.75)))
        });
        Some(Shapes { centre, x_tip, y_tip, z_tip, ring, scale, scale_axes })
    }

    /// Where along the vertical through `c` (mm) the pointer's ray passes closest: a height.
    fn height_at(&self, cx: &Cx, state: &AppState, screen: DVec2, c: [f64; 3]) -> Option<f64> {
        let ray = self.projector(cx, state).ray(screen);
        let o = [ray.origin.x as f64 * 1000.0, ray.origin.y as f64 * 1000.0, ray.origin.z as f64 * 1000.0];
        let d = [ray.dir.x as f64, ray.dir.y as f64, ray.dir.z as f64];
        let w = [c[0] - o[0], c[1] - o[1], c[2] - o[2]];
        let (b, dd) = (d[2], d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
        let denom = dd - b * b;
        if denom < 1e-9 {
            return None;
        }
        let e = d[0] * w[0] + d[1] * w[1] + d[2] * w[2];
        Some(c[2] + (b * e - dd * w[2]) / denom)
    }

    /// The gesture a feature drag has made so far.
    fn gesture(drag: &Drag) -> PlaceGesture {
        match drag.handle {
            Handle::MoveX | Handle::MoveY | Handle::MoveZ | Handle::MoveFree | Handle::Ghost => PlaceGesture::Move(drag.offset),
            Handle::Turn => PlaceGesture::Turn(drag.turn),
            Handle::Scale | Handle::ScaleAxis(_) => PlaceGesture::Scale(drag.by[0]),
        }
    }

    /// The pose a feature shows at: its drag's, or the one let go while its edit rebuilds.
    fn feature_pose(&self, state: &AppState, b: &ObjectBox) -> Option<PlaceGesture> {
        if let Some(d) = self.drag.as_ref().filter(|d| d.feature.as_ref().is_some_and(|f| f.object == b.object)) {
            return d.sent;
        }
        self.settling.as_ref().filter(|s| s.feature == *b && in_flight(state, b)).map(|s| s.gesture)
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
        let tw = self.draw_text.prepare_single_line_run(cx, text).map(|r| r.width_in_lpxs as f64).unwrap_or(60.0);
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

    /// A feature's box where its gesture puts it (its eight corners, plate mm).
    fn draw_corners(&mut self, cx: &mut Cx2d, state: &AppState, corners: &[[f64; 3]; 8]) {
        let proj = self.projector(cx, state);
        let col = [COL_BOX[0], COL_BOX[1], COL_BOX[2], 0.85];
        for (i, j) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
            if let (Some(a), Some(b)) = (proj.project(mm_to_world(corners[i])), proj.project(mm_to_world(corners[j]))) {
                self.line(cx, a, b, 1.25, col);
            }
        }
    }

    /// A feature's gizmo at its own origin (at the pose its drag or its rebuilding edit has).
    fn draw_feature(&mut self, cx: &mut Cx2d, state: &AppState, b: &ObjectBox) {
        let pose = self.feature_pose(state, b);
        let (origin, corners) = posed(b, pose);
        let size: [f64; 3] = std::array::from_fn(|k| b.max[k] - b.min[k]);
        let Some(shapes) = self.build_shapes(cx, state, origin, size, corners[7], true, None) else { return };
        self.shapes = Some(shapes.clone());
        self.mode = state.ui.gizmo;
        if pose.is_some() {
            self.draw_corners(cx, state, &corners);
        }
        let c = shapes.centre;
        let drag = self.drag.clone().filter(|d| d.feature.is_some());
        match state.ui.gizmo {
            GizmoMode::Move => {
                for (tip, h, base) in [(shapes.x_tip, Handle::MoveX, AXIS_X), (shapes.y_tip, Handle::MoveY, AXIS_Y), (shapes.z_tip, Handle::MoveZ, AXIS_Z)] {
                    if let Some(t) = tip {
                        let col = self.colour(h, base);
                        self.arrow(cx, c, t, col);
                    }
                }
                let col = self.colour(Handle::MoveFree, MOVE_FREE);
                self.dot(cx, c, 7.0, col, true);
            }
            GizmoMode::Rotate => {
                let col = self.colour(Handle::Turn, RING);
                for w in shapes.ring.windows(2) {
                    self.line(cx, w[0], w[1], 2.5, col);
                }
                self.dot(cx, c, 3.5, RING, false);
            }
            GizmoMode::Scale => {
                if let Some(h) = shapes.scale {
                    let col = self.colour(Handle::Scale, SCALE);
                    self.line(cx, c, h, 1.5, [col[0], col[1], col[2], 0.6]);
                    self.dot(cx, h, 7.0, col, true);
                }
            }
            GizmoMode::Off | GizmoMode::Shape | GizmoMode::Paint | GizmoMode::Sculpt | GizmoMode::Force => {}
        }
        if let Some(d) = drag {
            let text = match Self::gesture(&d) {
                PlaceGesture::Move(m) => format!("{:+.2}  {:+.2}  {:+.2} mm", m[0], m[1], m[2]),
                PlaceGesture::Turn(t) => format!("{t:+.1}°"),
                PlaceGesture::Scale(f) => format!("{:.1} %", f * 100.0),
            };
            self.readout(cx, d.pointer, &text);
        }
    }
}

impl FabPartGizmo {
    pub(crate) fn handle_viewport_input(&mut self, cx: &mut Cx, input: &ViewportInput, state: &AppState) {
        if self.drag.is_some() { return; }
        let hover = match input.kind {
            ViewportInputKind::PointerMove { pos, buttons: 0, .. } => self.handle_at(cx, state, pos),
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

impl Widget for FabPartGizmo {
    /// The handles on screen, so a test, an automation client or a screen
    /// reader finds them (they are direct-manipulation; the Part panel's
    /// fields are the keyboard path to the same edits).
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
        match self.mode {
            GizmoMode::Move => {
                push(1, "Move freely", s.centre, 8.0);
                if let Some(t) = s.x_tip {
                    push(2, "Move along X", s.centre + (t - s.centre) * 0.7, 6.0);
                }
                if let Some(t) = s.y_tip {
                    push(3, "Move along Y", s.centre + (t - s.centre) * 0.7, 6.0);
                }
                if let Some(t) = s.z_tip {
                    push(6, "Move along Z", s.centre + (t - s.centre) * 0.7, 6.0);
                }
            }
            GizmoMode::Rotate => {
                if let Some(p) = s.ring.first() {
                    push(4, "Turn", *p, 6.0);
                }
            }
            GizmoMode::Scale => {
                if let Some(h) = s.scale {
                    push(5, "Scale", h, 8.0);
                }
                for (k, name) in ["Scale along X", "Scale along Y", "Scale along Z"].into_iter().enumerate() {
                    if let Some(t) = s.scale_axes[k] {
                        push(7 + k as u64, name, t, 6.0);
                    }
                }
            }
            GizmoMode::Off | GizmoMode::Shape | GizmoMode::Paint | GizmoMode::Sculpt | GizmoMode::Force => {}
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        let target = Self::wanted(state);
        if target.is_none() && self.drag.is_none() {
            self.hover = None;
            return;
        }
        // Hover stays the view's; only a drag of ours follows the pointer.
        if matches!(event, Event::MouseMove(_)) && self.drag.is_none() {
            return;
        }
        // A plain left press on a handle is ours; any other press (the other buttons, Cmd/Ctrl's
        // dolly) goes on to the view.
        if let Event::MouseDown(fe) = event {
            if !fe.button.is_primary() || fe.modifiers.control || fe.modifiers.logo || self.handle_at(cx, state, fe.abs).is_none() {
                return;
            }
        }
        if let Event::TouchUpdate(e) = event {
            if self.drag.is_none() && !e.touches.iter().any(|t| self.handle_at(cx, state, t.abs).is_some()) { return; }
        }
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                let Some(handle) = self.handle_at(cx, state, fe.abs) else { return };
                let Some(Target { part: i, feature }) = target else { return };
                let Some(rest) = state.plate.parts.get(i) else { return };
                let centre = self.shapes.as_ref().map_or(fe.abs, |s| s.centre);
                let along = match (handle, &self.shapes) {
                    (Handle::ScaleAxis(k), Some(s)) => s.scale_axes[k as usize].map_or(dvec2(1.0, 0.0), |t| {
                        let d = t - centre;
                        d / d.length().max(1e-9)
                    }),
                    _ => dvec2(1.0, 0.0),
                };
                if let Some(b) = feature {
                    // A feature: about its own origin, from the box it has now.
                    let o = b.origin;
                    let start_z = self.height_at(cx, state, fe.abs, o).unwrap_or(o[2]);
                    self.settling = None;
                    self.drag = Some(Drag {
                        handle,
                        part: i,
                        object: rest.object.clone(),
                        start_plate: self.plate_at(cx, state, fe.abs, o[2]).unwrap_or([o[0], o[1]]),
                        start_screen: fe.abs,
                        start_z,
                        feature: Some(b),
                        base: POSE_IDENTITY,
                        pivot: o,
                        rotate: 0.0,
                        scale: [1.0; 3],
                        axes: part_axes([0.0; 3]),
                        centre,
                        along,
                        offset: [0.0; 3],
                        turn: 0.0,
                        by: [1.0; 3],
                        // each value from where it is: the origin, no turn, a factor of 1
                        fine: match handle {
                            Handle::Turn => [Fine::new(0.0); 3],
                            Handle::Scale | Handle::ScaleAxis(_) => [Fine::new(1.0); 3],
                            _ => [Fine::new(o[0]), Fine::new(o[1]), Fine::new(o[2])],
                        },
                        pointer: fe.abs,
                        sent: None,
                    });
                    return;
                }
                let base = state.scene_state.nudge_of(rest.element);
                let part = shown_part(state, rest);
                let o = pivot(&part);
                let start_plate = self.plate_at(cx, state, fe.abs, part.min_mm[2]).unwrap_or([o[0], o[1]]);
                // each value from where it is: the origin, the turn, the scale (by axis)
                let fine = match handle {
                    Handle::Turn => [Fine::new(part.rotate); 3],
                    Handle::Scale => [Fine::new(part.scale[0]); 3],
                    Handle::ScaleAxis(_) => part.scale.map(Fine::new),
                    _ => [Fine::new(part.origin[0]), Fine::new(part.origin[1]), Fine::new(0.0)],
                };
                self.drag = Some(Drag {
                    handle,
                    part: i,
                    object: part.object.clone(),
                    feature: None,
                    start_plate,
                    start_screen: fe.abs,
                    start_z: 0.0,
                    base,
                    pivot: o,
                    rotate: part.rotate,
                    scale: part.scale,
                    axes: part_axes(part.turn),
                    centre,
                    along,
                    offset: [0.0; 3],
                    turn: 0.0,
                    by: [1.0; 3],
                    fine,
                    pointer: fe.abs,
                    sent: None,
                });
            }
            Hit::FingerMove(fe) if self.drag.as_ref().is_some_and(|d| d.feature.is_some()) => {
                let Some(mut drag) = self.drag.clone() else { return };
                let Some(f) = drag.feature.clone() else { return };
                let o = f.origin;
                drag.pointer = fe.abs;
                let mods = &fe.modifiers;
                match drag.handle {
                    Handle::MoveX | Handle::MoveY | Handle::MoveFree | Handle::Ghost => {
                        if let Some(p) = self.plate_at(cx, state, fe.abs, o[2]) {
                            let d = [p[0] - drag.start_plate[0], p[1] - drag.start_plate[1]];
                            // where its origin goes along each axis the handle moves, fine and snapped
                            let x = if drag.handle == Handle::MoveY { 0.0 } else { drag.fine[0].value(o[0] + d[0], mods, SNAP_MM) - o[0] };
                            let y = if drag.handle == Handle::MoveX { 0.0 } else { drag.fine[1].value(o[1] + d[1], mods, SNAP_MM) - o[1] };
                            drag.offset = [x, y, 0.0];
                        }
                    }
                    Handle::MoveZ => {
                        if let Some(h) = self.height_at(cx, state, fe.abs, o) {
                            drag.offset = [0.0, 0.0, drag.fine[2].value(o[2] + h - drag.start_z, mods, SNAP_MM) - o[2]];
                        }
                    }
                    Handle::Turn => {
                        if let Some(b) = self.plate_at(cx, state, fe.abs, o[2]) {
                            let t = swept_deg(drag.start_plate, b, [o[0], o[1]]);
                            drag.turn = drag.fine[0].value(t, mods, SNAP_DEG);
                        }
                    }
                    Handle::Scale | Handle::ScaleAxis(_) => {
                        let d0 = (drag.start_screen - drag.centre).length().max(4.0);
                        let f = ((fe.abs - drag.centre).length() / d0).clamp(0.05, 20.0);
                        drag.by = [drag.fine[2].value(f, mods, SNAP_SCALE).clamp(0.05, 20.0); 3];
                    }
                }
                let g = Self::gesture(&drag);
                let still = [PlaceGesture::Move([0.0; 3]), PlaceGesture::Turn(0.0), PlaceGesture::Scale(1.0)].contains(&g);
                if drag.sent != Some(g) && (drag.sent.is_some() || !still) {
                    drag.sent = Some(g);
                    cx.action(ParamAction::Place { object: drag.object.clone(), feature: f.object.clone(), gesture: g, done: false });
                }
                self.drag = Some(drag);
                crate::ui::redraw(cx, &[ids!(viewport), ids!(part_gizmo), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
            }
            Hit::FingerUp(fe) if self.drag.as_ref().is_some_and(|d| d.feature.is_some()) => {
                let Some(drag) = self.drag.take() else { return };
                let Some(f) = drag.feature.clone() else { return };
                let g = Self::gesture(&drag);
                let still = [PlaceGesture::Move([0.0; 3]), PlaceGesture::Turn(0.0), PlaceGesture::Scale(1.0)].contains(&g);
                if fe.cancelled || still {
                    // Back to the committed plate (only what was shown needs taking back).
                    if drag.sent.is_some() {
                        cx.action(ParamAction::Cancel { object: drag.object });
                    }
                } else {
                    cx.action(ParamAction::Place { object: drag.object, feature: f.object.clone(), gesture: g, done: true });
                    self.settling = Some(Settling { feature: f, gesture: g });
                }
                crate::ui::redraw(cx, &[ids!(viewport), ids!(part_gizmo), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
            }
            Hit::FingerMove(fe) => {
                let Some(mut drag) = self.drag.clone() else { return };
                let z = state.plate.parts.get(drag.part).map(|p| shown_part(state, p).min_mm[2]).unwrap_or(0.0);
                let element = state.plate.parts.get(drag.part).and_then(|p| p.element);
                let about = mm_to_world([drag.pivot[0], drag.pivot[1], z]);
                drag.pointer = fe.abs;
                let mods = &fe.modifiers;
                // the pose the part shows at: the drag's on top of the one it started from
                let pose = match drag.handle {
                    Handle::MoveX | Handle::MoveY | Handle::MoveZ | Handle::MoveFree | Handle::Ghost => self.plate_at(cx, state, fe.abs, z).map(|p| {
                        let (o, d) = (drag.pivot, [p[0] - drag.start_plate[0], p[1] - drag.start_plate[1]]);
                        // where its origin goes along each axis the handle moves, fine and snapped
                        let x = if drag.handle == Handle::MoveY { 0.0 } else { drag.fine[0].value(o[0] + d[0], mods, SNAP_MM) - o[0] };
                        let y = if drag.handle == Handle::MoveX { 0.0 } else { drag.fine[1].value(o[1] + d[1], mods, SNAP_MM) - o[1] };
                        drag.offset = [x, y, 0.0];
                        pose_turn_shift(about, 0.0, mm_to_world([x, y, 0.0]))
                    }),
                    Handle::Turn => self.plate_at(cx, state, fe.abs, z).map(|b| {
                        let t = swept_deg(drag.start_plate, b, [drag.pivot[0], drag.pivot[1]]);
                        // the turn it ends at, fine and snapped
                        drag.turn = drag.fine[0].value(drag.rotate + t, mods, SNAP_DEG) - drag.rotate;
                        pose_turn_shift(about, drag.turn as f32, vec3(0.0, 0.0, 0.0))
                    }),
                    Handle::Scale => {
                        let d0 = (drag.start_screen - drag.centre).length().max(4.0);
                        let f = ((fe.abs - drag.centre).length() / d0).clamp(0.05, 20.0);
                        // the scale it ends at (its X's), fine and snapped
                        let k = drag.scale[0].max(1e-6);
                        let f = (drag.fine[0].value(k * f, mods, SNAP_SCALE) / k).clamp(0.05, 20.0);
                        drag.by = [f; 3];
                        Some(pose_scale_about(about, f as f32))
                    }
                    Handle::ScaleAxis(a) => {
                        let a = a as usize;
                        // along the handle's line on screen, against the press's distance along it
                        let along = |p: DVec2| (p.x - drag.centre.x) * drag.along.x + (p.y - drag.centre.y) * drag.along.y;
                        let d0 = along(drag.start_screen).max(4.0);
                        let f = (along(fe.abs) / d0).clamp(0.05, 20.0);
                        let k = drag.scale[a].max(1e-6);
                        drag.by[a] = (drag.fine[a].value(k * f, mods, SNAP_SCALE) / k).clamp(0.05, 20.0);
                        Some(pose_stretch_about(about, &drag.axes, drag.by.map(|b| b as f32)))
                    }
                };
                if let (Some(pose), Some(e)) = (pose, element) {
                    state.scene_state.set_nudge(Some((e, pose_mul(&pose, &drag.base))));
                }
                self.drag = Some(drag);
                // Placement previews appear in the viewport and the part transform readouts.
                crate::ui::redraw(cx, &[ids!(viewport), ids!(part_gizmo), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
            }
            Hit::FingerUp(fe) => {
                let Some(drag) = self.drag.take() else { return };
                // The part keeps showing where it was let go (the preview stays until the
                // committed plate has it); a cancelled or empty drag goes back to where it was.
                let element = state.plate.parts.get(drag.part).and_then(|p| p.element);
                let back = |state: &mut AppState| {
                    let base = (drag.base != POSE_IDENTITY).then_some(drag.base);
                    state.scene_state.set_nudge(element.zip(base));
                };
                if fe.cancelled {
                    back(state);
                    // Placement previews appear in the viewport and the part transform readouts.
                    crate::ui::redraw(cx, &[ids!(viewport), ids!(part_gizmo), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
                    return;
                }
                let o = drag.pivot;
                match drag.handle {
                    Handle::MoveX | Handle::MoveY | Handle::MoveZ | Handle::MoveFree | Handle::Ghost => {
                        if drag.offset != [0.0; 3] {
                            cx.action(PartAction::Place { object: drag.object, at: [o[0] + drag.offset[0], o[1] + drag.offset[1]], rotate: drag.rotate });
                        } else {
                            back(state);
                        }
                    }
                    Handle::Turn => {
                        if drag.turn != 0.0 {
                            let mut r = drag.rotate + drag.turn;
                            while r > 180.0 {
                                r -= 360.0;
                            }
                            while r <= -180.0 {
                                r += 360.0;
                            }
                            cx.action(PartAction::Place { object: drag.object, at: [o[0], o[1]], rotate: r });
                        } else {
                            back(state);
                        }
                    }
                    Handle::Scale | Handle::ScaleAxis(_) => {
                        if drag.by.iter().any(|b| (b - 1.0).abs() > 1e-4) {
                            cx.action(PartAction::Scale { object: drag.object, scale: std::array::from_fn(|k| drag.scale[k] * drag.by[k]) });
                        } else {
                            back(state);
                        }
                    }
                }
                // Placement previews appear in the viewport and the part transform readouts.
                crate::ui::redraw(cx, &[ids!(viewport), ids!(part_gizmo), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        self.shapes = None;
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let Some(Target { part: i, feature }) = Self::wanted(state).or_else(|| self.drag.as_ref().map(|d| Target { part: d.part, feature: d.feature.clone() })) else { return DrawStep::done() };
        if let Some(b) = feature {
            // A feature dragged shows from the box its drag began with (the live preview moves
            // the one the panel has).
            let b = self.drag.as_ref().and_then(|d| d.feature.clone()).unwrap_or(b);
            self.draw_feature(cx, state, &b);
            return DrawStep::done();
        }
        // Drawn where the part shows: its placement preview is the drag's pose.
        let Some(rest) = state.plate.parts.get(i) else { return DrawStep::done() };
        let part = shown_part(state, rest);
        // the scale handles go along the part's own axes as it rests (a drag's stretch keeps them)
        let axes = part_axes(part.turn);
        let Some(shapes) = self.build_shapes(cx, state, pivot(&part), part.size(), part.max_mm, false, Some(&axes)) else { return DrawStep::done() };
        self.shapes = Some(shapes.clone());
        self.mode = state.ui.gizmo;
        let c = shapes.centre;
        match state.ui.gizmo {
            GizmoMode::Move => {
                if let Some(t) = shapes.x_tip {
                    let col = self.colour(Handle::MoveX, AXIS_X);
                    self.arrow(cx, c, t, col);
                }
                if let Some(t) = shapes.y_tip {
                    let col = self.colour(Handle::MoveY, AXIS_Y);
                    self.arrow(cx, c, t, col);
                }
                let col = self.colour(Handle::MoveFree, MOVE_FREE);
                self.dot(cx, c, 7.0, col, true);
                if let Some(d) = self.drag.clone() {
                    // Where it is, as the Part panel's Position reads it.
                    self.readout(cx, d.pointer, &format!("X {:.2}  Y {:.2} mm", part.origin[0], part.origin[1]));
                }
            }
            GizmoMode::Rotate => {
                let col = self.colour(Handle::Turn, RING);
                let pts = shapes.ring.clone();
                for w in pts.windows(2) {
                    self.line(cx, w[0], w[1], 2.5, col);
                }
                self.dot(cx, c, 3.5, RING, false);
                if let Some(d) = self.drag.clone() {
                    // The turn as a spoke from the centre, on the plate.
                    let pc = d.pivot;
                    let a0 = (d.start_plate[1] - pc[1]).atan2(d.start_plate[0] - pc[0]);
                    let a = a0 + d.turn.to_radians();
                    let r = (part.size()[0].hypot(part.size()[1]) * 0.5 + 6.0).max(10.0);
                    let proj = self.projector(cx, state);
                    if let Some(tip) = proj.project(mm_to_world([pc[0] + r * a.cos(), pc[1] + r * a.sin(), part.min_mm[2]])) {
                        self.line(cx, c, tip, 1.5, HOT);
                    }
                    self.readout(cx, d.pointer, &format!("{:+.1}°  → {:.1}°", d.turn, d.rotate + d.turn));
                }
            }
            GizmoMode::Scale => {
                for (k, base) in [AXIS_X, AXIS_Y, AXIS_Z].into_iter().enumerate() {
                    if let Some(t) = shapes.scale_axes[k] {
                        let col = self.colour(Handle::ScaleAxis(k as u8), base);
                        self.line(cx, c, t, 2.0, [col[0], col[1], col[2], 0.8]);
                        self.dot(cx, t, 5.5, col, true);
                    }
                }
                if let Some(h) = shapes.scale {
                    let col = self.colour(Handle::Scale, SCALE);
                    self.line(cx, c, h, 1.5, [col[0], col[1], col[2], 0.6]);
                    self.dot(cx, h, 7.0, col, true);
                }
                if let Some(d) = self.drag.clone() {
                    let s = part.size();
                    let pct = |k: usize| d.scale[k] * d.by[k] * 100.0;
                    let text = match d.handle {
                        Handle::ScaleAxis(a) => format!("{} {:.1} %", ["X", "Y", "Z"][a as usize], pct(a as usize)),
                        _ if pct(1) == pct(0) && pct(2) == pct(0) => format!("{:.1} %  ·  {:.1} × {:.1} × {:.1} mm", pct(0), s[0], s[1], s[2]),
                        _ => format!("{:.1} × {:.1} × {:.1} %  ·  {:.1} × {:.1} × {:.1} mm", pct(0), pct(1), pct(2), s[0], s[1], s[2]),
                    };
                    self.readout(cx, d.pointer, &text);
                }
            }
            GizmoMode::Off | GizmoMode::Shape | GizmoMode::Paint | GizmoMode::Sculpt | GizmoMode::Force => {}
        }
        DrawStep::done()
    }
}

/// The angle (degrees, -180..180) swept about `c` from `a` to `b` (plate mm).
pub(crate) fn swept_deg(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    let mut t = ((b[1] - c[1]).atan2(b[0] - c[0]) - (a[1] - c[1]).atan2(a[0] - c[0])).to_degrees();
    while t > 180.0 {
        t -= 360.0;
    }
    while t < -180.0 {
        t += 360.0;
    }
    t
}
