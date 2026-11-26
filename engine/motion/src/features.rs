//! Audio as animation data (EDITS.md §10.3): precomputed per-frame features
//! of the edit's voiceovers and music, supplied by the host as a pure input.
//! A document reads them through `voice(@id)` and `music(@id)` in property
//! functions and binds text to a voiceover's words (`Text{source:
//! voice(@vo)}`). Every read samples the precomputed curves at `t`
//! (linear interpolation, held at the ends): nothing listens to live audio,
//! so a frame stays a pure function of `t` and the features.
//!
//! Times are seconds on the clock of the document that reads them (see
//! [`crate::timing::Timing::shifted`]; [`AudioFeatures::shifted`] does the
//! same here).

use crate::timing::{BeatGrid, Onset, Section, SpokenTiming, Timing, VoiceTiming};
use std::sync::Arc;

/// A curve sampled at a fixed rate from `start` seconds.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FeatureCurve {
    pub start: f64,
    /// Samples per second.
    pub rate: f64,
    pub values: Vec<f32>,
}

impl FeatureCurve {
    pub fn new(start: f64, rate: f64, values: Vec<f32>) -> Self {
        Self { start, rate, values }
    }

    /// The value at `t`: linear between samples, the end samples held
    /// outside, 0 for an empty curve.
    pub fn sample(&self, t: f64) -> f64 {
        let n = self.values.len();
        if n == 0 || !(self.rate > 0.0) || !t.is_finite() {
            return 0.0;
        }
        let x = (t - self.start) * self.rate;
        if x <= 0.0 {
            return self.values[0] as f64;
        }
        let last = (n - 1) as f64;
        if x >= last {
            return self.values[n - 1] as f64;
        }
        let i = x.floor() as usize;
        let u = x - i as f64;
        self.values[i] as f64 * (1.0 - u) + self.values[i + 1] as f64 * u
    }
}

/// One spoken word of a voiceover, with its syllables.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FeatureWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// How stressed the word is, 0..1 (1: the line's peak).
    pub emphasis: f64,
    /// The line the word belongs to (index into [`VoiceFeatures::lines`]).
    pub line: u32,
    /// Syllable (or phoneme group) spans inside the word; empty when the
    /// analysis has none (then the word is one syllable).
    pub syllables: Vec<(f64, f64)>,
}

/// One timed phoneme of a voice (IPA or ARPAbet in `text`), when the voice
/// engine gave them (a Kokoro take does): what a Character's lip sync reads.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FeaturePhone {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct VoiceFeatures {
    pub words: Vec<FeatureWord>,
    /// `(start, end)` of each line.
    pub lines: Vec<(f64, f64)>,
    /// Loudness envelope, 0..1.
    pub amplitude: FeatureCurve,
    /// Pitch in Hz; 0 where unvoiced.
    pub pitch: FeatureCurve,
    /// `(start, end)` of every pause between words.
    pub pauses: Vec<(f64, f64)>,
    /// Every phoneme in time order, when known; empty otherwise (lip sync
    /// then spells the words).
    pub phones: Vec<FeaturePhone>,
}

impl VoiceFeatures {
    /// The index of the word being spoken at `t` (or the last one begun,
    /// while it is still within `hold` seconds of its end); `None` in a
    /// pause or outside the voice.
    pub fn word_at(&self, t: f64) -> Option<usize> {
        let next = self.words.partition_point(|word| word.start <= t);
        let index = next.checked_sub(1)?;
        (t < self.words[index].end).then_some(index)
    }

    /// The index of the line spoken at `t`.
    pub fn line_at(&self, t: f64) -> Option<usize> {
        let next = self.lines.partition_point(|line| line.0 <= t);
        let index = next.checked_sub(1)?;
        (t < self.lines[index].1).then_some(index)
    }

    /// Whether `t` falls in a pause.
    pub fn in_pause(&self, t: f64) -> bool {
        self.pauses.iter().any(|(start, end)| *start <= t && t < *end)
    }

    /// The word's state at `t`: 0 before it (upcoming), its progress 0..1
    /// while spoken, and 1 after (done). See [`WordState`].
    pub fn word_state(&self, index: usize, t: f64) -> WordState {
        let Some(word) = self.words.get(index) else { return WordState::Upcoming };
        if t < word.start {
            WordState::Upcoming
        } else if t >= word.end {
            WordState::Done
        } else {
            WordState::Active((t - word.start) / (word.end - word.start).max(1e-9))
        }
    }
}

/// Where a word is at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WordState {
    Upcoming,
    /// Being spoken; how far through, 0..1.
    Active(f64),
    Done,
}

/// The analysis of a music track.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MusicFeatures {
    pub bpm: f64,
    /// Beat times, seconds, increasing.
    pub beats: Vec<f64>,
    pub beats_per_bar: u32,
    /// Index into `beats` of the first downbeat.
    pub downbeat: u32,
    pub onsets: Vec<Onset>,
    /// Short-term loudness, 0..1.
    pub energy: FeatureCurve,
    /// Energy per band, low to high (`band(@low)` is the first, `@mid` the
    /// middle one, `@high` the last; `band(n)` by index).
    pub bands: Vec<FeatureCurve>,
    pub sections: Vec<Section>,
    /// The track's sound, when the host has it: what the spectrum and the
    /// waveform are read from (`music(@m).spectrum(t, n)`, `.wave(t, n,
    /// window)`). Without it both read silence.
    pub signal: Option<AudioSignal>,
}

/// A track's sound as mono samples on a clock: sample 0 sounds at `start`.
/// Shared, never copied: features are cloned per document.
#[derive(Clone, Debug)]
pub struct AudioSignal {
    pub start: f64,
    /// Samples per second.
    pub rate: f64,
    pub samples: Arc<[f32]>,
}

impl PartialEq for AudioSignal {
    fn eq(&self, other: &Self) -> bool {
        self.start == other.start && self.rate == other.rate && (Arc::ptr_eq(&self.samples, &other.samples) || self.samples == other.samples)
    }
}

/// The spectrum's bands span this range, log spaced.
pub const SPECTRUM_LOW_HZ: f64 = 30.0;
pub const SPECTRUM_HIGH_HZ: f64 = 16_000.0;
/// A band's level reads 0 at this level and 1 at full scale (a full-scale
/// sine in the band), linear in dB between.
pub const SPECTRUM_FLOOR_DB: f64 = -72.0;
/// The analysis window, seconds (2048 samples at 48 kHz): centred on the
/// time asked, so the spectrum at a frame is the sound around that frame.
pub const SPECTRUM_WINDOW: f64 = 0.0427;

thread_local! {
    /// One transform per thread and window size: its tables are built once.
    static FFT: std::cell::RefCell<Option<makepad_audio_picture::fft::Fft>> = const { std::cell::RefCell::new(None) };
}

impl AudioSignal {
    pub fn new(start: f64, rate: f64, samples: Arc<[f32]>) -> Self {
        Self { start, rate, samples }
    }

    /// The sample at fractional index `x` (linear; silence outside).
    fn at(&self, x: f64) -> f64 {
        if !(x >= 0.0) {
            return 0.0;
        }
        let i = x.floor() as usize;
        let Some(a) = self.samples.get(i).copied() else { return 0.0 };
        let b = self.samples.get(i + 1).copied().unwrap_or(0.0);
        let u = (x - i as f64) as f32;
        (a * (1.0 - u) + b * u) as f64
    }

    /// `bins` levels 0..1 from [`SPECTRUM_LOW_HZ`] to [`SPECTRUM_HIGH_HZ`]
    /// (log spaced, low first) of the sound in a [`SPECTRUM_WINDOW`] centred
    /// on clock second `t`. See [`SPECTRUM_FLOOR_DB`] for the scale.
    pub fn spectrum(&self, t: f64, bins: usize) -> Vec<f64> {
        if bins == 0 || !(self.rate > 0.0) || !t.is_finite() {
            return vec![0.0; bins];
        }
        let size = ((self.rate * SPECTRUM_WINDOW).round() as usize).max(16).next_power_of_two();
        let first = ((t - self.start) * self.rate).round() as i64 - size as i64 / 2;
        let frame: Vec<f32> = (0..size as i64).map(|k| usize::try_from(first + k).ok().and_then(|i| self.samples.get(i)).copied().unwrap_or(0.0)).collect();
        let mut magnitudes = vec![0.0f32; size / 2];
        FFT.with(|fft| {
            let mut fft = fft.borrow_mut();
            if fft.as_ref().is_none_or(|f| f.size() != size) {
                *fft = Some(makepad_audio_picture::fft::Fft::new(size));
            }
            fft.as_mut().unwrap().magnitudes(&frame, &mut magnitudes);
        });
        for m in &mut magnitudes {
            *m = (20.0 * m.max(1.0e-6).log10()).max(-120.0);
        }
        let mut bands = vec![0.0f32; bins];
        makepad_audio_picture::fft::log_bands(&magnitudes, self.rate as f32, SPECTRUM_LOW_HZ as f32, SPECTRUM_HIGH_HZ as f32, &mut bands);
        bands.into_iter().map(|db| ((db as f64 - SPECTRUM_FLOOR_DB) / -SPECTRUM_FLOOR_DB).clamp(0.0, 1.0)).collect()
    }

    /// The waveform around clock second `t`: `n` slots evenly across
    /// `window` seconds centred on `t`, each the sample of largest
    /// magnitude in it, with its sign (-1..1, full scale 1); a slot
    /// narrower than a sample reads the sound at its centre.
    pub fn wave(&self, t: f64, n: usize, window: f64) -> Vec<f64> {
        if n == 0 || !(self.rate > 0.0) || !t.is_finite() || !(window > 0.0) {
            return vec![0.0; n];
        }
        let from = (t - window * 0.5 - self.start) * self.rate;
        let step = window * self.rate / n as f64;
        (0..n).map(|k| {
            let (a, b) = (from + k as f64 * step, from + (k + 1) as f64 * step);
            let (lo, hi) = (a.ceil().max(0.0) as usize, (b.ceil().max(0.0) as usize).min(self.samples.len()));
            if hi <= lo {
                return self.at((a + b) * 0.5);
            }
            self.samples[lo..hi].iter().copied().fold(0.0f32, |best, s| if s.abs() > best.abs() { s } else { best }) as f64
        }).collect()
    }
}

impl MusicFeatures {
    /// The same features `dt` seconds later on the clock.
    pub fn shifted(&self, dt: f64) -> MusicFeatures {
        let mut out = self.clone();
        for beat in &mut out.beats {
            *beat += dt;
        }
        for onset in &mut out.onsets {
            onset.t += dt;
        }
        for section in &mut out.sections {
            section.start += dt;
            section.end += dt;
        }
        out.energy.start += dt;
        for band in &mut out.bands {
            band.start += dt;
        }
        if let Some(signal) = &mut out.signal {
            signal.start += dt;
        }
        out
    }

    /// [`AudioSignal::spectrum`], silence without a signal.
    pub fn spectrum(&self, t: f64, bins: usize) -> Vec<f64> {
        self.signal.as_ref().map_or_else(|| vec![0.0; bins], |s| s.spectrum(t, bins))
    }

    /// [`AudioSignal::wave`], silence without a signal.
    pub fn wave(&self, t: f64, n: usize, window: f64) -> Vec<f64> {
        self.signal.as_ref().map_or_else(|| vec![0.0; n], |s| s.wave(t, n, window))
    }

    pub fn grid(&self) -> BeatGrid {
        BeatGrid::from_beats(self.beats.clone(), self.beats_per_bar.max(1), self.downbeat, if self.bpm > 0.0 { self.bpm } else { 120.0 })
    }

    /// `@low`, `@mid`, `@high` or an index.
    pub fn band_index(&self, name: &str) -> Option<usize> {
        let n = self.bands.len();
        if n == 0 {
            return None;
        }
        match name {
            "low" | "bass" => Some(0),
            "mid" => Some(n / 2),
            "high" | "treble" => Some(n - 1),
            other => other.parse::<usize>().ok().filter(|i| *i < n),
        }
    }
}

/// Every audio source's features, by the node id (or name) a document
/// writes in `voice(@id)` / `music(@id)`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AudioFeatures {
    pub voices: Vec<(String, VoiceFeatures)>,
    pub music: Vec<(String, MusicFeatures)>,
}

impl AudioFeatures {
    pub fn voice(&self, id: &str) -> Option<&VoiceFeatures> {
        self.voices.iter().find(|(name, _)| name == id).map(|(_, voice)| voice)
    }

    pub fn music(&self, id: &str) -> Option<&MusicFeatures> {
        self.music.iter().find(|(name, _)| name == id).map(|(_, music)| music)
    }

    pub fn is_empty(&self) -> bool {
        self.voices.is_empty() && self.music.is_empty()
    }

    /// The timing context these features give musical and spoken time
    /// references: the grid, onsets and sections of `music` (or the only
    /// music source), and every voice's words.
    pub fn timing(&self, music: Option<&str>) -> Timing {
        let track = match music {
            Some(id) => self.music(id),
            None if self.music.len() == 1 => Some(&self.music[0].1),
            None => None,
        };
        Timing {
            grid: track.filter(|track| !track.beats.is_empty() || track.bpm > 0.0).map(MusicFeatures::grid),
            onsets: track.map_or(Vec::new(), |track| track.onsets.clone()),
            sections: track.map_or(Vec::new(), |track| track.sections.clone()),
            voices: self.voices.iter().map(|(id, voice)| {
                let lines = voice.lines.iter().enumerate().map(|(index, (start, _))| SpokenTiming {
                    start: *start,
                    words: voice.words.iter().filter(|word| word.line as usize == index).map(|word| word.start).collect(),
                    texts: voice.words.iter().filter(|word| word.line as usize == index).map(|word| word.text.clone()).collect(),
                }).collect();
                (id.clone(), VoiceTiming { lines })
            }).collect(),
        }
    }

    /// The same features `dt` seconds later on the clock.
    pub fn shifted(&self, dt: f64) -> AudioFeatures {
        let mut out = self.clone();
        let curve = |curve: &mut FeatureCurve| curve.start += dt;
        for (_, voice) in &mut out.voices {
            for word in &mut voice.words {
                word.start += dt;
                word.end += dt;
                for syllable in &mut word.syllables {
                    syllable.0 += dt;
                    syllable.1 += dt;
                }
            }
            for line in &mut voice.lines {
                line.0 += dt;
                line.1 += dt;
            }
            for pause in &mut voice.pauses {
                pause.0 += dt;
                pause.1 += dt;
            }
            for phone in &mut voice.phones {
                phone.start += dt;
                phone.end += dt;
            }
            curve(&mut voice.amplitude);
            curve(&mut voice.pitch);
        }
        for (_, music) in &mut out.music {
            *music = music.shifted(dt);
        }
        out
    }

    /// A synthetic feature set for tests and previews without analysis:
    /// a voice `vo` speaking `text` from `start` at `wps` words a second,
    /// and music `m` at `bpm` with an onset on every beat, energy pulsing
    /// on the beat, three bands and intro/drop sections of `bars` bars.
    pub fn synthetic(text: &str, start: f64, wps: f64, bpm: f64, seconds: f64) -> AudioFeatures {
        let rate = 100.0;
        let samples = (seconds * rate).ceil() as usize + 1;
        let beat = 60.0 / bpm;
        let mut words = Vec::new();
        let mut t = start;
        for (i, text) in text.split_whitespace().enumerate() {
            let length = 0.8 / wps;
            let syllables = (text.chars().filter(|c| "aeiouy".contains(c.to_ascii_lowercase())).count()).max(1);
            let syllables = (0..syllables).map(|k| {
                let a = t + length * k as f64 / syllables as f64;
                (a, a + length / syllables as f64)
            }).collect();
            words.push(FeatureWord { text: text.to_string(), start: t, end: t + length, emphasis: if i % 3 == 0 { 1.0 } else { 0.4 }, line: 0, syllables });
            t += 1.0 / wps;
        }
        let end = words.last().map_or(start, |word| word.end);
        let pauses = words.windows(2).map(|pair| (pair[0].end, pair[1].start)).collect();
        let amplitude = (0..samples).map(|i| {
            let t = i as f64 / rate;
            words.iter().find(|word| word.start <= t && t < word.end).map_or(0.0, |word| {
                let u = (t - word.start) / (word.end - word.start);
                (std::f64::consts::PI * u).sin() * (0.5 + 0.5 * word.emphasis)
            }) as f32
        }).collect();
        let pitch = (0..samples).map(|i| {
            let t = i as f64 / rate;
            if words.iter().any(|word| word.start <= t && t < word.end) { (140.0 + 40.0 * (t * 3.0).sin()) as f32 } else { 0.0 }
        }).collect();
        let pulse = |t: f64, phase: f64| {
            let x = ((t / beat) + phase).fract();
            (1.0 - x).powi(3) as f32
        };
        let beats: Vec<f64> = (0..).map(|i| i as f64 * beat).take_while(|t| *t <= seconds).collect();
        let half = seconds * 0.5;
        AudioFeatures {
            voices: vec![("vo".into(), VoiceFeatures {
                words,
                lines: vec![(start, end)],
                amplitude: FeatureCurve::new(0.0, rate, amplitude),
                pitch: FeatureCurve::new(0.0, rate, pitch),
                pauses,
                phones: Vec::new(),
            })],
            music: vec![("m".into(), MusicFeatures {
                bpm,
                onsets: beats.iter().map(|t| Onset { t: *t, strength: 1.0, name: None }).collect(),
                beats,
                beats_per_bar: 4,
                downbeat: 0,
                energy: FeatureCurve::new(0.0, rate, (0..samples).map(|i| 0.3 + 0.7 * pulse(i as f64 / rate, 0.0)).collect()),
                bands: (0..3).map(|b| FeatureCurve::new(0.0, rate, (0..samples).map(|i| pulse(i as f64 / rate, b as f64 * 0.25)).collect())).collect(),
                sections: vec![
                    Section { name: "intro".into(), start: 0.0, end: half },
                    Section { name: "drop".into(), start: half, end: seconds },
                ],
                signal: None,
            })],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine sweep from 100 Hz to 8 kHz over 4 s (exponential), at 48 kHz,
    /// starting 2 s into the clock; and the frequency it plays at `t`.
    fn sweep() -> (AudioSignal, impl Fn(f64) -> f64) {
        let (rate, secs, f0, f1) = (48_000.0, 4.0, 100.0, 8_000.0);
        let k = (f1 / f0 as f64).ln() / secs;
        let samples: Vec<f32> = (0..(rate * secs) as usize).map(|i| {
            let t = i as f64 / rate;
            // Phase of an exponential sweep: 2π f0 (e^{kt} - 1) / k.
            (0.5 * (std::f64::consts::TAU * f0 * ((k * t).exp() - 1.0) / k).sin()) as f32
        }).collect();
        (AudioSignal::new(2.0, rate, samples.into()), move |t: f64| f0 * (k * (t - 2.0)).exp())
    }

    /// The centre frequency of band `i` of `bins`.
    fn band_hz(i: usize, bins: usize) -> f64 {
        let ratio = SPECTRUM_HIGH_HZ / SPECTRUM_LOW_HZ;
        SPECTRUM_LOW_HZ * ratio.powf((i as f64 + 0.5) / bins as f64)
    }

    #[test]
    fn the_spectrum_follows_a_sweep_at_any_bin_count() {
        let (signal, playing) = sweep();
        for bins in [16, 32, 64, 100] {
            for t in [2.25, 3.0, 4.0, 5.5] {
                let levels = signal.spectrum(t, bins);
                assert_eq!(levels.len(), bins);
                // The loudest band (of equally loud neighbours reading one FFT
                // bin, the one nearest the tone).
                let top = levels.iter().copied().fold(0.0, f64::max);
                let distance = |i: usize| (band_hz(i, bins) / playing(t)).log2().abs();
                let peak = (0..bins).filter(|i| levels[*i] >= top - 1e-9).min_by(|a, b| distance(*a).total_cmp(&distance(*b))).unwrap();
                // Within one band (half an octave at 16 bands) of the tone.
                let octaves = distance(peak);
                // One band, or the window's resolution (one FFT bin) where
                // that is coarser.
                let band = ((SPECTRUM_HIGH_HZ / SPECTRUM_LOW_HZ).log2() / bins as f64).max((1.0 + 48_000.0 / 2048.0 / playing(t)).log2());
                assert!(octaves <= band * 1.01, "{bins} bins at {t} s: band {peak} ({:.0} Hz) for a {:.0} Hz tone", band_hz(peak, bins), playing(t));
                // A steady 0.5 sine is -6 dB in its band (1 - 6/72 = 0.92); the
                // sweep smears over the window as it speeds up (4.6 kHz at
                // 5.5 s moves 9 FFT bins in 43 ms: about -10 dB more).
                assert!(levels[peak] > 0.7 && levels[peak] < 0.93, "{bins} bins at {t} s: {}", levels[peak]);
                // Two octaves away is far below.
                let far = (0..bins).filter(|i| (band_hz(*i, bins) / playing(t)).log2().abs() > 2.0).map(|i| levels[i]).fold(0.0, f64::max);
                assert!(far < levels[peak] - 0.3, "{bins} bins at {t} s: {far}");
            }
        }
        // A steady tone reads its level exactly (within the window's scalloping).
        let steady: Vec<f32> = (0..48_000).map(|i| (0.5 * (std::f64::consts::TAU * 1000.0 * i as f64 / 48_000.0).sin()) as f32).collect();
        let levels = AudioSignal::new(0.0, 48_000.0, steady.into()).spectrum(0.5, 48);
        let peak = levels.iter().copied().fold(0.0, f64::max);
        assert!((peak - (1.0 - 6.02 / 72.0)).abs() < 0.025, "{peak}");
        // Outside the sound: silence; shifting the clock moves it.
        assert!(signal.spectrum(1.0, 32).iter().all(|v| *v == 0.0));
        let later = MusicFeatures { signal: Some(signal.clone()), ..MusicFeatures::default() }.shifted(10.0);
        assert_eq!(later.spectrum(13.0, 32), signal.spectrum(3.0, 32));
        assert!(MusicFeatures::default().spectrum(3.0, 8).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn the_wave_is_the_signed_sound_across_its_window() {
        // 440 Hz at 0.8 from second 1.
        let rate = 48_000.0;
        let samples: Vec<f32> = (0..48_000).map(|i| (0.8 * (std::f64::consts::TAU * 440.0 * i as f64 / rate).sin()) as f32).collect();
        let signal = AudioSignal::new(1.0, rate, samples.into());
        // Ten cycles in 1/44 s, 400 slots: a sine with 20 sign changes.
        let wave = signal.wave(1.5, 400, 10.0 / 440.0);
        assert_eq!(wave.len(), 400);
        let crossings = wave.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        assert!((19..=21).contains(&crossings), "{crossings}");
        let peak = wave.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!((peak - 0.8).abs() < 0.01, "{peak}");
        // Few wide slots keep each slot's loudest sample: the envelope.
        assert!(signal.wave(1.5, 8, 0.4).iter().all(|v| (v.abs() - 0.8).abs() < 0.01));
        // Slots narrower than a sample read between samples.
        let fine = signal.wave(1.0 + 0.25 / 440.0, 3, 0.5 / rate);
        assert!((fine[1] - 0.8).abs() < 1e-3, "{fine:?}");
        // Before and after the sound: silence.
        assert!(signal.wave(0.5, 16, 0.1).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn curves_interpolate_and_hold_their_ends() {
        let curve = FeatureCurve::new(1.0, 10.0, vec![0.0, 1.0, 0.5]);
        assert_eq!(curve.sample(0.0), 0.0);
        assert!((curve.sample(1.05) - 0.5).abs() < 1e-6);
        assert!((curve.sample(1.15) - 0.75).abs() < 1e-6);
        assert_eq!(curve.sample(9.0), 0.5);
        assert_eq!(FeatureCurve::default().sample(1.0), 0.0);
    }

    #[test]
    fn words_have_a_state_and_the_features_give_a_timing() {
        let features = AudioFeatures::synthetic("say it on the beat", 1.0, 2.0, 120.0, 8.0);
        let voice = features.voice("vo").unwrap();
        assert_eq!(voice.words.len(), 5);
        assert_eq!(voice.word_at(1.1), Some(0));
        assert_eq!(voice.word_at(1.45), None, "a pause between words");
        assert!(voice.in_pause(1.45));
        assert_eq!(voice.word_state(1, 1.0), WordState::Upcoming);
        assert!(matches!(voice.word_state(1, 1.7), WordState::Active(p) if (p - 0.5).abs() < 1e-9));
        assert_eq!(voice.word_state(0, 3.0), WordState::Done);
        assert!(voice.amplitude.sample(1.2) > 0.1 && voice.amplitude.sample(1.45).abs() < 1e-6);
        let timing = features.timing(None);
        assert_eq!(timing.grid.as_ref().unwrap().time_of_beat(3.0), 1.5);
        assert_eq!(timing.resolve_ref(&crate::timing::TimeRef::Word { voice: "vo".into(), index: 2 }, 0.0).unwrap(), 2.0);
        assert_eq!(timing.resolve_ref(&crate::timing::TimeRef::Section { name: "drop".into(), nth: 0 }, 0.0).unwrap(), 4.0);
        let music = features.music("m").unwrap();
        assert_eq!(music.band_index("high"), Some(2));
        assert!(music.energy.sample(0.0) > music.energy.sample(0.25));
        let shifted = features.shifted(-1.0);
        assert_eq!(shifted.voice("vo").unwrap().word_at(0.1), Some(0));
        assert!((shifted.voice("vo").unwrap().amplitude.sample(0.2) - voice.amplitude.sample(1.2)).abs() < 1e-6);
    }
}
