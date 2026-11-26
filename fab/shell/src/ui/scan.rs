//! The Scan workspace: a headset scan as it happens. The panel (`FabScanPanel`) starts and stops
//! the capture and says what it is doing: the headset linked or not, what the headset waits for
//! (the wearer's permissions), the keyframes kept, the scans kept before. The view (`FabScanView`)
//! shows it: where each keyframe was taken from, the surface growing under them (what the newest
//! keyframes added in the accent colour), a timeline of the keyframes along the bottom. The scan
//! itself runs in the app; both show `AppState::scan`. The surface is drawn as oriented splats in
//! the view's own 3D pass (`DrawScanSplat`), one instance per point made off the UI thread; the
//! overlays are drawn over its picture with `Proj`.

use crate::api::*;
use crate::tools::overlay::{DrawToolLine, DrawToolMarker};
use makepad_widgets_core::makepad_platform::retained_instances::RetainedInstances;
use makepad_widgets_core::makepad_platform::thread::{Class, TaskHandle};
use makepad_widgets_core::*;
use std::collections::HashSet;
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*
    use mod.math.*
    use mod.shader.*

    // One scan point: a disc across its normal, drawn in the view's own 3D pass with a depth
    // buffer (the depth test hides what is behind; nothing is sorted). The camera is the pass's:
    // an orbit or a zoom changes its matrices and no instance. `at`: the point and the square's
    // width (m); `facing`: its normal and the keyframe that saw it first (-1: no accent); `tint`:
    // its colour. In the cameras' colours the picture carries its own light, and the surface only
    // dims a little as it turns away; in the plain tint the shape reads by its shading alone, so
    // it is lit as the viewport lights a model: a sun over the world and a sky that sum to one.
    mod.draw.DrawScanSplat = mod.std.set_type_default() do #(DrawScanSplat::script_shader(vm)){
        alpha_blend: false
        depth_write: true
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(mod.draw.DrawCallUniforms)
        draw_pass: uniform_buffer(mod.draw.DrawPassUniforms)
        draw_list: uniform_buffer(mod.draw.DrawListUniforms)
        geom: vertex_buffer(mod.geom.QuadVertex, mod.geom.QuadGeom)
        // x = the newest keyframe, y = 1: the scan is under way (what the newest keyframes
        // added shows in the accent), z = how many keyframes back it shows
        u_new: uniform(vec4(0.0, 0.0, 3.0, 0.0))
        u_accent: uniform(vec4(1.0, 1.0, 1.0, 1.0))
        // xy = the pass's size in points; z = the least radius of a disc on the screen
        // (pixels), so a far splat is never lost between pixels; w = 1: the plain tint
        u_px: uniform(vec4(1.0, 1.0, 0.75, 0.0))
        lit_color: varying(vec3f)
        across: varying(vec2f)

        vertex: fn() {
            let n = normalize(self.facing.xyz)
            var a = vec3(0.0, 1.0, 0.0)
            if abs(n.y) > 0.9 {
                a = vec3(1.0, 0.0, 0.0)
            }
            let t = normalize(cross(a, n))
            let b = cross(n, t)
            // points a metre on the screen: the projection's x scale is 2 / the width in metres
            let across = self.draw_pass.camera_projection * vec4(1.0, 0.0, 0.0, 0.0)
            let per_metre = across.x * self.u_px.x * 0.5 * self.draw_pass.dpi_factor
            // (discs a point's width apart cover the surface at 0.71 of it: a little over)
            let half = max(0.78 * self.at.w, self.u_px.z / max(per_metre, 0.000001))
            let c = self.geom.pos * 2.0 - vec2(1.0, 1.0)
            let world = self.at.xyz + (t * c.x + b * c.y) * half
            var view_pos = self.draw_pass.camera_view * vec4(world.x, world.y, world.z, 1.0)
            // a base point sits three of its widths further along the ray (the same place on the
            // screen, only deeper): the base lies a cell or two off the surface where the detail
            // is not in yet, and where its disc and the detail overlap the detail is in front
            if self.facing.w >= 0.0 {
                view_pos = vec4(view_pos.xyz + normalize(view_pos.xyz) * 3.0 * self.at.w, 1.0)
            }
            let nv = (self.draw_pass.camera_view * vec4(n.x, n.y, n.z, 0.0)).xyz
            let light = normalize(vec3(-0.35, 0.7, 0.55))
            var lit = 0.55 + 0.45 * (0.3 + 0.7 * abs(dot(nv, light)))
            if self.u_px.w > 0.5 {
                // (a point's normal is of the surface, either way round: the side that faces the viewer)
                var facing = n
                if nv.z < 0.0 {
                    facing = n * (0.0 - 1.0)
                }
                lit = 0.28 + 0.72 * max(dot(facing, normalize(vec3(0.35, 0.8, 0.45))), 0.0)
            }
            var color = self.tint.xyz * lit
            let age = self.u_new.x - self.facing.w
            if self.facing.w >= 0.0 && self.u_new.y > 0.5 && age < self.u_new.z {
                color = mix(self.u_accent.xyz, color, 0.25 + 0.25 * age)
            }
            self.lit_color = color
            self.across = c
            self.vertex_pos = self.draw_pass.camera_projection * view_pos
        }

        pixel: fn() {
            if dot(self.across, self.across) > 1.0 {
                discard()
            }
            return vec4(self.lit_color, 1.0)
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // The 3D pass's picture, over the view's background: nothing where no splat is.
    mod.draw.DrawScanComposite = mod.std.set_type_default() do #(DrawScanComposite::script_shader(vm)){
        ..mod.draw.DrawQuad
        scan_texture: texture_2d(float)
        pixel: fn() {
            return Pal.premul(self.scan_texture.sample(self.pos))
        }
    }

    let Seg = mod.widgets.FabSegmentTab{ height: 22 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} }
    let Segs = View{ width: Fill height: Fit flow: Right{wrap: true} spacing: 2 }
    let Heading = mod.widgets.FabHeaderLabel{ width: Fill height: Fit margin: Inset{top: 4} }
    let Wrapped = mod.widgets.FabLabel{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
    let WrappedDim = mod.widgets.FabLabelDim{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
    let StatRow = View{
        width: Fill height: Fit flow: Right spacing: 8 visible: false
        stat := mod.widgets.FabLabelDim{ width: 120 height: Fit text: "" }
        value := Wrapped{}
    }
    let KeptRow = View{
        width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5} visible: false
        about := View{
            width: Fill height: Fit flow: Down spacing: 1
            name := Wrapped{}
            figures := WrappedDim{}
        }
        show := mod.widgets.FabButton{ text: "Show" height: 22 }
    }

    mod.widgets.FabScanPanelBase = #(FabScanPanel::register_widget(vm))
    mod.widgets.FabScanPanel = set_type_default() do mod.widgets.FabScanPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        body := mod.widgets.FabScroll{
            flow: Down padding: 10 spacing: 6
            WrappedDim{ text: "Put the headset on, link it to Fab and walk slowly round what you scan. Fab keeps a keyframe every few centimetres and builds the surface as you go." }
            run_row := View{
                width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5} margin: Inset{top: 6}
                go := mod.widgets.FabButtonAccent{ text: "Start scan" height: 24 padding: Inset{left: 14 right: 14} }
                clear := mod.widgets.FabButton{ text: "Clear" height: 24 }
            }
            status := Wrapped{}
            says := WrappedDim{}
            r0 := StatRow{} r1 := StatRow{} r2 := StatRow{} r3 := StatRow{} r4 := StatRow{} r5 := StatRow{} r6 := StatRow{} r7 := StatRow{}
            Heading{ text: "Colour" }
            colours := Segs{ colour_on := Seg{ text: "On" } colour_off := Seg{ text: "Off" } }
            WrappedDim{ text: "The scan in the colours the headset's cameras saw, or in a plain tint. Kept for the next start." }
            Heading{ text: "Cameras" }
            cameras := Segs{ cameras_on := Seg{ text: "On" } cameras_off := Seg{ text: "Off" } }
            WrappedDim{ text: "Where each keyframe was taken from: a dot each along the walk, and the newest one's view. Off shows the scan alone." }
            Heading{ text: "While you scan" }
            WrappedDim{ text: "A red dot in the headset means its cameras are on. Hold the headset about 40 cm from what you scan: steps of 2 mm show. From 70 cm a 4 mm step still shows, a little low; from a metre or more only shapes of centimetres do. Turn slowly: a blurred picture is skipped. Shiny and plain surfaces scan coarsely; a matte spray or a pattern on them gives the detail." }
            Heading{ text: "Kept scans" }
            kept_note := WrappedDim{}
            k0 := KeptRow{} k1 := KeptRow{} k2 := KeptRow{} k3 := KeptRow{}
            k4 := KeptRow{} k5 := KeptRow{} k6 := KeptRow{} k7 := KeptRow{}
        }
    }

    mod.widgets.FabScanViewBase = #(FabScanView::register_widget(vm))
    mod.widgets.FabScanView = set_type_default() do mod.widgets.FabScanViewBase{
        width: Fill
        height: Fill
        draw_bg +: { color: fab.color_vp_bg_bottom }
        draw_back +: { color: #x00000099 }
        draw_line: mod.draw.DrawToolLine{}
        draw_dot: mod.draw.DrawToolMarker{}
        draw_splat: mod.draw.DrawScanSplat{}
        draw_comp: mod.draw.DrawScanComposite{}
        draw_text: mod.draw.DrawText{
            text_style: theme.font_regular{ font_size: fab.font_size_small }
            color: fab.color_text_dim
        }
        draw_title: mod.draw.DrawText{
            text_style: theme.font_bold{ font_size: fab.font_size_small }
            color: fab.color_text_active
        }
        color_surface: #xc9c6c0
        color_detail: #xf4ecd8
        color_new: fab.color_accent
        color_camera: #x39c5ff
        color_path: #xf0f0f0
        color_text: #xd8d8d8
        color_failed: #xe5704a
    }
}

/// How many figure rows and kept-scan rows the panel has.
pub const STAT_ROWS: usize = 8;
pub const KEPT_ROWS: usize = 8;

const STATS: [&[LiveId]; STAT_ROWS] = [ids!(r0), ids!(r1), ids!(r2), ids!(r3), ids!(r4), ids!(r5), ids!(r6), ids!(r7)];
const KEPT: [&[LiveId]; KEPT_ROWS] = [ids!(k0), ids!(k1), ids!(k2), ids!(k3), ids!(k4), ids!(k5), ids!(k6), ids!(k7)];

#[derive(Script, ScriptHook, Widget)]
pub struct FabScanPanel {
    #[deref]
    view: View,
}

impl Widget for FabScanPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Scan");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if scope.data.get::<AppState>().is_some_and(|s| !s.ui.on_screen(crate::ui::editors::Editor::Scan)) {
            // off screen: none of its controls can be pressed
            return;
        }
        let Some(phase) = scope.data.get::<AppState>().map(|s| s.scan.phase) else { return };
        let v = &self.view;
        if let Some(i) = v.radio_button_set(cx, ids_array!(colour_on, colour_off)).selected(cx, actions) {
            cx.action(ScanAction::Colour(i == 0));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(cameras_on, cameras_off)).selected(cx, actions) {
            cx.action(ScanAction::Cameras(i == 0));
        }
        if v.button(cx, ids!(go)).clicked(actions) {
            cx.action(if phase.busy() { ScanAction::Stop } else { ScanAction::Start });
        }
        if v.button(cx, ids!(clear)).clicked(actions) {
            cx.action(ScanAction::Clear);
        }
        for (i, id) in KEPT.iter().enumerate() {
            if v.view(cx, id).button(cx, ids!(show)).clicked(actions) {
                cx.action(ScanAction::Show(i));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(s) = scope.data.get::<AppState>() {
            let a = &s.scan;
            for (ids, on) in [([ids!(colour_on), ids!(colour_off)], s.ui.scan_colour), ([ids!(cameras_on), ids!(cameras_off)], s.ui.scan_cameras)] {
                for (i, id) in ids.into_iter().enumerate() {
                    let r = self.view.radio_button(cx, id);
                    if r.active(cx) != ((i == 0) == on) {
                        r.set_active(cx, (i == 0) == on, Animate::No);
                    }
                }
            }
            self.view.button(cx, ids!(go)).set_text(cx, if a.phase.busy() { "Stop" } else { "Start scan" });
            self.view.widget(cx, ids!(clear)).set_disabled(cx, a.phase.busy() || (a.keyframes.is_empty() && a.surface.is_empty()));
            self.view.label(cx, ids!(status)).set_text(cx, &a.status);
            self.view.label(cx, ids!(says)).set_text(cx, &a.headset_says);
            for (i, id) in STATS.iter().enumerate() {
                let row = self.view.view(cx, id);
                row.set_visible(cx, i < a.stats.len());
                if let Some((name, value)) = a.stats.get(i) {
                    row.label(cx, ids!(stat)).set_text(cx, name);
                    row.label(cx, ids!(value)).set_text(cx, value);
                }
            }
            let note = match a.kept.len() {
                0 => "None yet. A scan is kept on this computer when it stops.".to_string(),
                n if n > KEPT_ROWS => format!("The newest {KEPT_ROWS} of {n}"),
                _ => String::new(),
            };
            self.view.label(cx, ids!(kept_note)).set_text(cx, &note);
            for (i, id) in KEPT.iter().enumerate() {
                let row = self.view.view(cx, id);
                let Some(k) = a.kept.get(i) else {
                    row.set_visible(cx, false);
                    continue;
                };
                row.set_visible(cx, true);
                row.label(cx, ids!(name)).set_text(cx, &k.name);
                row.label(cx, ids!(figures)).set_text(cx, &format!("{}, {:.0} MB", keyframes_in_words(k.keyframes), k.megabytes));
                row.widget(cx, ids!(show)).set_disabled(cx, a.phase.busy());
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

// ---- the scan view ---------------------------------------------------------

/// An orthographic view of the headset's space (y up, metres): `c` at `o` on screen, `s` points
/// a metre, looking along `-eye`.
#[derive(Clone, Copy)]
struct Proj {
    c: [f32; 3],
    s: f64,
    o: DVec2,
    right: [f32; 3],
    up: [f32; 3],
    eye: [f32; 3],
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl Proj {
    /// Orbit `yaw` about the up axis and `pitch` up from the horizon, fitting a sphere (`c`,
    /// `radius`) into `pane`, `zoom` times nearer.
    fn new(yaw: f64, pitch: f64, c: [f32; 3], radius: f32, pane: Rect, zoom: f64) -> Proj {
        let (sy, cy, sp, cp) = (yaw.sin() as f32, yaw.cos() as f32, pitch.sin() as f32, pitch.cos() as f32);
        let s = 0.5 * pane.size.x.min(pane.size.y) / f64::from(radius.max(1e-3)) * zoom;
        Proj { c, s, o: pane.pos + pane.size * 0.5, right: [cy, 0.0, -sy], up: [-sp * sy, cp, -sp * cy], eye: [cp * sy, sp, cp * cy] }
    }

    /// Where `p` shows and its depth (larger is nearer).
    fn at(&self, p: [f32; 3]) -> (DVec2, f32) {
        let d = [p[0] - self.c[0], p[1] - self.c[1], p[2] - self.c[2]];
        (dvec2(self.o.x + self.s * f64::from(dot3(self.right, d)), self.o.y - self.s * f64::from(dot3(self.up, d))), dot3(self.eye, d))
    }

    fn pt(&self, p: [f32; 3]) -> DVec2 {
        self.at(p).0
    }
}

/// The 3D pass's camera for a [`Proj`] over a pane of `size`: the same orthographic view as the
/// matrices, so a splat lands where `Proj::at` puts its point. The depth runs over `radius`
/// (metres) round the view's middle, clip z from 0 at the near plane to 1 at the far.
fn pass_camera(p: &Proj, size: DVec2, radius: f32) -> (Mat4f, Mat4f) {
    let reach = radius.max(1e-3) * 1.05;
    let back = 2.0 * reach;
    let (near, far) = (back - reach, back + reach);
    let (r, u, e) = (p.right, p.up, p.eye);
    let view = Mat4f {
        v: [r[0], u[0], e[0], 0.0, r[1], u[1], e[1], 0.0, r[2], u[2], e[2], 0.0, -dot3(r, p.c), -dot3(u, p.c), -dot3(e, p.c) - back, 1.0],
    };
    let (w, h) = (0.5 * size.x / p.s, 0.5 * size.y / p.s);
    let projection = Mat4f {
        v: [1.0 / w as f32, 0.0, 0.0, 0.0, 0.0, 1.0 / h as f32, 0.0, 0.0, 0.0, 0.0, 1.0 / (near - far), 0.0, 0.0, 0.0, near / (near - far), 1.0],
    };
    (view, projection)
}

/// One scan point's draw instance: `at` (point, square width), `facing` (normal, the keyframe that
/// saw it first, -1: none) and `tint` (colour, 1).
const SPLAT_SLOTS: usize = 12;

/// The shader of one scan point (see `DrawScanSplat` in the script above).
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawScanSplat {
    #[deref]
    draw_vars: DrawVars,
    #[live]
    at: Vec4f,
    #[live]
    facing: Vec4f,
    #[live]
    tint: Vec4f,
}

/// The 3D pass's picture into the view.
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawScanComposite {
    #[deref]
    draw_super: DrawQuad,
}

/// The points the splats were made from. The scan hands out new `Arc`s when its points change, so
/// the same pointers are the same points.
#[derive(Clone)]
struct Sources {
    surface: Arc<Vec<ScanPoint>>,
    detail: Arc<Vec<ScanPoint>>,
    covered: Arc<HashSet<[i32; 3]>>,
}

impl Sources {
    fn of(scan: &ScanView) -> Sources {
        Sources { surface: scan.surface.clone(), detail: scan.detail.clone(), covered: scan.covered.clone() }
    }

    fn same(&self, other: &Sources) -> bool {
        Arc::ptr_eq(&self.surface, &other.surface) && Arc::ptr_eq(&self.detail, &other.detail) && Arc::ptr_eq(&self.covered, &other.covered)
    }
}

/// The instances of a set of points, made off the UI thread, and the sphere that holds them.
struct Built {
    from: Sources,
    colour: bool,
    instances: Option<RetainedInstances>,
    count: usize,
    centre: [f32; 3],
    radius: f32,
}

/// Every point of the surface and the detail as a draw instance: the detail, and the base points
/// the detail does not stand in for (`covered`: the base's 10 mm cells). Each is in its own colour
/// when `colour`, else in the plain tint of its kind (`base`, `fine`).
fn build_splats(from: Sources, colour: bool, base: [f32; 3], fine: [f32; 3]) -> Built {
    let mut data: Vec<f32> = Vec::with_capacity((from.surface.len() + from.detail.len()) * SPLAT_SLOTS);
    let mut push = |s: &ScanPoint, born: f32, tint: [f32; 3]| {
        data.extend_from_slice(&[s.point[0], s.point[1], s.point[2], s.size, s.normal[0], s.normal[1], s.normal[2], born, tint[0], tint[1], tint[2], 1.0]);
    };
    for s in from.detail.iter() {
        push(s, -1.0, if colour { s.colour } else { fine });
    }
    for s in from.surface.iter() {
        let cell = s.point.map(|v| (v / 0.01).floor() as i32);
        // (the detail stands in for the base at its cells and the ones next to them: the base
        // lies up to a cell off the surface)
        if !from.covered.is_empty() && (-1..=1).any(|dx| (-1..=1).any(|dy| (-1..=1).any(|dz| from.covered.contains(&[cell[0] + dx, cell[1] + dy, cell[2] + dz])))) {
            continue;
        }
        push(s, s.first as f32, if colour { s.colour } else { base });
    }
    let (mut low, mut high) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for i in data.chunks_exact(SPLAT_SLOTS) {
        for k in 0..3 {
            low[k] = low[k].min(i[k]);
            high[k] = high[k].max(i[k]);
        }
    }
    let count = data.len() / SPLAT_SLOTS;
    if count == 0 {
        return Built { from, colour, instances: None, count: 0, centre: [0.0; 3], radius: 0.0 };
    }
    let centre: [f32; 3] = std::array::from_fn(|k| 0.5 * (low[k] + high[k]));
    let radius = data.chunks_exact(SPLAT_SLOTS).map(|i| (0..3).map(|k| (i[k] - centre[k]).powi(2)).sum::<f32>().sqrt()).fold(0.0, f32::max);
    let instances = RetainedInstances::new(SPLAT_SLOTS, Arc::from(data)).ok();
    Built { from, colour, instances, count, centre, radius }
}

fn alpha(c: Vec4f, a: f32) -> Vec4f {
    vec4(c.x, c.y, c.z, c.w * a)
}

/// How many keyframes back a part of the surface still shows as new.
const NEW_FOR: u32 = 3;
/// A camera's drawn view: a pyramid from its centre to a frame this far ahead and this wide
/// (metres), and the least the frame shows on screen (points): a room's keyframes stay seen, and
/// a head that turned in place shows as a fan.
const FRAME_AHEAD: f32 = 0.07;
const FRAME_HALF: f32 = 0.025;
const FRAME_LEAST: f64 = 6.0;
const START_YAW: f64 = 0.6;
const START_PITCH: f64 = 0.55;

/// What the wearer kept mid-view: where the cameras' axes come nearest together, and how far
/// from it they stood (the median). None while the cameras do not look at one place: under three
/// keyframes, a head that turned where it stood (its axes meet at the head), axes side by side.
fn looked_at(scan: &ScanView) -> Option<([f32; 3], f32)> {
    let cameras: Vec<&[[f32; 4]; 3]> = scan.keyframes.iter().flat_map(|k| k.cameras.iter()).collect();
    if scan.keyframes.len() < 3 {
        return None;
    }
    // least squares: the point p with the least summed squared distance to every axis
    let (mut a, mut b) = ([[0.0f64; 3]; 3], [0.0f64; 3]);
    for m in &cameras {
        let (c, d) = ([m[0][3], m[1][3], m[2][3]].map(f64::from), [m[0][2], m[1][2], m[2][2]].map(f64::from));
        for i in 0..3 {
            for j in 0..3 {
                let w = f64::from(u8::from(i == j)) - d[i] * d[j];
                a[i][j] += w;
                b[i] += w * c[j];
            }
        }
    }
    let det = |m: &[[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let whole = det(&a);
    if whole.abs() < 1e-6 * (cameras.len() as f64).powi(3) {
        return None;
    }
    let p: [f64; 3] = std::array::from_fn(|at| {
        let mut m = a;
        for (row, value) in m.iter_mut().zip(b) {
            row[at] = value;
        }
        det(&m) / whole
    });
    let mut away: Vec<f64> = scan.keyframes.iter().map(|k| (0..3).map(|i| (f64::from(k.head[i]) - p[i]).powi(2)).sum::<f64>().sqrt()).collect();
    away.sort_by(f64::total_cmp);
    let median = away[away.len() / 2];
    // (nearer than a forearm: the axes meet at the head itself; farther than a room: they do not meet)
    (0.15..4.0).contains(&median).then(|| (p.map(|v| v as f32), median as f32))
}

/// What the view fits. A walk round something: what the cameras looked at, out to where they
/// stood. Else the middle and the reach of the keyframes and of the surface near them (the
/// detail where there is some, else the base's finest points), not the room behind it.
fn extent(scan: &ScanView) -> Option<([f32; 3], f32)> {
    if let Some(found) = looked_at(scan) {
        return Some(found);
    }
    let (mut low, mut high) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    // (where the pictures gave detail, that is what is being scanned: the view fits it and the
    // cameras, and the base around it runs off the view's edges)
    let finest = scan.surface.iter().map(|s| s.size).fold(f32::INFINITY, f32::min);
    let near: Vec<[f32; 3]> = match scan.detail.is_empty() {
        true => scan.surface.iter().filter(|s| s.size <= finest).map(|s| s.point).collect(),
        false => scan.detail.iter().map(|s| s.point).collect(),
    };
    let stride = (near.len() / 4000).max(1);
    for p in near.into_iter().step_by(stride).chain(scan.keyframes.iter().map(|k| k.head)) {
        for k in 0..3 {
            low[k] = low[k].min(p[k]);
            high[k] = high[k].max(p[k]);
        }
    }
    if low[0] > high[0] {
        return None;
    }
    let c: [f32; 3] = std::array::from_fn(|k| 0.5 * (low[k] + high[k]));
    let r = (0..3).map(|k| (0.5 * (high[k] - low[k])).powi(2)).sum::<f32>().sqrt();
    Some((c, r.max(0.2)))
}

#[derive(Clone, Copy, PartialEq)]
struct SplatKey {
    instances: u64,
    newest: u32,
    busy: bool,
    size: DVec2,
    colour: bool,
    accent: [f32; 3],
}

#[derive(Clone, Copy, PartialEq)]
struct ScanFrameKey {
    view: Mat4f,
    projection: Mat4f,
    splats: SplatKey,
    dpi: f64,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabScanView {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    /// Behind the words over the scan: a light scan (the plain tint, a bright room) leaves them unread.
    #[live]
    draw_back: DrawColor,
    #[live]
    draw_line: DrawToolLine,
    #[live]
    draw_dot: DrawToolMarker,
    #[live]
    draw_splat: DrawScanSplat,
    #[live]
    draw_comp: DrawScanComposite,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_title: DrawText,
    #[live]
    color_surface: Vec4f,
    #[live]
    color_detail: Vec4f,
    #[live]
    color_new: Vec4f,
    #[live]
    color_camera: Vec4f,
    #[live]
    color_path: Vec4f,
    #[live]
    color_text: Vec4f,
    #[live]
    color_failed: Vec4f,
    /// The orbit: about the up axis, and up from the horizon (rad).
    #[rust(START_YAW)]
    yaw: f64,
    #[rust(START_PITCH)]
    pitch: f64,
    #[rust(1.0)]
    zoom: f64,
    #[rust]
    drag: Option<(DVec2, f64, f64)>,
    /// The splats' own pass: a picture of the surface, with a depth buffer, that the view shows.
    #[new]
    pass: DrawPass,
    #[new]
    pass_list: DrawList,
    /// The retained draw call. An orbit or zoom changes only the pass camera.
    #[new]
    splat_list: DrawList,
    #[new]
    color_texture: Texture,
    #[new]
    depth_texture: Texture,
    /// The instances drawn, and the job making the next set.
    #[rust]
    built: Option<Built>,
    #[rust]
    building: Option<(Sources, TaskHandle<Built>)>,
    /// The picture in the colour texture, including the inputs to its retained draw call.
    #[rust]
    recorded: Option<ScanFrameKey>,
    /// What the view fits, for the points and keyframes it was found for.
    #[rust]
    fitted: Option<(Sources, usize, Option<([f32; 3], f32)>)>,
}

impl FabScanView {
    fn seg(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, color: Vec4f, width: f64, dash: f64) {
        let pad = width + 3.0;
        let pos = dvec2(a.x.min(b.x) - pad, a.y.min(b.y) - pad);
        let size = dvec2((a.x - b.x).abs() + pad * 2.0, (a.y - b.y).abs() + pad * 2.0);
        if size.x > 8000.0 || size.y > 8000.0 || !size.x.is_finite() || !size.y.is_finite() {
            return;
        }
        self.draw_line.color = color;
        self.draw_line.line_a = vec2((a.x - pos.x) as f32, (a.y - pos.y) as f32);
        self.draw_line.line_b = vec2((b.x - pos.x) as f32, (b.y - pos.y) as f32);
        self.draw_line.line_width = width as f32;
        self.draw_line.dash = dash as f32;
        self.draw_line.draw_abs(cx, Rect { pos, size });
    }

    fn dot(&mut self, cx: &mut Cx2d, at: DVec2, r: f64, color: Vec4f, shape: f32) {
        if !at.x.is_finite() || !at.y.is_finite() {
            return;
        }
        self.draw_dot.color = color;
        self.draw_dot.shape = shape;
        self.draw_dot.hollow = 0.0;
        self.draw_dot.draw_abs(cx, Rect { pos: at - dvec2(r, r), size: dvec2(r * 2.0, r * 2.0) });
    }

    /// What the view fits, found again only when the points or the keyframes change.
    fn fit(&mut self, scan: &ScanView) -> Option<([f32; 3], f32)> {
        let from = Sources::of(scan);
        if let Some((was, keyframes, found)) = &self.fitted {
            if was.same(&from) && *keyframes == scan.keyframes.len() {
                return *found;
            }
        }
        let found = extent(scan);
        self.fitted = Some((from, scan.keyframes.len(), found));
        found
    }

    /// Take the finished instances in, and start making the set the scan's points now call for
    /// (one job at a time; what is drawn stays until the next is ready).
    fn sync_splats(&mut self, cx: &mut Cx, scan: &ScanView, colour: bool) {
        if self.building.as_ref().is_some_and(|(_, job)| job.is_finished()) {
            if let Some((from, mut job)) = self.building.take() {
                match job.try_take() {
                    Some(Ok(built)) => self.built = Some(built),
                    Some(Err(e)) => log!("scan: the splats of {} surface points were not made: {e:?}", from.surface.len() + from.detail.len()),
                    None => {}
                }
            }
        }
        let wanted = Sources::of(scan);
        let current = self.built.as_ref().map_or(wanted.surface.is_empty() && wanted.detail.is_empty(), |b| b.colour == colour && b.from.same(&wanted));
        if current || self.building.is_some() {
            return;
        }
        let (base, fine) = ([self.color_surface.x, self.color_surface.y, self.color_surface.z], [self.color_detail.x, self.color_detail.y, self.color_detail.z]);
        let from = wanted.clone();
        match cx.task_pool().submit_named(Class::Visible, "fab scan splats", move || {
            let began = std::time::Instant::now();
            let built = build_splats(wanted, colour, base, fine);
            log!("scan: {} splats made in {:.0} ms", built.count, began.elapsed().as_secs_f64() * 1000.0);
            SignalToUI::set_ui_signal();
            built
        }) {
            Ok(job) => self.building = Some((from, job)),
            Err(e) => log!("scan: the splats could not be started: {e:?}"),
        }
        if self.built.is_some() && scan.surface.is_empty() && scan.detail.is_empty() {
            self.built = None;
        }
    }

    /// Composite the last splat picture. Record the pass again when its pixels change.
    fn draw_splats(&mut self, cx: &mut Cx2d, p: &Proj, scan: &ScanView, colour: bool, rect: Rect) {
        self.sync_splats(cx.cx.cx, scan, colour);
        let Some(built) = &self.built else { return };
        let Some(instances) = &built.instances else { return };
        if self.recorded.is_none() {
            self.color_texture = Texture::new_with_format(cx.cx.cx, TextureFormat::RenderBGRAu8 { size: TextureSize::Auto, initial: true }).label(cx.cx.cx, "fab/scan-color");
            self.depth_texture = Texture::new_with_format(cx.cx.cx, TextureFormat::DepthD32 { size: TextureSize::Auto, initial: true }).label(cx.cx.cx, "fab/scan-depth");
            self.pass.set_color_texture(cx, &self.color_texture, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
            self.pass.set_depth_texture(cx, &self.depth_texture, DrawPassClearDepth::ClearWith(1.0));
            self.pass.set_keep_camera_matrix(cx.cx.cx, true);
            self.pass.set_pass_name(cx.cx.cx, "fab_scan");
        }
        cx.make_child_pass(&self.pass);
        let dpi = cx.get_delegated_dpi_factor(self.pass.draw_pass_id());
        let off = (0..3).map(|k| (built.centre[k] - p.c[k]).powi(2)).sum::<f32>().sqrt();
        let (view, projection) = pass_camera(p, rect.size, off + built.radius);
        let splats = SplatKey {
            instances: instances.id(), newest: scan.keyframes.len().saturating_sub(1) as u32, busy: scan.phase.busy(),
            size: rect.size, colour, accent: [self.color_new.x, self.color_new.y, self.color_new.z],
        };
        let key = ScanFrameKey { view, projection, splats, dpi };
        let changed = self.recorded != Some(key);
        if changed {
            self.pass.set_size(cx, rect.size);
            // Passing the delegated DPI preserves the panel rect when the child pass begins.
            cx.begin_pass(&self.pass, Some(dpi));
            self.pass.set_camera(cx, view, projection);
            self.pass_list.begin_maybe(cx, changed).expect_redraw();
            if self.splat_list.begin_maybe(cx, self.recorded.is_none_or(|k| k.splats != splats)).is_redrawing() {
                let vars = &mut self.draw_splat.draw_vars;
                let [r, g, b] = splats.accent;
                vars.set_uniform(cx, live_id!(u_new), &[splats.newest as f32, f32::from(u8::from(splats.busy)), NEW_FOR as f32, 0.0]);
                vars.set_uniform(cx, live_id!(u_accent), &[r, g, b, 1.0]);
                vars.set_uniform(cx, live_id!(u_px), &[splats.size.x as f32, splats.size.y as f32, 0.75, f32::from(u8::from(!splats.colour))]);
                vars.area = cx.add_retained_instances_count(vars, instances, built.count);
                self.splat_list.end(cx);
            }
            self.pass_list.end(cx);
            cx.end_pass(&self.pass);
            self.recorded = Some(key);
        }
        self.draw_comp.draw_super.draw_vars.set_texture(0, &self.color_texture);
        self.draw_comp.draw_super.draw_abs(cx, rect);
    }

    /// Where the keyframes were taken from: the head's path with a dot a keyframe, and the
    /// newest keyframe's cameras' views. Over the surface, each on a dark one: they show against it.
    fn draw_keyframes(&mut self, cx: &mut Cx2d, p: &Proj, scan: &ScanView) {
        let under = vec4(0.0, 0.0, 0.0, 0.55);
        let path = alpha(self.color_path, 0.5);
        for pair in scan.keyframes.windows(2) {
            self.seg(cx, p.pt(pair[0].head), p.pt(pair[1].head), path, 1.0, 3.0);
        }
        let last = scan.keyframes.len().saturating_sub(1);
        let grown = (FRAME_LEAST / (p.s * f64::from(FRAME_HALF))).max(1.0) as f32;
        for (i, keyframe) in scan.keyframes.iter().enumerate() {
            let newest = i == last && scan.phase.busy();
            let color = if newest { self.color_new } else { alpha(self.color_camera, 0.75) };
            let width = if newest { 2.0 } else { 1.0 };
            // (the view of the newest keyframe alone: every keyframe's would stand as a ring of
            // lines over what is being scanned; the others are their dots along the walk)
            for m in keyframe.cameras.iter().filter(|_| newest) {
                let centre = [m[0][3], m[1][3], m[2][3]];
                let corner = |x: f32, y: f32| -> [f32; 3] { std::array::from_fn(|k| centre[k] + grown * (m[k][0] * x * FRAME_HALF + m[k][1] * y * FRAME_HALF + m[k][2] * FRAME_AHEAD)) };
                let corners = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)].map(|c| p.pt(c));
                let at = p.pt(centre);
                for pass in 0..2 {
                    let (color, width) = if pass == 0 { (under, width + 2.0) } else { (color, width) };
                    for k in 0..4 {
                        self.seg(cx, corners[k], corners[(k + 1) % 4], color, width, 0.0);
                        self.seg(cx, at, corners[k], color, width, 0.0);
                    }
                }
            }
            self.dot(cx, p.pt(keyframe.head), if newest { 6.0 } else { 3.0 }, under, 0.0);
            self.dot(cx, p.pt(keyframe.head), if newest { 4.5 } else { 2.0 }, color, 0.0);
        }
    }

    /// The keyframes along the bottom, each where it came in the scan's time.
    fn draw_timeline(&mut self, cx: &mut Cx2d, rect: Rect, scan: &ScanView) {
        let (x0, x1, y) = (rect.pos.x + 14.0, rect.pos.x + rect.size.x - 14.0, rect.pos.y + rect.size.y - 18.0);
        if x1 - x0 < 40.0 {
            return;
        }
        self.draw_back.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y - 34.0), size: dvec2(rect.size.x, rect.pos.y + rect.size.y - (y - 34.0)) });
        self.seg(cx, dvec2(x0, y), dvec2(x1, y), alpha(self.color_text, 0.35), 1.0, 0.0);
        let length = scan.seconds.max(scan.keyframes.last().map_or(0.0, |k| k.at)).max(1.0);
        let last = scan.keyframes.len().saturating_sub(1);
        for (i, keyframe) in scan.keyframes.iter().enumerate() {
            let x = x0 + (x1 - x0) * (keyframe.at / length).clamp(0.0, 1.0);
            let newest = i == last && scan.phase.busy();
            let color = if newest { self.color_new } else if keyframe.depth { self.color_camera } else { alpha(self.color_camera, 0.4) };
            self.seg(cx, dvec2(x, y - if newest { 9.0 } else { 6.0 }), dvec2(x, y + 1.0), color, if newest { 2.5 } else { 1.5 }, 0.0);
        }
        let words = format!("{} in {:.0} s", keyframes_in_words(scan.keyframes.len()), length);
        self.draw_text.color = self.color_text;
        self.draw_text.draw_abs(cx, dvec2(x0, y - 28.0), &words);
    }
}

impl Widget for FabScanView {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Image;
        makepad_platform::a11y::set_string(&mut node.name, "Scan");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Event::Signal = event {
            // the splats of the scan's newest points are made: show them
            if self.building.as_ref().is_some_and(|(_, job)| job.is_finished()) {
                self.draw_bg.redraw(cx);
            }
        }
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerDown(fe) => {
                if fe.tap_count >= 2 {
                    (self.yaw, self.pitch, self.zoom) = (START_YAW, START_PITCH, 1.0);
                    self.draw_bg.redraw(cx);
                }
                self.drag = Some((fe.abs, self.yaw, self.pitch));
            }
            Hit::FingerMove(fe) => {
                if let Some((from, yaw, pitch)) = self.drag {
                    let d = fe.abs - from;
                    self.yaw = yaw - d.x * 0.01;
                    self.pitch = (pitch + d.y * 0.01).clamp(-0.2, 1.55);
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::FingerUp(_) => self.drag = None,
            Hit::FingerScroll(fe) => {
                let notches = (-fe.scroll.y / 40.0).clamp(-3.0, 3.0);
                self.zoom = (self.zoom * 1.12f64.powf(notches)).clamp(0.3, 40.0);
                self.draw_bg.redraw(cx);
                event.set_scroll_handled(Vec2Index::X);
                event.set_scroll_handled(Vec2Index::Y);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        let Some((scan, colour, cameras)) = scope.data.get::<AppState>().map(|s| (s.scan.clone(), s.ui.scan_colour, s.ui.scan_cameras)) else { return DrawStep::done() };
        let fit = self.fit(&scan);
        if let Some((c, r)) = fit {
            self.draw_splats(cx, &Proj::new(self.yaw, self.pitch, c, r * 1.1, rect, self.zoom), &scan, colour, rect);
        }
        cx.begin_turtle(Walk::abs_rect(rect), Layout::default().with_clip(true, true));
        if let Some((c, r)) = fit {
            let p = Proj::new(self.yaw, self.pitch, c, r * 1.1, rect, self.zoom);
            if cameras {
                self.draw_keyframes(cx, &p, &scan);
            }
            self.draw_timeline(cx, rect, &scan);
        }
        // what the scan is doing, always: the first thing seen and the last
        if fit.is_some() {
            self.draw_back.draw_abs(cx, Rect { pos: rect.pos, size: dvec2(rect.size.x, if scan.headset_says.is_empty() { 34.0 } else { 54.0 }) });
        }
        self.draw_title.color = if scan.phase == ScanPhase::Failed { self.color_failed } else { self.color_text };
        self.draw_title.draw_abs(cx, rect.pos + dvec2(14.0, 12.0), &scan.status);
        self.draw_text.color = alpha(self.color_text, 0.8);
        self.draw_text.draw_abs(cx, rect.pos + dvec2(14.0, 32.0), &scan.headset_says);
        if scan.keyframes.is_empty() && scan.surface.is_empty() {
            self.draw_text.draw_abs(cx, rect.pos + dvec2(14.0, 60.0), "The scan shows here from its first keyframe: where each was taken from, and the surface growing under them.");
        }
        cx.end_turtle();
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_splat_pass_paints_only_when_its_picture_changes() {
        use makepad_widgets_core::makepad_draw::cx_draw::CxDraw;

        let mut cx = Cx::new(Box::new(|_, _| {}));
        let view = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, WindowFamilies::default(), &[makepad_widgets_fab::fab_mod]);
            makepad_xr_stream::serve_script_mod(vm);
            crate::theme::script_mod(vm);
            crate::ui::script_mod_kit(vm);
            crate::tools::overlay::script_mod(vm);
            super::script_mod(vm);
            let value = vm.eval(script! { mod.widgets.FabScanView{} });
            WidgetRef::script_from_value(vm, value)
        });
        let mut state = AppState::default();
        state.scan.surface = Arc::new(vec![ScanPoint {
            point: [0.0; 3], normal: [0.0, 1.0, 0.0], first: 0, size: 0.01, colour: [0.5; 3],
        }]);
        let pass = {
            let mut scan_view = view.borrow_mut::<FabScanView>().unwrap();
            scan_view.built = Some(build_splats(Sources::of(&state.scan), state.ui.scan_colour, [0.5; 3], [0.5; 3]));
            scan_view.pass.draw_pass_id()
        };
        let parent = DrawPass::new(&mut cx);
        let parent_size = dvec2(320.0, 200.0);
        let panel_size = dvec2(160.0, 100.0);
        let dpi = 1.0;
        parent.set_size(&mut cx, parent_size);
        let mut list = DrawList2d::new(&mut cx);
        let mut draw = |cx: &mut Cx, state: &mut AppState| {
            cx.redraw_all();
            let event = std::mem::take(&mut cx.new_draw_event);
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&parent, Some(dpi));
            list.begin_always(&mut frame);
            frame.begin_root_turtle(parent_size, Layout::default());
            frame.begin_turtle(Walk::fixed(panel_size.x, panel_size.y), Layout::default());
            view.draw_all(&mut frame, &mut Scope::with_data(state));
            frame.end_turtle();
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&parent);
        };

        draw(&mut cx, &mut state);
        assert!(cx.passes[pass].paint_dirty, "the first scan draw requests a paint");
        assert_eq!(cx.get_pass_rect(pass, dpi).map(|r| r.size), Some(panel_size), "the scan pass uses the panel size");
        // The GPU would clear the flag after consuming the pass.
        cx.passes[pass].paint_dirty = false;
        draw(&mut cx, &mut state);
        assert!(!cx.passes[pass].paint_dirty, "an unchanged scan reuses its painted texture");
        assert!(!cx.pass_attachment_is_stale(pass), "the composite reattaches the cached scan");

        view.borrow_mut::<FabScanView>().unwrap().yaw += 0.25;
        draw(&mut cx, &mut state);
        assert!(cx.passes[pass].paint_dirty, "orbiting the scan requests a paint");
        cx.passes[pass].paint_dirty = false;
        view.borrow_mut::<FabScanView>().unwrap().zoom *= 2.0;
        draw(&mut cx, &mut state);
        assert!(cx.passes[pass].paint_dirty, "zooming the scan requests a paint");
        cx.passes[pass].paint_dirty = false;
        state.scan.phase = ScanPhase::Capturing;
        draw(&mut cx, &mut state);
        assert!(cx.passes[pass].paint_dirty, "changing the scan's busy flag requests a paint");
    }

    #[test]
    fn the_pass_camera_puts_a_point_where_proj_does() {
        let pane = Rect { pos: dvec2(40.0, 25.0), size: dvec2(640.0, 400.0) };
        let (c, radius) = ([0.1, -0.2, 0.3], 0.5);
        let p = Proj::new(0.7, 0.4, c, radius, pane, 2.5);
        // a pass is the pane's own size: its pixel (0, 0) is the pane's top left
        let (view, projection) = pass_camera(&p, pane.size, radius);
        let points = [c, [0.3, -0.1, 0.1], [-0.2, -0.45, 0.6], [0.3, 0.1, 0.1], [0.1, -0.2, 0.8]];
        let mut last: Option<(f32, f32)> = None;
        for q in points {
            let v = view.transform_vec4(vec4(q[0], q[1], q[2], 1.0));
            let clip = projection.transform_vec4(v);
            assert!((clip.w - 1.0).abs() < 1e-6, "an orthographic view divides by one, not {}", clip.w);
            let on_screen = dvec2(pane.pos.x + (f64::from(clip.x) + 1.0) * 0.5 * pane.size.x, pane.pos.y + (1.0 - f64::from(clip.y)) * 0.5 * pane.size.y);
            let (at, depth) = p.at(q);
            assert!((on_screen - at).length() < 1e-2, "{q:?} shows at {on_screen:?} in the pass and at {at:?} in the overlays");
            assert!((0.0..=1.0).contains(&clip.z), "{q:?} is cut by the near or far plane: clip z {}", clip.z);
            if let Some((d, z)) = last {
                assert_eq!(depth > d, clip.z < z, "a nearer point has the smaller depth: Proj says {depth} against {d}, the pass {} against {z}", clip.z);
            }
            last = Some((depth, clip.z));
        }
    }
}
