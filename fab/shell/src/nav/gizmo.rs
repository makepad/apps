//! The navigation gizmo: the view's one navigation control.
//!
//! Axis balls X / Y / Z with their negative ghosts, depth-sorted and scaled by
//! depth so the near ones read as near; click a ball for its preset view (Z:
//! from above), the gizmo's middle for the three-quarter view, double-click it
//! to frame the plate; drag anywhere on it to orbit, hover for the backdrop
//! ring. Zoom, pan and orthographic are the mouse's and View's.
//!
//! Placement note: the plate view docks this in an overlay row that far-edge-aligns
//! (`align: Align{x: 1.0}`), and `DrawVector` — which every SVG icon rides
//! on — is submitted with `add_aligned_instance`, so a deferred turtle shift
//! displaces its geometry. The widget therefore takes the **full width** (no
//! slack for the parent to distribute, so no shift) and corner-pins its own
//! panel rect, exactly the fix `PerfGraph` uses.
//!
//! The balls and the axis lines are `Sdf2d` because they are procedural marks.

use crate::api::*;
use makepad_widgets_core::tip::TipAction;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    mod.draw.DrawGizmoBall = mod.std.set_type_default() do #(DrawGizmoBall::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(1.0, 1.0, 1.0, 1.0)
        ghost: 0.0
        hover: 0.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let r = min(self.rect_size.x, self.rect_size.y) * 0.5 - 1.0
            sdf.circle(self.rect_size.x * 0.5, self.rect_size.y * 0.5, r)
            // Positive balls are solid; negatives are hollow until hovered,
            // which is exactly how Fab tells them apart.
            let bright = self.color.mix(vec4(1.0, 1.0, 1.0, 1.0), self.hover * 0.4)
            let hollow = vec4(self.color.xyz * 0.26, 0.85)
            let fill = bright.mix(hollow, self.ghost).mix(bright, self.hover)
            sdf.fill_keep(fill)
            sdf.stroke(vec4(bright.xyz, 0.9), 1.0)
            return sdf.result
        }
    }

    mod.draw.DrawGizmoLine = mod.std.set_type_default() do #(DrawGizmoLine::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(1.0, 1.0, 1.0, 1.0)
        p0: vec2(0.0, 0.0)
        p1: vec2(1.0, 1.0)
        thickness: 2.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.move_to(self.p0.x, self.p0.y)
            sdf.line_to(self.p1.x, self.p1.y)
            sdf.stroke(vec4(self.color.xyz * 0.85, 0.85), self.thickness)
            return sdf.result
        }
    }

    // The hover backdrop: invisible until the pointer is on the gizmo, then a
    // soft disc behind the balls — Fab's ring.
    mod.draw.DrawGizmoBackdrop = mod.std.set_type_default() do #(DrawGizmoBackdrop::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(0.0, 0.0, 0.0, 0.0)
        hover: 0.0
        radius: 44.0
        opacity: 0.55
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.circle(self.rect_size.x * 0.5, self.radius, self.radius - 1.0)
            sdf.fill(vec4(self.color.xyz, self.opacity * self.hover))
            return sdf.result
        }
    }

    mod.widgets.FabNavGizmoBase = #(FabNavGizmo::register_widget(vm))
    mod.widgets.FabNavGizmo = set_type_default() do mod.widgets.FabNavGizmoBase{
        width: Fill
        height: fab.gizmo_size
        ball_size: fab.gizmo_size
        draw_backdrop: mod.draw.DrawGizmoBackdrop{
            color: fab.color_editor
        }
        draw_ball: mod.draw.DrawGizmoBall{}
        draw_line: mod.draw.DrawGizmoLine{}
        draw_text: mod.draw.DrawText{
            text_style: theme.font_bold{
                font_size: fab.font_size_small
            }
            color: #x1a1a1a
        }
        color_x: fab.color_vp_axis_x
        color_y: fab.color_vp_axis_y
        color_z: fab.color_vp_axis_z
    }
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawGizmoBall {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    ghost: f32,
    #[live]
    hover: f32,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawGizmoLine {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    p0: Vec2f,
    #[live]
    p1: Vec2f,
    #[live]
    thickness: f32,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawGizmoBackdrop {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    hover: f32,
    #[live]
    radius: f32,
    #[live]
    opacity: f32,
}

#[derive(Script, ScriptHook, WidgetRef, WidgetRegister)]
pub struct FabNavGizmo {
    #[rust(PaneId::PLATE_ZERO)]
    pub pane: PaneId,
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[live(96.0)]
    ball_size: f64,
    #[live]
    draw_backdrop: DrawGizmoBackdrop,
    #[live]
    draw_ball: DrawGizmoBall,
    #[live]
    draw_line: DrawGizmoLine,
    #[live]
    draw_text: DrawText,
    #[live]
    color_x: Vec4f,
    #[live]
    color_y: Vec4f,
    #[live]
    color_z: Vec4f,
    #[rust]
    area: Area,
    #[rust]
    ball_rect: Rect,
    #[rust]
    balls: Vec<(PresetView, DVec2, f64)>,
    #[rust]
    drag_start: Option<DVec2>,
    #[rust]
    last_pos: Option<DVec2>,
    #[rust]
    dragged: bool,
    #[rust]
    hover_ball: Option<usize>,
    /// The press was the second of a double click: it frames the plate.
    #[rust]
    double: bool,
    #[rust(false)]
    hovered: bool,
}

impl FabNavGizmo {
    fn column_height(&self) -> f64 {
        self.ball_size
    }

    /// View-space direction of a world axis. `z` grows toward the viewer.
    fn axis_screen(cam: &Camera, axis: Vec3f) -> Vec3f {
        let r = cam.right();
        let u = cam.true_up();
        let f = cam.forward();
        vec3(axis.dot(r), axis.dot(u), -axis.dot(f))
    }

    fn ball_radius(&self, depth: f64, ghost: bool) -> f64 {
        let base = if ghost {
            self.ball_size * 0.072
        } else {
            self.ball_size * 0.094
        };
        // Near balls read bigger, which is the whole trick of the widget.
        base * (0.78 + 0.22 * (depth * 0.5 + 0.5))
    }

    fn nearest_ball(&self, pos: DVec2) -> Option<usize> {
        let mut best = None;
        let mut best_d = f64::MAX;
        for (i, (_, p, depth)) in self.balls.iter().enumerate() {
            let d = (pos - *p).length();
            let r = self.ball_radius(*depth, false) + 2.0;
            if d < r && d < best_d {
                best_d = d;
                best = Some(i);
            }
        }
        best
    }


}

impl WidgetNode for FabNavGizmo {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        Walk {
            height: Size::Fixed(self.column_height()),
            ..self.walk
        }
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
}

impl Widget for FabNavGizmo {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) => {
                // what it does acts on its own view
                if let Some(state) = scope.data.get_mut::<AppState>() {
                    state.view.pressed_pane = Some(self.pane);
                }
                self.drag_start = Some(fe.abs);
                self.last_pos = Some(fe.abs);
                self.dragged = false;
                self.double = fe.tap_count >= 2;
                self.area.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                let Some(last) = self.last_pos else {
                    return;
                };
                let delta = fe.abs - last;
                self.last_pos = Some(fe.abs);
                if self.drag_start.is_some_and(|start| (fe.abs - start).length() > 3.0) {
                    self.dragged = true;
                }
                if self.dragged {
                    cx.action(ShellAction::OrbitBy(delta.x as f32, delta.y as f32));
                    cx.set_cursor(MouseCursor::Grabbing);
                }
            }
            Hit::FingerUp(fe) => {
                // A press taken away (a list or the host took the finger) frames and snaps nothing.
                if !self.dragged && !fe.cancelled {
                    if self.double {
                        cx.action(ShellAction::FrameAll);
                    } else if let Some(i) = self.nearest_ball(fe.abs) {
                        cx.action(ShellAction::PresetView(self.balls[i].0));
                    } else {
                        cx.action(ShellAction::PresetView(PresetView::Isometric));
                    }
                }
                self.drag_start = None;
                self.last_pos = None;
                self.dragged = false;
                self.double = false;
                cx.set_cursor(MouseCursor::Default);
                self.area.redraw(cx);
            }
            Hit::FingerHoverIn(fh) | Hit::FingerHoverOver(fh) => {
                let ball = self.nearest_ball(fh.abs);
                if ball != self.hover_ball || !self.hovered {
                    if !self.hovered {
                        let r = self.ball_rect;
                        cx.widget_action(self.uid, TipAction::HoverIn("Click an axis to look along it, the middle for three-quarter; double-click to frame the plate (Home); drag to orbit".to_string(), r));
                    }
                    self.hover_ball = ball;
                    self.hovered = true;
                    self.area.redraw(cx);
                }
                cx.set_cursor(if ball.is_some() { MouseCursor::Hand } else { MouseCursor::Grab });
            }
            Hit::FingerHoverOut(_) => {
                cx.widget_action(self.uid, TipAction::HoverOut);
                self.hover_ball = None;
                self.hovered = false;
                self.area.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let walk = Walk {
            height: Size::Fixed(self.column_height()),
            ..walk
        };
        let outer = cx.walk_turtle(walk);
        let Some(state) = scope.data.get_mut::<AppState>() else {
            return DrawStep::done();
        };
        if !state.view.overlays.nav_gizmo {
            self.area = Area::Empty;
            self.balls.clear();
            return DrawStep::done();
        }
        let cam = *state.view.camera(self.pane);
        
        // Corner-pin: the widget owns the whole width so the parent's far-edge
        // align has no slack to shift the vector geometry with.
        let size = self.ball_size;
        let x0 = outer.pos.x + (outer.size.x - size).max(0.0);
        self.ball_rect = Rect {
            pos: dvec2(x0, outer.pos.y),
            size: dvec2(size, size),
        };
        let panel = Rect {
            pos: self.ball_rect.pos,
            size: dvec2(size, self.column_height()),
        };

        // Backdrop first, so the balls sit on it. It spans the whole column
        // (its shader only paints the disc at the top) and so it is also the
        // widget's hit area.
        self.draw_backdrop.hover = if self.hovered { 1.0 } else { 0.0 };
        self.draw_backdrop.radius = (size * 0.5) as f32;
        self.draw_backdrop.draw_abs(cx, panel);
        self.area = self.draw_backdrop.area();

        let center = self.ball_rect.pos + self.ball_rect.size * 0.5;
        let radius = size * 0.5 - self.ball_size * 0.115;
        let axes = [
            (PresetView::Right, vec3(1.0, 0.0, 0.0), self.color_x, "X", false),
            (PresetView::Back, vec3(0.0, 1.0, 0.0), self.color_y, "Y", false),
            (PresetView::Top, vec3(0.0, 0.0, 1.0), self.color_z, "Z", false),
            (PresetView::Left, vec3(-1.0, 0.0, 0.0), self.color_x, "", true),
            (PresetView::Front, vec3(0.0, -1.0, 0.0), self.color_y, "", true),
            (PresetView::Bottom, vec3(0.0, 0.0, -1.0), self.color_z, "", true),
        ];
        let mut items: Vec<(PresetView, DVec2, f64, Vec4f, &str, bool)> = axes
            .iter()
            .map(|(p, a, c, l, ghost)| {
                let s = Self::axis_screen(&cam, *a);
                let pos = center + dvec2(s.x as f64 * radius, -(s.y as f64) * radius);
                (*p, pos, s.z as f64, *c, *l, *ghost)
            })
            .collect();
        items.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

        // One anti-aliased quad per positive axis, not a trail of dots.
        for (_, pos, _, color, _, ghost) in &items {
            if *ghost {
                continue;
            }
            let d = *pos - center;
            if d.length() < 1.0 {
                continue;
            }
            let pad = 3.0;
            let min = dvec2(center.x.min(pos.x), center.y.min(pos.y)) - dvec2(pad, pad);
            let max = dvec2(center.x.max(pos.x), center.y.max(pos.y)) + dvec2(pad, pad);
            let rect = Rect {
                pos: min,
                size: max - min,
            };
            self.draw_line.color = *color;
            self.draw_line.p0 = vec2((center.x - min.x) as f32, (center.y - min.y) as f32);
            self.draw_line.p1 = vec2((pos.x - min.x) as f32, (pos.y - min.y) as f32);
            self.draw_line.thickness = 1.6;
            self.draw_line.draw_abs(cx, rect);
        }

        self.balls.clear();
        for (i, (preset, pos, depth, color, label, ghost)) in items.iter().enumerate() {
            let r = self.ball_radius(*depth, *ghost);
            self.draw_ball.color = *color;
            self.draw_ball.ghost = if *ghost { 1.0 } else { 0.0 };
            self.draw_ball.hover = if self.hover_ball == Some(i) { 1.0 } else { 0.0 };
            self.draw_ball.draw_abs(
                cx,
                Rect {
                    pos: *pos - dvec2(r, r),
                    size: dvec2(r * 2.0, r * 2.0),
                },
            );
            if !label.is_empty() {
                self.draw_text
                    .draw_abs(cx, *pos + dvec2(-r * 0.38, -r * 0.75), label);
            }
            self.balls.push((*preset, *pos, *depth));
        }

        DrawStep::done()
    }
}
