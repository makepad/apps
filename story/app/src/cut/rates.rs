//! Frame rates a cut is made or exported at: the presets (film, PAL, NTSC
//! and their fractional rates, high rates) and any other the person types.
//! Every rate renders natively: a cut's frame n is drawn at t = n / fps
//! on locked time.

/// The presets: what the pickers list, in order.
pub const RATES: [(&str, f64); 12] = [
    ("23.976", 24000.0 / 1001.0),
    ("24", 24.0),
    ("25", 25.0),
    ("29.97", 30000.0 / 1001.0),
    ("30", 30.0),
    ("48", 48.0),
    ("50", 50.0),
    ("59.94", 60000.0 / 1001.0),
    ("60", 60.0),
    ("90", 90.0),
    ("120", 120.0),
    ("144", 144.0),
];

/// What a new cut is made at.
pub const DEFAULT_FPS: f64 = 60.0;

/// The preset `fps` is, if any.
pub fn preset(fps: f64) -> Option<usize> {
    RATES.iter().position(|(_, rate)| (rate - fps).abs() < 2e-3)
}

/// `fps` as people write it: "60", "59.94", "23.976", "37.5".
pub fn label(fps: f64) -> String {
    match preset(fps) {
        Some(index) => RATES[index].0.to_string(),
        None => {
            let text = format!("{fps:.3}");
            text.trim_end_matches('0').trim_end_matches('.').to_string()
        }
    }
}

/// A typed rate: "72", "59.94", "60000/1001" or "60 fps"; 1 to 1000.
pub fn parse(text: &str) -> Option<f64> {
    let text = text.trim().trim_end_matches("fps").trim();
    let fps = match text.split_once('/') {
        Some((num, den)) => num.trim().parse::<f64>().ok()? / den.trim().parse::<f64>().ok().filter(|den| *den > 0.0)?,
        None => text.parse::<f64>().ok()?,
    };
    // A rate typed as 23.976 or 59.94 is the exact NTSC one.
    let fps = preset(fps).map_or(fps, |index| RATES[index].1);
    (fps.is_finite() && (1.0..=1000.0).contains(&fps)).then_some(fps)
}

/// The value written into the cut: an NTSC rate with enough digits for
/// the renderer to know it (fps_ratio maps it back to n*1000/1001).
pub fn written(fps: f64) -> f64 {
    (fps * 1e6).round() / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_story_edit_render::audio::{fps_ratio, frame_count};

    #[test]
    fn presets_parse_and_label_round_trip() {
        for (text, fps) in RATES {
            assert_eq!(parse(text), Some(fps), "{text}");
            assert_eq!(label(fps), text);
        }
        assert_eq!(parse("60000/1001"), Some(60000.0 / 1001.0));
        assert_eq!(parse(" 72 fps"), Some(72.0));
        assert_eq!(label(72.0), "72");
        assert_eq!(label(37.5), "37.5");
        assert_eq!(parse("0"), None);
        assert_eq!(parse("abc"), None);
        assert_eq!(parse("5000"), None);
    }

    /// What the renderer makes of each written rate: the exact ratio, and
    /// an exact frame count for an 8 s edit.
    #[test]
    fn written_rates_are_exact_for_the_renderer() {
        let cases = [(24.0, (24, 1), 192), (60.0, (60, 1), 480), (120.0, (120, 1), 960), (144.0, (144, 1), 1152),
            (24000.0 / 1001.0, (24000, 1001), 192), (30000.0 / 1001.0, (30000, 1001), 240), (60000.0 / 1001.0, (60000, 1001), 480)];
        for (fps, ratio, frames) in cases {
            let ratio_now = fps_ratio(written(fps));
            assert_eq!(ratio_now, ratio, "{fps}");
            assert_eq!(frame_count(8.0, ratio_now), frames, "{fps}");
        }
    }
}
