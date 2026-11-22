//! One panel's committed pixels. Publication is independent of reader activity.

use std::fmt;

use makepad_lock_free::{triple_buffer, Reader, Writer};

#[derive(Clone, Debug, PartialEq)]
pub struct ScreenFrame {
    /// Zero until the first publication; local to this port, not a host repaint key.
    pub generation: u64,
    /// The run's simulated time at the publication, seconds.
    pub t: f64,
    pub width: usize,
    pub height: usize,
    /// TextureFormat::VecBGRAu8_32 words: 0xFFRRGGBB (B, G, R, A on little endian).
    /// RGB565 is expanded and inversion is already applied. The viewer only copies.
    pub bgra: Vec<u32>,
    pub on: bool,
    pub brightness: u8,
    /// Mean emitted linear RGB over the glass, sRGB decoded before brightness.
    /// A display that is off emits zero, independently of its stored pixels.
    pub mean: [f32; 3],
}

#[derive(Clone)]
struct Slot {
    frame: ScreenFrame,
    // Each slot keeps its own dirty-row state as last-converted row versions.
    // Writer::update returns that slot's old contents, without cloning another slot.
    rows: Vec<u64>,
    // Linear sums before display gating, one per glass row in this slot.
    sums: Vec<[f64; 3]>,
}

pub struct ScreenWriter {
    frames: Writer<Slot>,
    width: usize,
    rows: Vec<u64>,
    generation: u64,
    changed: bool,
    inverted: bool,
    glass: [usize; 4],
    linear5: [f64; 32],
    linear6: [f64; 64],
}

impl fmt::Debug for ScreenWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScreenWriter")
            .field("width", &self.width)
            .field("height", &self.rows.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

pub struct ScreenReader {
    frames: Reader<Slot>,
    glass: [usize; 4],
    size: (usize, usize),
}

/// Allocates all three fixed-size slots and their independent row versions up front.
/// `glass` is [x, y, width, height] in memory texels, with row zero at the top.
pub fn screen_feed(width: usize, height: usize, glass: [usize; 4]) -> (ScreenWriter, ScreenReader) {
    let pixels = width.checked_mul(height).expect("screen dimensions overflow");
    assert!(width != 0 && height != 0, "screen dimensions must be nonzero");
    let [x, y, w, h] = glass;
    assert!(w != 0 && h != 0 && x.checked_add(w).is_some_and(|end| end <= width)
        && y.checked_add(h).is_some_and(|end| end <= height), "glass must fit screen memory");
    // Decode the same bit-replicated RGB8 values that the texture carries.
    // Only feed construction uses powf; publication is table lookup and addition.
    let decode = |byte: usize| {
        let srgb = byte as f64 / 255.0;
        if srgb <= 0.04045 { srgb / 12.92 } else { ((srgb + 0.055) / 1.055).powf(2.4) }
    };
    let (writer, reader) = triple_buffer(Slot {
        frame: ScreenFrame {
            generation: 0, t: 0.0, width, height,
            bgra: vec![0xff00_0000; pixels], on: false, brightness: 0,
            mean: [0.0; 3],
        },
        rows: vec![0; height],
        sums: vec![[0.0; 3]; h],
    });
    (
        ScreenWriter {
            frames: writer, width, rows: vec![1; height], generation: 0,
            changed: true, inverted: false,
            glass,
            linear5: std::array::from_fn(|v| decode((v << 3) | (v >> 2))),
            linear6: std::array::from_fn(|v| decode((v << 2) | (v >> 4))),
        },
        ScreenReader { frames: reader, glass, size: (width, height) },
    )
}

impl ScreenReader {
    /// The glass window in memory texels: x, y, width and height.
    pub fn glass(&self) -> [usize; 4] { self.glass }

    /// The whole frame memory's width and height in texels.
    pub fn size(&self) -> (usize, usize) { self.size }

    /// The newest completed frame, without waiting or requesting another publication.
    pub fn latest(&mut self) -> &ScreenFrame {
        &self.frames.latest().frame
    }
}

impl ScreenWriter {
    pub(crate) fn glass(&self) -> [usize; 4] { self.glass }

    /// Observation only: never journal or restore these versions on rewind.
    pub fn dirty_row(&mut self, row: usize) {
        self.rows[row] = self.generation.checked_add(1).expect("screen generation overflow");
        self.changed = true;
    }

    /// The next publication sends the whole restored memory, including unchanged rows.
    pub fn dirty_all(&mut self) {
        self.rows.fill(self.generation.checked_add(1).expect("screen generation overflow"));
        self.changed = true;
    }

    /// Call when on, brightness or inversion changes, including their restoration.
    pub fn changed(&mut self) {
        self.changed = true;
    }

    /// Publish only committed memory. With no changes this is one comparison; otherwise
    /// only rows stale in the selected slot are converted. No allocation or drop.
    pub fn publish(&mut self, t: f64, pixels: &[u16], on: bool, brightness: u8, inverted: bool) -> bool {
        if !self.changed { return false; }
        assert_eq!(pixels.len(), self.width * self.rows.len());
        self.generation = self.generation.checked_add(1).expect("screen generation overflow");
        if inverted != self.inverted {
            self.rows.fill(self.generation);
            self.inverted = inverted;
        }
        let width = self.width;
        let rows = &self.rows;
        let generation = self.generation;
        let [x, y, w, h] = self.glass;
        let linear5 = &self.linear5;
        let linear6 = &self.linear6;
        self.frames.update(|slot| {
            for (row, &changed) in rows.iter().enumerate() {
                if slot.rows[row] != changed {
                    let start = row * width;
                    for (out, &pixel) in slot.frame.bgra[start..start + width].iter_mut()
                        .zip(&pixels[start..start + width])
                    {
                        // Complementing RGB565 before bit replication equals complementing RGB8.
                        let pixel = if inverted { !pixel } else { pixel };
                        let r = u32::from(pixel >> 11);
                        let g = u32::from((pixel >> 5) & 63);
                        let b = u32::from(pixel & 31);
                        *out = 0xff00_0000 | (((r << 3) | (r >> 2)) << 16)
                            | (((g << 2) | (g >> 4)) << 8) | (b << 3) | (b >> 2);
                    }
                    if (y..y + h).contains(&row) {
                        let mut sum = [0.0; 3];
                        for &pixel in &pixels[start + x..start + x + w] {
                            let pixel = if inverted { !pixel } else { pixel };
                            sum[0] += linear5[usize::from(pixel >> 11)];
                            sum[1] += linear6[usize::from((pixel >> 5) & 63)];
                            sum[2] += linear5[usize::from(pixel & 31)];
                        }
                        slot.sums[row - y] = sum;
                    }
                    slot.rows[row] = changed;
                }
            }
            slot.frame.generation = generation;
            slot.frame.t = t;
            slot.frame.on = on;
            slot.frame.brightness = brightness;
            let mut sum = [0.0; 3];
            for row in &slot.sums {
                for channel in 0..3 { sum[channel] += row[channel]; }
            }
            slot.frame.mean = if on {
                sum.map(|channel| (channel / (w * h) as f64) as f32)
            } else {
                [0.0; 3]
            };
        });
        self.changed = false;
        true
    }
}
