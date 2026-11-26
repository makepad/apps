//! Numbers about a picture, so an agent can tell a blown-out or crushed
//! frame without looking at it: the share of clipped pixels, the share of
//! near-black ones, the mean luma and the tonal range.
//!
//! Measured on the frame the grab already read back (the output's own
//! pixels, before any scaling), in one pass over its bytes; no extra GPU
//! work and no extra readback. The thresholds follow the usual
//! false-colour convention: a pixel is clipped when any channel is at or
//! above 99 %, near black when its luma is below 2.5 %. Luma uses the
//! Rec. 709 weights on the encoded values. The numbers are reported, never
//! gated on: a dark look can be intended.

use crate::inspect::num;
use makepad_strict_json::{obj, Value};

/// Clipped: any channel at or above 99 % (252.45 of 255).
const CLIP_AT: u8 = 253;
/// Near black: luma below 2.5 %, in luma units of 1/10000 of a level.
const BLACK_BELOW: u32 = 63_750;

/// One picture's exposure.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Exposure {
    /// Share of pixels with a channel at or above 99 %, 0..1.
    pub clipped: f64,
    /// Share of pixels with luma below 2.5 %, 0..1.
    pub near_black: f64,
    /// Mean luma, 0..1.
    pub mean_luma: f64,
    /// Luma at the 2nd and 98th percentile, 0..1: the tonal range (a
    /// narrow one is a flat, washed-out or murky picture).
    pub low: f64,
    pub high: f64,
}

impl Exposure {
    /// Measure tight RGBA bytes (alpha is not read: a grab is opaque).
    pub fn of_rgba(rgba: &[u8]) -> Exposure {
        let mut histogram = [0u64; 256];
        let (mut clipped, mut black, mut sum) = (0u64, 0u64, 0u64);
        for px in rgba.chunks_exact(4) {
            let (r, g, b) = (px[0], px[1], px[2]);
            // Rec. 709 luma in 1/10000 of a level (0..2_550_000).
            let luma = 2126 * r as u32 + 7152 * g as u32 + 722 * b as u32;
            clipped += (r.max(g).max(b) >= CLIP_AT) as u64;
            black += (luma < BLACK_BELOW) as u64;
            sum += luma as u64;
            histogram[((luma + 5000) / 10_000).min(255) as usize] += 1;
        }
        let n = (rgba.len() / 4) as u64;
        if n == 0 {
            return Exposure::default();
        }
        // The level below which `share` of the pixels lie.
        let percentile = |share: f64| -> f64 {
            let target = (share * n as f64).ceil().max(1.0) as u64;
            let mut seen = 0;
            for (level, count) in histogram.iter().enumerate() {
                seen += count;
                if seen >= target {
                    return level as f64 / 255.0;
                }
            }
            1.0
        };
        Exposure {
            clipped: clipped as f64 / n as f64,
            near_black: black as f64 / n as f64,
            mean_luma: sum as f64 / n as f64 / 2_550_000.0,
            low: percentile(0.02),
            high: percentile(0.98),
        }
    }

    pub fn to_json(&self) -> Value {
        obj(vec![
            ("clipped", num(self.clipped)),
            ("near_black", num(self.near_black)),
            ("mean_luma", num(self.mean_luma)),
            ("range", Value::Arr(vec![num(self.low), num(self.high)])),
        ])
    }

    /// `12% clipped, 3% near-black, mean luma 0.41, range 0.02–0.98`.
    pub fn to_text(&self) -> String {
        format!("{} clipped, {} near-black, mean luma {:.2}, range {:.2}–{:.2}", percent(self.clipped), percent(self.near_black), self.mean_luma, self.low, self.high)
    }
}

fn percent(share: f64) -> String {
    let p = share * 100.0;
    if p > 0.0 && p < 10.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// The exposure of a run of frames: the worst frame for clipping and for
/// black, and the mean luma over all of them.
#[derive(Clone, Debug, PartialEq)]
pub struct ExposureSummary {
    /// The frame with the most clipped pixels, and its share.
    pub most_clipped: (u64, f64, f64),
    /// The frame with the most near-black pixels, and its share.
    pub most_black: (u64, f64, f64),
    /// The mean of the frames' mean lumas, and their least and most.
    pub mean_luma: f64,
    pub luma_span: (f64, f64),
}

impl ExposureSummary {
    /// `frames`: `(frame, t, exposure)` in order; `None` for none.
    pub fn of(frames: &[(u64, f64, Exposure)]) -> Option<ExposureSummary> {
        let first = frames.first()?;
        let worst = |key: fn(&Exposure) -> f64| {
            let (frame, t, e) = frames.iter().fold(first, |best, f| if key(&f.2) > key(&best.2) { f } else { best });
            (*frame, *t, key(e))
        };
        let lumas = frames.iter().map(|f| f.2.mean_luma);
        Some(ExposureSummary {
            most_clipped: worst(|e| e.clipped),
            most_black: worst(|e| e.near_black),
            mean_luma: lumas.clone().sum::<f64>() / frames.len() as f64,
            luma_span: (lumas.clone().fold(f64::MAX, f64::min), lumas.fold(f64::MIN, f64::max)),
        })
    }

    pub fn to_json(&self) -> Value {
        let at = |(frame, t, share): (u64, f64, f64)| obj(vec![("share", num(share)), ("frame", Value::Int(frame as i64)), ("t", num(t))]);
        obj(vec![
            ("most_clipped", at(self.most_clipped)),
            ("most_near_black", at(self.most_black)),
            ("mean_luma", num(self.mean_luma)),
            ("mean_luma_span", Value::Arr(vec![num(self.luma_span.0), num(self.luma_span.1)])),
        ])
    }

    /// `worst 12% clipped (@48, 1.60 s), worst 3% near-black (@0, 0.00 s), mean luma 0.41 (0.20–0.65)`.
    pub fn to_text(&self) -> String {
        let (cf, ct, c) = self.most_clipped;
        let (bf, bt, b) = self.most_black;
        format!(
            "worst {} clipped (@{cf}, {ct:.2} s), worst {} near-black (@{bf}, {bt:.2} s), mean luma {:.2} ({:.2}–{:.2})",
            percent(c),
            percent(b),
            self.mean_luma,
            self.luma_span.0,
            self.luma_span.1
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `w`×`h` RGBA where `paint(x, y)` gives each pixel's colour.
    fn frame(w: usize, h: usize, paint: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = paint(x, y);
                out.extend_from_slice(&[r, g, b, 255]);
            }
        }
        out
    }

    /// A test card with known clipping: a quarter blown white, a quarter
    /// one channel clipped (pure red: clipped, not white), a quarter black,
    /// a quarter mid grey; the numbers come out as made.
    #[test]
    fn a_frame_with_known_clipping_and_black_gives_its_shares() {
        let card = frame(40, 20, |x, y| match (x < 20, y < 10) {
            (true, true) => [255, 255, 255],
            (false, true) => [255, 0, 0],
            (true, false) => [0, 0, 0],
            (false, false) => [128, 128, 128],
        });
        let e = Exposure::of_rgba(&card);
        assert_eq!(e.clipped, 0.5, "white and pure red are clipped: {e:?}");
        assert_eq!(e.near_black, 0.25, "only the black quarter is near black (pure red's luma is 21%): {e:?}");
        let expected = (1.0 + 0.2126 + 0.0 + 128.0 / 255.0) / 4.0;
        assert!((e.mean_luma - expected).abs() < 1e-6, "mean luma {} is {expected}: {e:?}", e.mean_luma);
        assert_eq!((e.low, e.high), (0.0, 1.0), "the range spans black to white: {e:?}");
        assert_eq!(e.to_text(), "50% clipped, 25% near-black, mean luma 0.43, range 0.00–1.00");

        // The edges of the thresholds: 252 is not clipped, 253 is; luma
        // 6 levels (2.35%) is near black, 7 (2.75%) is not.
        let edges = frame(4, 1, |x, _| [[252, 252, 252], [253, 0, 0], [6, 6, 6], [7, 7, 7]][x]);
        let e = Exposure::of_rgba(&edges);
        assert_eq!((e.clipped, e.near_black), (0.25, 0.25), "{e:?}");

        // A flat grey frame: nothing clipped or black, a narrow range.
        let e = Exposure::of_rgba(&frame(8, 8, |_, _| [100, 100, 100]));
        assert_eq!((e.clipped, e.near_black), (0.0, 0.0));
        assert_eq!((e.low, e.high), (100.0 / 255.0, 100.0 / 255.0));
    }

    #[test]
    fn a_run_of_frames_names_its_worst_frames() {
        let dark = Exposure { near_black: 0.8, mean_luma: 0.05, ..Default::default() };
        let blown = Exposure { clipped: 0.3, mean_luma: 0.7, ..Default::default() };
        let even = Exposure { clipped: 0.01, near_black: 0.02, mean_luma: 0.45, ..Default::default() };
        let s = ExposureSummary::of(&[(0, 0.0, dark), (24, 1.0, even), (48, 2.0, blown)]).unwrap();
        assert_eq!(s.most_clipped, (48, 2.0, 0.3));
        assert_eq!(s.most_black, (0, 0.0, 0.8));
        assert!((s.mean_luma - 0.4).abs() < 1e-9);
        assert_eq!(s.luma_span, (0.05, 0.7));
        assert_eq!(s.to_text(), "worst 30% clipped (@48, 2.00 s), worst 80% near-black (@0, 0.00 s), mean luma 0.40 (0.05–0.70)");
        assert!(ExposureSummary::of(&[]).is_none());
    }
}
