//! A kept phosphor image, accumulated and graded on the GPU.
//!
//! A freeze costs one full-screen copy pass, once. While the ghost is on, each
//! graded pixel samples one more texel. Freezing uploads nothing.
//! A live twin adds one dirty-rectangle upload (and CPU copy of those rows) and one R32F
//! accumulation quad per secondary frame. Both pictures share the final grading quad; swapping
//! exchanges retained textures and buffers without uploading or restarting either source.
//! Each accumulation also uploads its columns' blends, two floats per column (a picture's columns
//! decay apart while a long window sweeps across it).
//! The dark look's glow costs three passes at a quarter of the plot's size (a 16-tap
//! downsample and a five-tap blur on each axis) per drawn frame, and one filtered sample per
//! graded pixel; the beam's finite spot costs eight more nearest samples per pixel.

use super::phosphor::{grade_x, Blend, Dirty, Hits, Persistence, PhosphorMap};
use crate::live::{ramp, RampId};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.elec_live.*

    mod.draw.DrawPhosphorAccum = set_type_default() do #(DrawPhosphorAccum::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color_format: @Rf32
        prev: texture_2d(float)
        hits: texture_2d(float)
        // Per column: keep in the first row, scale in the second; read while `columns` is set.
        blend: texture_2d(float)
        keep: uniform(0.0)
        scale: uniform(0.0)
        columns: uniform(0.0)
        rect: uniform(vec4(0.0, 0.0, 0.0, 0.0))

        pixel: fn() {
            var keep = self.keep
            var scale = self.scale
            if self.columns > 0.5 {
                keep = self.blend.sample_nearest(vec2(self.pos.x, 0.25)).x
                scale = self.blend.sample_nearest(vec2(self.pos.x, 0.75)).x
            }
            var p = 0.0
            if keep > 0.0 {
                p = keep * self.prev.sample_nearest(self.pos).x
            }
            let pixel = floor(self.pos * self.rect_size)
            if scale > 0.0 && pixel.x >= self.rect.x && pixel.x < self.rect.z
                && pixel.y >= self.rect.y && pixel.y < self.rect.w {
                p += scale * self.hits.sample_nearest(vec2(self.pos.y, self.pos.x)).x
            }
            return vec4(p, 0.0, 0.0, 1.0)
        }
    }

    mod.draw.DrawWaterfallRow = set_type_default() do #(DrawWaterfallRow::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color_format: @Rf32
        hits: texture_2d(float)
        rect: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clear_row: uniform(0.0)
        pixel: fn() {
            var level = 0.0 - 1.0
            let size = self.hits.size()
            let col = floor(self.pos.x * size.y)
            // The upload is transposed, and texels outside its dirty rectangle may be stale.
            if self.clear_row < 0.5 && col >= self.rect.x && col < self.rect.z {
                var row = self.rect.y
                while row < self.rect.w {
                    if self.hits.sample_nearest(vec2((row + 0.5) / size.x, (col + 0.5) / size.y)).x > 0.0 {
                        level = 1.0 - row / max(size.x - 1.0, 1.0)
                        break
                    }
                    row += 1.0
                }
            }
            return vec4(level, 0.0, 0.0, 1.0)
        }
    }

    mod.draw.DrawWaterfallGrade = set_type_default() do #(DrawWaterfallGrade::script_shader(vm)) {
        ..mod.draw.DrawQuad
        ring: texture_2d(float)
        head: uniform(0.0)
        live_light: uniform(0.0)
        screen: uniform(vec3(0.0, 0.0, 0.0))
        channel: uniform(vec4(0.0, 0.0, 0.0, 0.0))

        grade: fn(x: float) -> vec3 {
            if self.channel.w < 0.5 { return Live.grade(x, self.live_light) }
            if self.live_light > 0.5 { return mix(self.screen, self.channel.xyz, clamp(0.15 + 0.85 * x, 0.0, 1.0)) }
            return mix(self.channel.xyz * 0.15, mix(self.channel.xyz, vec3(1.0, 1.0, 1.0), 0.85), x)
        }
        pixel: fn() {
            let height = self.ring.size().y
            let row = floor(self.pos.y * height)
            let y = fract((self.head - 1.0 - row + 0.5) / height)
            let level = self.ring.sample_nearest(vec2(self.pos.x, y)).x
            if level < 0.0 { return vec4(self.screen, 1.0) }
            return vec4(self.grade(level), 1.0)
        }
    }

    mod.draw.DrawPhosphorGlow = set_type_default() do #(DrawPhosphorGlow::script_shader(vm)) {
        ..mod.draw.DrawQuad
        src: texture_2d(float)
        mode: uniform(0.0)
        gain: uniform(1000.0)

        intensity: fn(p: float) -> float {
            return clamp(log(1.0 + max(p * self.gain, 0.0)) / log(1.0 + max(self.gain, 1.0)), 0.0, 1.0)
        }

        tap: fn(uv: vec2) -> float {
            return self.intensity(self.src.sample_nearest(uv).x)
        }

        // Each glow texel is the mean brightness of the 4 x 4 picture pixels under it.
        down: fn() -> float {
            let d = vec2(1.0, 1.0) / self.src.size()
            let c = self.pos
            return (self.tap(c + d * vec2(0.0 - 1.5, 0.0 - 1.5)) + self.tap(c + d * vec2(0.0 - 0.5, 0.0 - 1.5)) + self.tap(c + d * vec2(0.5, 0.0 - 1.5)) + self.tap(c + d * vec2(1.5, 0.0 - 1.5)) + self.tap(c + d * vec2(0.0 - 1.5, 0.0 - 0.5)) + self.tap(c + d * vec2(0.0 - 0.5, 0.0 - 0.5)) + self.tap(c + d * vec2(0.5, 0.0 - 0.5)) + self.tap(c + d * vec2(1.5, 0.0 - 0.5)) + self.tap(c + d * vec2(0.0 - 1.5, 0.5)) + self.tap(c + d * vec2(0.0 - 0.5, 0.5)) + self.tap(c + d * vec2(0.5, 0.5)) + self.tap(c + d * vec2(1.5, 0.5)) + self.tap(c + d * vec2(0.0 - 1.5, 1.5)) + self.tap(c + d * vec2(0.0 - 0.5, 1.5)) + self.tap(c + d * vec2(0.5, 1.5)) + self.tap(c + d * vec2(1.5, 1.5))) / 16.0
        }

        // One axis of a gaussian (sigma two quarter texels, about 8 picture pixels) in five filtered taps.
        blur: fn(axis: vec2) -> float {
            let d = axis / self.src.size()
            let c = self.pos
            return self.src.sample(c).x * 0.2
                + (self.src.sample(c + d * 1.405).x + self.src.sample(c - d * 1.405).x) * 0.296
                + (self.src.sample(c + d * 3.29).x + self.src.sample(c - d * 3.29).x) * 0.092
        }

        pixel: fn() {
            var g = 0.0
            if self.mode < 0.5 {
                g = self.down()
            } else if self.mode < 1.5 {
                g = self.blur(vec2(1.0, 0.0))
            } else {
                g = self.blur(vec2(0.0, 1.0))
            }
            return vec4(g, g, g, 1.0)
        }
    }

    mod.draw.DrawPhosphorGrade = set_type_default() do #(DrawPhosphorGrade::script_shader(vm)) {
        ..mod.draw.DrawQuad
        persistence: texture_2d(float)
        ghost: texture_2d(float)
        glow: texture_2d(float)
        stretch: uniform(vec4(1.0, 1.0, 0.0, 0.0))
        ghost_on: uniform(0.0)
        ghost_top: uniform(0.0)
        twin_channel: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        twin_weight: uniform(1.0)
        ghost_stretch: uniform(vec4(1.0, 1.0, 0.0, 0.0))
        glow_on: uniform(0.0)
        gain: uniform(1000.0)
        bloom: uniform(0.6)
        live_light: uniform(0.0)
        screen: uniform(vec3(0.0, 0.0, 0.0))
        channel: uniform(vec4(0.0, 0.0, 0.0, 0.0))

        grade: fn(x: float) -> vec3 {
            if self.channel.w < 0.5 { return Live.grade(x, self.live_light) }
            if self.live_light > 0.5 { return mix(self.screen, self.channel.xyz, clamp(0.15 + 0.85 * x, 0.0, 1.0)) }
            return mix(self.channel.xyz * 0.15, mix(self.channel.xyz, vec3(1.0, 1.0, 1.0), 0.85), x)
        }

        twin_grade: fn(x: float) -> vec3 {
            if self.live_light > 0.5 { return mix(self.screen, self.twin_channel.xyz, clamp(0.15 + 0.85 * x, 0.0, 1.0)) }
            // The guest has no beam spread around it, so a hot core near white would lose its
            // hue: an overlaid net stays recognisably its own colour at full intensity.
            return mix(self.twin_channel.xyz * 0.15, mix(self.twin_channel.xyz, vec3(1.0, 1.0, 1.0), 0.3), x)
        }

        intensity: fn(p: float) -> float {
            return clamp(log(1.0 + max(p * self.gain, 0.0)) / log(1.0 + max(self.gain, 1.0)), 0.0, 1.0)
        }

        ring: fn(uv: vec2, radius: float) -> float {
            let d = vec2(radius, radius) / self.persistence.size()
            return self.persistence.sample_nearest(uv + d * vec2(1.0, 0.0)).x
                + self.persistence.sample_nearest(uv + d * vec2(0.70710678, 0.70710678)).x
                + self.persistence.sample_nearest(uv + d * vec2(0.0, 1.0)).x
                + self.persistence.sample_nearest(uv + d * vec2(0.0 - 0.70710678, 0.70710678)).x
                + self.persistence.sample_nearest(uv + d * vec2(0.0 - 1.0, 0.0)).x
                + self.persistence.sample_nearest(uv + d * vec2(0.0 - 0.70710678, 0.0 - 0.70710678)).x
                + self.persistence.sample_nearest(uv + d * vec2(0.0, 0.0 - 1.0)).x
                + self.persistence.sample_nearest(uv + d * vec2(0.70710678, 0.0 - 0.70710678)).x
        }

        pixel: fn() {
            let uv = self.pos * self.stretch.xy + self.stretch.zw
            // Revealed pixels have no history; never smear the old edge texels into them.
            if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 { return vec4(self.screen, 1.0) }
            let p = self.persistence.sample_nearest(uv).x
            var x = self.intensity(p)
            if self.live_light < 0.5 {
                // The beam has a finite spot: a path one pixel wide lights its neighbours at about
                // half. Ink on a light screen does not spread.
                x = max(x, self.intensity(self.ring(uv, 1.0) / 8.0) * 0.55)
            }
            let a = clamp(x * 8.0, 0.0, 1.0)
            let live = self.grade(x)
            var color = self.screen
            if self.ghost_on > 0.5 {
                let guv = self.pos * self.ghost_stretch.xy + self.ghost_stretch.zw
                var gp = 0.0
                if guv.x >= 0.0 && guv.x <= 1.0 && guv.y >= 0.0 && guv.y <= 1.0 { gp = self.ghost.sample_nearest(guv).x }
                let gx = self.intensity(gp)
                let grade = self.grade(gx)
                let luma = dot(grade, vec3(0.2126, 0.7152, 0.0722))
                var muted = mix(grade, vec3(luma, luma, luma), 0.65)
                var weight = clamp(gx * 8.0, 0.0, 1.0) * 0.45
                if self.twin_channel.w > 0.5 {
                    muted = self.twin_grade(gx)
                    weight = clamp(gx * 8.0, 0.0, 1.0) * self.twin_weight
                }
                if self.ghost_top > 0.5 && self.twin_channel.w < 0.5 {
                    color = mix(color, live, a)
                    color = mix(color, muted, weight)
                } else {
                    color = mix(color, muted, weight)
                    color = mix(color, live, a)
                }
            } else {
                color = mix(color, live, a)
            }
            if self.live_light < 0.5 && self.bloom > 0.0 && self.glow_on > 0.5 {
                // The halo takes the trace's mid colour and falls off with the blurred brightness.
                let g = min(self.glow.sample(uv).x * 6.0, 1.0)
                color += self.grade(0.35 + 0.4 * g) * self.bloom * g * 0.9
            }
            return vec4(color, 1.0)
        }
    }

    mod.widgets.PhosphorViewBase = #(PhosphorView::register_widget(vm))
    mod.widgets.PhosphorView = set_type_default() do mod.widgets.PhosphorViewBase {
        width: Fill height: Fill
        screen: #000
        draw_accumulate: mod.draw.DrawPhosphorAccum {}
        draw_grade: mod.draw.DrawPhosphorGrade {}
        draw_glow: mod.draw.DrawPhosphorGlow {}
        draw_waterfall_row: mod.draw.DrawWaterfallRow {}
        draw_waterfall_grade: mod.draw.DrawWaterfallGrade {}
    }
}

/// Writes a waveform-weighted mean from the previous picture and new hits.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawPhosphorAccum {
    #[deref]
    pub draw_super: DrawQuad,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawWaterfallRow { #[deref] pub draw_super: DrawQuad }

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawWaterfallGrade { #[deref] pub draw_super: DrawQuad }

/// Downsamples the kept image's brightness and blurs it for the dark look's glow.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawPhosphorGlow {
    #[deref]
    pub draw_super: DrawQuad,
}

/// Grades the kept image into an opaque screen colour.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawPhosphorGrade {
    #[deref]
    pub draw_super: DrawQuad,
}

/// The old picture sampled at the new screen's coordinates; no image copy or CPU resampling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stretch { pub from: PhosphorMap, pub to: PhosphorMap }

impl Stretch {
    fn matches(self) -> bool {
        // Host constructors can round-trip a span through seconds/pixel or volts/pixel.
        // Compare within arithmetic roundoff, using the span rather than a large DC offset.
        let epsilon = 16.0 * f64::EPSILON;
        (self.from.w, self.from.h) == (self.to.w, self.to.h)
            && (self.from.t_px - self.to.t_px).abs() <= self.to.t_px.abs() * epsilon
            && (self.from.v_px - self.to.v_px).abs() <= self.to.v_px.abs() * epsilon
            && (self.from.t_left - self.to.t_left).abs() <= self.to.t_px.abs() * self.to.w.max(1) as f64 * epsilon
            && (self.from.v_top - self.to.v_top).abs() <= self.to.v_px.abs() * self.to.h.max(1) as f64 * epsilon
    }

    fn uniform(self) -> [f32; 4] {
        let time = self.from.t_px * self.from.w.max(1) as f64;
        let value = self.from.v_px * self.from.h.max(1) as f64;
        [
            (self.to.t_px * self.to.w.max(1) as f64 / time) as f32,
            (self.to.v_px * self.to.h.max(1) as f64 / value) as f32,
            ((self.to.t_left - self.from.t_left) / time) as f32,
            ((self.from.v_top - self.to.v_top) / value) as f32,
        ]
    }
}

/// Persistence and display controls for a phosphor screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhosphorLook {
    /// Decay time in seconds; zero replaces, infinity averages all waveforms.
    pub tau: f64,
    pub gain: f32,
    pub bloom: f32,
    pub light: bool,
}

impl Default for PhosphorLook {
    fn default() -> Self {
        Self {
            tau: 0.5,
            gain: 1000.0,
            bloom: 0.6,
            light: false,
        }
    }
}

/// The screen colour at hit fraction `p`, without bloom. `ring` is the mean fraction of the eight
/// neighbouring texels ([`spot_ring`]); on a dark screen it lights the beam's spot at 0.55 of its
/// grade, as the grading shader does.
pub fn graded(p: f32, ring: f32, look: &PhosphorLook, screen: [f32; 3]) -> [f32; 3] {
    let mut x = grade_x(p * look.gain, look.gain);
    if !look.light {
        x = x.max(grade_x(ring * look.gain, look.gain) * 0.55);
    }
    let c = ramp(if look.light { RampId::PhosphorLight } else { RampId::PhosphorDark }, x);
    let a = (x * 8.0).clamp(0.0, 1.0);
    let color = [c.x, c.y, c.z];
    std::array::from_fn(|i| screen[i] + (color[i] - screen[i]) * a)
}

/// The mean of the eight texels around (`col`, `row`), clamped to the edge as the sampler is: the
/// beam spot's taps at radius one texel.
pub fn spot_ring(hits: &Hits, col: u32, row: u32) -> f32 {
    let mut sum = 0.0;
    for (dc, dr) in [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)] {
        let c = (col as i64 + dc).clamp(0, hits.w as i64 - 1) as u32;
        let r = (row as i64 + dr).clamp(0, hits.h as i64 - 1) as u32;
        sum += hits.at(c, r);
    }
    sum / 8.0
}

/// Ownership of a submitted hits buffer.
#[derive(Debug)]
pub enum Pushed {
    /// Accepted, with the previous buffer available for reuse.
    Taken(Option<Hits>),
    /// A frame is already pending, or a recording still reads the last one; retain this buffer and retry
    /// after drawing.
    Busy(Hits),
    /// The preceding upload, or the recording that reads it, is still waiting after two draw events; do
    /// not retry this buffer.
    Hidden(Hits),
    /// The buffer does not match the plot's device size.
    WrongSize(Hits),
}

/// When one of the view's passes was recorded (`Cx::repaint_id` and `Cx::drawing_id` then), and whether
/// it reads the hits texture.
#[derive(Clone, Copy)]
struct Recorded { repaint: u64, drawing: u64, reads_hits: bool }

impl Recorded {
    fn now(cx: &Cx, reads_hits: bool) -> Option<Self> { Some(Self { repaint: cx.repaint_id(), drawing: cx.drawing_id(), reads_hits }) }
}

/// The recording of `pass` while the GPU has not painted it whole: the frame that paints it moves
/// `painted_serial` past the repaint it was recorded in. A frame drawn again before its paint (a glyph
/// wait), or a paint that left the draw out, leaves it waiting, and the step it records (a picture, a
/// ghost, a waterfall row) is not there yet.
fn unpainted(cx: &Cx, pass: &DrawPass, recorded: Option<Recorded>) -> Option<Recorded> {
    recorded.filter(|at| cx.passes[pass.draw_pass_id()].painted_serial <= at.repaint)
}

/// The drawing of a recording that still has to read the hits texture: an upload waits for its paint.
fn reading_hits(recorded: Option<Recorded>) -> Option<u64> {
    recorded.filter(|at| at.reads_hits).map(|at| at.drawing)
}

/// Keeps a recording an earlier drawing made until it has painted: attached again under the pass being
/// drawn, it paints with this frame, from the inputs it read (the view uploads no hits meanwhile).
fn attach_unpainted(cx: &mut Cx2d, pass: &DrawPass) {
    cx.make_child_pass(pass);
    cx.repaint_pass(pass.draw_pass_id());
}

struct Pictures {
    textures: [Texture; 2],
    passes: [DrawPass; 2],
    lists: [DrawList2d; 2],
    current: usize,
    /// The recording of the step that made `current`, until it has painted.
    recorded: Option<Recorded>,
}

impl Pictures {
    fn new(cx: &mut Cx, pixels: (u32, u32)) -> Self {
        let textures = std::array::from_fn(|_| {
            let texture = Texture::new_with_format(
                cx,
                TextureFormat::RenderRf32 {
                    size: TextureSize::Fixed {
                        width: pixels.0 as usize,
                        height: pixels.1 as usize,
                    },
                    initial: true,
                },
            );
            texture.set_transient(cx, false);
            texture
        });
        let passes = std::array::from_fn(|i| {
            let pass = DrawPass::new(cx);
            // InitWith clears the new allocation once; every later pass overwrites it.
            pass.set_color_texture(cx, &textures[i], DrawPassClearColor::InitWith(vec4(0.0, 0.0, 0.0, 0.0)));
            pass.set_size(cx, dvec2(pixels.0 as f64, pixels.1 as f64));
            pass
        });
        Self {
            textures,
            passes,
            lists: std::array::from_fn(|_| DrawList2d::new(cx)),
            current: 0,
            recorded: None,
        }
    }

    /// The current picture's step while it has not painted.
    fn unpainted(&self, cx: &Cx) -> Option<Recorded> { unpainted(cx, &self.passes[self.current], self.recorded) }

    /// Withdraws both passes, except the current picture's step while it has not painted: an old recording
    /// never replays against newer hits or a swapped picture, and a fresh one is never lost to a frame
    /// drawn again before its paint.
    fn withdraw_painted(&self, cx: &mut Cx) {
        let keep = self.unpainted(cx).map(|_| self.current);
        for (index, pass) in self.passes.iter().enumerate() {
            if keep != Some(index) { pass.withdraw(cx); }
        }
    }
}

/// The glow chain: the picture's brightness at a quarter of its size, then blurred across and down. Each is a
/// pass the next one depends on, so the GPU orders them after this frame's accumulation.
struct Glow {
    textures: [Texture; 3],
    passes: [DrawPass; 3],
    lists: [DrawList2d; 3],
    size: DVec2,
    ready: bool,
}

impl Glow {
    fn new(cx: &mut Cx, pixels: (u32, u32)) -> Self {
        let (w, h) = (pixels.0.div_ceil(4).max(1), pixels.1.div_ceil(4).max(1));
        let textures = std::array::from_fn(|_| {
            let texture = Texture::new_with_format(
                cx,
                TextureFormat::RenderBGRAu8 {
                    size: TextureSize::Fixed {
                        width: w as usize,
                        height: h as usize,
                    },
                    initial: true,
                },
            );
            texture.set_transient(cx, false);
            texture
        });
        let size = dvec2(w as f64, h as f64);
        let passes = std::array::from_fn(|i| {
            let pass = DrawPass::new(cx);
            pass.set_color_texture(cx, &textures[i], DrawPassClearColor::InitWith(vec4(0.0, 0.0, 0.0, 0.0)));
            pass.set_size(cx, size);
            pass
        });
        Self {
            textures,
            passes,
            lists: std::array::from_fn(|_| DrawList2d::new(cx)),
            size,
            ready: false,
        }
    }
}

struct Ghost {
    texture: Texture,
    pass: DrawPass,
    list: DrawList2d,
    pixels: (u32, u32),
    /// The copy's recording, until it has painted.
    recorded: Option<Recorded>,
}

impl Ghost {
    fn new(cx: &mut Cx, pixels: (u32, u32)) -> Self {
        let texture = Texture::new_with_format(
            cx,
            TextureFormat::RenderRf32 {
                size: TextureSize::Fixed {
                    width: pixels.0 as usize,
                    height: pixels.1 as usize,
                },
                initial: true,
            },
        );
        texture.set_transient(cx, false);
        let pass = DrawPass::new(cx);
        pass.set_color_texture(cx, &texture, DrawPassClearColor::InitWith(vec4(0.0, 0.0, 0.0, 0.0)));
        pass.set_size(cx, dvec2(pixels.0 as f64, pixels.1 as f64));
        Self {
            texture,
            pass,
            list: DrawList2d::new(cx),
            pixels,
            recorded: None,
        }
    }
}

// The secondary keeps the same accumulation state as the primary. Only the primary pays
// for bloom and waterfall. Exchanging this state swaps pictures without copying or reacquiring.
#[derive(Default)]
struct LiveGhost {
    pictures: Option<Pictures>,
    hits_texture: Option<Texture>,
    blend_texture: Option<Texture>,
    uploaded: Option<Hits>,
    uploaded_bytes: usize,
    pending_hits: bool,
    accepted_at: u64,
    clear_pending: bool,
    persistence: Persistence,
    picture_map: Option<PhosphorMap>,
    held: Option<f64>,
    below: Option<(f64, f64)>,
    stretch: Option<Stretch>,
    last_draw: Option<f64>,
    picture_has_data: bool,
}

impl LiveGhost {
    fn exchange(&mut self, view: &mut PhosphorView) {
        std::mem::swap(&mut self.pictures, &mut view.pictures);
        std::mem::swap(&mut self.hits_texture, &mut view.hits_texture);
        std::mem::swap(&mut self.blend_texture, &mut view.blend_texture);
        std::mem::swap(&mut self.uploaded, &mut view.uploaded);
        std::mem::swap(&mut self.uploaded_bytes, &mut view.uploaded_bytes);
        std::mem::swap(&mut self.pending_hits, &mut view.pending_hits);
        std::mem::swap(&mut self.accepted_at, &mut view.accepted_at);
        std::mem::swap(&mut self.clear_pending, &mut view.clear_pending);
        std::mem::swap(&mut self.persistence, &mut view.persistence);
        std::mem::swap(&mut self.picture_map, &mut view.picture_map);
        std::mem::swap(&mut self.held, &mut view.held);
        std::mem::swap(&mut self.stretch, &mut view.stretch);
        std::mem::swap(&mut self.last_draw, &mut view.last_draw);
        std::mem::swap(&mut self.picture_has_data, &mut view.picture_has_data);
    }
}

/// Allocated when the widget is constructed, including the bench's spare card.
/// `recorded`: the last row's (or clear's) recording, until it has painted.
struct Waterfall { texture: Texture, pass: DrawPass, list: DrawList2d, pixels: (u32, u32), head: u32, clear: bool, recorded: Option<Recorded> }

impl Waterfall {
    fn new(cx: &mut Cx) -> Self {
        let texture = Texture::new_with_format(cx, TextureFormat::RenderRf32 { size: TextureSize::Fixed { width: 1, height: 1 }, initial: true });
        texture.set_transient(cx, false);
        let pass = DrawPass::new(cx);
        pass.set_color_texture(cx, &texture, DrawPassClearColor::InitWith(vec4(-1.0, 0.0, 0.0, 1.0)));
        Self { texture, pass, list: DrawList2d::new(cx), pixels: (1, 1), head: 0, clear: true, recorded: None }
    }
    fn resize(&mut self, cx: &mut Cx, plot: (u32, u32)) {
        let pixels = (plot.0.max(1), (plot.1 / 2).max(1));
        if self.pixels == pixels { return; }
        *self.texture.get_format(cx) = TextureFormat::RenderRf32 { size: TextureSize::Fixed { width: pixels.0 as usize, height: pixels.1 as usize }, initial: true };
        self.pixels = pixels;
        self.head = 0;
        self.clear = true;
        self.recorded = None;
    }
}

/// A plot whose device pixels hold decaying waveform hit fractions.
#[derive(Script, WidgetRegister, WidgetRef, WidgetSet)]
pub struct PhosphorView {
    #[uid]
    uid: WidgetUid,
    #[source]
    script_source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    pub screen: Vec4f,
    #[live]
    pub draw_accumulate: DrawPhosphorAccum,
    #[live]
    pub draw_grade: DrawPhosphorGrade,
    #[live]
    pub draw_glow: DrawPhosphorGlow,
    #[live]
    pub draw_waterfall_row: DrawWaterfallRow,
    #[live]
    pub draw_waterfall_grade: DrawWaterfallGrade,
    #[rust]
    channel: [f32; 4],
    #[rust]
    twin_channel: Option<[f32; 4]>,
    #[rust]
    twin_preview: bool,
    #[rust]
    waterfall: Option<Waterfall>,
    #[rust]
    waterfall_on: bool,
    #[rust]
    waterfall_pending: bool,
    #[rust]
    area: Area,
    #[rust]
    look: PhosphorLook,
    #[rust]
    pictures: Option<Pictures>,
    #[rust]
    picture_map: Option<PhosphorMap>,
    #[rust]
    target_map: Option<PhosphorMap>,
    #[rust]
    stretch: Option<Stretch>,
    #[rust]
    ghost: Option<Ghost>,
    #[rust]
    twin: Option<LiveGhost>,
    #[rust]
    twin_active: bool,
    #[rust]
    picture_has_data: bool,
    #[rust]
    glow: Option<Glow>,
    #[rust]
    ghost_on: bool,
    #[rust]
    ghost_top: bool,
    /// Whether the newest waveform shown holds a steady source's values, and from when.
    #[rust]
    held: Option<f64>,
    /// The newest waveform's stretch below the simulation's time resolution (`Hits::below`).
    #[rust]
    below: Option<(f64, f64)>,
    #[rust]
    freeze_pending: bool,
    #[rust]
    hits_texture: Option<Texture>,
    #[rust]
    pixels: Option<(u32, u32)>,
    #[rust]
    raster_size: Option<DVec2>,
    #[rust]
    persistence: Persistence,
    /// The persistence a blend would leave, kept only once its accumulation is recorded.
    #[rust]
    trial: Persistence,
    /// The next accumulation's per-column blends, uploaded to `blend_texture`.
    #[rust]
    blends: Vec<Blend>,
    #[rust]
    blend_texture: Option<Texture>,
    // The uploaded frame's metadata; its data is owned by hits_texture.
    #[rust]
    uploaded: Option<Hits>,
    #[rust]
    uploaded_bytes: usize,
    #[rust]
    pending_hits: bool,
    #[rust]
    accepted_at: u64,
    #[rust(true)]
    clear_pending: bool,
    #[rust]
    last_draw: Option<f64>,
    #[rust]
    frame_interval: Option<f64>,
    #[rust]
    next_frame: NextFrame,
}

impl ScriptHook for PhosphorView {
    fn on_after_new(&mut self, vm: &mut ScriptVm) { self.waterfall = Some(Waterfall::new(vm.cx_mut())); }
}

impl PhosphorView {
    /// Set on each mapping edit and draw. An identity transform still records the desired map
    /// so a delayed frame cannot replace a picture already rasterised at the current settings.
    pub fn set_stretch(&mut self, stretch: Option<Stretch>) {
        self.target_map = stretch.map(|s| s.to);
        self.stretch = stretch.filter(|s| !s.matches());
        if let Some(twin) = &mut self.twin {
            twin.stretch = twin.picture_map.zip(self.target_map).map(|(from, to)| Stretch { from, to }).filter(|s| !s.matches());
        }
    }

    /// Axes of the picture actually accumulated, not merely an uploaded or queued frame.
    pub fn picture_map(&self) -> Option<PhosphorMap> { self.picture_map }

    /// The newest waveform shown repeats a steady source's values from this trigger-relative time.
    pub fn held(&self) -> Option<f64> { self.held }

    /// The newest waveform shown has this trigger-relative stretch below the simulation's time
    /// resolution.
    pub fn below(&self) -> Option<(f64, f64)> { self.below }

    fn refresh_stretch(&mut self) {
        self.stretch = self.picture_map.zip(self.target_map).map(|(from, to)| Stretch { from, to }).filter(|s| !s.matches());
    }

    /// Channel hue with monotone lightness; alpha zero selects the temperature density lens.
    pub fn set_channel(&mut self, channel: [f32; 4]) { self.channel = channel; }

    pub fn set_twin_channel(&mut self, channel: Option<[f32; 4]>) { self.twin_channel = channel; }
    pub(super) fn set_twin_preview(&mut self, preview: bool) { self.twin_preview = preview; }

    pub fn set_waterfall(&mut self, on: bool) { self.waterfall_on = on; }

    /// While set, the raster keeps this logical size and the picture is stretched to the drawn rect
    /// (a display-only drag keeps its picture until the release lets the raster follow the rect).
    pub(super) fn set_raster_size(&mut self, size: Option<DVec2>) { self.raster_size = size; }

    /// A scrub replaces persistence but leaves the live waterfall untouched.
    pub fn push_still(&mut self, cx: &mut Cx, hits: Hits) -> Pushed {
        let pushed = self.push_frame(cx, hits);
        if matches!(&pushed, Pushed::Taken(_)) { self.waterfall_pending = false; }
        pushed
    }

    /// Draw the kept ring under the scope; newest row first, with the phosphor palette.
    pub fn draw_waterfall(&mut self, cx: &mut Cx2d, rect: Rect) {
        let Some(ring) = &self.waterfall else { return };
        let vars = &mut self.draw_waterfall_grade.draw_vars;
        vars.set_texture(0, &ring.texture);
        vars.set_uniform(cx, id!(head), &[ring.head as f32]);
        vars.set_uniform(cx, id!(live_light), &[if self.look.light { 1.0 } else { 0.0 }]);
        vars.set_uniform(cx, id!(screen), &[self.screen.x, self.screen.y, self.screen.z]);
        vars.set_uniform(cx, id!(channel), &self.channel);
        self.draw_waterfall_grade.draw_abs(cx, rect);
    }

    fn waterfall_row(&mut self, cx: &mut Cx2d) {
        let Some(ring) = &mut self.waterfall else { return };
        if !ring.clear && unpainted(cx, &ring.pass, ring.recorded).is_some() { attach_unpainted(cx, &ring.pass); return; }
        if !ring.clear && !self.waterfall_pending { return; }
        let ready = self.draw_waterfall_row.draw_vars.draw_shader_id.is_some_and(|id| cx.draw_shader_ready(id, false));
        let clearing = ring.clear;
        let size = dvec2(ring.pixels.0 as f64, ring.pixels.1 as f64);
        ring.pass.set_size(cx, size);
        cx.make_child_pass(&ring.pass);
        cx.begin_pass(&ring.pass, Some(1.0));
        ring.list.begin_always(cx);
        cx.begin_root_turtle(size, Layout::flow_overlay());
        let dirty = self.uploaded.as_ref().map_or(Dirty::default(), |hits| hits.dirty);
        let vars = &mut self.draw_waterfall_row.draw_vars;
        vars.set_texture(0, self.hits_texture.as_ref().unwrap());
        vars.set_uniform(cx, id!(clear_row), &[if clearing { 1.0 } else { 0.0 }]);
        vars.set_uniform(cx, id!(rect), &[dirty.c0 as f32, dirty.r0 as f32, dirty.c1 as f32, dirty.r1 as f32]);
        let rect = if clearing { Rect { pos: dvec2(0.0, 0.0), size } } else { Rect { pos: dvec2(0.0, ring.head as f64), size: dvec2(size.x, 1.0) } };
        self.draw_waterfall_row.draw_abs(cx, rect);
        cx.end_pass_sized_turtle();
        ring.list.end(cx);
        cx.end_pass(&ring.pass);
        if ready {
            if clearing { ring.clear = false; ring.head = 0; } else { ring.head = (ring.head + 1) % ring.pixels.1; self.waterfall_pending = false; }
            ring.recorded = Recorded::now(cx, !clearing);
        }
    }

    /// The plot's device size as last drawn, or None before its first draw.
    pub fn plot_px(&self) -> Option<(u32, u32)> {
        self.pixels
    }

    /// Upload only changed counts, returning the previous buffer without copying.
    pub fn push_frame(&mut self, cx: &mut Cx, mut hits: Hits) -> Pushed {
        if self.pixels != Some((hits.w, hits.h)) || hits.data.len() != hits.w as usize * hits.h as usize {
            return Pushed::WrongSize(hits);
        }
        if self.pending_hits {
            return if cx.drawing_id() >= self.accepted_at + 2 { Pushed::Hidden(hits) } else { Pushed::Busy(hits) };
        }
        // The texture keeps the hits a recording read until it has painted.
        let reading = reading_hits(self.pictures.as_ref().and_then(|pictures| pictures.unpainted(cx)))
            .max(reading_hits(self.waterfall.as_ref().and_then(|ring| unpainted(cx, &ring.pass, ring.recorded))));
        if let Some(drawing) = reading {
            return if cx.drawing_id() >= drawing + 2 { Pushed::Hidden(hits) } else { Pushed::Busy(hits) };
        }
        let Some(texture) = &self.hits_texture else { return Pushed::WrongSize(hits) };
        let data = texture.take_vec_f32(cx);
        let old = self.uploaded.take().map(|mut old| {
            old.data = data;
            old
        });
        let rect = upload_rect(hits.dirty);
        self.uploaded_bytes = rect.size.width * rect.size.height * std::mem::size_of::<f32>();
        texture.put_back_vec_f32(cx, std::mem::take(&mut hits.data), Some(rect));
        self.waterfall_pending = self.waterfall_on && hits.waveforms > 0;
        if hits.waveforms > 0 { self.held = hits.held; self.below = hits.below; }
        self.uploaded = Some(hits);
        self.pending_hits = true;
        self.accepted_at = cx.drawing_id();
        self.area.redraw(cx);
        Pushed::Taken(old)
    }

    /// Upload a live secondary frame using the primary's transposed dirty rectangle, returning
    /// its buffer for reuse. Pending uploads use the same busy/hidden gate as the primary.
    /// Only dirty CPU rows are copied; the main picture is untouched.
    pub fn set_ghost_hits(&mut self, cx: &mut Cx, mut hits: Hits) -> Pushed {
        if self.pixels != Some((hits.w, hits.h)) || hits.data.len() != hits.w as usize * hits.h as usize { return Pushed::WrongSize(hits); }
        self.ensure_twin(cx);
        let Some(twin) = &mut self.twin else { return Pushed::WrongSize(hits) };
        if twin.pending_hits { return if cx.drawing_id() >= twin.accepted_at + 2 { Pushed::Hidden(hits) } else { Pushed::Busy(hits) }; }
        if let Some(drawing) = reading_hits(twin.pictures.as_ref().and_then(|pictures| pictures.unpainted(cx))) {
            return if cx.drawing_id() >= drawing + 2 { Pushed::Hidden(hits) } else { Pushed::Busy(hits) };
        }
        let texture = twin.hits_texture.as_ref().unwrap();
        let mut data = texture.take_vec_f32(cx);
        let rect = upload_rect(hits.dirty);
        for c in hits.dirty.c0..hits.dirty.c1 {
            let start = c as usize * hits.h as usize + hits.dirty.r0 as usize;
            let end = c as usize * hits.h as usize + hits.dirty.r1 as usize;
            data[start..end].copy_from_slice(&hits.data[start..end]);
        }
        texture.put_back_vec_f32(cx, data, Some(rect));
        twin.uploaded_bytes = rect.size.width * rect.size.height * std::mem::size_of::<f32>();
        if hits.waveforms > 0 { twin.held = hits.held; twin.below = hits.below; }
        // The returned buffer takes the previous frame's column counts, so neither allocates.
        let mut partial = twin.uploaded.take().map(|old| old.partial).unwrap_or_default();
        std::mem::swap(&mut partial, &mut hits.partial);
        twin.uploaded = Some(Hits { w: hits.w, h: hits.h, data: Vec::new(), waveforms: hits.waveforms, partial, partial_done: hits.partial_done,
            generation: hits.generation, map: hits.map, mixed_maps: hits.mixed_maps, clipped: hits.clipped, held: hits.held, below: hits.below, dirty: hits.dirty });
        twin.pending_hits = true;
        twin.accepted_at = cx.drawing_id();
        self.area.redraw(cx);
        Pushed::Taken(Some(hits))
    }

    pub(super) fn enable_twin(&mut self) { self.twin_active = true; self.freeze_pending = false; }

    fn ensure_twin(&mut self, cx: &mut Cx) {
        self.enable_twin();
        if self.twin.is_some() { return; }
        let Some((w, h)) = self.pixels.filter(|&(w, h)| w > 0 && h > 0) else { return };
        let mut hits = Hits::new(w, h);
        let texture = Texture::new_with_format(cx, TextureFormat::VecRf32 {
            width: h as usize, height: w as usize, data: Some(std::mem::take(&mut hits.data)), updated: TextureUpdated::Full,
        });
        self.twin = Some(LiveGhost { pictures: Some(Pictures::new(cx, (w, h))), hits_texture: Some(texture),
            blend_texture: Some(blend_texture(cx, w)), uploaded: Some(hits), clear_pending: true, ..LiveGhost::default() });
    }

    /// Exchanges both retained live pictures and queued frames. Feeds keep their original
    /// identities; the host routes subsequent frames to the newly selected primary.
    pub fn swap_twin(&mut self, cx: &mut Cx) {
        self.ensure_twin(cx);
        if let Some(channel) = &mut self.twin_channel { std::mem::swap(&mut self.channel, channel); }
        if let Some(mut twin) = self.twin.take() {
            twin.exchange(self);
            self.twin = Some(twin);
            self.waterfall_pending = self.waterfall_on && self.pending_hits;
            if let Some(ring) = &mut self.waterfall { ring.clear = true; }
            self.area.redraw(cx);
        }
    }

    /// Removes the live secondary; the ordinary freeze ghost is available again.
    pub fn clear_twin(&mut self, cx: &mut Cx) {
        if self.twin_active { self.area.redraw(cx); }
        self.twin_active = false;
        if let Some(twin) = self.twin.take() {
            if let Some(pictures) = twin.pictures { for pass in &pictures.passes { pass.withdraw(cx); } }
            self.area.redraw(cx);
        }
    }

    /// CPU-backed secondary upload texture, transposed like the primary hits. Its texels outside
    /// the latest dirty rectangle are unspecified; accumulation masks those texels out.
    pub fn ghost_hits_texture(&self) -> Option<&Texture> { self.twin.as_ref().and_then(|twin| twin.hits_texture.as_ref()) }

    /// Bytes in the most recently accepted secondary dirty rectangle.
    pub fn ghost_uploaded_bytes(&self) -> usize { self.twin.as_ref().map_or(0, |twin| twin.uploaded_bytes) }

    /// Bytes in the dirty rectangle of the last accepted frame.
    pub fn uploaded_bytes(&self) -> usize {
        self.uploaded_bytes
    }

    /// The current persistence picture, available after the first nonempty plot draw.
    pub fn picture(&self) -> Option<&Texture> {
        self.pictures.as_ref().map(|pictures| &pictures.textures[pictures.current])
    }

    /// Keeps the current picture as a ghost on the next draw, then clears live persistence.
    /// Discards already queued hits; later submissions start the new picture.
    pub fn freeze_ghost(&mut self, cx: &mut Cx) {
        if self.twin_active || self.freeze_pending || self.pictures.is_none() {
            return;
        }
        self.freeze_pending = true;
        self.pending_hits = false;
        self.waterfall_pending = false;
        self.area.redraw(cx);
    }

    /// Chooses whether the ghost lies over the live picture.
    pub fn set_ghost_on_top(&mut self, cx: &mut Cx, on_top: bool) {
        self.ghost_top = on_top;
        self.area.redraw(cx);
    }

    /// Hides the ghost and cancels any pending freeze, keeping its texture for reuse.
    pub fn clear_ghost(&mut self, cx: &mut Cx) {
        self.ghost_on = false;
        self.freeze_pending = false;
        self.area.redraw(cx);
    }

    /// Whether a frozen picture or a live secondary with data is visible.
    pub fn has_ghost(&self) -> bool {
        if self.twin_active { self.twin.as_ref().is_some_and(|twin| twin.picture_has_data) } else { self.ghost_on }
    }

    /// The live secondary picture, or the frozen texture retained for reuse while hidden.
    pub fn ghost_picture(&self) -> Option<&Texture> {
        if self.twin_active { self.twin.as_ref().and_then(|twin| twin.pictures.as_ref()).map(|p| &p.textures[p.current]) }
        else { self.ghost.as_ref().map(|ghost| &ghost.texture) }
    }

    /// Changes persistence and grading without discarding the kept image.
    pub fn set_look(&mut self, cx: &mut Cx, mut look: PhosphorLook) {
        look.bloom = look.bloom.clamp(0.0, 1.0);
        self.look = look;
        self.area.redraw(cx);
    }

    /// Restarts the persistence clock of a view that was not drawn for a while, so the time it
    /// spent off screen is not decay: the kept picture shows as it was left and fades from there.
    pub fn resume_clock(&mut self) {
        self.last_draw = None;
        if let Some(twin) = &mut self.twin { twin.last_draw = None; }
    }

    /// Whether the live picture holds any waveform.
    pub fn picture_has_data(&self) -> bool { self.picture_has_data }

    /// Whether the picture on screen holds a waveform: the step that put it there has painted.
    pub fn picture_shown(&self, cx: &Cx) -> bool {
        self.picture_has_data && self.pictures.as_ref().is_none_or(|pictures| pictures.unpainted(cx).is_none())
    }

    /// A view without a picture yet that must be drawn to get one: its first draw sizes it, and
    /// later draws build the picture from a raster waiting for it.
    pub fn wants_first_picture(&self) -> bool { !self.picture_has_data && (self.pixels.is_none() || self.pending_hits) }

    /// Clears the next accumulation pass and discards any queued frame.
    pub fn clear(&mut self, cx: &mut Cx) {
        if let Some(ring) = &mut self.waterfall { ring.clear = true; }
        self.clear_picture(cx);
    }

    pub fn clear_picture(&mut self, cx: &mut Cx) {
        self.clear_layer(cx, false);
        self.clear_layer(cx, true);
    }

    /// Replace one source's still without erasing the other source or its queued upload.
    pub(super) fn clear_layer(&mut self, cx: &mut Cx, secondary: bool) {
        if secondary {
            if let Some(twin) = &mut self.twin {
                twin.picture_map = None; twin.held = None; twin.below = None; twin.stretch = None; twin.clear_pending = true;
                twin.pending_hits = false; twin.persistence.clear(); twin.last_draw = None; twin.picture_has_data = false;
            }
            self.area.redraw(cx);
            return;
        }
        self.picture_has_data = false;
        self.picture_map = None;
        self.held = None;
        self.below = None;
        self.stretch = None;
        self.waterfall_pending = false;
        self.clear_pending = true;
        self.pending_hits = false;
        self.persistence.clear();
        self.last_draw = None;
        self.area.redraw(cx);
    }

    /// Uses a fixed frame interval for capture; None restores the monotonic clock.
    pub fn set_frame_interval(&mut self, dt: Option<f64>) {
        self.frame_interval = dt.filter(|dt| dt.is_finite() && *dt > 0.0);
        self.last_draw = None;
    }

    fn size_pictures(&mut self, cx: &mut Cx, pixels: (u32, u32)) {
        if self.pixels == Some(pixels) {
            return;
        }
        let twin_active = self.twin_active;
        self.clear_twin(cx);
        self.twin_active = twin_active;
        self.picture_has_data = false;
        self.pixels = Some(pixels);
        self.picture_map = None;
        self.stretch = None;
        if let Some(ring) = &mut self.waterfall { ring.resize(cx, pixels); }
        self.waterfall_pending = false;
        self.pending_hits = false;
        self.clear_pending = true;
        self.persistence.clear();
        self.uploaded = None;
        self.uploaded_bytes = 0;
        self.last_draw = None;
        if pixels.0 == 0 || pixels.1 == 0 {
            self.pictures = None;
            self.glow = None;
            self.hits_texture = None;
            self.blend_texture = None;
            return;
        }
        self.blend_texture = Some(blend_texture(cx, pixels.0));
        let mut hits = Hits::new(pixels.0, pixels.1);
        self.hits_texture = Some(Texture::new_with_format(
            cx,
            TextureFormat::VecRf32 {
                width: pixels.1 as usize,
                height: pixels.0 as usize,
                data: Some(std::mem::take(&mut hits.data)),
                updated: TextureUpdated::Full,
            },
        ));
        self.uploaded = Some(hits);
        self.pictures = Some(Pictures::new(cx, pixels));
        self.glow = Some(Glow::new(cx, pixels));
    }

    /// Uploads `blends`, one per column, for an accumulation that reads them. O(columns).
    fn upload_blends(&mut self, cx: &mut Cx) {
        let Some(texture) = &self.blend_texture else { return };
        let mut data = texture.take_vec_f32(cx);
        let w = self.blends.len();
        data.resize(2 * w, 0.0);
        for (c, blend) in self.blends.iter().enumerate() {
            data[c] = blend.keep;
            data[w + c] = blend.scale;
        }
        texture.put_back_vec_f32(cx, data, None);
    }

    /// One accumulation pass: `blend` for every column, or each column's from `blends` (None).
    fn accumulate(&mut self, cx: &mut Cx2d, blend: Option<Blend>, dirty: Dirty) -> bool {
        let ready = self.draw_accumulate.draw_vars.draw_shader_id.is_some_and(|id| cx.draw_shader_ready(id, false));
        let pictures = self.pictures.as_mut().unwrap();
        let next = 1 - pictures.current;
        let pass = &pictures.passes[next];
        let (w, h) = self.pixels.unwrap();
        let size = dvec2(w as f64, h as f64);
        cx.make_child_pass(pass);
        cx.begin_pass(pass, Some(1.0));
        pictures.lists[next].begin_always(cx);
        cx.begin_root_turtle(size, Layout::flow_overlay());
        let vars = &mut self.draw_accumulate.draw_vars;
        vars.set_texture(0, &pictures.textures[pictures.current]);
        vars.set_texture(1, self.hits_texture.as_ref().unwrap());
        vars.set_texture(2, self.blend_texture.as_ref().unwrap());
        let reads_hits = blend.is_none_or(|blend| blend.scale != 0.0);
        let columns = blend.is_none();
        let blend = blend.unwrap_or(Blend { keep: 0.0, scale: 0.0 });
        vars.set_uniform(cx, id!(keep), &[blend.keep]);
        vars.set_uniform(cx, id!(scale), &[blend.scale]);
        vars.set_uniform(cx, id!(columns), &[if columns { 1.0 } else { 0.0 }]);
        vars.set_uniform(cx, id!(rect), &[dirty.c0 as f32, dirty.r0 as f32, dirty.c1 as f32, dirty.r1 as f32]);
        self.draw_accumulate.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size });
        cx.end_pass_sized_turtle();
        pictures.lists[next].end(cx);
        cx.end_pass(pass);
        if ready {
            pictures.current = next;
            pictures.recorded = Recorded::now(cx, reads_hits);
        }
        ready
    }

    /// Opens the glow chain around this frame's accumulation: the vertical blur (sampled by the grade) depends
    /// on the horizontal one, that on the downsample, and that on the accumulation pass recorded inside it.
    fn begin_glow(&mut self, cx: &mut Cx2d) {
        let glow = self.glow.as_mut().unwrap();
        for index in [2, 1, 0] {
            cx.make_child_pass(&glow.passes[index]);
            cx.begin_pass(&glow.passes[index], Some(1.0));
            glow.lists[index].begin_always(cx);
            cx.begin_root_turtle(glow.size, Layout::flow_overlay());
        }
    }

    /// Draws the downsample from the current picture and closes its pass, then each blur from the one before.
    fn end_glow(&mut self, cx: &mut Cx2d) {
        let ready = self.draw_glow.draw_vars.draw_shader_id.is_some_and(|id| cx.draw_shader_ready(id, false));
        let glow = self.glow.as_mut().unwrap();
        let pictures = self.pictures.as_ref().unwrap();
        let rect = Rect { pos: dvec2(0.0, 0.0), size: glow.size };
        for (index, source) in [(0, &pictures.textures[pictures.current]), (1, &glow.textures[0]), (2, &glow.textures[1])] {
            let vars = &mut self.draw_glow.draw_vars;
            vars.set_texture(0, source);
            vars.set_uniform(cx, id!(mode), &[index as f32]);
            vars.set_uniform(cx, id!(gain), &[self.look.gain]);
            self.draw_glow.draw_abs(cx, rect);
            cx.end_pass_sized_turtle();
            glow.lists[index].end(cx);
            cx.end_pass(&glow.passes[index]);
        }
        glow.ready |= ready;
    }

    fn check_pending_map(&mut self) {
        // Old-layout hits can still be queued, and a value retune leaves open strokes on their
        // original map. Keep the old texture until a homogeneous, current-map frame arrives.
        if self.pending_hits && self.target_map.is_some() && self.uploaded.as_ref().is_some_and(|hits|
            hits.mixed_maps || hits.map.zip(self.target_map).is_some_and(|(from, to)| !(Stretch { from, to }).matches())
                || (self.stretch.is_some() && !hits.swept())) {
            self.pending_hits = false;
            self.waterfall_pending = false;
        }
    }

    fn update_picture(&mut self, cx: &mut Cx2d, dt: f64) {
        let Some(pictures) = &self.pictures else { return };
        let waiting = pictures.unpainted(cx).is_some();
        if self.clear_pending && !self.freeze_pending {
            // A clear replaces the step still waiting for its paint.
            if waiting { pictures.passes[pictures.current].withdraw(cx); }
            if self.accumulate(cx, Some(Blend { keep: 0.0, scale: 0.0 }), Dirty::default()) {
                self.clear_pending = false;
                self.picture_has_data = false;
            }
        } else if waiting {
            // The step an earlier drawing recorded is the picture; the next one follows its paint, and the time
            // until then still decays it.
            attach_unpainted(cx, &pictures.passes[pictures.current]);
            self.persistence.idle(dt);
        } else if !self.freeze_pending && !self.waterfall_pending && (self.stretch.is_none() || self.pending_hits) {
            let frame = self.uploaded.as_ref().filter(|_| self.pending_hits);
            let swept = frame.is_some_and(Hits::swept);
            let dirty = frame.map_or(Dirty::default(), |hits| hits.dirty);
            let map = frame.filter(|_| swept).and_then(|hits| hits.map);
            let (whole, partial) = frame.map_or((0, &[][..]), |hits| (hits.whole(), &hits.partial[..]));
            let columns = self.pixels.map_or(0, |(w, _)| w as usize);
            // The blend is kept only once its accumulation is recorded: an O(columns) copy.
            self.trial.clone_from(&self.persistence);
            if map.is_some() && map != self.picture_map { self.trial.clear(); }
            if self.trial.frame(columns, whole, partial, dt, self.look.tau, &mut self.blends) {
                self.upload_blends(cx);
                if self.accumulate(cx, None, dirty) {
                    std::mem::swap(&mut self.persistence, &mut self.trial);
                    if swept { self.picture_has_data = true; self.picture_map = map; self.refresh_stretch(); }
                    self.pending_hits = false;
                } else {
                    // Shader warm-up retains both the queued frame and its elapsed decay.
                    self.persistence.idle(dt);
                }
            } else {
                std::mem::swap(&mut self.persistence, &mut self.trial);
                self.pending_hits = false;
            }
        }
    }

    fn copy_ghost(&mut self, cx: &mut Cx2d) -> bool {
        let ready = self.draw_accumulate.draw_vars.draw_shader_id.is_some_and(|id| cx.draw_shader_ready(id, false));
        let pixels = self.pixels.unwrap();
        if self.ghost.as_ref().map(|ghost| ghost.pixels) != Some(pixels) {
            self.ghost = Some(Ghost::new(cx, pixels));
        }
        let ghost = self.ghost.as_mut().unwrap();
        let pictures = self.pictures.as_ref().unwrap();
        let size = dvec2(pixels.0 as f64, pixels.1 as f64);
        cx.make_child_pass(&ghost.pass);
        cx.begin_pass(&ghost.pass, Some(1.0));
        ghost.list.begin_always(cx);
        cx.begin_root_turtle(size, Layout::flow_overlay());
        let vars = &mut self.draw_accumulate.draw_vars;
        vars.set_texture(0, &pictures.textures[pictures.current]);
        vars.set_texture(1, self.hits_texture.as_ref().unwrap());
        vars.set_texture(2, self.blend_texture.as_ref().unwrap());
        vars.set_uniform(cx, id!(keep), &[1.0]);
        vars.set_uniform(cx, id!(scale), &[0.0]);
        vars.set_uniform(cx, id!(columns), &[0.0]);
        vars.set_uniform(cx, id!(rect), &[0.0; 4]);
        self.draw_accumulate.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size });
        cx.end_pass_sized_turtle();
        ghost.list.end(cx);
        cx.end_pass(&ghost.pass);
        ready
    }
}

impl WidgetNode for PhosphorView {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
    fn children(&self, _visit: &mut dyn FnMut(LiveId, WidgetRef)) {}
    fn find_widgets_from_point(&self, _cx: &Cx, _p: DVec2, _found: &mut dyn FnMut(&WidgetRef)) {}
}

impl Widget for PhosphorView {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        let dpi = cx.current_dpi_factor();
        let size = self.raster_size.unwrap_or(rect.size);
        let pixels = ((size.x * dpi).ceil().max(0.0) as u32, (size.y * dpi).ceil().max(0.0) as u32);
        // An old recording must never replay against newer hits or a swapped picture. One an earlier
        // drawing made that has not painted yet (this frame drawn again) is kept and attached again below.
        if let Some(pictures) = &self.pictures { pictures.withdraw_painted(cx); }
        if let Some(ghost) = &self.ghost {
            if unpainted(cx, &ghost.pass, ghost.recorded).is_some() { attach_unpainted(cx, &ghost.pass); } else { ghost.pass.withdraw(cx); }
        }
        if let Some(pictures) = self.twin.as_ref().and_then(|twin| twin.pictures.as_ref()) { pictures.withdraw_painted(cx); }
        if let Some(glow) = &self.glow {
            for pass in &glow.passes {
                pass.withdraw(cx);
            }
        }
        if let Some(ring) = &self.waterfall {
            if unpainted(cx, &ring.pass, ring.recorded).is_none() { ring.pass.withdraw(cx); }
        }
        // Copy before a resize can discard the picture, and hold it during shader warm-up.
        if self.freeze_pending && self.copy_ghost(cx) {
            if let Some(ghost) = &mut self.ghost { ghost.recorded = Recorded::now(cx, false); }
            self.freeze_pending = false;
            self.ghost_on = true;
            self.clear_pending = true;
            self.persistence.clear();
            self.last_draw = None;
        }
        if !self.freeze_pending {
            self.size_pictures(cx, pixels);
        }
        if pixels.0 == 0 || pixels.1 == 0 {
            if self.freeze_pending && !cx.next_frame_is_pending(self.next_frame) {
                self.next_frame = cx.new_next_frame();
            }
            return DrawStep::done();
        }
        self.check_pending_map();
        self.waterfall_row(cx);
        let now = Cx::monotonic_now();
        let dt = self
            .frame_interval
            .unwrap_or_else(|| self.last_draw.map_or(1.0 / 60.0, |last| (now - last).max(0.0)));
        self.last_draw = Some(now);
        let glow_on = !self.look.light && self.look.bloom > 0.0 && !self.freeze_pending && self.glow.is_some();
        // While waiting, one grading quad samples the old picture and its already cached glow.
        let refresh_glow = glow_on && (self.stretch.is_none() || self.pending_hits || self.pictures.as_ref().is_some_and(|pictures| pictures.unpainted(cx).is_some()));
        if refresh_glow { self.begin_glow(cx); }
        self.update_picture(cx, dt);
        if let Some(mut twin) = self.twin.take() {
            twin.exchange(self);
            self.last_draw = Some(now);
            self.refresh_stretch();
            self.check_pending_map();
            let waterfall_pending = std::mem::replace(&mut self.waterfall_pending, false);
            self.update_picture(cx, dt);
            self.waterfall_pending = waterfall_pending;
            twin.exchange(self);
            self.twin = Some(twin);
        }

        if refresh_glow { self.end_glow(cx); }

        let ghost_on = self.has_ghost();
        let ghost_stretch = self.twin.as_ref().and_then(|twin| twin.stretch).map_or([1.0, 1.0, 0.0, 0.0], Stretch::uniform);
        let ghost = self.ghost_picture().cloned();
        let vars = &mut self.draw_grade.draw_vars;
        let pictures = self.pictures.as_ref().unwrap();
        let picture = &pictures.textures[pictures.current];
        let glow = self.glow.as_ref().filter(|glow| glow_on && glow.ready);
        vars.set_texture(0, picture);
        vars.set_texture(1, if ghost_on { ghost.as_ref().unwrap() } else { picture });
        vars.set_texture(2, glow.map_or(picture, |glow| &glow.textures[2]));
        vars.set_uniform(cx, id!(stretch), &self.stretch.map_or([1.0, 1.0, 0.0, 0.0], Stretch::uniform));
        vars.set_uniform(cx, id!(ghost_on), &[if ghost_on { 1.0 } else { 0.0 }]);
        vars.set_uniform(cx, id!(ghost_stretch), &ghost_stretch);
        vars.set_uniform(cx, id!(ghost_top), &[if self.ghost_top { 1.0 } else { 0.0 }]);
        vars.set_uniform(cx, id!(twin_channel), &if self.twin_active { self.twin_channel.unwrap_or([0.0; 4]) } else { [0.0; 4] });
        vars.set_uniform(cx, id!(twin_weight), &[if self.twin_preview { 0.5 } else { 1.0 }]);
        vars.set_uniform(cx, id!(glow_on), &[if glow.is_some() { 1.0 } else { 0.0 }]);
        vars.set_uniform(cx, id!(gain), &[self.look.gain]);
        vars.set_uniform(cx, id!(bloom), &[self.look.bloom]);
        vars.set_uniform(cx, id!(live_light), &[if self.look.light { 1.0 } else { 0.0 }]);
        vars.set_uniform(cx, id!(screen), &[self.screen.x, self.screen.y, self.screen.z]);
        vars.set_uniform(cx, id!(channel), &self.channel);
        self.draw_grade.draw_abs(cx, rect);

        if (self.pending_hits || self.clear_pending || self.freeze_pending || self.twin.as_ref().is_some_and(|twin| twin.pending_hits || twin.clear_pending) || self.waterfall.as_ref().is_some_and(|ring| ring.clear)) && !cx.next_frame_is_pending(self.next_frame) {
            self.next_frame = cx.new_next_frame();
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.next_frame.is_event(event).is_some() {
            self.area.redraw(cx);
        }
    }
}

/// A picture's per-column blends: `w` keeps, then `w` scales.
fn blend_texture(cx: &mut Cx, w: u32) -> Texture {
    Texture::new_with_format(cx, TextureFormat::VecRf32 {
        width: w as usize, height: 2, data: Some(vec![0.0; 2 * w as usize]), updated: TextureUpdated::Full,
    })
}

pub(crate) fn upload_rect(dirty: Dirty) -> RectUsize {
    let dirty = if dirty.is_empty() { Dirty::default() } else { dirty };
    RectUsize {
        origin: PointUsize { x: dirty.r0 as usize, y: dirty.c0 as usize },
        size: SizeUsize { width: (dirty.r1 - dirty.r0) as usize, height: (dirty.c1 - dirty.c0) as usize },
    }
}
