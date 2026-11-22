use super::{near, Fault, Rule};

fn walk(stream: &[u8], pixels: usize, mut run: impl FnMut(u8, usize)) -> Result<(), String> {
    let mut offset = 0;
    let mut total = 0usize;
    while offset < stream.len() {
        let byte = stream[offset];
        offset += 1;
        let level = byte >> 4;
        let mut count = (byte & 15) as usize;
        if level == 0 || level == 15 {
            let low = stream.get(offset).ok_or_else(|| format!("run at byte {} is truncated", offset - 1))?;
            count = (count << 8) | *low as usize;
            offset += 1;
        }
        if count == 0 { return Err(format!("run ending at byte {offset} has zero count")); }
        if count > pixels - total {
            return Err(format!("run ending at byte {offset} reaches beyond {pixels} pixels from {total} by {count}"));
        }
        total += count;
        run(level, count);
    }
    if total != pixels { return Err(format!("stream fills {total} pixels, expected {pixels}")); }
    Ok(())
}

/// Decode a run stream to row-major grey levels, requiring exactly `pixels` values.
pub fn decode_runs(stream: &[u8], pixels: usize) -> Result<Vec<u8>, String> {
    walk(stream, pixels, |_, _| {})?;
    let mut levels = Vec::new();
    levels.try_reserve_exact(pixels).map_err(|e| format!("cannot allocate {pixels} pixels: {e}"))?;
    walk(stream, pixels, |level, count| levels.resize(levels.len() + count, level))?;
    Ok(levels)
}

pub(super) fn check_area(stream: &[u8], pixels: usize, pitch: [f64; 2], expected: f32) -> Result<(), Fault> {
    let mut weighted = 0.0;
    walk(stream, pixels, |level, count| weighted += level as f64 * count as f64)
        .map_err(|e| Fault::new(Rule::Runs, e))?;
    let area = weighted / 15.0 * pitch[0] * pitch[1];
    if !near(area, expected as f64, (expected as f64).abs() * 0.005 + 1e-6) {
        return Err(Fault::new(Rule::LitArea, format!("run area {area}, scene area {expected} mm²")));
    }
    Ok(())
}
