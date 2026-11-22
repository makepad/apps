//! A layer as spans: per row of pixels, the runs it covers, each either wholly covered or a run
//! of edge pixels with their covered shares. Empty rows and the space around the part cost
//! nothing, and long runs of the inside cost one entry. The view, the checks and the export all
//! read a layer this way; a dense image is made only for a texture or a file.
//!
//! A share is a pixel's coverage in 255ths (255: the whole pixel). How a share shows
//! on the screen (its grey level) is the printer's ([`grey`]).

use crate::grid::PixelRect;

/// A run of pixels in one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub x0: u32,
    pub len: u32,
    /// [`FULL`]: every pixel wholly covered; otherwise where its pixels' shares start in the
    /// layer's shares.
    pub shares: u32,
}

/// A run's `shares` for a run of wholly covered pixels.
pub const FULL: u32 = u32::MAX;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spans {
    /// The rows and columns the runs lie in.
    pub rect: PixelRect,
    /// Row `r` of `rect` holds `runs[rows[r]..rows[r + 1]]`, left to right.
    rows: Vec<u32>,
    runs: Vec<Run>,
    shares: Vec<u8>,
}

impl Spans {
    /// The spans of `cov`, shares over `rect` row by row.
    pub fn of(rect: PixelRect, cov: &[u8]) -> Spans {
        let w = rect.width() as usize;
        let mut s = Spans { rect, rows: Vec::with_capacity(rect.height() as usize + 1), runs: Vec::new(), shares: Vec::new() };
        s.rows.push(0);
        // a word of pixels at a time where all are uncovered or all wholly covered: most are
        let word = |row: &[u8], x: usize| u64::from_ne_bytes(row[x..x + 8].try_into().unwrap());
        for row in cov.chunks_exact(w.max(1)).take(rect.height() as usize) {
            let mut x = 0;
            while x < w {
                while x + 8 <= w && word(row, x) == 0 {
                    x += 8;
                }
                if x == w {
                    break;
                }
                let v = row[x];
                if v == 0 {
                    x += 1;
                    continue;
                }
                let start = x;
                if v == 255 {
                    while x + 8 <= w && word(row, x) == u64::MAX {
                        x += 8;
                    }
                    while x < w && row[x] == 255 {
                        x += 1;
                    }
                    s.runs.push(Run { x0: rect.x0 + start as u32, len: (x - start) as u32, shares: FULL });
                } else {
                    let at = s.shares.len() as u32;
                    while x < w && row[x] != 0 && row[x] != 255 {
                        s.shares.push(row[x]);
                        x += 1;
                    }
                    s.runs.push(Run { x0: rect.x0 + start as u32, len: (x - start) as u32, shares: at });
                }
            }
            s.rows.push(s.runs.len() as u32);
        }
        // nothing at all: no rect either
        if s.runs.is_empty() {
            return Spans::default();
        }
        s
    }

    /// Spans whose rects never meet as one.
    pub fn join(mut parts: Vec<Spans>) -> Spans {
        parts.retain(|p| !p.is_empty());
        if parts.len() < 2 {
            return parts.pop().unwrap_or_default();
        }
        // apart, two rects sharing a row share no column, so in a row they lie in x order
        parts.sort_unstable_by_key(|p| p.rect.x0);
        let rect = parts.iter().fold(PixelRect::EMPTY, |r, p| r.union(&p.rect));
        let mut s = Spans { rect, rows: Vec::with_capacity(rect.height() as usize + 1), ..Spans::default() };
        s.rows.push(0);
        for j in rect.y0..rect.y1 {
            for p in &parts {
                for run in p.row(j) {
                    let shares = if run.shares == FULL {
                        FULL
                    } else {
                        s.shares.extend_from_slice(p.shares_of(run));
                        (s.shares.len() - run.len as usize) as u32
                    };
                    s.runs.push(Run { shares, ..*run });
                }
            }
            s.rows.push(s.runs.len() as u32);
        }
        s
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Row `j`'s runs (none outside the rect).
    pub fn row(&self, j: u32) -> &[Run] {
        if j < self.rect.y0 || j >= self.rect.y1 {
            return &[];
        }
        let r = (j - self.rect.y0) as usize;
        &self.runs[self.rows[r] as usize..self.rows[r + 1] as usize]
    }

    /// The shares of a run of edge pixels (empty for a wholly covered run).
    pub fn shares_of(&self, run: &Run) -> &[u8] {
        if run.shares == FULL { &[] } else { &self.shares[run.shares as usize..(run.shares + run.len) as usize] }
    }

    /// Pixel `(i, j)`'s share.
    pub fn at(&self, i: u32, j: u32) -> u8 {
        let row = self.row(j);
        // the last run starting at or left of i
        let k = row.partition_point(|r| r.x0 <= i);
        if k == 0 {
            return 0;
        }
        let r = &row[k - 1];
        if i >= r.x0 + r.len {
            0
        } else if r.shares == FULL {
            255
        } else {
            self.shares[(r.shares + i - r.x0) as usize]
        }
    }

    /// The covered area in pixels: each pixel by its share.
    pub fn covered(&self) -> f64 {
        let full: u64 = self.runs.iter().filter(|r| r.shares == FULL).map(|r| r.len as u64).sum();
        let grey: u64 = self.shares.iter().map(|s| *s as u64).sum();
        full as f64 + grey as f64 / 255.0
    }

    /// The pixels covered at all.
    pub fn lit(&self) -> usize {
        self.runs.iter().map(|r| r.len as usize).sum()
    }

    /// The image over `rect`, row-major, each pixel's grey level on a screen of `levels`
    /// ([`grey`]).
    pub fn image(&self, rect: PixelRect, levels: u16) -> Vec<u8> {
        let w = rect.width() as usize;
        let mut out = vec![0u8; rect.pixels()];
        let lut: [u8; 256] = std::array::from_fn(|s| grey(s as u8, levels));
        for j in rect.y0.max(self.rect.y0)..rect.y1.min(self.rect.y1) {
            let line = &mut out[(j - rect.y0) as usize * w..][..w];
            for run in self.row(j) {
                let (a, b) = (run.x0.max(rect.x0), (run.x0 + run.len).min(rect.x1));
                if a >= b {
                    continue;
                }
                let dst = &mut line[(a - rect.x0) as usize..(b - rect.x0) as usize];
                if run.shares == FULL {
                    dst.fill(255);
                } else {
                    let src = &self.shares_of(run)[(a - run.x0) as usize..(b - run.x0) as usize];
                    for (d, s) in dst.iter_mut().zip(src) {
                        *d = lut[*s as usize];
                    }
                }
            }
        }
        out
    }
}

/// The value a pixel covered by `share` (of 255) shows on a screen of `levels` grey levels: the
/// nearest of them, 0 and 255 at the ends.
pub fn grey(share: u8, levels: u16) -> u8 {
    let top = (levels.clamp(2, 256) - 1) as u32;
    let level = (share as u32 * top + 127) / 255;
    ((level * 255 + top / 2) / top) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long runs of empty and of wholly covered pixels, starting and ending on and off the
    /// words the scan steps by, in rows of every width up to 40: the same runs as a pixel at a
    /// time finds.
    #[test]
    fn spans_of_long_runs_are_the_runs_pixel_by_pixel() {
        let by_pixel = |w: usize, row: &[u8]| {
            let mut runs: Vec<(usize, usize, bool)> = Vec::new();
            for (x, &v) in row.iter().enumerate() {
                if v == 0 {
                    continue;
                }
                let full = v == 255;
                match runs.last_mut() {
                    Some((x0, len, f)) if *f == full && *x0 + *len == x => *len += 1,
                    _ => runs.push((x, 1, full)),
                }
            }
            assert!(runs.iter().all(|r| r.0 + r.1 <= w));
            runs
        };
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for w in 1..=40usize {
            let h = 12;
            let mut cov = vec![0u8; w * h];
            for row in cov.chunks_exact_mut(w) {
                let mut x = 0;
                while x < w {
                    let len = 1 + (next() % 19) as usize;
                    let v = match next() % 4 {
                        0 | 1 => 0,
                        2 => 255,
                        _ => 1 + (next() % 254) as u8,
                    };
                    for p in row.iter_mut().skip(x).take(len) {
                        *p = if v == 0 || v == 255 { v } else { 1 + (next() % 254) as u8 };
                    }
                    x += len;
                }
            }
            let rect = PixelRect { x0: 3, y0: 5, x1: 3 + w as u32, y1: 5 + h as u32 };
            let s = Spans::of(rect, &cov);
            for (r, row) in cov.chunks_exact(w).enumerate() {
                let got: Vec<(usize, usize, bool)> = s.row(5 + r as u32).iter().map(|run| ((run.x0 - 3) as usize, run.len as usize, run.shares == FULL)).collect();
                assert_eq!(got, by_pixel(w, row), "width {w}, row {r}");
                for (x, &v) in row.iter().enumerate() {
                    assert_eq!(s.at(3 + x as u32, 5 + r as u32), v, "width {w}, ({x}, {r})");
                }
            }
        }
    }

    /// Spans hold what the image held, a share and a grey level each pixel, in fewer entries.
    #[test]
    fn spans_keep_every_pixel() {
        let rect = PixelRect { x0: 10, y0: 20, x1: 18, y1: 23 };
        #[rustfmt::skip]
        let cov = [
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 40, 255, 255, 255, 200, 0, 9,
            128, 255, 255, 0, 255, 255, 255, 255,
        ];
        let s = Spans::of(rect, &cov);
        assert_eq!(s.row(20).len(), 0);
        assert_eq!(s.row(21), &[Run { x0: 11, len: 1, shares: 0 }, Run { x0: 12, len: 3, shares: FULL }, Run { x0: 15, len: 1, shares: 1 }, Run { x0: 17, len: 1, shares: 2 }]);
        assert_eq!(s.row(22).len(), 3);
        for j in 20..23 {
            for i in 10..18 {
                assert_eq!(s.at(i, j), cov[(j - 20) as usize * 8 + (i - 10) as usize], "({i}, {j})");
            }
        }
        assert_eq!((s.at(9, 21), s.at(18, 22), s.at(12, 19)), (0, 0, 0));
        assert_eq!(s.lit(), 6 + 7);
        assert!((s.covered() - (3.0 + 6.0 + (40.0 + 200.0 + 9.0 + 128.0) / 255.0)).abs() < 1e-9);
        assert_eq!(s.image(rect, 256), cov);
        // on 16 levels, a share rounds to the nearest 17th
        assert_eq!(s.image(rect, 16)[8 + 1], 34);
        assert_eq!((grey(0, 16), grey(8, 16), grey(9, 16), grey(255, 16), grey(127, 2), grey(128, 2)), (0, 0, 17, 255, 0, 255));
        assert!(Spans::of(rect, &[0; 24]).is_empty());
        // two apart, side by side and above: one layer, each pixel as it was
        let right = PixelRect { x0: 18, y0: 21, x1: 20, y1: 22 };
        let below = PixelRect { x0: 4, y0: 10, x1: 6, y1: 12 };
        let joined = Spans::join(vec![Spans::of(right, &[77, 255]), s.clone(), Spans::default(), Spans::of(below, &[255, 255, 0, 3])]);
        assert_eq!(joined.rect, PixelRect { x0: 4, y0: 10, x1: 20, y1: 23 });
        assert_eq!(joined.row(21).len(), 6);
        assert_eq!((joined.at(18, 21), joined.at(19, 21), joined.at(5, 11), joined.at(4, 11), joined.at(11, 21)), (77, 255, 3, 0, 40));
        assert_eq!(joined.lit(), s.lit() + 2 + 3);
        assert!((joined.covered() - s.covered() - 1.0 - (77.0 + 3.0) / 255.0 - 2.0).abs() < 1e-9);
    }
}
