//! The export's timing: frames of a cut as exact sample ranges of its
//! 48 kHz mix ([`makepad_story_cut::mix`]), so the sound is pushed
//! in step with the pictures.

use makepad_story_cut::mix::SAMPLE_RATE;

pub const CHANNELS: u16 = 2;

/// An output frame rate as an exact ratio: integral rates are `n/1`, the
/// NTSC family `n*1000/1001`.
pub fn fps_ratio(fps: f64) -> (u32, u32) {
    let whole = fps.round();
    if (fps - whole).abs() < 1e-6 {
        return (whole.max(1.0) as u32, 1);
    }
    let ntsc = (fps * 1001.0 / 1000.0).round();
    if (fps - ntsc * 1000.0 / 1001.0).abs() < 1e-4 {
        return (ntsc as u32 * 1000, 1001);
    }
    ((fps * 1000.0).round().max(1.0) as u32, 1000)
}

/// Frames of a cut of `duration` seconds: frame `n` shows `t = n / fps`
/// for every `t` in `[0, duration)`.
pub fn frame_count(duration: f64, fps: (u32, u32)) -> u64 {
    let exact = duration * fps.0 as f64 / fps.1 as f64;
    // A duration that is a whole number of frames up to float noise.
    (exact - 1e-6).ceil().max(0.0) as u64
}

/// The cut time of frame `n`.
pub fn frame_time(n: u64, fps: (u32, u32)) -> f64 {
    n as f64 * fps.1 as f64 / fps.0 as f64
}

/// The first output sample of frame `n`; frame `n` carries the samples up
/// to the first of `n + 1`, so the sound never drifts from the pictures.
pub fn frame_sample(n: u64, fps: (u32, u32)) -> u64 {
    (n as u128 * SAMPLE_RATE as u128 * fps.1 as u128 / fps.0 as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_stage_audio_media::pcm::TrackPcm;
    use makepad_story_cut::mix::AudioMix;
    use makepad_story_cut::{AssetRef, CutDoc, MediaRef};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn tone(freq: f64, rate: u32, seconds: f64, amplitude: f64) -> Arc<TrackPcm> {
        let frames = (seconds * rate as f64) as usize;
        let v = |i: usize| ((i as f64 / rate as f64 * freq * std::f64::consts::TAU).sin() * amplitude * 32767.0) as i16;
        Arc::new(TrackPcm { frames: (0..frames).map(|i| [v(i), v(i)]).collect(), sample_rate: rate })
    }

    const A: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
    const A_REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

    fn doc_with(audio: &str, output: &str) -> Arc<CutDoc> {
        let source = format!(
            "use mod.flow.*\nuse mod.cut.*\n{audio}\nlet title = Title{{text: \"t\"}}\n\
             let out = CutOutput{{frame: title.out(@frame) audio: [conn(1, music.out(@audio))]}}\n\
             Flow{{\n kind: @cut\n output: {{width: 64 height: 36 fps: 30 {output}}}\n duration: 2.0\n music, title, out\n}}\n"
        );
        Arc::new(CutDoc::load(&source).unwrap_or_else(|errors| panic!("{errors:?}")))
    }

    fn mix(doc: Arc<CutDoc>, pcm: Arc<TrackPcm>) -> AudioMix {
        let media = MediaRef::Asset(AssetRef { id: A.into(), rev: A_REV.into() });
        AudioMix::new(doc, HashMap::from([(media, pcm)]))
    }


    #[test]
    fn frames_and_samples_tile_the_edit_exactly() {
        assert_eq!(fps_ratio(60.0), (60, 1));
        assert_eq!(fps_ratio(29.97), (30000, 1001));
        assert_eq!(frame_count(17.0, (60, 1)), 1020);
        assert_eq!(frame_count(1.0 / 3.0, (30, 1)), 10);
        assert_eq!(frame_count(0.34, (30, 1)), 11);
        let ntsc = fps_ratio(29.97);
        // Frame chunks cover every sample once, with no rounding drift.
        let total: u64 = (0..3000).map(|n| frame_sample(n + 1, ntsc) - frame_sample(n, ntsc)).sum();
        assert_eq!(total, frame_sample(3000, ntsc));
        assert_eq!(frame_sample(30000, ntsc), 48_000 * 1001);
    }

    /// The export's sound through the audio engine is the mix, sample for
    /// sample, in the export's frame-sized pieces, and the same run after run.
    #[test]
    fn the_engine_mixdown_is_the_mix_exactly() {
        let fixtures = [
            format!("let music = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") in: 0.3137 span: [0.1003, 1.8] gain: keys([0.0, 0.2], [1.5, 1.0]) pan: 0.3}}"),
            format!("let music = Clip{{src: asset(\"{A}\", \"{A_REV}\") in: 0.3137 speed: 1.37 audio: true span: [0.0, 2.0]}}"),
        ];
        for (k, audio) in fixtures.iter().enumerate() {
            let mut mixer = mix(doc_with(audio, "loudness: -16"), tone(441.3, 44_100, 5.0, 0.9));
            mixer.normalize(96_000);
            let mixer = Arc::new(mixer);
            let render = || {
                let mut down = crate::mixdown::Mixdown::new(mixer.clone()).unwrap();
                let mut out = Vec::new();
                for n in 0..60 {
                    let (a, b) = (frame_sample(n, (30, 1)), frame_sample(n + 1, (30, 1)));
                    out.extend(down.mix(a, (b - a) as usize));
                }
                out
            };
            let engine = render();
            assert_eq!(engine, mixer.mix(0, 96_000), "fixture {k}: the engine's render is not the mix");
            assert_eq!(engine, render(), "fixture {k}: two renders differ");
            assert!(engine.iter().any(|v| *v != 0));
            // A segment of the export starts mid-edit.
            let mut down = crate::mixdown::Mixdown::new(mixer.clone()).unwrap();
            let from = frame_sample(30, (30, 1));
            assert_eq!(down.mix(from, 4_000), mixer.mix(from, 4_000), "fixture {k}: a later start");
        }
    }
}
