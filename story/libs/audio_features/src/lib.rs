//! Audio as animation data.
//!
//! What a Motion document, the edit's musical and spoken time references
//! (`beat(n)`, `bar(n, b)`, `onset(n)`, `section(@drop)`, `word(@voice, i)`)
//! and the Generate agent read from a music track and a voice take:
//!
//! - [`music::analyze_music`]: BPM with its beat and downbeat grid (the one
//!   detector, `makepad-beat`), onsets with strength, loudness and
//!   energy per frame, six band energies per frame (sub … high), and
//!   sections labelled intro / build / drop / break / verse / outro.
//! - [`voice::analyze_voice`]: amplitude envelope, pitch (YIN), pauses,
//!   syllable nuclei, and per word (from the take's word timings) its
//!   loudness, pitch and emphasis; phonemes pass through when the TTS gave
//!   them.
//!
//! Per-frame values are at [`RATE`] frames per second and sampled with
//! linear interpolation, so any output fps reads them exactly the same way
//! (a frame is a pure function of `t`). [`json`] writes and reads them for
//! the cache kept beside the asset (keyed by the audio's content) and
//! summarises them for tools; [`to_motion`] hands them to Motion documents
//! as the pure timing input they read (`voice(@id)`, `music(@id)`).
//!
//! Cost: linear in the samples (six biquad pairs per sample, YIN at 16 kHz
//! for voice); run on a worker, once per asset revision.

pub mod frames;
pub mod json;
pub mod music;
pub mod to_motion;
pub mod voice;

pub use music::{analyze_music, analyze_music_with, describe_sections, MusicFeatures, Onset, Section, SectionKind, Snap};
pub use voice::{analyze_voice, Phone, VoiceFeatures, Word, WordFeature};

/// Feature frames per second.
pub const RATE: f64 = 100.0;

/// A per-frame series at `t`, linearly interpolated (0 outside).
pub fn sample(series: &[f32], t: f64) -> f32 {
    let pos = t * RATE;
    if !(pos >= 0.0) || series.is_empty() {
        return 0.0;
    }
    let i = pos.floor() as usize;
    match (series.get(i), series.get(i + 1)) {
        (Some(a), Some(b)) => {
            let f = (pos - i as f64) as f32;
            a * (1.0 - f) + b * f
        }
        (Some(a), None) => *a,
        _ => 0.0,
    }
}

/// A content key for a cache entry: FNV-1a over the samples' bits and the
/// rate, so the same audio always maps to the same features.
pub fn content_key(mono: &[f32], sample_rate: f64) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&sample_rate.to_bits().to_le_bytes());
    for s in mono {
        eat(&s.to_bits().to_le_bytes());
    }
    format!("af1-{hash:016x}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn sampling_interpolates() {
        let s = [0.0, 1.0];
        assert_eq!(super::sample(&s, 0.005), 0.5);
        assert_eq!(super::sample(&s, 1.0), 0.0);
        assert_ne!(super::content_key(&[0.0, 1.0], 48_000.0), super::content_key(&[0.0, 1.0], 44_100.0));
    }
}
