//! The analysis as the features a Motion document reads (M1's
//! `makepad_motion::features`): curves at [`crate::RATE`], words with
//! their line and syllables, the grid, onsets and named sections.

use crate::music::MusicFeatures;
use crate::voice::VoiceFeatures;
use crate::RATE;
use makepad_motion as motion;

fn curve(values: &[f32]) -> motion::FeatureCurve {
    motion::FeatureCurve::new(0.0, RATE, values.to_vec())
}

pub fn music(m: &MusicFeatures) -> motion::MusicFeatures {
    let first_down = m.downbeats.first().and_then(|down| m.beats.iter().position(|b| (b - down).abs() < 1e-3)).unwrap_or(0);
    motion::MusicFeatures {
        bpm: m.bpm,
        beats: m.beats.clone(),
        beats_per_bar: 4,
        downbeat: first_down as u32,
        onsets: m.onsets.iter().map(|o| motion::Onset { t: o.t, strength: o.strength as f64, name: None }).collect(),
        energy: curve(&m.energy),
        bands: (0..6).map(|band| curve(&m.bands.iter().map(|b| b[band]).collect::<Vec<f32>>())).collect(),
        sections: m.sections.iter().map(|s| motion::Section { name: s.kind.name().to_string(), start: s.start, end: s.end }).collect(),
        signal: None,
    }
}

/// [`music`] with the analysed sound itself (`mono` at `rate`, sample 0 at
/// second 0 of the analysis' clock), for documents that read its spectrum
/// and waveform (`music(@m).spectrum(t, n)`, `.wave(t, n, window)`).
pub fn music_with_sound(m: &MusicFeatures, mono: std::sync::Arc<[f32]>, rate: f64) -> motion::MusicFeatures {
    motion::MusicFeatures { signal: Some(motion::AudioSignal::new(0.0, rate, mono)), ..music(m) }
}

/// `lines`: each voiceover line's `(start, end)` on the same clock as the
/// words (a word belongs to the line its start falls in).
pub fn voice(v: &VoiceFeatures, lines: &[(f64, f64)]) -> motion::VoiceFeatures {
    let words = v
        .words
        .iter()
        .map(|word| {
            let line = lines.iter().position(|(start, end)| word.start >= start - 1e-6 && word.start < *end).unwrap_or_else(|| lines.iter().rposition(|(start, _)| *start <= word.start).unwrap_or(0));
            // Syllables: the word's span cut halfway between its nuclei.
            let nuclei: Vec<f64> = v.syllables.iter().copied().filter(|t| *t >= word.start && *t < word.end).collect();
            let syllables = if nuclei.len() < 2 {
                Vec::new()
            } else {
                let mut cuts = vec![word.start];
                cuts.extend(nuclei.windows(2).map(|pair| (pair[0] + pair[1]) * 0.5));
                cuts.push(word.end);
                cuts.windows(2).map(|pair| (pair[0], pair[1])).collect()
            };
            motion::FeatureWord { text: word.text.clone(), start: word.start, end: word.end, emphasis: word.emphasis as f64, line: line as u32, syllables }
        })
        .collect();
    let phones = v.phones.iter().map(|p| motion::FeaturePhone { text: p.text.clone(), start: p.start, end: p.end }).collect();
    motion::VoiceFeatures { words, lines: lines.to_vec(), amplitude: curve(&v.envelope), pitch: curve(&v.pitch_hz), pauses: v.pauses.clone(), phones }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::{Onset, Section, SectionKind};
    use crate::voice::{Phone, WordFeature};

    #[test]
    fn features_convert_for_motion_documents() {
        let m = MusicFeatures {
            duration: 4.0,
            bpm: 120.0,
            beats: vec![0.5, 1.0, 1.5, 2.0],
            downbeats: vec![1.0],
            onsets: vec![Onset { t: 1.0, strength: 0.5 }],
            energy: vec![0.0, 1.0],
            bands: vec![[0.5; 6]; 2],
            sections: vec![Section { kind: SectionKind::Drop, start: 1.0, end: 4.0, energy: 0.9, nth: 1 }],
            ..MusicFeatures::default()
        };
        let mm = music(&m);
        assert_eq!(mm.downbeat, 1);
        assert_eq!(mm.bands.len(), 6);
        assert_eq!(mm.energy.sample(0.005), 0.5);
        assert_eq!(mm.sections[0].name, "drop");
        let v = VoiceFeatures {
            duration: 2.0,
            envelope: vec![0.5],
            pitch_hz: vec![100.0],
            pauses: vec![],
            syllables: vec![0.1, 0.3, 1.2],
            words: vec![
                WordFeature { text: "hello".into(), start: 0.0, end: 0.5, loudness_db: -20.0, pitch_hz: 100.0, emphasis: 0.4 },
                WordFeature { text: "there".into(), start: 1.0, end: 1.5, loudness_db: -18.0, pitch_hz: 110.0, emphasis: 0.7 },
            ],
            phones: vec![Phone { text: "h".into(), start: 0.0, end: 0.1 }],
        };
        let vv = voice(&v, &[(0.0, 0.6), (1.0, 1.6)]);
        assert_eq!(vv.words[0].syllables, vec![(0.0, 0.2), (0.2, 0.5)]);
        assert_eq!(vv.words[1].line, 1);
        assert!(vv.words[1].syllables.is_empty());
        assert_eq!(vv.phones, vec![motion::FeaturePhone { text: "h".into(), start: 0.0, end: 0.1 }], "the take's phones pass through");
    }
}
