//! Spectral colour, D65, wavelength sampling, RGB fits, LED emission and dispersion.

use crate::oklab::Rgb;
use std::{cell::RefCell, collections::BTreeMap, sync::OnceLock};

pub const LAMBDA_MIN: f32 = 360.0;
pub const LAMBDA_MAX: f32 = 830.0;

// CIE D65 at 10 nm intervals, from 360 to 830 nm.
const D65: [f32; 48] = [
    46.6383, 52.0891, 49.9755, 54.6482, 82.7549, 91.486, 93.4318, 86.6823, 104.865, 117.008, 117.812, 114.861,
    115.923, 108.811, 109.354, 107.802, 104.790, 107.689, 104.405, 104.046, 100.000, 96.3342, 95.788, 88.6856,
    90.0062, 89.5991, 87.6987, 83.2886, 83.6992, 80.0268, 80.2146, 82.2778, 78.2842, 69.7213, 71.6091, 74.349,
    61.604, 69.8856, 75.087, 63.5927, 46.4182, 66.8054, 63.3828, 64.304, 59.4519, 51.959, 57.4406, 60.3125,
];

fn d65_raw(lambda: f32) -> f32 {
    let x = (lambda - 360.0) * 0.1;
    if x <= 0.0 { return D65[0]; }
    if x >= 47.0 { return D65[47]; }
    let i = x as usize;
    let f = x - i as f32;
    D65[i] * (1.0 - f) + D65[i + 1] * f
}

/// A Gaussian lobe with separate widths on either side of its peak.
pub fn lobe(x: f32, mu: f32, s1: f32, s2: f32) -> f32 {
    let t = (x - mu) / if x < mu { s1 } else { s2 };
    (-0.5 * t * t).exp()
}

/// The analytic CIE 1931 two-degree colour-matching fit at a wavelength in nm.
pub fn cie_xyz(lambda: f32) -> [f32; 3] {
    let x = 1.056 * lobe(lambda, 599.8, 37.9, 31.0) + 0.362 * lobe(lambda, 442.0, 16.0, 26.7)
        - 0.065 * lobe(lambda, 501.1, 20.4, 26.2);
    let y = 0.821 * lobe(lambda, 568.8, 46.9, 40.5) + 0.286 * lobe(lambda, 530.9, 16.3, 31.1);
    let z = 1.217 * lobe(lambda, 437.0, 11.8, 36.0) + 0.681 * lobe(lambda, 459.0, 26.0, 13.8);
    [x, y, z]
}

struct Colorimetry {
    d65_scale: f32,
    m: [f32; 9],
}

impl Colorimetry {
    fn get() -> &'static Self {
        static COLOUR: OnceLock<Colorimetry> = OnceLock::new();
        COLOUR.get_or_init(|| {
            let mut xyz = [0.0_f64; 3];
            for l in 360..=830 {
                let c = cie_xyz(l as f32);
                let e = d65_raw(l as f32) as f64;
                for k in 0..3 { xyz[k] += c[k] as f64 * e; }
            }
            let d65_scale = (1.0 / xyz[1]) as f32;
            xyz[0] /= xyz[1];
            xyz[2] /= xyz[1];
            xyz[1] = 1.0;
            let s: [f32; 9] = [
                3.2404542, -1.5371385, -0.4985314, -0.9692660, 1.8760108, 0.0415560,
                0.0556434, -0.2040259, 1.0572252,
            ];
            let mut m = [0.0; 9];
            for r in 0..3 {
                let w = s[r * 3] as f64 * xyz[0] + s[r * 3 + 1] as f64 * xyz[1] + s[r * 3 + 2] as f64 * xyz[2];
                for c in 0..3 { m[r * 3 + c] = (s[r * 3 + c] as f64 / w) as f32; }
            }
            Self { d65_scale, m }
        })
    }
}

/// D65 power, interpolated in nm and normalised to integrated Y = 1.
pub fn d65(lambda: f32) -> f32 {
    d65_raw(lambda) * Colorimetry::get().d65_scale
}

fn matrix_rgb(m: &[f32; 9], c: [f32; 3]) -> Rgb {
    [
        m[0] * c[0] + m[1] * c[1] + m[2] * c[2],
        m[3] * c[0] + m[4] * c[1] + m[5] * c[2],
        m[6] * c[0] + m[7] * c[1] + m[8] * c[2],
    ]
}

/// XYZ to linear sRGB, white-balanced to the integrated D65 fit.
pub fn xyz_to_rgb(xyz: [f32; 3]) -> Rgb {
    matrix_rgb(&Colorimetry::get().m, xyz)
}

/// Linear RGB weight of a wavelength sampled with the given density.
pub fn rgb_weight(lambda: f32, pdf: f32) -> Rgb {
    let weight = 1.0 / pdf.max(1e-12);
    xyz_to_rgb(cie_xyz(lambda)).map(|c| c * weight)
}

/// Sample a visible wavelength in nm from a uniform value in 0..1.
pub fn sample_lambda(u: f32) -> f32 {
    (538.0 - 138.888889 * (0.85691062 - 1.82750197 * u).atanh()).clamp(LAMBDA_MIN, LAMBDA_MAX)
}

/// Probability density per nm of the visible wavelength sampler.
pub fn lambda_pdf(lambda: f32) -> f32 {
    if lambda < LAMBDA_MIN || lambda > LAMBDA_MAX { return 0.0; }
    let c = (0.0072 * (lambda - 538.0)).cosh();
    0.0039398042 / (c * c)
}

/// A mixture of emission-weighted 1 nm bins and the visible wavelength density.
#[derive(Clone, Debug, Default)]
pub struct WavelengthSampler {
    pub cdf: Vec<f32>,
    pub pdf: Vec<f32>,
}

impl WavelengthSampler {
    pub const BINS: usize = (LAMBDA_MAX - LAMBDA_MIN) as usize;

    /// Build the mixture; `share` in 0..1 weights emission, typically 0.75.
    pub fn build(&mut self, emission: impl Fn(f32) -> f32, share: f32) {
        let mut w = vec![0.0_f64; Self::BINS];
        let mut total = 0.0;
        for (i, weight) in w.iter_mut().enumerate() {
            let l = LAMBDA_MIN + i as f32 + 0.5;
            let c = cie_xyz(l);
            *weight = (emission(l) as f64).max(0.0) * (c[0] + c[1] + c[2]) as f64;
            total += *weight;
        }
        self.cdf.clear();
        self.pdf.clear();
        if !(total > 0.0) { return; }
        self.cdf.resize(Self::BINS + 1, 0.0);
        self.pdf.resize(Self::BINS, 0.0);
        let mut acc = 0.0_f64;
        for i in 0..Self::BINS {
            let l = LAMBDA_MIN + i as f32 + 0.5;
            self.pdf[i] = (share as f64 * w[i] / total + ((1.0 - share) * lambda_pdf(l)) as f64) as f32;
            self.cdf[i] = acc as f32;
            acc += self.pdf[i] as f64;
        }
        for i in 0..Self::BINS {
            self.cdf[i] /= acc as f32;
            self.pdf[i] /= acc as f32;
        }
        self.cdf[Self::BINS] = 1.0;
    }

    /// Return a wavelength in nm and its density for a uniform value in 0..1.
    pub fn sample(&self, u: f32) -> (f32, f32) {
        if self.cdf.is_empty() {
            let l = sample_lambda(u);
            return (l, lambda_pdf(l));
        }
        let i = self.cdf.partition_point(|&c| c <= u).saturating_sub(1).min(Self::BINS - 1);
        let f = (u - self.cdf[i]) / (self.cdf[i + 1] - self.cdf[i]).max(1e-12);
        (LAMBDA_MIN + i as f32 + f.clamp(0.0, 1.0), self.pdf[i])
    }
}

/// A scaled sigmoid of a quadratic in normalised wavelength, or a constant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SigmoidSpectrum {
    pub c0: f32,
    pub c1: f32,
    pub c2: f32,
    pub scale: f32,
    pub constant: bool,
    pub value: f32,
}

impl Default for SigmoidSpectrum {
    fn default() -> Self {
        Self { c0: 0.0, c1: 0.0, c2: 0.0, scale: 1.0, constant: true, value: 1.0 }
    }
}

impl SigmoidSpectrum {
    /// Evaluate at a wavelength in nm; emission fits need a D65 multiplier.
    pub fn eval(&self, lambda: f32) -> f32 {
        if self.constant { return self.value; }
        let x = (lambda - LAMBDA_MIN) * (1.0 / (LAMBDA_MAX - LAMBDA_MIN));
        let z = (self.c0 * x + self.c1) * x + self.c2;
        self.scale * (0.5 + 0.5 * z / (1.0 + z * z).sqrt())
    }
}

struct FitTable {
    w: [Rgb; Self::N],
    x: [f32; Self::N],
}

impl FitTable {
    const N: usize = 95;

    fn get() -> &'static Self {
        static TABLE: OnceLock<FitTable> = OnceLock::new();
        TABLE.get_or_init(|| {
            let mut t = Self { w: [[0.0; 3]; Self::N], x: [0.0; Self::N] };
            let mut sum = [0.0; 3];
            for i in 0..Self::N {
                let l = LAMBDA_MIN + 5.0 * i as f32;
                t.x[i] = (l - LAMBDA_MIN) / (LAMBDA_MAX - LAMBDA_MIN);
                t.w[i] = xyz_to_rgb(cie_xyz(l)).map(|c| c * d65(l));
                for k in 0..3 { sum[k] += t.w[i][k]; }
            }
            for w in &mut t.w {
                for k in 0..3 { w[k] /= sum[k]; }
            }
            t
        })
    }
}

fn model(c: [f32; 3]) -> Rgb {
    let t = FitTable::get();
    let mut r = [0.0; 3];
    for i in 0..FitTable::N {
        let z = (c[0] * t.x[i] + c[1]) * t.x[i] + c[2];
        let value = 0.5 + 0.5 * z / (1.0 + z * z).sqrt();
        for k in 0..3 { r[k] += t.w[i][k] * value; }
    }
    r
}

fn determinant(a: &[f64; 9]) -> f64 {
    a[0] * (a[4] * a[8] - a[5] * a[7]) - a[1] * (a[3] * a[8] - a[5] * a[6])
        + a[2] * (a[3] * a[7] - a[4] * a[6])
}

fn solve3(a: [f64; 9], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = determinant(&a);
    if det.abs() < 1e-30 { return None; }
    let mut x = [0.0; 3];
    for k in 0..3 {
        let mut m = a;
        for r in 0..3 { m[r * 3 + k] = b[r]; }
        x[k] = determinant(&m) / det;
    }
    Some(x)
}

fn luminance(c: Rgb) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn residual(m: Rgb, target: Rgb) -> [f64; 3] {
    std::array::from_fn(|k| (m[k] - target[k]) as f64 / (target[k] as f64 + 0.01))
}

fn error(r: [f64; 3]) -> f64 {
    r[0] * r[0] + r[1] * r[1] + r[2] * r[2]
}

fn fit(target: Rgb) -> SigmoidSpectrum {
    let lo = target[0].min(target[1].min(target[2]));
    let hi = target[0].max(target[1].max(target[2]));
    if hi - lo < 1e-5 {
        return SigmoidSpectrum { value: 0.5 * (lo + hi), ..Default::default() };
    }
    let y = luminance(target).clamp(1e-3, 0.999);
    let g = 2.0 * y - 1.0;
    let mut c = [0.0, 0.0, g / (1.0 - g * g).sqrt()];
    let steps = 8;
    for st in 1..=steps {
        let t = target.map(|v| y + (v - y) * (st as f32 / steps as f32));
        let mut lambda = 1e-3_f64;
        let mut r = residual(model(c), t);
        let mut err = error(r);
        for _ in 0..if st == steps { 60 } else { 15 } {
            if !(err > 1e-12) { break; }
            let mut jacobian = [0.0; 9];
            for p in 0..3 {
                let mut cp = c;
                let h = 1e-3 * c[p].abs().max(1.0);
                cp[p] += h;
                let rp = residual(model(cp), t);
                for k in 0..3 { jacobian[k * 3 + p] = (rp[k] - r[k]) / h as f64; }
            }
            let mut a = [0.0; 9];
            let mut b = [0.0; 3];
            for i in 0..3 {
                for k in 0..3 { b[i] -= jacobian[k * 3 + i] * r[k]; }
                for j in 0..3 {
                    for k in 0..3 { a[i * 3 + j] += jacobian[k * 3 + i] * jacobian[k * 3 + j]; }
                }
            }
            let mut improved = false;
            for _ in 0..8 {
                let mut al = a;
                for i in 0..3 { al[i * 4] += lambda * (a[i * 4] + 1e-9); }
                let Some(dx) = solve3(al, b) else {
                    lambda *= 10.0;
                    continue;
                };
                let cn = std::array::from_fn(|k| (c[k] as f64 + dx[k]) as f32);
                let rn = residual(model(cn), t);
                let en = error(rn);
                if en.is_finite() && en < err {
                    c = cn;
                    r = rn;
                    err = en;
                    lambda = (lambda * 0.3).max(1e-7);
                    improved = true;
                    break;
                }
                lambda *= 10.0;
            }
            if !improved { break; }
        }
    }
    SigmoidSpectrum { c0: c[0], c1: c[1], c2: c[2], scale: 1.0, constant: false, value: 1.0 }
}

thread_local! {
    // A fit never holds a lock that another thread needs.
    static FIT_CACHE: RefCell<BTreeMap<[u32; 3], SigmoidSpectrum>> = RefCell::new(BTreeMap::new());
}

/// Fit a reflectance in 0..1 to linear sRGB under D65, clamping the input.
pub fn fit_reflectance(rgb: Rgb) -> SigmoidSpectrum {
    let t = rgb.map(|c| c.max(0.0).min(1.0));
    // Numeric zero has one key, including inputs carrying a negative zero.
    let key = t.map(|c| if c == 0.0 { 0 } else { c.to_bits() });
    FIT_CACHE.with(|cache| {
        if let Some(&s) = cache.borrow().get(&key) { return s; }
        let s = fit(t);
        let mut cache = cache.borrow_mut();
        if cache.len() > 4096 { cache.clear(); }
        cache.insert(key, s);
        s
    })
}

/// Fit a nonnegative linear sRGB emitter; multiply its evaluated shape by D65.
pub fn fit_emission(rgb: Rgb) -> SigmoidSpectrum {
    let rgb = rgb.map(|c| c.max(0.0));
    let m = rgb[0].max(rgb[1].max(rgb[2]));
    let lo = rgb[0].min(rgb[1].min(rgb[2]));
    if m - lo < 1e-6 * m.max(1e-30) {
        return SigmoidSpectrum { value: m, ..Default::default() };
    }
    let k = 1.0 / (2.0 * m);
    let mut s = fit_reflectance(rgb.map(|c| c * k));
    s.scale = 2.0 * m;
    s
}

/// Equally spaced spectral samples in nm, interpolated linearly and zero outside.
#[derive(Clone, Debug, PartialEq)]
pub struct Spectrum {
    pub values: Vec<f32>,
    pub lambda_min: f32,
    pub lambda_step: f32,
}

impl Default for Spectrum {
    fn default() -> Self {
        Self { values: Vec::new(), lambda_min: 360.0, lambda_step: 5.0 }
    }
}

impl Spectrum {
    pub fn is_empty(&self) -> bool { self.values.is_empty() }

    /// Evaluate at a wavelength in nm; empty or invalid spacing gives zero.
    pub fn eval(&self, lambda: f32) -> f32 {
        if self.values.is_empty() || self.lambda_step <= 0.0 { return 0.0; }
        let x = (lambda - self.lambda_min) / self.lambda_step;
        if x < 0.0 || x > (self.values.len() - 1) as f32 { return 0.0; }
        let i = (x as usize).min(self.values.len() - 1);
        if i + 1 >= self.values.len() { return self.values[i]; }
        let f = x - i as f32;
        self.values[i] * (1.0 - f) + self.values[i + 1] * f
    }
}

/// Integrate an emission spectrum to linear sRGB at 1 nm intervals.
pub fn spectrum_to_rgb_emission(s: &Spectrum) -> Rgb {
    let mut r = [0.0; 3];
    for l in 360..=830 {
        let c = xyz_to_rgb(cie_xyz(l as f32));
        let value = s.eval(l as f32);
        for k in 0..3 { r[k] += c[k] * value; }
    }
    r
}

/// Integrate to linear sRGB as emission, or as reflectance lit by D65.
pub fn spectrum_rgb(s: &Spectrum, emission: bool) -> Rgb {
    if emission { return spectrum_to_rgb_emission(s); }
    let mut r = [0.0; 3];
    for l in 360..=830 {
        let c = xyz_to_rgb(cie_xyz(l as f32));
        let value = s.eval(l as f32) * d65(l as f32);
        for k in 0..3 { r[k] += c[k] * value; }
    }
    r
}

/// LED emission kinds, including a blue-pumped phosphor cool white.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Led { CoolWhite, Red, Green, Blue }

/// An LED's emission spectrum, sampled every 5 nm and scaled to luminance 1.
pub fn led_spectrum(which: Led) -> Spectrum {
    let mut peaks: Vec<[f32; 3]> = match which {
        Led::Red => vec![[630.0, 17.0, 1.0]],
        Led::Green => vec![[520.0, 32.0, 1.0]],
        Led::Blue => vec![[467.0, 23.0, 1.0]],
        Led::CoolWhite => vec![[450.0, 20.0, 1.0], [557.0, 118.0, 0.0]],
    };
    let make = |peaks: &[[f32; 3]]| {
        let mut s = Spectrum { lambda_min: LAMBDA_MIN, lambda_step: 5.0, ..Default::default() };
        let mut l = LAMBDA_MIN;
        while l <= LAMBDA_MAX + 0.01 {
            let mut v = 0.0;
            for &[peak, fwhm, weight] in peaks {
                let sg = fwhm / 2.3548;
                let t = (l - peak) / if l < peak { sg * 0.9 } else { sg * 1.1 };
                v += weight * (-0.5 * t * t).exp();
            }
            s.values.push(v);
            l += 5.0;
        }
        s
    };
    if which == Led::CoolWhite {
        let (mut lo, mut hi) = (0.05, 5.0);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            peaks[1][2] = mid;
            let c = spectrum_to_rgb_emission(&make(&peaks));
            if c[2] > c[0] { lo = mid; } else { hi = mid; }
        }
        peaks[1][2] = 0.5 * (lo + hi);
    }
    let mut s = make(&peaks);
    let y = luminance(spectrum_to_rgb_emission(&s));
    if y > 0.0 {
        for v in &mut s.values { *v /= y; }
    }
    s
}

/// Refractive index at nm from nd at 587.6 nm and Abbe number; abbe <= 0 disables dispersion.
pub fn cauchy_ior(nd: f32, abbe: f32, lambda: f32) -> f32 {
    if abbe <= 0.0 { return nd; }
    let b = (nd - 1.0) / abbe / (1.0 / (0.4861 * 0.4861) - 1.0 / (0.6563 * 0.6563));
    let a = nd - b / (0.5876 * 0.5876);
    let l = lambda * 1e-3;
    a + b / (l * l)
}

/// Relative power of a display primary: 0 red, 1 green, 2 blue, at nm.
pub fn display_primary(c: i32, lambda: f32) -> f32 {
    let peak = [612.0, 540.0, 455.0];
    let fwhm = [38.0, 50.0, 26.0];
    let c = c.clamp(0, 2) as usize;
    let sg = fwhm[c] / 2.3548;
    let t = (lambda - peak[c]) / if lambda < peak[c] { sg * 0.9 } else { sg * 1.15 };
    (-0.5 * t * t).exp()
}

/// Display-primary weights for linear sRGB; weights may be negative outside the gamut.
pub fn display_weights(rgb: Rgb) -> Rgb {
    static INVERSE: OnceLock<[f32; 9]> = OnceLock::new();
    let m = INVERSE.get_or_init(|| {
        let mut col = [[0.0; 3]; 3];
        for c in 0..3 {
            for l in 360..=830 {
                let rgb = xyz_to_rgb(cie_xyz(l as f32));
                let p = display_primary(c as i32, l as f32);
                for k in 0..3 { col[c][k] += rgb[k] * p; }
            }
        }
        let [a, d, g] = col[0];
        let [b, e, h] = col[1];
        let [c, f, i] = col[2];
        let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
        let k = 1.0 / det;
        [
            (e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k,
            (f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k,
            (d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k,
        ]
    });
    matrix_rgb(m, rgb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oklab::{to_linear, to_srgb};

    fn tabulate(value: impl Fn(f32) -> f32) -> Spectrum {
        Spectrum { values: (360..=830).map(|l| value(l as f32)).collect(), lambda_min: 360.0, lambda_step: 1.0 }
    }

    #[test]
    fn d65_white_and_spectrum_interpolation() {
        let mut xyz = [0.0; 3];
        for l in 360..=830 {
            let c = cie_xyz(l as f32);
            for k in 0..3 { xyz[k] += c[k] * d65(l as f32); }
        }
        // The spectral colour checks allow 3% relative error.
        let white = [0.95047, 1.0, 1.08883];
        for k in 0..3 {
            assert!((xyz[k] / white[k] - 1.0).abs() < 0.03, "D65 XYZ white point, channel {k}: {xyz:?}");
        }
        assert!((xyz[1] - 1.0).abs() < 1e-5, "D65 integrated luminance: {xyz:?}");
        let rgb = spectrum_to_rgb_emission(&tabulate(d65));
        assert!(rgb.iter().all(|c| (c - 1.0).abs() < 1e-5), "D65 white-balanced RGB: {rgb:?}");
        let mut s = Spectrum { values: vec![0.2, 0.8], lambda_min: 400.0, lambda_step: 10.0 };
        assert_eq!([s.eval(400.0), s.eval(405.0), s.eval(410.0)], [0.2, 0.5, 0.8], "spectrum endpoints and interpolation");
        assert_eq!([s.eval(399.0), s.eval(411.0)], [0.0; 2], "spectrum is zero outside its samples");
        s.lambda_step = 0.0;
        assert_eq!(s.eval(400.0), 0.0, "zero sample spacing gives zero");
        assert_eq!(Spectrum::default().eval(400.0), 0.0, "empty spectrum gives zero");
    }

    #[test]
    fn fitted_colours_round_trip() {
        for c in [
            [0.8, 0.8, 0.8], [0.6, 0.3, 0.1], [0.05, 0.4, 0.1], [0.9, 0.05, 0.05],
            [0.1, 0.2, 0.8], [0.02, 0.02, 0.025], [1.0; 3], [0.95, 0.86, 0.67],
        ] {
            let fit = fit_reflectance(c);
            let tab = tabulate(|l| fit.eval(l));
            let rgb = spectrum_rgb(&tab, false);
            for k in 0..3 {
                assert!((rgb[k] - c[k]).abs() / c[k].max(0.05) < 0.03,
                    "reflectance linear RGB round trip, channel {k}: {c:?} -> {rgb:?}");
            }
            assert!(tab.values.iter().all(|v| (0.0..=1.0).contains(v)), "reflectance bounds for {c:?}");
        }
        for srgb in [[0.6, 0.3, 0.1], [0.2, 0.5, 0.8], [0.0; 3], [0.5; 3], [1.0; 3]] {
            let linear = srgb.map(to_linear);
            let reflectance = fit_reflectance(linear);
            let emission = fit_emission(linear);
            let reflected = spectrum_rgb(&tabulate(|l| reflectance.eval(l)), false).map(to_srgb);
            let emitted = spectrum_to_rgb_emission(&tabulate(|l| emission.eval(l) * d65(l))).map(to_srgb);
            for k in 0..3 {
                assert!((reflected[k] - srgb[k]).abs() < 1e-3,
                    "reflectance sRGB round trip, channel {k}: {srgb:?} -> {reflected:?}");
                assert!((emitted[k] - srgb[k]).abs() < 1e-3,
                    "emission sRGB round trip, channel {k}: {srgb:?} -> {emitted:?}");
            }
        }
    }

    #[test]
    fn wavelength_densities_and_emission_sampling() {
        let integral: f32 = (360..830).map(|l| lambda_pdf(l as f32 + 0.5)).sum();
        assert!((integral - 1.0).abs() < 1e-5, "visible wavelength PDF normalisation: {integral}");
        assert_eq!([lambda_pdf(359.0), lambda_pdf(831.0)], [0.0; 2], "wavelength PDF outside visible range");
        for u in [0.1, 0.5, 0.9] {
            let l = sample_lambda(u);
            let step = (l - LAMBDA_MIN) / 470.0;
            let cdf: f32 = (0..470).map(|i| lambda_pdf(LAMBDA_MIN + (i as f32 + 0.5) * step) * step).sum();
            assert!((cdf - u).abs() < 1e-5, "wavelength inverse CDF at {u}: {l} nm gives {cdf}");
        }
        let mut sampler = WavelengthSampler::default();
        sampler.build(|l| lobe(l, 630.0, 5.0, 5.0), 0.75);
        let integral: f32 = sampler.pdf.iter().sum();
        assert!((integral - 1.0).abs() < 1e-5, "emission wavelength PDF normalisation: {integral}");
        let mut near_peak = 0;
        for i in 0..1000 {
            let (l, p) = sampler.sample((i as f32 + 0.5) / 1000.0);
            assert!((LAMBDA_MIN..=LAMBDA_MAX).contains(&l) && p > 0.0,
                "emission sample has a visible wavelength and positive PDF: {l}, {p}");
            if (620.0..=640.0).contains(&l) { near_peak += 1; }
        }
        assert!(near_peak > 700, "emission sampling concentrates near the LED line: {near_peak}/1000");
        sampler.build(|_| 0.0, 0.75);
        let (l, p) = sampler.sample(0.5);
        assert_eq!((l, p), (sample_lambda(0.5), lambda_pdf(l)), "dark emission restores the base sampler");
    }

    #[test]
    fn led_peaks_luminance_and_hues() {
        let kinds = [(Led::Red, 630.0), (Led::Green, 520.0), (Led::Blue, 467.0), (Led::CoolWhite, 450.0)];
        let mut colours = [[0.0; 3]; 4];
        for (i, (kind, expected_peak)) in kinds.into_iter().enumerate() {
            let s = led_spectrum(kind);
            let peak_index = s.values.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
            let peak = s.lambda_min + peak_index as f32 * s.lambda_step;
            assert!((peak - expected_peak).abs() <= s.lambda_step * 0.5,
                "{kind:?} LED peak: expected {expected_peak} nm within half a sample, got {peak}");
            let rgb = spectrum_to_rgb_emission(&s);
            assert!((luminance(rgb) - 1.0).abs() < 1e-3, "{kind:?} LED unit luminance: {rgb:?}");
            colours[i] = rgb;
        }
        let [r, g, b, w] = colours;
        assert!(r[0] > 3.0 * r[1].max(r[2]), "red LED hue: {r:?}");
        assert!(g[1] > 2.0 * g[0].max(g[2]), "green LED hue: {g:?}");
        assert!(b[2] > 3.0 * b[0].max(b[1]), "blue LED hue: {b:?}");
        assert!((w[0] / w[2] - 1.0).abs() < 0.02, "cool-white LED red/blue balance: {w:?}");
    }

    #[test]
    fn cauchy_dispersion_and_display_colour() {
        let (nd, abbe) = (1.586, 30.0);
        assert!((cauchy_ior(nd, abbe, 587.6) - nd).abs() < 1e-6, "Cauchy index at the d line equals nd");
        let nf = cauchy_ior(nd, abbe, 486.1);
        let nc = cauchy_ior(nd, abbe, 656.3);
        assert!(nf > nd && nd > nc, "Cauchy index decreases with wavelength: {nf}, {nd}, {nc}");
        assert!(((nd - 1.0) / (nf - nc) - abbe).abs() < 1e-3, "Cauchy F/C lines recover the Abbe number");
        assert_eq!(cauchy_ior(nd, 0.0, 450.0), nd, "zero Abbe number disables dispersion");
        assert_eq!(cauchy_ior(nd, -1.0, 650.0), nd, "negative Abbe number disables dispersion");
        let rgb = [0.6, 0.05, 0.02];
        let w = display_weights(rgb);
        let s = tabulate(|l| w[0] * display_primary(0, l) + w[1] * display_primary(1, l) + w[2] * display_primary(2, l));
        let back = spectrum_to_rgb_emission(&s);
        assert!((0..3).all(|k| (back[k] - rgb[k]).abs() < 1e-5), "display primary RGB round trip: {rgb:?} -> {back:?}");
    }
}
