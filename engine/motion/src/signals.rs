//! Time domains (EDITS.md §10.3): what a document reads besides its own
//! clock. Property functions get `local` (the layer's clock, span-relative)
//! and `edit` (the host's global time), and read free-running signals at
//! whatever time they pass, usually `edit`: `music(@m).beat_phase(edit)`
//! pulses on the music's grid wherever the clip starts or is trimmed.
//!
//! [`SignalSource`] is the provider. The host chooses it; the document
//! text is the same for both:
//! - [`AudioFeatures`] (analyzed): precomputed curves, sampled exactly, for
//!   frame-exact renders and exports;
//! - [`LiveSignals`] (live): the VJ graph's transport clock (`stage.clock`:
//!   beats and bpm from tap, MIDI clock or the deck) and live audio levels,
//!   as a snapshot the host refreshes every frame. Sampling a snapshot is
//!   still a pure function of `t`.

use crate::features::{AudioFeatures, MusicFeatures, VoiceFeatures, WordState};
use crate::timing::{BeatGrid, Timing};

/// Signals a document reads, all at a time `t` on the host's clock.
pub trait SignalSource: Send + Sync + std::fmt::Debug {
    /// Fractional beats since the grid's beat 0 at `t`.
    fn beat_index(&self, music: &str, t: f64) -> f64;
    /// Beats in a bar and the beat index of bar 0.
    fn bar(&self, music: &str) -> (u32, u32);
    fn bpm(&self, music: &str) -> f64;
    /// Loudness, 0..1.
    fn energy(&self, music: &str, t: f64) -> f64;
    /// A band's energy: `low`, `mid`, `high` or an index.
    fn band(&self, music: &str, band: &str, t: f64) -> f64;
    /// How strongly an onset hit in the last `window` seconds (fading).
    fn hit(&self, music: &str, t: f64, window: f64) -> f64;
    /// A voice's loudness, 0..1.
    fn amplitude(&self, voice: &str, t: f64) -> f64;
    /// A voice's pitch in Hz, 0 unvoiced.
    fn pitch(&self, voice: &str, t: f64) -> f64;
    /// A voice's analysed words (none live).
    fn voice(&self, voice: &str) -> Option<&VoiceFeatures>;
    /// A track's analysis (none live).
    fn music(&self, music: &str) -> Option<&MusicFeatures>;
    /// The timing context the signals give musical time references.
    fn timing(&self, music: Option<&str>) -> Timing;
    /// The app runs a bound graphic follows (`run()`; see [`crate::bind`]).
    fn runs(&self) -> Option<&crate::bind::RunContext> {
        None
    }
    /// The music and voice ids there are, for a reader that names none
    /// (a motion3d scene's `music(…)` without an id): the first is used.
    fn default_music(&self) -> Option<&str> {
        None
    }
    fn default_voice(&self) -> Option<&str> {
        None
    }

    fn beat_phase(&self, music: &str, t: f64) -> f64 {
        self.beat_index(music, t).rem_euclid(1.0)
    }

    /// `bins` spectrum levels 0..1, low to high, around `t` (see
    /// [`crate::AudioSignal::spectrum`]); silence without the track's sound.
    fn spectrum(&self, music: &str, t: f64, bins: usize) -> Vec<f64> {
        self.music(music).map_or_else(|| vec![0.0; bins], |m| m.spectrum(t, bins))
    }

    /// `n` waveform slots across `window` seconds centred on `t` (see
    /// [`crate::AudioSignal::wave`]); silence without the track's sound.
    fn wave(&self, music: &str, t: f64, n: usize, window: f64) -> Vec<f64> {
        self.music(music).map_or_else(|| vec![0.0; n], |m| m.wave(t, n, window))
    }

    fn bar_phase(&self, music: &str, t: f64) -> f64 {
        let (per_bar, downbeat) = self.bar(music);
        ((self.beat_index(music, t) - downbeat as f64) / per_bar.max(1) as f64).rem_euclid(1.0)
    }
}

impl SignalSource for AudioFeatures {
    fn default_music(&self) -> Option<&str> {
        self.music.first().map(|(id, _)| id.as_str())
    }

    fn default_voice(&self) -> Option<&str> {
        self.voices.first().map(|(id, _)| id.as_str())
    }

    fn beat_index(&self, music: &str, t: f64) -> f64 {
        self.music(music).map_or(t * 2.0, |m| m.grid().beat_at(t))
    }

    fn bar(&self, music: &str) -> (u32, u32) {
        self.music(music).map_or((4, 0), |m| (m.beats_per_bar.max(1), m.downbeat))
    }

    fn bpm(&self, music: &str) -> f64 {
        self.music(music).map_or(120.0, |m| if m.bpm > 0.0 { m.bpm } else { 120.0 })
    }

    fn energy(&self, music: &str, t: f64) -> f64 {
        self.music(music).map_or(0.0, |m| m.energy.sample(t))
    }

    fn band(&self, music: &str, band: &str, t: f64) -> f64 {
        self.music(music).and_then(|m| m.band_index(band).map(|i| m.bands[i].sample(t))).unwrap_or(0.0)
    }

    fn hit(&self, music: &str, t: f64, window: f64) -> f64 {
        let Some(m) = self.music(music) else { return 0.0 };
        let window = window.max(1e-6);
        m.onsets.iter().filter(|onset| (0.0..window).contains(&(t - onset.t))).map(|onset| onset.strength * (1.0 - (t - onset.t) / window)).fold(0.0, f64::max)
    }

    fn amplitude(&self, voice: &str, t: f64) -> f64 {
        AudioFeatures::voice(self, voice).map_or(0.0, |v| v.amplitude.sample(t))
    }

    fn pitch(&self, voice: &str, t: f64) -> f64 {
        AudioFeatures::voice(self, voice).map_or(0.0, |v| v.pitch.sample(t))
    }

    fn voice(&self, voice: &str) -> Option<&VoiceFeatures> {
        AudioFeatures::voice(self, voice)
    }

    fn music(&self, music: &str) -> Option<&MusicFeatures> {
        AudioFeatures::music(self, music)
    }

    fn timing(&self, music: Option<&str>) -> Timing {
        AudioFeatures::timing(self, music)
    }
}

/// The live transport and levels at one moment, refreshed by the host each
/// frame: the beat position at `anchor` (host seconds) and the tempo since,
/// and the latest audio levels (held until the next refresh).
#[derive(Clone, Debug, PartialEq)]
pub struct LiveSignals {
    pub bpm: f64,
    /// Host time the beat position was taken at.
    pub anchor: f64,
    /// Fractional beat at `anchor`.
    pub beat: f64,
    pub beats_per_bar: u32,
    /// The beat index bar 0 starts on (the transport's downbeat phase).
    pub downbeat: u32,
    pub energy: f64,
    /// Band levels, low to high.
    pub bands: Vec<f64>,
    /// Voice levels by node id: (amplitude, pitch).
    pub voices: Vec<(String, f64, f64)>,
}

impl Default for LiveSignals {
    fn default() -> Self {
        Self { bpm: 120.0, anchor: 0.0, beat: 0.0, beats_per_bar: 4, downbeat: 0, energy: 0.0, bands: Vec::new(), voices: Vec::new() }
    }
}

impl SignalSource for LiveSignals {
    fn default_music(&self) -> Option<&str> {
        Some("")
    }

    fn default_voice(&self) -> Option<&str> {
        self.voices.first().map(|(id, _, _)| id.as_str())
    }

    fn beat_index(&self, _music: &str, t: f64) -> f64 {
        self.beat + (t - self.anchor) * self.bpm / 60.0
    }

    fn bar(&self, _music: &str) -> (u32, u32) {
        (self.beats_per_bar.max(1), self.downbeat)
    }

    fn bpm(&self, _music: &str) -> f64 {
        self.bpm
    }

    fn energy(&self, _music: &str, _t: f64) -> f64 {
        self.energy
    }

    fn band(&self, _music: &str, band: &str, _t: f64) -> f64 {
        let n = self.bands.len();
        if n == 0 {
            return 0.0;
        }
        let index = match band {
            "low" | "bass" => 0,
            "mid" => n / 2,
            "high" | "treble" => n - 1,
            other => match other.parse::<usize>() {
                Ok(i) if i < n => i,
                _ => return 0.0,
            },
        };
        self.bands[index]
    }

    fn hit(&self, music: &str, t: f64, window: f64) -> f64 {
        // Live: the beat itself is the onset.
        let since = self.beat_phase(music, t) * 60.0 / self.bpm.max(1e-6);
        if since < window { 1.0 - since / window.max(1e-6) } else { 0.0 }
    }

    fn amplitude(&self, voice: &str, _t: f64) -> f64 {
        self.voices.iter().find(|(id, _, _)| id == voice).map_or(0.0, |v| v.1)
    }

    fn pitch(&self, voice: &str, _t: f64) -> f64 {
        self.voices.iter().find(|(id, _, _)| id == voice).map_or(0.0, |v| v.2)
    }

    fn voice(&self, _voice: &str) -> Option<&VoiceFeatures> {
        None
    }

    fn music(&self, _music: &str) -> Option<&MusicFeatures> {
        None
    }

    fn timing(&self, _music: Option<&str>) -> Timing {
        // The transport's grid, extended both ways at its tempo.
        let origin = self.anchor - self.beat * 60.0 / self.bpm.max(1e-6);
        Timing { grid: Some(BeatGrid { beats: Vec::new(), bpm: self.bpm, origin, beats_per_bar: self.beats_per_bar.max(1), downbeat: self.downbeat }), ..Default::default() }
    }
}

/// A word's state through any source that has the words.
pub fn word_state(source: &dyn SignalSource, voice: &str, index: usize, t: f64) -> WordState {
    source.voice(voice).map_or(WordState::Upcoming, |v| v.word_state(index, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzed_and_live_agree_on_a_steady_grid() {
        let analyzed = AudioFeatures::synthetic("a b", 0.0, 2.0, 120.0, 8.0);
        let live = LiveSignals { bpm: 120.0, anchor: 3.0, beat: 6.0, ..Default::default() };
        for t in [0.0, 0.25, 1.1, 2.0, 3.7] {
            assert!((analyzed.beat_index("m", t) - live.beat_index("m", t)).abs() < 1e-9, "{t}");
            assert!((analyzed.bar_phase("m", t) - live.bar_phase("m", t)).abs() < 1e-9, "{t}");
        }
        assert_eq!(live.timing(None).grid.unwrap().time_of_beat(4.0), 2.0);
        assert!((live.beat_phase("m", 3.25) - 0.5).abs() < 1e-9);
        assert!(live.hit("m", 3.0, 0.2) > 0.99 && live.hit("m", 3.3, 0.2) == 0.0);
    }
}
