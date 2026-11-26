//! The GPU side: [`RenderKit`] records an effect stack as offscreen passes
//! on Makepad's pipeline and returns the result texture.
//!
//! - Targets are pooled: every pass of a frame takes the next slot (a
//!   `DrawPass`, its draw list and a BGRA target), reused frame after frame
//!   and resized only when its size changes. Slots unused for a while are
//!   freed. A texture returned by the kit is valid until the next
//!   [`RenderKit::begin_frame`].
//! - Ordering is stated, never left to pool ids: within one call each pass
//!   is made the child of the pass after it (children paint first), and the
//!   call's last pass stays the child of the pass the host is drawing.
//!   [`Applied`] names a call's first and last pass so a host can chain
//!   several calls (the edit compositor's `spans`).
//! - Everything is premultiplied, 8 bits per channel, drawn at dpi 1.
//! - Temporal effects (motion blur, echo) re-render the layer at other
//!   times through [`LayerSource`]; the effects before them in the stack
//!   are applied per sub-frame, the ones after once.

use crate::budget::{self, Budget, Plan};
use crate::catalog::EffectKind;
use crate::color;
use crate::particles;
use crate::path;
use crate::shaders::{DrawKit, KitBlend, KitDraws};
use crate::space::{self, Camera, Plane};
use crate::stack::{EffectStack, ResolvedEffect};
use crate::temporal::{self, Affine2};
use makepad_widgets::*;
use std::collections::HashMap;

/// What a stack is applied with, besides its input.
pub struct FxContext<'a> {
    /// The time the stack is evaluated at (the host's time base).
    pub t: f64,
    /// The host's frame rate: the motion blur shutter and grain refer to it.
    pub fps: f64,
    /// The comp camera for 2.5D layers; `None` is the default camera.
    pub camera: Option<&'a Camera>,
    /// Other layers by name, for mattes, displacement maps, blend layers
    /// and particle emitters (at the frame's time).
    pub layers: &'a HashMap<String, Texture>,
}

impl<'a> FxContext<'a> {
    pub fn new(t: f64, fps: f64, layers: &'a HashMap<String, Texture>) -> Self {
        Self { t, fps, camera: None, layers }
    }
}

/// Draws a layer at any time: the host side of temporal effects.
pub trait LayerSource {
    /// Draw the layer as it is at `t` into the pass being drawn, `size`
    /// pixels at dpi 1 (the same frame the stack renders into).
    fn draw_layer(&mut self, cx: &mut Cx2d, t: f64, size: (u32, u32));

    /// The layer's placement at `t`, when the host knows it: motion blur
    /// then adapts its sample count to the motion and can fall back to the
    /// velocity blur in previews.
    fn placement(&self, _t: f64) -> Option<Affine2> {
        None
    }

    /// The layer's content rectangle in layer pixels (x, y, w, h), for
    /// measuring motion; `None` is the whole frame.
    fn bounds(&self) -> Option<[f64; 4]> {
        None
    }

    /// A layer that already is a texture at every time: the kit copies it
    /// instead of calling `draw_layer`.
    fn texture(&self) -> Option<Texture> {
        None
    }
}

/// A layer that is one texture at all times (nothing moves).
pub struct TextureLayer(pub Texture);

impl LayerSource for TextureLayer {
    fn draw_layer(&mut self, _cx: &mut Cx2d, _t: f64, _size: (u32, u32)) {}

    fn texture(&self) -> Option<Texture> {
        Some(self.0.clone())
    }
}

/// A layer drawn by a closure `(cx, t, size)`.
pub struct FnLayer<F: FnMut(&mut Cx2d, f64, (u32, u32))> {
    pub draw: F,
    pub placement: Option<Box<dyn Fn(f64) -> Affine2>>,
}

impl<F: FnMut(&mut Cx2d, f64, (u32, u32))> LayerSource for FnLayer<F> {
    fn draw_layer(&mut self, cx: &mut Cx2d, t: f64, size: (u32, u32)) {
        (self.draw)(cx, t, size)
    }

    fn placement(&self, t: f64) -> Option<Affine2> {
        self.placement.as_ref().map(|f| f(t))
    }
}

/// The result of one call: the texture and the passes that make it.
#[derive(Clone, Debug)]
pub struct Applied {
    pub texture: Texture,
    /// The first and last pass the call recorded (None when it recorded
    /// nothing and returned its input).
    pub first: Option<DrawPassId>,
    pub last: Option<DrawPassId>,
}

struct Slot {
    pass: DrawPass,
    list: DrawList2d,
    target: Texture,
    size: (u32, u32),
    /// The target is linear RGBA16F (an HDR frame), else BGRA8.
    hdr: bool,
    /// Frame this slot was last used in.
    used: u64,
}

/// Frame statistics, for the host's status line and the benchmarks.
#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    pub passes: usize,
    pub particles: usize,
    pub segments: usize,
    pub sub_frames: usize,
    pub notes: Vec<String>,
}

pub struct RenderKit {
    draws: KitDraws,
    pool: Vec<Slot>,
    next: usize,
    frame: u64,
    /// Every pass recorded this frame, in order.
    frame_passes: Vec<DrawPassId>,
    /// The repaint id at `begin_frame`, for [`RenderKit::painted`].
    recorded_at: u64,
    pub budget: Budget,
    transparent: Texture,
    ramps: HashMap<u64, Texture>,
    luts: HashMap<String, Result<(Texture, usize), String>>,
    pub stats: FrameStats,
    logged: std::collections::HashSet<String>,
    /// Command-buffer GPU timing on every pass (the benchmarks).
    timing: bool,
    unqueued_at_record: u64,
    /// The pool slots of `frame_passes`, in the same order.
    frame_slots: Vec<usize>,
    /// Draw into linear half-float targets (see [`RenderKit::set_hdr`]).
    hdr: bool,
}

/// Passes whose output is data packed into 8 bits (jump-flood seeds): they
/// keep BGRA8 in an HDR frame too.
const DATA_PASSES: &[&str] = &["kit_jfa_seed", "kit_jfa_step"];

/// See [`RenderKit::painted`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Painted {
    Painting,
    Whole,
    Partial,
}

/// A gaussian pass runs at the level where sigma is at most this many
/// texels (cost stays flat in the radius; the level is smooth enough to
/// sample bilinearly).
const MAX_SIGMA_PER_LEVEL: f64 = 6.0;

/// A blurred picture at `level` halvings of the full resolution.
struct Low {
    texture: Texture,
    size: (u32, u32),
    level: u32,
}
/// Slots unused for this many frames are freed.
const SLOT_IDLE_FRAMES: u64 = 240;

fn v4(v: [f64; 4]) -> Vec4f {
    vec4(v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32)
}

fn premul(c: [f64; 4], opacity: f64) -> Vec4f {
    let a = c[3] * opacity;
    vec4((c[0] * a) as f32, (c[1] * a) as f32, (c[2] * a) as f32, a as f32)
}

fn full(size: (u32, u32)) -> Rect {
    Rect { pos: dvec2(0.0, 0.0), size: dvec2(size.0 as f64, size.1 as f64) }
}

/// The shadow's offset in pixels for a light from `angle` degrees
/// (counter-clockwise from the right, y up) at `distance`.
pub fn shadow_offset(angle: f64, distance: f64) -> [f64; 2] {
    let a = angle.to_radians();
    [-a.cos() * distance, a.sin() * distance]
}

impl RenderKit {
    pub fn new(cx: &mut Cx) -> Self {
        let draws = cx.with_vm(|vm| KitDraws::script_new_with_default(vm));
        let transparent = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width: 2, height: 2, data: Some(vec![0; 4]), updated: TextureUpdated::Full });
        Self {
            draws,
            pool: Vec::new(),
            next: 0,
            frame: 0,
            frame_passes: Vec::new(),
            recorded_at: 0,
            budget: Budget::default(),
            transparent,
            ramps: HashMap::new(),
            luts: HashMap::new(),
            stats: FrameStats::default(),
            logged: Default::default(),
            timing: false,
            unqueued_at_record: 0,
            frame_slots: Vec::new(),
            hdr: false,
        }
    }

    /// The frame is linear half-float light (an HDR Motion frame): effects
    /// draw into RGBA16F targets, so light over 1 (an emissive line's glow)
    /// is carried through blurs, glows and composites instead of clamped.
    /// The jump-flood data passes keep 8 bits.
    pub fn set_hdr(&mut self, on: bool) {
        self.hdr = on;
    }

    /// Time every pass on the GPU from now on, tagged by frame; drain
    /// with [`RenderKit::take_gpu_ms`].
    pub fn set_gpu_timing(&mut self, on: bool) {
        self.timing = on;
    }

    /// Completed GPU times, (frame, ms summed over that frame's passes).
    pub fn take_gpu_ms(&self, cx: &Cx) -> Vec<(u64, f64)> {
        let mut by_frame: HashMap<u64, f64> = HashMap::new();
        for slot in &self.pool {
            for (tag, ms) in slot.pass.take_gpu_time_samples(cx) {
                *by_frame.entry(tag).or_insert(0.0) += ms;
            }
        }
        by_frame.into_iter().collect()
    }

    /// Start a frame: every slot is free again (textures returned by the
    /// previous frame are overwritten from here on).
    pub fn begin_frame(&mut self, cx: &mut Cx) {
        self.frame += 1;
        self.next = 0;
        self.frame_passes.clear();
        self.recorded_at = cx.repaint_id();
        self.unqueued_at_record = cx.draw_skip_reasons()[1];
        self.frame_slots.clear();
        self.stats = FrameStats::default();
        // Free what has been idle a long time; slots are position-bound, so
        // only the tail can go.
        while self.pool.last().is_some_and(|slot| self.frame - slot.used > SLOT_IDLE_FRAMES) {
            self.pool.pop();
        }
    }

    /// Every pass recorded this frame, in order.
    pub fn passes(&self) -> &[DrawPassId] {
        &self.frame_passes
    }

    /// How far this frame has come. A pass whose draw was left out while
    /// its pipeline compiled stays dirty and paints again, so `Whole` means
    /// every draw is in. `Partial`: a draw was dropped for a shader that was
    /// not even queued for compiling yet, which the platform does not mark
    /// dirty; record the frame again (hosts cap how often).
    pub fn painted(&self, cx: &Cx) -> Painted {
        if cx.draw_shaders_pending() || cx.pipelines_pending() > 0 {
            return Painted::Painting;
        }
        if !self.frame_passes.iter().all(|&id| !cx.passes[id].paint_dirty && cx.passes[id].painted_serial > self.recorded_at) {
            return Painted::Painting;
        }
        if cx.draw_skip_reasons()[1] != self.unqueued_at_record {
            return Painted::Partial;
        }
        Painted::Whole
    }

    /// Keep this frame's passes attached while they have not painted: the
    /// platform drops a pass whose attaching draw list was recorded again
    /// without it, so every pass is attached anew under the pass the host
    /// is drawing and the chain is stated again. Call from the host's draw
    /// until [`RenderKit::painted`] says `Whole`.
    pub fn reattach(&self, cx: &mut Cx2d) {
        for &slot in &self.frame_slots {
            cx.make_child_pass(&self.pool[slot].pass);
        }
        for pair in self.frame_passes.windows(2) {
            cx.set_pass_parent(pair[0], pair[1]);
        }
    }

    fn log_once(&mut self, message: String) {
        if self.logged.insert(message.clone()) {
            log!("render-kit: {message}");
        }
    }

    /// One pass into a pooled target of `size`: `draw` binds and draws.
    fn run(&mut self, cx: &mut Cx2d, size: (u32, u32), name: &str, draw: impl FnOnce(&mut Cx2d, &mut KitDraws, Rect)) -> Texture {
        let size = (size.0.max(1), size.1.max(1));
        let index = self.next;
        self.next += 1;
        while self.pool.len() <= index {
            self.pool.push(Slot {
                pass: DrawPass::new_with_name(cx, name),
                list: DrawList2d::new(cx),
                target: Texture::new_with_format(cx, TextureFormat::RenderBGRAu8 { size: TextureSize::Auto, initial: true }).label(cx, "render-kit/target"),
                size: (0, 0),
                hdr: false,
                used: 0,
            });
        }
        let hdr = self.hdr && !DATA_PASSES.contains(&name);
        let slot = &mut self.pool[index];
        slot.used = self.frame;
        if slot.size != size || slot.hdr != hdr {
            let fixed = TextureSize::Fixed { width: size.0 as usize, height: size.1 as usize };
            slot.target = Texture::new_with_format(cx, if hdr {
                TextureFormat::RenderRGBAf16 { size: fixed, initial: true }
            } else {
                TextureFormat::RenderBGRAu8 { size: fixed, initial: true }
            }).label(cx, "render-kit/target");
            slot.size = size;
            slot.hdr = hdr;
        }
        let pixels = dvec2(size.0 as f64, size.1 as f64);
        if self.timing {
            slot.pass.set_gpu_timing_enabled(cx, true);
            slot.pass.set_gpu_time_tag(cx, self.frame);
        }
        slot.pass.set_size(cx, pixels);
        slot.pass.set_color_texture(cx, &slot.target, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
        cx.make_child_pass(&slot.pass);
        cx.begin_pass(&slot.pass, Some(1.0));
        slot.list.begin_always(cx);
        cx.begin_root_turtle(pixels, Layout::flow_overlay());
        draw(cx, &mut self.draws, full(size));
        cx.end_pass_sized_turtle();
        slot.list.end(cx);
        cx.end_pass(&slot.pass);
        let id = slot.pass.draw_pass_id();
        // Chain: the previous pass of this frame paints before this one.
        if let Some(&prev) = self.frame_passes.last() {
            cx.set_pass_parent(prev, id);
        }
        self.frame_passes.push(id);
        self.frame_slots.push(index);
        self.stats.passes += 1;
        slot.target.clone()
    }

    fn quad(cx: &mut Cx2d, draw: &mut DrawKit, textures: &[&Texture], rect: Rect) {
        for (i, texture) in textures.iter().enumerate() {
            draw.draw_vars.set_texture(i, texture);
        }
        draw.draw_abs(cx, rect);
    }

    /// A texture drawn into a new target of `size` (a copy, or a resample
    /// when sizes differ). Hosts use it to bring a layer into the kit.
    pub fn copy(&mut self, cx: &mut Cx2d, input: &Texture, size: (u32, u32)) -> Texture {
        self.run(cx, size, "kit_copy", |cx, draws, rect| {
            let d = &mut draws.copy;
            reset(d);
            d.set_f32(cx, live_id!(opacity), 1.0);
            d.set_vec4(cx, live_id!(src_rect), vec4(0.0, 0.0, 1.0, 1.0));
            Self::quad(cx, d, &[input], rect);
        })
    }

    /// `layer` over `below` in a blend mode (W3C, premultiplied). `clip`
    /// keeps the result inside `below`'s alpha.
    pub fn composite(&mut self, cx: &mut Cx2d, below: &Texture, layer: &Texture, mode: KitBlend, opacity: f64, clip: bool, size: (u32, u32)) -> Texture {
        self.run(cx, size, "kit_composite", |cx, draws, rect| {
            let d = &mut draws.composite;
            reset(d);
            d.set_f32(cx, live_id!(opacity), opacity as f32);
            d.set_f32(cx, live_id!(clip), if clip { 1.0 } else { 0.0 });
            d.blend = mode;
            Self::quad(cx, d, &[below, layer], rect);
        })
    }

    /// Apply a resolved stack to `input` (`size` pixels). Temporal effects
    /// in it are skipped with a log line: they need [`RenderKit::apply_layer`].
    pub fn apply(&mut self, cx: &mut Cx2d, stack: &[ResolvedEffect], input: &Texture, size: (u32, u32), ctx: &FxContext) -> Applied {
        let first_index = self.frame_passes.len();
        let plan = budget::plan(&self.budget, stack, size, &vec![None; stack.len()], false);
        self.note_plan(&plan);
        let mut current = input.clone();
        for (i, effect) in stack.iter().enumerate() {
            if !effect.enabled {
                continue;
            }
            if crate::catalog::info(effect.kind).temporal {
                self.log_once(format!("{} needs the layer at other times; use apply_layer", crate::catalog::info(effect.kind).name));
                continue;
            }
            current = self.effect(cx, effect, &current, size, ctx, plan.scale[i]);
        }
        let recorded = &self.frame_passes[first_index..];
        Applied { texture: current, first: recorded.first().copied(), last: recorded.last().copied() }
    }

    fn note_plan(&mut self, plan: &Plan) {
        for note in &plan.notes {
            self.log_once(note.clone());
        }
        self.stats.notes.extend(plan.notes.iter().cloned());
    }

    /// Apply a keyable stack to a layer the host draws at any time. The
    /// stack is resolved at `ctx.t`; effects before a temporal effect are
    /// resolved and applied at each of its sub-frame times.
    pub fn apply_layer(&mut self, cx: &mut Cx2d, stack: &EffectStack, size: (u32, u32), ctx: &FxContext, source: &mut dyn LayerSource) -> Applied {
        let first_index = self.frame_passes.len();
        let effects: Vec<_> = stack.effects.iter().filter(|e| e.enabled).cloned().collect();
        let texture = self.layer_at(cx, &effects, size, ctx, ctx.t, source);
        let recorded = &self.frame_passes[first_index..];
        Applied { texture, first: recorded.first().copied(), last: recorded.last().copied() }
    }

    fn draw_source(&mut self, cx: &mut Cx2d, size: (u32, u32), t: f64, source: &mut dyn LayerSource) -> Texture {
        self.stats.sub_frames += 1;
        if let Some(texture) = source.texture() {
            return texture;
        }
        self.run(cx, size, "kit_layer", |cx, _, _| source.draw_layer(cx, t, size))
    }

    fn layer_at(&mut self, cx: &mut Cx2d, effects: &[crate::stack::Effect], size: (u32, u32), ctx: &FxContext, t: f64, source: &mut dyn LayerSource) -> Texture {
        let resolved: Vec<ResolvedEffect> = effects.iter().map(|e| e.at(t)).collect();
        let Some(k) = effects.iter().rposition(|e| e.is_temporal()) else {
            let base = self.draw_source(cx, size, t, source);
            let sub = FxContext { t, fps: ctx.fps, camera: ctx.camera, layers: ctx.layers };
            return self.apply(cx, &resolved, &base, size, &sub).texture;
        };
        let temporal = &resolved[k];
        let rect = source.bounds().unwrap_or([0.0, 0.0, size.0 as f64, size.1 as f64]);
        let (times, mode, velocity) = match temporal.kind {
            EffectKind::MotionBlur => {
                let (open, close) = temporal::shutter(temporal, t, ctx.fps);
                let motion = match (source.placement(open), source.placement(close)) {
                    (Some(a), Some(b)) => Some(temporal::travel(&a, &b, rect)),
                    _ => None,
                };
                let wanted = temporal::sample_count(temporal, motion);
                let mut samples = vec![None; resolved.len()];
                samples[k] = Some(wanted);
                let plan = budget::plan(&self.budget, &resolved, size, &samples, motion.is_some());
                self.note_plan(&plan);
                let n = plan.samples[k].unwrap_or(wanted);
                let velocity = plan.velocity[k].then(|| (open, close));
                (temporal::motion_blur_samples(open, close, n), 0.0, velocity)
            }
            _ => {
                let mode = match temporal.choice("mode") {
                    "add" => 2.0,
                    "screen" => 3.0,
                    _ => 1.0,
                };
                (temporal::echo_samples(temporal, t), mode, None)
            }
        };
        let reduced = match velocity {
            Some((open, close)) => {
                let frame = self.layer_at(cx, &effects[..k], size, ctx, t, source);
                match (source.placement(t), source.placement(open), source.placement(close)) {
                    (Some(now), Some(a), Some(b)) => self.velocity_blur(cx, &frame, size, &now, &a, &b),
                    _ => frame,
                }
            }
            None => {
                let mut textures = Vec::with_capacity(times.len());
                for (s, _) in &times {
                    textures.push(self.layer_at(cx, &effects[..k], size, ctx, *s, source));
                }
                let weights: Vec<f64> = times.iter().map(|(_, w)| *w).collect();
                self.reduce(cx, &textures, &weights, mode, size)
            }
        };
        let after: Vec<ResolvedEffect> = resolved[k + 1..].to_vec();
        let sub = FxContext { t, fps: ctx.fps, camera: ctx.camera, layers: ctx.layers };
        self.apply(cx, &after, &reduced, size, &sub).texture
    }

    /// Reduce samples 8 at a time: a weighted sum (exact average), or
    /// newest-first "over", add or screen. Over, add and screen are
    /// associative, and a weighted sum of weighted sums is one, so the
    /// tree gives the same picture as one pass over all samples would.
    fn reduce(&mut self, cx: &mut Cx2d, textures: &[Texture], weights: &[f64], mode: f32, size: (u32, u32)) -> Texture {
        if textures.len() == 1 && (weights[0] - 1.0).abs() < 1e-12 {
            return textures[0].clone();
        }
        let mut level: Vec<(Texture, f64)> = textures.iter().cloned().zip(weights.iter().copied()).collect();
        loop {
            let mut next = Vec::new();
            for group in level.chunks(8) {
                // In a weighted sum the partial carries its weight out; the
                // other modes carry per-sample opacity in and 1 out.
                let total: f64 = if mode == 0.0 { group.iter().map(|(_, w)| w).sum() } else { 1.0 };
                let tex = self.run(cx, size, "kit_accum", |cx, draws, rect| {
                    let d = &mut draws.accum;
                    reset(d);
                    let mut w = [0.0f32; 8];
                    for (i, (_, weight)) in group.iter().enumerate() {
                        w[i] = if mode == 0.0 { (weight / total.max(1e-12)) as f32 } else { *weight as f32 };
                    }
                    d.set_vec4(cx, live_id!(weights_lo), vec4(w[0], w[1], w[2], w[3]));
                    d.set_vec4(cx, live_id!(weights_hi), vec4(w[4], w[5], w[6], w[7]));
                    d.set_f32(cx, live_id!(mode), mode);
                    for i in 0..8 {
                        d.draw_vars.set_texture(i, &group.get(i).unwrap_or(&group[0]).0);
                    }
                    d.draw_abs(cx, rect);
                });
                next.push((tex, total));
            }
            if next.len() == 1 {
                return next.pop().unwrap().0;
            }
            level = next;
        }
    }

    fn velocity_blur(&mut self, cx: &mut Cx2d, frame: &Texture, size: (u32, u32), now: &Affine2, open: &Affine2, close: &Affine2) -> Texture {
        // Where the pixel p of the frame at `t` was at shutter time s:
        // now · s⁻¹ · p … sampled along the segment between open and close.
        let (Some(inv_open), Some(inv_close)) = (temporal::invert_affine(open), temporal::invert_affine(close)) else { return frame.clone() };
        let m0 = temporal::compose_affine(now, &inv_open);
        let m1 = temporal::compose_affine(now, &inv_close);
        let rect = [0.0, 0.0, size.0 as f64, size.1 as f64];
        let taps = (temporal::travel(open, close, rect).ceil() as usize).clamp(2, 64) as f32;
        self.run(cx, size, "kit_velocity", |cx, draws, r| {
            let d = &mut draws.velocity;
            reset(d);
            d.set_vec4(cx, live_id!(map_open), vec4(m0[0] as f32, m0[1] as f32, m0[2] as f32, m0[3] as f32));
            d.set_vec2(cx, live_id!(map_open_t), [m0[4] as f32, m0[5] as f32]);
            d.set_f32(cx, live_id!(taps), taps);
            d.set_vec4(cx, live_id!(map_close), vec4(m1[0] as f32, m1[1] as f32, m1[2] as f32, m1[3] as f32));
            d.set_vec2(cx, live_id!(map_close_t), [m1[4] as f32, m1[5] as f32]);
            Self::quad(cx, d, &[frame], r);
        })
    }

    /// A gaussian blur of `sigma` pixels at full resolution; `axes` 0
    /// both, 1 horizontal, 2 vertical; `edge` 0 transparent, 1 extend.
    /// See [`RenderKit::blur_low`]; the result is brought back one doubling
    /// at a time. `min_levels` forces downsampling (previews).
    pub fn blur(&mut self, cx: &mut Cx2d, input: &Texture, size: (u32, u32), sigma: f64, axes: u32, edge: f32, min_levels: u32) -> Texture {
        if !(sigma > 0.3) {
            return input.clone();
        }
        let low = self.blur_low(cx, &Low { texture: input.clone(), size, level: 0 }, sigma, axes, edge, min_levels, true);
        let mut sizes = vec![size];
        for _ in 0..low.level {
            let (w, h) = *sizes.last().unwrap();
            sizes.push((if axes != 2 { w.div_ceil(2) } else { w }, if axes != 1 { h.div_ceil(2) } else { h }));
        }
        let mut current = low.texture;
        for level in sizes[..low.level as usize].iter().rev() {
            current = self.copy(cx, &current, *level);
        }
        current
    }

    /// A gaussian blur of `sigma` full-resolution pixels, left at the
    /// resolution it was computed at: the input is halved one level at a
    /// time (2:1 box, along the blurred axes) until the remaining sigma is
    /// at most a few texels, and the variance the halving (and, with
    /// `upsampled`, the bilinear doublings back) adds is subtracted, so the
    /// whole stays gaussian. Consumers sample it with bilinear filtering;
    /// at ≥ 2 texels of sigma that is smooth.
    fn blur_low(&mut self, cx: &mut Cx2d, input: &Low, sigma: f64, axes: u32, edge: f32, min_levels: u32, upsampled: bool) -> Low {
        let (bx, by) = (axes != 2, axes != 1);
        let mut level = input.level.max(min_levels);
        while sigma / (1u64 << level) as f64 > MAX_SIGMA_PER_LEVEL && level < 12 {
            level += 1;
        }
        let mut current = input.texture.clone();
        let mut size = input.size;
        let mut reached = input.level;
        while reached < level {
            let next = (if bx { size.0.div_ceil(2) } else { size.0 }, if by { size.1.div_ceil(2) } else { size.1 });
            if (bx && next.0 < 4) || (by && next.1 < 4) {
                break;
            }
            current = self.copy(cx, &current, next);
            size = next;
            reached += 1;
        }
        let (from, n) = (input.level as i32, reached as i32);
        let down = (4f64.powi(n) - 4f64.powi(from)) / 12.0;
        let up = if upsampled && n > 0 { (4f64.powi(n + 1) - 4.0) / 18.0 } else { 0.0 };
        let s = ((sigma * sigma - down - up).max(0.09)).sqrt() / (1u64 << reached) as f64;
        for (dir, on) in [((1.0, 0.0), bx), ((0.0, 1.0), by)] {
            if !on {
                continue;
            }
            let src = current.clone();
            current = self.run(cx, size, "kit_gauss", |cx, draws, rect| {
                let d = &mut draws.gauss;
                reset(d);
                d.set_vec2(cx, live_id!(axis), [dir.0, dir.1]);
                d.set_f32(cx, live_id!(sigma), s as f32);
                d.set_f32(cx, live_id!(edge), edge);
                Self::quad(cx, d, &[&src], rect);
            });
        }
        Low { texture: current, size, level: reached }
    }

    /// A soft copy for consumers that sample it bilinearly (glow, shadows,
    /// bevel height, matte feather): the blur without the way back up.
    fn soft(&mut self, cx: &mut Cx2d, input: &Texture, size: (u32, u32), sigma: f64, edge: f32, min_levels: u32) -> Texture {
        if !(sigma > 0.3) {
            return input.clone();
        }
        self.blur_low(cx, &Low { texture: input.clone(), size, level: 0 }, sigma, 0, edge, min_levels, false).texture
    }

    fn mix_with(&mut self, cx: &mut Cx2d, input: &Texture, result: Texture, amount: f64, size: (u32, u32)) -> Texture {
        if amount >= 1.0 {
            return result;
        }
        if amount <= 0.0 {
            return input.clone();
        }
        self.run(cx, size, "kit_mix", |cx, draws, rect| {
            let d = &mut draws.mix;
            reset(d);
            d.set_f32(cx, live_id!(amount), amount as f32);
            Self::quad(cx, d, &[input, &result], rect);
        })
    }

    fn one(&mut self, cx: &mut Cx2d, size: (u32, u32), name: &str, pick: fn(&mut KitDraws) -> &mut DrawKit, textures: &[&Texture], set: impl FnOnce(&Cx, &mut DrawKit)) -> Texture {
        self.run(cx, size, name, |cx, draws, rect| {
            let d = pick(draws);
            reset(d);
            set(cx, d);
            Self::quad(cx, d, textures, rect);
        })
    }

    fn layer_ref(&mut self, ctx: &FxContext, name: &str, what: &str) -> Option<Texture> {
        if name.is_empty() {
            return None;
        }
        let found = ctx.layers.get(name).cloned();
        if found.is_none() {
            self.log_once(format!("{what}: no layer `{name}` was handed in; the effect draws without it"));
        }
        found
    }

    fn ramp(&mut self, cx: &mut Cx, stops: &[(f64, [f64; 4])]) -> Texture {
        self.table(cx, color::ramp(stops))
    }

    /// A 256×1 table texture, cached by content (ramps, curves).
    fn table(&mut self, cx: &mut Cx, data: Vec<u32>) -> Texture {
        let key = data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, v| (h ^ *v as u64).wrapping_mul(0x100_0000_01b3));
        if self.ramps.len() > 256 {
            self.ramps.clear();
        }
        self.ramps
            .entry(key)
            .or_insert_with(|| Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width: 256, height: 1, data: Some(data), updated: TextureUpdated::Full }).label(cx, "render-kit/ramp"))
            .clone()
    }

    fn lut(&mut self, cx: &mut Cx, file: &str) -> Result<(Texture, usize), String> {
        if !self.luts.contains_key(file) {
            let loaded = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}")).and_then(|text| color::CubeLut::parse(&text).map_err(|e| format!("{file}: {e}")));
            let entry = loaded.map(|lut| {
                let (w, h, data) = lut.atlas();
                (Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width: w, height: h, data: Some(data), updated: TextureUpdated::Full }).label(cx, "render-kit/lut"), lut.size)
            });
            self.luts.insert(file.to_string(), entry);
        }
        self.luts[file].clone()
    }

    /// Hands a parsed LUT to the kit under a name (hosts that load files
    /// off the UI thread, or tests).
    pub fn insert_lut(&mut self, cx: &mut Cx, name: &str, lut: &color::CubeLut) {
        let (w, h, data) = lut.atlas();
        let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width: w, height: h, data: Some(data), updated: TextureUpdated::Full }).label(cx, "render-kit/image");
        self.luts.insert(name.to_string(), Ok((texture, lut.size)));
    }

    /// One effect.
    fn effect(&mut self, cx: &mut Cx2d, e: &ResolvedEffect, input: &Texture, size: (u32, u32), ctx: &FxContext, scale: f64) -> Texture {
        use EffectKind::*;
        let mix = if crate::catalog::info(e.kind).param("mix").is_some() { e.num("mix") } else { 1.0 };
        let preview_levels = if scale < 1.0 { (1.0 / scale).log2().round() as u32 } else { 0 };
        let (w, h) = (size.0 as f64, size.1 as f64);
        let result = match e.kind {
            GaussianBlur => {
                let axes = e.choice_index("direction") as u32;
                self.blur(cx, input, size, e.num("radius"), axes, e.choice_index("edge") as f32, preview_levels)
            }
            DirectionalBlur | RadialBlur | ZoomBlur => {
                let (mode, amount, taps, center, dir) = match e.kind {
                    DirectionalBlur => {
                        let a = e.num("angle").to_radians();
                        let len = e.num("length");
                        (0.0, len, len.ceil().max(1.0), [0.0, 0.0], [a.cos(), -a.sin()])
                    }
                    RadialBlur => {
                        let c = e.vec2("center");
                        let ang = e.num("angle").to_radians();
                        let reach = ((w * w + h * h).sqrt() * 0.5 * ang).ceil();
                        (1.0, ang, reach.max(1.0), [c[0] * w, c[1] * h], [0.0, 0.0])
                    }
                    _ => {
                        let c = e.vec2("center");
                        let amount = e.num("amount");
                        let reach = ((w * w + h * h).sqrt() * 0.5 * amount).ceil();
                        (2.0, amount, reach.max(1.0), [c[0] * w, c[1] * h], [0.0, 0.0])
                    }
                };
                if amount <= 0.0 {
                    input.clone()
                } else {
                    // Nested boxes of 16 taps: pass j spans 16^j of the
                    // finest, all together exactly `amount`, so the cost
                    // grows with the log of the reach, not the reach.
                    let per = 16.0f64.min(taps.max(1.0));
                    let passes = if taps <= 16.0 { 1 } else { (taps.ln() / 16f64.ln()).ceil() as i32 };
                    let total: f64 = (0..passes).map(|j| per.powi(j)).sum();
                    let mut current = input.clone();
                    for j in 0..passes {
                        let part = amount * per.powi(j) / total;
                        let src = current.clone();
                        current = self.one(cx, size, "kit_line_blur", |d| &mut d.line_blur, &[&src], |cx, d| {
                            d.set_f32(cx, live_id!(mode), mode);
                            d.set_f32(cx, live_id!(taps), per as f32);
                            d.set_f32(cx, live_id!(amount), part as f32);
                            d.set_f32(cx, live_id!(edge), 0.0);
                            d.set_vec2(cx, live_id!(center), [center[0] as f32, center[1] as f32]);
                            d.set_vec2(cx, live_id!(axis), [dir[0] as f32, dir[1] as f32]);
                        });
                    }
                    current
                }
            }
            Glow | Halation => {
                let (tint, intensity) = if e.kind == Glow { (e.color("color"), e.num("intensity")) } else { (e.color("color"), e.num("amount")) };
                let knee = if e.kind == Glow { e.num("knee") } else { 0.15 };
                let bright = self.one(cx, size, "kit_threshold", |d| &mut d.threshold, &[input], |cx, d| {
                    d.set_f32(cx, live_id!(threshold), e.f("threshold"));
                    d.set_f32(cx, live_id!(knee), knee as f32);
                    d.set_vec4(cx, live_id!(tint), v4(tint));
                });
                let glow = self.soft(cx, &bright, size, e.num("radius"), 0.0, preview_levels);
                let t = self.transparent.clone();
                self.one(cx, size, "kit_add_light", |d| &mut d.add_light, &[input, &glow, &t, &t, &t], |cx, d| {
                    d.set_vec4(cx, live_id!(weights), vec4(intensity as f32, 0.0, 0.0, 0.0));
                    d.set_f32(cx, live_id!(screen), 0.0);
                })
            }
            Bloom => {
                let bright = self.one(cx, size, "kit_threshold", |d| &mut d.threshold, &[input], |cx, d| {
                    d.set_f32(cx, live_id!(threshold), e.f("threshold"));
                    d.set_f32(cx, live_id!(knee), e.f("knee"));
                    d.set_vec4(cx, live_id!(tint), v4(e.color("tint")));
                });
                // Four scales, each blurred from the one before at its own
                // resolution (gaussians compose: sigma² adds).
                let r = e.num("radius");
                let mut levels = Vec::new();
                let mut prev_sigma = 0.0;
                let mut src = Low { texture: bright, size, level: 0 };
                for k in 0..4 {
                    let sigma = r / 8.0 * (1u32 << k) as f64;
                    let step = (sigma * sigma - prev_sigma * prev_sigma).max(0.0).sqrt();
                    src = self.blur_low(cx, &src, step, 0, 0.0, preview_levels, false);
                    levels.push(src.texture.clone());
                    prev_sigma = sigma;
                }
                let k = e.num("intensity") as f32;
                self.one(cx, size, "kit_add_light", |d| &mut d.add_light, &[input, &levels[0], &levels[1], &levels[2], &levels[3]], |cx, d| {
                    d.set_vec4(cx, live_id!(weights), vec4(0.35 * k, 0.3 * k, 0.2 * k, 0.15 * k));
                    d.set_f32(cx, live_id!(screen), 0.0);
                })
            }
            DropShadow | InnerShadow => {
                let inner = e.kind == InnerShadow;
                let offset = shadow_offset(e.num("angle"), e.num("distance"));
                let spread = if inner { e.num("choke") } else { e.num("spread") };
                let shape = self.one(cx, size, "kit_shadow", |d| &mut d.shadow_mask, &[input], |cx, d| {
                    d.set_vec2(cx, live_id!(offset), [offset[0] as f32, offset[1] as f32]);
                    d.set_f32(cx, live_id!(spread), spread as f32);
                    d.set_f32(cx, live_id!(invert), if inner { 1.0 } else { 0.0 });
                    d.set_vec4(cx, live_id!(tint), premul(e.color("color"), e.num("opacity")));
                });
                let soft = self.soft(cx, &shape, size, e.num("radius"), if inner { 1.0 } else { 0.0 }, preview_levels);
                let mode = if inner { 2.0 } else if e.flag("shadow_only") { 1.0 } else { 0.0 };
                self.one(cx, size, "kit_shadow_comp", |d| &mut d.shadow_comp, &[input, &soft], |cx, d| d.set_f32(cx, live_id!(mode), mode))
            }
            Stroke => {
                let width = e.num("width");
                if width <= 0.0 {
                    input.clone()
                } else {
                    let position = e.choice_index("position") as f32;
                    let reach = if position == 2.0 { width * 0.5 } else { width } + 2.0;
                    let inside = self.flood(cx, input, size, 0.0, reach);
                    let outside = if position > 0.5 { self.flood(cx, input, size, 1.0, reach) } else { inside.clone() };
                    self.one(cx, size, "kit_stroke", |d| &mut d.stroke_comp, &[input, &inside, &outside], |cx, d| {
                        d.set_f32(cx, live_id!(width), width as f32);
                        d.set_f32(cx, live_id!(position), position);
                        d.set_vec4(cx, live_id!(tint), premul(e.color("color"), e.num("opacity")));
                    })
                }
            }
            Bevel => {
                let size_px = e.num("size");
                let height = self.soft(cx, input, size, size_px * 0.5 + e.num("soften"), 0.0, preview_levels);
                let (az, el) = (e.num("angle").to_radians(), e.num("altitude").to_radians());
                let light = [az.cos() * el.cos(), -az.sin() * el.cos(), el.sin()];
                self.one(cx, size, "kit_bevel", |d| &mut d.bevel, &[input, &height], |cx, d| {
                    d.set_f32(cx, live_id!(depth), e.f("depth"));
                    d.set_vec3(cx, live_id!(light_dir), [light[0] as f32, light[1] as f32, light[2] as f32]);
                    d.set_vec4(cx, live_id!(highlight), premul(e.color("highlight"), e.num("highlight_opacity")));
                    d.set_vec4(cx, live_id!(shadow), premul(e.color("shadow"), e.num("shadow_opacity")));
                    d.set_f32(cx, live_id!(extent), size_px as f32);
                })
            }
            Fill => {
                let solid = self.one(cx, size, "kit_solid", |d| &mut d.solid, &[], |cx, d| d.set_vec4(cx, live_id!(fill), premul(e.color("color"), 1.0)));
                self.composite(cx, input, &solid, KitBlend::from_name(e.choice("blend")), e.num("opacity"), e.flag("clip"), size)
            }
            Gradient => {
                let ramp = self.ramp(cx, e.stops("stops"));
                let (s, en) = (e.vec2("start"), e.vec2("end"));
                let layer = self.one(cx, size, "kit_gradient", |d| &mut d.gradient, &[input, &ramp], |cx, d| {
                    d.set_f32(cx, live_id!(kind), e.choice_index("kind") as f32);
                    d.set_f32(cx, live_id!(wrap), e.choice_index("repeat") as f32);
                    d.set_f32(cx, live_id!(dither), 1.0);
                    d.set_vec2(cx, live_id!(start_px), [(s[0] * w) as f32, (s[1] * h) as f32]);
                    d.set_vec2(cx, live_id!(end_px), [(en[0] * w) as f32, (en[1] * h) as f32]);
                });
                self.composite(cx, input, &layer, KitBlend::from_name(e.choice("blend")), e.num("opacity"), e.flag("clip"), size)
            }
            Noise => {
                let offset = e.vec2("offset");
                let layer = self.one(cx, size, "kit_noise", |d| &mut d.noise_gen, &[], |cx, d| {
                    d.set_f32(cx, live_id!(kind), e.choice_index("kind") as f32);
                    d.set_f32(cx, live_id!(scale), e.f("scale"));
                    d.set_f32(cx, live_id!(evolve), (ctx.t * e.num("evolution")) as f32);
                    d.set_f32(cx, live_id!(seed), e.f("seed"));
                    d.set_f32(cx, live_id!(octaves), e.f("octaves"));
                    d.set_f32(cx, live_id!(lacunarity), e.f("lacunarity"));
                    d.set_f32(cx, live_id!(gain), e.f("gain"));
                    d.set_f32(cx, live_id!(contrast), e.f("contrast"));
                    d.set_f32(cx, live_id!(brightness), e.f("brightness"));
                    d.set_vec2(cx, live_id!(offset), [offset[0] as f32, offset[1] as f32]);
                    d.set_vec4(cx, live_id!(color_a), v4(e.color("color_a")));
                    d.set_vec4(cx, live_id!(color_b), v4(e.color("color_b")));
                });
                self.composite(cx, input, &layer, KitBlend::from_name(e.choice("blend")), e.num("opacity"), e.flag("clip"), size)
            }
            StrokePath => self.stroke_path(cx, e, input, size),
            Particles => self.particles(cx, e, input, size, ctx),
            Displace => {
                let map = self.layer_ref(ctx, e.choice("map"), "displace").unwrap_or_else(|| input.clone());
                let amount = e.vec2("amount");
                self.one(cx, size, "kit_displace", |d| &mut d.displace, &[input, &map], |cx, d| {
                    d.set_vec2(cx, live_id!(amount), [amount[0] as f32, amount[1] as f32]);
                    d.set_f32(cx, live_id!(channel_x), e.choice_index("channel_x") as f32);
                    d.set_f32(cx, live_id!(channel_y), e.choice_index("channel_y") as f32);
                    d.set_f32(cx, live_id!(edge), e.choice_index("edge") as f32);
                })
            }
            Slices => {
                // The wave clock on the CPU: which wave, how far into it,
                // and (for `cycle`) which direction this wave cuts.
                let ph = (ctx.t + e.num("offset")) / e.num("period").max(0.01);
                let wave = ph.floor();
                let dir = match e.choice_index("dir") {
                    3 => wave.rem_euclid(3.0),
                    d => d as f64,
                };
                self.one(cx, size, "kit_slices", |d| &mut d.slices, &[input], |cx, d| {
                    d.set_f32(cx, live_id!(phase), (ph - wave) as f32);
                    d.set_f32(cx, live_id!(axis), dir as f32);
                    d.set_f32(cx, live_id!(wave), wave as f32);
                    d.set_f32(cx, live_id!(count), e.num("count").round() as f32);
                    d.set_f32(cx, live_id!(amp), e.f("amp"));
                    d.set_f32(cx, live_id!(spread), e.f("spread"));
                    d.set_f32(cx, live_id!(strip_len), e.f("length"));
                    d.set_f32(cx, live_id!(seed), e.f("seed"));
                    d.set_f32(cx, live_id!(edge), e.choice_index("edge") as f32);
                })
            }
            Turbulence => self.one(cx, size, "kit_turbulence", |d| &mut d.turbulence, &[input], |cx, d| {
                d.set_f32(cx, live_id!(amount), e.f("amount"));
                d.set_f32(cx, live_id!(cell), e.f("size"));
                d.set_f32(cx, live_id!(octaves), e.f("octaves"));
                d.set_f32(cx, live_id!(clock), (ctx.t * e.num("evolution")) as f32);
                d.set_f32(cx, live_id!(seed), e.f("seed"));
                d.set_f32(cx, live_id!(edge), e.choice_index("edge") as f32);
            }),
            Ripple => {
                let c = e.vec2("center");
                self.one(cx, size, "kit_ripple", |d| &mut d.ripple, &[input], |cx, d| {
                    d.set_vec2(cx, live_id!(center), [(c[0] * w) as f32, (c[1] * h) as f32]);
                    d.set_f32(cx, live_id!(wavelength), e.f("wavelength"));
                    d.set_f32(cx, live_id!(amplitude), e.f("amplitude"));
                    d.set_f32(cx, live_id!(phase), (e.num("speed") * ctx.t) as f32);
                    d.set_f32(cx, live_id!(decay), e.f("decay"));
                    d.set_f32(cx, live_id!(edge), e.choice_index("edge") as f32);
                })
            }
            Blend => match self.layer_ref(ctx, e.choice("layer"), "blend") {
                Some(other) => {
                    let mode = KitBlend::from_name(e.choice("mode"));
                    if e.choice("order") == "under" {
                        // This layer over the other, in the mode.
                        self.composite(cx, &other, input, mode, 1.0, false, size)
                    } else {
                        self.composite(cx, input, &other, mode, e.num("opacity"), false, size)
                    }
                }
                None => input.clone(),
            },
            Mask => {
                let (c, s) = (e.vec2("center"), e.vec2("size"));
                self.one(cx, size, "kit_mask", |d| &mut d.shape_mask, &[input], |cx, d| {
                    d.set_f32(cx, live_id!(shape), e.choice_index("shape") as f32);
                    d.set_vec2(cx, live_id!(center), [(c[0] * w) as f32, (c[1] * h) as f32]);
                    d.set_f32(cx, live_id!(rotation), e.num("rotation").to_radians() as f32);
                    d.set_vec2(cx, live_id!(half_size), [(s[0] * w * 0.5) as f32, (s[1] * h * 0.5) as f32]);
                    d.set_f32(cx, live_id!(radius), e.f("radius"));
                    d.set_f32(cx, live_id!(feather), e.f("feather"));
                    d.set_f32(cx, live_id!(expand), e.f("expand"));
                    d.set_f32(cx, live_id!(invert), if e.flag("invert") { 1.0 } else { 0.0 });
                    d.set_f32(cx, live_id!(opacity), e.f("opacity"));
                })
            }
            Matte => match self.layer_ref(ctx, e.choice("layer"), "matte") {
                Some(matte) => {
                    let matte = self.soft(cx, &matte, size, e.num("feather"), 1.0, preview_levels);
                    self.one(cx, size, "kit_matte", |d| &mut d.matte, &[input, &matte], |cx, d| {
                        d.set_f32(cx, live_id!(mode), e.choice_index("mode") as f32);
                        d.set_f32(cx, live_id!(invert), if e.flag("invert") { 1.0 } else { 0.0 });
                    })
                }
                None => input.clone(),
            },
            Transform3d => self.transform_3d(cx, e, input, size, ctx),
            MotionBlur | Echo => input.clone(),
            Curves | Levels | HueSaturation | WhiteBalance | Lut | ChromaticAberration | Grain | Vignette | Exposure | BrightnessContrast | Tint | Invert => {
                return self.color_op(cx, e, input, size, ctx);
            }
        };
        self.mix_with(cx, input, result, mix, size)
    }

    /// Jump flooding: each pixel learns its nearest seed within `reach`.
    fn flood(&mut self, cx: &mut Cx2d, input: &Texture, size: (u32, u32), outside: f32, reach: f64) -> Texture {
        let mut field = self.one(cx, size, "kit_jfa_seed", |d| &mut d.jfa_seed, &[input], |cx, d| d.set_f32(cx, live_id!(outside), outside));
        let mut step = (reach.max(1.0)).log2().ceil().exp2();
        // Jump flooding plus one extra unit step (JFA+1) for exactness.
        let mut steps = Vec::new();
        while step >= 1.0 {
            steps.push(step);
            step /= 2.0;
        }
        steps.push(1.0);
        for s in steps {
            let src = field.clone();
            field = self.one(cx, size, "kit_jfa_step", |d| &mut d.jfa_step, &[&src], |cx, d| d.set_f32(cx, live_id!(jump), s as f32));
        }
        field
    }

    fn stroke_path(&mut self, cx: &mut Cx2d, e: &ResolvedEffect, input: &Texture, size: (u32, u32)) -> Texture {
        let mut polylines: Vec<(Vec<[f64; 2]>, bool)> = Vec::new();
        if !e.points("points").is_empty() {
            polylines.push((e.points("points").to_vec(), e.flag("closed")));
        } else if !e.choice("svg").is_empty() {
            match path::parse_svg(e.choice("svg"), 0.2) {
                Ok(paths) => polylines = paths,
                Err(error) => self.log_once(format!("stroke_path: {error}")),
            }
        }
        let mut segments = Vec::new();
        for (points, closed) in &polylines {
            segments.extend(path::stroke_segments(points, *closed, e.num("width"), e.num("trim_start"), e.num("trim_end"), e.num("offset"), e.num("taper_start"), e.num("taper_end")));
        }
        self.stats.segments += segments.len();
        let color = premul(e.color("color"), e.num("opacity"));
        let butt = e.choice("cap") == "butt";
        let stroke = self.run(cx, size, "kit_stroke_path", |cx, draws, _| {
            let d = &mut draws.segment;
            reset(d);
            d.set_vec4(cx, live_id!(fill), color);
            for s in &segments {
                let r = s.ra.max(s.rb) + 1.0;
                let (x0, y0) = (s.a[0].min(s.b[0]) - r, s.a[1].min(s.b[1]) - r);
                let (x1, y1) = (s.a[0].max(s.b[0]) + r, s.a[1].max(s.b[1]) + r);
                d.set_vec2(cx, live_id!(seg_a), [s.a[0] as f32, s.a[1] as f32]);
                d.set_vec2(cx, live_id!(seg_b), [s.b[0] as f32, s.b[1] as f32]);
                d.set_f32(cx, live_id!(radius_a), s.ra as f32);
                d.set_f32(cx, live_id!(radius_b), s.rb as f32);
                d.set_f32(cx, live_id!(butt), if butt { 1.0 } else { 0.0 });
                d.draw_abs(cx, Rect { pos: dvec2(x0, y0), size: dvec2(x1 - x0, y1 - y0) });
            }
        });
        match e.choice("layer") {
            "under" => self.composite(cx, &stroke, input, KitBlend::Normal, 1.0, false, size),
            "only" => stroke,
            _ => self.composite(cx, input, &stroke, KitBlend::Normal, 1.0, false, size),
        }
    }

    fn particles(&mut self, cx: &mut Cx2d, e: &ResolvedEffect, input: &Texture, size: (u32, u32), ctx: &FxContext) -> Texture {
        let list = particles::particles_at(e, ctx.t, [size.0 as f64, size.1 as f64]);
        if list.len() > 1_000_000 {
            self.log_once(format!("particles: {} alive at once; this frame is heavy", list.len()));
        }
        self.stats.particles += list.len();
        let emitter = if e.choice("emitter") == "layer" { self.layer_ref(ctx, e.choice("from"), "particles") } else { None };
        let check = emitter.is_some();
        let emitter = emitter.unwrap_or_else(|| self.transparent.clone());
        let additive = e.choice("particle_blend") != "normal";
        let shape = e.choice_index("shape") as f32;
        let layer = self.run(cx, size, "kit_particles", |cx, draws, _| {
            let d = &mut draws.particle;
            reset(d);
            d.draw_vars.set_texture(0, &emitter);
            d.set_vec2(cx, live_id!(frame), [size.0 as f32, size.1 as f32]);
            for p in &list {
                // Sparks stretch along their velocity: a bigger quad.
                let speed = (p.velocity[0].powi(2) + p.velocity[1].powi(2)).sqrt();
                let half = if shape == 2.0 { (p.size * 0.5).max(speed * 0.03) + 1.0 } else { p.size * 0.5 + 1.0 };
                d.set_vec2(cx, live_id!(center), [p.pos[0] as f32, p.pos[1] as f32]);
                d.set_f32(cx, live_id!(diameter), p.size as f32);
                d.set_f32(cx, live_id!(rotation), p.rotation as f32);
                d.set_vec4(cx, live_id!(tint), v4(p.color));
                d.set_vec2(cx, live_id!(velocity), [p.velocity[0] as f32, p.velocity[1] as f32]);
                d.set_f32(cx, live_id!(shape), shape);
                d.set_f32(cx, live_id!(additive), if additive { 1.0 } else { 0.0 });
                d.set_vec2(cx, live_id!(birth), [p.birth[0] as f32, p.birth[1] as f32]);
                d.set_f32(cx, live_id!(check), if check { 1.0 } else { 0.0 });
                d.draw_abs(cx, Rect { pos: dvec2(p.pos[0] - half, p.pos[1] - half), size: dvec2(half * 2.0, half * 2.0) });
            }
        });
        let t = self.transparent.clone();
        let base = if e.choice("layer") == "only" { t.clone() } else { input.clone() };
        if additive {
            let screen = if e.choice("particle_blend") == "screen" { 1.0 } else { 0.0 };
            if e.choice("layer") == "under" {
                // Light under the layer: the layer over the lit backdrop.
                let lit = self.one(cx, size, "kit_add_light", |d| &mut d.add_light, &[&t, &layer, &t, &t, &t], |cx, d| {
                    d.set_vec4(cx, live_id!(weights), vec4(1.0, 0.0, 0.0, 0.0));
                    d.set_f32(cx, live_id!(screen), 0.0);
                });
                return self.composite(cx, &lit, input, KitBlend::Normal, 1.0, false, size);
            }
            self.one(cx, size, "kit_add_light", |d| &mut d.add_light, &[&base, &layer, &t, &t, &t], |cx, d| {
                d.set_vec4(cx, live_id!(weights), vec4(1.0, 0.0, 0.0, 0.0));
                d.set_f32(cx, live_id!(screen), screen);
            })
        } else {
            match e.choice("layer") {
                "under" => self.composite(cx, &layer, input, KitBlend::Normal, 1.0, false, size),
                "only" => layer,
                _ => self.composite(cx, input, &layer, KitBlend::Normal, 1.0, false, size),
            }
        }
    }

    fn transform_3d(&mut self, cx: &mut Cx2d, e: &ResolvedEffect, input: &Texture, size: (u32, u32), ctx: &FxContext) -> Texture {
        let default_camera;
        let camera = match ctx.camera {
            Some(camera) => camera,
            None => {
                default_camera = Camera::default_for([size.0 as f64, size.1 as f64]);
                &default_camera
            }
        };
        let plane = Plane::from_effect(e);
        let Some(projection) = space::project_plane(camera, &plane) else {
            return self.transparent.clone();
        };
        let m = projection.inverse;
        let row = |r: [f64; 3]| [r[0] as f32, r[1] as f32, r[2] as f32];
        let flat = self.one(cx, size, "kit_plane", |d| &mut d.plane, &[input], |cx, d| {
            d.set_vec3(cx, live_id!(row_a), row(m[0]));
            d.set_vec3(cx, live_id!(row_b), row(m[1]));
            d.set_vec3(cx, live_id!(row_c), row(m[2]));
        });
        let max_sigma = projection.coc[1] * 0.5;
        if !e.flag("dof") || camera.aperture <= 0.0 || max_sigma < 0.3 {
            return flat;
        }
        let sigmas = [max_sigma / 3.0, max_sigma * 2.0 / 3.0, max_sigma];
        let l1 = self.blur_low(cx, &Low { texture: flat.clone(), size, level: 0 }, sigmas[0], 0, 0.0, 0, false);
        let l2 = self.blur_low(cx, &l1, (sigmas[1].powi(2) - sigmas[0].powi(2)).sqrt(), 0, 0.0, 0, false);
        let l3 = self.blur_low(cx, &l2, (sigmas[2].powi(2) - sigmas[1].powi(2)).sqrt(), 0, 0.0, 0, false);
        let (l1, l2, l3) = (l1.texture, l2.texture, l3.texture);
        self.one(cx, size, "kit_dof", |d| &mut d.dof, &[&flat, &l1, &l2, &l3], |cx, d| {
            d.set_vec3(cx, live_id!(row_c), row(m[2]));
            d.set_f32(cx, live_id!(sigma_1), sigmas[0] as f32);
            d.set_f32(cx, live_id!(sigma_2), sigmas[1] as f32);
            d.set_f32(cx, live_id!(sigma_3), sigmas[2] as f32);
            d.set_f32(cx, live_id!(aperture), camera.aperture as f32);
            d.set_f32(cx, live_id!(focus), camera.focus as f32);
        })
    }

    fn color_op(&mut self, cx: &mut Cx2d, e: &ResolvedEffect, input: &Texture, size: (u32, u32), ctx: &FxContext) -> Texture {
        use EffectKind::*;
        let (w, h) = (size.0 as f64, size.1 as f64);
        let mix = if crate::catalog::info(e.kind).param("mix").is_some() { e.f("mix") } else { 1.0 };
        let t = self.transparent.clone();
        match e.kind {
            Levels => {
                let mask = match e.choice("channel") {
                    "red" => [1.0, 0.0, 0.0],
                    "green" => [0.0, 1.0, 0.0],
                    "blue" => [0.0, 0.0, 1.0],
                    _ => [1.0, 1.0, 1.0],
                };
                self.one(cx, size, "kit_levels", |d| &mut d.levels, &[input, &t], |cx, d| {
                    d.set_f32(cx, live_id!(amount), mix);
                    d.set_f32(cx, live_id!(in_black), e.f("in_black"));
                    d.set_f32(cx, live_id!(in_white), e.f("in_white"));
                    d.set_f32(cx, live_id!(gamma), e.f("gamma"));
                    d.set_f32(cx, live_id!(out_black), e.f("out_black"));
                    d.set_f32(cx, live_id!(out_white), e.f("out_white"));
                    d.set_vec3(cx, live_id!(channels), mask);
                })
            }
            HueSaturation => self.one(cx, size, "kit_hue_saturation", |d| &mut d.hue_saturation, &[input, &t], |cx, d| {
                d.set_f32(cx, live_id!(amount), mix);
                d.set_f32(cx, live_id!(hue), (e.num("hue") / 360.0) as f32);
                d.set_f32(cx, live_id!(saturation), (1.0 + e.num("saturation")) as f32);
                d.set_f32(cx, live_id!(lightness), e.f("lightness"));
                d.set_f32(cx, live_id!(colorize), if e.flag("colorize") { 1.0 } else { 0.0 });
                d.set_f32(cx, live_id!(colorize_hue), (e.num("colorize_hue") / 360.0) as f32);
                d.set_f32(cx, live_id!(colorize_sat), e.f("colorize_saturation"));
            }),
            WhiteBalance => {
                let g = color::white_balance_gains(e.num("temperature"), e.num("tint"));
                self.one(cx, size, "kit_gains", |d| &mut d.gains, &[input, &t], |cx, d| {
                    d.set_f32(cx, live_id!(amount), mix);
                    d.set_vec3(cx, live_id!(gains), [g[0] as f32, g[1] as f32, g[2] as f32]);
                })
            }
            Exposure => self.one(cx, size, "kit_exposure", |d| &mut d.exposure, &[input, &t], |cx, d| {
                d.set_f32(cx, live_id!(amount), mix);
                d.set_f32(cx, live_id!(scale), e.num("exposure").exp2() as f32);
                d.set_f32(cx, live_id!(offset), e.f("offset"));
                d.set_f32(cx, live_id!(power), (1.0 / e.num("gamma")) as f32);
            }),
            BrightnessContrast => self.one(cx, size, "kit_brightness_contrast", |d| &mut d.brightness_contrast, &[input, &t], |cx, d| {
                d.set_f32(cx, live_id!(amount), mix);
                d.set_f32(cx, live_id!(brightness), e.f("brightness"));
                d.set_f32(cx, live_id!(contrast), e.f("contrast"));
            }),
            Tint => {
                let (b, wh) = (e.color("black"), e.color("white"));
                self.one(cx, size, "kit_tint", |d| &mut d.tint, &[input, &t], |cx, d| {
                    d.set_f32(cx, live_id!(amount), mix);
                    d.set_vec4(cx, live_id!(black), v4(b));
                    d.set_vec4(cx, live_id!(white), v4(wh));
                    d.set_f32(cx, live_id!(strength), e.f("amount"));
                })
            }
            Invert => self.one(cx, size, "kit_invert", |d| &mut d.invert, &[input, &t], |cx, d| {
                d.set_f32(cx, live_id!(amount), mix);
            }),
            Vignette => self.one(cx, size, "kit_vignette", |d| &mut d.vignette, &[input, &t], |cx, d| {
                d.set_f32(cx, live_id!(amount), mix);
                d.set_f32(cx, live_id!(strength), e.f("amount"));
                d.set_f32(cx, live_id!(midpoint), e.f("midpoint"));
                d.set_f32(cx, live_id!(roundness), e.f("roundness"));
                d.set_f32(cx, live_id!(feather), e.f("feather"));
                d.set_vec4(cx, live_id!(edge_color), v4(e.color("color")));
            }),
            Grain => {
                let frame = if e.num("rate") > 0.0 { (ctx.t * e.num("rate") + 1e-6).floor() } else { 0.0 };
                let seed = (frame + e.num("seed") * 7.13).rem_euclid(4096.0);
                self.one(cx, size, "kit_grain", |d| &mut d.grain, &[input, &t], |cx, d| {
                    d.set_f32(cx, live_id!(amount), mix);
                    d.set_f32(cx, live_id!(strength), e.f("amount"));
                    d.set_f32(cx, live_id!(grain_size), e.f("size"));
                    d.set_f32(cx, live_id!(colored), if e.flag("color") { 1.0 } else { 0.0 });
                    d.set_f32(cx, live_id!(seed), seed as f32);
                })
            }
            ChromaticAberration => {
                let c = e.vec2("center");
                let a = e.num("angle").to_radians();
                let corner = ((w * 0.5).powi(2) + (h * 0.5).powi(2)).sqrt();
                let linear = if e.choice("mode") == "linear" { 1.0 } else { 0.0 };
                self.one(cx, size, "kit_chroma", |d| &mut d.chroma, &[input, &t], |cx, d| {
                    d.set_f32(cx, live_id!(amount), mix);
                    d.set_f32(cx, live_id!(strength), e.f("amount"));
                    d.set_vec2(cx, live_id!(center), [(c[0] * w) as f32, (c[1] * h) as f32]);
                    d.set_f32(cx, live_id!(linear), linear);
                    d.set_vec2(cx, live_id!(axis), [a.cos() as f32, -a.sin() as f32]);
                    d.set_f32(cx, live_id!(corner), corner as f32);
                })
            }
            Curves => {
                let aux = self.table(cx, color::curves_texture(e.points("master"), e.points("red"), e.points("green"), e.points("blue")));
                self.one(cx, size, "kit_curves", |d| &mut d.curves, &[input, &aux], |cx, d| {
                    d.set_f32(cx, live_id!(amount), mix);
                })
            }
            Lut => match self.lut(cx, e.choice("file")) {
                Ok((aux, n)) => self.one(cx, size, "kit_lut", |d| &mut d.lut, &[input, &aux], |cx, d| {
                    d.set_f32(cx, live_id!(amount), mix);
                    d.set_f32(cx, live_id!(lut_size), n as f32);
                    d.set_f32(cx, live_id!(strength), e.f("intensity"));
                }),
                Err(error) => {
                    self.log_once(format!("lut: {error}"));
                    input.clone()
                }
            },
            _ => input.clone(),
        }
    }
}

fn reset(d: &mut DrawKit) {
    d.reset();
    // Stale bindings would make this pass wait on whatever produced them.
    for i in 0..8 {
        d.draw_vars.empty_texture(i);
    }
}

#[cfg(test)]
mod tests {
    use super::shadow_offset;

    #[test]
    fn light_from_the_upper_left_casts_down_right() {
        let [x, y] = shadow_offset(120.0, 10.0);
        assert!(x > 0.0 && y > 0.0, "{x} {y}");
        let [x, y] = shadow_offset(90.0, 10.0);
        assert!(x.abs() < 1e-9 && (y - 10.0).abs() < 1e-9);
    }
}
