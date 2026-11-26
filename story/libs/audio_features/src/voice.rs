//! Voice features: envelope, pitch, pauses, syllable nuclei, and per word
//! its loudness, pitch and emphasis.

use crate::frames::{measure, normalize, smooth};
use crate::RATE;

/// A timed word (from the voiceover's word timings, §8.2).
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// A timed phoneme or syllable, when the TTS reports them (§9 P8).
#[derive(Clone, Debug, PartialEq)]
pub struct Phone {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WordFeature {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Mean level while it is spoken, dBFS.
    pub loudness_db: f32,
    /// Median voiced pitch, Hz (0 when unvoiced).
    pub pitch_hz: f32,
    /// How much it stands out from the line around it, 0..1: louder,
    /// higher and longer words score higher.
    pub emphasis: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoiceFeatures {
    pub duration: f64,
    /// Per frame at [`RATE`]: amplitude 0..1 and pitch in Hz (0 = unvoiced).
    pub envelope: Vec<f32>,
    pub pitch_hz: Vec<f32>,
    /// Silences of at least 150 ms: (start, end).
    pub pauses: Vec<(f64, f64)>,
    /// Syllable nuclei (vowel peaks), seconds.
    pub syllables: Vec<f64>,
    pub words: Vec<WordFeature>,
    pub phones: Vec<Phone>,
}

/// YIN pitch per frame on a signal already at `sample_rate`.
pub fn pitch_track(mono: &[f32], sample_rate: f64, rate: f64) -> Vec<f32> {
    // Work at ~16 kHz: plenty for 60–500 Hz and a quarter of the cost.
    let step = ((sample_rate / 16_000.0).floor() as usize).max(1);
    let x: Vec<f32> = mono.chunks(step).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect();
    let sr = sample_rate / step as f64;
    let window = (0.040 * sr) as usize;
    let (min_lag, max_lag) = ((sr / 500.0) as usize, (sr / 60.0) as usize);
    let hop = sr / rate;
    let frames = (mono.len() as f64 / sample_rate * rate).ceil() as usize;
    let mut out = Vec::with_capacity(frames);
    let mut d = vec![0.0f32; max_lag + 1];
    for f in 0..frames {
        let start = (f as f64 * hop) as usize;
        if start + window + max_lag >= x.len() {
            out.push(0.0);
            continue;
        }
        let frame = &x[start..start + window + max_lag];
        let energy: f32 = frame[..window].iter().map(|v| v * v).sum::<f32>() / window as f32;
        if energy < 1e-6 {
            out.push(0.0);
            continue;
        }
        for (lag, slot) in d.iter_mut().enumerate().skip(1) {
            let mut sum = 0.0;
            for i in 0..window {
                let diff = frame[i] - frame[i + lag];
                sum += diff * diff;
            }
            *slot = sum;
        }
        // Cumulative mean normalised difference, then the first dip under
        // the threshold, refined to its local minimum.
        let mut running = 0.0;
        let mut found = None;
        let mut cmnd = vec![1.0f32; max_lag + 1];
        for lag in 1..=max_lag {
            running += d[lag];
            cmnd[lag] = if running > 0.0 { d[lag] * lag as f32 / running } else { 1.0 };
        }
        for lag in min_lag.max(2)..max_lag {
            if cmnd[lag] < 0.15 {
                let mut best = lag;
                while best + 1 < max_lag && cmnd[best + 1] < cmnd[best] {
                    best += 1;
                }
                found = Some(best);
                break;
            }
        }
        out.push(match found {
            Some(lag) => {
                // Parabolic interpolation around the minimum.
                let (a, b, c) = (cmnd[lag - 1], cmnd[lag], cmnd[lag + 1]);
                let shift = if a + c - 2.0 * b != 0.0 { 0.5 * (a - c) / (a + c - 2.0 * b) } else { 0.0 };
                (sr / (lag as f64 + shift as f64)) as f32
            }
            None => 0.0,
        });
    }
    out
}

pub fn analyze_voice(mono: &[f32], sample_rate: f64, words: &[Word], phones: &[Phone]) -> VoiceFeatures {
    let sample_rate = sample_rate.max(1.0);
    let duration = mono.len() as f64 / sample_rate;
    let frames = measure(mono, sample_rate, RATE);
    let envelope = smooth(&normalize(&frames.loudness_db, 0.02, 0.995), 1);
    let pitch_hz = pitch_track(mono, sample_rate, RATE);
    // Pauses: 150 ms or more under −40 dB from the loud level.
    let loud = crate::frames::quantile(&frames.loudness_db, 0.95);
    let mut pauses = Vec::new();
    let mut quiet_from = None;
    for (i, level) in frames.loudness_db.iter().chain(std::iter::once(&0.0)).enumerate() {
        let quiet = i < frames.loudness_db.len() && *level < loud - 40.0;
        match (quiet, quiet_from) {
            (true, None) => quiet_from = Some(i),
            (false, Some(from)) => {
                let (a, b) = (from as f64 / RATE, i as f64 / RATE);
                if b - a >= 0.15 {
                    pauses.push((a, b.min(duration)));
                }
                quiet_from = None;
            }
            _ => {}
        }
    }
    // Syllable nuclei: envelope peaks that rise at least 0.08 above the dip
    // since the previous nucleus, at least 90 ms apart.
    let smoothed = smooth(&envelope, 3);
    let mut syllables: Vec<f64> = Vec::new();
    let near = (0.045 * RATE) as usize;
    // The take starts from silence.
    let mut dip = 0.0f32;
    for i in 0..smoothed.len() {
        let v = smoothed[i];
        dip = dip.min(v);
        let a = i.saturating_sub(near);
        let b = (i + near + 1).min(smoothed.len());
        if v < 0.35 || smoothed[a..b].iter().any(|w| *w > v) || v - dip <= 0.08 {
            continue;
        }
        let t = i as f64 / RATE;
        if syllables.last().is_none_or(|last| t - last >= 0.09) {
            syllables.push(t);
            dip = v;
        }
    }
    let words = word_features(words, &frames.loudness_db, &pitch_hz);
    VoiceFeatures { duration, envelope, pitch_hz, pauses, syllables, words, phones: phones.to_vec() }
}

fn word_features(words: &[Word], loudness_db: &[f32], pitch_hz: &[f32]) -> Vec<WordFeature> {
    let range = |start: f64, end: f64| {
        let a = ((start * RATE) as usize).min(loudness_db.len());
        let b = ((end * RATE).ceil() as usize).clamp(a, loudness_db.len());
        (a, b)
    };
    let mut raw: Vec<(f32, f32, f32)> = Vec::new();
    for word in words {
        let (a, b) = range(word.start, word.end);
        let level = if b > a { loudness_db[a..b].iter().sum::<f32>() / (b - a) as f32 } else { -120.0 };
        let mut voiced: Vec<f32> = pitch_hz.get(a..b.min(pitch_hz.len())).unwrap_or(&[]).iter().copied().filter(|p| *p > 0.0).collect();
        voiced.sort_by(|x, y| x.total_cmp(y));
        let pitch = voiced.get(voiced.len() / 2).copied().unwrap_or(0.0);
        raw.push((level, pitch, (word.end - word.start) as f32));
    }
    // Standard scores within the take; a word's emphasis is the logistic
    // of loudness (weight 1), pitch (0.7) and length (0.4).
    let z = |values: Vec<f32>| {
        let n = values.len().max(1) as f32;
        let mean = values.iter().sum::<f32>() / n;
        let sd = (values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n).sqrt().max(1e-3);
        values.into_iter().map(move |v| (v - mean) / sd).collect::<Vec<f32>>()
    };
    let zl = z(raw.iter().map(|r| r.0).collect());
    let voiced_mean = {
        let v: Vec<f32> = raw.iter().map(|r| r.1).filter(|p| *p > 0.0).collect();
        if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
    };
    let zp = z(raw.iter().map(|r| if r.1 > 0.0 { r.1 } else { voiced_mean }).collect());
    let zd = z(raw.iter().map(|r| r.2).collect());
    words
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let score = zl[i] + 0.7 * zp[i] + 0.4 * zd[i];
            WordFeature { text: word.text.clone(), start: word.start, end: word.end, loudness_db: raw[i].0, pitch_hz: raw[i].1, emphasis: 1.0 / (1.0 + (-1.2 * score).exp()) }
        })
        .collect()
}

impl VoiceFeatures {
    pub fn envelope_at(&self, t: f64) -> f32 {
        crate::sample(&self.envelope, t)
    }

    pub fn pitch_at(&self, t: f64) -> f32 {
        crate::sample(&self.pitch_hz, t)
    }

    /// The word being spoken at `t`.
    pub fn word_at(&self, t: f64) -> Option<&WordFeature> {
        self.words.iter().find(|w| t >= w.start && t < w.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three "words": tones at 120, 120 and 220 Hz, the last one louder,
    /// with silences between.
    fn take(rate: f64) -> (Vec<f32>, Vec<Word>) {
        let mut out = Vec::new();
        let mut words = Vec::new();
        for (i, (freq, level)) in [(120.0, 0.2), (120.0, 0.2), (220.0, 0.6)].into_iter().enumerate() {
            let start = out.len() as f64 / rate;
            for n in 0..(0.4 * rate) as usize {
                let t = n as f64 / rate;
                let fade = (t / 0.02).min(1.0).min((0.4 - t) / 0.02);
                out.push((level * fade * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32);
            }
            words.push(Word { text: format!("w{i}"), start, end: out.len() as f64 / rate });
            out.extend(std::iter::repeat(0.0).take((0.3 * rate) as usize));
        }
        (out, words)
    }

    #[test]
    fn pitch_pauses_syllables_and_emphasis() {
        let rate = 24_000.0;
        let (mono, words) = take(rate);
        let features = analyze_voice(&mono, rate, &words, &[]);
        let p = features.pitch_at(0.2);
        assert!((p - 120.0).abs() < 3.0, "pitch {p}");
        assert!((features.pitch_at(1.6) - 220.0).abs() < 4.0, "pitch {}", features.pitch_at(1.6));
        assert_eq!(features.pauses.len(), 3, "{:?}", features.pauses);
        assert!((features.pauses[0].0 - 0.4).abs() < 0.03);
        assert_eq!(features.syllables.len(), 3, "{:?}", features.syllables);
        let e: Vec<f32> = features.words.iter().map(|w| w.emphasis).collect();
        assert!(e[2] > 0.8 && e[0] < 0.4 && e[1] < 0.4, "{e:?}");
        assert_eq!(features.word_at(1.5).unwrap().text, "w2");
    }
}
