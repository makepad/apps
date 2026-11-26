//! Frame arithmetic of the script calls (EDITS.md §6.2): every call moves
//! the virtual clock in whole frames, so its timing is a pure function of
//! its options and the frame rate.

/// Frames `seconds` last at `fps`, rounded to the nearest whole frame.
pub fn frames_for(seconds: f64, fps: u32) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * fps as f64).round() as u64
}

/// The cursor's position at the end of each of `frames` frames moving from
/// `from` to `to`: eased in and out, the last one exactly at `to`. Zero
/// frames is one jump.
pub fn move_path(from: (f64, f64), to: (f64, f64), frames: u64) -> Vec<(f64, f64)> {
    let frames = frames.max(1);
    (1..=frames)
        .map(|i| {
            let t = i as f64 / frames as f64;
            let e = t * t * (3.0 - 2.0 * t);
            if i == frames {
                to
            } else {
                (from.0 + (to.0 - from.0) * e, from.1 + (to.1 - from.1) * e)
            }
        })
        .collect()
}

/// The frame a time in seconds falls on: the nearest whole frame.
pub fn frame_at(seconds: f64, fps: u32) -> u64 {
    frames_for(seconds, fps)
}

/// `total` split over `parts` steps that add up to it exactly.
pub fn spread(total: f64, parts: u64) -> Vec<f64> {
    let parts = parts.max(1);
    let mut out = Vec::with_capacity(parts as usize);
    let mut done = 0.0;
    for i in 1..=parts {
        let at = total * i as f64 / parts as f64;
        out.push(at - done);
        done = at;
    }
    out
}

/// A window size in points keeps the output's aspect ratio.
pub fn same_aspect(width: f64, height: f64, out_width: u32, out_height: u32) -> bool {
    width > 0.0 && height > 0.0 && (width * out_height as f64 - height * out_width as f64).abs() <= 1e-6 * width * out_height as f64
}

/// The key a character is typed with when it is not text: newline and tab.
pub fn control_key(c: char) -> Option<&'static str> {
    match c {
        '\n' => Some("ReturnKey"),
        '\t' => Some("Tab"),
        _ => None,
    }
}

/// A target path matches a widget path exactly or as a suffix of whole
/// segments (the remote's `q=path=` rule): `new_note` finds
/// `main.new_note`, `note` does not.
pub fn path_matches(target: &str, path: &str) -> bool {
    path == target || (path.len() > target.len() && path.ends_with(target) && path.as_bytes()[path.len() - target.len() - 1] == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_round_to_whole_frames() {
        assert_eq!(frames_for(0.5, 60), 30);
        assert_eq!(frames_for(1.5, 5), 8);
        assert_eq!(frames_for(0.0, 60), 0);
        assert_eq!(frames_for(-1.0, 60), 0);
        assert_eq!(frames_for(f64::NAN, 60), 0);
    }

    #[test]
    fn a_move_eases_and_ends_on_the_target() {
        let path = move_path((0.0, 0.0), (100.0, 50.0), 4);
        assert_eq!(path.len(), 4);
        assert_eq!(path[3], (100.0, 50.0));
        assert!(path.windows(2).all(|w| w[0].0 <= w[1].0));
        // Slow at both ends: the middle step is the largest.
        let steps: Vec<f64> = std::iter::once(path[0].0).chain(path.windows(2).map(|w| w[1].0 - w[0].0)).collect();
        assert!(steps[1] > steps[0] && steps[2] > steps[3]);
        assert_eq!(move_path((1.0, 1.0), (2.0, 2.0), 0), vec![(2.0, 2.0)]);
    }

    #[test]
    fn a_schedule_in_seconds_lands_within_a_frame_at_any_rate() {
        // Twelve 25/60 s calls: at 60 fps and at 5 fps every boundary is
        // within half a frame of its time.
        for fps in [60, 5] {
            for i in 1..=12 {
                let t = i as f64 * 25.0 / 60.0;
                assert!((frame_at(t, fps) as f64 / fps as f64 - t).abs() <= 0.5 / fps as f64 + 1e-9);
            }
        }
    }

    #[test]
    fn a_spread_adds_up_exactly() {
        let parts = spread(-120.0, 7);
        assert_eq!(parts.len(), 7);
        assert!((parts.iter().sum::<f64>() + 120.0).abs() < 1e-9);
    }

    #[test]
    fn aspect_and_paths() {
        assert!(same_aspect(180.0, 320.0, 1080, 1920));
        assert!(!same_aspect(200.0, 320.0, 1080, 1920));
        assert!(path_matches("new_note", "main.new_note"));
        assert!(path_matches("main.new_note", "main.new_note"));
        assert!(!path_matches("note", "main.new_note"));
        assert!(!path_matches("main", "main.new_note"));
    }
}
