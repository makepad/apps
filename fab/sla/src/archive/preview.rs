//! The archive's previews: the print seen from above, every layer at once, in the part colour
//! on the preview's background, the screen's whole area fitted into the image. The printer shows
//! them in its list of files.

use crate::export::chunk;
use crate::grid::PixelGrid;
use crate::spans::Spans;
use makepad_fast_inflate::deflate::compress_to_vec_zlib;
use std::sync::atomic::{AtomicU64, Ordering};

/// The printer's two previews: their sizes and backgrounds.
pub const PREVIEWS: [([u32; 2], [u8; 3]); 2] = [([224, 168], [2, 72, 100]), ([336, 252], [20, 27, 41])];
/// The parts' colour in a preview.
const PART: [u8; 3] = [205, 205, 205];

/// A preview's pixels any layer lights, marked by layers made in parallel.
pub struct TopView {
    size: [u32; 2],
    /// Preview pixels per mm, and the preview pixel of the screen's back left corner.
    scale: f64,
    at: [f64; 2],
    /// The screen's depth (mm): its back is the preview's top row.
    depth: f64,
    lit: Vec<AtomicU64>,
}

impl TopView {
    /// An empty view `size` pixels large of `grid`'s whole screen.
    pub fn new(size: [u32; 2], grid: &PixelGrid) -> TopView {
        let mm = [grid.pixels[0] as f64 * grid.pitch[0], grid.pixels[1] as f64 * grid.pitch[1]];
        let scale = (size[0] as f64 / mm[0]).min(size[1] as f64 / mm[1]);
        let at = [0.5 * (size[0] as f64 - mm[0] * scale), 0.5 * (size[1] as f64 - mm[1] * scale)];
        let words = (size[0] as usize * size[1] as usize).div_ceil(64);
        TopView { size, scale, at, depth: mm[1], lit: (0..words).map(|_| AtomicU64::new(0)).collect() }
    }

    /// Marks the preview pixels `spans` lights on `grid`.
    pub fn mark(&self, spans: &Spans, grid: &PixelGrid) {
        let [w, h] = self.size.map(|s| s as f64);
        let mut last = None;
        for j in spans.rect.y0..spans.rect.y1 {
            let row = spans.row(j);
            if row.is_empty() {
                continue;
            }
            let r = (self.at[1] + (self.depth - (j as f64 + 0.5) * grid.pitch[1]) * self.scale).floor().clamp(0.0, h - 1.0) as usize;
            for run in row {
                let col = |i: u32| (self.at[0] + i as f64 * grid.pitch[0] * self.scale).clamp(0.0, w);
                let (c0, c1) = (col(run.x0).floor() as usize, (col(run.x0 + run.len).ceil() as usize).max(col(run.x0).floor() as usize + 1).min(self.size[0] as usize));
                // runs of one row of the screen mostly land on the same preview row and cells
                if last == Some((r, c0, c1)) {
                    continue;
                }
                last = Some((r, c0, c1));
                for c in c0..c1 {
                    let i = r * self.size[0] as usize + c;
                    self.lit[i / 64].fetch_or(1 << (i % 64), Ordering::Relaxed);
                }
            }
        }
    }

    /// The preview as an opaque 8-bit RGBA PNG: the part colour where any layer is lit, else
    /// `background`.
    pub fn png(&self, background: [u8; 3]) -> Vec<u8> {
        let [w, h] = self.size;
        let mut raw = Vec::with_capacity(h as usize * (1 + 4 * w as usize));
        for r in 0..h as usize {
            raw.push(0);
            for c in 0..w as usize {
                let i = r * w as usize + c;
                let on = self.lit[i / 64].load(Ordering::Relaxed) >> (i % 64) & 1 == 1;
                raw.extend_from_slice(&if on { PART } else { background });
                raw.push(255);
            }
        }
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&w.to_be_bytes());
        header.extend_from_slice(&h.to_be_bytes());
        // 8 bits a sample, RGBA, deflate, rows filtered by type, no interlace
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &header);
        chunk(&mut out, b"IDAT", &compress_to_vec_zlib(&raw, 6));
        chunk(&mut out, b"IEND", &[]);
        out
    }
}
