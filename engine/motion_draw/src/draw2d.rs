//! Drawing a `Draw2D` layer (`makepad_motion::draw2d`): what its draw
//! function recorded, into a target of the layer's box (linear half float
//! in an HDR frame), in order. Segments are line batches (one draw call per
//! run of the same blend: `makepad-render-lines`, linear and unclamped, so a
//! colour over 1 glows); text is Makepad's text renderer, a transform with a
//! turn, shear or uneven scale drawn through its own draw list's view
//! transform (glyphs rasterised at their drawn size); fills are the vector
//! renderer's. Each run of one kind is its own draw list, so batching never
//! reorders them.

use crate::MotionDraw;
use makepad_motion::draw2d::{Baseline, Draw2dList, Gradient, Op, SegBlend, TextOp};
use makepad_motion::frame::{Affine, Item, ItemKind, Local};
use makepad_motion::model::{Blend, Cap, Join};
use makepad_motion::geometry;
use makepad_render_lines as lines;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*

    // One stage of a Draw2D layer onto the next, full-target, premultiplied:
    // mode 0 copies `layer`, 1 keeps it inside `mask` (a clip), 2 removes it
    // under `mask` (destination-out).
    set_type_default() do #(DrawDraw2dComposite::script_shader(vm)){
        ..mod.draw.DrawQuad
        layer: texture_2d(float)
        mask: texture_2d(float)

        pixel: fn() {
            let l = self.layer.sample(self.pos)
            let m = self.mask.sample(self.pos).w
            if self.mode > 1.5 {
                return l * (1.0 - m)
            }
            if self.mode > 0.5 {
                return l * m
            }
            return l
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawDraw2dComposite {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub mode: f32,
}

/// A text run's layout metrics (by [`RunKey`]).
#[derive(Clone)]
struct RunMetrics {
    width: f64,
    asc: f64,
    desc: f64,
    top_to_base: f64,
    /// Each glyph's byte in the text and its pen x.
    pens: Vec<(usize, f64)>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RunKey {
    text: String,
    font: String,
    weight: u64,
    italic: bool,
    axes: Vec<(String, u64)>,
    /// Pixel size in 1/64 px.
    px: i64,
}

/// The line renderer, the composite shader and the draw lists runs are
/// recorded into.
#[derive(Default)]
pub(crate) struct Draw2dDraws {
    runs: std::collections::HashMap<RunKey, RunMetrics>,
    /// A vector draw per gradient fill this frame (each has its own ramp texture).
    grads: Vec<Box<DrawVector>>,
    grad_next: usize,
    lines: Option<lines::LineRenderer>,
    composite: Option<Box<DrawDraw2dComposite>>,
    lists: Vec<DrawList2d>,
    next: usize,
}

impl Draw2dDraws {
    pub(crate) fn begin_frame(&mut self) {
        self.next = 0;
        self.grad_next = 0;
    }
}

/// A target being drawn (one stage of the layer).
struct Stage {
    index: usize,
}

fn open(md: &mut MotionDraw, cx: &mut Cx2d, px: (u32, u32)) -> Stage {
    let index = md.target(cx, px, "motion_draw2d");
    md.pool[index].begin_unclipped(cx, vec4(0.0, 0.0, 0.0, 0.0));
    // A clip far past the pass for every instance (the turtle writes each
    // one's clip): turned and stretched runs' list coordinates leave the
    // pass rect; text clips nest inside it.
    cx.push_clip_rect(Rect { pos: dvec2(-1.0e7, -1.0e7), size: dvec2(2.0e7, 2.0e7) });
    Stage { index }
}

fn close(md: &mut MotionDraw, cx: &mut Cx2d, stage: Stage) -> Texture {
    cx.pop_clip_rect();
    md.pool[stage.index].end_unclipped(cx);
    let id = md.pool[stage.index].pass.draw_pass_id();
    md.record(id);
    md.pool[stage.index].texture.clone()
}

/// Draw `layer` over the open stage (full target): copied, clipped to or
/// erased under `mask`.
fn composite(md: &mut MotionDraw, cx: &mut Cx2d, px: (u32, u32), layer: &Texture, mask: Option<&Texture>, mode: f32) {
    let Some(c) = md.draw2d.composite.as_mut() else { return };
    c.draw_vars.set_texture(0, layer);
    c.draw_vars.set_texture(1, mask.unwrap_or(layer));
    c.mode = mode;
    c.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(px.0 as f64, px.1 as f64) });
}

/// A stage of white coverage: the contours, each at its alpha.
fn mask(md: &mut MotionDraw, cx: &mut Cx2d, px: (u32, u32), s: f64, shapes: &[(&[Vec<[f64; 2]>], f64)]) -> Texture {
    let stage = open(md, cx, px);
    let saved = md.base;
    md.base = Affine::scale(s, s);
    let bare = bare_item();
    for (contours, alpha) in shapes {
        let path = geometry::polygons_path(contours);
        md.vector(cx, &bare, &path, [1.0, 1.0], Some([1.0, 1.0, 1.0, *alpha]), None, 0.0, Cap::Round, Join::Round, 1.0);
    }
    md.flush_vector(cx);
    md.base = saved;
    close(md, cx, stage)
}

/// Record `list` into a target of `px` pixels showing the layer's box of
/// `size` units. Returns the texture (`None` for an empty box). Clips and
/// erasing split the layer into stages, each composited onto the next.
pub(crate) fn prepare(md: &mut MotionDraw, cx: &mut Cx2d, list: &Draw2dList, size: [f64; 2], px: (u32, u32)) -> Option<Texture> {
    if size[0] <= 0.0 || size[1] <= 0.0 {
        return None;
    }
    let s = px.0 as f64 / size[0];
    if md.draw2d.lines.is_none() {
        md.draw2d.lines = lines::LineRenderer::new(cx);
    }
    if md.draw2d.composite.is_none() {
        md.draw2d.composite = cx.try_with_vm(|vm| Box::new(DrawDraw2dComposite::script_new_with_default(vm)));
    }
    let hdr = md.hdr;
    if md.draw2d.lines.as_ref().is_none_or(|l| !l.ready(cx, hdr)) || md.draw2d.composite.is_none() {
        // A pipeline still building: this recording is not the frame.
        md.fresh_glyphs = true;
        md.fresh_why = if md.draw2d.composite.is_none() { "draw2d: no composite" } else { "draw2d: lines pipeline not ready" };
    }
    let view = lines::LineView::screen((px.0 as f32, px.1 as f32), s as f32);
    let mut stage = open(md, cx, px);
    // Open clips: the stage outside them, and their contours.
    let mut clips: Vec<(Texture, &Vec<Vec<[f64; 2]>>)> = Vec::new();
    let mut k = 0;
    while k < list.ops.len() {
        match &list.ops[k] {
            Op::ClipBegin { contours } => {
                let outside = close(md, cx, stage);
                clips.push((outside, contours));
                stage = open(md, cx, px);
                k += 1;
                continue;
            }
            Op::ClipEnd => {
                if let Some((outside, contours)) = clips.pop() {
                    stage = end_clip(md, cx, px, s, stage, outside, contours);
                }
                k += 1;
                continue;
            }
            Op::Erase { .. } | Op::EraseText(_) => {
                // Consecutive erases, one mask (shapes and letters in white at their alpha).
                let mut end = k;
                while matches!(list.ops.get(end), Some(Op::Erase { .. } | Op::EraseText(_))) {
                    end += 1;
                }
                let before = close(md, cx, stage);
                let m = mask_ops(md, cx, px, s, &view, &list.ops[k..end]);
                md.note_kind("draw2d composite");
                stage = open(md, cx, px);
                composite(md, cx, px, &before, Some(&m), 2.0);
                k = end;
                continue;
            }
            _ => {}
        }
        // A run: consecutive drawing ops of one kind (text: under one turn and shear).
        let kind = run_kind(&list.ops[k]);
        let mut end = k + 1;
        while end < list.ops.len() && run_kind(&list.ops[end]) == kind {
            end += 1;
        }
        draw_run(md, cx, &view, list, &list.ops[k..end], k, kind, s);
        md.pick_op(cx, None);
        k = end;
    }
    // The recorder closes every clip; close any left all the same.
    while let Some((outside, contours)) = clips.pop() {
        stage = end_clip(md, cx, px, s, stage, outside, contours);
    }
    Some(close(md, cx, stage))
}

/// A stage of white coverage from erase ops: filled contours and letters,
/// each at its alpha.
fn mask_ops(md: &mut MotionDraw, cx: &mut Cx2d, px: (u32, u32), s: f64, view: &lines::LineView, ops: &[Op]) -> Texture {
    let stage = open(md, cx, px);
    let saved = md.base;
    md.base = Affine::scale(s, s);
    let bare = bare_item();
    let mut k = 0;
    while k < ops.len() {
        match &ops[k] {
            Op::Erase { contours, alpha } => {
                let path = geometry::polygons_path(contours);
                md.vector(cx, &bare, &path, [1.0, 1.0], Some([1.0, 1.0, 1.0, *alpha]), None, 0.0, Cap::Round, Join::Round, 1.0);
                k += 1;
            }
            Op::EraseText(_) => {
                md.flush_vector(cx);
                // letters in white (their own runs, as text draws)
                let mut end = k;
                let mut texts = Vec::new();
                while let Some(Op::EraseText(t)) = ops.get(end) {
                    let mut w = t.clone();
                    w.color = [1.0, 1.0, 1.0, t.color[3]];
                    texts.push(Op::Text(w));
                    end += 1;
                }
                let empty = Draw2dList::default();
                let mut j = 0;
                while j < texts.len() {
                    let kind = run_kind(&texts[j]);
                    let mut e = j + 1;
                    while e < texts.len() && run_kind(&texts[e]) == kind {
                        e += 1;
                    }
                    draw_run(md, cx, view, &empty, &texts[j..e], usize::MAX, kind, s);
                    j = e;
                }
                k = end;
            }
            _ => k += 1,
        }
    }
    md.flush_vector(cx);
    md.base = saved;
    close(md, cx, stage)
}

/// Close a clip: what was drawn inside it, masked by its contours, over
/// what was drawn before it, as the new open stage.
fn end_clip(md: &mut MotionDraw, cx: &mut Cx2d, px: (u32, u32), s: f64, stage: Stage, outside: Texture, contours: &[Vec<[f64; 2]>]) -> Stage {
    let inside = close(md, cx, stage);
    let m = mask(md, cx, px, s, &[(contours, 1.0)]);
    md.note_kind("draw2d composite");
    let stage = open(md, cx, px);
    composite(md, cx, px, &outside, None, 0.0);
    composite(md, cx, px, &inside, Some(&m), 1.0);
    stage
}

/// One run of a kind, in its own draw list (so batching keeps the order).
/// `first`: the run's first op in the list (each op its own pick id;
/// `usize::MAX`: ops of no list, the item's).
fn draw_run(md: &mut MotionDraw, cx: &mut Cx2d, view: &lines::LineView, list: &Draw2dList, ops: &[Op], first: usize, kind: RunKind, s: f64) {
    let picks = cx.pick_ids_on() && first != usize::MAX;
    let pick = |md: &mut MotionDraw, cx: &mut Cx2d, i: usize| {
        if picks {
            md.pick_op(cx, Some(first + i));
        }
    };
    let lid = md.draw2d.next;
    md.draw2d.next += 1;
    while md.draw2d.lists.len() <= lid {
        let l = DrawList2d::new(cx);
        md.draw2d.lists.push(l);
    }
    let mut dl = std::mem::replace(&mut md.draw2d.lists[lid], DrawList2d::new(cx));
    dl.begin_always(cx);
    let mut view_m = None;
    match kind {
        RunKind::Barrier => {}
        RunKind::Segs => {
            md.note_kind("draw2d lines");
            for (i, op) in ops.iter().enumerate() {
                pick(md, cx, i);
                if let Op::Segs { blend, start, end } = op {
                    let mut batch = lines::LineBatch::new(style(*blend));
                    for seg in &list.segs[*start..*end] {
                        batch.push(lines::Segment::new([seg.a[0], seg.a[1], 0.0], [seg.b[0], seg.b[1], 0.0], seg.width, seg.color));
                    }
                    if let Some(lr) = md.draw2d.lines.as_mut() {
                        lr.draw_lines(cx, view, &batch);
                    }
                }
            }
        }
        RunKind::Fill => {
            let saved = md.base;
            md.base = Affine::scale(s, s);
            let bare = bare_item();
            // A run of fills is one mesh, named as the layer's (a click on
            // it picks among the layer's fills by shape): a mesh per fill
            // for their own names would make a mesh per shape.
            if picks {
                md.pick_op(cx, None);
            }
            for op in ops.iter() {
                match op {
                    Op::Fill { contours, color } => {
                        let path = geometry::polygons_path(contours);
                        md.vector(cx, &bare, &path, [1.0, 1.0], Some(*color), None, 0.0, Cap::Round, Join::Round, 1.0);
                    }
                    Op::Gradient { contours, paint, alpha } => {
                        md.flush_vector(cx);
                        gradient(md, cx, contours, paint, *alpha, s);
                    }
                    _ => {}
                }
            }
            md.flush_vector(cx);
            md.base = saved;
        }
        RunKind::Text(_) => {
            md.draws.text.new_draw_call(cx);
            // The run's turn and shear (unit scale) as the list's view
            // transform; each op's own scale goes into its glyph size.
            let (l, turned) = unit_linear(kind);
            if turned {
                view_m = Some(l);
            }
            for (i, op) in ops.iter().enumerate() {
                pick(md, cx, i);
                if let Op::Text(t) = op {
                    draw_text(md, cx, view, t, s, turned.then_some(l));
                }
            }
        }
    }
    dl.end(cx);
    if let Some(l) = view_m {
        // list coordinates (pixels, unturned) -> pass pixels
        let mat = Mat4f { v: [l[0] as f32, l[1] as f32, 0.0, 0.0, l[2] as f32, l[3] as f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0] };
        dl.set_view_transform(cx, &mat);
    } else {
        dl.set_view_transform(cx, &Mat4f::identity());
    }
    md.draw2d.lists[lid] = dl;
}

/// Contours filled with a gradient: the vector renderer's gradient paint in
/// target pixels (its ramp is 8-bit: colours up to 1).
fn gradient(md: &mut MotionDraw, cx: &mut Cx2d, contours: &[Vec<[f64; 2]>], paint: &Gradient, alpha: f64, s: f64) {
    use makepad_svg::paint::{GradientStop, VectorPaint};
    let px = |p: [f64; 2]| [p[0] * s, p[1] * s];
    let path: Vec<Vec<[f64; 2]>> = contours.iter().map(|c| c.iter().map(|p| px(*p)).collect()).collect();
    let path = geometry::polygons_path(&path);
    let stop = |md: &MotionDraw, offset: f64, c: [f64; 4]| {
        let l = md.lin([c[0].min(1.0), c[1].min(1.0), c[2].min(1.0), 1.0]);
        let a = (c[3] * alpha) as f32;
        GradientStop { offset: offset as f32, color: [l[0] as f32 * a, l[1] as f32 * a, l[2] as f32 * a, a] }
    };
    let vp = match paint {
        Gradient::Radial { center, r0, r1, ry, stops } => {
            // Canvas2D's inner circle: the ramp starts at r0 / r1.
            let k0 = if *r1 > 0.0 { (r0 / r1).clamp(0.0, 1.0) } else { 0.0 };
            let stops = stops.iter().map(|(o, c)| stop(md, k0 + o * (1.0 - k0), *c)).collect();
            let c = px(*center);
            VectorPaint::RadialGradient { cx: c[0] as f32, cy: c[1] as f32, rx: (r1 * s) as f32, ry: (ry * s) as f32, stops }
        }
        Gradient::Linear { from, to, stops } => {
            let stops = stops.iter().map(|(o, c)| stop(md, *o, *c)).collect();
            let (a, b) = (px(*from), px(*to));
            VectorPaint::LinearGradient { x0: a[0] as f32, y0: a[1] as f32, x1: b[0] as f32, y1: b[1] as f32, stops }
        }
    };
    md.note_kind("draw2d gradient");
    let gi = md.draw2d.grad_next;
    while md.draw2d.grads.len() <= gi {
        let Some(dv) = cx.try_with_vm(|vm| Box::new(DrawVector::script_new_with_default(vm))) else {
            md.fresh_glyphs = true;
            md.fresh_why = "draw2d: no VM for a gradient draw";
            return;
        };
        md.draw2d.grads.push(dv);
    }
    md.draw2d.grad_next += 1;
    let dv = &mut md.draw2d.grads[gi];
    dv.begin();
    dv.cur_tolerance = 0.2;
    dv.path = geometry::without_tiny_segments(&path, 0.05);
    // the ramp is a row of this draw's gradient texture
    let stops = match &vp {
        VectorPaint::RadialGradient { stops, .. } | VectorPaint::LinearGradient { stops, .. } => stops.clone(),
        VectorPaint::Solid { .. } => Vec::new(),
    };
    dv.cur_gradient_row_v = dv.add_gradient_row(&stops);
    dv.set_paint(vp);
    dv.fill();
    dv.end(cx);
    let full = dv.geometry_full();
    md.geometry_full(full);
}

#[derive(Clone, Copy, PartialEq)]
enum RunKind {
    Segs,
    Fill,
    Barrier,
    /// The transform's linear part, scale removed (bits, so runs compare exactly).
    Text([u64; 4]),
}

fn run_kind(op: &Op) -> RunKind {
    match op {
        Op::Segs { .. } => RunKind::Segs,
        Op::Fill { .. } | Op::Gradient { .. } => RunKind::Fill,
        // Split out before runs are formed.
        Op::Erase { .. } | Op::EraseText(_) | Op::ClipBegin { .. } | Op::ClipEnd => RunKind::Barrier,
        Op::Text(t) => {
            let sc = scale_of(&t.m);
            let q = |v: f64| ((v / sc) * 1e9).round() / 1e9;
            RunKind::Text([q(t.m[0]).to_bits(), q(t.m[1]).to_bits(), q(t.m[2]).to_bits(), q(t.m[3]).to_bits()])
        }
    }
}

fn scale_of(m: &[f64; 6]) -> f64 {
    (m[0] * m[3] - m[1] * m[2]).abs().sqrt().max(1e-12)
}

/// A run key's linear part, and whether it is more than identity.
fn unit_linear(key: RunKind) -> ([f64; 4], bool) {
    let RunKind::Text(bits) = key else { return ([1.0, 0.0, 0.0, 1.0], false) };
    let l = [f64::from_bits(bits[0]), f64::from_bits(bits[1]), f64::from_bits(bits[2]), f64::from_bits(bits[3])];
    let identity = (l[0] - 1.0).abs() < 1e-7 && l[1].abs() < 1e-7 && l[2].abs() < 1e-7 && (l[3] - 1.0).abs() < 1e-7;
    (l, !identity)
}

fn style(blend: SegBlend) -> lines::LineStyle {
    lines::LineStyle {
        space: lines::Space::Screen,
        blend: match blend {
            SegBlend::Normal => lines::Blend::Over,
            SegBlend::Add => lines::Blend::Add,
            SegBlend::Max => lines::Blend::Max,
        },
        min_px: 0.7,
        round_caps: true,
        ..Default::default()
    }
}

fn bare_item() -> Item {
    Item {
        path: String::new(),
        world: Affine::IDENTITY,
        local: Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
        opacity: 1.0,
        blur: 0.0,
        feather: 0.0,
        feather_side: Default::default(),
        effects: Vec::new(),
        blend: Blend::Normal,
        is_static: false,
        kind: ItemKind::Group { children: Vec::new(), mask: None, mask_mode: Default::default(), size: None },
    }
}

/// One text op. Pixels of the target are `s` per layer unit; `turn` is the
/// run's unit linear map (drawn through the list's view transform) or none.
fn draw_text(md: &mut MotionDraw, cx: &mut Cx2d, view: &lines::LineView, t: &TextOp, s: f64, turn: Option<[f64; 4]>) {
    let sc = scale_of(&t.m);
    let k = s * sc; // pixels per text unit
    let px = t.px * k;
    if px < 0.05 {
        return;
    }
    // Where the text's own origin (0, 0) lands, in list coordinates: pass
    // pixels, or the unturned frame of the view transform.
    let origin = [t.m[4] * s, t.m[5] * s];
    let origin = match turn {
        Some(l) => {
            let det = l[0] * l[3] - l[1] * l[2];
            [(l[3] * origin[0] - l[2] * origin[1]) / det, (-l[1] * origin[0] + l[0] * origin[1]) / det]
        }
        None => origin,
    };
    let to_list = |u: f64, v: f64| [origin[0] + u * k, origin[1] + v * k];
    if !crate::text::set_face(md, cx, &t.font, t.weight, t.italic, &t.axes, px * 0.75) {
        return;
    }
    // The run's metrics, kept across frames by text and face (a string drawn
    // again costs no second layout: draw_abs lays it out to draw it).
    let key = RunKey { text: t.text.clone(), font: t.font.clone(), weight: t.weight.to_bits(), italic: t.italic, axes: t.axes.iter().map(|(a, v)| (a.clone(), v.to_bits())).collect(), px: (px * 64.0).round() as i64 };
    if !md.draw2d.runs.contains_key(&key) {
        let laid = md.draws.text.layout(cx, 0.0, 0.0, None, false, Align::default(), &t.text);
        let Some(row) = laid.rows.first() else { return };
        let base = laid.text.as_ptr() as usize;
        let row_start = (row.text.as_ptr() as usize).saturating_sub(base);
        let run = RunMetrics {
            width: row.width_in_lpxs as f64,
            asc: row.ascender_in_lpxs as f64,
            desc: row.descender_in_lpxs as f64,
            top_to_base: row.origin_in_lpxs.y as f64,
            pens: row.glyphs.iter().map(|g| (row_start + g.cluster, row.origin_in_lpxs.x as f64 + g.origin_in_lpxs.x as f64)).collect(),
        };
        if md.draw2d.runs.len() > 4096 {
            md.draw2d.runs.clear();
        }
        md.draw2d.runs.insert(key.clone(), run);
    }
    let run = md.draw2d.runs[&key].clone();
    let n = t.text.chars().count() as f64;
    // Canvas2D letterSpacing follows every character (the last too).
    let width = run.width + t.ls * k * n;
    // Baseline in list pixels.
    let base_y = match t.baseline {
        Baseline::Alphabetic => 0.0,
        Baseline::Top => run.asc,
        Baseline::Middle => (run.asc + run.desc) * 0.5,
        Baseline::Bottom => run.desc,
    };
    let p0 = to_list(t.x, t.y);
    let x0 = p0[0] - t.align * width;
    let y_top = p0[1] + base_y - run.top_to_base;
    if t.stroke > 0.0 {
        // The outlines hang from the font's own ascender line, `lift` below
        // the layout's top (see text::outline_unit).
        let Some(face) = crate::text::face(cx, &t.font, t.weight, t.italic) else { return };
        let lift = face.font_family.ascender_fudge_in_ems() as f64 * px;
        stroke_text(md, cx, view, t, [x0, y_top + lift], px, t.stroke * k, turn);
        return;
    }
    crate::text::note_glyphs(md, &t.font, t.weight, t.italic, &t.axes, px * 0.75, &t.text);
    let lc = md.lin([t.color[0], t.color[1], t.color[2], 1.0]);
    md.draws.text.color = vec4(lc[0] as f32, lc[1] as f32, lc[2] as f32, t.color[3] as f32);
    // A clip is a rect of the text's units: the turtle's clip stack (it
    // writes every glyph instance's clip), in list coordinates.
    if let Some(c) = t.clip {
        let a = to_list(c[0], c[1]);
        let b = to_list(c[2], c[3]);
        let (lo, hi) = ([a[0].min(b[0]), a[1].min(b[1])], [a[0].max(b[0]), a[1].max(b[1])]);
        cx.push_clip_rect(Rect { pos: dvec2(lo[0], lo[1]), size: dvec2(hi[0] - lo[0], hi[1] - lo[1]) });
    }
    if t.ls == 0.0 {
        md.draws.text.draw_abs(cx, dvec2(x0, y_top), &t.text);
    } else {
        // Letter by letter at the laid-out pen plus the spacing so far.
        for (ci, (b, ch)) in t.text.char_indices().enumerate() {
            if ch.is_whitespace() {
                continue;
            }
            let Some(&(_, pen)) = run.pens.iter().find(|(at, _)| *at == b) else { continue };
            let end = b + ch.len_utf8();
            md.draws.text.draw_abs(cx, dvec2(x0 + pen + t.ls * k * ci as f64, y_top), &t.text[b..end]);
        }
    }
    if t.clip.is_some() {
        cx.pop_clip_rect();
    }
}

/// A text's outlines stroked (Canvas2D strokeText): the glyph outlines as
/// segments of `width` pixels, `at` the outlines' origin (list pixels).
#[allow(clippy::too_many_arguments)]
fn stroke_text(md: &mut MotionDraw, cx: &mut Cx2d, view: &lines::LineView, t: &TextOp, at: [f64; 2], px: f64, width: f64, turn: Option<[f64; 4]>) {
    let Some(outline) = makepad_motion::text_outline_contours(&t.text, &t.font, px * 0.75, t.weight, t.italic, &t.axes) else { return };
    let lc = md.lin([t.color[0], t.color[1], t.color[2], 1.0]);
    let color = [lc[0] as f32, lc[1] as f32, lc[2] as f32, t.color[3] as f32];
    let mut batch = lines::LineBatch::new(style(SegBlend::Normal));
    let to_pass = |p: [f64; 2]| -> [f32; 3] {
        let q = [at[0] + p[0], at[1] + p[1]];
        match turn {
            Some(l) => [(l[0] * q[0] + l[2] * q[1]) as f32, (l[1] * q[0] + l[3] * q[1]) as f32, 0.0],
            None => [q[0] as f32, q[1] as f32, 0.0],
        }
    };
    // The segments draw in pass pixels (the line renderer ignores the list's
    // view transform): the turn applied here.
    let s = view.px_scale as f64;
    for contour in outline.iter() {
        let closed = contour.iter().chain(contour.first());
        let points: Vec<[f64; 2]> = closed.copied().collect();
        for w in points.windows(2) {
            let (a, b) = (to_pass(w[0]), to_pass(w[1]));
            batch.push(lines::Segment::new([a[0] / s as f32, a[1] / s as f32, 0.0], [b[0] / s as f32, b[1] / s as f32, 0.0], (width / s) as f32, color));
        }
    }
    md.note_kind("draw2d lines");
    if let Some(lr) = md.draw2d.lines.as_mut() {
        lr.draw_lines(cx, view, &batch);
    }
}

