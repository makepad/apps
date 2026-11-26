//! The draw primitives the library's cards and Story's timeline and viewer
//! share: the clip role colours, the block shape and the picture shader, a
//! source's loudness outline and its filmstrip.

use makepad_widgets::*;
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    set_type_default() do #(DrawEditBlock::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            let p = self.pos * self.rect_size
            if self.shape > 4.5 {
                // A dashed rounded outline (ghosts, provisional runs).
                sdf.box(1., 1., w - 2., h - 2., self.radius)
                sdf.fill_keep(self.fill)
                let along = p.x + p.y
                let dash = step(0.43, fract(along / 7.))
                sdf.stroke(vec4(self.edge.xyz, self.edge.w * (1. - dash)), 1.5)
                return sdf.result
            }
            if self.shape > 3.5 {
                // A diagonal hatch over the body.
                let hatch = 1. - smoothstep(0.5, 1.2, abs(fract((p.x + p.y) / 6.) - 0.5) * 6.)
                return vec4(self.edge.xyz * self.edge.w * hatch, self.edge.w * hatch)
            }
            if self.shape > 2.5 {
                // A circle (fade handles, dots).
                sdf.circle(w * 0.5, h * 0.5, min(w, h) * 0.5 - 1.)
                sdf.fill_keep(self.fill)
                sdf.stroke(self.edge, 1.)
                return sdf.result
            }
            if self.shape > 1.5 {
                // A pentagon pointing down (playhead handle, markers).
                sdf.move_to(0.5, 0.5)
                sdf.line_to(w - 0.5, 0.5)
                sdf.line_to(w - 0.5, h * 0.55)
                sdf.line_to(w * 0.5, h - 0.5)
                sdf.line_to(0.5, h * 0.55)
                sdf.close_path()
                sdf.fill(self.fill)
                return sdf.result
            }
            if self.shape > 0.5 {
                // A diamond (keys).
                sdf.move_to(w * 0.5, 0.5)
                sdf.line_to(w - 0.5, h * 0.5)
                sdf.line_to(w * 0.5, h - 0.5)
                sdf.line_to(0.5, h * 0.5)
                sdf.close_path()
                sdf.fill_keep(self.fill)
                sdf.stroke(self.edge, 1.)
                return sdf.result
            }
            sdf.box(0.5, 0.5, w - 1., h - 1., self.radius)
            sdf.fill_keep(self.fill)
            sdf.stroke(self.edge, self.stroke)
            return sdf.result
        }
    }

    set_type_default() do #(DrawEditPicture::script_shader(vm)){
        ..mod.draw.DrawQuad
        image: texture_2d(float)
        pixel: fn() {
            let c = self.image.sample(mix(self.uv.xy, self.uv.zw, self.pos))
            return vec4(c.xyz * self.opacity, self.opacity)
        }
    }
}

/// Clip role colours (design §2.1): hue, wash, strip, hover wash.
#[derive(Clone, Copy)]
pub struct CutRole {
    pub hue: u32,
    pub wash: u32,
    pub strip: u32,
    pub hover: u32,
}

impl CutRole {
    pub const VIDEO: Self = Self { hue: 0x4f8feaff, wash: 0x18273dff, strip: 0x2c4e7eff, hover: 0x1d314eff };
    pub const APP_RUN: Self = Self { hue: 0x7c83f5ff, wash: 0x21253fff, strip: 0x434884ff, hover: 0x2a2e52ff };
    pub const TITLE: Self = Self { hue: 0xb98af0ff, wash: 0x2d263eff, strip: 0x624c81ff, hover: 0x3b3050ff };
    pub const EFFECT: Self = Self { hue: 0xd05fd0ff, wash: 0x321d38ff, strip: 0x6d3671ff, hover: 0x412447ff };
    pub const TRANSITION: Self = Self { hue: 0xc2ccd8ff, wash: 0x2f333aff, strip: 0x666c75ff, hover: 0x3e4249ff };
    pub const AUDIO: Self = Self { hue: 0x2fb894ff, wash: 0x112f2cff, strip: 0x1c6253ff, hover: 0x143d36ff };
    pub const VOICE: Self = Self { hue: 0x3cc2dcff, wash: 0x14313aff, strip: 0x236877ff, hover: 0x18404bff };
    pub const CAPTION: Self = Self { hue: 0xd6dee6ff, wash: 0x33373cff, strip: 0x33373cff, hover: 0x43484dff };
    pub const STALE: Self = Self { hue: 0xf0b34dff, wash: 0x382e1eff, strip: 0x7d6030ff, hover: 0x382e1eff };
    pub const BROKEN: Self = Self { hue: 0xe5484dff, wash: 0x36191eff, strip: 0x782a30ff, hover: 0x36191eff };
    pub const MARKER: u32 = 0xf5c542ff;
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawEditBlock {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub fill: Vec4f,
    #[live]
    pub edge: Vec4f,
    #[live]
    pub stroke: f32,
    #[live]
    pub radius: f32,
    /// 0 a rounded box, 1 a diamond, 2 a pentagon, 3 a circle, 4 a hatch,
    /// 5 a dashed box.
    #[live]
    pub shape: f32,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawEditPicture {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub opacity: f32,
    /// The part of the texture drawn: `(x0, y0, x1, y1)` in fractions.
    #[live]
    pub uv: Vec4f,
}

/// A source's loudness outline: `per_second` peaks, each 0..255.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Peaks {
    pub per_second: f64,
    /// Linear peaks, 0..255.
    pub values: Vec<u8>,
    /// RMS per window, dB-coded (`code_level`): 60 dB over 0..255.
    pub rms: Vec<u8>,
}

impl Peaks {
    /// The peak over `[t0, t1)` source seconds, 0..1.
    pub fn over(&self, t0: f64, t1: f64) -> f64 {
        let (a, b) = self.window(t0, t1, self.values.len());
        self.values.get(a..b).and_then(|slice| slice.iter().max()).map_or(0.0, |peak| *peak as f64 / 255.0)
    }

    /// The loudest RMS over `[t0, t1)`, linear 0..1.
    pub fn rms_over(&self, t0: f64, t1: f64) -> f64 {
        let (a, b) = self.window(t0, t1, self.rms.len());
        self.rms.get(a..b).and_then(|slice| slice.iter().max()).map_or(0.0, |code| Self::level_of(*code))
    }

    fn window(&self, t0: f64, t1: f64, len: usize) -> (usize, usize) {
        let a = ((t0 * self.per_second).floor().max(0.0) as usize).min(len);
        let b = (((t1 * self.per_second).ceil().max(0.0) as usize).max(a + 1)).min(len);
        (a, b)
    }

    /// A linear level as a byte: −60..0 dBFS over 0..255 (0 is silence).
    pub fn code_level(level: f64) -> u8 {
        if level <= 1e-6 {
            return 0;
        }
        (((20.0 * level.log10() + 60.0) / 60.0).clamp(0.0, 1.0) * 255.0).round() as u8
    }

    pub fn level_of(code: u8) -> f64 {
        if code == 0 { 0.0 } else { 10f64.powf((code as f64 / 255.0 * 60.0 - 60.0) / 20.0) }
    }
}

/// A source's pictures across its length: `(source seconds, picture)`.
pub type Filmstrip = Arc<Vec<(f64, Texture)>>;

/// A level (0..1, linear) as a waveform height fraction: through dB,
/// −42 dBFS the floor (design review 2, B1).
pub fn wave_height(level: f64) -> f64 {
    if level <= 0.0 {
        return 0.0;
    }
    ((20.0 * level.log10() + 42.0) / 42.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn waveforms_are_drawn_in_decibels() {
        assert_eq!(super::wave_height(0.0), 0.0);
        assert_eq!(super::wave_height(1.0), 1.0);
        assert!((super::wave_height(10f64.powf(-21.0 / 20.0)) - 0.5).abs() < 1e-9);
        let code = super::Peaks::code_level(0.1);
        assert!((super::Peaks::level_of(code) - 0.1).abs() < 0.01);
        assert_eq!(super::Peaks::code_level(0.0), 0);
    }
}
