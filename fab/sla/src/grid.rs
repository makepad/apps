//! The screen's pixels on the bed, and the layers' heights.
//!
//! Pixel `(i, j)` covers `[i, i + 1] × [j, j + 1]` pitches from the bed's origin: column `i`
//! along X, row `j` along Y, row 0 at the front (Y = 0). Images stay in this bed order; the
//! printer's mirror and turn are applied only on the way out to the printer.

use fab_plate::machine::ResinScreen;

/// A rectangle of pixels, half-open: columns `x0..x1`, rows `y0..y1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl PixelRect {
    pub const EMPTY: PixelRect = PixelRect { x0: 0, y0: 0, x1: 0, y1: 0 };

    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }
    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }
    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }
    pub fn pixels(&self) -> usize {
        self.width() as usize * self.height() as usize
    }
    /// The smallest rectangle holding both.
    pub fn union(&self, o: &PixelRect) -> PixelRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        PixelRect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
    pub fn intersect(&self, o: &PixelRect) -> PixelRect {
        let r = PixelRect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) };
        if r.is_empty() { PixelRect::EMPTY } else { r }
    }
    pub fn contains(&self, i: u32, j: u32) -> bool {
        i >= self.x0 && i < self.x1 && j >= self.y0 && j < self.y1
    }
}

/// The screen's pixel grid on the bed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelGrid {
    pub pixels: [u32; 2],
    /// mm per pixel along X and Y.
    pub pitch: [f64; 2],
}

impl PixelGrid {
    pub fn of(screen: &ResinScreen) -> PixelGrid {
        PixelGrid { pixels: screen.pixels, pitch: screen.pitch }
    }

    /// Every pixel of the screen.
    pub fn all(&self) -> PixelRect {
        PixelRect { x0: 0, y0: 0, x1: self.pixels[0], y1: self.pixels[1] }
    }

    /// The bed point (mm) of pixel coordinates `(u, v)` (pixel `(i, j)`'s centre is
    /// `(i + 0.5, j + 0.5)`).
    pub fn at(&self, u: f64, v: f64) -> [f64; 2] {
        [u * self.pitch[0], v * self.pitch[1]]
    }

    /// Pixel `(i, j)`'s centre on the bed (mm).
    pub fn center(&self, i: u32, j: u32) -> [f64; 2] {
        self.at(i as f64 + 0.5, j as f64 + 0.5)
    }

    /// The pixel coordinates of a bed point (mm).
    pub fn to_pixels(&self, p: [f64; 2]) -> [f64; 2] {
        [p[0] / self.pitch[0], p[1] / self.pitch[1]]
    }

    /// The pixels a bed box (mm) touches, on the screen.
    pub fn rect_of(&self, lo: [f64; 2], hi: [f64; 2]) -> PixelRect {
        let a = self.to_pixels(lo);
        let b = self.to_pixels(hi);
        let clip = |v: f64, n: u32| v.clamp(0.0, n as f64) as u32;
        let r = PixelRect { x0: clip(a[0].floor(), self.pixels[0]), y0: clip(a[1].floor(), self.pixels[1]), x1: clip(b[0].ceil(), self.pixels[0]), y1: clip(b[1].ceil(), self.pixels[1]) };
        if r.is_empty() { PixelRect::EMPTY } else { r }
    }

    /// One pixel's area (mm²).
    pub fn pixel_area(&self) -> f64 {
        self.pitch[0] * self.pitch[1]
    }
}

/// The layers of a print `height` mm tall: layer `k` cures the slab `[k h, (k + 1) h]` above the
/// plate, and its image is the cross-section at the slab's middle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layers {
    pub height: f64,
    pub count: u32,
}

impl Layers {
    /// Enough layers of `layer_height` to reach `top` (mm above the plate).
    pub fn up_to(top: f64, layer_height: f64) -> Layers {
        let count = if top > 0.0 { (top / layer_height - 1e-9).ceil().max(1.0) as u32 } else { 0 };
        Layers { height: layer_height, count }
    }

    /// The height (mm) layer `k`'s image is cut at.
    pub fn z(&self, k: u32) -> f64 {
        (k as f64 + 0.5) * self.height
    }

    /// The layer whose slab holds `z` (clamped to the stack).
    pub fn at(&self, z: f64) -> u32 {
        ((z / self.height).floor().max(0.0) as u32).min(self.count.saturating_sub(1))
    }
}
