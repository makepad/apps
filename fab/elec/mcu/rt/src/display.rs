//! Reference panel storage. Draws reach the panel in call order; firmware
//! cannot observe when. A refusal leaves the framebuffer unchanged.
use crate::{module::Buffer, sched::DrawRefusal, DisplayProfile};
use alloc::{vec, vec::Vec};

pub(crate) struct Display {
    profile: DisplayProfile,
    pixels: Vec<u16>,
}

struct Window {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    dx: usize,
    dy: usize,
}

impl Display {
    pub(crate) fn new(profile: DisplayProfile) -> Self {
        // The reference starts at zero. Real panel RAM is junk until written
        // (CO5300 P17); programs must draw all pixels whose contents matter.
        let pixels = vec![0; profile.width as usize * profile.height as usize];
        Self { profile, pixels }
    }

    pub(crate) fn pixels(&self) -> &[u16] { &self.pixels }

    fn clip(&self, x: i32, y: i32, w: i32, h: i32) -> Result<Option<Window>, DrawRefusal> {
        if w < 0 || h < 0 { return Err(DrawRefusal::SourceOutside); }
        // Widen before adding: signed port coordinates can span the i32 range.
        let (x, y, w, h) = (x as i64, y as i64, w as i64, h as i64);
        let (left, top) = (x.max(0), y.max(0));
        let (right, bottom) = ((x + w).min(self.profile.width as i64), (y + h).min(self.profile.height as i64));
        if left >= right || top >= bottom { return Ok(None); }
        let (width, height) = (right - left, bottom - top);
        // CO5300 P15/P16: refuse odd clipped windows, never invent pixels
        // to widen them. An odd source origin is allowed.
        if (left | top | width | height) & 1 != 0 { return Err(DrawRefusal::OddWindow); }
        Ok(Some(Window { x: left as usize, y: top as usize, w: width as usize, h: height as usize, dx: (left - x) as usize, dy: (top - y) as usize }))
    }

    pub(crate) fn present(&mut self, buffer: Buffer, pixels: &[u32], (width, height): (u32, u32), args: [i32; 8]) -> Result<(), DrawRefusal> {
        let [d, _, sx, sy, w, h, x, y] = args;
        // v1 has one display, whose verified ordinal is zero.
        if d != 0 { return Err(DrawRefusal::Handle); }
        if w < 0 || h < 0 || sx < 0 || sy < 0
            || sx as i64 + w as i64 > width as i64 || sy as i64 + h as i64 > height as i64 {
            return Err(DrawRefusal::SourceOutside);
        }
        let Some(window) = self.clip(x, y, w, h)? else { return Ok(()); };
        let (sx, sy) = (sx as usize + window.dx, sy as usize + window.dy);
        for row in 0..window.h {
            let at = (window.y + row) * self.profile.width as usize + window.x;
            for col in 0..window.w {
                self.pixels[at + col] = buffer.load(pixels, (sy + row) * width as usize + sx + col) as u16;
            }
        }
        Ok(())
    }

    pub(crate) fn fill_rect(&mut self, args: [i32; 6]) -> Result<(), DrawRefusal> {
        let [d, x, y, w, h, color] = args;
        if d != 0 { return Err(DrawRefusal::Handle); }
        let Some(window) = self.clip(x, y, w, h)? else { return Ok(()); };
        for row in 0..window.h {
            let at = (window.y + row) * self.profile.width as usize + window.x;
            self.pixels[at..at + window.w].fill(color as u16);
        }
        Ok(())
    }
}
