//! Fab's own purge-volume model for filament changes: how much of the new
//! filament must run through a nozzle to clear the old colour.
//!
//! Going from dark to light costs far more than light to dark; hue and
//! saturation distance add on top; the nozzle's internal volume is the
//! floor. Calibration prints are expected to tune `DARK_TO_LIGHT` and
//! `CHROMA`; the user's own project values override the model.

const BASE: f64 = 120.0;
const DARK_TO_LIGHT: f64 = 560.0;
const CHROMA: f64 = 230.0;
const MAX: f64 = 900.0;

fn rgb(c: &str) -> (f64, f64, f64) {
    let h = c.trim().trim_start_matches('#');
    if h.len() < 6 {
        return (0.8, 0.8, 0.8);
    }
    let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map(|x| x as f64 / 255.0).unwrap_or(0.8);
    (v(0), v(2), v(4))
}

fn luminance((r, g, b): (f64, f64, f64)) -> f64 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn chroma_dist(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    // Distance of the colours with their luminance taken out.
    let (la, lb) = (luminance(a), luminance(b));
    let d = ((a.0 - la) - (b.0 - lb)).powi(2) + ((a.1 - la) - (b.1 - lb)).powi(2) + ((a.2 - la) - (b.2 - lb)).powi(2);
    d.sqrt()
}

/// Purge volume (mm³) from colour `from` to colour `to`.
pub fn purge_volume(from: &str, to: &str) -> f64 {
    if from.eq_ignore_ascii_case(to) {
        return 0.0;
    }
    let (a, b) = (rgb(from), rgb(to));
    let dl = (luminance(b) - luminance(a)).max(0.0);
    let v = BASE + DARK_TO_LIGHT * dl.powf(0.8) + CHROMA * chroma_dist(a, b);
    v.min(MAX)
}

/// The flush matrix in the profile layout: per nozzle an N×N block,
/// row-major (from, to).
pub fn matrix(colours: &[String], nozzles: usize) -> Vec<f64> {
    let n = colours.len();
    let mut out = Vec::with_capacity(nozzles * n * n);
    for _ in 0..nozzles {
        for i in 0..n {
            for j in 0..n {
                out.push(if i == j { 0.0 } else { purge_volume(&colours[i], &colours[j]).round() });
            }
        }
    }
    out
}
