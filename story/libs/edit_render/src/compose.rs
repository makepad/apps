//! The compositor: one [`FramePlan`] drawn into offscreen passes, one pass
//! per drawing op, every op at its own texture size, and the plan's result
//! placed over black in the output target.
//!
//! Ordering is stated, never left to pass-pool ids: every op records its
//! first and last pass (an effect's scene and chain passes come first,
//! under the op's own pass), and each op's last pass is made the child of
//! the next op's first, so the whole frame executes in plan order (a plan
//! only reads earlier slots). The output pass stays the child of whatever
//! pass the host is drawing.

use makepad_story_cut::reframe::Rect as CutRect;
use makepad_story_cut::{Blend, DrawKind, FramePlan};
use makepad_stage_render_kit::{Anim, Effect, FxContext, ParamKind, RenderKit, Value};
use makepad_story_cut::EffectRef;
use makepad_content_packs::effects::EffectView;
use makepad_motion_draw::{blend_mode, DrawMotionBlend};
use makepad_widgets::makepad_platform::video_file::{VideoColor, VideoPrimaries};
use makepad_widgets::*;
use std::collections::HashMap;

/// Records a pooled op target stays allocated without a use.
const OP_IDLE_FRAMES: u32 = 90;

script_mod! {
    use mod.prelude.widgets_internal.*

    // A picture's `src` rectangle (uv) drawn into the quad, at `opacity`
    // (targets hold premultiplied colour, so everything scales). `src` and
    // `opacity` are the Rust struct's instance fields.
    set_type_default() do #(DrawEditTexture::script_shader(vm)){
        ..mod.draw.DrawQuad
        tex: texture_2d(float)

        pixel: fn() {
            let uv = self.src.xy + self.pos * self.src.zw
            return self.tex.sample(clamp(uv, vec2(0.0, 0.0), vec2(1.0, 1.0))) * self.opacity
        }
    }

    // A camera's picture: its `crop` rectangle (uv of the frame) turned
    // `turns` quarter turns clockwise, mirrored, corrected, and the
    // `window` of that (fractions of the turned picture) drawn into the
    // quad. The work is the camera layer's crop, rotate, mirror and adjust.
    set_type_default() do #(DrawEditCamera::script_shader(vm)){
        ..mod.draw.DrawQuad
        tex: texture_2d(float)

        pixel: fn() {
            var p = self.window.xy + self.pos * self.window.zw
            if self.mirror > 0.5 {
                p = vec2(1.0 - p.x, p.y)
            }
            var q = p
            if self.turns > 2.5 {
                q = vec2(1.0 - p.y, p.x)
            } else if self.turns > 1.5 {
                q = vec2(1.0 - p.x, 1.0 - p.y)
            } else if self.turns > 0.5 {
                q = vec2(p.y, 1.0 - p.x)
            }
            let uv = self.crop.xy + q * self.crop.zw
            let c = self.tex.sample(clamp(uv, self.crop.xy, self.crop.xy + self.crop.zw))
            let grey = dot(c.xyz, vec3(0.299, 0.587, 0.114))
            var rgb = mix(vec3(grey, grey, grey), c.xyz, self.adjust.z)
            rgb = (rgb - vec3(0.5, 0.5, 0.5)) * self.adjust.y + vec3(0.5, 0.5, 0.5) + vec3(self.adjust.x, self.adjust.x, self.adjust.x)
            rgb = clamp(rgb, vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0))
            return vec4(rgb, 1.0) * self.opacity
        }
    }

    // The encoder's picture of the output (an export), drawn
    // into a target of its own after the output. `enc`: the encoded size,
    // how many output pixels a side each encoded pixel averages (1, or 2
    // for a half-size render) and whether the picture's primaries are not
    // BT.709 (its light converted by `gamut_*`, `mod.draw.VideoColor`); `rows`: this target's texels across, its
    // rows, its first row of the whole picture, and whether its bytes are
    // read as BGRA (a shared texture) rather than RGBA (a readback). As
    // NV12 (`form` 0) each texel holds four bytes of a plane row: the luma
    // rows, then the interleaved Cb Cr rows at half height, each byte the
    // `ycoef`/`ucoef`/`vcoef` row (r, g, b weights and the offset) of a
    // pixel or of its 2x2 block's mean. As BGRA (`form` 1, an alpha
    // export) each texel is the pixel, premultiplied, its alpha kept.
    set_type_default() do #(DrawEditPicture::script_shader(vm)){
        ..mod.draw.DrawQuad
        tex: texture_2d(float)

        encoded: fn(x: float, y: float) -> vec4 {
            let uv = (vec2(x, y) + vec2(0.5, 0.5)) * self.enc.z / self.src_size
            if self.enc.z > 1.5 {
                return self.tex.sample(uv)
            }
            return self.tex.sample_nearest(uv)
        }

        // An encoded pixel's R'G'B' in the picture's primaries.
        rgb: fn(x: float, y: float) -> vec3 {
            let c = self.encoded(x, y).xyz
            if self.enc.w > 0.5 {
                return mod.draw.VideoColor.from_bt709(c, self.gamut_r, self.gamut_g, self.gamut_b)
            }
            return c
        }

        luma: fn(x: float, y: float) -> float {
            return clamp(dot(self.rgb(x, y), self.ycoef.xyz) + self.ycoef.w, 0.0, 1.0)
        }

        chroma: fn(j: float, k: float) -> vec2 {
            let c = (self.rgb(2.0 * j, 2.0 * k) + self.rgb(2.0 * j + 1.0, 2.0 * k) + self.rgb(2.0 * j, 2.0 * k + 1.0) + self.rgb(2.0 * j + 1.0, 2.0 * k + 1.0)) * 0.25
            return clamp(vec2(dot(c, self.ucoef.xyz) + self.ucoef.w, dot(c, self.vcoef.xyz) + self.vcoef.w), vec2(0.0, 0.0), vec2(1.0, 1.0))
        }

        pixel: fn() {
            let x = floor(self.pos.x * self.rows.x)
            let y = floor(self.pos.y * self.rows.y) + self.rows.z
            if self.form > 0.5 {
                return self.encoded(x, y)
            }
            var out = vec4(0.0, 0.0, 0.0, 0.0)
            if y < self.enc.y {
                out = vec4(self.luma(4.0 * x, y), self.luma(4.0 * x + 1.0, y), self.luma(4.0 * x + 2.0, y), self.luma(4.0 * x + 3.0, y))
            } else {
                let a = self.chroma(2.0 * x, y - self.enc.y)
                let b = self.chroma(2.0 * x + 1.0, y - self.enc.y)
                out = vec4(a.x, a.y, b.x, b.y)
            }
            if self.rows.w > 0.5 {
                return vec4(out.z, out.y, out.x, out.w)
            }
            return out
        }
    }

    mod.widgets.CutDraws = set_type_default() do #(CutDraws::script_component(vm)){
        title +: {
            text_style: theme.font_bold{font_size: 32}
            color: #fff
        }
    }
}

/// A video_cut blend as `DrawMotionBlend`'s mode.
fn blend_mode_of(blend: Blend) -> f32 {
    match blend {
        Blend::Normal => blend_mode::NORMAL,
        Blend::Add => blend_mode::ADD,
        Blend::Screen => blend_mode::SCREEN,
        Blend::Multiply => blend_mode::MULTIPLY,
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawEditTexture {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub src: Vec4f,
    #[live]
    pub opacity: f32,
}

/// The encoder's picture of the output (see the shader).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawEditPicture {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub src_size: Vec2f,
    #[live]
    pub enc: Vec4f,
    #[live]
    pub rows: Vec4f,
    #[live]
    pub ycoef: Vec4f,
    #[live]
    pub ucoef: Vec4f,
    #[live]
    pub vcoef: Vec4f,
    #[live]
    pub gamut_r: Vec3f,
    #[live]
    pub gamut_g: Vec3f,
    #[live]
    pub gamut_b: Vec3f,
    #[live]
    pub form: f32,
}

/// A camera picture drawn with its `crop` (uv of the frame), `turns`
/// clockwise quarter turns, `mirror` (above 0.5: flipped), `adjust`
/// (brightness, contrast, saturation) and the `window` of the turned
/// picture the quad shows (fractions).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawEditCamera {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub crop: Vec4f,
    #[live]
    pub window: Vec4f,
    #[live]
    pub opacity: f32,
    #[live]
    pub turns: f32,
    #[live]
    pub mirror: f32,
    #[live]
    pub adjust: Vec4f,
}

/// The compositor's draw shaders and title text.
#[derive(Script, ScriptHook)]
pub struct CutDraws {
    #[live]
    texture: DrawEditTexture,
    #[live]
    camera: DrawEditCamera,
    #[live]
    nv12: DrawNv12,
    #[live]
    #[live]
    blend: DrawMotionBlend,
    #[live]
    picture: DrawEditPicture,
    #[live]
    title: DrawText,
}

/// Caption rows: word ranges that fit `max` (a word wider than `max` gets
/// its own row), with each row's width.
pub fn caption_rows(widths: &[f64], space: f64, max: f64) -> Vec<(std::ops::Range<usize>, f64)> {
    let mut rows = Vec::new();
    let (mut start, mut width) = (0, 0.0);
    for (i, w) in widths.iter().enumerate() {
        let wider = if i == start { *w } else { width + space + w };
        if i > start && wider > max {
            rows.push((start..i, width));
            start = i;
            width = *w;
        } else {
            width = wider;
        }
    }
    if start < widths.len() {
        rows.push((start..widths.len(), width));
    }
    rows
}

/// How far a recorded frame has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Painted {
    Painting,
    /// Every draw of it is in its target.
    Whole,
}

/// A source picture as the compositor gets it.
pub enum SourcePicture {
    /// A reader frame in its NV12 texture (`Texture::new_nv12`), `width` x
    /// `height`;
    /// `color`: the frame's colour (the GPU converts it to display RGB);
    /// `next`: a frame blend's following frame (of the same colour) and how much of it.
    Nv12 { texture: Texture, width: u32, height: u32, color: VideoColor, next: Option<(Texture, f32)> },
    /// A still, BGRA, `width` x `height`.
    Image { texture: Texture, width: u32, height: u32 },
}

/// The picture a host holds for an `AppView` node: a presented app window
/// in its swapchain image (`size` pixels of it, the share `scale` of the
/// image's width and height), for one record.
#[derive(Clone)]
pub struct ExternalFrame {
    pub texture: Texture,
    pub size: (u32, u32),
    pub scale: [f32; 2],
}

/// The tone behind an `AppView` placeholder.
const APP_PLACEHOLDER: Vec4f = Vec4f { x: 0.05, y: 0.06, z: 0.08, w: 1.0 };

/// Per-frame values an effect unit is driven with.
pub struct EffectDrive {
    pub local_time: f64,
    pub beat: f64,
    /// The host's frame step, the `dt` signal.
    pub dt: f64,
}

/// The form of an export's pictures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureForm {
    /// NV12 in the plan's colour (the encoder's own format).
    Nv12,
    /// BGRA, premultiplied, its alpha kept (HEVC with alpha).
    Bgra,
}

/// How a render hands each frame to its encoder: the output converted on
/// the GPU into the encoder's picture, in targets of its own (a ring of
/// [`PICTURE_SLOTS`], so frames convert while earlier ones are read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PicturePlan {
    /// The encoded size: the output's, or its half (a half-size render).
    pub size: (u32, u32),
    /// Output pixels a side per encoded pixel: 1, or 2 for half size.
    pub scale: u32,
    pub form: PictureForm,
    /// The NV12 picture's matrix and range (an output in other primaries
    /// converts its light first).
    pub color: VideoColor,
    /// Shared textures, read by the encoder by their handle (no readback);
    /// otherwise read back, in strips of at most `strip_rows` rows.
    pub shared: bool,
    pub strip_rows: Option<u32>,
}

impl PicturePlan {
    /// The picture's texels: across, and rows (NV12: four bytes of a plane
    /// row a texel, luma rows then chroma rows; BGRA: one pixel a texel).
    pub fn texels(&self) -> (u32, u32) {
        let (w, h) = self.size;
        match self.form {
            PictureForm::Nv12 => (w.div_ceil(4), h + h / 2),
            PictureForm::Bgra => (w, h),
        }
    }
}

/// Picture targets an export keeps in flight.
pub const PICTURE_SLOTS: usize = 4;

/// One picture target: its strips, each with its first row.
struct PictureSlot {
    strips: Vec<(RenderTarget, u32)>,
    busy: bool,
}

pub struct Compositor {
    draws: CutDraws,
    black: Texture,
    pool: RenderTargetPool,
    /// Two output targets, alternating: the one last recorded, and the
    /// one before it, still whole while the next frame paints.
    outputs: [Option<RenderTarget>; 2],
    current: usize,
    /// Horizontal strips of the output, each within the platform's
    /// readback budget, when the whole frame is not.
    strip_rows: Option<u32>,
    strips: Vec<(RenderTarget, u32)>,
    /// An export's pictures (see [`PicturePlan`]), the ring of their
    /// targets and the slot the last recorded frame drew into.
    picture: Option<PicturePlan>,
    pictures: Vec<PictureSlot>,
    picture_slot: Option<usize>,
    /// The output keeps its transparency (an alpha export) instead of
    /// lying over opaque black.
    transparent: bool,
    /// The passes of the last recorded frame, and the effects in it.
    recorded: Option<(u64, Vec<DrawPassId>, Vec<String>)>,
    /// The recorded frame's pass order (each op's first and last pass), to
    /// restore after a reattach: attaching resets parents, and a graphic
    /// whose Scene3D records a later sample (temporal AA, motion blur)
    /// repaints only when the op reading it is its parent again.
    recorded_spans: Vec<(DrawPassId, DrawPassId)>,
    /// R2's render kit, for `kit/<name>` effects.
    kit: RenderKit,
    /// Motion graphics and titles (lane M1, `crate::graphic`).
    pub graphics: crate::graphic::Graphics,
    /// For the next record: per graphic node, the videos its document shows
    /// (place in the document, the picture's key in `sources`).
    pub graphic_videos: HashMap<String, Vec<(String, String)>>,
    /// For the next record: per graphic node, its Freezes' edit pictures
    /// (place in the document, the slots under the graphic to stack).
    pub graphic_edit_frames: HashMap<String, Vec<(String, Vec<(usize, Blend)>)>>,
    /// The output the host shows: records go to the other one.
    pub hold: Option<Texture>,
    /// For the next record: per `AppView` node, the picture its app presents
    /// (a node without one draws the placeholder). The host's map for one
    /// render, not a global.
    pub externals: HashMap<String, ExternalFrame>,
    /// Where the last record placed each `AppView` picture, in its op's
    /// texture pixels (the output's, for a layer straight into the result).
    pub placed: HashMap<String, CutRect>,
}

/// Scale targets and placements together. Crops remain in their input
/// texture's coordinate space, while media UVs keep their original units.
fn raster_plan(plan: &FramePlan, size: (u32, u32)) -> FramePlan {
    let mut raster = plan.clone();
    raster.size = size;
    let scale = [size.0 as f64 / plan.size.0.max(1) as f64, size.1 as f64 / plan.size.1.max(1) as f64];
    for op in &mut raster.ops {
        op.size = ((op.size.0 as f64 * scale[0]).round().max(1.0) as u32,
            (op.size.1 as f64 * scale[1]).round().max(1.0) as u32);
    }
    let sizes: Vec<_> = raster.ops.iter().map(|op| op.size).collect();
    let scaled = |r: makepad_story_cut::reframe::Rect, x: f64, y: f64| makepad_story_cut::reframe::Rect::new(r.x*x, r.y*y, r.w*x, r.h*y);
    for (index, op) in raster.ops.iter_mut().enumerate() {
        let original = &plan.ops[index];
        let x = op.size.0 as f64 / original.size.0.max(1) as f64;
        let y = op.size.1 as f64 / original.size.1.max(1) as f64;
        match &mut op.kind {
            DrawKind::Source { dst_rect: Some(dst), .. } => *dst = scaled(*dst, x, y),
            DrawKind::Text { position, font_size, .. } => { position[0] *= x; position[1] *= y; *font_size *= y; },
            DrawKind::Reframe { input, crop, dst_rect } => {
                *crop = scaled(*crop, sizes[*input].0 as f64 / plan.ops[*input].size.0.max(1) as f64,
                    sizes[*input].1 as f64 / plan.ops[*input].size.1.max(1) as f64);
                *dst_rect = scaled(*dst_rect, x, y);
            },
            _ => {},
        }
    }
    raster
}

impl Compositor {
    pub fn new(cx: &mut Cx) -> Self {
        let draws = cx.with_vm(|vm| CutDraws::script_new_with_default(vm));
        let black = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
            width: 2, height: 2, data: Some(vec![0xff00_0000; 4]), updated: TextureUpdated::Full,
        });
        Self {
            draws, black, pool: RenderTargetPool::new(OP_IDLE_FRAMES), outputs: [None, None], current: 0, strip_rows: None, strips: Vec::new(),
            picture: None, pictures: Vec::new(), picture_slot: None, transparent: false, recorded: None, recorded_spans: Vec::new(),
            kit: RenderKit::new(cx),
            graphics: crate::graphic::Graphics::new(),
            graphic_videos: HashMap::new(),
            graphic_edit_frames: HashMap::new(),
            hold: None,
            externals: HashMap::new(),
            placed: HashMap::new(),
        }
    }

    /// `layers` (slots, bottom first) blended in order at `size`: one
    /// layer is itself, no pass.
    fn stack(&mut self, cx: &mut Cx2d, slots: &[Option<Texture>], layers: &[(usize, Blend)], size: (u32, u32), spans: &mut Vec<(DrawPassId, DrawPassId)>, passes: &mut Vec<DrawPassId>) -> Option<Texture> {
        let mut below: Option<Texture> = None;
        for (at, mode) in layers {
            let Some(layer) = slots[*at].clone() else { continue };
            let Some(under) = below.take() else {
                below = Some(layer);
                continue;
            };
            let index = self.pool.take(cx, size, RenderTargetFormat::Bgra8, "edit_stack");
            let pass = &mut self.pool[index];
            pass.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
            let draw = &mut self.draws.blend;
            draw.draw_vars.set_texture(0, &under);
            draw.draw_vars.set_texture(1, &layer);
            draw.mode = blend_mode_of(*mode);
            draw.amount = 1.0;
            draw.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(size.0 as f64, size.1 as f64) });
            pass.end(cx);
            let id = pass.pass.draw_pass_id();
            spans.push((id, id));
            passes.push(id);
            below = Some(pass.texture.clone());
        }
        below
    }

    /// `picture` as the output shows it: over the output's opaque black,
    /// as [`Self::finish`] lays the result (an alpha export keeps its
    /// transparency, and the picture is returned as it is). A Freeze's
    /// picture is the frame the cut showed: what is under the graphic does
    /// not show through the frozen frame's transparent parts (a dissolve
    /// into a fitted clip leaves its bars part transparent).
    fn as_shown(&mut self, cx: &mut Cx2d, picture: &Texture, size: (u32, u32), spans: &mut Vec<(DrawPassId, DrawPassId)>, passes: &mut Vec<DrawPassId>) -> Texture {
        if self.transparent {
            return picture.clone();
        }
        let index = self.pool.take(cx, size, RenderTargetFormat::Bgra8, "edit_freeze");
        let pass = &mut self.pool[index];
        pass.begin(cx, vec4(0.0, 0.0, 0.0, 1.0));
        let draw = &mut self.draws.texture;
        draw.draw_vars.set_texture(0, picture);
        draw.src = vec4(0.0, 0.0, 1.0, 1.0);
        draw.opacity = 1.0;
        draw.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(size.0 as f64, size.1 as f64) });
        pass.end(cx);
        let id = pass.pass.draw_pass_id();
        spans.push((id, id));
        passes.push(id);
        pass.texture.clone()
    }

    /// The target the last recorded frame lands in.
    pub fn output(&self) -> Option<&Texture> {
        self.outputs[self.current].as_ref().map(|out| &out.texture)
    }

    /// Read the output back in horizontal strips of at most `rows` rows.
    pub fn set_readback_strips(&mut self, rows: Option<u32>) {
        self.strip_rows = rows;
    }

    /// Convert every recorded frame into the encoder's picture (`None`:
    /// no pictures, the output alone).
    pub fn set_picture(&mut self, plan: Option<PicturePlan>) {
        if self.picture != plan {
            self.pictures.clear();
            self.picture_slot = None;
        }
        self.picture = plan;
    }

    /// Keep the output's transparency (an alpha export).
    pub fn set_transparent(&mut self, transparent: bool) {
        self.transparent = transparent;
    }

    /// Whether a picture slot is free for the next record (always, without
    /// pictures).
    pub fn picture_free(&self) -> bool {
        self.picture.is_none() || self.pictures.len() < PICTURE_SLOTS || self.pictures.iter().any(|slot| !slot.busy)
    }

    /// The last recorded frame's picture, whole: its slot and its targets,
    /// top down, each with its first row. Taken, the slot is the frame's
    /// (no record draws into it) until [`Self::release_picture`]; a frame
    /// recorded again before it was taken draws into the same free slot.
    pub fn take_picture(&mut self) -> Option<(usize, Vec<(Texture, u32)>)> {
        let slot = self.picture_slot.take()?;
        self.pictures[slot].busy = true;
        Some((slot, self.pictures[slot].strips.iter().map(|(target, y)| (target.texture.clone(), *y)).collect()))
    }

    /// The encoder is done with a picture's targets.
    pub fn release_picture(&mut self, slot: usize) {
        if let Some(slot) = self.pictures.get_mut(slot) {
            slot.busy = false;
        }
    }

    /// The textures the last recorded frame is read back from, top down,
    /// each with its first row.
    pub fn readback_targets(&self) -> Vec<(Texture, u32)> {
        if self.strip_rows.is_some() && !self.strips.is_empty() {
            return self.strips.iter().map(|(strip, y)| (strip.texture.clone(), *y)).collect();
        }
        self.output().map(|texture| vec![(texture.clone(), 0)]).unwrap_or_default()
    }

    fn quad_rect(rect: CutRect) -> Rect {
        Rect { pos: dvec2(rect.x, rect.y), size: dvec2(rect.w, rect.h) }
    }

    /// Record `plan` into passes under the pass being drawn. `sources`
    /// holds every source picture the plan reads (a missing one draws
    /// nothing), by the node that draws it; `effects` the effect unit of every Fx and Transition node
    /// (by node id). Returns the output target.
    pub fn record(
        &mut self,
        cx: &mut Cx2d,
        plan: &FramePlan,
        sources: &HashMap<String, SourcePicture>,
        effects: &mut HashMap<String, Box<dyn EffectView>>,
        dt: f64,
        raster_size: Option<(u32, u32)>,
    ) -> Texture {
        let logical = plan;
        let raster;
        let plan = if let Some(size) = raster_size {
            raster = raster_plan(plan, size);
            &raster
        } else { plan };
        let transparent = vec4(0.0, 0.0, 0.0, 0.0);
        self.kit.begin_frame(cx);
        self.graphics.begin_frame(cx.cx);
        self.placed.clear();
        let mut slots: Vec<Option<Texture>> = Vec::with_capacity(plan.ops.len());
        // Whether each slot's picture is final this record: a graphic that
        // drew not exact (its shutter still gathering in locked time) is
        // not, nor anything made from it.
        let mut settled: Vec<bool> = Vec::with_capacity(plan.ops.len());
        let mut spans: Vec<(DrawPassId, DrawPassId)> = Vec::new();
        let mut passes: Vec<DrawPassId> = Vec::new();
        let mut units: Vec<String> = Vec::new();
        self.pool.next_frame(cx);
        let black = self.black.clone();
        for (op_index, op) in plan.ops.iter().enumerate() {
            let slot = |slots: &Vec<Option<Texture>>, at: Option<usize>| at.and_then(|at| slots[at].clone());
            let texture = match &op.kind {
                DrawKind::Source { src_rect: Some(src), dst_rect: Some(dst), opacity, .. } => match sources.get(&op.node) {
                    Some(picture) if *opacity > 0.0 => {
                        let index = self.pool.take(cx, op.size, RenderTargetFormat::Bgra8, "cut_source");
                        let pass = &mut self.pool[index];
                        pass.begin(cx, transparent);
                        match picture {
                            SourcePicture::Nv12 { texture, width, height, color, next } => {
                                let draw = &mut self.draws.nv12;
                                draw.set_nv12(texture, *width, *height, *color, next.as_ref().map(|(next, blend)| (next, *blend)));
                                draw.src = vec4(src.x as f32, src.y as f32, src.w as f32, src.h as f32);
                                draw.opacity = *opacity as f32;
                                draw.draw_abs(cx, Self::quad_rect(*dst));
                            }
                            SourcePicture::Image { texture, width: w, height: h } => {
                                let draw = &mut self.draws.texture;
                                draw.draw_vars.set_texture(0, texture);
                                draw.src = vec4((src.x / *w as f64) as f32, (src.y / *h as f64) as f32, (src.w / *w as f64) as f32, (src.h / *h as f64) as f32);
                                draw.opacity = *opacity as f32;
                                draw.draw_abs(cx, Self::quad_rect(*dst));
                            }
                        }
                        pass.end(cx);
                        let id = pass.pass.draw_pass_id();
                        spans.push((id, id));
                        passes.push(id);
                        Some(pass.texture.clone())
                    }
                    _ => None,
                },
                DrawKind::Source { .. } => None,
                DrawKind::Text { text, position, font_size, karaoke, style, .. } => {
                    let index = self.pool.take(cx, op.size, RenderTargetFormat::Bgra8, "edit_title");
                    let pass = &mut self.pool[index];
                    pass.begin(cx, transparent);
                    self.draws.title.text_style.font_size = *font_size as f32;
                    if let Some(karaoke) = karaoke {
                        // A caption. `@movie` (the default): the
                        // whole line, in rows that fit the safe region's
                        // width, centred on the anchor and kept inside it, white over a soft shadow,
                        // no word-by-word progression. `karaoke/…`
                        // (opt-in): the same rows, the words spoken so far
                        // in the highlight colour and the one being spoken
                        // blending into it by its elapsed fraction.
                        let sung = style.starts_with("karaoke");
                        let words: Vec<String> = if karaoke.words.is_empty() {
                            text.split_whitespace().map(str::to_string).collect()
                        } else {
                            karaoke.words.clone()
                        };
                        let title = &mut self.draws.title;
                        // The caption's safe region in pixels (the
                        // platform bands kept clear); none: the frame.
                        let (fw, fh) = (op.size.0 as f64, op.size.1 as f64);
                        let safe = if karaoke.safe[2] > 0.0 && karaoke.safe[3] > 0.0 {
                            [karaoke.safe[0] * fw, karaoke.safe[1] * fh, karaoke.safe[2] * fw, karaoke.safe[3] * fh]
                        } else {
                            [0.0, 0.0, fw, fh]
                        };
                        // Measured at a size: word sizes, the space, the
                        // line height and the rows at the safe width.
                        let measure = |title: &mut DrawText, cx: &mut Cx2d, size: f64| {
                            title.text_style.font_size = size as f32;
                            let width = |title: &DrawText, cx: &mut Cx2d, text: &str| title.layout(cx, 0.0, 0.0, None, false, Align::default(), text).size_in_lpxs;
                            let space = width(title, cx, "a a").width as f64 - 2.0 * width(title, cx, "a").width as f64;
                            let sizes: Vec<(f64, f64)> = words.iter().map(|word| {
                                let size = width(title, cx, word);
                                (size.width as f64, size.height as f64)
                            }).collect();
                            let line = sizes.iter().map(|(_, h)| *h).fold(size * 1.2, f64::max);
                            let rows = caption_rows(&sizes.iter().map(|(w, _)| *w).collect::<Vec<_>>(), space, safe[2]);
                            (space, sizes, line, rows)
                        };
                        // Shrink until it is at most two rows inside the
                        // safe height, down to 0.6 of its size; below that
                        // only as far as the height needs.
                        let mut size = *font_size;
                        let (mut space, mut sizes, mut line, mut rows) = measure(title, cx, size);
                        let shadow_of = |size: f64| (size * 0.06).max(1.0);
                        loop {
                            let tall = rows.len() as f64 * line + shadow_of(size) > safe[3];
                            let over_floor = size > *font_size * 0.6 + 1e-9;
                            if !(tall || (rows.len() > 2 && over_floor)) || size < 1.0 {
                                break;
                            }
                            size *= 0.9;
                            if !tall && size < *font_size * 0.6 {
                                size = *font_size * 0.6;
                            }
                            (space, sizes, line, rows) = measure(title, cx, size);
                        }
                        let shadow = shadow_of(size);
                        // The block, shadow included, clamped into the safe
                        // region.
                        let block_w = rows.iter().map(|(_, w)| *w).fold(0.0, f64::max) + shadow;
                        let block_h = rows.len() as f64 * line + shadow;
                        let centre_x = if block_w > safe[2] { safe[0] + safe[2] * 0.5 } else { position[0].clamp(safe[0] + block_w * 0.5, safe[0] + safe[2] - block_w * 0.5) };
                        let centre_y = if block_h > safe[3] { safe[1] + safe[3] * 0.5 } else { position[1].clamp(safe[1] + block_h * 0.5, safe[1] + safe[3] - block_h * 0.5) };
                        let top = centre_y - block_h * 0.5;
                        let position = [centre_x - shadow * 0.5, 0.0];
                        let white = vec4(1.0, 1.0, 1.0, 1.0);
                        let lit = vec4(1.0, 0.82, 0.25, 1.0);
                        for (offset, shade) in [(shadow, Some(vec4(0.0, 0.0, 0.0, 0.7))), (0.0, None)] {
                            for (r, (range, width)) in rows.iter().enumerate() {
                                let mut x = position[0] - width * 0.5 + offset;
                                let y = top + r as f64 * line + offset;
                                for i in range.clone() {
                                    let spoken = if sung { (karaoke.highlight - i as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
                                    title.color = shade.unwrap_or(white + (lit - white) * spoken);
                                    title.draw_abs(cx, dvec2(x, y), &words[i]);
                                    x += sizes[i].0 + space;
                                }
                            }
                        }
                        title.color = white;
                    } else {
                        let (w, h) = (op.size.0 as f64, (*font_size * 3.0).max(1.0));
                        // Centred on the anchor: a box as wide as the frame
                        // whose middle is the anchor.
                        let walk = Walk::abs_rect(Rect { pos: dvec2(position[0] - w * 0.5, position[1] - h * 0.5), size: dvec2(w, h) });
                        self.draws.title.color = vec4(1.0, 1.0, 1.0, 1.0);
                        self.draws.title.draw_walk(cx, walk, Align { x: 0.5, y: 0.5 }, text);
                    }
                    pass.end(cx);
                    let id = pass.pass.draw_pass_id();
                    spans.push((id, id));
                    passes.push(id);
                    Some(pass.texture.clone())
                }
                DrawKind::Effect { effect: EffectRef::Preset(name), input, local_time, params, .. } if is_kit_effect(name) => {
                    // A render-kit effect (`kit/<name>`): p1..p4 are its
                    // first four number parameters, the rest their defaults.
                    let input = slot(&slots, *input).unwrap_or_else(|| black.clone());
                    match kit_effect(name, params) {
                        Ok(effect) => {
                            let fps = 1.0 / dt.max(1e-6);
                            let layers = HashMap::new();
                            let applied = self.kit.apply(cx, &[effect.at(*local_time)], &input, op.size, &FxContext::new(*local_time, fps, &layers));
                            if let (Some(first), Some(last)) = (applied.first, applied.last) {
                                spans.push((first, last));
                            }
                            Some(applied.texture)
                        }
                        // Refused at load; nothing to draw.
                        Err(_) => Some(input),
                    }
                }
                DrawKind::Effect { input, local_time, beat, params, .. } => match effects.get_mut(&op.node) {
                    Some(view) => {
                        let input = slot(&slots, *input).unwrap_or_else(|| black.clone());
                        view.set_input_texture(0, Some(input));
                        view.set_input_texture(1, None);
                        set_edit_dials(view.as_mut(), params, edit_dial(params[3]));
                        let drive = EffectDrive { local_time: *local_time, beat: *beat, dt };
                        units.push(op.node.clone());
                        Some(self.effect_pass(cx, op.size, view.as_mut(), &drive, &mut spans, &mut passes))
                    }
                    None => slot(&slots, *input),
                },
                DrawKind::Transition { a, b, mix, local_time, beat, params, .. } => {
                    let (ta, tb) = (slot(&slots, *a).unwrap_or_else(|| black.clone()), slot(&slots, *b).unwrap_or_else(|| black.clone()));
                    let drive = EffectDrive { local_time: *local_time, beat: *beat, dt };
                    self.transition_pass(cx, ta, tb, effects.get_mut(&op.node).map(|view| view.as_mut()), op.size, *mix, params, &drive, &op.node, &mut spans, &mut passes, &mut units)
                }
                DrawKind::Stack { layers } => self.stack(cx, &slots, layers, op.size, &mut spans, &mut passes),
                // A Motion document or a title (lane M1).
                DrawKind::Graphic { .. } => {
                    // Its videos' frames, NV12 to BGRA, first (the document
                    // samples them as textures).
                    for (path, key) in self.graphic_videos.get(&op.node).cloned().unwrap_or_default() {
                        let Some(SourcePicture::Nv12 { texture, width, height, color, .. }) = sources.get(&key) else { continue };
                        let (width, height, color) = (*width, *height, *color);
                        let index = self.pool.take(cx, (width, height), RenderTargetFormat::Bgra8, "edit_graphic_video");
                        let pass = &mut self.pool[index];
                        pass.begin(cx, transparent);
                        let draw = &mut self.draws.nv12;
                        draw.set_nv12(texture, width, height, color, None);
                        draw.opacity = 1.0;
                        draw.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(width as f64, height as f64) });
                        pass.end(cx);
                        let id = pass.pass.draw_pass_id();
                        spans.push((id, id));
                        passes.push(id);
                        let target = pass.texture.clone();
                        self.graphics.set_video_frame(cx, &op.node, &path, target, width, height);
                    }
                    // Its Freezes' edit pictures: the layers under it at their
                    // time, already drawn (slots before this op). One not
                    // final yet (a graphic in it gathering its shutter) is
                    // not summed into this one's shutter: it waits for it.
                    for (path, layers) in self.graphic_edit_frames.get(&op.node).cloned().unwrap_or_default() {
                        let picture = self.stack(cx, &slots, &layers, op.size, &mut spans, &mut passes).unwrap_or_else(|| black.clone());
                        let picture = self.as_shown(cx, &picture, op.size, &mut spans, &mut passes);
                        self.graphics.set_video_frame(cx, &op.node, &path, picture, op.size.0, op.size.1);
                        if let Some((slot, _)) = layers.iter().find(|(slot, _)| !settled[*slot]) {
                            self.graphics.set_pictures_pending(&op.node, format!("the picture of `{path}` waits for `{}`", plan.ops[*slot].node));
                        }
                    }
                    self.graphics.draw(cx, op, logical.ops[op_index].size, &mut spans)
                }
                DrawKind::App { app: label, fit, rect } | DrawKind::Code { label, fit, rect } => {
                    let index = self.pool.take(cx, op.size, RenderTargetFormat::Bgra8, "edit_app");
                    let frame = self.externals.get(&op.node).filter(|frame| frame.size.0 > 0 && frame.size.1 > 0).cloned();
                    let pass = &mut self.pool[index];
                    // A layer placed in a rectangle keeps what is under the rest of the picture.
                    pass.begin(cx, if frame.is_some() || rect.is_some() { transparent } else { APP_PLACEHOLDER });
                    match frame {
                        Some(frame) => {
                            let (src, dst) = makepad_story_cut::place_rects(frame.size, op.size, *fit, *rect);
                            let draw = &mut self.draws.texture;
                            draw.draw_vars.set_texture(0, &frame.texture);
                            draw.src = vec4(
                                (src.x / frame.size.0 as f64) as f32 * frame.scale[0],
                                (src.y / frame.size.1 as f64) as f32 * frame.scale[1],
                                (src.w / frame.size.0 as f64) as f32 * frame.scale[0],
                                (src.h / frame.size.1 as f64) as f32 * frame.scale[1],
                            );
                            draw.opacity = 1.0;
                            draw.draw_abs(cx, Self::quad_rect(dst));
                            self.placed.insert(op.node.clone(), dst);
                        }
                        None => {
                            // The dark frame naming the app (or the file), in its rectangle if it has one.
                            let (w, h) = (op.size.0 as f64, op.size.1 as f64);
                            let area = match rect {
                                Some([x, y, rw, rh]) => {
                                    let area = Rect { pos: dvec2(x * w, y * h), size: dvec2(rw * w, rh * h) };
                                    let draw = &mut self.draws.texture;
                                    draw.draw_vars.set_texture(0, &self.black);
                                    draw.src = vec4(0.0, 0.0, 1.0, 1.0);
                                    draw.opacity = 1.0;
                                    draw.draw_abs(cx, area);
                                    area
                                }
                                None => Rect { pos: dvec2(0.0, 0.0), size: dvec2(w, h) },
                            };
                            self.draws.title.text_style.font_size = (area.size.y * 0.06).max(8.0) as f32;
                            self.draws.title.color = vec4(0.62, 0.68, 0.76, 1.0);
                            let walk = Walk::abs_rect(Rect { pos: dvec2(area.pos.x, area.pos.y + area.size.y * 0.4), size: dvec2(area.size.x, area.size.y * 0.2) });
                            self.draws.title.draw_walk(cx, walk, Align { x: 0.5, y: 0.5 }, label);
                        }
                    }
                    pass.end(cx);
                    let id = pass.pass.draw_pass_id();
                    spans.push((id, id));
                    passes.push(id);
                    Some(pass.texture.clone())
                }
                DrawKind::Camera { spec } => {
                    let index = self.pool.take(cx, op.size, RenderTargetFormat::Bgra8, "edit_camera");
                    let frame = self.externals.get(&op.node).filter(|frame| frame.size.0 > 0 && frame.size.1 > 0).cloned();
                    let pass = &mut self.pool[index];
                    // A camera placed in a rectangle keeps what is under the rest of the picture.
                    pass.begin(cx, if frame.is_some() || spec.rect.is_some() { transparent } else { APP_PLACEHOLDER });
                    match frame {
                        Some(frame) => {
                            // The cropped picture, in pixels, turned: the size `fit` places.
                            let (cw, ch) = (frame.size.0 as f64 * spec.crop[2], frame.size.1 as f64 * spec.crop[3]);
                            let turned = if spec.rotate % 180 == 90 { (ch, cw) } else { (cw, ch) };
                            let turned = (turned.0.round().max(1.0) as u32, turned.1.round().max(1.0) as u32);
                            // `src` is the part of the turned picture that shows, `dst` where it lands.
                            let (src, dst) = makepad_story_cut::place_rects(turned, op.size, spec.fit, spec.rect);
                            let (tw, th) = (turned.0 as f64, turned.1 as f64);
                            let draw = &mut self.draws.camera;
                            draw.draw_vars.set_texture(0, &frame.texture);
                            draw.window = vec4((src.x / tw) as f32, (src.y / th) as f32, (src.w / tw) as f32, (src.h / th) as f32);
                            draw.crop = vec4(
                                spec.crop[0] as f32 * frame.scale[0],
                                spec.crop[1] as f32 * frame.scale[1],
                                spec.crop[2] as f32 * frame.scale[0],
                                spec.crop[3] as f32 * frame.scale[1],
                            );
                            draw.turns = (spec.rotate / 90) as f32;
                            draw.mirror = if spec.mirror { 1.0 } else { 0.0 };
                            draw.adjust = vec4(spec.adjust.brightness as f32, spec.adjust.contrast as f32, spec.adjust.saturation as f32, 0.0);
                            draw.opacity = 1.0;
                            draw.draw_abs(cx, Self::quad_rect(dst));
                            self.placed.insert(op.node.clone(), dst);
                        }
                        None => {
                            // The dark frame naming the device, in its rectangle if it has one.
                            let (w, h) = (op.size.0 as f64, op.size.1 as f64);
                            let area = match spec.rect {
                                Some([x, y, rw, rh]) => {
                                    let area = Rect { pos: dvec2(x * w, y * h), size: dvec2(rw * w, rh * h) };
                                    let draw = &mut self.draws.texture;
                                    draw.draw_vars.set_texture(0, &self.black);
                                    draw.src = vec4(0.0, 0.0, 1.0, 1.0);
                                    draw.opacity = 1.0;
                                    draw.draw_abs(cx, area);
                                    area
                                }
                                None => Rect { pos: dvec2(0.0, 0.0), size: dvec2(w, h) },
                            };
                            self.draws.title.text_style.font_size = (area.size.y * 0.06).max(8.0) as f32;
                            self.draws.title.color = vec4(0.62, 0.68, 0.76, 1.0);
                            let walk = Walk::abs_rect(Rect { pos: dvec2(area.pos.x, area.pos.y + area.size.y * 0.4), size: dvec2(area.size.x, area.size.y * 0.2) });
                            self.draws.title.draw_walk(cx, walk, Align { x: 0.5, y: 0.5 }, &format!("camera: {}", spec.device.label()));
                        }
                    }
                    pass.end(cx);
                    let id = pass.pass.draw_pass_id();
                    spans.push((id, id));
                    passes.push(id);
                    Some(pass.texture.clone())
                }
                DrawKind::Reframe { input, crop, dst_rect } => match slots[*input].clone() {
                    Some(input_texture) => {
                        let (iw, ih) = plan.ops[*input].size;
                        let index = self.pool.take(cx, op.size, RenderTargetFormat::Bgra8, "edit_reframe");
                        let pass = &mut self.pool[index];
                        pass.begin(cx, transparent);
                        let draw = &mut self.draws.texture;
                        draw.draw_vars.set_texture(0, &input_texture);
                        draw.src = vec4(
                            (crop.x / iw as f64) as f32, (crop.y / ih as f64) as f32,
                            (crop.w / iw as f64) as f32, (crop.h / ih as f64) as f32,
                        );
                        draw.opacity = 1.0;
                        draw.draw_abs(cx, Self::quad_rect(*dst_rect));
                        pass.end(cx);
                        let id = pass.pass.draw_pass_id();
                        spans.push((id, id));
                        passes.push(id);
                        Some(pass.texture.clone())
                    }
                    None => None,
                },
            };
            let inputs_settled = crate::engine::slot_inputs(&op.kind).into_iter().all(|slot| settled[slot]);
            settled.push(inputs_settled && (!matches!(op.kind, DrawKind::Graphic { .. }) || self.graphics.drew_final(&op.node)));
            slots.push(texture);
        }

        let result = plan.result.and_then(|at| slots[at].clone());
        self.finish(cx, result, plan.size, spans, passes, units)
    }

    /// The frame's last step: `result` over opaque black at the output
    /// size, in the output the host will show, then the strips and the pass
    /// order. Returns the output target.
    fn finish(&mut self, cx: &mut Cx2d, result: Option<Texture>, size: (u32, u32), mut spans: Vec<(DrawPassId, DrawPassId)>, mut passes: Vec<DrawPassId>, units: Vec<String>) -> Texture {
        // The output: the result over opaque black at the output size.
        // Never into the picture the host shows (a frame recorded again
        // before it painted would otherwise land in it, half drawn).
        if self.hold.as_ref().is_none_or(|held| self.outputs[1 - self.current].as_ref().is_none_or(|out| out.texture != *held)) {
            self.current = 1 - self.current;
        }
        let out = self.outputs[self.current].get_or_insert_with(|| RenderTarget::new(cx, "edit_output"));
        out.ensure(cx, size, RenderTargetFormat::Bgra8);
        out.begin(cx, if self.transparent { vec4(0.0, 0.0, 0.0, 0.0) } else { vec4(0.0, 0.0, 0.0, 1.0) });
        if let Some(result) = &result {
            let draw = &mut self.draws.texture;
            draw.draw_vars.set_texture(0, result);
            draw.src = vec4(0.0, 0.0, 1.0, 1.0);
            draw.opacity = 1.0;
            draw.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(size.0 as f64, size.1 as f64) });
        }
        out.end(cx);
        let id = out.pass.draw_pass_id();
        let output_target = out.texture.clone();
        spans.push((id, id));
        passes.push(id);
        // The strips a large frame is read back in: exact copies of rows.
        if let Some(rows) = self.strip_rows.filter(|rows| *rows < size.1) {
            let (width, height) = size;
            let count = height.div_ceil(rows) as usize;
            self.strips.truncate(count);
            for k in 0..count {
                let y = k as u32 * rows;
                let size = (width, rows.min(height - y));
                if self.strips.len() <= k {
                    self.strips.push((RenderTarget::new(cx, "edit_strip"), y));
                }
                let (strip, first) = &mut self.strips[k];
                *first = y;
                strip.ensure(cx, size, RenderTargetFormat::Bgra8);
                strip.begin(cx, vec4(0.0, 0.0, 0.0, 1.0));
                let draw = &mut self.draws.texture;
                draw.draw_vars.set_texture(0, &output_target);
                draw.src = vec4(0.0, y as f32 / height as f32, 1.0, size.1 as f32 / height as f32);
                draw.opacity = 1.0;
                draw.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(size.0 as f64, size.1 as f64) });
                strip.end(cx);
                let id = strip.pass.draw_pass_id();
                spans.push((id, id));
                passes.push(id);
            }
        } else {
            self.strips.clear();
        }
        // The encoder's picture, in a free slot of the ring.
        self.picture_slot = None;
        if let Some(plan) = self.picture {
            let free = self.pictures.iter().position(|slot| !slot.busy);
            let slot = match free {
                Some(slot) => Some(slot),
                None if self.pictures.len() < PICTURE_SLOTS => {
                    self.pictures.push(PictureSlot { strips: Vec::new(), busy: false });
                    Some(self.pictures.len() - 1)
                }
                None => None,
            };
            if let Some(slot) = slot {
                let (texels, rows) = plan.texels();
                let strip = if plan.shared { rows } else { plan.strip_rows.unwrap_or(rows).clamp(1, rows) };
                let count = rows.div_ceil(strip) as usize;
                let format = if plan.shared { RenderTargetFormat::SharedBgra8 } else { RenderTargetFormat::Bgra8 };
                let strips = &mut self.pictures[slot].strips;
                strips.truncate(count);
                for k in 0..count {
                    let first = k as u32 * strip;
                    let here = strip.min(rows - first);
                    if strips.len() <= k {
                        strips.push((RenderTarget::new(cx, "edit_picture"), first));
                    }
                    let (target, y) = &mut strips[k];
                    *y = first;
                    target.ensure_kept(cx, (texels, here), format);
                    target.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
                    let draw = &mut self.draws.picture;
                    draw.draw_vars.set_texture(0, &output_target);
                    let rows_of = plan.color.rgb_to_yuv_rows();
                    let coef = |row: [f32; 4]| vec4(row[0], row[1], row[2], row[3]);
                    draw.src_size = vec2(size.0 as f32, size.1 as f32);
                    let gamut = plan.color.primaries != VideoPrimaries::Bt709;
                    draw.enc = vec4(plan.size.0 as f32, plan.size.1 as f32, plan.scale as f32, if gamut { 1.0 } else { 0.0 });
                    let g = plan.color.gamut_from_bt709().map(|r| vec3(r[0] as f32, r[1] as f32, r[2] as f32));
                    (draw.gamut_r, draw.gamut_g, draw.gamut_b) = (g[0], g[1], g[2]);
                    draw.rows = vec4(texels as f32, here as f32, first as f32, if plan.shared { 1.0 } else { 0.0 });
                    draw.ycoef = coef(rows_of[0]);
                    draw.ucoef = coef(rows_of[1]);
                    draw.vcoef = coef(rows_of[2]);
                    draw.form = if plan.form == PictureForm::Bgra { 1.0 } else { 0.0 };
                    draw.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(texels as f64, here as f64) });
                    target.end(cx);
                    let id = target.pass.draw_pass_id();
                    spans.push((id, id));
                    passes.push(id);
                }
                self.picture_slot = Some(slot);
            }
        }
        // One total order: each op's last pass under the next op's first.
        for pair in spans.windows(2) {
            let ((_, last), (first, _)) = (pair[0], pair[1]);
            cx.set_pass_parent(last, first);
        }
        self.recorded = Some((cx.repaint_id(), passes, units));
        self.recorded_spans = spans;
        output_target
    }

    /// Mix picture `ta` into picture `tb` at `mix` (0 is `ta`, 1 is `tb`)
    /// with a transition: the one body of an edit's `Transition` op and of
    /// [`Self::transition`]. A transition without a preset (`view` None) is
    /// the dissolve. A preset that reads both pictures (`wants_deck_inputs`)
    /// gets them as its inputs and `mix` as its lever; any other transforms
    /// the dissolve, its lever rising to 1 at the middle and falling again.
    /// `params` are p1..p3 (below 0 leaves the preset's value). `unit` names
    /// the effect among the frame's units when the preset draws.
    #[allow(clippy::too_many_arguments)]
    fn transition_pass(
        &mut self,
        cx: &mut Cx2d,
        ta: Texture,
        tb: Texture,
        view: Option<&mut (dyn EffectView + 'static)>,
        size: (u32, u32),
        mix: f64,
        params: &[f64; 4],
        drive: &EffectDrive,
        unit: &str,
        spans: &mut Vec<(DrawPassId, DrawPassId)>,
        passes: &mut Vec<DrawPassId>,
        units: &mut Vec<String>,
    ) -> Option<Texture> {
        if mix <= 0.0 {
            return Some(ta);
        }
        if mix >= 1.0 {
            return Some(tb);
        }
        let decks = view.as_ref().is_some_and(|view| view.wants_deck_inputs());
        // The dissolve: what a transition without a preset is, and what a
        // single-input preset transforms.
        let premix = (!decks).then(|| {
            let index = self.pool.take(cx, size, RenderTargetFormat::Bgra8, "edit_premix");
            let pass = &mut self.pool[index];
            pass.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
            let full = Rect { pos: dvec2(0.0, 0.0), size: dvec2(size.0 as f64, size.1 as f64) };
            let draw = &mut self.draws.blend;
            draw.draw_vars.set_texture(0, &ta);
            draw.draw_vars.set_texture(1, &tb);
            draw.mode = blend_mode::DISSOLVE;
            draw.amount = mix as f32;
            draw.draw_abs(cx, full);
            pass.end(cx);
            let id = pass.pass.draw_pass_id();
            spans.push((id, id));
            passes.push(id);
            pass.texture.clone()
        });
        match view {
            Some(view) => {
                let lever;
                if decks {
                    view.set_input_texture(0, Some(ta));
                    view.set_input_texture(1, Some(tb));
                    lever = mix;
                } else {
                    view.set_input_texture(0, premix.clone());
                    view.set_input_texture(1, None);
                    lever = 1.0 - (2.0 * mix - 1.0).abs();
                }
                set_edit_dials(view, params, Some(lever as f32));
                units.push(unit.to_string());
                Some(self.effect_pass(cx, size, view, drive, spans, passes))
            }
            None => premix,
        }
    }

    /// Two finished pictures mixed by a transition into this compositor's
    /// output (DECK's slide changes): the same mix as an edit's
    /// `Transition` op. `view` is the preset's effect unit (None: a
    /// dissolve); `unit` names it for [`Self::painted`] (the host keeps the
    /// unit under that name in the map it passes there). Records under the
    /// pass being drawn; the result stays whole until the frame after next
    /// is recorded.
    #[allow(clippy::too_many_arguments)]
    pub fn transition(
        &mut self,
        cx: &mut Cx2d,
        a: &Texture,
        b: &Texture,
        view: Option<&mut (dyn EffectView + 'static)>,
        unit: &str,
        mix: f64,
        params: &[f64; 4],
        drive: &EffectDrive,
        size: (u32, u32),
    ) -> Texture {
        self.kit.begin_frame(cx);
        self.graphics.begin_frame(cx.cx);
        self.pool.next_frame(cx);
        let (mut spans, mut passes, mut units) = (Vec::new(), Vec::new(), Vec::new());
        let result = self.transition_pass(cx, a.clone(), b.clone(), view, size, mix, params, drive, unit, &mut spans, &mut passes, &mut units);
        self.finish(cx, result, size, spans, passes, units)
    }

    /// One effect op: the view on its seekable clock, rendered under the
    /// op's own pass and copied into the op's target.
    #[allow(clippy::too_many_arguments)]
    fn effect_pass(
        &mut self,
        cx: &mut Cx2d,
        size: (u32, u32),
        view: &mut dyn EffectView,
        drive: &EffectDrive,
        spans: &mut Vec<(DrawPassId, DrawPassId)>,
        passes: &mut Vec<DrawPassId>,
    ) -> Texture {
        view.set_speed_scale(1.0);
        view.set_time(cx, drive.local_time, drive.beat, drive.dt);
        let index = self.pool.take(cx, size, RenderTargetFormat::Bgra8, "edit_effect");
        let pass = &mut self.pool[index];
        pass.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
        let pixels = dvec2(size.0 as f64, size.1 as f64);
        let output = view.render_live(cx, pixels);
        if let Some(texture) = &output {
            let draw = &mut self.draws.texture;
            draw.draw_vars.set_texture(0, texture);
            draw.src = vec4(0.0, 0.0, 1.0, 1.0);
            draw.opacity = 1.0;
            draw.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: pixels });
        }
        pass.end(cx);
        let id = pass.pass.draw_pass_id();
        let first = view.live_passes().map_or(id, |(first, _)| first);
        spans.push((first, id));
        passes.push(id);
        pass.texture.clone()
    }

    /// How far the last recorded frame has come: whole once every pass of
    /// it painted cleanly after it was recorded, and every effect's chain.
    /// A draw left out (a shader still compiling, or a source pass not yet
    /// painted) leaves its pass dirty, so a clean pass is a whole one.
    pub fn painted(&self, cx: &Cx, effects: &HashMap<String, Box<dyn EffectView>>) -> Painted {
        let Some((recorded, passes, units)) = &self.recorded else { return Painted::Painting };
        if cx.draw_shaders_pending() {
            return Painted::Painting;
        }
        let done = passes.iter().all(|&id| !cx.passes[id].paint_dirty && cx.passes[id].painted_serial > *recorded)
            && units.iter().all(|node| effects.get(node).map_or(true, |view| view.live_output_painted(cx, *recorded) && view.live_output_exact()))
            && self.kit.painted(cx) == makepad_stage_render_kit::Painted::Whole
            && self.graphics.painted(cx) && self.graphics.exact(cx);
        if done { Painted::Whole } else { Painted::Painting }
    }

    /// The last recorded frame will not paint whole however long it is kept
    /// attached (a render-kit draw was dropped for a shader not yet queued):
    /// record it again.
    pub fn stale(&self, cx: &Cx, effects: &HashMap<String, Box<dyn EffectView>>) -> bool {
        // An effect gathering motion-blur sub-frames painted this one: the
        // frame is recorded again for the next.
        let gathering = self.recorded.as_ref().is_some_and(|(recorded, _, units)| {
            units.iter().any(|node| effects.get(node).is_some_and(|view| view.live_output_painted(cx, *recorded) && !view.live_output_exact()))
        });
        gathering
            || self.kit.painted(cx) == makepad_stage_render_kit::Painted::Partial
            // A graphic drew before something it shows was ready (a glyph,
            // a shader): painted, but to be recorded again.
            || (self.graphics.painted(cx) && !self.graphics.exact(cx))
    }

    /// Why the last recorded frame is not whole yet, for a log: the
    /// shaders pending, and every pass or effect chain not painted since.
    pub fn painting_report(&self, cx: &Cx, effects: &HashMap<String, Box<dyn EffectView>>) -> String {
        let Some((recorded, passes, units)) = &self.recorded else { return "nothing recorded".into() };
        let mut out = format!("recorded at {recorded} (now {}); shaders pending {}; graphics [{}]", cx.repaint_id(), cx.draw_shaders_pending(), self.graphics.debug_state(cx));
        for &id in passes {
            let pass = &cx.passes[id];
            if pass.paint_dirty || pass.painted_serial <= *recorded {
                out.push_str(&format!("; pass {:?} dirty {} painted {}", id, pass.paint_dirty, pass.painted_serial));
            }
        }
        for node in units {
            if let Some(view) = effects.get(node) {
                if !view.live_output_painted(cx, *recorded) {
                    let chain = view.live_passes().map(|(first, last)| (cx.passes[first].painted_serial, cx.passes[first].paint_dirty, cx.passes[last].painted_serial, cx.passes[last].paint_dirty));
                    out.push_str(&format!("; effect `{node}` chain (first serial, dirty, last serial, dirty) {chain:?}"));
                }
            }
        }
        out
    }

    /// Keep the recorded frame attached while it has not painted (the
    /// platform only draws attached passes). Every pass of the frame was
    /// attached by the host's draw list when it was recorded; a host draw
    /// that does not attach them again orphans them, and the platform then
    /// drops them unpainted (a draw with no repaint after it: GPU
    /// backpressure, a loaded machine). So all of them, in recording order.
    pub fn reattach(&self, cx: &mut Cx2d) {
        self.kit.reattach(cx);
        self.graphics.reattach(cx);
        let Some((_, passes, _)) = &self.recorded else { return };
        let all = self.pool.iter().chain(self.outputs.iter().flatten()).chain(self.strips.iter().map(|(strip, _)| strip))
            .chain(self.pictures.iter().flat_map(|slot| slot.strips.iter().map(|(target, _)| target)));
        let by_id: HashMap<DrawPassId, &RenderTarget> = all.map(|op| (op.pass.draw_pass_id(), op)).collect();
        for id in passes {
            if let Some(op) = by_id.get(id) {
                cx.make_child_pass(&op.pass);
            }
        }
        for pair in self.recorded_spans.windows(2) {
            let ((_, last), (first, _)) = (pair[0], pair[1]);
            cx.set_pass_parent(last, first);
        }
    }

}

/// An edit dial's value for a vj_fx view: below 0 leaves the document's.
fn edit_dial(value: f64) -> Option<f32> {
    (value >= 0.0).then_some(value as f32)
}

/// AN EDIT'S DIALS ON A VJ_FX EFFECT, as the live card drives them
/// (`VjFxView::set_card_dials`): p1..p3 are the document's first three
/// dials in its list order, whatever slots they bind, and `lever` is p3,
/// the host's lever (an Fx node's p4, a transition's mix). So an edit, a
/// Motion layer and the VJ card mean the same dial by P1..P3.
fn set_edit_dials(view: &mut dyn EffectView, params: &[f64; 4], lever: Option<f32>) {
    view.set_card_dials(&[edit_dial(params[0]), edit_dial(params[1]), edit_dial(params[2])], lever);
}

/// Whether an Fx preset name is a render-kit effect (`kit/<name>`).
pub fn is_kit_effect(name: &str) -> bool {
    name.starts_with("kit/")
}

/// The render-kit effect an Fx node names, with its dials: p1..p4 (0..1,
/// below 0 leaves the default) set the effect's first four number
/// parameters across their ranges. `Err` names what is wrong with it; a
/// temporal effect (motion blur, echo) renders the layer at other times,
/// which a cut's clip cannot be yet.
pub fn kit_effect(name: &str, dials: &[f64; 4]) -> Result<Effect, String> {
    let info = makepad_stage_render_kit::catalog::by_name(name).ok_or_else(|| format!("`{name}` is not a render-kit effect"))?;
    if info.temporal {
        return Err(format!("`{name}` draws the layer at other times (motion blur, echo); edit clips do not support it yet"));
    }
    let mut values: Vec<(&str, Anim)> = Vec::new();
    let numbers = info.params.iter().filter_map(|p| match p.kind {
        ParamKind::Num { min, max, .. } => Some((p.name, min, max)),
        _ => None,
    });
    for ((param, min, max), dial) in numbers.zip(dials.iter()) {
        if *dial >= 0.0 {
            values.push((param, Anim::Const(Value::Num(min + (max - min) * dial.clamp(0.0, 1.0)))));
        }
    }
    Effect::build(name, values).map(|(effect, _)| effect).map_err(|diagnostics| format!("`{name}`: {diagnostics:?}"))
}

#[cfg(test)]
mod tests {
    use super::caption_rows;

    #[test]
    fn caption_rows_wrap_at_the_width_and_keep_every_word() {
        let rows = caption_rows(&[40.0, 30.0, 50.0, 20.0], 10.0, 100.0);
        assert_eq!(rows, vec![(0..2, 80.0), (2..4, 80.0)]);
        // A word wider than the row stands alone.
        assert_eq!(caption_rows(&[150.0, 20.0], 10.0, 100.0), vec![(0..1, 150.0), (1..2, 20.0)]);
        assert!(caption_rows(&[], 10.0, 100.0).is_empty());
    }
}
