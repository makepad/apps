//! CPU form of the donor `music_view` texture pyramid and stem-share encoding.

use crate::{cancelled, WaveError, MAX_COLUMNS, WAVE_CURVE};

pub const TEX_WIDTH: usize = 2048;
pub const MAX_LEVELS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveLevel {
    pub base_row: usize,
    pub cols: usize,
}

/// Logical RGBA columns packed as BGRA u32: `(alpha<<24)|(red<<16)|(green<<8)|blue`.
/// Every level begins on its own row; unused texels are zero, never source data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WavePyramid {
    pub data: Vec<u32>,
    pub width: usize,
    pub height: usize,
    pub levels: Vec<WaveLevel>,
}

/// The levels bracketing the requested source columns per display pixel,
/// their respective source-column strides, and their interpolation factor.
pub fn levels_for(
    levels: &[WaveLevel],
    cols_per_px: f64,
) -> (WaveLevel, f64, WaveLevel, f64, f64) {
    if levels.is_empty() {
        let empty = WaveLevel { base_row: 0, cols: 0 };
        return (empty, 1.0, empty, 1.0, 0.0);
    }
    let last = levels.len().min(MAX_LEVELS) - 1;
    let cols_per_px = if cols_per_px.is_finite() { cols_per_px } else { 1.0 };
    let lod = if cols_per_px > 1.0 { cols_per_px.log2() } else { 0.0 };
    let lo_index = (lod.floor().max(0.0) as usize).min(last);
    let hi_index = (lo_index + 1).min(last);
    let blend = if hi_index == lo_index { 0.0 } else { (lod - lo_index as f64).clamp(0.0, 1.0) };
    (
        levels[lo_index],
        (1u64 << lo_index) as f64,
        levels[hi_index],
        (1u64 << hi_index) as f64,
        blend,
    )
}

impl WavePyramid {
    pub fn levels_for(&self, cols_per_px: f64) -> (WaveLevel, f64, WaveLevel, f64, f64) {
        levels_for(&self.levels, cols_per_px)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reduce {
    /// Stem shares: keep each channel's maximum independently.
    PerChannel,
    /// Mixed bands: preserve the complete color of the louder column.
    /// Ties retain the first column, exactly as in the donor.
    BandsOfTheLouder,
}

fn reduce_once(
    below: &[[u8; 4]],
    reduce: Reduce,
    abort: &mut impl FnMut() -> bool,
) -> Result<Vec<[u8; 4]>, WaveError> {
    cancelled(abort)?;
    let mut level = Vec::with_capacity(below.len().div_ceil(2));
    for (index, pair) in below.chunks(2).enumerate() {
        if index % 1024 == 0 { cancelled(abort)?; }
        let mut column = pair[0];
        if let Some(second) = pair.get(1) {
            match reduce {
                Reduce::PerChannel => {
                    for channel in 0..4 {
                        column[channel] = column[channel].max(second[channel]);
                    }
                }
                Reduce::BandsOfTheLouder => {
                    if second[3] > column[3] { column = *second; }
                    column[3] = pair[0][3].max(second[3]);
                }
            }
        }
        level.push(column);
    }
    cancelled(abort)?;
    Ok(level)
}

/// The donor's row-stacked pyramid, prepared without creating a GPU texture.
/// Both reduction rules use identical layout for the same source column count.
pub fn build_pyramid(
    columns: &[[u8; 4]],
    reduce: Reduce,
    abort: &mut impl FnMut() -> bool,
) -> Result<WavePyramid, WaveError> {
    cancelled(abort)?;
    if columns.is_empty() { return Err(WaveError::Empty); }
    if columns.len() > MAX_COLUMNS { return Err(WaveError::TooLong); }
    let width = TEX_WIDTH.min(columns.len());
    let mut pyramid = vec![columns.to_vec()];
    while pyramid.last().is_some_and(|level| level.len() > 1) && pyramid.len() < MAX_LEVELS {
        let below = pyramid.last().expect("pyramid always has a source level");
        pyramid.push(reduce_once(below, reduce, abort)?);
    }
    cancelled(abort)?;
    let mut levels = Vec::with_capacity(pyramid.len());
    let mut height = 0usize;
    for level in &pyramid {
        levels.push(WaveLevel { base_row: height, cols: level.len() });
        height += level.len().div_ceil(width);
    }
    let mut data = vec![0u32; width * height];
    for (level, info) in pyramid.iter().zip(&levels) {
        for (index, column) in level.iter().enumerate() {
            if index % 1024 == 0 { cancelled(abort)?; }
            let row = info.base_row + index / width;
            let texel = row * width + index % width;
            data[texel] = ((column[3] as u32) << 24)
                | ((column[0] as u32) << 16)
                | ((column[1] as u32) << 8)
                | column[2] as u32;
        }
    }
    cancelled(abort)?;
    Ok(WavePyramid { data, width, height, levels })
}

/// Donor stem-color proportions. The mixed track's level remains the only
/// height source; these bytes merely divide that height among the four colors.
pub fn stem_column_shares(rms: [f64; 4]) -> [u8; 4] {
    let top = rms.iter().fold(0.0f64, |a, b| a.max(*b));
    if top <= 1e-9 {
        // Measured silence is covered. Zero channels instead mean unavailable.
        return [1; 4];
    }
    let mut out = [0u8; 4];
    for (lane, value) in out.iter_mut().enumerate() {
        let share = (rms[lane] / top).clamp(0.0, 1.0).powf(WAVE_CURVE as f64);
        *value = (share * 255.0).round() as u8;
    }
    out
}
