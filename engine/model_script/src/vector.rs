//! VECTOR: a small, fast, deterministic 2D canvas for generated textures —
//! shop fascias, posters, street and venue signs, liveries.
//!
//! Everything is drawn as anti-aliased coverage into a linear-light RGBA
//! canvas plus an emissive mask (what glows after dark):
//! - text from real font outlines (IBM Plex Sans semibold, shipped with
//!   Makepad's widgets), filled by the same coverage rasteriser Makepad's
//!   glyph atlas uses, with optional outline and letter spacing;
//! - rounded rectangles, circles and rings, stripes, linear gradients;
//! - a handful of simple logo marks (leaf, star, drop, wave, bolt, sun).
//!
//! Pure functions of their inputs: the same calls give the same texels on
//! every device, so a generated city looks the same to every peer. The
//! text is whatever the caller asks for — use invented names, never real
//! brands.

use makepad_font_core::{font_face::FontFace, geom::Size, image::{Image, R}};
use makepad_model::msdf_text::{shape_text, text_face};

/// An sRGB colour with alpha (0..1).
pub type Rgba = [f32; 4];

fn to_lin(c: f32) -> f32 { if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } }
fn to_srgb(c: f32) -> f32 { let c = c.clamp(0.0, 1.0); if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 } }
fn lin4(c: Rgba) -> Rgba { [to_lin(c[0]), to_lin(c[1]), to_lin(c[2]), c[3]] }

/// Linear → 8-bit sRGB through a table (a canvas is a million texels; a
/// `powf` per channel was most of its cost).
fn srgb8(c: f32) -> u32 {
    static T: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| (0..4096).map(|i| (to_srgb(i as f32 / 4095.0) * 255.0 + 0.5) as u8).collect());
    t[(c.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize] as u32
}

/// Horizontal text alignment about the given x.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align { Left, Center, Right }

/// A canvas: linear-light RGBA plus an emissive mask, top row first.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    rgba: Vec<[f32; 4]>,
    glow: Vec<f32>,
}

impl Canvas {
    pub fn new(width: usize, height: usize, background: Rgba) -> Canvas {
        let bg = [to_lin(background[0]), to_lin(background[1]), to_lin(background[2]), background[3]];
        Canvas { width, height, rgba: vec![bg; width * height], glow: vec![0.0; width * height] }
    }

    /// Blend LINEAR `color` over pixel (x, y) with coverage `a`; `glow`
    /// (0..1) marks it as lit after dark.
    fn blend(&mut self, x: usize, y: usize, color: Rgba, a: f32, glow: f32) {
        let a = (a * color[3]).clamp(0.0, 1.0);
        if a <= 0.0 { return; }
        let i = y * self.width + x;
        let d = &mut self.rgba[i];
        for k in 0..3 { d[k] += (color[k] - d[k]) * a; }
        d[3] += (1.0 - d[3]) * a;
        self.glow[i] += (glow - self.glow[i]) * a;
    }

    /// Fill by a coverage function of the pixel centre over a box (bounded).
    fn fill_by(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Rgba, glow: f32, cover: impl Fn(f32, f32) -> f32) {
        let (xa, ya) = ((x0.floor().max(0.0)) as usize, (y0.floor().max(0.0)) as usize);
        let (xb, yb) = ((x1.ceil().max(0.0) as usize).min(self.width), (y1.ceil().max(0.0) as usize).min(self.height));
        let color = lin4(color);
        for y in ya..yb {
            for x in xa..xb {
                let c = cover(x as f32 + 0.5, y as f32 + 0.5);
                if c > 0.0 { self.blend(x, y, color, c, glow); }
            }
        }
    }

    /// A rectangle with rounded corners (radius in pixels).
    pub fn rounded_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, color: Rgba, glow: f32) {
        let (cx, cy, hx, hy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5, (x1 - x0) * 0.5, (y1 - y0) * 0.5);
        let r = radius.min(hx).min(hy).max(0.0);
        self.fill_by(x0 - 1.0, y0 - 1.0, x1 + 1.0, y1 + 1.0, color, glow, |x, y| {
            let qx = ((x - cx).abs() - (hx - r)).max(0.0);
            let qy = ((y - cy).abs() - (hy - r)).max(0.0);
            let outside = (qx * qx + qy * qy).sqrt() - r + ((x - cx).abs() - hx).max((y - cy).abs() - hy).min(0.0);
            (0.5 - outside).clamp(0.0, 1.0)
        });
    }

    /// A rounded-rect frame of `width` pixels.
    pub fn rounded_frame(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, width: f32, color: Rgba, glow: f32) {
        let (cx, cy, hx, hy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5, (x1 - x0) * 0.5, (y1 - y0) * 0.5);
        let r = radius.min(hx).min(hy).max(0.0);
        let cover = move |x: f32, y: f32| {
            let qx = ((x - cx).abs() - (hx - r)).max(0.0);
            let qy = ((y - cy).abs() - (hy - r)).max(0.0);
            let d = (qx * qx + qy * qy).sqrt() - r + ((x - cx).abs() - hx).max((y - cy).abs() - hy).min(0.0);
            (0.5 - (d + width * 0.5).abs() + width * 0.5).clamp(0.0, 1.0)
        };
        // Only the band the frame can touch: four strips, not the middle.
        let b = r + width + 2.0;
        self.fill_by(x0 - 1.0, y0 - 1.0, x1 + 1.0, y0 + b, color, glow, cover);
        self.fill_by(x0 - 1.0, y1 - b, x1 + 1.0, y1 + 1.0, color, glow, cover);
        self.fill_by(x0 - 1.0, y0 + b, x0 + b, y1 - b, color, glow, cover);
        self.fill_by(x1 - b, y0 + b, x1 + 1.0, y1 - b, color, glow, cover);
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgba, glow: f32) {
        self.fill_by(cx - r - 1.0, cy - r - 1.0, cx + r + 1.0, cy + r + 1.0, color, glow, |x, y| (r - ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0));
    }

    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, color: Rgba, glow: f32) {
        self.fill_by(cx - r - width, cy - r - width, cx + r + width, cy + r + width, color, glow, |x, y| {
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            (width * 0.5 - (d - r).abs() + 0.5).clamp(0.0, 1.0)
        });
    }

    /// Diagonal or straight stripes over a box: `period` pixels, `duty` 0..1,
    /// `slope` = x shift per row (0 = vertical).
    pub fn stripes(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, period: f32, duty: f32, slope: f32, color: Rgba, glow: f32) {
        self.fill_by(x0, y0, x1, y1, color, glow, |x, y| {
            let t = ((x + y * slope) / period).rem_euclid(1.0);
            let e = (duty - t).min(t) * period;
            (e + 0.5).clamp(0.0, 1.0)
        });
    }

    /// A vertical (or horizontal) two-colour gradient over a box.
    pub fn gradient(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, top: Rgba, bottom: Rgba, vertical: bool) {
        let (top, bottom) = (lin4(top), lin4(bottom));
        for y in (y0.max(0.0) as usize)..(y1.min(self.height as f32) as usize) {
            for x in (x0.max(0.0) as usize)..(x1.min(self.width as f32) as usize) {
                let t = if vertical { (y as f32 - y0) / (y1 - y0).max(1.0) } else { (x as f32 - x0) / (x1 - x0).max(1.0) };
                let c = [top[0] + (bottom[0] - top[0]) * t, top[1] + (bottom[1] - top[1]) * t, top[2] + (bottom[2] - top[2]) * t, top[3] + (bottom[3] - top[3]) * t];
                self.blend(x, y, c, 1.0, 0.0);
            }
        }
    }

    /// Width at the canvas's historical size convention (1.35 em per px).
    pub fn measure(text: &str, px: f32, spacing: f32) -> f32 {
        Self::measure_with_face(&text_face("plex-semibold", None, None).expect("embedded text face"), text, px, spacing)
    }

    pub fn measure_with_face(face: &FontFace, text: &str, px: f32, spacing: f32) -> f32 {
        let scale = px * 1.35;
        if scale <= 0.0 || !scale.is_finite() { return 0.0; }
        let line = shape_text(face, text, spacing / scale);
        if text.is_empty() { 0.0 } else { line.advance * scale - spacing }
    }

    /// Default face, for existing canvas callers. Text layers can select the
    /// same embedded face and variations as an MSDF material.
    pub fn text(&mut self, text: &str, x: f32, y: f32, px: f32, align: Align, spacing: f32, color: Rgba, glow: f32, outline: Option<(Rgba, f32)>) -> f32 {
        self.text_with_face(&text_face("plex-semibold", None, None).expect("embedded text face"), text, x, y, px, align, spacing, color, glow, outline)
    }

    pub fn text_with_face(&mut self, face: &FontFace, text: &str, x: f32, y: f32, px: f32, align: Align, spacing: f32, color: Rgba, glow: f32, outline: Option<(Rgba, f32)>) -> f32 {
        let scale = px * 1.35;
        if scale <= 0.0 || !scale.is_finite() { return 0.0; }
        let line = shape_text(face, text, spacing / scale);
        let w = if text.is_empty() { 0.0 } else { line.advance * scale - spacing };
        let x0 = match align { Align::Left => x, Align::Center => x - w * 0.5, Align::Right => x - w };
        // Coverage is rasterised once per glyph. An outline reuses the same
        // coverage at its eight offsets instead of re-parsing/rasterising it.
        let glyphs: Vec<_> = line.glyphs.iter().map(|g| {
            let b = g.outline.bounds_in_ems();
            let size = Size::new((b.size.width * scale).ceil() as usize, (b.size.height * scale).ceil() as usize);
            let mut coverage = Image::<R>::new(size);
            g.outline.rasterize(scale, &mut coverage.subimage_mut(size.into()));
            let gx = x0 + (g.x + b.origin.x) * scale;
            let gy = y - (g.y + b.origin.y) * scale - size.height as f32;
            (gx, gy, coverage)
        }).collect();
        let mut stamp = |dx: f32, dy: f32, color: Rgba, glow: f32| {
            let color = lin4(color);
            for (gx, gy, coverage) in &glyphs {
                let size = coverage.size();
                let (x0, y0) = ((gx + dx).round() as i64, (gy + dy).round() as i64);
                for cy in 0..size.height {
                    let py = y0 + cy as i64;
                    if py < 0 || py >= self.height as i64 { continue; }
                    for cx in 0..size.width {
                        let px = x0 + cx as i64;
                        if px < 0 || px >= self.width as i64 { continue; }
                        let a = coverage.as_pixels()[cy * size.width + cx].r() as f32 / 255.0;
                        if a > 0.0 { self.blend(px as usize, py as usize, color, a, glow); }
                    }
                }
            }
        };
        if let Some((oc, ow)) = outline {
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                stamp(a.cos() * ow, a.sin() * ow, oc, glow * 0.5);
            }
        }
        stamp(0.0, 0.0, color, glow);
        w
    }

    /// A simple logo mark centred at (cx, cy), `r` pixels across.
    pub fn mark(&mut self, kind: Mark, cx: f32, cy: f32, r: f32, color: Rgba, glow: f32) {
        match kind {
            Mark::Leaf => {
                // Two circle arcs meet in a leaf, a stem through it.
                self.fill_by(cx - r, cy - r, cx + r, cy + r, color, glow, |x, y| {
                    let (u, v) = ((x - cx) / r, (y - cy) / r);
                    let (a, b) = ((u + 0.45).powi(2) + (v + 0.45).powi(2), (u - 0.45).powi(2) + (v - 0.45).powi(2));
                    let inside = (0.9f32 - a.sqrt()).min(0.9 - b.sqrt());
                    (inside * r + 0.5).clamp(0.0, 1.0)
                });
            }
            Mark::Star => {
                self.fill_by(cx - r, cy - r, cx + r, cy + r, color, glow, |x, y| {
                    let (u, v) = (x - cx, y - cy);
                    let ang = v.atan2(u) + std::f32::consts::FRAC_PI_2;
                    let k = (ang / (std::f32::consts::TAU / 5.0)).rem_euclid(1.0);
                    let edge = r * (0.45 + 0.55 * (1.0 - (k - 0.5).abs() * 2.0).powf(2.2));
                    (edge - (u * u + v * v).sqrt() + 0.5).clamp(0.0, 1.0)
                });
            }
            Mark::Drop => {
                self.circle(cx, cy + r * 0.25, r * 0.6, color, glow);
                self.fill_by(cx - r, cy - r, cx + r, cy + r, color, glow, |x, y| {
                    let t = (y - (cy - r)) / (r * 1.25);
                    if !(0.0..=1.0).contains(&t) { return 0.0; }
                    let half = r * 0.6 * t.sqrt();
                    (half - (x - cx).abs() + 0.5).clamp(0.0, 1.0)
                });
            }
            Mark::Wave => {
                for k in 0..3 {
                    let yk = cy - r * 0.5 + k as f32 * r * 0.5;
                    self.fill_by(cx - r, yk - r * 0.3, cx + r, yk + r * 0.3, color, glow, |x, y| {
                        let c = yk + (x - cx).mul_add(3.0 / r, 0.0).sin() * r * 0.12;
                        (r * 0.1 - (y - c).abs() + 0.5).clamp(0.0, 1.0)
                    });
                }
            }
            Mark::Bolt => {
                self.fill_by(cx - r, cy - r, cx + r, cy + r, color, glow, |x, y| {
                    let (u, v) = ((x - cx) / r, (y - cy) / r);
                    // Two slanted bars offset at the middle.
                    let top = v < 0.05 && v > -0.95 && (u + v * 0.4 + 0.05).abs() < 0.22;
                    let bottom = v > -0.05 && v < 0.95 && (u + v * 0.4 - 0.18).abs() < 0.22;
                    if top || bottom { 1.0 } else { 0.0 }
                });
            }
            Mark::Sun => {
                self.circle(cx, cy, r * 0.5, color, glow);
                for k in 0..10 {
                    let a = k as f32 * std::f32::consts::TAU / 10.0;
                    self.circle(cx + a.cos() * r * 0.82, cy + a.sin() * r * 0.82, r * 0.11, color, glow);
                }
            }
        }
    }

    /// The canvas as ARGB texels (sRGB colour), the alpha channel written
    /// by `alpha(glow, cover)` — e.g. the city's lit-window convention.
    pub fn to_argb(&self, alpha: impl Fn(f32, f32) -> f32) -> Vec<u32> {
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        self.rgba.iter().zip(&self.glow).map(|(c, g)| (b(alpha(*g, c[3])) << 24) | (srgb8(c[0]) << 16) | (srgb8(c[1]) << 8) | srgb8(c[2])).collect()
    }

    /// Linear RGBA and the emissive mask, for callers compositing further.
    pub fn pixels(&self) -> (&[[f32; 4]], &[f32]) { (&self.rgba, &self.glow) }
}

/// Simple logo marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark { Leaf, Star, Drop, Wave, Bolt, Sun }
impl Mark {
    pub const ALL: [Mark; 6] = [Mark::Leaf, Mark::Star, Mark::Drop, Mark::Wave, Mark::Bolt, Mark::Sun];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_draws_real_glyphs_deterministically() {
        let draw = || {
            let mut c = Canvas::new(256, 64, [0.1, 0.1, 0.12, 1.0]);
            let w = c.text("HARBOR BAKERY", 128.0, 40.0, 18.0, Align::Center, 1.0, [1.0, 0.9, 0.7, 1.0], 1.0, None);
            (w, c.to_argb(|g, _| g))
        };
        let (w, a) = draw();
        let (_, b) = draw();
        assert_eq!(a, b, "the same calls give the same texels");
        assert!(w > 120.0 && w < 256.0, "{w}");
        // Ink landed, glowing, inside the text box only.
        let lit = a.iter().filter(|t| (**t >> 24) > 128).count();
        assert!(lit > 300, "{lit}");
        let row = |y: usize| a[y * 256..(y + 1) * 256].iter().filter(|t| (**t >> 24) > 128).count();
        assert_eq!(row(2), 0, "nothing above the capitals");
        assert!(row(32) > 20);
        // The mean luminance inside the letters is the ink's.
        assert!(a.iter().any(|t| ((*t >> 16) & 255) > 230));
    }

    #[test]
    fn shapes_cover_what_they_say() {
        let mut c = Canvas::new(64, 64, [0.0, 0.0, 0.0, 0.0]);
        c.rounded_rect(8.0, 8.0, 56.0, 56.0, 10.0, [1.0, 0.0, 0.0, 1.0], 0.0);
        let t = c.to_argb(|_, a| a);
        assert_eq!(t[32 * 64 + 32] >> 24, 255);
        assert_eq!(t[9 * 64 + 9] >> 24, 0, "the rounded corner is clear");
        assert_eq!(t[2 * 64 + 32] >> 24, 0);
        for m in Mark::ALL {
            let mut c = Canvas::new(48, 48, [0.0, 0.0, 0.0, 0.0]);
            c.mark(m, 24.0, 24.0, 18.0, [1.0, 1.0, 1.0, 1.0], 0.0);
            let n = c.to_argb(|_, a| a).iter().filter(|t| (**t >> 24) > 128).count();
            assert!(n > 60, "{m:?} drew {n}");
        }
    }
}
