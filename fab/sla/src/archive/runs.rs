//! A layer image as the archive's run stream: one stream over the whole image, row after row,
//! each run of one 4-bit level either two bytes (black or full: the level and a 12-bit count) or
//! one byte (a grey level and a 4-bit count). Runs carry on across the ends of rows, so the black
//! rows around a part cost a few bytes, and only the band of lit rows is looked at.

use crate::export::Band;

/// The most pixels one run of black or full holds.
const LONG: u32 = 4095;
/// The most pixels one run of a grey level holds.
const SHORT: u32 = 15;

/// The run stream being written: the run still open, and the bytes of those closed.
struct Stream {
    out: Vec<u8>,
    level: u8,
    count: u64,
}

impl Stream {
    /// `count` pixels of `level` (0 to 15) after those so far.
    fn push(&mut self, level: u8, count: u64) {
        if count == 0 {
            return;
        }
        if level != self.level {
            self.close();
            self.level = level;
        }
        self.count += count;
    }

    /// The open run's bytes, split into runs no longer than its level allows.
    fn close(&mut self) {
        let level = self.level;
        let long = level == 0 || level == 15;
        let most = u64::from(if long { LONG } else { SHORT });
        while self.count > 0 {
            let n = self.count.min(most) as u32;
            if long {
                self.out.push(level << 4 | (n >> 8) as u8);
                self.out.push(n as u8);
            } else {
                self.out.push(level << 4 | n as u8);
            }
            self.count -= n as u64;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.close();
        self.out
    }
}

/// The run stream of an image's pixels, each a level from 0 to 15, row-major.
pub fn runs_of(levels: &[u8]) -> Vec<u8> {
    let mut s = Stream { out: Vec::new(), level: 0, count: 0 };
    for v in levels {
        s.push(*v, 1);
    }
    s.finish()
}

/// A layer's image in the archive: its run stream, and what the scene table says of it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunLayer {
    pub stream: Vec<u8>,
    /// Its lit pixels, each by its level's share of full (level / 15).
    pub lit: f64,
    /// The columns and rows its lit pixels lie in, half-open (`[x0, y0, x1, y1]`); none when it
    /// lights nothing.
    pub bounds: Option<[u32; 4]>,
}

/// The run stream of the screen image `band` (16-level greys: each pixel a multiple of 17): the
/// black rows above and below it, and its own rows.
pub fn run_layer(band: &Band) -> RunLayer {
    let [w, h] = band.size;
    let w64 = w as u64;
    let mut s = Stream { out: Vec::new(), level: 0, count: 0 };
    s.push(0, band.rows.start as u64 * w64);
    let (mut lit, mut sixteenths) = (None::<[u32; 4]>, 0u64);
    for (r, (row, on)) in band.pixels.chunks_exact(w as usize).zip(&band.lit).enumerate() {
        if !on {
            s.push(0, w64);
            continue;
        }
        let y = band.rows.start + r as u32;
        let mut x = 0;
        while x < row.len() {
            let v = row[x];
            debug_assert!(v % 17 == 0, "a 16-level grey");
            let level = v / 17;
            let start = x;
            while x < row.len() && row[x] == v {
                x += 1;
            }
            s.push(level, (x - start) as u64);
            if level > 0 {
                sixteenths += level as u64 * (x - start) as u64;
                let b = lit.get_or_insert([start as u32, y, x as u32, y + 1]);
                *b = [b[0].min(start as u32), b[1], b[2].max(x as u32), y + 1];
            }
        }
    }
    s.push(0, (h - band.rows.end) as u64 * w64);
    RunLayer { stream: s.finish(), lit: sixteenths as f64 / 15.0, bounds: lit }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The run code's worked examples: a short image whose last black run carries on across the
    /// end of its row, a full row of the 14K screen split at 4095 pixels, and a black 14K layer.
    #[test]
    fn the_run_codes_examples_encode_as_given() {
        let mut image = vec![0u8; 40];
        image[5..8].fill(15);
        image[8] = 7;
        assert_eq!(runs_of(&image), [0x00, 0x05, 0xF0, 0x03, 0x71, 0x00, 0x1F]);
        assert_eq!(runs_of(&[15; 13312]), [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xF4, 0x03]);
        // a grey run past 15 pixels splits too
        assert_eq!(runs_of(&[3; 17]), [0x3F, 0x32]);
        let black = Band { size: [13312, 5120], rows: 0..0, pixels: Vec::new(), lit: Vec::new() };
        let layer = run_layer(&black);
        assert_eq!(layer.stream.len(), 33_290);
        assert_eq!(layer.stream[..2], [0x0F, 0xFF]);
        assert_eq!(layer.stream[33_288..], [0x01, 0x04], "16 644 runs of 4095, then 260");
        assert_eq!((layer.lit, layer.bounds), (0.0, None));
    }

    /// A band's stream is the whole image's, its lit share and bounds the image's own.
    #[test]
    fn a_bands_stream_is_its_whole_images() {
        // a 6 x 5 image, rows 1 and 3 of its band lit
        let (w, h) = (6usize, 5usize);
        let mut image = vec![0u8; w * h];
        image[w + 2] = 17 * 15;
        image[w + 3] = 17 * 4;
        image[3 * w + 1..3 * w + 5].fill(17 * 15);
        let band = Band { size: [w as u32, h as u32], rows: 1..4, pixels: image[w..4 * w].to_vec(), lit: vec![true, false, true] };
        let layer = run_layer(&band);
        let levels: Vec<u8> = image.iter().map(|v| v / 17).collect();
        assert_eq!(layer.stream, runs_of(&levels));
        assert!((layer.lit - (5.0 + 4.0 / 15.0)).abs() < 1e-12);
        assert_eq!(layer.bounds, Some([1, 1, 5, 4]));
    }
}
