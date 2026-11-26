//! `FabToolOverlay` — everything the tools draw over the plate view.
//!
//! The plate view (`ui/stage.rs`) stacks one over the viewport. It is a 2D
//! overlay: it never renders geometry, it projects world points with
//! `api::ViewProjector` and draws lines, markers and labels.
//!
//! What it draws, all of it from `AppState` + `tools::session`:
//!
//! * finished measurements — dimension line, end ticks, halo label
//! * the measurement being placed — rubber band to the snapped cursor, live
//!   value, and the snap glyph that says *why* the point is where it is
//! * section planes (outline + normal arrow) and the section box (12 edges),
//!   with grab handles that `ToolSet` drags
//! * the tool's one-line feedback and the plate notice
//! * in the Force render mode: each fixture's mark and each load's arrow on the studied part,
//!   its weakest point, the colour key and the study's state in a line (`draw_force`)
//! * in Mechanics, the joint check's marks: each joint's region, the closest gap and its value,
//!   the layers a bead could tack the parts in, each turn's stop and its range, and what binds a
//!   joint that is not free (`draw_mech_check`)
//!
//! It owns one `NextFrame` for the section animate-in.

use crate::api::*;
use crate::api::PaneId;
use crate::tools::{measure, picks, section, session, snap};
use makepad_widgets_core::*;
use std::sync::Arc;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    // A screen-space segment: one quad over the segment's bounding box, the
    // distance to the line computed in the pixel shader. Dashed when dash > 0.
    mod.draw.DrawToolLine = mod.std.set_type_default() do #(DrawToolLine::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(1.0, 1.0, 1.0, 1.0)
        line_a: vec2(0.0, 0.0)
        line_b: vec2(1.0, 1.0)
        line_width: 1.5
        dash: 0.0
        pixel: fn() {
            let p = self.pos * self.rect_size
            let ba = self.line_b - self.line_a
            let pa = p - self.line_a
            let h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.0001), 0.0, 1.0)
            let d = length(pa - ba * h)
            let cover = 1.0 - smoothstep(self.line_width * 0.5 - 0.7, self.line_width * 0.5 + 0.7, d)
            let t = h * length(ba)
            let period = max(self.dash, 0.0001)
            let on = 1.0 - step(0.5, self.dash) * (1.0 - step(fract(t / period), 0.58))
            let a = cover * on * self.color.w
            return vec4(self.color.xyz * a, a)
        }
    }

    // Point marker. shape 0 = disc, 1 = diamond, 2 = square.
    mod.draw.DrawToolMarker = mod.std.set_type_default() do #(DrawToolMarker::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(1.0, 1.0, 1.0, 1.0)
        shape: 0.0
        hollow: 0.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            let r = min(w, h) * 0.5 - 1.0
            if self.shape < 0.5 {
                sdf.circle(w * 0.5, h * 0.5, r)
            } else {
                if self.shape < 1.5 {
                    sdf.rotate(0.785398, w * 0.5, h * 0.5)
                    sdf.box(w * 0.5 - r * 0.68, h * 0.5 - r * 0.68, r * 1.36, r * 1.36, 0.5)
                } else {
                    sdf.box(w * 0.5 - r * 0.78, h * 0.5 - r * 0.78, r * 1.56, r * 1.56, 0.5)
                }
            }
            sdf.fill_keep(vec4(self.color.xyz, self.color.w * (1.0 - self.hollow)))
            sdf.stroke(vec4(self.color.xyz, self.color.w), 1.25)
            return sdf.result
        }
    }

    // A list's backing (Select Other): a rounded dark box.
    mod.draw.DrawToolPanel = mod.std.set_type_default() do #(DrawToolPanel::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(0.06, 0.06, 0.07, 0.92)
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 5.0)
            sdf.fill(self.color)
            return sdf.result
        }
    }

    mod.widgets.FabToolOverlayBase = #(FabToolOverlay::register_widget(vm))
    mod.widgets.FabToolOverlay = set_type_default() do mod.widgets.FabToolOverlayBase{
        width: Fill
        height: Fill
        color_overlap: fab.color_error
        color_notice_warn: fab.color_warning
        color_measure: fab.color_vp_measure
        color_section: fab.color_vp_section
        color_accent: fab.color_accent
        color_force_text: #xd8d8d8
        color_fixed: #x5a8cff
        color_force: #xff5ad9
        color_pressure: #xffa633
        color_torque: #x4dffa6
        draw_line: mod.draw.DrawToolLine{}
        draw_marker: mod.draw.DrawToolMarker{}
        draw_panel: mod.draw.DrawToolPanel{}
        draw_text: mod.draw.DrawText{
            text_style: theme.font_bold{
                font_size: fab.font_size_small
            }
            color: fab.color_vp_measure
        }
        draw_text_small: mod.draw.DrawText{
            text_style: theme.font_regular{
                font_size: fab.font_size_small
            }
            color: fab.color_text_dim
        }
    }
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawToolLine {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
    #[live]
    pub line_a: Vec2f,
    #[live]
    pub line_b: Vec2f,
    #[live(1.5)]
    pub line_width: f32,
    #[live(0.0)]
    pub dash: f32,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawToolMarker {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
    #[live(0.0)]
    pub shape: f32,
    #[live(0.0)]
    pub hollow: f32,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawToolPanel {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
}

/// Where the view's top-left lines start: right of the tool rail (its 10 pt inset and 38 pt
/// width, with a gap), and further right by a tool strip's width when one is open beside it.
const RAIL_RIGHT: f64 = 56.0;
const STRIP_WIDTH: f64 = 42.0;
/// Where those lines wrap while the navigation gizmo is shown: left of its column (96 pt and the
/// stage's 8 pt inset, `nav_col` in stage.rs), with a gap.
const NAV_LEFT: f64 = 112.0;
/// Under the gizmo's column (its top, `nav_top`, the gizmo's 96 pt, a gap): where the lines start
/// when the room left of it is narrower than `LINE_MIN`, and take the view's width instead.
const NAV_BELOW: f64 = 104.0;
const LINE_MIN: f64 = 160.0;

/// A tool's strip is open beside the rail (Paint's, Sculpt's, Force's, Edit shape's).
fn strip_open(state: &AppState) -> bool {
    state.tool == Tool::MeasureFit || crate::tools::paint::active(state)
        || crate::tools::sculpt::active(state)
        || state.force_tool()
        || (state.ui.gizmo == GizmoMode::Shape && state.tool == Tool::Select && state.ui.in_mode(PlateMode::Design))
}

/// Marker shapes.
const DISC: f32 = 0.0;
const DIAMOND: f32 = 1.0;
const SQUARE: f32 = 2.0;
/// The tick along a crease under the pointer, long (points).
const TICK_PX: f64 = 22.0;

/// The ring as drawn, retained for semantic queries between frames.
struct DrawnMenu {
    bounds: Rect,
    layout: Arc<picks::MenuLayout>,
    items: Vec<picks::MenuItem>,
}

#[derive(Script, ScriptHook, WidgetRef, WidgetRegister)]
pub struct FabToolOverlay {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[live]
    color_notice_warn: Vec4f,
    #[live]
    color_overlap: Vec4f,
    #[live]
    color_measure: Vec4f,
    #[live]
    color_section: Vec4f,
    #[live]
    color_accent: Vec4f,
    /// The Force render mode's text and its load kinds' colours.
    #[live]
    color_force_text: Vec4f,
    #[live]
    color_fixed: Vec4f,
    #[live]
    color_force: Vec4f,
    #[live]
    color_pressure: Vec4f,
    #[live]
    color_torque: Vec4f,
    #[live]
    draw_line: DrawToolLine,
    #[live]
    draw_marker: DrawToolMarker,
    #[live]
    draw_panel: DrawToolPanel,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_text_small: DrawText,
    #[rust]
    area: Area,
    #[rust(PaneId::PLATE_ZERO)]
    pub pane: PaneId,
    /// The gizmo's top in the view, as the stage placed it (lower under a render row that would
    /// reach its column, or under Mechanics' header strip).
    #[rust(6.0)]
    pub nav_top: f64,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    last_time: f64,
    #[rust]
    readout_shown: bool,
    #[rust]
    notice_revision: u64,
    /// The plate notice as drawn this frame (its words and where), for the semantic tree.
    #[rust]
    notice_shown: Option<(String, Rect)>,
    /// Whether the plate drawn this frame is a drag's preview, not committed (`PlateInfo::preview`),
    /// for the semantic tree: the view shows it by its shape, a reader by this.
    #[rust]
    preview_shown: bool,
    /// Select Other's rows as drawn (window points), for the pointer.
    #[rust]
    other_rows: Vec<Rect>,
    #[rust]
    menu_shown: Option<DrawnMenu>,
}

impl FabToolOverlay {
    /// True while the size readout is up (the overlay wants frames to take it down).
    fn draw_brush(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector) -> bool {
        let sculpt = crate::tools::sculpt::active(state);
        if !sculpt && !crate::tools::paint::active(state) { return false; }
        let (hit, size_at) = session::with(|s| if sculpt { (s.sculpt.hover, s.sculpt.size_at) } else { (s.paint.hover, s.paint.size_at) });
        let Some(hit) = hit else { return false };
        let radius_mm = if sculpt { state.ui.sculpt_radius } else { state.ui.paint_radius };
        let n = hit.normal;
        let axis = if n.z.abs() < 0.9 { vec3(0.0, 0.0, 1.0) } else { vec3(0.0, 1.0, 0.0) };
        let u = Vec3f::cross(n, axis).normalize();
        let v = Vec3f::cross(n, u).normalize();
        let radius = radius_mm as f32 * 0.001;
        let mut circle = Vec::with_capacity(48);
        for i in 0..48 {
            let angle = i as f32 * std::f32::consts::TAU / 48.0;
            if let Some(p) = proj.project(hit.point + (u * angle.cos() + v * angle.sin()) * radius) { circle.push(p); }
        }
        // A dark halo keeps the physical-size disc legible on light filaments.
        self.polyline(cx, &circle, true, vec4(0.0, 0.0, 0.0, 0.8), 3.5, false);
        // Erase and cutting brushes have a dashed, red-tinted ring.
        let erase = if sculpt { matches!(crate::tools::sculpt::operation(state), fab_paint_field::StrokeOp::Lower | fab_paint_field::StrokeOp::Crease) } else { crate::tools::paint::erasing(state) };
        let [r, g, b] = if erase { [1.0, 0.35, 0.3] } else if sculpt { [0.8, 0.8, 0.85] } else { state.ui.paint_color };
        self.polyline(cx, &circle, true, vec4(r, g, b, 1.0), 1.8, erase);
        if let Some(p) = proj.project(hit.point) { self.marker(cx, p, 2.5, vec4(r, g, b, 1.0), DISC, erase); }
        // The size readout by the ring while Shift+scroll sizes it: a floating label, no layout.
        let since = Cx::time_now() - size_at;
        if since < crate::tools::paint::SIZE_READOUT_SECS {
            let side = circle.iter().map(|p| p.x).fold(f64::MIN, f64::max);
            if let Some(c) = proj.project(hit.point) {
                self.halo_text(cx, dvec2(side.max(c.x) + 8.0, c.y - 6.0), &format!("{} mm", crate::tools::paint::diameter_text(radius_mm * 2.0)), vec4(1.0, 1.0, 1.0, 1.0));
            }
            return true;
        }
        false
    }

    // ---- picks -----------------------------------------------------------

    /// The picks for the AI ([`crate::tools::picks`]): a ring at each pick's spot with its words
    /// (a dashed box round a pick named without a spot), the one whose chat chip is under the
    /// pointer brighter; what a click would take, in words at the pointer (a short tick along a
    /// crease there); Select Other's list. An exact part's face and edge outlines are the
    /// viewport's (`DrawFabPickLines`), and a field part's edits are tinted where it draws them.
    fn draw_picks(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector, rect: Rect) {
        use crate::tools::picks::on_plate;
        let picks = &state.picks;
        // the commands the picks take (Round, Chamfer), while picks are made: the one running lit.
        // The kind filter is the plate view's tool row's (`stage.rs`), not drawn here.
        if crate::tools::picks::active(state) {
            let controls = picks::strip_view(state, self.pane, rect);
            let running = state.params.command.as_ref().map(|c| c.param.label.as_str());
            let blocked = state.pick_tool_block_reason().is_some();
            for (r, label, _) in crate::tools::picks::commands(controls, &picks.list) {
                let on = running == Some(label);
                self.draw_panel.color = if on { vec4(self.color_accent.x, self.color_accent.y, self.color_accent.z, 0.9) } else { vec4(0.06, 0.06, 0.07, 0.8) };
                self.draw_panel.draw_abs(cx, r);
                self.small_text(cx, r.pos + dvec2(9.0, 5.0), label, if blocked { vec4(0.55, 0.55, 0.55, 1.0) } else if on { vec4(1.0, 1.0, 1.0, 1.0) } else { self.color_measure });
            }
            self.draw_panel.color = vec4(0.06, 0.06, 0.07, 0.92);
        }
        let at = |p: [f64; 3]| proj.project(vec3(p[0] as f32 * 0.001, p[1] as f32 * 0.001, p[2] as f32 * 0.001));
        let halo = vec4(0.04, 0.04, 0.04, 0.85);
        // what a click would take on a crease: a tick along it at the pointer's point
        if let (Some(under), None, None) = (&picks.under, &picks.other, &picks.menu) {
            if let Some((p, d)) = under.tick.filter(|_| under.view == (*state.view.camera(self.pane), rect)) {
                let ends = [0.0, 0.5].map(|k| on_plate(state, &under.object, [p[0] + d[0] * k, p[1] + d[1] * k, p[2] + d[2] * k]).and_then(at));
                if let [Some(a), Some(b)] = ends {
                    let way = b - a;
                    let l = way.length();
                    if l > 1e-6 {
                        let half = way * (TICK_PX * 0.5 / l);
                        self.seg(cx, a - half, a + half, halo, 4.4, false);
                        self.seg(cx, a - half, a + half, self.color_measure, 2.2, false);
                    }
                }
            }
        }
        for pick in picks.list.iter() {
            let lit = picks.lit.as_deref() == Some(pick.id.as_str());
            let color = if lit { vec4(1.0, 1.0, 1.0, 1.0) } else { self.color_accent };
            let words = format!("{} {}", pick.kind, pick.label);
            if let Some(p) = pick.at.and_then(|(p, _)| on_plate(state, &pick.object, p)).and_then(at) {
                let r = if lit { 8.0 } else { 6.0 };
                self.marker(cx, p, r + 1.5, halo, DISC, true);
                self.marker(cx, p, r, color, DISC, !lit);
                self.halo_text(cx, p + dvec2(r + 5.0, -8.0), &words, color);
            } else if let Some((lo, hi)) = pick.bounds {
                let corner = |c: usize| on_plate(state, &pick.object, [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }]).and_then(at);
                let corners: Vec<Option<DVec2>> = (0..8).map(corner).collect();
                for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                    if let (Some(a), Some(b)) = (corners[a], corners[b]) {
                        self.seg(cx, a, b, color, if lit { 2.0 } else { 1.3 }, true);
                    }
                }
                if let Some(top) = corners.iter().flatten().copied().reduce(|a, p| if p.y < a.y { p } else { a }) {
                    self.halo_text(cx, top + dvec2(6.0, -18.0), &words, color);
                }
            }
        }
        if let (Some(under), None, None) = (&picks.under, &picks.other, &picks.menu) {
            if under.view == (*state.view.camera(self.pane), rect) {
                self.small_text(cx, under.at + dvec2(16.0, 14.0), &under.words, self.color_measure);
            }
        }
        self.other_rows.clear();
        if let Some(other) = &picks.other {
            const ROW: f64 = 20.0;
            let width = other.rows.iter().map(|r| r.kind.len() + r.label.len() + 2).max().unwrap_or(8) as f64 * 6.5 + 28.0;
            let top = other.at + dvec2(12.0, 8.0);
            let height = ROW * other.rows.len().max(1) as f64 + 8.0;
            self.draw_panel.draw_abs(cx, Rect { pos: top, size: dvec2(width, height) });
            if other.rows.is_empty() {
                self.small_text(cx, top + dvec2(10.0, 7.0), "nothing along the pointer", self.color_measure);
            }
            for (i, row) in other.rows.iter().enumerate() {
                let rect = Rect { pos: top + dvec2(0.0, 4.0 + ROW * i as f64), size: dvec2(width, ROW) };
                let hover = other.hover == Some(i);
                if hover {
                    self.marker(cx, rect.pos + dvec2(9.0, ROW * 0.5), 3.0, self.color_accent, DISC, false);
                }
                self.small_text(cx, rect.pos + dvec2(18.0, 4.0), &format!("{}  {}", row.kind, row.label), if hover { vec4(1.0, 1.0, 1.0, 1.0) } else { self.color_measure });
                self.other_rows.push(rect);
            }
        }
        if let Some(menu) = picks.menu.as_ref().filter(|m| m.pane == self.pane) {
            self.draw_menu(cx, menu, rect, halo);
        }
    }

    fn draw_menu(&mut self, cx: &mut Cx2d, menu: &picks::MarkingMenu, view: Rect, halo: Vec4f) {
        let dark = vec4(0.06, 0.06, 0.07, 0.95);
        let white = vec4(1.0, 1.0, 1.0, 1.0);
        let previous_panel = self.draw_panel.color;
        if menu.stroke {
            self.seg(cx, menu.at, menu.pointer, halo, 4.4, false);
            self.seg(cx, menu.at, menu.pointer, self.color_accent, 2.2, false);
        }
        self.marker(cx, menu.at, picks::HUB_RADIUS + 2.0, halo, DISC, true);
        self.marker(cx, menu.at, picks::HUB_RADIUS, self.color_accent, DISC, true);
        for (slot, rect, lines) in &menu.layout.pills {
            let hot = menu.hot == Some(*slot);
            let panel = if hot { self.color_accent } else { dark };
            let disabled = menu.items.iter().any(|item| item.slot == *slot && item.disabled);
            let text = if disabled { vec4(0.55, 0.55, 0.55, 1.0) } else if hot { white } else { self.color_measure };
            self.menu_pill(cx, *rect, lines, panel, text);
        }
        // what the stroke will do: its words beside the hot pill, away from the hub
        if let Some(item) = menu.items.iter().find(|item| Some(item.slot) == menu.hot) {
            if let Some((rect, lines)) = picks::chip_rect(view, &menu.layout, item.slot, menu.pointer, &item.words) {
                self.menu_pill(cx, rect, &lines, dark, white);
            }
        }
        self.draw_panel.color = previous_panel;
        if self.menu_shown.as_ref().is_none_or(|shown| !Arc::ptr_eq(&shown.layout, &menu.layout)) {
            let hub = Rect { pos: menu.at, size: DVec2::default() }.add_margin(DVec2::all(picks::HUB_RADIUS));
            let bounds = menu.layout.pills.iter().fold(hub, |bounds, (_, rect, _)| bounds.hull(*rect));
            self.menu_shown = Some(DrawnMenu { bounds, layout: menu.layout.clone(), items: menu.items.clone() });
        }
    }

    fn menu_pill(&mut self, cx: &mut Cx2d, rect: Rect, lines: &[String], panel: Vec4f, text: Vec4f) {
        self.draw_panel.color = panel;
        self.draw_panel.draw_abs(cx, rect);
        for (i, line) in lines.iter().enumerate() {
            self.small_text(cx, rect.pos + dvec2(9.0, 4.0 + i as f64 * 14.0), line, text);
        }
    }

    /// Select Other is the pointer's while it is open: a row under it lights up, a press picks
    /// that row or closes the list, Esc closes it.
    fn other_event(&mut self, cx: &mut Cx, event: &Event, state: &mut AppState) {
        use crate::tools::picks::PickAction;
        // a press on a pick command's box runs it
        if let Event::MouseDown(e) = event {
            if crate::tools::picks::active(state) {
                let controls = picks::strip_view(state, self.pane, self.area.rect(cx));
                if let Some((.., command)) = crate::tools::picks::commands(controls, &state.picks.list).into_iter().find(|(r, ..)| r.contains(e.abs)) {
                    cx.action(crate::api::ShellAction::Command(command.into()));
                    return;
                }
            }
        }
        let Some(other) = &mut state.picks.other else { return };
        match event {
            Event::MouseMove(e) => {
                let hover = self.other_rows.iter().position(|r| r.contains(e.abs));
                if hover != other.hover {
                    other.hover = hover;
                    self.area.redraw(cx);
                }
            }
            Event::MouseDown(e) => cx.action(PickAction::Row(self.other_rows.iter().position(|r| r.contains(e.abs)))),
            Event::KeyDown(ke) if ke.key_code == KeyCode::Escape => cx.action(PickAction::Row(None)),
            _ => {}
        }
    }

    // ---- the marker's marks ---------------------------------------------

    /// The marks the person drew for the AI: the raw line while the worker resolves a stroke,
    /// then the stroke as it sits on the model (it turns with the camera and moves with its
    /// part), a halo under one bright line, a number beside each.
    fn draw_marks(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector, rect: Rect) {
        use crate::tools::annotate::{lines, COLOR, WIDTH};
        let halo = vec4(0.04, 0.04, 0.04, 0.85);
        for line in lines(state, proj, self.pane) {
            let points: Vec<DVec2> = line.points.iter().map(|p| *p + rect.pos).collect();
            if points.len() == 1 {
                self.marker(cx, points[0], WIDTH + 2.0, halo, DISC, false);
                self.marker(cx, points[0], WIDTH, COLOR, DISC, false);
            } else {
                self.polyline(cx, &points, line.closed, halo, WIDTH + 2.5, false);
                self.polyline(cx, &points, line.closed, COLOR, WIDTH, false);
            }
            if let Some(number) = line.number {
                let top = points.iter().fold(points[0], |a, p| if p.y < a.y { *p } else { a });
                self.halo_text(cx, top + dvec2(6.0, -18.0), &number.to_string(), COLOR);
            }
        }
    }

    // ---- primitives ---------------------------------------------------

    fn seg(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, color: Vec4f, width: f64, dash: bool) {
        self.seg_dash(cx, a, b, color, width, if dash { 9.0 } else { 0.0 })
    }

    /// `dash_px` is the dash period in points; 0 draws a solid line.
    fn seg_dash(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, color: Vec4f, width: f64, dash_px: f64) {
        let pad = width + 3.0;
        let pos = dvec2(a.x.min(b.x) - pad, a.y.min(b.y) - pad);
        let size = dvec2(
            (a.x - b.x).abs() + pad * 2.0,
            (a.y - b.y).abs() + pad * 2.0,
        );
        if size.x > 8000.0 || size.y > 8000.0 || !size.x.is_finite() || !size.y.is_finite() {
            return;
        }
        self.draw_line.color = color;
        self.draw_line.line_a = vec2((a.x - pos.x) as f32, (a.y - pos.y) as f32);
        self.draw_line.line_b = vec2((b.x - pos.x) as f32, (b.y - pos.y) as f32);
        self.draw_line.line_width = width as f32;
        self.draw_line.dash = dash_px as f32;
        self.draw_line.draw_abs(cx, Rect { pos, size });
    }

    fn marker(&mut self, cx: &mut Cx2d, p: DVec2, r: f64, color: Vec4f, shape: f32, hollow: bool) {
        if !p.x.is_finite() || !p.y.is_finite() {
            return;
        }
        self.draw_marker.color = color;
        self.draw_marker.shape = shape;
        self.draw_marker.hollow = if hollow { 1.0 } else { 0.0 };
        self.draw_marker.draw_abs(
            cx,
            Rect {
                pos: p - dvec2(r, r),
                size: dvec2(r * 2.0, r * 2.0),
            },
        );
    }

    /// Text with a dark halo, so it reads over any shading mode.
    fn halo_text(&mut self, cx: &mut Cx2d, p: DVec2, text: &str, color: Vec4f) {
        let dark = vec4(0.04, 0.04, 0.04, 0.9);
        self.draw_text.color = dark;
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            self.draw_text.draw_abs(cx, p + dvec2(dx, dy), text);
        }
        self.draw_text.color = color;
        self.draw_text.draw_abs(cx, p, text);
    }

    fn small_text(&mut self, cx: &mut Cx2d, p: DVec2, text: &str, color: Vec4f) {
        self.draw_text_small.color = color;
        self.draw_text_small.draw_abs(cx, p, text);
    }

    fn polyline(&mut self, cx: &mut Cx2d, pts: &[DVec2], closed: bool, color: Vec4f, width: f64, dash: bool) {
        if pts.len() < 2 {
            return;
        }
        for w in pts.windows(2) {
            self.seg(cx, w[0], w[1], color, width, dash);
        }
        if closed && pts.len() > 2 {
            self.seg(cx, pts[pts.len() - 1], pts[0], color, width, dash);
        }
    }

    // ---- a part's centre of mass (`params::mass_marked`) -----------------

    /// The centre of mass, a plumb line to the base it stands on and that base's outline: in
    /// the measure colour when it stands, the accent when it tips over.
    fn draw_mass(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector) {
        let Some(f) = crate::params::mass_marked(state) else { return };
        let at = |p: [f64; 3]| proj.project(vec3((p[0] * 1e-3) as f32, (p[1] * 1e-3) as f32, (p[2] * 1e-3) as f32));
        let colour = if f.stands { self.color_measure } else { self.color_accent };
        let base: Vec<DVec2> = f.base.iter().filter_map(|p| at(*p)).collect();
        if base.len() == f.base.len() {
            self.polyline(cx, &base, true, colour, 1.5, true);
        }
        if let (Some(c), Some(foot)) = (at(f.com), at(f.foot)) {
            self.seg(cx, c, foot, colour, 1.5, true);
            self.marker(cx, foot, 3.0, colour, DISC, false);
            self.marker(cx, c, 8.0, colour, DISC, true);
            self.marker(cx, c, 3.5, colour, DISC, false);
            self.halo_text(cx, c + dvec2(12.0, -9.0), "centre of mass", colour);
        }
    }

    // ---- the Mechanics joint check -------------------------------------

    fn draw_mech_check(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector, view: Rect) {
        let c = &state.mech.check;
        let colour = |k: MechMarkKind| match k {
            MechMarkKind::Region => vec4(0.55, 0.62, 0.75, 0.7),
            MechMarkKind::Gap => vec4(0.35, 0.8, 1.0, 1.0),
            MechMarkKind::Tack => vec4(0.95, 0.66, 0.15, 1.0),
            MechMarkKind::Stop => vec4(0.45, 0.85, 0.5, 1.0),
            MechMarkKind::Bind => vec4(1.0, 0.3, 0.28, 1.0),
        };
        let at = |p: [f32; 3]| proj.project(vec3(p[0], p[1], p[2]));
        for (pts, kind) in &c.lines {
            let on: Vec<DVec2> = pts.iter().filter_map(|p| at(*p)).collect();
            if on.len() == pts.len() {
                self.polyline(cx, &on, false, colour(*kind), if *kind == MechMarkKind::Region { 1.0 } else { 2.0 }, *kind == MechMarkKind::Region);
            }
        }
        // Each mark, then its label beside it where there is room in the view: the problems first,
        // then the gaps, the stops and the tacks. A label with no room on any side of its mark is left
        // out (the joint's row in the panel says it), so a crowd of joints never runs a column of
        // labels down past the piece.
        let shown: Vec<(DVec2, &MechMark)> = c.marks.iter().filter_map(|m| Some((at(m.at)?, m))).collect();
        for (p, m) in &shown {
            let k = colour(m.kind);
            let shape = match m.kind {
                MechMarkKind::Tack => SQUARE,
                MechMarkKind::Bind => DIAMOND,
                _ => DISC,
            };
            self.marker(cx, *p, 5.0, k, shape, false);
            self.marker(cx, *p, 8.0, k, shape, true);
        }
        let first = |k: MechMarkKind| match k {
            MechMarkKind::Bind => 0,
            MechMarkKind::Gap => 1,
            MechMarkKind::Stop => 2,
            MechMarkKind::Tack => 3,
            MechMarkKind::Region => 4,
        };
        let mut order: Vec<&(DVec2, &MechMark)> = shown.iter().filter(|(_, m)| !m.label.is_empty()).collect();
        order.sort_by_key(|(_, m)| first(m.kind));
        let inside = |r: &Rect| r.pos.x >= view.pos.x && r.pos.y >= view.pos.y && r.pos.x + r.size.x <= view.pos.x + view.size.x && r.pos.y + r.size.y <= view.pos.y + view.size.y;
        let mut placed: Vec<Rect> = Vec::new();
        for (p, m) in order {
            let size = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &m.label).size_in_lpxs;
            let scale = self.draw_text.font_scale as f64;
            let (w, h) = (size.width as f64 * scale, size.height as f64 * scale);
            // right of the mark, left of it, then above and below either side
            let sides = [dvec2(10.0, -h * 0.5), dvec2(-10.0 - w, -h * 0.5), dvec2(10.0, -h - 8.0), dvec2(10.0, 8.0), dvec2(-10.0 - w, -h - 8.0), dvec2(-10.0 - w, 8.0)];
            let room = sides.iter().map(|d| Rect { pos: *p + *d, size: dvec2(w, h) }).find(|r| inside(r) && !placed.iter().any(|o| o.intersects(*r)));
            if let Some(r) = room {
                placed.push(r);
                self.halo_text(cx, r.pos, &m.label, colour(m.kind));
            }
        }
    }

    // ---- the Force render mode ----------------------------------------

    fn kind_color(&self, k: ForceKind) -> Vec4f {
        match k {
            ForceKind::Fixed => self.color_fixed,
            ForceKind::Force => self.color_force,
            ForceKind::Pressure => self.color_pressure,
            ForceKind::Torque => self.color_torque,
        }
    }

    /// An arrow on screen from `a` to `b`, its head at `b`.
    fn arrow(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, color: Vec4f) {
        self.seg(cx, a, b, color, 2.5, false);
        let d = b - a;
        let l = d.length();
        if l < 1e-3 {
            return;
        }
        let u = d / l;
        let side = dvec2(-u.y, u.x);
        let head = 9.0f64.min(l * 0.5);
        self.seg(cx, b, b - u * head + side * head * 0.5, color, 2.5, false);
        self.seg(cx, b, b - u * head - side * head * 0.5, color, 2.5, false);
    }

    /// Over the studied part: each fixture's mark and each load's arrow (a force along its
    /// vector, a pressure into its face, a torque's turn about its axis) at its region's centre,
    /// the weakest point's ring, the largest stress's diamond, the colour key, and what the
    /// study is doing or needs.
    fn draw_force(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector, rect: Rect, line_at: DVec2) {
        let f = &state.force;
        let at = |p: [f64; 3]| proj.project(vec3((p[0] * 1e-3) as f32, (p[1] * 1e-3) as f32, (p[2] * 1e-3) as f32));
        let scene = f.scene.as_ref().filter(|_| state.heatmap_part().is_some());
        if let Some(s) = scene {
            let len = glyph_len(s);
            for (i, l) in f.loads.iter().enumerate() {
                let c = self.kind_color(l.kind);
                let Some(p) = at(l.centroid) else { continue };
                // a load a handle holds shows the value it is dragged to
                let value = f.held.filter(|(h, _)| *h == i).map_or(l.value, |(_, v)| v);
                match l.kind {
                    ForceKind::Fixed => {
                        self.marker(cx, p, 6.0, c, DISC, false);
                        self.marker(cx, p, 9.0, c, DISC, true);
                    }
                    ForceKind::Force => {
                        let m = (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt();
                        if m > 0.0 {
                            if let Some(t) = at(std::array::from_fn(|a| l.centroid[a] - value[a] / m * len)) {
                                self.arrow(cx, t, p, c);
                            }
                        }
                    }
                    ForceKind::Pressure => {
                        let s = if value[0] >= 0.0 { 1.0 } else { -1.0 };
                        if let Some(t) = at(std::array::from_fn(|a| l.centroid[a] + s * l.normal[a] * len * PRESSURE_LEN)) {
                            self.arrow(cx, t, p, c);
                        }
                    }
                    ForceKind::Torque => {
                        let (t, b) = torque_frame(l);
                        let r = TORQUE_RADIUS * len;
                        let turn = if value[0] >= 0.0 { 1.0 } else { -1.0 };
                        let pts: Vec<Option<DVec2>> = (0..=20).map(|k| {
                            let a = turn * k as f64 / 20.0 * TORQUE_SWEEP;
                            at(std::array::from_fn(|i| l.centroid[i] + r * (a.cos() * t[i] + a.sin() * b[i])))
                        }).collect();
                        for w in pts.windows(2) {
                            if let (Some(a), Some(b)) = (w[0], w[1]) {
                                self.seg(cx, a, b, c, 2.5, false);
                            }
                        }
                        if let (Some(a), Some(b)) = (pts[18], pts[20]) {
                            self.arrow(cx, a, b, c);
                        }
                    }
                }
            }
            if let Some((p, _)) = s.weakest.filter(|_| s.answer.is_some()) {
                if let Some(p) = at(p.map(|v| v as f64)) {
                    self.marker(cx, p, 7.0, vec4(1.0, 1.0, 1.0, 1.0), DISC, true);
                }
            }
            if let Some(p) = s.peak.filter(|_| s.answer.is_some()) {
                if let Some(p) = at(p.map(|v| v as f64)) {
                    self.marker(cx, p, 5.0, vec4(1.0, 1.0, 1.0, 1.0), DIAMOND, true);
                }
            }
            if let Some(answer) = &s.answer {
                self.draw_force_key(cx, rect, f, answer);
            }
        }
        // what the study is doing or still needs (what a click of the Force tool takes is the
        // pointer's floating tip)
        let line = match scene {
            _ if !f.note.is_empty() => f.note.clone(),
            Some(s) if s.answer.is_some() && f.status.starts_with("Done") => format!("{}; white ring: the lowest safety factor, diamond: the largest von Mises stress", f.field.label()),
            _ => f.headline(),
        };
        if !line.is_empty() {
            self.halo_text(cx, line_at, line.trim_end_matches('.'), self.color_force_text);
        }
    }

    /// The colour scale's key along the view's bottom.
    fn draw_force_key(&mut self, cx: &mut Cx2d, rect: Rect, f: &ForceView, answer: &ForceAnswer) {
        let n = 32;
        let w = (rect.size.x - 24.0).min(260.0);
        let y = rect.pos.y + rect.size.y - 40.0;
        let x0 = rect.pos.x + 16.0;
        for k in 0..n {
            let t = k as f64 / (n - 1) as f64;
            let c = match f.field {
                ForceField::Principal => crate::ui::force::diverge(t * 2.0 - 1.0),
                _ => crate::ui::force::ramp(t),
            };
            let a = dvec2(x0 + w * k as f64 / n as f64, y);
            self.seg(cx, a, a + dvec2(w / n as f64, 0.0), c, 6.0, false);
        }
        let k = f.field.index();
        let (left, right) = force_key_ends(f.field, answer.display.scale[k] as f64, answer.display.peak[k] as f64);
        let color = self.color_force_text;
        self.halo_text(cx, dvec2(x0, y - 20.0), &left, color);
        self.halo_text(cx, dvec2(x0 + w - 7.0 * right.len() as f64, y - 20.0), &right, color);
    }

    // ---- measurements --------------------------------------------------

    fn draw_feature_measurement(&mut self, cx: &mut Cx2d, m: &Measurement, units: &Units, proj: &ViewProjector, color: Vec4f) {
        let dimension;
        let line;
        if let Some(circle) = m.circle {
            let center = measure::world(circle.center);
            let normal = measure::world(circle.normal);
            let radial = m.points[0] - center;
            let u = (radial - normal * radial.dot(normal)).normalize();
            let v = Vec3f::cross(normal, u);
            let radius = circle.radius as f32;
            let mut last = None;
            for i in 0..=96 {
                let angle = std::f32::consts::TAU * i as f32 / 96.0;
                let p = proj.project(center + (u * angle.cos() + v * angle.sin()) * radius);
                if let (Some(a), Some(b)) = (last, p) { self.seg(cx, a, b, color, 1.6, false); }
                last = p;
            }
            dimension = [center, center + u * radius];
            line = &dimension[..];
        } else if m.gap.is_some_and(|gap| gap.parallel) {
            let normal = m.normals[0].normalize();
            let end = m.points[0] + normal * (m.points[1] - m.points[0]).dot(normal);
            if let (Some(a), Some(b)) = (proj.project(end), proj.project(m.points[1])) {
                self.seg(cx, a, b, color, 1.0, true);
            }
            dimension = [m.points[0], end];
            line = &dimension[..];
        } else {
            line = &m.points[..];
        }
        for pair in line.windows(2) {
            if let (Some(a), Some(b)) = (proj.project(pair[0]), proj.project(pair[1])) {
                self.seg(cx, a, b, color, 2.0, false);
            }
        }
        if let (Some(first), Some(last)) = (line.first(), line.last()) {
            for point in [first, last] {
                if let Some(p) = proj.project(*point) { self.marker(cx, p, 3.5, color, DISC, false); }
            }
            if let Some(p) = measure::label_position(m, proj) {
                self.halo_text(cx, p, &measure::measurement_label(m, units), color);
            }
        }
    }

    fn draw_fit(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector, view: Rect) {
        let at = |point| proj.project(super::transform::mm_to_world(point));
        session::with(|s| {
            let Some(Ok(fit)) = &s.fit.result else {
                if let Some(p) = s.fit.a.as_ref().and_then(|pick| at(pick.point)) {
                    self.marker(cx, p, 5.0, self.color_accent, DIAMOND, true);
                }
                return;
            };
            if let Some(bounds) = &fit.overlap_bounds {
                let corners = bounds.map(at);
                for i in 0..8 {
                    for bit in [1, 2, 4] {
                        if i & bit != 0 { continue; }
                        if let (Some(a), Some(b)) = (corners[i], corners[i | bit]) {
                            self.seg(cx, a, b, self.color_overlap, 2.0, false);
                        }
                    }
                }
            }
            let mut placed = Vec::<Rect>::new();
            for (i, gap) in fit.gaps.iter().enumerate() {
                let Some(gap) = gap else { continue };
                let (Some(a), Some(b)) = (at(gap.from), at(gap.to)) else { continue };
                let smallest = fit.smallest == Some(i);
                let color = if smallest { self.color_accent } else { self.color_measure };
                let width = if smallest { 2.5 } else { 1.5 };
                self.seg(cx, a, b, color, width, false);
                let along = at(std::array::from_fn(|k| gap.from[k] + gap.direction[k])).map_or(b - a, |p| p - a);
                let unit = if along.length() > 1e-6 { along / along.length() } else { dvec2(1.0, 0.0) };
                let tick = dvec2(-unit.y, unit.x) * 3.5;
                for p in [a, b] { self.seg(cx, p - tick, p + tick, color, width, false); }

                let label = super::fit::length(&state.units, gap.distance_mm);
                let size = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &label).size_in_lpxs;
                let size = dvec2(size.width as f64, size.height as f64) * self.draw_text.font_scale as f64;
                let mid = (a + b) * 0.5;
                let offset = unit * 12.0;
                let base = mid + offset - dvec2(if unit.x < 0.0 { size.x } else { 0.0 }, size.y * 0.5);
                let mut rect = Rect { pos: base, size };
                // Four gaps may project onto almost the same pixel at plate scale.
                for row in [0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0] {
                    let top = base + dvec2(0.0, row * (size.y + 3.0));
                    rect.pos = dvec2(top.x.clamp(view.pos.x, (view.pos.x + view.size.x - size.x).max(view.pos.x)),
                        top.y.clamp(view.pos.y, (view.pos.y + view.size.y - size.y).max(view.pos.y)));
                    if placed.iter().all(|other| !rect.intersects(*other)) { break; }
                }
                let end = dvec2(mid.x.clamp(rect.pos.x, rect.pos.x + size.x), mid.y.clamp(rect.pos.y, rect.pos.y + size.y));
                self.seg(cx, mid, end, color, 1.0, false);
                self.halo_text(cx, rect.pos, &label, color);
                placed.push(rect);
            }
        });
    }

    fn draw_measurements(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector) {
        let units = state.units;
        session::with(|session| {
            for (index, saved) in state.measurements.iter().enumerate() {
                let m = session.measure_drag.as_ref().filter(|d| d.index == index).map_or(saved, |d| &d.preview);
                let selected = state.selected_measure == Some(index);
                let color = if selected { self.color_accent } else { self.color_measure };
                if selected && state.tool == Tool::Select {
                    for point in &m.points {
                        if let Some(p) = proj.project(*point) { self.marker(cx, p, 6.0, color, SQUARE, false); }
                    }
                }
                if matches!(m.kind, MeasureKind::EdgeLength | MeasureKind::Radius | MeasureKind::FaceDistance) {
                    self.draw_feature_measurement(cx, m, &units, proj, color);
                    continue;
                }
                let pts: Vec<DVec2> = m.points.iter().filter_map(|p| proj.project(*p)).collect();
                if pts.len() < m.points.len() || pts.is_empty() {
                    continue;
                }
                let closed = m.kind == MeasureKind::Area;
                self.polyline(cx, &pts, closed, color, 1.6, false);
                for p in &pts {
                    self.marker(cx, *p, 3.5, color, DISC, false);
                }
                // Extension ticks on a distance, the way a dimension line reads.
                if m.kind == MeasureKind::Distance && pts.len() == 2 {
                    let d = pts[1] - pts[0];
                    let len = d.length();
                    if len > 1.0 {
                        let n = dvec2(-d.y / len, d.x / len) * 4.0;
                        self.seg(cx, pts[0] - n, pts[0] + n, color, 1.6, false);
                        self.seg(cx, pts[1] - n, pts[1] + n, color, 1.6, false);
                    }
                }
                if let Some(a) = measure::label_position(m, proj) {
                    self.halo_text(cx, a, &measure::measurement_label(m, &units), color);
                }
            }
        });
    }

    fn draw_draft(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector) {
        let (draft_points, draft_snaps, draft_normals, kind, preview) = session::with(|s| {
            (
                s.measure.points.clone(),
                s.measure.snaps.clone(),
                s.measure.normals.clone(),
                s.measure.kind,
                s.measure.preview,
            )
        });
        let color = self.color_measure;
        let pts: Vec<DVec2> = draft_points.iter().filter_map(|p| proj.project(*p)).collect();
        for (i, p) in pts.iter().enumerate() {
            let shape = draft_snaps
                .get(i)
                .map(|k| snap_shape(*k))
                .unwrap_or(DISC);
            self.marker(cx, *p, 4.0, color, shape, false);
        }
        self.polyline(cx, &pts, false, color, 1.6, false);

        // Rubber band from the last placed point to the snapped cursor.
        if let Some(h) = preview {
            if let Some(s) = proj.project(h.point) {
                if let Some(last) = pts.last().copied() {
                    self.seg(cx, last, s, color, 1.4, true);
                    if kind == MeasureKind::Area && pts.len() >= 2 {
                        self.seg(cx, s, pts[0], color, 1.0, true);
                    }
                    let units = state.units;
                    let points = draft_live(&draft_points, h.point, kind);
                    let mut normals = draft_normals.clone();
                    normals.push(h.normal.unwrap_or_default());
                    let value = measure::value_of(kind, &points, &normals);
                    let gap = if kind == MeasureKind::FaceDistance { measure::face_gap(&points, &normals) } else { None };
                    let text = match gap {
                        Some(gap) if !gap.parallel => units.format_angle(gap.angle_deg),
                        _ => measure::format(kind, value, &units),
                    };
                    if value.is_finite() && (kind != MeasureKind::FaceDistance || h.kind == SnapKind::Face) {
                        self.halo_text(cx, s + dvec2(10.0, -16.0), &text, color);
                    }
                }
                // The snap glyph: shape + one-letter tag says why it landed.
                self.marker(cx, s, 6.0, self.color_accent, snap_shape(h.kind), true);
                self.small_text(cx, s + dvec2(9.0, 1.0), snap::tag(h.kind), self.color_accent);
            }
        }
    }

    // ---- section --------------------------------------------------------

    fn draw_section(&mut self, cx: &mut Cx2d, state: &AppState, proj: &ViewProjector) {
        let sec = &state.scene_state.section;
        if !sec.enabled {
            return;
        }
        let bounds = state.scene.bounds;
        if aabb_is_empty(&bounds) {
            return;
        }
        let color = self.color_section;
        for p in sec.planes.iter().filter(|p| p.enabled) {
            let quad = section::plane_quad(&p.plane, &bounds);
            let pts: Vec<DVec2> = quad.iter().filter_map(|q| proj.project(*q)).collect();
            if pts.len() == 4 {
                self.polyline(cx, &pts, true, color, 1.4, true);
            }
            // Normal arrow: which half survives.
            let anchor = section::plane_anchor(&p.plane, &bounds);
            let n = section::normal(&p.plane).normalize();
            let tip = anchor + n * (aabb_radius(&bounds) * 0.18);
            if let (Some(a), Some(b)) = (proj.project(anchor), proj.project(tip)) {
                self.seg(cx, a, b, color, 1.6, false);
                let d = b - a;
                let l = d.length().max(1e-6);
                let u = d / l;
                let v = dvec2(-u.y, u.x);
                self.seg(cx, b, b - u * 7.0 + v * 4.0, color, 1.6, false);
                self.seg(cx, b, b - u * 7.0 - v * 4.0, color, 1.6, false);
            }
        }
        if let Some(b) = sec.boxed {
            let corners = section::box_corners(&b);
            let pts: Vec<Option<DVec2>> = corners.iter().map(|c| proj.project(*c)).collect();
            for (i, j) in section::BOX_EDGES {
                if let (Some(a), Some(b)) = (pts[i], pts[j]) {
                    self.seg(cx, a, b, color, 1.3, true);
                }
            }
        }
        // Handles.
        let hover = session::with(|s| s.section_hover);
        for (handle, world, _) in section::handles(sec, &bounds) {
            if let Some(s) = proj.project(world) {
                let on = hover == Some(handle);
                let c = if on { self.color_accent } else { color };
                self.marker(cx, s, if on { 7.0 } else { 5.5 }, c, SQUARE, !on);
            }
        }
    }

    // ---- animation -------------------------------------------------------

    /// Advance the section animate-in. Returns true while it still wants
    /// frames.
    fn animate(&mut self, cx: &mut Cx, dt: f32) -> bool {
        let mut wants = false;

        let step = session::with(|s| {
            let anim = s.section_anim.as_mut()?;
            anim.t = (anim.t + dt / anim.duration.max(1e-3)).min(1.0);
            let done = anim.t >= 1.0;
            let f = section::ease(anim.t);
            let next = section::lerp_section(&anim.from, &anim.to, f);
            if done {
                s.section_anim = None;
            }
            Some((next, done))
        });
        if let Some((next, done)) = step {
            cx.action(ShellAction::SetSection(next));
            wants |= !done;
        }
        wants
    }
}

/// Marker shape for a snap kind, so the glyph says what it caught.
fn snap_shape(kind: SnapKind) -> f32 {
    match kind {
        SnapKind::Vertex => SQUARE,
        SnapKind::EdgeMidpoint => DIAMOND,
        SnapKind::Edge => DIAMOND,
        SnapKind::Face => DISC,
        SnapKind::Ground => DISC,
    }
}

/// The point list a live value is computed from (draft + cursor).
fn draft_live(points: &[Vec3f], preview: Vec3f, _kind: MeasureKind) -> Vec<Vec3f> {
    let mut v = points.to_vec();
    v.push(preview);
    v
}

impl WidgetNode for FabToolOverlay {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
}

impl FabToolOverlay {
    pub(crate) fn handle_viewport_input(&mut self, cx: &mut Cx, input: &ViewportInput, state: &AppState) {
        if state.picks.menu.is_some() || state.tool != Tool::Select || !state.view.overlays.measurements { return; }
        if let ViewportInputKind::PointerMove { pos, buttons: 0, .. } = input.kind {
            let proj = ViewProjector::new(*state.view.camera(self.pane), input.rect);
            if measure::point_at(state, &proj, pos).is_some() { cx.set_cursor(MouseCursor::Hand); }
        }
    }
}

impl Widget for FabToolOverlay {
    /// The plate notice, as a status the way it reads, and whether the plate shows a drag's
    /// preview ("Plate preview"): what the view says in words and shows by its shape.
    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        if let Some((text, rect)) = &self.notice_shown {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::Status, bounds: makepad_platform::a11y::semantic_rect(*rect), ..Default::default() };
            makepad_platform::a11y::set_string(&mut node.name, text);
            out.push((1, None, node));
        }
        if self.preview_shown {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::Status, bounds: makepad_platform::a11y::semantic_rect(self.area.rect(cx)), ..Default::default() };
            makepad_platform::a11y::set_string(&mut node.name, "Plate preview");
            out.push((2, None, node));
        }
        if let Some(menu) = &self.menu_shown {
            const MENU: u64 = 3;
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Menu, bounds: makepad_platform::a11y::semantic_rect(menu.bounds),
                set_size: menu.layout.pills.len() as u32, ..Default::default()
            };
            makepad_platform::a11y::set_string(&mut node.name, "Marking menu");
            out.push((MENU, None, node));
            for (i, (slot, rect, _)) in menu.layout.pills.iter().enumerate() {
                let Some(item) = menu.items.iter().find(|item| item.slot == *slot) else { continue };
                let mut node = a11y_tree::Node {
                    role: a11y_tree::Role::MenuItem, bounds: makepad_platform::a11y::semantic_rect(*rect),
                    pos_in_set: i as u32 + 1, ..Default::default()
                };
                node.states.set(a11y_tree::States::DISABLED, item.disabled);
                makepad_platform::a11y::set_string(&mut node.name, &item.label);
                makepad_platform::a11y::set_string(&mut node.description, &item.words);
                out.push((MENU + 1 + *slot as u64, Some(MENU), node));
            }
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Some(ne) = self.next_frame.is_event(event) {
            let dt = if self.last_time > 0.0 {
                (ne.time - self.last_time).clamp(0.0, 0.25) as f32
            } else {
                1.0 / 60.0
            };
            self.last_time = ne.time;
            // The brush's size readout is on for a moment: redraw to show it, once more to clear it.
            let readout = Cx::time_now() - session::with(|s| s.paint.size_at.max(s.sculpt.size_at)) < crate::tools::paint::SIZE_READOUT_SECS;
            if readout || self.readout_shown { self.area.redraw(cx); }
            self.readout_shown = readout;
            if self.animate(cx, dt) || readout {
                self.next_frame = cx.new_next_frame();
            } else {
                self.last_time = 0.0;
            }
        }
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        if self.notice_revision != state.ui.notices.revision {
            self.area.redraw(cx);
        }
        // In Mechanics the pointer belongs to its picker.
        if state.ui.in_mode(PlateMode::Mechanics) || state.picks.menu.is_some() { return; }
        self.other_event(cx, event, state);
        if state.tool != Tool::Select || !state.view.overlays.measurements { return; }
        let proj = ViewProjector::new(*state.view.camera(self.pane), self.area.rect(cx));
        let dragging = session::with(|s| s.measure_drag.is_some());
        if let Event::KeyDown(ke) = event {
            if dragging && ke.key_code == KeyCode::Escape {
                session::with(|s| { s.measure_drag = None; s.snap_cursor = None; });
                // Measurement drafts and snap feedback live in the viewport overlay; commits update the tree.
                crate::ui::redraw(cx, &[ids!(tool_overlay)]);
                return;
            }
        }
        // Like shape handles, only capture a press that actually hits a handle.
        // The viewport's primary-button navigation never sees an endpoint drag.
        if matches!(event, Event::MouseMove(_)) && !dragging { return; }
        if let Event::MouseDown(e) = event {
            if !e.button.is_primary() || measure::point_at(state, &proj, e.abs).is_none() { return; }
        }
        if let Event::TouchUpdate(e) = event {
            if !dragging && !e.touches.iter().any(|t| measure::point_at(state, &proj, t.abs).is_some()) { return; }
        }
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                if let Some((index, point)) = measure::point_at(state, &proj, fe.abs) {
                    cx.set_key_focus(self.area);
                    state.ui.set_plate_key(self.pane, self.area);
                    state.view.pressed_plate = self.pane;
                    measure::start_drag(state, &proj, fe.abs, index, point);
                    cx.set_cursor(MouseCursor::Grabbing);
                    // Measurement drafts and snap feedback live in the viewport overlay; commits update the tree.
                    crate::ui::redraw(cx, &[ids!(tool_overlay)]);
                }
            }
            Hit::FingerMove(fe) if dragging => {
                measure::drag_to(state, &proj, fe.abs);
                cx.set_cursor(MouseCursor::Grabbing);
                // Measurement drafts and snap feedback live in the viewport overlay; commits update the tree.
                crate::ui::redraw(cx, &[ids!(tool_overlay)]);
            }
            Hit::FingerUp(fe) if dragging => {
                if fe.cancelled {
                    session::with(|s| { s.measure_drag = None; s.snap_cursor = None; });
                } else {
                    measure::drag_to(state, &proj, fe.abs);
                    measure::finish_drag(cx);
                }
                // Measurement drafts and snap feedback live in the viewport overlay; commits update the tree.
                crate::ui::redraw(cx, &[ids!(tool_overlay)]);
            }
            Hit::KeyDown(ke) if !ke.is_repeat && matches!(ke.key_code, KeyCode::Delete | KeyCode::Backspace) => {
                super::delete_measurement(cx, state, ke.key_code);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.layout);
        let rect = cx.turtle().rect();
        let mut wants_frames = false;
        self.notice_shown = None;
        self.preview_shown = false;
        let menu = scope.data.get_mut::<AppState>().filter(|s| picks::active(s)).and_then(|s| s.picks.menu.as_mut())
            .filter(|m| m.pane == self.pane && rect.size.x > 2.0 && rect.size.y > 2.0);
        if let Some(menu) = menu {
            menu.fit(rect);
        } else {
            self.menu_shown = None;
        }
        if rect.size.x > 2.0 && rect.size.y > 2.0 {
            if let Some(state) = scope.data.get_mut::<AppState>() {
                let state: &AppState = state;
                let vs = &state.view;
                let proj = ViewProjector::new(*vs.camera(self.pane), rect);
                let mechanics = state.ui.in_mode(PlateMode::Mechanics);
                let left = RAIL_RIGHT + if strip_open(state) { STRIP_WIDTH } else { 0.0 };
                let full = rect.size.x - left - 16.0;
                let beside = rect.size.x - left - if vs.overlays.nav_gizmo { NAV_LEFT } else { 16.0 };
                // a narrow view leaves too little room left of the gizmo: the lines go under it
                let (line_width, mut line_y) = if vs.overlays.nav_gizmo && beside < LINE_MIN { (full, self.nav_top + NAV_BELOW) } else { (beside, 46.0) };
                if mechanics {
                    self.draw_mech_check(cx, state, &proj, rect);
                } else {
                    self.draw_marks(cx, state, &proj, rect);
                    self.draw_picks(cx, state, &proj, rect);
                    self.draw_mass(cx, state, &proj);
                    let readout = self.draw_brush(cx, state, &proj);
                    wants_frames = readout || session::with(|s| s.wants_frames());

                    if vs.overlays.measurements {
                        self.draw_measurements(cx, state, &proj);
                        if matches!(state.tool, Tool::Measure(_)) {
                            self.draw_draft(cx, state, &proj);
                        }
                    }
                    if vs.overlays.section_planes {
                        self.draw_section(cx, state, &proj);
                    }
                    // The tool's one-line feedback, under the view's top edge and right of the rail
                    // (and of a tool's strip beside it).
                    let fitting = super::fit::in_pane(state, self.pane);
                    let hint = if fitting {
                        self.draw_fit(cx, state, &proj, rect);
                        session::with(|s| s.fit.label(&state.units))
                    } else { session::with(|s| match (s.hint.is_empty(), s.draft.is_empty()) {
                        (true, false) if !crate::tools::picks::active(state) => format!("{} ({})", s.draft, crate::ui::keymap::shortcut(crate::ui::keymap::ShortcutId::Shape)),
                        (true, _) => s.draft.clone(),
                        (false, _) => s.hint.clone(),
                    }) };
                    // wrapped at the lines' width, so a long hint never runs under the gizmo
                    if (fitting || !hint.is_empty()) && line_width > 0.0 {
                        let walk = Walk::new(Size::Fixed(line_width), Size::fit()).with_abs_pos(rect.pos + dvec2(left, line_y));
                        self.draw_text_small.color = self.color_measure;
                        let drawn = self.draw_text_small.draw_walk_with_wrap(cx, walk, Align::default(), true, &hint);
                        line_y += (drawn.size.y + 4.0).max(20.0);
                    }
                }
                self.notice_revision = state.ui.notices.revision;
                self.preview_shown = state.plate.preview;
                if let Some(notice) = state.ui.notices.get(NoticeView::Plate) {
                    if line_width > 0.0 {
                        let walk = Walk::new(Size::Fixed(line_width), Size::fit()).with_abs_pos(rect.pos + dvec2(left, line_y));
                        self.draw_text_small.color = if notice.standing { self.color_notice_warn } else { self.color_measure };
                        let max_lines = self.draw_text_small.max_lines;
                        self.draw_text_small.max_lines = 3;
                        let drawn = self.draw_text_small.draw_walk_with_wrap(cx, walk, Align::default(), true, &notice.text);
                        self.draw_text_small.max_lines = max_lines;
                        self.notice_shown = Some((notice.text.clone(), Rect { pos: rect.pos + dvec2(left, line_y), size: drawn.size }));
                        line_y += (drawn.size.y + 4.0).max(20.0);
                    }
                }
                // The Force render mode's loads, key and state (whatever the tool)
                if !mechanics && state.force_shown() {
                    self.draw_force(cx, state, &proj, rect, rect.pos + dvec2(left, line_y));
                }
            }
        }
        let old_area = self.area;
        cx.end_turtle_with_area(&mut self.area);
        if let Some(state) = scope.data.get_mut::<AppState>() {
            state.ui.refresh_plate_keys(old_area, self.area);
        }
        if wants_frames {
            self.next_frame = cx.new_next_frame();
        }
        DrawStep::done()
    }
}

/// How long a load's arrow is drawn (mm): a share of the part's largest extent.
pub fn glyph_len(s: &ForceScene) -> f64 {
    0.18 * (0..3).map(|a| (s.hi[a] - s.lo[a]) as f64).fold(0.0, f64::max)
}

/// A pressure's arrow, as a share of [`glyph_len`].
pub const PRESSURE_LEN: f64 = 0.6;
/// A torque's arc radius, as a share of [`glyph_len`].
pub const TORQUE_RADIUS: f64 = 0.08 / 0.18;
/// How far round a torque's arc goes (radians); its head shows which way it turns.
pub const TORQUE_SWEEP: f64 = 1.5 * std::f64::consts::PI;

/// The plane a torque's arc lies in: two unit vectors square to its axis (the hole's, else the
/// face's normal).
pub fn torque_frame(l: &ForceLoad) -> ([f64; 3], [f64; 3]) {
    let axis = l.axis.unwrap_or(l.normal);
    let o = if axis[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let t = cross(axis, o);
    let tl = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt().max(1e-12);
    let t = t.map(|v| v / tl);
    (t, cross(axis, t))
}

/// The colour key's two ends. The scale stops short of a concentrated corner so the rest of the
/// part reads: where the part goes past it, the key says so and how far.
fn force_key_ends(field: ForceField, top: f64, peak: f64) -> (String, String) {
    let unit = match field {
        ForceField::Displacement => "mm",
        _ => "MPa",
    };
    let past = peak > top * 1.005;
    let right = match past {
        true => format!("{top:.3}+ {unit}, largest {peak:.3}"),
        false => format!("{top:.3} {unit}"),
    };
    match field {
        ForceField::Safety => ("safety factor 4+".to_string(), "1 (fails)".to_string()),
        ForceField::Principal => (format!("-{top:.3}{} {unit}", if past { "+" } else { "" }), right),
        ForceField::VonMises | ForceField::Displacement => (format!("0.000 {unit}"), right),
    }
}
