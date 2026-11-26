//! Per-frame loudness and band energies at [`crate::RATE`] frames per
//! second, from a mono signal.

/// The bands, as (name, low Hz, high Hz).
pub const BANDS: [(&str, f64, f64); 6] = [
    ("sub", 20.0, 60.0),
    ("bass", 60.0, 250.0),
    ("low_mid", 250.0, 500.0),
    ("mid", 500.0, 2000.0),
    ("high_mid", 2000.0, 6000.0),
    ("high", 6000.0, 16000.0),
];

/// A second-order section (RBJ cookbook), direct form I.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn band_pass(rate: f64, center: f64, q: f64) -> Biquad {
        let w = 2.0 * std::f64::consts::PI * (center / rate).min(0.49);
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Biquad { b: [alpha / a0, 0.0, -alpha / a0], a: [-2.0 * w.cos() / a0, (1.0 - alpha) / a0], ..Biquad::default() }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// Loudness in dBFS (RMS) and each band's RMS level in dB, per frame.
pub struct Frames {
    pub loudness_db: Vec<f32>,
    pub bands_db: Vec<[f32; 6]>,
}

fn db(mean_square: f64) -> f32 {
    (10.0 * (mean_square + 1e-12).log10()).max(-120.0) as f32
}

pub fn measure(mono: &[f32], sample_rate: f64, rate: f64) -> Frames {
    let hop = (sample_rate / rate).round().max(1.0) as usize;
    // Two cascaded band-passes per band at its geometric centre; the Q
    // follows the band's width in octaves.
    let mut filters: Vec<[Biquad; 2]> = BANDS
        .iter()
        .map(|(_, lo, hi)| {
            let hi = hi.min(sample_rate * 0.45);
            let center = (lo * hi).sqrt();
            let q = center / (hi - lo).max(1.0);
            let q = q.max(0.3);
            [Biquad::band_pass(sample_rate, center, q), Biquad::band_pass(sample_rate, center, q)]
        })
        .collect();
    let frames = mono.len().div_ceil(hop);
    let mut out = Frames { loudness_db: Vec::with_capacity(frames), bands_db: Vec::with_capacity(frames) };
    for chunk in mono.chunks(hop) {
        let mut total = 0.0;
        let mut bands = [0.0f64; 6];
        for &sample in chunk {
            let x = sample as f64;
            total += x * x;
            for (band, filter) in filters.iter_mut().enumerate() {
                let first = filter[0].run(x);
                let y = filter[1].run(first);
                bands[band] += y * y;
            }
        }
        let n = chunk.len().max(1) as f64;
        out.loudness_db.push(db(total / n));
        out.bands_db.push(bands.map(|energy| db(energy / n)));
    }
    out
}

/// The `p`-quantile (0..1) of finite values.
pub fn quantile(values: &[f32], p: f64) -> f32 {
    let mut sorted: Vec<f32> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return 0.0;
    }
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted[((sorted.len() - 1) as f64 * p.clamp(0.0, 1.0)).round() as usize]
}

/// Map dB values to 0..1 between the track's quiet floor (`lo` quantile)
/// and its loud ceiling (`hi` quantile).
pub fn normalize(values_db: &[f32], lo: f64, hi: f64) -> Vec<f32> {
    let ceil = quantile(values_db, hi);
    // Digital silence is not the floor: at most 50 dB of range is shown.
    let floor = quantile(values_db, lo).max(ceil - 50.0);
    let ceil = ceil.max(floor + 1.0);
    values_db.iter().map(|v| ((v - floor) / (ceil - floor)).clamp(0.0, 1.0)).collect()
}

/// Levels in dB averaged as power over `radius` frames each side: a beat's
/// attack and its silence read as one level, not as their mean in dB.
pub fn smooth_db(values_db: &[f32], radius: usize) -> Vec<f32> {
    let power: Vec<f32> = values_db.iter().map(|db| 10f32.powf(db / 10.0)).collect();
    smooth(&power, radius).into_iter().map(|p| (10.0 * (p.max(1e-12)).log10()).max(-120.0)).collect()
}

/// A centred moving average over `radius` frames each side.
pub fn smooth(values: &[f32], radius: usize) -> Vec<f32> {
    let mut prefix = vec![0.0f64; values.len() + 1];
    for (i, v) in values.iter().enumerate() {
        prefix[i + 1] = prefix[i] + *v as f64;
    }
    (0..values.len())
        .map(|i| {
            let a = i.saturating_sub(radius);
            let b = (i + radius + 1).min(values.len());
            ((prefix[b] - prefix[a]) / (b - a).max(1) as f64) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, secs: f64, rate: f64) -> Vec<f32> {
        (0..(secs * rate) as usize).map(|i| (0.5 * (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin()) as f32).collect()
    }

    #[test]
    fn a_tone_lands_in_its_band() {
        let frames = measure(&tone(100.0, 1.0, 48_000.0), 48_000.0, 100.0);
        assert_eq!(frames.loudness_db.len(), 100);
        let last = frames.bands_db[80];
        let loudest = (0..6).max_by(|a, b| last[*a].total_cmp(&last[*b])).unwrap();
        assert_eq!(BANDS[loudest].0, "bass");
        let frames = measure(&tone(3000.0, 1.0, 48_000.0), 48_000.0, 100.0);
        let last = frames.bands_db[80];
        let loudest = (0..6).max_by(|a, b| last[*a].total_cmp(&last[*b])).unwrap();
        assert_eq!(BANDS[loudest].0, "high_mid");
        // A 0.5 sine is −9 dBFS RMS.
        assert!((frames.loudness_db[50] + 9.03).abs() < 0.2, "{}", frames.loudness_db[50]);
    }

    #[test]
    fn quantiles_normalize_and_smooth() {
        let v: Vec<f32> = (0..101).map(|i| i as f32 * 0.4).collect();
        assert_eq!(quantile(&v, 0.5), 20.0);
        let n = normalize(&v, 0.1, 0.9);
        assert_eq!((n[0], n[50], n[100]), (0.0, 0.5, 1.0));
        // Silence far below the music does not stretch the scale.
        let quiet: Vec<f32> = [-120.0f32; 10].into_iter().chain((0..=20).map(|i| -40.0 + i as f32 * 2.0)).collect();
        let n = normalize(&quiet, 0.05, 1.0);
        assert!((n[20] - 0.6).abs() < 1e-5, "{}", n[20]);
        assert_eq!(smooth(&[0.0, 3.0, 0.0], 1), vec![1.5, 1.0, 1.5]);
    }
}
