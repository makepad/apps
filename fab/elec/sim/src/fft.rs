//! Fourier transforms, periodic windows, and harmonic analysis of sampled records.
//!
//! Forward transforms use a negative exponential; inverse transforms include the
//! reciprocal length. Real spectra report peak amplitudes and cosine phases.

use crate::num::C64;
use std::f64::consts::TAU;

/// A transform length that is not a nonzero power of two.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FftLenError(pub usize);

/// Transforms `data` in place without allocating, using radix-2 decimation in time.
///
/// The forward transform is `X[k] = sum_j x[j] exp(-2*pi*i*j*k/n)`; the
/// inverse uses the positive sign and divides by `n`. Length one is the
/// identity. Other non-power-of-two lengths (including zero) return an error
/// without changing the input. Direct sine/cosine twiddles avoid recurrence
/// drift at long lengths, including 65536 points.
pub fn fft(data: &mut [C64], inverse: bool) -> Result<(), FftLenError> {
    let n = data.len();
    if !n.is_power_of_two() {
        return Err(FftLenError(n));
    }
    if n == 1 {
        return Ok(());
    }

    let mut reversed = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if i < reversed {
            data.swap(i, reversed);
        }
    }

    let direction = if inverse { 1.0 } else { -1.0 };
    let mut width = 2;
    loop {
        let half = width / 2;
        for offset in 0..half {
            let angle = direction * TAU * (offset as f64 / width as f64);
            let (sin, cos) = angle.sin_cos();
            let twiddle = C64::new(cos, sin);
            for first in (offset..n).step_by(width) {
                let even = data[first];
                let odd = data[first + half] * twiddle;
                data[first] = even + odd;
                data[first + half] = even - odd;
            }
        }
        if width == n {
            break;
        }
        width *= 2;
    }

    if inverse {
        let scale = 1.0 / n as f64;
        for value in data {
            *value = value.scale(scale);
        }
    }
    Ok(())
}

/// Returns the smallest power of two at least `n`, with zero mapping to one.
/// Panics if that power of two cannot be represented by `usize`.
pub fn next_pow2(n: usize) -> usize {
    n.checked_next_power_of_two()
        .expect("FFT length exceeds the largest representable power of two")
}

/// Periodic (DFT-even) windows, evaluated with phase `2*pi*i/n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// Unit weights.
    Rect,
    /// Raised cosine: `0.5 - 0.5*cos(p)`.
    Hann,
    /// Hamming: `0.54 - 0.46*cos(p)`.
    Hamming,
    /// Four-term minimum-sidelobe Blackman-Harris window.
    BlackmanHarris,
    /// Five-term flat-top window for low scalloping loss.
    FlatTop,
}

impl Window {
    /// Returns coefficient `i` of the periodic window of length `n` (zero for
    /// an empty window). The phase denominator is `n`, rather than `n - 1`.
    pub fn coeff(self, i: usize, n: usize) -> f64 {
        if n == 0 {
            return 0.0;
        }
        let p = TAU * (i as f64 / n as f64);
        match self {
            Self::Rect => 1.0,
            Self::Hann => 0.5 - 0.5 * p.cos(),
            Self::Hamming => 0.54 - 0.46 * p.cos(),
            Self::BlackmanHarris => {
                0.35875 - 0.48829 * p.cos() + 0.14128 * (2.0 * p).cos() - 0.01168 * (3.0 * p).cos()
            }
            Self::FlatTop => {
                0.21557895 - 0.41663158 * p.cos() + 0.277263158 * (2.0 * p).cos()
                    - 0.083578947 * (3.0 * p).cos()
                    + 0.006947368 * (4.0 * p).cos()
            }
        }
    }

    /// Computes `sum(w)/n` from the actual coefficients, including short-window
    /// effects. An empty window has gain zero.
    pub fn coherent_gain(self, n: usize) -> f64 {
        if n == 0 {
            return 0.0;
        }
        (0..n).map(|i| self.coeff(i, n)).sum::<f64>() / n as f64
    }

    /// Computes the equivalent noise bandwidth `n*sum(w*w)/sum(w)^2` in bins.
    /// Returns zero for an empty window or a window whose coefficient sum is zero.
    pub fn enbw_bins(self, n: usize) -> f64 {
        let mut sum = 0.0;
        let mut sum_squares = 0.0;
        for i in 0..n {
            let w = self.coeff(i, n);
            sum += w;
            sum_squares += w * w;
        }
        if sum == 0.0 {
            0.0
        } else {
            n as f64 * sum_squares / (sum * sum)
        }
    }
}

/// A single-sided, window-corrected peak-amplitude spectrum of a real record.
pub struct Spectrum {
    /// Frequency spacing in Hz: `1/(n_fft*dt)`.
    pub df: f64,
    /// Peak amplitudes in bins `0..=n_fft/2`; DC and Nyquist are not doubled.
    pub amp: Vec<f64>,
    /// Cosine phases in radians, relative to the first sample. Phases below
    /// `1e-12` of the largest amplitude, and phases of zero bins, are zero.
    pub phase: Vec<f64>,
}

/// Windows samples spaced by positive `dt`, then zero-pads to `next_pow2(len)`.
///
/// Normalization is by the original coefficient sum, `len*coherent_gain(len)`,
/// never by the padded length. Padding therefore samples the same normalized
/// finite-record transform on a finer frequency grid. At a tone's frequency,
/// its matching complex exponential retains its amplitude. A real cosine's
/// interior bin is guaranteed to read its peak amplitude when the windowed
/// negative-frequency image contributes zero there; padding alone does not
/// ensure that orthogonality, even at a bin centre of the padded grid.
/// DC and Nyquist use the undoubled magnitude. Empty records and windows with
/// zero coefficient sum return zero amplitudes and phases.
pub fn spectrum(samples: &[f64], dt: f64, window: Window) -> Spectrum {
    let n_fft = next_pow2(samples.len());
    let mut data = vec![C64::new(0.0, 0.0); n_fft];
    let mut weight_sum = 0.0;
    for (i, &sample) in samples.iter().enumerate() {
        let weight = window.coeff(i, samples.len());
        data[i] = C64::new(sample * weight, 0.0);
        weight_sum += weight;
    }
    let mut result = Spectrum {
        df: 1.0 / (n_fft as f64 * dt),
        amp: vec![0.0; n_fft / 2 + 1],
        phase: vec![0.0; n_fft / 2 + 1],
    };
    if weight_sum == 0.0 {
        return result;
    }
    fft(&mut data, false).expect("zero-padded FFT length is a power of two");
    let mut largest: f64 = 0.0;
    for (k, &value) in data[..result.amp.len()].iter().enumerate() {
        let value = value.scale(1.0 / weight_sum);
        let factor = if k == 0 || k == n_fft / 2 { 1.0 } else { 2.0 };
        result.amp[k] = factor * value.norm();
        result.phase[k] = value.arg();
        largest = largest.max(result.amp[k]);
    }
    for (&amp, phase) in result.amp.iter().zip(&mut result.phase) {
        if amp == 0.0 || amp < 1e-12 * largest {
            *phase = 0.0;
        }
    }
    result
}

/// The RMS of a record's content between `lo` and `hi` Hz, both inclusive: Parseval's sum over the
/// [`spectrum`] bins in the band, corrected for the window's noise bandwidth and the zero padding.
/// Content that fills the band reads its RMS whatever the window. A tone reads `amp/sqrt(2)` when
/// the band holds its main lobe; one within a lobe's width of an edge reads only partly. An empty
/// record, or a window whose coefficients sum to zero, reads zero.
pub fn band_rms(samples: &[f64], dt: f64, window: Window, lo: f64, hi: f64) -> f64 {
    let n = samples.len();
    let enbw = window.enbw_bins(n);
    if enbw == 0.0 {
        return 0.0;
    }
    let s = spectrum(samples, dt, window);
    let last = s.amp.len() - 1;
    let sum: f64 = s.amp.iter().enumerate()
        .filter(|&(k, _)| (lo..=hi).contains(&(k as f64 * s.df)))
        .map(|(k, a)| if k == 0 || k == last { a * a } else { a * a / 2.0 })
        .sum();
    (sum * n as f64 / (next_pow2(n) as f64 * enbw)).sqrt()
}

/// Resamples strictly increasing times at `t0 + k*dt` by cubic Hermite interpolation.
///
/// Targets must be nondecreasing (`dt >= 0`); interval lookup is a forward walk.
/// Supplied derivatives are used verbatim. Otherwise, three-point nonuniform
/// parabolas supply derivatives, including one-sided parabolas at the ends;
/// two samples use their secant. Outside the record, values clamp to its ends.
/// An empty record returns zeros; one sample returns its constant value.
/// Panics only when the input slice lengths do not match.
pub fn resample_hermite(
    t: &[f64],
    y: &[f64],
    dydt: Option<&[f64]>,
    t0: f64,
    dt: f64,
    n: usize,
) -> Vec<f64> {
    assert_eq!(t.len(), y.len(), "resampling time/value lengths must match");
    if let Some(slopes) = dydt {
        assert_eq!(
            t.len(),
            slopes.len(),
            "resampling time/derivative lengths must match"
        );
    }
    if t.len() < 2 {
        return vec![y.first().copied().unwrap_or(0.0); n];
    }

    let estimated;
    let slopes = match dydt {
        Some(slopes) => slopes,
        None => {
            estimated = estimate_slopes(t, y);
            &estimated
        }
    };
    let last = t.len() - 1;
    let mut interval = 0;
    let mut result = Vec::with_capacity(n);
    for k in 0..n {
        let target = t0 + k as f64 * dt;
        if target <= t[0] {
            result.push(y[0]);
        } else if target >= t[last] {
            result.push(y[last]);
        } else {
            while interval + 1 < last && target > t[interval + 1] {
                interval += 1;
            }
            let span = t[interval + 1] - t[interval];
            let u = (target - t[interval]) / span;
            let delta = y[interval + 1] - y[interval];
            let left = span * slopes[interval];
            let right = span * slopes[interval + 1];
            result.push(
                y[interval]
                    + u * (left
                        + u * (3.0 * delta - 2.0 * left - right
                            + u * (left + right - 2.0 * delta))),
            );
        }
    }
    result
}

/// Differentiates neighbouring parabolas, with one-sided endpoints or a secant.
fn estimate_slopes(t: &[f64], y: &[f64]) -> Vec<f64> {
    let n = t.len();
    let mut slopes = vec![0.0; n];
    if n == 2 {
        slopes.fill((y[1] - y[0]) / (t[1] - t[0]));
        return slopes;
    }
    for i in 1..n - 1 {
        let before = t[i] - t[i - 1];
        let after = t[i + 1] - t[i];
        let left = (y[i] - y[i - 1]) / before;
        let right = (y[i + 1] - y[i]) / after;
        let curvature = (right - left) / (before + after);
        slopes[i] = left + before * curvature;
        if i == 1 {
            slopes[0] = left - before * curvature;
        }
        if i == n - 2 {
            slopes[n - 1] = right + after * curvature;
        }
    }
    slopes
}

/// DC and peak-amplitude harmonics of one fundamental period.
pub struct Harmonics {
    /// Signed DC value (the mean of the uniform period samples).
    pub dc: f64,
    /// Peak amplitudes through `n_harm`, with `amp[0] = 0`.
    pub amp: Vec<f64>,
    /// Cosine phases relative to time zero, with `phase[0] = 0`.
    pub phase: Vec<f64>,
}

/// Analyzes `[t_last - 1/f0, t_last)` for positive fundamental frequency `f0`.
///
/// The uniform grid excludes its right endpoint and uses a rectangular window.
/// Its size is rounded up to a power of two at least `2*n_harm + 2`.
/// Resampling follows [`resample_hermite`], including endpoint clamping when the
/// record is shorter than a period. Phases describe `A*cos(2*pi*h*f0*t + phase)`
/// in the original time coordinates, not relative to the selected period.
/// An empty record yields zero harmonics. Unrepresentable grid sizes panic.
pub fn fourier(
    t: &[f64],
    y: &[f64],
    dydt: Option<&[f64]>,
    f0: f64,
    n_harm: usize,
    n_points: usize,
) -> Harmonics {
    let minimum = n_harm
        .checked_add(1)
        .and_then(|n| n.checked_mul(2))
        .expect("harmonic count exceeds the largest representable FFT length");
    let n = next_pow2(n_points.max(minimum));
    let period = 1.0 / f0;
    let start = t.last().copied().unwrap_or(0.0) - period;
    let samples = resample_hermite(t, y, dydt, start, period / n as f64, n);
    let mut data: Vec<_> = samples.into_iter().map(|x| C64::new(x, 0.0)).collect();
    fft(&mut data, false).expect("harmonic FFT length is a power of two");
    let mut result = Harmonics {
        dc: data[0].re / n as f64,
        amp: vec![0.0; n_harm + 1],
        phase: vec![0.0; n_harm + 1],
    };
    let start_cycles = (start * f0).rem_euclid(1.0);
    for (h, &value) in data.iter().enumerate().take(n_harm + 1).skip(1) {
        let corrected = value * C64::polar(1.0, -TAU * (h as f64 * start_cycles).rem_euclid(1.0));
        result.amp[h] = value.norm() * (2.0 / n as f64);
        result.phase[h] = corrected.arg();
    }
    result
}

impl Harmonics {
    /// Returns `sqrt(sum_{h>=2} amp[h]^2)/amp[1]`, or zero if the fundamental
    /// is zero or absent. Accumulating with `hypot` avoids intermediate overflow.
    pub fn thd(&self) -> f64 {
        let fundamental = self.amp.get(1).copied().unwrap_or(0.0);
        if fundamental == 0.0 {
            return 0.0;
        }
        self.amp
            .iter()
            .skip(2)
            .fold(0.0_f64, |sum, &a| sum.hypot(a))
            / fundamental
    }
}
