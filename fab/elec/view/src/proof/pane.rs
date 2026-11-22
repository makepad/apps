//! The proof pane: a canvas that draws an export's files as read back, on the shared canvas base
//! (camera, navigation, hover, semantic nodes, looks). It holds view state only: the scene it was
//! given, which layers are shown, the theme and "what the factory sees"; the host builds the
//! scene from the snapshot's export on a worker and hands it over with `set_scene`.
//!
//! One look per state (ARCHITECTURE 6.6): the read-back is full ink; with "what the factory
//! sees" off the model's layers are drawn under it as a ghost; a check that failed is a marker in
//! the violation look at its place; the hovered object is the shared hover rim; the focused one
//! the focus look. Keys: 1-9 and 0 toggle the layers (the Layers rail does the same through
//! `set_layer_visible`), W the ghost, B the panel, T the theme, F fits.
//!
//! Cost: a rebuild (scene change, layer toggle, focus) is O(visible instances); a frame sets
//! uniforms per retained layer, O(layers). Picking is one index query per pointer event.

use super::inst::Inst;
use crate::board::{
    batch::{batch_scene, Templates as BoardTemplates},
    liveix::LiveIx,
    draw::{DrawBoardPad, DrawBoardStroke, DrawBoardTri},
    inst::{PadTemplate, StrokeTemplate, TriTemplate},
    theme::BoardTheme,
    lists::{BoardTextures, BoardUniforms, StreamList},
};
use super::marks::{dot_insts, drill_insts, marker_insts};
use super::paint::layer_insts;
use super::panel::{panel_lines, Tone};
use super::scene::{ghost_planes, planes, Layer, ProofScene, Role, Thing};
use crate::canvas::{
    draw::{
        ArcTemplate, BoxTemplate, CanvasLayer, CanvasLive, CanvasUniforms, DrawCanvasArc, DrawCanvasBox, DrawCanvasLine, LineTemplate,
    },
    pane::BOARD_MAX_PPU, Camera2d, CanvasInput, CanvasItems, CanvasNode, CanvasPane, Look, Tokens,
};
use fab_elec_core::{LayerId, PolySet};
use makepad_widgets_core::makepad_platform::a11y_tree;
use makepad_widgets_core::*;
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.ProofPaneBase = #(ProofPane::register_widget(vm))
    mod.widgets.ProofPane = set_type_default() do mod.widgets.ProofPaneBase {
        width: Fill
        height: Fill
        draw_text +: { text_style: theme.font_code { font_size: 9.0 } }
        draw_stroke: mod.draw.DrawBoardStroke{}
        draw_pad: mod.draw.DrawBoardPad{}
        draw_tri: mod.draw.DrawBoardTri{}
    }
}

struct Templates {
    line: LineTemplate,
    boxes: BoxTemplate,
    arc: ArcTemplate,
}

#[derive(Default)]
struct Rows {
    lines: Vec<f32>,
    boxes: Vec<f32>,
    arcs: Vec<f32>,
}

impl Rows {
    fn push(&mut self, t: &Templates, inst: &Inst) {
        match inst {
            Inst::Line(i) => t.line.append(&mut self.lines, i),
            Inst::Box(i) => t.boxes.append(&mut self.boxes, i),
            Inst::Arc(i) => t.arc.append(&mut self.arcs, i),
        }
    }
}

/// One layer's polygon fills: triangles and the ring hairlines, from the board's batcher.
struct Pour {
    /// Index into the scene's layers (or ghost layers).
    layer: usize,
    tris: Option<StreamList>,
    strokes: Option<StreamList>,
}

#[derive(Default)]
struct Gpu {
    lines: CanvasLayer,
    boxes: CanvasLayer,
    arcs: CanvasLayer,
}

impl Gpu {
    fn set(&mut self, t: &Templates, rows: Rows) {
        self.lines.set(t.line.slots(), rows.lines);
        self.boxes.set(t.boxes.slots(), rows.boxes);
        self.arcs.set(t.arc.slots(), rows.arcs);
    }
}

/// What the pointer sees: the scene and which layers are shown.
struct Pick<'a> {
    scene: &'a ProofScene,
    visible: &'a [bool],
}

impl CanvasItems for Pick<'_> {
    type Key = u32;

    fn hit(&self, at: [f64; 2], radius: f64) -> Vec<u32> {
        self.scene.pick(at, radius, &|l| self.visible.get(l).copied().unwrap_or(true))
    }

    fn nodes(&self, cam: &Camera2d) -> Vec<CanvasNode<u32>> {
        let v = cam.visible();
        let on = |l: usize| self.visible.get(l).copied().unwrap_or(true);
        let mut out = Vec::new();
        for h in &self.scene.drills {
            if inside(v, h.at) {
                let r = cam.rect_to_screen([h.at[0] as f64 - 0.3, h.at[1] as f64 - 0.3, h.at[0] as f64 + 0.3, h.at[1] as f64 + 0.3]);
                out.push(CanvasNode { sel: h.id, rect: r, role: "drill", name: format!("{:.2} mm hole", h.dia), value: None });
            }
        }
        for d in &self.scene.dots {
            if inside(v, d.at) {
                let r = cam.rect_to_screen([d.at[0] as f64 - 0.4, d.at[1] as f64 - 0.4, d.at[0] as f64 + 0.4, d.at[1] as f64 + 0.4]);
                out.push(CanvasNode { sel: d.id, rect: r, role: "part", name: d.refdes.clone(), value: None });
            }
        }
        for m in &self.scene.markers {
            let r = cam.rect_to_screen([m.at[0] as f64 - 0.8, m.at[1] as f64 - 0.8, m.at[0] as f64 + 0.8, m.at[1] as f64 + 0.8]);
            out.push(CanvasNode { sel: m.id, rect: r, role: "check", name: m.cause.clone(), value: None });
        }
        // pads and tracks that carry a pin or a net: one node per named object, visible layers only
        for (li, l) in self.scene.layers.iter().enumerate().filter(|(i, _)| on(*i)) {
            for it in &l.items {
                let Some(a) = it.attrs.map(|i| &l.attrs[i as usize]) else { continue };
                let name = match (&a.pin, &a.net) {
                    (Some((r, p)), _) => format!("{r}.{p}"),
                    (None, Some(n)) => n.clone(),
                    _ => continue,
                };
                let b = it.prim.bbox();
                if !inside(v, [fab_elec_core::to_mm(b.center().x) as f32, fab_elec_core::to_mm(b.center().y) as f32]) {
                    continue;
                }
                let r = cam.rect_to_screen([
                    fab_elec_core::to_mm(b.min.x), fab_elec_core::to_mm(b.min.y), fab_elec_core::to_mm(b.max.x), fab_elec_core::to_mm(b.max.y),
                ]);
                let role = if a.pin.is_some() { "pad" } else { "track" };
                out.push(CanvasNode { sel: it.id, rect: r, role, name, value: Some(l.label.clone()) });
                let _ = li;
            }
        }
        out
    }
}

fn inside(v: [f64; 4], p: [f32; 2]) -> bool {
    (p[0] as f64) >= v[0] && (p[0] as f64) <= v[2] && (p[1] as f64) >= v[1] && (p[1] as f64) <= v[3]
}

#[derive(Script, ScriptHook, Widget)]
pub struct ProofPane {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_line: DrawCanvasLine,
    #[live]
    draw_box: DrawCanvasBox,
    #[live]
    draw_arc: DrawCanvasArc,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_stroke: DrawBoardStroke,
    #[live]
    draw_pad: DrawBoardPad,
    #[live]
    draw_tri: DrawBoardTri,
    #[rust]
    board_templates: Option<BoardTemplates>,
    /// The read-back layers' exact polygon fills, and the model's under them.
    #[rust]
    pours: Vec<Pour>,
    #[rust]
    ghost_pours: Vec<Pour>,
    #[rust(CanvasPane::with_max_ppu(BOARD_MAX_PPU))]
    pane: CanvasPane<u32>,
    #[rust]
    scene: Arc<ProofScene>,
    #[rust]
    visible: Vec<bool>,
    #[rust]
    gpu: Vec<Gpu>,
    #[rust]
    ghost: Vec<Gpu>,
    #[rust]
    marks: Gpu,
    #[rust]
    focus_gpu: Gpu,
    #[rust(true)]
    dark: bool,
    /// "What the factory sees": the read-back alone. Off adds the model's ghost under it.
    #[rust(true)]
    factory_sees: bool,
    #[rust(true)]
    show_panel: bool,
    #[rust]
    focus: Option<u32>,
    #[rust(true)]
    dirty: bool,
    #[rust]
    templates: Option<Templates>,
    #[rust]
    capture_attempted: bool,
    #[rust]
    fitted: bool,
    /// Live off: no host feeds the proof pane (the files carry nothing live).
    #[rust]
    live: Option<CanvasLive>,
}

impl ProofPane {
    pub fn set_scene(&mut self, cx: &mut Cx, scene: Arc<ProofScene>) {
        self.visible.resize(scene.layers.len(), true);
        self.scene = scene;
        self.focus = None;
        self.dirty = true;
        self.fitted = false;
        self.draw_bg.redraw(cx);
    }

    pub fn set_layer_visible(&mut self, cx: &mut Cx, layer: usize, on: bool) {
        if let Some(v) = self.visible.get_mut(layer) {
            if *v != on {
                *v = on;
                self.dirty = true;
                self.draw_bg.redraw(cx);
            }
        }
    }

    pub fn layer_visible(&self, layer: usize) -> bool {
        self.visible.get(layer).copied().unwrap_or(true)
    }

    pub fn set_factory_sees(&mut self, cx: &mut Cx, on: bool) {
        if self.factory_sees != on {
            self.factory_sees = on;
            self.dirty = true;
            self.draw_bg.redraw(cx);
        }
    }

    pub fn set_dark(&mut self, cx: &mut Cx, dark: bool) {
        if self.dark != dark {
            self.dark = dark;
            self.dirty = true;
            self.draw_bg.redraw(cx);
        }
    }

    /// The object the pane has focused (what the host resolves to a `Sel` and broadcasts).
    pub fn focused(&self) -> Option<u32> {
        self.focus
    }

    pub fn scene(&self) -> &Arc<ProofScene> {
        &self.scene
    }

    fn fit(&mut self, cx: &mut Cx, now: f64) {
        let b = self.scene.bounds;
        if b[2] > b[0] {
            self.pane.nav.fly_to_box(&self.pane.cam, b, 28.0, now, None);
            self.pane.animate(cx);
        }
    }

    fn input(&mut self, cx: &mut Cx, input: CanvasInput<u32>, now: f64) {
        match input {
            CanvasInput::Click { key, .. } => {
                if self.focus != key {
                    self.focus = key;
                    self.dirty = true;
                }
            }
            CanvasInput::Escape => {
                self.dirty |= self.focus.take().is_some();
                self.fit(cx, now);
            }
            CanvasInput::Key { code: KeyCode::KeyT, .. } => {
                self.dark = !self.dark;
                self.dirty = true;
            }
            CanvasInput::Key { code: KeyCode::KeyF, .. } => self.fit(cx, now),
            CanvasInput::Key { code: KeyCode::KeyW, .. } => {
                self.factory_sees = !self.factory_sees;
                self.dirty = true;
            }
            CanvasInput::Key { code: KeyCode::KeyB, .. } => self.show_panel = !self.show_panel,
            CanvasInput::Key { code, .. } => {
                let n = match code {
                    KeyCode::Key1 => 0,
                    KeyCode::Key2 => 1,
                    KeyCode::Key3 => 2,
                    KeyCode::Key4 => 3,
                    KeyCode::Key5 => 4,
                    KeyCode::Key6 => 5,
                    KeyCode::Key7 => 6,
                    KeyCode::Key8 => 7,
                    KeyCode::Key9 => 8,
                    KeyCode::Key0 => 9,
                    _ => return,
                };
                // the keys count from the top of the stack
                let l = self.scene.layers.len();
                if n < l {
                    let i = l - 1 - n;
                    self.visible[i] = !self.visible[i];
                    self.dirty = true;
                }
            }
            CanvasInput::Hover(_) | CanvasInput::Camera | CanvasInput::Marquee { .. } | CanvasInput::Press { .. } => {}
            CanvasInput::DragStart { .. } | CanvasInput::DragMove { .. } | CanvasInput::DragEnd { .. } | CanvasInput::DragCancel { .. } => return,
        }
        self.pane.animate(cx);
        self.draw_bg.redraw(cx);
    }

    fn rebuild(&mut self, tokens: &Tokens) {
        let Some(t) = &self.templates else { return };
        let scene = self.scene.clone();
        let build = |layers: &[Layer], look: Look, alpha: f32, rows_of: &mut Vec<Gpu>| {
            rows_of.clear();
            for l in layers {
                let mut rows = Rows::default();
                for i in layer_insts(l, look, alpha, tokens) {
                    rows.push(t, &i);
                }
                let mut g = Gpu::default();
                g.set(t, rows);
                rows_of.push(g);
            }
        };
        build(&scene.layers, Look::Committed, 1.0, &mut self.gpu);
        // exact polygon fills through the board's batcher, on this thread: a pour batch is a few
        // milliseconds (the host can hand in batches built on the pool)
        if let Some(bt) = &self.board_templates {
            let pours = |layers: &[Layer], plane: &crate::board::scene::DrawScene| -> Vec<Pour> {
                // read-back pours carry no simulation rows
                let batch = batch_scene(plane, bt, &LiveIx::none());
                batch
                    .layers
                    .iter()
                    .filter_map(|lb| {
                        let layer = layers.iter().position(|l| l.id == lb.layer)?;
                        Some(Pour { layer, tris: StreamList::of(&lb.tris), strokes: StreamList::of(&lb.strokes) })
                    })
                    .collect()
            };
            self.pours = pours(&scene.layers, &planes(&scene));
            self.ghost_pours = if self.factory_sees { Vec::new() } else { pours(&scene.ghost, &ghost_planes(&scene)) };
        }
        if self.factory_sees {
            self.ghost.clear();
        } else {
            build(&scene.ghost, Look::Ghost, 1.0, &mut self.ghost);
        }
        let mut rows = Rows::default();
        for i in drill_insts(&scene.drills, tokens).into_iter().chain(dot_insts(&scene.dots, tokens)).chain(marker_insts(&scene.markers, tokens)) {
            rows.push(t, &i);
        }
        self.marks.set(t, rows);
        // the focused object again, in the focus look, over everything
        let mut rows = Rows::default();
        if let Some(id) = self.focus {
            if let Some(Thing::Item { item, .. }) = scene.thing(id) {
                let one = Layer { file: String::new(), label: String::new(), role: Role::Other, items: vec![item.clone()], attrs: Vec::new(), id: LayerId::F_CU, fills: PolySet::new() };
                for i in layer_insts(&one, Look::Focus, 1.0, tokens) {
                    rows.push(t, &i);
                }
            }
        }
        self.focus_gpu.set(t, rows);
        self.dirty = false;
    }

    /// The panel's width in points and the characters that fit in it.
    fn panel_size(rect: Rect) -> (f64, usize) {
        let chars = ((rect.size.x * 0.34 - 16.0) / 6.0).floor().max(20.0) as usize;
        ((chars as f64 * 6.0 + 16.0).min(rect.size.x * 0.5), chars)
    }

    fn panel(&mut self, cx: &mut Cx2d, tokens: &Tokens, rect: Rect) {
        let (w, chars) = Self::panel_size(rect);
        let area = Rect { pos: dvec2(rect.pos.x + rect.size.x - w, rect.pos.y), size: dvec2(w, rect.size.y) };
        let bg = tokens.canvas;
        self.draw_bg.color = vec4(bg[0], bg[1], bg[2], 0.86);
        self.draw_bg.draw_abs(cx, area);
        cx.push_clip_rect(area);
        let lines = panel_lines(&self.scene, chars);
        let mut y = area.pos.y + 10.0;
        for l in lines {
            let c = match l.tone {
                Tone::Head => tokens.ink,
                Tone::Ok => tokens.ok,
                Tone::Fail => tokens.violation,
                Tone::Dim => tokens.ink_dim,
                Tone::Normal => tokens.wire_signal,
            };
            self.draw_text.color = vec4(c[0], c[1], c[2], c[3]);
            self.draw_text.draw_abs(cx, dvec2(area.pos.x + 8.0, y), &l.text);
            y += 14.0;
            if y > area.pos.y + area.size.y {
                break;
            }
        }
        cx.pop_clip_rect();
        self.draw_bg.color = vec4(bg[0], bg[1], bg[2], bg[3]);
    }

    /// The layer legend: which layers show, from the top of the stack, as the keys count them.
    fn legend(&mut self, cx: &mut Cx2d, tokens: &Tokens, rect: Rect) {
        let n = self.scene.layers.len();
        let bg = tokens.canvas;
        self.draw_bg.color = vec4(bg[0], bg[1], bg[2], 0.8);
        self.draw_bg.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + 4.0, rect.pos.y + 4.0), size: dvec2(300.0, 14.0 * (n as f64 + 1.0) + 22.0) });
        self.draw_bg.color = vec4(bg[0], bg[1], bg[2], bg[3]);
        let mut y = rect.pos.y + 10.0;
        for k in 0..n {
            let i = n - 1 - k;
            let on = self.visible.get(i).copied().unwrap_or(true);
            let l = &self.scene.layers[i];
            let c = if on { tokens.ink } else { tokens.ink_dim };
            self.draw_text.color = vec4(c[0], c[1], c[2], c[3]);
            let key = if k < 9 { format!("{}", k + 1) } else if k == 9 { "0".into() } else { " ".into() };
            let text = format!("{} {} {}", key, if on { "[x]" } else { "[ ]" }, l.label);
            self.draw_text.draw_abs(cx, dvec2(rect.pos.x + 10.0, y), &text);
            y += 14.0;
        }
        let c = tokens.ink_dim;
        self.draw_text.color = vec4(c[0], c[1], c[2], c[3]);
        let mode = if self.factory_sees { "W: what the factory sees (read-back only)" } else { "W: read-back over the model (ghost)" };
        self.draw_text.draw_abs(cx, dvec2(rect.pos.x + 10.0, y + 6.0), mode);
    }
}

impl Widget for ProofPane {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.draw_bg.area().is_valid(cx) {
            let inputs = {
                let pick = Pick { scene: &self.scene, visible: &self.visible };
                self.pane.handle_event(cx, event, self.draw_bg.area(), &pick)
            };
            let now = cx.seconds_since_app_start();
            for i in inputs {
                self.input(cx, i, now);
            }
        }
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        self.pane.semantics_parts(out);
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        if let Some(input) = self.pane.a11y_press(cx, request) {
            cx.set_key_focus(self.draw_bg.area());
            let now = cx.seconds_since_app_start();
            self.input(cx, input, now);
            a11y_tree::ActionResult::Handled
        } else {
            a11y_tree::ActionResult::Unhandled
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        let b = self.scene.bounds;
        // the camera's view is the canvas beside the panel
        let view = if self.show_panel { Rect { pos: rect.pos, size: dvec2(rect.size.x - Self::panel_size(rect).0, rect.size.y) } } else { rect };
        self.pane.place(cx, view, if b[2] > b[0] { Some(b) } else { None });
        if !self.capture_attempted {
            self.capture_attempted = true;
            self.templates = match (
                LineTemplate::capture(cx, &self.draw_line),
                BoxTemplate::capture(cx, &self.draw_box),
                ArcTemplate::capture(cx, &self.draw_arc),
            ) {
                (Some(line), Some(boxes), Some(arc)) => Some(Templates { line, boxes, arc }),
                _ => None,
            };
        }
        if self.board_templates.is_none() {
            self.board_templates = match (
                StrokeTemplate::capture(cx, &self.draw_stroke.draw_vars),
                PadTemplate::capture(cx, &self.draw_pad.draw_vars),
                TriTemplate::capture(cx, &self.draw_tri.draw_vars),
            ) {
                (Some(stroke), Some(pad), Some(tri)) => Some(BoardTemplates { stroke, pad, tri }),
                _ => None,
            };
            self.dirty = true;
        }
        if self.templates.is_none() {
            return DrawStep::done();
        }
        let now = cx.seconds_since_app_start();
        let live = self.live.get_or_insert_with(|| CanvasLive::off(cx)).clone();
        let scene = self.scene.clone();
        let tokens = Tokens::for_theme(self.dark);
        let bg = tokens.canvas;
        self.draw_bg.color = vec4(bg[0], bg[1], bg[2], bg[3]);
        self.draw_bg.draw_abs(cx, rect);
        if self.dirty {
            self.rebuild(tokens);
        }
        let uniforms = CanvasUniforms {
            drag_item: -1.0, drag_offset: [0.0; 2],
            cam: self.pane.cam.uniforms(),
            motion_t: 0.0,
            hover_item: self.pane.hovered().map_or(-1.0, |id| *id as f32),
            hover_mix: self.pane.hover_mix(),
            layer_alpha: 1.0,
            time: now as f32,
            live_time: now as f32,
            light: !self.dark,
            clip: [rect.pos.x as f32, rect.pos.y as f32, (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32],
        };
        // the polygon fills first of each layer: board-pipeline lists, the board theme's inks
        let theme = BoardTheme::for_light(!self.dark);
        let base = BoardUniforms {
            cam: self.pane.cam.uniforms(),
            motion_t: 0.0,
            hover_item: -1.0,
            hover_mix: 0.0,
            layer_alpha: 1.0,
            layer_ink: [1.0; 4],
            thin_alpha: theme.thin_alpha,
            clip: [rect.pos.x as f32, rect.pos.y as f32, (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32],
            ..BoardUniforms::default()
        };
        uniforms.with(tokens, |u| {
            let line = &mut self.draw_line.draw_vars;
            let boxes = &mut self.draw_box.draw_vars;
            let arc = &mut self.draw_arc.draw_vars;
            let (tri, stroke) = (&mut self.draw_tri.draw_vars, &mut self.draw_stroke.draw_vars);
            let mut pour = |cx: &mut Cx2d, p: &mut Pour, id: LayerId, alpha: f32| {
                let uni = BoardUniforms { layer_ink: theme.ink(id), layer_alpha: alpha, ..base };
                if let Some(l) = p.tris.as_mut() {
                    l.draw(cx, tri, &BoardTextures::default(), &uni);
                }
                if let Some(l) = p.strokes.as_mut() {
                    l.draw(cx, stroke, &BoardTextures::default(), &uni);
                }
            };
            for p in self.ghost_pours.iter_mut() {
                let id = scene.ghost[p.layer].id;
                pour(cx, p, id, 0.2);
            }
            for g in self.ghost.iter_mut() {
                g.boxes.draw(cx, boxes, u, &live);
                g.lines.draw(cx, line, u, &live);
                g.arcs.draw(cx, arc, u, &live);
            }
            for (i, g) in self.gpu.iter_mut().enumerate() {
                if self.visible.get(i).copied().unwrap_or(true) {
                    let id = scene.layers[i].id;
                    for p in self.pours.iter_mut().filter(|p| p.layer == i) {
                        pour(cx, p, id, if id.is_copper() { 0.55 } else { 0.8 });
                    }
                    g.boxes.draw(cx, boxes, u, &live);
                    g.lines.draw(cx, line, u, &live);
                    g.arcs.draw(cx, arc, u, &live);
                }
            }
            for g in [&mut self.marks, &mut self.focus_gpu] {
                g.boxes.draw(cx, boxes, u, &live);
                g.lines.draw(cx, line, u, &live);
                g.arcs.draw(cx, arc, u, &live);
            }
        });
        cx.push_clip_rect(rect);
        self.legend(cx, tokens, rect);
        cx.pop_clip_rect();
        if self.show_panel {
            self.panel(cx, tokens, rect);
        }
        if !self.fitted && b[2] > b[0] {
            self.fitted = true;
            let c = self.pane.cam;
            self.pane.cam = Camera2d::fit(view, c.dpi, b, 28.0);
        }
        let pick = Pick { scene: &self.scene, visible: &self.visible };
        self.pane.gather_nodes(&pick);
        self.pane.animate(cx);
        DrawStep::done()
    }
}
