//! Worker-side waveform analysis, extracted from the donor Stage console.
//!
//! No codecs, widgets, threads, or retained PCM: the caller supplies decoded
//! stereo frames and may discard each stem immediately after analysis. Only
//! the mixed track sets waveform height. Frequency bands and measured stem
//! shares determine its color. Both views can upload these same pyramids.

mod pyramid;

pub use pyramid::{build_pyramid, levels_for, stem_column_shares, Reduce, WaveLevel, WavePyramid, MAX_LEVELS, TEX_WIDTH};

use std::{f32::consts::PI, fmt};

pub const COLS_PER_SECOND: f64 = 100.0;
pub const WAVE_CURVE: f32 = 0.62;
/// Same decoded-frame bound as Stage's local audio decoder.
pub const MAX_FRAMES: usize = 48_000 * 60 * 15;
/// At 8 kHz the decoder's frame bound spans ninety minutes.
pub const MAX_COLUMNS: usize = 540_000;
pub const MIN_SAMPLE_RATE: u32 = 8_000;
pub const MAX_SAMPLE_RATE: u32 = 384_000;
/// Small codec-padding differences are allowed, without extending coverage.
pub const STEM_DURATION_TOLERANCE: f64 = 0.1;
const REFERENCE_PERCENTILE: f64 = 0.995;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveError {
    Cancelled,
    Empty,
    InvalidSampleRate(u32),
    TooLong,
    InvalidStem(usize),
    StemDurationMismatch(usize),
}

impl fmt::Display for WaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("waveform analysis cancelled"),
            Self::Empty => f.write_str("waveform source is empty"),
            Self::InvalidSampleRate(rate) => write!(f, "unsupported waveform sample rate {rate}"),
            Self::TooLong => f.write_str("waveform source exceeds the analysis limit"),
            Self::InvalidStem(stem) => write!(f, "invalid waveform envelope for stem {stem}"),
            Self::StemDurationMismatch(stem) => write!(f, "waveform stem {stem} duration differs from the mixed track"),
        }
    }
}

impl std::error::Error for WaveError {}

/// Prepared CPU textures. Wrap in an `Arc` when passing to the UI worker channel.
#[derive(Clone, Debug, PartialEq)]
pub struct WaveData {
    pub duration_secs: f64,
    pub cols_per_second: f64,
    /// Logical RGBA channels: low, mid, high, absolute mixed-track level.
    pub bands: WavePyramid,
    /// Logical RGBA channels: vocals, drums, bass, other. Same layout as bands.
    /// All-zero columns are uncovered and must use frequency colors instead.
    pub stems: Option<WavePyramid>,
}

/// Compact measured RMS at exactly 100 columns per second, including a partial
/// final column. This owns no PCM and is independent of the source sample rate.
#[derive(Clone, Debug, PartialEq)]
pub struct StemEnvelope {
    pub duration_secs: f64,
    pub rms: Vec<f32>,
}

/// Analyze one stem, allowing its PCM to be released before decoding the next.
/// Stereo channel energies are averaged, so anti-phase audio remains visible.
pub fn analyze_stem(
    frames: &[[i16; 2]],
    sample_rate: u32,
    abort: &mut impl FnMut() -> bool,
) -> Result<StemEnvelope, WaveError> {
    cancelled(abort)?;
    let grid = Grid::new(frames.len(), sample_rate)?;
    let mut rms = Vec::with_capacity(grid.columns);
    for column in 0..grid.columns {
        cancelled(abort)?;
        let (start, end) = grid.span(column);
        let mut energy = 0.0f64;
        for frame in &frames[start..end] {
            let left = frame[0] as f64 / 32_768.0;
            let right = frame[1] as f64 / 32_768.0;
            energy += 0.5 * (left * left + right * right);
        }
        rms.push((energy / (end - start) as f64).sqrt() as f32);
    }
    cancelled(abort)?;
    Ok(StemEnvelope { duration_secs: grid.duration_secs(), rms })
}

/// Analyze the mixed track and optionally attach four already measured stems,
/// ordered vocals, drums, bass, other. Call on a worker; it checks cancellation
/// at every 10 ms hop and between bounded normalization and packing passes.
///
/// The caller must establish common source revision and time origin. Durations
/// may differ by at most 100 ms for codec padding. A stem column is published
/// only where all four stems cover the mixed track's entire column.
pub fn analyze_mix(
    frames: &[[i16; 2]],
    sample_rate: u32,
    stems: Option<&[StemEnvelope; 4]>,
    abort: &mut impl FnMut() -> bool,
) -> Result<WaveData, WaveError> {
    cancelled(abort)?;
    let grid = Grid::new(frames.len(), sample_rate)?;
    if let Some(stems) = stems {
        validate_stems(stems, grid.duration_secs(), abort)?;
    }
    let envelopes = build_envelopes(frames, &grid, abort)?;
    let columns = build_tiles(&envelopes, abort)?;
    // Keep only the finished texture when constructing the optional stem one.
    drop(envelopes);
    let bands = build_pyramid(&columns, Reduce::BandsOfTheLouder, abort)?;
    drop(columns);
    let stems = match stems {
        Some(stems) => {
            let columns = stem_columns(stems, &grid, abort)?;
            Some(build_pyramid(&columns, Reduce::PerChannel, abort)?)
        }
        None => None,
    };
    cancelled(abort)?;
    Ok(WaveData { duration_secs: grid.duration_secs(), cols_per_second: COLS_PER_SECOND, bands, stems })
}

fn cancelled(abort: &mut impl FnMut() -> bool) -> Result<(), WaveError> {
    if abort() { Err(WaveError::Cancelled) } else { Ok(()) }
}

struct Grid {
    frames: usize,
    sample_rate: u32,
    columns: usize,
}

impl Grid {
    fn new(frames: usize, sample_rate: u32) -> Result<Self, WaveError> {
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(WaveError::InvalidSampleRate(sample_rate));
        }
        if frames == 0 { return Err(WaveError::Empty); }
        if frames > MAX_FRAMES { return Err(WaveError::TooLong); }
        // Integer boundaries keep different sample rates on the same time grid.
        let columns = ((frames as u64 * 100).div_ceil(sample_rate as u64)) as usize;
        if columns > MAX_COLUMNS { return Err(WaveError::TooLong); }
        Ok(Self { frames, sample_rate, columns })
    }

    fn duration_secs(&self) -> f64 { self.frames as f64 / self.sample_rate as f64 }

    fn span(&self, column: usize) -> (usize, usize) {
        let start = (column as u64 * self.sample_rate as u64 / 100) as usize;
        let end = (((column + 1) as u64 * self.sample_rate as u64 / 100) as usize).min(self.frames);
        (start, end)
    }
}

/// Donor 200/2000 Hz one-pole split, applied independently to both channels.
#[derive(Clone, Copy)]
struct OnePole {
    alpha: f32,
    state: f32,
}

impl OnePole {
    fn new(cutoff: f32, sample_rate: f32) -> Self {
        let cutoff = cutoff.min(sample_rate * 0.45);
        Self { alpha: 1.0 - (-2.0 * PI * cutoff / sample_rate).exp(), state: 0.0 }
    }

    fn process(&mut self, value: f32) -> f32 {
        self.state += self.alpha * (value - self.state);
        self.state
    }
}

struct Envelopes {
    band_rms: Vec<[f32; 3]>,
    peak: Vec<f32>,
}

fn build_envelopes(
    frames: &[[i16; 2]],
    grid: &Grid,
    abort: &mut impl FnMut() -> bool,
) -> Result<Envelopes, WaveError> {
    let mut band_rms = Vec::with_capacity(grid.columns);
    let mut peak = Vec::with_capacity(grid.columns);
    let mut low = [OnePole::new(200.0, grid.sample_rate as f32); 2];
    let mut mid = [OnePole::new(2_000.0, grid.sample_rate as f32); 2];
    for column in 0..grid.columns {
        cancelled(abort)?;
        let (start, end) = grid.span(column);
        let mut sums = [0.0f64; 3];
        let mut hop_peak = 0.0f32;
        for frame in &frames[start..end] {
            for channel in 0..2 {
                let value = frame[channel] as f32 / 32_768.0;
                let low_band = low[channel].process(value);
                let mid_band = mid[channel].process(value) - low_band;
                let high_band = value - low[channel].state - mid_band;
                for (sum, band) in sums.iter_mut().zip([low_band, mid_band, high_band]) {
                    *sum += band as f64 * band as f64;
                }
                hop_peak = hop_peak.max(value.abs());
            }
        }
        let inverse = 0.5 / (end - start) as f64;
        band_rms.push(sums.map(|sum| (sum * inverse).sqrt() as f32));
        peak.push(hop_peak);
    }
    cancelled(abort)?;
    Ok(Envelopes { band_rms, peak })
}

/// The donor's track-wide 99.5th-percentile normalization. Selecting that
/// element gives the same reference as sorting, with bounded linear work.
fn track_scale(values: &mut [f32], abort: &mut impl FnMut() -> bool) -> Result<f32, WaveError> {
    cancelled(abort)?;
    if values.is_empty() { return Ok(0.0); }
    let index = ((values.len() as f64 * REFERENCE_PERCENTILE) as usize).min(values.len() - 1);
    let (_, reference, _) = values.select_nth_unstable_by(index, |a, b| a.total_cmp(b));
    let reference = *reference;
    cancelled(abort)?;
    Ok(if reference > 1e-6 { 1.0 / reference } else { 0.0 })
}

fn hop_level(peak: f32, rms: [f32; 3]) -> f32 {
    let broadband = (rms[0] * rms[0] + rms[1] * rms[1] + rms[2] * rms[2]).sqrt();
    0.5 * peak + 0.5 * broadband
}

fn build_tiles(envelopes: &Envelopes, abort: &mut impl FnMut() -> bool) -> Result<Vec<[u8; 4]>, WaveError> {
    let mut scratch = Vec::with_capacity(envelopes.band_rms.len());
    let mut band_scale = [0.0; 3];
    for (band, scale) in band_scale.iter_mut().enumerate() {
        scratch.clear();
        for (index, rms) in envelopes.band_rms.iter().enumerate() {
            if index % 1024 == 0 { cancelled(abort)?; }
            scratch.push(rms[band]);
        }
        *scale = track_scale(&mut scratch, abort)?;
    }
    scratch.clear();
    for (index, (peak, rms)) in envelopes.peak.iter().zip(&envelopes.band_rms).enumerate() {
        if index % 1024 == 0 { cancelled(abort)?; }
        scratch.push(hop_level(*peak, *rms));
    }
    let level_scale = track_scale(&mut scratch, abort)?;
    drop(scratch);
    let mut columns = Vec::with_capacity(envelopes.band_rms.len());
    for (index, (peak, rms)) in envelopes.peak.iter().zip(&envelopes.band_rms).enumerate() {
        if index % 1024 == 0 { cancelled(abort)?; }
        let mut column = [0u8; 4];
        for band in 0..3 {
            column[band] = ((rms[band] * band_scale[band]).clamp(0.0, 1.0).powf(WAVE_CURVE) * 255.0) as u8;
        }
        column[3] = ((hop_level(*peak, *rms) * level_scale).clamp(0.0, 1.0).powf(WAVE_CURVE) * 255.0) as u8;
        columns.push(column);
    }
    cancelled(abort)?;
    Ok(columns)
}

fn validate_stems(
    stems: &[StemEnvelope; 4],
    duration: f64,
    abort: &mut impl FnMut() -> bool,
) -> Result<(), WaveError> {
    for (stem, envelope) in stems.iter().enumerate() {
        cancelled(abort)?;
        if !envelope.duration_secs.is_finite() || envelope.duration_secs <= 0.0
            || envelope.duration_secs > MAX_COLUMNS as f64 / COLS_PER_SECOND
            || envelope.rms.is_empty() || envelope.rms.len() > MAX_COLUMNS
        {
            return Err(WaveError::InvalidStem(stem));
        }
        // Permit floating-point round-off at exact 10 ms boundaries only.
        let expected = (envelope.duration_secs * COLS_PER_SECOND - 1e-8).ceil() as usize;
        if envelope.rms.len() != expected {
            return Err(WaveError::InvalidStem(stem));
        }
        if (envelope.duration_secs - duration).abs() > STEM_DURATION_TOLERANCE + 1e-9 {
            return Err(WaveError::StemDurationMismatch(stem));
        }
        for (column, value) in envelope.rms.iter().enumerate() {
            if column % 1024 == 0 { cancelled(abort)?; }
            if !value.is_finite() || *value < 0.0 || *value > 1.0 {
                return Err(WaveError::InvalidStem(stem));
            }
        }
    }
    Ok(())
}

fn stem_columns(
    stems: &[StemEnvelope; 4],
    grid: &Grid,
    abort: &mut impl FnMut() -> bool,
) -> Result<Vec<[u8; 4]>, WaveError> {
    let mut columns = Vec::with_capacity(grid.columns);
    let covered_until = stems.iter().map(|stem| stem.duration_secs).fold(f64::INFINITY, f64::min);
    for column in 0..grid.columns {
        if column % 1024 == 0 { cancelled(abort)?; }
        let column_end = ((column + 1) as f64 / COLS_PER_SECOND).min(grid.duration_secs());
        if column_end > covered_until + 1e-9 || stems.iter().any(|stem| column >= stem.rms.len()) {
            columns.push([0; 4]);
        } else {
            columns.push(stem_column_shares(std::array::from_fn(|stem| stems[stem].rms[column] as f64)));
        }
    }
    cancelled(abort)?;
    Ok(columns)
}
