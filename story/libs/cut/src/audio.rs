//! The pure audio curves of a cut's mix: fades, the
//! ducking envelope and integrated loudness (ITU-R BS.1770-4). Gains are
//! functions of time, so a render reads them per sample and a preview at
//! any point; nothing here holds state between calls.

/// The gain of linear fades at `t` for a source sounding on `[start, end)`:
/// it rises from 0 at `start` to 1 after `fade_in` seconds and falls to 0
/// at `end` over the last `fade_out` seconds. A zero fade is a cut.
pub fn fade_gain(t: f64, start: f64, end: f64, fade_in: f64, fade_out: f64) -> f64 {
    let mut gain: f64 = 1.0;
    if fade_in > 0.0 {
        gain = gain.min((t - start) / fade_in);
    }
    if fade_out > 0.0 {
        gain = gain.min((end - t) / fade_out);
    }
    gain.clamp(0.0, 1.0)
}

/// Merge speaking intervals `(start, end)` into sorted, disjoint ones, the
/// form [`duck_amount`] reads.
pub fn merge_intervals(mut intervals: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    intervals.retain(|(start, end)| start.is_finite() && end.is_finite() && end > start);
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(intervals.len());
    for (start, end) in intervals {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// The shortest duck ramp: an "instant" duck (attack or release 0) still
/// ramps over 5 ms, so the gain never steps within a sample (a click).
pub const MIN_RAMP: f64 = 0.005;

/// How far a ducked source is down at `t`, 0 (not at all) to 1 (the full
/// depth), under speech on the sorted, disjoint `intervals`. Each interval
/// ramps in linearly over the `attack` seconds before its start, so the
/// drop is complete when the first word lands, holds while it lasts, and
/// ramps out over `release` after its end; overlapping ramps take the
/// deeper one. Ramps last at least [`MIN_RAMP`]. Continuous in `t`.
pub fn duck_amount(t: f64, intervals: &[(f64, f64)], attack: f64, release: f64) -> f64 {
    if !t.is_finite() {
        return 0.0;
    }
    let (attack, release) = (attack.max(MIN_RAMP), release.max(MIN_RAMP));
    // Only intervals whose attack has begun can pull the level down; their
    // ends increase with their starts, so the ones still releasing are a
    // run at the end of that prefix.
    let begun = intervals.partition_point(|(start, _)| start - attack <= t);
    let mut amount: f64 = 0.0;
    for &(start, end) in intervals[..begun].iter().rev() {
        if end + release <= t && t > end {
            break;
        }
        let level = if t < start {
            (t - (start - attack)) / attack
        } else if t <= end {
            1.0
        } else {
            1.0 - (t - end) / release
        };
        amount = amount.max(level.clamp(0.0, 1.0));
        if amount >= 1.0 {
            break;
        }
    }
    amount
}

/// A level change in dB as a linear gain.
pub fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// The linear gain of a source ducked by `depth_db` (negative) at
/// `amount` (see [`duck_amount`]): the drop is linear in dB.
pub fn duck_gain(depth_db: f64, amount: f64) -> f64 {
    db_to_gain(depth_db * amount.clamp(0.0, 1.0))
}

/// The linear gain that brings a mix measured at `measured` LUFS to
/// `target` LUFS (`output.loudness`). A render applies it to the whole mix
/// after measuring it; peaks past full scale are the renderer's limiter's.
pub fn loudness_gain(measured: f64, target: f64) -> f64 {
    db_to_gain(target - measured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_audio_dsp::loudness::{integrated_lufs, LoudnessMeter};

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn fades_ramp_linearly_and_zero_is_a_cut() {
        assert_eq!(fade_gain(1.0, 1.0, 5.0, 0.0, 0.0), 1.0);
        assert!(close(fade_gain(1.5, 1.0, 5.0, 1.0, 2.0), 0.5, 1e-12));
        assert!(close(fade_gain(4.0, 1.0, 5.0, 1.0, 2.0), 0.5, 1e-12));
        assert_eq!(fade_gain(3.0, 1.0, 5.0, 1.0, 2.0), 1.0);
        assert_eq!(fade_gain(0.5, 1.0, 5.0, 1.0, 2.0), 0.0);
        assert_eq!(fade_gain(5.0, 1.0, 5.0, 1.0, 2.0), 0.0);
        // Fades longer than the source meet in the middle.
        assert!(close(fade_gain(2.0, 1.0, 3.0, 2.0, 2.0), 0.5, 1e-12));
    }

    #[test]
    fn the_duck_is_down_before_the_first_word_and_releases_after_the_last() {
        let speech = merge_intervals(vec![(5.0, 6.0), (2.0, 3.0), (2.5, 3.5)]);
        assert_eq!(speech, [(2.0, 3.5), (5.0, 6.0)]);
        let amount = |t: f64| duck_amount(t, &speech, 0.1, 0.5);
        assert_eq!(amount(0.0), 0.0);
        assert_eq!(amount(1.9), 0.0);
        assert!(close(amount(1.95), 0.5, 1e-9), "halfway through the attack");
        assert_eq!(amount(2.0), 1.0, "fully down when the speech starts");
        assert_eq!(amount(3.5), 1.0);
        assert!(close(amount(3.75), 0.5, 1e-9), "halfway through the release");
        assert_eq!(amount(4.0), 0.0);
        assert!(close(amount(4.95), 0.5, 1e-9), "the next attack");
        assert!(close(amount(6.1), 0.8, 1e-9));
        assert_eq!(amount(f64::NAN), 0.0);
        // A release still running when the next attack starts takes the
        // deeper of the two.
        let close_speech = [(1.0, 2.0), (2.3, 3.0)];
        assert!(close(duck_amount(2.25, &close_speech, 0.1, 0.5), 0.5, 1e-9));
        assert!(close(duck_amount(2.2, &close_speech, 0.1, 0.5), 0.6, 1e-9));
        // The envelope is continuous: no step larger than its ramps allow.
        let mut last = amount(0.0);
        for i in 1..=8000 {
            let now = amount(i as f64 * 0.001);
            assert!((now - last).abs() <= 0.001 / 0.1 + 1e-9, "jump at {}", i as f64 * 0.001);
            last = now;
        }
        // Zero attack and release still ramp over MIN_RAMP: no click.
        assert_eq!(duck_amount(1.994, &speech, 0.0, 0.0), 0.0);
        assert!(close(duck_amount(1.9975, &speech, 0.0, 0.0), 0.5, 1e-9));
        assert_eq!(duck_amount(2.0, &speech, 0.0, 0.0), 1.0);
        assert!(close(duck_amount(3.5025, &speech, 0.0, 0.0), 0.5, 1e-9));
        assert_eq!(duck_amount(3.506, &speech, 0.0, 0.0), 0.0);
    }

    #[test]
    fn duck_gain_is_linear_in_db() {
        assert_eq!(duck_gain(-12.0, 0.0), 1.0);
        assert!(close(duck_gain(-12.0, 1.0), 0.251188643, 1e-8));
        assert!(close(duck_gain(-12.0, 0.5), db_to_gain(-6.0), 1e-12));
    }

    fn sine(rate: u32, seconds: f64, amplitude: f32, left: bool, right: bool) -> Vec<f32> {
        let frames = (rate as f64 * seconds) as usize;
        let mut out = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = amplitude * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate as f64).sin() as f32;
            out.push(if left { v } else { 0.0 });
            out.push(if right { v } else { 0.0 });
        }
        out
    }

    #[test]
    fn a_full_scale_1k_sine_in_one_channel_is_minus_3_lufs() {
        // BS.1770-4: a 0 dBFS 1 kHz sine in one front channel reads -3.01.
        for rate in [48_000, 44_100] {
            let lufs = integrated_lufs(&sine(rate, 5.0, 1.0, true, false), rate).unwrap();
            assert!(close(lufs, -3.01, 0.05), "{rate} Hz: {lufs}");
        }
        let both = integrated_lufs(&sine(48_000, 5.0, 1.0, true, true), 48_000).unwrap();
        assert!(close(both, 0.0, 0.05), "{both}");
        // -20 dB of amplitude is -20 LU.
        let quiet = integrated_lufs(&sine(48_000, 5.0, 0.1, true, true), 48_000).unwrap();
        assert!(close(quiet, -20.0, 0.05), "{quiet}");
    }

    #[test]
    fn gating_ignores_silence_and_the_gain_meets_the_target() {
        let mut signal = sine(48_000, 3.0, 0.1, true, true);
        signal.extend(std::iter::repeat(0.0).take(48_000 * 2 * 10));
        let lufs = integrated_lufs(&signal, 48_000).unwrap();
        // The gates drop the silence; the three blocks that straddle the
        // sine's end still count, a little quieter.
        assert!(close(lufs, -20.0, 0.3), "silence does not pull the loudness down: {lufs}");
        assert!(integrated_lufs(&vec![0.0; 48_000 * 2], 48_000).is_none());
        assert!(integrated_lufs(&sine(48_000, 0.2, 1.0, true, true), 48_000).is_none(), "shorter than one block");
        // Streaming in odd chunks measures the same.
        let mut meter = LoudnessMeter::new(48_000);
        for chunk in signal.chunks(2 * 1237) {
            meter.push(chunk);
        }
        assert!(close(meter.finish().unwrap(), lufs, 1e-9));
        let gain = loudness_gain(lufs, -14.0);
        let louder = signal.iter().map(|v| v * gain as f32).collect::<Vec<_>>();
        assert!(close(integrated_lufs(&louder, 48_000).unwrap(), -14.0, 0.05));
    }
}
