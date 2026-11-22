//! Shape handles: the selected part's parameters as handles in the view,
//! generated from their anchors (see [`crate::params`]). An arrow for a
//! length along an axis, a ring for a radius, an arc for an angle, a point
//! for a position or a profile's control points, + and − for a count.
//!
//! They show in the Edit shape mode (E), so they never clash with the move,
//! turn and scale gizmo. Which handles: the focused feature's (click a
//! feature of the part, or its group in the panel), else the part's declared
//! parameters, else its own object's fields. A parameter hovered in the
//! panel lights up here in any mode, with a faint box on the small features
//! it shapes unless its ghost shows them. No box shows while a value is edited.
//!
//! A drag previews the value live ([`ParamAction::Preview`], coalesced by the
//! app: the mesh and the slice follow the newest value) and commits one undo
//! step on release. A drag is continuous; Shift moves at a tenth of the speed without
//! a jump. Cmd (Ctrl off a Mac) snaps to whole steps or nearby faces of the part's
//! other features. A label by the pointer shows the value while dragging; a
//! double-click on a handle types one in.

use crate::api::*;
use crate::api::PaneId;
use crate::handle_style::*;
use crate::params::*;
use crate::ui::parts::active_part;
use super::fine_drag::{inside, readout_rect, Fine, SNAP_DEG, SNAP_MM, SNAP_SCALE};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    /// A segment from `a` to `b` (quad-local) with round caps, a disc (`kind` 1), or a
    /// rounded square (`kind` 2).
    mod.draw.DrawHandlePrim = mod.std.set_type_default() do #(DrawHandlePrim::script_shader(vm)){
        ..mod.draw.DrawQuad
        a: vec2(0.0, 0.0)
        b: vec2(0.0, 0.0)
        c: vec2(0.0, 0.0)
        width: 2.0
        kind: 0.0
        color: vec4(1.0, 1.0, 1.0, 1.0)
        pixel: fn() {
            let p = self.pos * self.rect_size
            if self.kind > 2.5 {
                // a filled triangle a, b, c (either winding)
                let d0 = (p.x - self.a.x) * (self.b.y - self.a.y) - (p.y - self.a.y) * (self.b.x - self.a.x)
                let d1 = (p.x - self.b.x) * (self.c.y - self.b.y) - (p.y - self.b.y) * (self.c.x - self.b.x)
                let d2 = (p.x - self.c.x) * (self.a.y - self.c.y) - (p.y - self.c.y) * (self.a.x - self.c.x)
                let pos = step(0.0, d0) * step(0.0, d1) * step(0.0, d2)
                let neg = step(0.0, 0.0 - d0) * step(0.0, 0.0 - d1) * step(0.0, 0.0 - d2)
                let inside = min(pos + neg, 1.0)
                return vec4(self.color.xyz * self.color.w * inside, self.color.w * inside)
            }
            if self.kind > 1.5 {
                let sdf = Sdf2d.viewport(p)
                sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, 3.0)
                sdf.fill_keep(self.color)
                sdf.stroke(vec4(0.0, 0.0, 0.0, #(OUTLINE_ALPHA)), #(OUTLINE_PX))
                return sdf.result
            }
            if self.kind > 0.5 {
                let sdf = Sdf2d.viewport(p)
                let c = self.rect_size * 0.5
                sdf.circle(c.x, c.y, min(c.x, c.y) - 1.0)
                sdf.fill_keep(self.color)
                sdf.stroke(vec4(0.0, 0.0, 0.0, #(OUTLINE_ALPHA)), #(OUTLINE_PX))
                return sdf.result
            }
            let pa = p - self.a
            let ba = self.b - self.a
            let h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.0001), 0.0, 1.0)
            let d = length(pa - ba * h) - self.width * 0.5
            let alpha = clamp(0.5 - d / 0.75, 0.0, 1.0)
            return vec4(self.color.xyz * self.color.w * alpha, self.color.w * alpha)
        }
    }

    mod.widgets.FabShapeHandlesBase = #(FabShapeHandles::register_widget(vm))
    mod.widgets.FabShapeHandles = set_type_default() do mod.widgets.FabShapeHandlesBase{
        width: Fill
        height: Fill
        draw_prim: mod.draw.DrawHandlePrim{}
        draw_text: mod.draw.DrawText{
            color: #xffffff
            text_style: theme.font_bold{ font_size: fab.font_size_ui }
        }
        draw_tag: mod.draw.DrawHandlePrim{ kind: #(HandlePrim::RoundedSquare as u32 as f32) }
        editor: ValueInput{ width: 120 height: theme.prop_row_height min: 0.0 max: 0.0 step: 0.01 precision: 2 }
    }
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawHandlePrim {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub a: Vec2f,
    #[live]
    pub b: Vec2f,
    #[live]
    pub c: Vec2f,
    #[live]
    pub width: f32,
    #[live]
    pub kind: f32,
    #[live]
    pub color: Vec4f,
}

impl DrawHandlePrim {
    /// A filled triangle (window points).
    pub fn set_triangle(&mut self, cx: &mut Cx2d, p: [DVec2; 3], color: Vec4f) {
        let lo = dvec2(p[0].x.min(p[1].x).min(p[2].x) - 1.0, p[0].y.min(p[1].y).min(p[2].y) - 1.0);
        let hi = dvec2(p[0].x.max(p[1].x).max(p[2].x) + 1.0, p[0].y.max(p[1].y).max(p[2].y) + 1.0);
        self.kind = HandlePrim::Triangle as u32 as f32;
        self.a = vec2((p[0].x - lo.x) as f32, (p[0].y - lo.y) as f32);
        self.b = vec2((p[1].x - lo.x) as f32, (p[1].y - lo.y) as f32);
        self.c = vec2((p[2].x - lo.x) as f32, (p[2].y - lo.y) as f32);
        self.color = color;
        self.draw_abs(cx, Rect { pos: lo, size: hi - lo });
    }

    /// A round-capped segment from `a` to `b` (window points).
    pub fn set_segment(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, width: f64, color: Vec4f) {
        let pad = width + 2.0;
        let lo = dvec2(a.x.min(b.x) - pad, a.y.min(b.y) - pad);
        let hi = dvec2(a.x.max(b.x) + pad, a.y.max(b.y) + pad);
        self.kind = HandlePrim::Segment as u32 as f32;
        self.width = width as f32;
        self.a = vec2((a.x - lo.x) as f32, (a.y - lo.y) as f32);
        self.b = vec2((b.x - lo.x) as f32, (b.y - lo.y) as f32);
        self.color = color;
        self.draw_abs(cx, Rect { pos: lo, size: hi - lo });
    }
}

/// Grab radius around a handle's grip (points).
const GRAB: f64 = 10.0;
/// What a no-trees area shows in (linear): amber, apart from the tools' red and blue.
const NO_TREES_RGB: [f32; 3] = [1.0, 0.62, 0.12];
/// What a print settings area shows in (linear): a pale cyan, apart from both.
const SETTINGS_RGB: [f32; 3] = [0.3, 0.85, 0.95];
/// The Strength list's picked place.
const WEAK_SPOT_RGB: [f32; 3] = [1.0, 0.35, 0.3];

/// How a body is drawn: its colour, its faces' and outline's alpha, and the outline's width.
struct Look {
    rgb: [f32; 3],
    film: f32,
    outline: f32,
    width: f64,
}

/// What a press hits.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    HandleGrip { key: String, grip: HandleGrip },
    Count { key: String, delta: f64 },
}

#[derive(Clone, Debug)]
struct Drag {
    key: String,
    grip: HandleGrip,
    part: usize,
    object: String,
    value: ParamValue,
    fine: Vec<Fine>,
    pointer: DVec2,
    /// The value changed at some point in this gesture.
    moved: bool,
    snapped: bool,
}

/// What a handle's drag sets for the pointer's raw value: continuous, a tenth of the speed
/// with Shift, and whole steps with Cmd/Ctrl. A nearby face takes priority over a step.
/// Numbers are held at their label's precision. The bool says whether a face was snapped to.
fn dragged(p: &ParamView, raw: ParamValue, grip: HandleGrip, fine: &mut [Fine], shift: bool, snap: bool, edge: impl Fn(f64) -> Option<f64>) -> (ParamValue, bool) {
    let step = match p.kind {
        ParamKind::Angle => SNAP_DEG,
        ParamKind::Ratio => SNAP_SCALE,
        ParamKind::Count | ParamKind::Seed => 1.0,
        ParamKind::Number if p.step > 0.0 && p.step < 0.1 => p.step,
        _ => SNAP_MM,
    };
    let mut snapped = false;
    let value = match raw {
        ParamValue::Num(x) => {
            let v = fine[0].value_with(x, shift, snap, step);
            let face = if snap { edge(v) } else { None };
            snapped = face.is_some();
            let scale = 10.0_f64.powi(precision(p.kind, p.step) as i32);
            ParamValue::Num((clamp_value(p, face.unwrap_or(v)) * scale).round() / scale)
        }
        ParamValue::Point(mut c) => {
            for (k, x) in c.iter_mut().enumerate() {
                *x = (fine[k].value_with(*x, shift, snap, SNAP_MM) * 100.0).round() / 100.0;
            }
            ParamValue::Point(c)
        }
        ParamValue::Profile(mut pts) => {
            if let Some(q) = pts.get_mut(grip.point) {
                for (k, x) in q.iter_mut().enumerate() {
                    *x = (fine[k].value_with(*x, shift, snap, SNAP_MM) * 100.0).round() / 100.0;
                }
            }
            ParamValue::Profile(pts)
        }
        other => other,
    };
    (value, snapped)
}

#[derive(Clone, Debug)]
struct Shape {
    target: Target,
    at: DVec2,
}

/// A print settings box's size or position row as it reads on the plate, as its rows in the
/// Overrides editor do ([`plate_axis`]); none for any other row.
fn box_axis(state: &AppState, p: &ParamView) -> Option<PlateAxis> {
    let boxed = state.params.areas.iter().any(|a| a.kind == AreaKind::Settings && p.objects.contains(&a.object));
    plate_axis(p).filter(|_| boxed)
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabShapeHandles {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_prim: DrawHandlePrim,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_tag: DrawHandlePrim,
    #[live]
    editor: ValueInput,
    #[rust]
    area: Area,
    #[rust(PaneId::PLATE_ZERO)]
    pub pane: PaneId,
    #[rust]
    shapes: Vec<Shape>,
    #[rust]
    hover: Option<Target>,
    #[rust]
    drag: Option<Drag>,
    /// The parameter being typed into (double-click), and where.
    #[rust]
    editing: Option<(String, usize, String, DVec2)>,
    /// Press position for a feature pick (a click that is not a drag).
    #[rust]
    press: Option<DVec2>,
    /// A press in the Edit shape mode held still opens Select Other ([`super::picks`]): its
    /// timer, and where it was pressed, with Shift, as a double click.
    #[rust]
    hold: Timer,
    #[rust]
    held: Option<(DVec2, bool, bool)>,
    /// Where the last click picked, until the pointer moves off it: a press there is the pick's
    /// (a double click's second), though the first click's part now shows a handle under it.
    #[rust]
    last_pick: Option<DVec2>,
    /// Frames while a ghost fades.
    #[rust]
    next_frame: NextFrame,
    /// The handles drawn, for their semantic nodes: (name, value text, grip rect).
    #[rust]
    nodes: Vec<(String, String, Rect)>,
    /// The print settings box drawn (it is drawn while picked): its name, its size and middle on
    /// the plate as drawn, its screen rect; for its semantic node.
    #[rust]
    drawn_box: Option<(String, String, Rect)>,
    /// What the rebuild says, for its semantic node (it is not drawn over the view).
    #[rust]
    status: Option<(String, Rect)>,
}

/// How long a press is held still before Select Other opens (seconds).
const HOLD_SECONDS: f64 = 0.45;

fn mm(p: V3) -> Vec3f {
    vec3(p[0] as f32 * 0.001, p[1] as f32 * 0.001, p[2] as f32 * 0.001)
}

/// The shared visual vocabulary for a parameter's existing handle definition.
pub fn handle_shape(h: &ParamHandle) -> HandleShape {
    match h {
        ParamHandle::Linear { .. } => HandleShape::Length,
        ParamHandle::Radius { .. } => HandleShape::Radius,
        ParamHandle::Angle { .. } => HandleShape::Angle,
        ParamHandle::Point { .. } => HandleShape::Point,
        ParamHandle::Profile { .. } => HandleShape::Profile,
        ParamHandle::Count { .. } => HandleShape::Count,
    }
}

/// The parameters whose handles show for a part.
pub fn shown_params<'a>(state: &AppState, pp: &'a PartParamsView) -> Vec<&'a ParamView> {
    let with = |p: &&ParamView| p.handle.is_some() && !p.detail && !p.derived;
    if let Some(focus) = &state.params.focus {
        let v: Vec<&ParamView> = pp.params.iter().filter(with).filter(|p| p.objects.iter().any(|o| o == focus)).collect();
        if !v.is_empty() {
            return v;
        }
    }
    let declared: Vec<&ParamView> = pp.params.iter().filter(with).filter(|p| p.declared).collect();
    if !declared.is_empty() {
        return declared;
    }
    pp.params.iter().filter(with).filter(|p| p.objects.first() == Some(&pp.object)).collect()
}

impl FabShapeHandles {
    fn projector(&self, cx: &Cx, state: &AppState) -> ViewProjector {
        ViewProjector::new(*state.view.camera(self.pane), self.area.rect(cx))
    }

    /// The selected part's handles show in the Select tool (the default) and in Edit shape; the
    /// Move, Turn, Scale and Paint tools have their own grips.
    fn wanted(state: &AppState) -> Option<usize> {
        if crate::ui::test_prints::picked(state).is_some() { return None; }
        if state.slice_blocks_edits() || !matches!(state.ui.gizmo, GizmoMode::Off | GizmoMode::Shape) || state.tool != Tool::Select {
            return None;
        }
        active_part(state)
    }

    fn pointer(&self, cx: &Cx, state: &AppState, screen: DVec2) -> DragPointer {
        let ray = self.projector(cx, state).ray(screen);
        let o = ray.origin;
        let d = ray.dir;
        DragPointer::ray([o.x as f64 * 1000.0, o.y as f64 * 1000.0, o.z as f64 * 1000.0], [d.x as f64, d.y as f64, d.z as f64])
    }

    /// Towards the eye, in plate mm.
    fn view_dir(state: &AppState, pane: PaneId) -> V3 {
        let cam = *state.view.camera(pane);
        let f = cam.forward();
        [-f.x as f64, -f.y as f64, -f.z as f64]
    }

    fn target_at(&self, p: DVec2) -> Option<Target> {
        self.shapes.iter().filter(|s| (s.at - p).length() < GRAB).min_by(|a, b| (a.at - p).length().total_cmp(&(b.at - p).length())).map(|s| s.target.clone())
    }

    fn line(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, width: f64, color: [f32; 4]) {
        self.draw_prim.set_segment(cx, a, b, width, vec4(color[0], color[1], color[2], color[3]));
    }

    fn dot(&mut self, cx: &mut Cx2d, c: DVec2, r: f64, color: [f32; 4], square: bool) {
        self.draw_prim.kind = (if square { HandlePrim::RoundedSquare } else { HandlePrim::Disc }) as u32 as f32;
        self.draw_prim.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_prim.draw_abs(cx, Rect { pos: c - dvec2(r, r), size: dvec2(r * 2.0, r * 2.0) });
    }

    fn polyline(&mut self, cx: &mut Cx2d, pts: &[DVec2], width: f64, color: [f32; 4]) {
        for w in pts.windows(2) {
            self.line(cx, w[0], w[1], width, color);
        }
    }

    /// A value label with its lower left corner at the given anchor.
    fn label(&mut self, cx: &mut Cx2d, at: DVec2, text: &str) {
        let tw = self.draw_text.prepare_single_line_run(cx, text).map(|r| r.width_in_lpxs as f64).unwrap_or(60.0);
        self.draw_tag.color = vec4(LABEL_BG[0], LABEL_BG[1], LABEL_BG[2], LABEL_BG[3]);
        self.draw_tag.kind = HandlePrim::RoundedSquare as u32 as f32;
        // while a drag runs, by its pointer; else by the handle; inside the view either way
        let (size, view) = (dvec2(tw + 14.0, 22.0), self.area.rect(cx));
        let r = match &self.drag {
            Some(d) => readout_rect(d.pointer, size, view),
            None => inside(Rect { pos: at + dvec2(14.0, -28.0), size }, view),
        };
        self.draw_tag.draw_abs(cx, r);
        self.draw_text.draw_abs(cx, r.pos + dvec2(7.0, 5.0), text);
    }

    /// The 12 edges of a box on the plate.
    fn draw_box(&mut self, cx: &mut Cx2d, proj: &ViewProjector, b: &ObjectBox) {
        let c = |i: usize| -> V3 { [if i & 1 == 0 { b.min[0] } else { b.max[0] }, if i & 2 == 0 { b.min[1] } else { b.max[1] }, if i & 4 == 0 { b.min[2] } else { b.max[2] }] };
        for (i, j) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
            if let (Some(a), Some(e)) = (proj.project(mm(c(i))), proj.project(mm(c(j)))) {
                self.line(cx, a, e, 0.75, COL_BOX);
            }
        }
    }

    /// A cage's wireframe, faint under its point handles: a point being dragged (or set and not
    /// yet built) is where its handle is drawn.
    fn draw_wire(&mut self, cx: &mut Cx2d, proj: &ViewProjector, state: &AppState, pp: &PartParamsView, w: &Wire) {
        let mut points = w.points.clone();
        let moved = state.params.dragging.iter().map(|(k, _)| k).chain(state.params.pending.iter().map(|(k, _)| k));
        for key in moved {
            let (Some(k), Some(p)) = (w.keys.iter().position(|x| x.as_ref() == Some(key)), pp.param(key)) else { continue };
            if let Some(ParamHandle::Point { at, .. }) = p.handle.as_ref().map(|h| h.at_value(&p.value, &state.params.shown_value(p))) {
                points[k] = at;
            }
        }
        let at: Vec<Option<DVec2>> = points.iter().map(|q| proj.project(mm(*q))).collect();
        for [a, b] in &w.edges {
            if let (Some(Some(a)), Some(Some(b))) = (at.get(*a as usize), at.get(*b as usize)) {
                self.line(cx, *a, *b, 1.0, COL_WIRE);
            }
        }
    }

    fn hot(&self, key: &str) -> bool {
        let hit = |t: &Target| match t {
            Target::HandleGrip { key: k, .. } | Target::Count { key: k, .. } => k == key,
        };
        self.hover.as_ref().is_some_and(hit) || self.drag.as_ref().is_some_and(|d| d.key == key)
    }

    /// Draw one parameter's handle and register its grips.
    /// `shown`: the value it is drawn at (a drag or a typed value moves it before the rebuild).
    fn draw_param(&mut self, cx: &mut Cx2d, proj: &ViewProjector, view: V3, p: &ParamView, shown: &ParamValue, lit: bool) {
        let Some(built) = &p.handle else { return };
        let moved = built.at_value(&p.value, shown);
        let h = &moved;
        let base = handle_shape(h).color();
        let visual = if self.drag.as_ref().is_some_and(|d| d.key == p.key) { HandleState::Active } else if lit || self.hot(&p.key) { HandleState::Hover } else { HandleState::Idle };
        let col = visual.color(base);
        let ppm = |q: V3| proj.points_per_meter_at(mm(q)) / 1000.0; // points per mm
        match h {
            ParamHandle::Linear { from, to, .. } => {
                let (Some(a), Some(b)) = (proj.project(mm(*from)), proj.project(mm(*to))) else { return };
                self.line(cx, a, b, 1.0, [col[0], col[1], col[2], 0.45]);
                // The arrow head past the tip, ~16 points long whatever the zoom.
                let d = unit(sub(*to, *from));
                let k = 16.0 / ppm(*to).max(1e-6);
                if let Some(tip) = proj.project(mm(add(*to, mul(d, k)))) {
                    let u = (tip - b) / (tip - b).length().max(1e-6);
                    let n = dvec2(-u.y, u.x);
                    self.line(cx, b, tip, 2.5, col);
                    self.line(cx, tip, tip - u * 7.0 + n * 4.5, 2.5, col);
                    self.line(cx, tip, tip - u * 7.0 - n * 4.5, 2.5, col);
                }
                self.dot(cx, b, 5.5, col, false);
                self.shapes.push(Shape { target: Target::HandleGrip { key: p.key.clone(), grip: HandleGrip::default() }, at: b });
            }
            ParamHandle::Radius { center, axis, radius, .. } => {
                let x = perp(*axis);
                let y = cross(unit(*axis), x);
                let pts: Vec<DVec2> = (0..=64)
                    .filter_map(|i| {
                        let a = i as f64 / 64.0 * std::f64::consts::TAU;
                        proj.project(mm(add(*center, add(mul(x, radius * a.cos()), mul(y, radius * a.sin())))))
                    })
                    .collect();
                self.polyline(cx, &pts, 2.0, [col[0], col[1], col[2], 0.85]);
                let g = grip_point(h, HandleGrip::default(), view);
                if let Some(s) = proj.project(mm(g)) {
                    self.dot(cx, s, 5.5, col, false);
                    self.shapes.push(Shape { target: Target::HandleGrip { key: p.key.clone(), grip: HandleGrip::default() }, at: s });
                }
            }
            ParamHandle::Angle { center, axis, from, radius, angle, .. } => {
                let r = if *radius > 0.0 { *radius } else { 40.0 / ppm(*center).max(1e-6) };
                let steps = 32;
                let pts: Vec<DVec2> = (0..=steps)
                    .filter_map(|i| {
                        let a = angle.to_radians() * i as f64 / steps as f64;
                        proj.project(mm(add(*center, mul(rotate(unit(*from), *axis, a), r))))
                    })
                    .collect();
                if let Some(c) = proj.project(mm(*center)) {
                    if let Some(first) = pts.first() {
                        self.line(cx, c, *first, 1.0, [col[0], col[1], col[2], 0.5]);
                    }
                    if let Some(last) = pts.last().copied() {
                        self.line(cx, c, last, 1.0, [col[0], col[1], col[2], 0.5]);
                        self.polyline(cx, &pts, 2.0, col);
                        self.dot(cx, last, 5.5, col, false);
                        self.shapes.push(Shape { target: Target::HandleGrip { key: p.key.clone(), grip: HandleGrip::default() }, at: last });
                    }
                }
            }
            ParamHandle::Point { at, .. } => {
                if let Some(s) = proj.project(mm(*at)) {
                    self.dot(cx, s, 5.0, col, true);
                    self.shapes.push(Shape { target: Target::HandleGrip { key: p.key.clone(), grip: HandleGrip::default() }, at: s });
                }
            }
            ParamHandle::Profile { origin, u, v, points } => {
                let pts: Vec<Option<DVec2>> = points.iter().map(|q| proj.project(mm(profile_point(*origin, *u, *v, *q)))).collect();
                let closed: Vec<DVec2> = pts.iter().chain(pts.first()).flatten().copied().collect();
                self.polyline(cx, &closed, 1.5, [col[0], col[1], col[2], 0.7]);
                for (i, s) in pts.iter().enumerate() {
                    if let Some(s) = s {
                        let g = HandleGrip { point: i };
                        let hot = self.drag.as_ref().is_some_and(|d| d.key == p.key && d.grip == g)
                            || self.hover == Some(Target::HandleGrip { key: p.key.clone(), grip: g });
                        self.dot(cx, *s, if hot { 5.5 } else { 4.0 }, if hot { COL_HOT } else { base }, true);
                        self.shapes.push(Shape { target: Target::HandleGrip { key: p.key.clone(), grip: g }, at: *s });
                    }
                }
            }
            ParamHandle::Count { at, axis } => {
                let Some(c) = proj.project(mm(*at)) else { return };
                let k = 18.0 / ppm(*at).max(1e-6);
                let along = proj.project(mm(add(*at, mul(*axis, k)))).map(|t| t - c).filter(|d| d.length() > 4.0).unwrap_or(dvec2(18.0, 0.0));
                let u = along / along.length().max(1e-6) * 16.0;
                for (sign, text) in [(1.0, "+"), (-1.0, "−")] {
                    let s = c + u * sign;
                    let target = Target::Count { key: p.key.clone(), delta: sign };
                    let hot = self.hover.as_ref() == Some(&target);
                    self.dot(cx, s, 8.5, if hot { COL_HOT } else { COUNT_BG }, false);
                    let tw = self.draw_text.prepare_single_line_run(cx, text).map(|r| r.width_in_lpxs as f64).unwrap_or(6.0);
                    self.draw_text.color = if hot { vec4(COUNT_INK_HOT[0], COUNT_INK_HOT[1], COUNT_INK_HOT[2], COUNT_INK_HOT[3]) } else { vec4(col[0], col[1], col[2], 1.0) };
                    self.draw_text.draw_abs(cx, s - dvec2(tw * 0.5, 8.0), text);
                    self.draw_text.color = vec4(1.0, 1.0, 1.0, 1.0);
                    self.shapes.push(Shape { target, at: s });
                }
            }
        }
    }

    fn draw_ghost(&mut self, cx: &mut Cx2d, proj: &ViewProjector, view: V3, g: &GhostView, fade: f32) {
        for body in &g.bodies {
            let base = if body.subtract { [0.95, 0.30, 0.25] } else { [0.30, 0.55, 1.0] };
            self.draw_body(cx, proj, view, body, Look { rgb: base, film: 0.16 * fade, outline: 0.9 * fade, width: 1.5 });
        }
    }

    /// A body as a film of its faces towards the eye and its outline.
    fn draw_body(&mut self, cx: &mut Cx2d, proj: &ViewProjector, view: V3, body: &GhostBody, look: Look) {
        let Look { rgb, film, outline, width } = look;
        const MAX_TRIS: usize = 6000;
        let pts: Vec<Option<DVec2>> = body.positions.iter().map(|q| proj.project(mm(*q))).collect();
        for t in body.triangles.iter().take(MAX_TRIS) {
            let [a, b, c] = t.map(|i| body.positions[i as usize]);
            let n = cross(sub(b, a), sub(c, a));
            // faces turned towards the eye only: the fill reads as one even film
            if dot(n, view) <= 0.0 {
                continue;
            }
            if let (Some(pa), Some(pb), Some(pc)) = (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]) {
                self.draw_prim.set_triangle(cx, [pa, pb, pc], vec4(rgb[0], rgb[1], rgb[2], film));
            }
        }
        for e in &body.edges {
            if let (Some(a), Some(b)) = (pts[e[0] as usize], pts[e[1] as usize]) {
                self.line(cx, a, b, width, [rgb[0], rgb[1], rgb[2], outline]);
            }
        }
    }

    /// The areas the scripts mark, in Design: where the trees keep out (amber), in the Sliced
    /// view where its trees stand, the one picked stronger; and the print settings box picked in
    /// the Overrides or the Parts list (cyan), in every mode, none other: as its size and place
    /// rows stand this frame. Not in the Sliced view while its slice is a job's or the printer's.
    fn draw_areas(&mut self, cx: &mut Cx2d, proj: &ViewProjector, state: &AppState) {
        self.drawn_box = None;
        let slice = state.render_mode() == RenderMode::Slice;
        if !state.ui.in_mode(PlateMode::Design) || (slice && state.slice_blocks_edits()) {
            return;
        }
        // The area being drawn: its outline at its foot and its top, and its sides (a sphere's
        // equator alone).
        if let Some((d, area)) = crate::tools::areas::draft().and_then(|d| Some((d, d.area()?))) {
            let ring: Vec<[f64; 2]> = match area {
                BedArea::Rect { min, max } => vec![min, [max[0], min[1]], max, [min[0], max[1]]],
                BedArea::Disc { centre, radius } => (0..48).map(|k| {
                    let a = k as f64 / 48.0 * std::f64::consts::TAU;
                    [centre[0] + radius * a.cos(), centre[1] + radius * a.sin()]
                }).collect(),
            };
            let (rgb, lo, hi) = match d.tool {
                Tool::SettingsHere => {
                    let (lo, hi) = settings_span(area, d.z);
                    let mid = 0.5 * (lo + hi);
                    (SETTINGS_RGB, if d.disc { mid } else { lo }, if d.disc { mid } else { hi })
                }
                _ => (NO_TREES_RGB, 0.0, NO_TREES_HEIGHT),
            };
            let at = |p: [f64; 2], z: f64| proj.project(mm([p[0], p[1], z]));
            let colour = [rgb[0], rgb[1], rgb[2], 1.0];
            let sides = if ring.len() == 4 { 1 } else { 12 };
            for (k, &p) in ring.iter().enumerate() {
                let q = ring[(k + 1) % ring.len()];
                for z in [lo, hi] {
                    if let (Some(a), Some(b)) = (at(p, z), at(q, z)) {
                        self.line(cx, a, b, 2.0, colour);
                    }
                }
                if k % sides == 0 && hi > lo {
                    if let (Some(a), Some(b)) = (at(p, lo), at(p, hi)) {
                        self.line(cx, a, b, 1.5, colour);
                    }
                }
            }
        }
        let picked = state.source.selected.as_ref().map(|id| format!("{}/{id}", state.source.instance));
        let view = Self::view_dir(state, self.pane);
        // the Strength list's picked place, in whichever way the plate's drawn
        if let Some(body) = state.ui.strength.highlight() {
            self.draw_body(cx, proj, view, &body, Look { rgb: WEAK_SPOT_RGB, film: 0.24, outline: 1.0, width: 2.0 });
        }
        for area in state.params.areas.clone().iter() {
            let on = picked.as_deref() == Some(area.object.as_str());
            let settings = area.kind == AreaKind::Settings;
            if if settings { !on } else { !slice } {
                continue;
            }
            let rgb = if settings { SETTINGS_RGB } else { NO_TREES_RGB };
            let bodies = if settings { area.shown_bodies(&state.params) } else { area.bodies.clone() };
            for body in &bodies {
                let (film, outline, width) = if on { (0.24, 1.0, 2.0) } else { (0.12, 0.75, 1.2) };
                self.draw_body(cx, proj, view, body, Look { rgb, film, outline, width });
            }
            if settings {
                let all = || bodies.iter().flat_map(|b| b.positions.iter());
                let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
                for p in all() {
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                let screen: Vec<DVec2> = all().filter_map(|p| proj.project(mm(*p))).collect();
                if !screen.is_empty() && hi[0] >= lo[0] {
                    let (a, b) = screen.iter().fold((screen[0], screen[0]), |(a, b), p| (dvec2(a.x.min(p.x), a.y.min(p.y)), dvec2(b.x.max(p.x), b.y.max(p.y))));
                    let size = format!("{:.1} x {:.1} x {:.1} mm at {:.1}, {:.1}, {:.1}", hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2], 0.5 * (lo[0] + hi[0]), 0.5 * (lo[1] + hi[1]), 0.5 * (lo[2] + hi[2]));
                    self.drawn_box = Some((area.object.clone(), size, Rect { pos: a, size: b - a }));
                }
            }
        }
    }

    fn commit(cx: &mut Cx, object: &str, key: &str, value: ParamValue) {
        cx.action(ParamAction::Commit { object: object.to_string(), key: key.to_string(), value });
    }

    /// The pick of a part's feature under a click: the smallest box the ray passes through.
    fn pick_feature(&self, cx: &Cx, state: &AppState, screen: DVec2, pp: &PartParamsView) -> Option<String> {
        let ptr = self.pointer(cx, state, screen);
        // A part drawn by its field: the feature whose surface the ray hits
        // (its Id node), exact.
        if let Some(sdf) = state.plate.parts.iter().find(|p| p.object == pp.object).and_then(|p| p.sdf.as_ref()) {
            let step = state.sdf.as_ref().map_or(1.0, |l| l.borrow().kernel_scale(&pp.object));
            if let Some(crate::viewport::sdf::PlateHit { id: Some(id), .. }) = crate::viewport::sdf::pick_plate(sdf, ptr.origin, ptr.dir, step) {
                if let Some(b) = pp.boxes.iter().find(|b| fab_sdf_engine::makepad_csg_sdf::part_id(&b.object) == id) {
                    return Some(b.object.clone());
                }
            }
        }
        let mut best: Option<(f64, &ObjectBox)> = None;
        for b in &pp.boxes {
            if ray_box(&ptr, b).is_some() {
                let vol = (0..3).map(|k| (b.max[k] - b.min[k]).max(0.1)).product::<f64>();
                if best.is_none_or(|(v, _)| vol < v) {
                    best = Some((vol, b));
                }
            }
        }
        best.map(|(_, b)| b.object.clone())
    }
}

fn ray_box(ptr: &DragPointer, b: &ObjectBox) -> Option<f64> {
    let (mut t0, mut t1) = (0.0f64, f64::INFINITY);
    for k in 0..3 {
        if ptr.dir[k].abs() < 1e-12 {
            if ptr.origin[k] < b.min[k] || ptr.origin[k] > b.max[k] {
                return None;
            }
            continue;
        }
        let a = (b.min[k] - ptr.origin[k]) / ptr.dir[k];
        let c = (b.max[k] - ptr.origin[k]) / ptr.dir[k];
        t0 = t0.max(a.min(c));
        t1 = t1.min(a.max(c));
    }
    (t0 <= t1).then_some(t0)
}

impl FabShapeHandles {
    /// A pick's ray at window point `pos`, with the view's plate mm per pixel.
    fn pick_ray(&self, cx: &Cx, state: &AppState, pos: DVec2) -> (V3, V3, [f64; 2]) {
        let ptr = self.pointer(cx, state, pos);
        (ptr.origin, ptr.dir, super::picks::pixel(state.view.camera(self.pane), self.area.rect(cx)))
    }

    /// The Edit shape mode's picks: hover asks what a click would take, a click picks (Shift
    /// adds, a double click the body), a press held still opens Select Other. A press on a
    /// handle is the handle's.
    fn pick_input(&mut self, cx: &mut Cx, input: &ViewportInput, state: &AppState) {
        use super::picks::PickAction;
        if !super::picks::active(state) {
            return;
        }
        match input.kind {
            ViewportInputKind::PointerMove { pos, buttons: 0, .. } => {
                if self.last_pick.is_some_and(|p| (pos - p).length() >= 4.0) {
                    self.last_pick = None;
                }
                let on_strip = super::picks::strip(self.area.rect(cx)).iter().any(|(r, _)| r.contains(pos));
                let ray = (self.hover.is_none() && state.picks.other.is_none() && !on_strip).then(|| self.pick_ray(cx, state, pos));
                let view = (*state.view.camera(self.pane), self.area.rect(cx));
                cx.action(PickAction::Hover(ray.map(|(from, dir, pixel)| super::picks::Hover { from, dir, pixel, at: pos, view })));
            }
            ViewportInputKind::PointerMove { pos, .. } => {
                if self.held.is_some_and(|(at, _, _)| (pos - at).length() >= 4.0) {
                    cx.stop_timer(self.hold);
                    self.held = None;
                }
            }
            ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, mods, tap_count } => {
                // Select Other open: this press closes it (a row's press is the overlay's); the
                // kind filter's strip and a handle take their own presses.
                let on_strip = super::picks::strip(self.area.rect(cx)).iter().any(|(r, _)| r.contains(pos));
                if state.picks.other.is_some() || on_strip || (self.target_at(pos).is_some() && !self.on_last_pick(state, pos)) {
                    self.held = None;
                    return;
                }
                self.held = Some((pos, mods.shift, tap_count >= 2));
                self.hold = cx.start_timeout(HOLD_SECONDS);
            }
            ViewportInputKind::PointerUp { button: PointerButton::Primary, pos, .. } => {
                cx.stop_timer(self.hold);
                if let Some((at, add, body)) = self.held.take() {
                    if (pos - at).length() < 4.0 {
                        self.last_pick = Some(pos);
                        let (from, dir, pixel) = self.pick_ray(cx, state, pos);
                        cx.action(PickAction::At { from, dir, pixel, add, body });
                    }
                }
            }
            ViewportInputKind::HoverOut => {
                cx.stop_timer(self.hold);
                self.held = None;
                cx.action(PickAction::Hover(None));
            }
            _ => {}
        }
    }

    /// A press where the last click picked, the pointer not moved off since (Edit shape mode).
    fn on_last_pick(&self, state: &AppState, pos: DVec2) -> bool {
        super::picks::active(state) && self.last_pick.is_some_and(|p| (pos - p).length() < 4.0)
    }

    pub(crate) fn handle_viewport_input(&mut self, cx: &mut Cx, input: &ViewportInput, state: &mut AppState) {
        if self.drag.is_some() || self.editing.is_some() { return; }
        self.pick_input(cx, input, state);
        match input.kind {
            ViewportInputKind::PointerMove { pos, buttons: 0, .. } => {
                let target = Self::wanted(state).and_then(|_| self.target_at(pos));
                if target != self.hover {
                    let previous = self.hover.take().map(|t| match t { Target::HandleGrip { key, .. } | Target::Count { key, .. } => key });
                    self.hover = target.clone();
                    let key = target.map(|t| match t { Target::HandleGrip { key, .. } | Target::Count { key, .. } => key });
                    if key.is_some() || state.params.hover == previous { state.params.hover = key; }
                    // Parameter hover and drag values appear in the viewport handles and the part panel.
                    crate::ui::redraw(cx, &[ids!(viewport), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
                }
                if self.hover.is_some() { cx.set_cursor(MouseCursor::Hand); }
            }
            ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, .. } => {
                self.press = Self::wanted(state).filter(|_| self.target_at(pos).is_none()).map(|_| pos);
            }
            ViewportInputKind::PointerUp { button: PointerButton::Primary, pos, .. } => {
                if self.press.take().is_some_and(|p| (pos - p).length() < 4.0) {
                    if let Some(pp) = Self::wanted(state).and_then(|i| state.params.part(i)) {
                        let focus = self.pick_feature(cx, state, pos, pp).filter(|o| *o != pp.object);
                        if focus != state.params.focus { cx.action(ParamAction::Focus(focus)); }
                    }
                }
            }
            ViewportInputKind::HoverOut => {
                self.press = None;
                if let Some(target) = self.hover.take() {
                    let key = match target { Target::HandleGrip { key, .. } | Target::Count { key, .. } => key };
                    if state.params.hover.as_ref() == Some(&key) { state.params.hover = None; }
                    // Parameter hover and drag values appear in the viewport handles and the part panel.
                    crate::ui::redraw(cx, &[ids!(viewport), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
                }
            }
            _ => {}
        }
    }
}

impl Widget for FabShapeHandles {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Shape handles");
        node.set_size = self.nodes.len() as u32;
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        // the print settings box drawn: its size on the plate as drawn
        if let Some((object, size, rect)) = &self.drawn_box {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::Image, bounds: makepad_platform::a11y::semantic_rect(*rect), ..Default::default() };
            makepad_platform::a11y::set_string(&mut node.name, &format!("Drawn print settings box {object}"));
            makepad_platform::a11y::set_string(&mut node.description, size);
            out.push((u64::MAX - 1, None, node));
        }
        if let Some((text, rect)) = &self.status {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::Status, bounds: makepad_platform::a11y::semantic_rect(*rect), ..Default::default() };
            makepad_platform::a11y::set_string(&mut node.name, text);
            out.push((u64::MAX, None, node));
        }
        for (i, (name, value, rect)) in self.nodes.iter().enumerate() {
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Slider,
                bounds: makepad_platform::a11y::semantic_rect(*rect),
                pos_in_set: i as u32 + 1,
                set_size: self.nodes.len() as u32,
                ..Default::default()
            };
            makepad_platform::a11y::set_string(&mut node.name, name);
            makepad_platform::a11y::set_string(&mut node.description, value);
            out.push((i as u64 + 1, None, node));
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.next_frame.is_event(event).is_some() {
            self.draw_prim.redraw(cx);
        }
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        // A press held still in the Edit shape mode: Select Other at the pointer, and no click.
        if self.hold.is_event(event).is_some() {
            if let Some((at, add, _)) = self.held.take() {
                let (from, dir, pixel) = self.pick_ray(cx, state, at);
                cx.action(super::picks::PickAction::Other { from, dir, pixel, at, add });
            }
        }
        // The typed value of a double-clicked handle.
        if let Some((key, part, object, _)) = self.editing.clone() {
            let uid = self.editor.widget_uid();
            let actions = cx.capture_actions(|cx| self.editor.handle_event(cx, event, scope));
            let ended = actions.filter_widget_actions_cast::<ValueInputAction>(uid)
                .filter_map(|action| match action { ValueInputAction::Ended(v) => Some(v), _ => None }).last();
            if let Some(v) = ended {
                self.editing = None;
                let Some(state) = scope.data.get::<AppState>() else { return };
                if let Some(p) = state.params.part(part).and_then(|pp| pp.param(&key)) {
                    Self::commit(cx, &object, &key, ParamValue::Num(clamp_value(p, v)));
                }
                self.draw_prim.redraw(cx);
            } else if !self.editor.is_interacting() {
                self.editing = None;
                self.draw_prim.redraw(cx);
            }
            // A press elsewhere ends typing: the editor commits on its own focus loss.
            return;
        }
        let Some(i) = Self::wanted(state).or_else(|| self.drag.as_ref().map(|d| d.part)) else {
            self.hover = None;
            return;
        };
        let Some(pp) = state.params.part(i).cloned() else { return };
        // Only grips capture the pointer; the viewport handles empty-space picks and hover.
        if matches!(event, Event::MouseMove(_)) && self.drag.is_none() { return; }
        // Only a plain left press on a grip is ours: the other buttons and Cmd/Ctrl's dolly are
        // the camera's, over a grip or not.
        if let Event::MouseDown(e) = event {
            if !e.button.is_primary() || e.modifiers.control || e.modifiers.logo || self.target_at(e.abs).is_none() || self.on_last_pick(state, e.abs) { return; }
        }
        if let Event::TouchUpdate(e) = event {
            if self.drag.is_none() && !e.touches.iter().any(|t| self.target_at(t.abs).is_some()) { return; }
        }
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                let Some(target) = self.target_at(fe.abs) else { return };
                match target {
                    Target::Count { key, delta } => {
                        if let Some(p) = pp.param(&key) {
                            if let ParamValue::Num(v) = p.value {
                                let nv = clamp_value(p, v + delta);
                                if nv != v {
                                    Self::commit(cx, &pp.object, &key, ParamValue::Num(nv));
                                }
                            }
                        }
                    }
                    Target::HandleGrip { key, grip } => {
                        let Some(p) = pp.param(&key) else { return };
                        if fe.tap_count >= 2 && p.kind.is_numeric() {
                            // Type a value in.
                            let at = self.shapes.iter().find(|s| s.target == Target::HandleGrip { key: key.clone(), grip }).map(|s| s.at).unwrap_or(fe.abs);
                            self.editing = Some((key.clone(), i, pp.object.clone(), at));
                            self.drag = None;
                            state.params.dragging = None;
                            let v = p.value.num().unwrap_or(0.0);
                            self.editor.set_value(cx, v);
                            self.editor.begin_edit(cx);
                            self.draw_prim.redraw(cx);
                            return;
                        }
                        let raw = p.handle.as_ref().and_then(|h| drag_value(h, &p.value, grip, &self.pointer(cx, state, fe.abs), Self::view_dir(state, self.pane)));
                        let fine = match &p.value {
                            ParamValue::Num(v) => vec![raw.as_ref().and_then(ParamValue::num).map_or_else(|| Fine::new(*v), |x| Fine::at(x, *v))],
                            ParamValue::Point(c) => c.iter().enumerate().map(|(k, v)| {
                                let x = match &raw { Some(ParamValue::Point(c)) => c.get(k), _ => None };
                                x.map_or_else(|| Fine::new(*v), |x| Fine::at(*x, *v))
                            }).collect(),
                            ParamValue::Profile(pts) => pts.get(grip.point).map(|q| q.iter().enumerate().map(|(k, v)| {
                                let x = match &raw { Some(ParamValue::Profile(pts)) => pts.get(grip.point).map(|q| q[k]), _ => None };
                                x.map_or_else(|| Fine::new(*v), |x| Fine::at(x, *v))
                            }).collect()).unwrap_or_default(),
                            _ => Vec::new(),
                        };
                        self.drag = Some(Drag { key: key.clone(), grip, part: i, object: pp.object.clone(), value: p.value.clone(), fine, pointer: fe.abs, moved: false, snapped: false });
                        state.params.dragging = Some((key, p.value.clone()));
                    }
                }
            }
            Hit::FingerMove(fe) => {
                let Some(mut drag) = self.drag.clone() else { return };
                let Some(p) = pp.param(&drag.key) else { return };
                let Some(h) = &p.handle else { return };
                let ptr = self.pointer(cx, state, fe.abs);
                let view = Self::view_dir(state, self.pane);
                let Some(raw) = drag_value(h, &p.value, drag.grip, &ptr, view) else { return };
                let proj = self.projector(cx, state);
                let tol_mm = 8.0 / (proj.points_per_meter_at(mm(grip_point(h, drag.grip, view))) / 1000.0).max(1e-6);
                let now = p.value.num().unwrap_or(0.0);
                let (value, snapped) = dragged(p, raw, drag.grip, &mut drag.fine, fe.modifiers.shift, fe.modifiers.is_primary(), |v| edge_snap(h, now, v, &pp.boxes, &p.objects, tol_mm));
                drag.snapped = snapped;
                drag.pointer = fe.abs;
                if value != drag.value {
                    drag.moved = true;
                    drag.value = value.clone();
                    state.params.dragging = Some((drag.key.clone(), value.clone()));
                    cx.action(ParamAction::Preview { object: drag.object.clone(), key: drag.key.clone(), value });
                }
                self.drag = Some(drag);
                // Parameter hover and drag values appear in the viewport handles and the part panel.
                crate::ui::redraw(cx, &[ids!(viewport), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
            }
            Hit::FingerUp(fe) => {
                let Some(drag) = self.drag.take() else { return };
                state.params.dragging = None;
                // A moved drag always answers: a preview may be showing. Let go where the model
                // already is, the commit is a cancel (the app knows what is committed).
                if fe.cancelled && drag.moved {
                    cx.action(ParamAction::Cancel { object: drag.object });
                } else if drag.moved {
                    Self::commit(cx, &drag.object, &drag.key, drag.value);
                }
                // Parameter hover and drag values appear in the viewport handles and the part panel.
                crate::ui::redraw(cx, &[ids!(viewport), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        self.shapes.clear();
        self.nodes.clear();
        self.status = None;
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let proj = ViewProjector::new(*state.view.camera(self.pane), rect);
        // The tool bodies of the parameter being edited or hovered: translucent faces towards the
        // eye and their outline, drawn over the part (they show before the booleans are redone).
        // (A marked area's shows as the view's areas do: picked.)
        let slice = state.render_mode() == RenderMode::Slice;
        let area = state.params.areas.iter().any(|a| (slice || a.kind == AreaKind::Settings) && state.params.ghost.as_ref().is_some_and(|g| g.key == a.object));
        if let Some(g) = state.params.ghost.clone().filter(|_| !area) {
            let fade = g.fade_from.map_or(1.0, |t| 1.0 - ((Cx::time_now() - t) / 0.4).clamp(0.0, 1.0)) as f32;
            if fade > 0.0 {
                self.draw_ghost(cx, &proj, Self::view_dir(state, self.pane), &g, fade);
                if g.fade_from.is_some() {
                    self.next_frame = cx.new_next_frame();
                }
            }
        }
        self.draw_areas(cx, &proj, state);
        // A part being recalculated shows in the view's shader (its surface animates); the build
        // job's words ("Rebuilding the model … fillets 3/7") live on the caption's job strip,
        // after 0.4 s. Nothing is drawn over the model; the words only feed the semantic node.
        let building = state.params.build_jobs.iter().rev().find_map(|id| makepad_widgets_core::jobs::board().get(*id).filter(|j| j.end.is_running()));
        if building.is_some() {
            self.next_frame = cx.new_next_frame();
        }
        let stage = building.as_ref().map(|j| j.stage_words()).filter(|w| !w.is_empty() && !w.starts_with("waiting"));
        let mut shown = false;
        if let Some(r) = state.params.rebuilding.clone() {
            if let Some(part) = state.plate.parts.iter().find(|p| p.object == r.object) {
                let top = [(part.min_mm[0] + part.max_mm[0]) * 0.5, (part.min_mm[1] + part.max_mm[1]) * 0.5, part.max_mm[2]];
                if let Some(at) = proj.project(mm(top)) {
                    let text = match &stage {
                        Some(w) => format!("Rebuilding… {w}"),
                        None => "Rebuilding…".to_string(),
                    };
                    self.status = Some((text, Rect { pos: at, size: dvec2(160.0, 22.0) }));
                    shown = true;
                }
            }
        }
        // The plate's build with no part chip (a file opening, a code edit), as the status
        // strip: only past 0.4 s, so quick work never flashes. A parameter edit has its part:
        // a mesh part says so above, a field part's view is current and says nothing.
        let long = building.as_ref().is_some_and(|j| j.elapsed(makepad_widgets_core::jobs::now()) >= 0.4);
        if let (false, true, false, Some(j)) = (shown, long, state.params.is_editing(), &building) {
            let text = match &stage {
                Some(w) => format!("{}… {w}", j.title),
                None => format!("{}…", j.title),
            };
            let at = rect.pos + dvec2(rect.size.x * 0.5 - 80.0, rect.size.y * 0.5);
            self.status = Some((text, Rect { pos: at, size: dvec2(240.0, 22.0) }));
        }
        let view = Self::view_dir(state, self.pane);
        let active = active_part(state);
        let wanted = Self::wanted(state).or_else(|| self.drag.as_ref().map(|d| d.part));
        // Feature boxes are only a hint: never over a value being edited (the model and the
        // ghost show the change), nor on what the hovered parameter's ghost already shows.
        let hover = state.params.hover.clone();
        let editing = self.drag.is_some() || self.editing.is_some() || state.source.editing || state.params.is_editing();
        let ghosted = state.params.ghost.as_ref().is_some_and(|g| !g.bodies.is_empty() && hover.as_deref() == Some(g.key.as_str()));
        let hint_boxes = !editing && !ghosted;
        // A panel row under the pointer lights its handle in any mode.
        if let (Some(key), Some(i)) = (hover.as_ref(), active) {
            if let Some(pp) = state.params.part(i).cloned() {
                if let Some(p) = pp.param(key) {
                    if hint_boxes {
                        for o in &p.objects {
                            if let Some(b) = pp.feature_box(o) {
                                self.draw_box(cx, &proj, b);
                            }
                        }
                    }
                    if wanted.is_none() {
                        let p = p.clone();
                        let shown = state.params.shown_value(&p);
                        self.draw_param(cx, &proj, view, &p, &shown, true);
                        self.shapes.clear();
                    }
                }
            }
        }
        if let Some(i) = wanted {
            if let Some(pp) = state.params.part(i).cloned() {
                if let Some(f) = state.params.focus.as_ref().and_then(|f| pp.feature_box(f)).filter(|_| hint_boxes) {
                    self.draw_box(cx, &proj, f);
                }
                for w in pp.wires.iter().filter(|w| state.params.focus.as_ref().is_none_or(|f| *f == w.object)) {
                    self.draw_wire(cx, &proj, state, &pp, w);
                }
                for p in shown_params(state, &pp) {
                    let lit = hover.as_deref() == Some(p.key.as_str());
                    let first = self.shapes.len();
                    let shown = state.params.shown_value(p);
                    self.draw_param(cx, &proj, view, p, &shown, lit);
                    let axis = box_axis(state, p);
                    let label = axis.as_ref().map_or_else(|| p.label.clone(), |a| a.label.clone());
                    let value = match (&shown, axis.as_ref().map(|a| a.sign)) {
                        (ParamValue::Num(x), Some(sign)) => format_value(p, &ParamValue::Num(sign * x)),
                        _ => format_value(p, &shown),
                    };
                    for s in &self.shapes[first..] {
                        let name = match &s.target {
                            Target::HandleGrip { grip, .. } if p.kind == ParamKind::Profile => format!("{label} point {}", grip.point + 1),
                            Target::HandleGrip { .. } => format!("{label} handle"),
                            Target::Count { delta, .. } => format!("{label} {}", if *delta > 0.0 { "more" } else { "fewer" }),
                        };
                        self.nodes.push((name, value.clone(), Rect { pos: s.at - dvec2(GRAB, GRAB), size: dvec2(GRAB * 2.0, GRAB * 2.0) }));
                    }
                }
                // The live value follows the pointer during a drag, or stays by a hovered handle.
                let shown = self.drag.as_ref().map(|d| (d.key.clone(), d.grip, Some(d.value.clone()), d.snapped)).or_else(|| match &self.hover {
                    Some(Target::HandleGrip { key, grip }) => Some((key.clone(), *grip, None, false)),
                    Some(Target::Count { key, .. }) => Some((key.clone(), HandleGrip::default(), None, false)),
                    None => None,
                });
                if let Some((key, grip, value, snapped)) = shown {
                    if let Some(p) = pp.param(&key) {
                        let v = value.unwrap_or_else(|| state.params.shown_value(p));
                        let at = self.shapes.iter().find(|s| match &s.target {
                            Target::HandleGrip { key: k, grip: g } => *k == key && *g == grip,
                            Target::Count { key: k, .. } => *k == key,
                        });
                        if let Some(at) = at.map(|s| s.at) {
                            let text = match (&v, grip, &box_axis(state, p)) {
                                (ParamValue::Profile(pts), g, _) => pts.get(g.point).map(|q| format!("{} {}: {:.1}, {:.1} mm", p.label, g.point + 1, q[0], q[1])).unwrap_or_default(),
                                (ParamValue::Num(x), _, Some(a)) => format!("{}  {}", a.label, format_value(p, &ParamValue::Num(a.sign * x))),
                                _ => format!("{}  {}", p.label, format_value(p, &v)),
                            };
                            if snapped {
                                self.dot(cx, at, 9.0, [COL_SNAP[0], COL_SNAP[1], COL_SNAP[2], 0.35], false);
                            }
                            self.label(cx, at, &text);
                        }
                    }
                }
            }
        }
        if let Some((_, _, _, at)) = self.editing {
            let mut walk = self.editor.walk(cx);
            walk.abs_pos = Some(at + dvec2(12.0, -24.0));
            let _ = self.editor.draw_walk(cx, &mut Scope::empty(), walk);
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shape handle's drag is continuous, a tenth of the speed with Shift, and snaps with
    /// Cmd/Ctrl: to whole millimetres (5° for an angle), or to a face within reach first.
    #[test]
    fn a_handle_drag_is_continuous_fine_with_shift_and_snaps_with_cmd() {
        let mut p = ParamView { kind: ParamKind::Length, step: 0.1, ..Default::default() };
        let grip = HandleGrip::default();
        let mut fine = [Fine::at(10.0, 10.0)];
        let scale = 10.0_f64.powi(precision(p.kind, p.step) as i32);
        let rounded = |v: f64| ParamValue::Num((v * scale).round() / scale);
        assert_eq!(dragged(&p, ParamValue::Num(12.37), grip, &mut fine, false, false, |_| panic!("a continuous drag never snaps to a face")), (rounded(12.37), false), "a plain drag follows the pointer at label precision");
        assert_eq!(dragged(&p, ParamValue::Num(12.37), grip, &mut fine, true, false, |_| None), (rounded(12.37), false), "Shift starts without a jump");
        assert_eq!(dragged(&p, ParamValue::Num(14.37), grip, &mut fine, true, false, |_| None), (rounded(12.37 + 0.2), false), "Shift moves at a tenth of the speed");
        let mut fine = [Fine::at(10.0, 10.0)];
        assert_eq!(dragged(&p, ParamValue::Num(12.6), grip, &mut fine, false, true, |_| None), (ParamValue::Num(13.0), false), "Cmd/Ctrl snaps to whole millimetres");
        assert_eq!(dragged(&p, ParamValue::Num(12.6), grip, &mut fine, false, true, |_| Some(12.0)), (ParamValue::Num(12.0), true), "a nearby face takes priority");
        p.kind = ParamKind::Angle;
        assert_eq!(dragged(&p, ParamValue::Num(17.4), grip, &mut fine, false, true, |_| None), (ParamValue::Num(15.0), false), "angles snap to five degrees");
    }
}
