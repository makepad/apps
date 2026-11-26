//! Crop keys for showing an app run on a device screen of another shape
//! (motion3d `Device{crop: …}`, `Seq3D{crop: …}`): the run's focus track
//! (`run.json`) through the reframe camera, so a
//! 16:9 run on a phone follows the action instead of stretching or
//! showing only its centre.

use makepad_story_cut::reframe::{keys_splash, solve, FocusSample, ReframeInput, ReframeParams, Rect, SafeZone};

/// A run's focus track: per-frame samples, the frame size and the fps.
pub struct RunFocus {
    pub track: Vec<FocusSample>,
    pub size: (f64, f64),
    pub fps: f64,
    pub frames: u64,
}

pub fn read_focus(bytes: &[u8]) -> Result<RunFocus, String> {
    use makepad_strict_json::Value;
    let record = makepad_strict_json::parse_depth(bytes, 64).map_err(|e| e.to_string())?;
    let number = |v: Option<&Value>| match v {
        Some(Value::Int(i)) => Some(*i as f64),
        Some(Value::F64(f)) => Some(*f),
        _ => None,
    };
    let output = record.get("output").ok_or("no `output`")?;
    let size = (number(output.get("width")).ok_or("no output width")?, number(output.get("height")).ok_or("no output height")?);
    let fps = number(output.get("fps")).filter(|f| *f > 0.0).ok_or("no output fps")?;
    let frames = number(record.get("frames")).unwrap_or(0.0).max(0.0) as u64;
    let mut track = Vec::new();
    if let Some(Value::Arr(runs)) = record.get("focus").and_then(|f| f.get("frames")) {
        for run in runs {
            let (Some(from), Some(to)) = (number(run.get("from")), number(run.get("to"))) else { continue };
            let mut rects = Vec::new();
            let mut typing = false;
            if let Some(Value::Arr(items)) = run.get("rects") {
                for item in items {
                    let Value::Arr(v) = item else { continue };
                    let n = |i: usize| number(v.get(i));
                    let (Some(x), Some(y), Some(w), Some(h)) = (n(0), n(1), n(2), n(3)) else { continue };
                    rects.push((Rect::new(x, y, w, h), n(4).unwrap_or(1.0)));
                    typing |= matches!(v.get(5), Some(Value::Str(s)) if s == "type");
                }
            }
            for frame in from as u64..to as u64 {
                track.push(FocusSample { frame, rects: rects.clone(), typing });
            }
        }
    }
    Ok(RunFocus { track, size, fps, frames })
}

/// `keys(...)` of `[x, y, w, h]` crops (run pixels, run seconds) for a
/// screen of `aspect` (width / height). Without `zoom` the crop keeps the
/// largest size that fits (the camera pans); with it the reframe camera
/// may also zoom in on the action, down to `min_width` run pixels.
pub fn screen_crop(focus: &RunFocus, aspect: f64, zoom: Option<f64>) -> Result<String, String> {
    let (sw, sh) = focus.size;
    let full = if sw / sh > aspect { (sh * aspect, sh) } else { (sw, sw / aspect) };
    let output = match zoom {
        Some(min_width) => (min_width.clamp(1.0, full.0), min_width.clamp(1.0, full.0) / aspect),
        None => full,
    };
    let input = ReframeInput {
        track: &focus.track,
        source: focus.size,
        output,
        fps: focus.fps,
        frames: focus.frames,
        safe: SafeZone::None,
        pinned: &[],
        params: ReframeParams::default(),
    };
    let solution = solve(&input).map_err(|e| e.to_string())?;
    Ok(keys_splash(&solution.keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_run_on_a_phone_pans_to_the_action() {
        let json = br#"{"output":{"width":1920,"height":1080,"fps":30},"frames":60,
            "focus":{"events":[],"frames":[{"from":0,"to":30,"rects":[[100,400,100,100,1,"click"]]},{"from":30,"to":60,"rects":[[1700,400,100,100,1,"click"]]}]}}"#;
        let focus = read_focus(json).unwrap();
        assert_eq!(focus.track.len(), 60);
        let keys = screen_crop(&focus, 9.0 / 16.0, None).unwrap();
        assert!(keys.starts_with("keys(["), "{keys}");
        // Full height and the phone's width (608): at the left, then a cut to the right.
        assert!(keys.contains(", [0, 0, 608, "), "{keys}");
        assert!(keys.contains(", [1313, 0, 608, ") && keys.contains("@hold"), "{keys}");
    }
}
