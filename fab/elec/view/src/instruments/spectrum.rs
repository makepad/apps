//! Uniformly resampled, window-corrected spectra drawn through the phosphor column raster.

use super::{phosphor::{Hits, PhosphorMap, Stroke}, scope::{FftSettings, Measurement}};
use fab_elec_sim::{fft::{fft, next_pow2, Window}, num::C64};

/// Resolution follows the device plot, including plots wider than the initial card.
pub fn fft_size(width: u32) -> usize { next_pow2((width as usize * 2).max(256)) }

/// The horizontal coordinate in plot pixels; bin one is the left edge on a log axis.
pub fn bin_x(bin: f64, n: usize, width: u32, log: bool) -> f64 {
    let last = (n / 2) as f64;
    if log { bin.max(1.0).ln() / last.ln() * width as f64 } else { bin / last * width as f64 }
}

/// Copied from the last completed window, without retaining or allocating a spectrum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectrumMeasurements {
    pub peak_f: f64,
    pub peak_dbv: f64,
    /// The largest resolved peak outside the main peak's lobe (frequency, dBV); none when no other peak resolves.
    pub second: Option<(f64, f64)>,
    /// Harmonics 2 through 5 of the main peak: (Hz, dBc relative to the peak).
    /// None outside the spectrum or when the window's main lobe contains no bin above
    /// the noise floor (currently 60 dB below the peak, as no floor is estimated).
    pub harmonics: [Option<(f64, f64)>; 4],
    /// Transform length of this completed stroke, for marker mapping.
    pub n: usize,
    /// Uniform sample rate in Hz of this completed stroke, for marker mapping.
    pub rate: f64,
    /// The window's equivalent noise bandwidth: bin spacing times the window's noise bandwidth in bins.
    pub rbw: f64,
    pub span: f64,
}

impl SpectrumMeasurements {
    /// The same owned row representation used by time-domain measurement publication.
    pub fn rows(self) -> Vec<Measurement> {
        let second = self.second.map(|(f, dbv)| [("2nd f", f, "Hz"), ("2nd", dbv, "dBV")]);
        let harmonics = ["H2", "H3", "H4", "H5"].into_iter().zip(self.harmonics)
            .filter_map(|(name, harmonic)| harmonic.map(|(_, dbc)| (name, dbc, "dBc")));
        [("peak f", self.peak_f, "Hz"), ("peak", self.peak_dbv, "dBV")].into_iter().chain(second.into_iter().flatten()).chain([("RBW", self.rbw, "Hz"), ("span", self.span, "Hz")])
            .chain(harmonics)
            .map(|(name, value, unit)| Measurement::exact(0, name, value, unit)).collect()
    }
}

/// One completed lane result, including a rejected gate, carried in the existing frame vector.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpectrumResult {
    pub measurements: Option<SpectrumMeasurements>,
    pub gate_too_short: bool,
}

impl SpectrumResult {
    /// Empty gated captures publish a note through the same row path as spectral answers.
    pub fn rows(self) -> Vec<Measurement> {
        if self.gate_too_short { vec![Measurement::exact(0, "gate too short", 1.0, "")] }
        else { self.measurements.map_or_else(Vec::new, SpectrumMeasurements::rows) }
    }
}

/// Pooled with acquisition strokes. All transform storage survives finish/begin.
#[derive(Default)]
pub struct SpectrumStroke {
    slots: Vec<f64>,
    scratch: Vec<C64>,
    coeff: Vec<f64>,
    sum: f64,
    /// The window's equivalent noise bandwidth in bins: n * sum(w^2) / sum(w)^2.
    enbw: f64,
    window: Option<Window>,
    bins: Vec<f64>,
    raster: Stroke,
    settings: FftSettings,
    plot: (u32, u32),
    start: f64,
    record: f64,
    filled: usize,
    last: Option<f64>,
    active: bool,
    measurements: Option<SpectrumMeasurements>,
    missing: bool,
    gate: Option<(f64, f64, f64)>,
    gate_samples: u8,
    gate_previous: Option<(f64, f32)>,
    gate_covered: (bool, bool),
    gate_short: bool,
}

impl SpectrumStroke {
    pub fn begin(&mut self, start: f64, record: f64, plot: (u32, u32), settings: FftSettings) {
        assert!(record.is_finite() && record > 0.0, "FFT record must be finite and positive");
        let n = fft_size(plot.0);
        if self.slots.len() != n || self.window != Some(settings.window) {
            self.slots.resize(n, 0.0);
            self.scratch.resize(n, C64::new(0.0, 0.0));
            self.coeff.resize(n, 0.0);
            self.bins.resize(n / 2 + 1, 0.0);
            self.sum = 0.0;
            let mut squares = 0.0;
            for (i, c) in self.coeff.iter_mut().enumerate() { *c = settings.window.coeff(i, n); self.sum += *c; squares += *c * *c; }
            self.enbw = n as f64 * squares / (self.sum * self.sum);
            self.window = Some(settings.window);
        }
        self.start = start;
        self.record = record;
        self.plot = plot;
        self.settings = settings;
        self.filled = 0;
        self.last = None;
        self.active = true;
        self.measurements = None;
        self.missing = false;
        self.gate = None;
        self.gate_samples = 0;
        self.gate_previous = None;
        self.gate_covered = (false, false);
        self.gate_short = false;
    }

    /// The capture start and optional gate are in trigger-relative seconds. Only samples in [t0, t1)
    /// contribute, with the first/last in-gate values clamping the resampled tails. Both
    /// endpoints must be covered by the capture and at least four source samples must remain.
    pub fn begin_gated(&mut self, start: f64, record: f64, trigger: f64, gate: Option<(f64, f64)>, plot: (u32, u32), settings: FftSettings) {
        self.begin(trigger + start, record, plot, settings);
        if let Some((t0, t1)) = gate {
            self.gate = Some((trigger, t0, t1));
            self.gate_short = !trigger.is_finite() || !t0.is_finite() || !t1.is_finite()
                || !(t1 - t0).is_finite() || t0 >= t1 || t0 < start || t1 > start + record;
            if self.gate_short { self.active = false; return; }
            self.start = t0;
            self.record = t1 - t0;
        }
    }

    fn gate_point(&mut self, t: f64, y: f32) {
        let Some((trigger, t0, t1)) = self.gate else { return };
        if !self.active || !t.is_finite() { return; }
        let t = t - trigger;
        self.gate_covered.0 |= t <= t0;
        self.gate_covered.1 |= t >= t1;
        if t < t0 || t >= t1 || self.gate_previous.is_some_and(|(previous, _)| t <= previous) { return; }
        if !y.is_finite() { self.missing = true; return; }
        self.gate_samples = (self.gate_samples + 1).min(4);
        while self.filled < self.slots.len() {
            let at = self.start + self.filled as f64 * self.record / self.slots.len() as f64;
            if at >= t { break; }
            self.slots[self.filled] = self.gate_previous.map_or(f64::from(y), |(previous, value)| {
                f64::from(value) + ((at - previous) / (t - previous)).clamp(0.0, 1.0) * (f64::from(y) - f64::from(value))
            });
            self.filled += 1;
        }
        self.gate_previous = Some((t, y));
        self.last = Some(f64::from(y));
    }

    /// Fill each uniform slot once, using the accepted pair's linear interpolant on [t0, t1).
    pub fn segment(&mut self, t0: f64, y0: f32, t1: f64, y1: f32) {
        if !self.active || !t0.is_finite() || !t1.is_finite() || t1 <= t0 { return; }
        if self.gate.is_some() { self.gate_point(t0, y0); self.gate_point(t1, y1); return; }
        if !y0.is_finite() || !y1.is_finite() { self.missing = true; return; }
        self.point(t0, y0);
        let n = self.slots.len();
        while self.filled < n {
            let t = self.start + self.filled as f64 * self.record / n as f64;
            if t >= t1 { break; }
            self.slots[self.filled] = f64::from(y0) + ((t - t0) / (t1 - t0)).clamp(0.0, 1.0) * (f64::from(y1) - f64::from(y0));
            self.filled += 1;
        }
        self.last = Some(f64::from(y1));
    }

    /// Slots before the first accepted sample take its value; the final tail clamps likewise.
    pub fn point(&mut self, t: f64, y: f32) {
        if self.gate.is_some() { self.gate_point(t, y); return; }
        if !y.is_finite() { self.missing = true; return; }
        if !self.active || self.last.is_some() { return; }
        self.last = Some(f64::from(y));
        while self.filled < self.slots.len() && self.start + self.filled as f64 * self.record / (self.slots.len() as f64) < t {
            self.slots[self.filled] = f64::from(y);
            self.filled += 1;
        }
    }

    pub fn finish(&mut self, hits: &mut Hits) {
        self.finish_optional(Some(hits));
    }

    /// Hidden lanes still answer spectral measurements, without creating any pixel segments.
    pub(super) fn finish_optional(&mut self, hits: Option<&mut Hits>) {
        if !self.active { return; }
        self.active = false;
        if self.gate.is_some() && (self.gate_samples < 4 || self.gate_covered != (true, true)) { self.gate_short = true; return; }
        if self.missing { return; }
        let Some(last) = self.last else { return };
        self.slots[self.filled..].fill(last);
        let n = self.slots.len();
        for ((value, sample), weight) in self.scratch.iter_mut().zip(&self.slots).zip(&self.coeff) { *value = C64::new(sample * weight, 0.0); }
        fft(&mut self.scratch, false).expect("spectrum length is a power of two");
        for (k, dbv) in self.bins.iter_mut().enumerate() {
            let factor = if k == 0 || k == n / 2 { 1.0 } else { 2.0 };
            *dbv = 20.0 * (self.scratch[k].norm() * factor / self.sum).max(1e-12).log10();
        }
        let first = usize::from(self.settings.log_freq);
        let mut peak = first;
        for k in first..self.bins.len() { if self.bins[k] > self.bins[peak] { peak = k; } }
        // The peak's lobe runs down both sides until the spectrum rises again; a neighbour inside it is not a peak.
        let (mut lo, mut hi) = (peak, peak);
        while lo > first && self.bins[lo - 1] < self.bins[lo] { lo -= 1; }
        while hi + 1 < self.bins.len() && self.bins[hi + 1] < self.bins[hi] { hi += 1; }
        let bins = &self.bins;
        let resolved = |k: usize| (k == first || bins[k] >= bins[k - 1]) && (k + 1 == bins.len() || bins[k] >= bins[k + 1]);
        let second = (first..lo).chain(hi + 1..bins.len()).filter(|&k| resolved(k)).max_by(|&a, &b| bins[a].total_cmp(&bins[b]));
        let df = 1.0 / self.record;
        let peak_f = peak as f64 * df;
        // Half the null-to-null main-lobe width, in bins, of each periodic window.
        // These four searches visit at most 11 bins each, independent of the FFT length.
        let half_lobe = match self.settings.window { Window::Rect => 1.0, Window::Hann | Window::Hamming => 2.0, Window::BlackmanHarris => 4.0, Window::FlatTop => 5.0 };
        // Bins clamped to the numerical amplitude floor are never evidence of a harmonic.
        let floor = (bins[peak] - 60.0).max(-240.0);
        let mut harmonics = [None; 4];
        if peak_f > 0.0 {
            for (i, harmonic) in harmonics.iter_mut().enumerate() {
                let target = (i + 2) as f64 * peak_f / df;
                if target > (bins.len() - 1) as f64 { continue; }
                let lo = (target - half_lobe).ceil().max(first as f64) as usize;
                let hi = (target + half_lobe).floor().min((bins.len() - 1) as f64) as usize;
                if let Some(k) = (lo..=hi).max_by(|&a, &b| bins[a].total_cmp(&bins[b])) {
                    if bins[k] > floor { *harmonic = Some((k as f64 * df, bins[k] - bins[peak])); }
                }
            }
        }
        self.measurements = Some(SpectrumMeasurements { peak_f, peak_dbv: bins[peak], second: second.map(|k| (k as f64 * df, bins[k])), harmonics, n, rate: n as f64 * df, rbw: df * self.enbw, span: n as f64 * df * 0.5 });
        let Some(hits) = hits else { return };
        // Values are negative pixel y so Stroke's upward value axis becomes an identity in pixels.
        self.raster.begin(&PhosphorMap { w: self.plot.0, h: self.plot.1, t_left: 0.0, t_px: 1.0, v_top: 0.0, v_px: 1.0 }, 0.0);
        let y = |dbv: f64| ((dbv - self.settings.ref_dbv) * self.plot.1 as f64 / (10.0 * self.settings.db_per_div)) as f32;
        for k in first + 1..self.bins.len() {
            self.raster.segment(bin_x((k - 1) as f64, n, self.plot.0, self.settings.log_freq), y(self.bins[k - 1]), bin_x(k as f64, n, self.plot.0, self.settings.log_freq), y(self.bins[k]));
        }
        self.raster.finish(hits);
    }

    pub fn measurements(&self) -> Option<SpectrumMeasurements> { self.measurements }
    pub fn result(&self) -> SpectrumResult { SpectrumResult { measurements: self.measurements, gate_too_short: self.gate_short } }
    pub fn bins(&self) -> &[f64] { if self.measurements.is_some() { &self.bins } else { &[] } }
}
