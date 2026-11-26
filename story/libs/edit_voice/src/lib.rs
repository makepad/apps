//! Voiceovers and subtitles for Stage's edits.
//!
//! - [`synth`]: a line's take from TTS (Kokoro by default, IndexTTS opt-in).
//! - [`timing`]: the take's word times, in order of preference: the TTS's
//!   own, forced alignment of the known text, else [`envelope`]'s spread of
//!   the words over the take's speech.
//! - [`transcribe`]: words of a source without a script (whisper, through
//!   the karaoke alignment core).
//! - [`lyrics`]: the `vj-lyrics` JSON, version 5 (word start and end),
//!   reading version 4 too.
//! - [`sidecar`]: SRT and WebVTT captions.
//! - [`publish`]: takes as `Audio` assets (with their word times) and
//!   transcripts as text assets.
//!
//! Everything here runs on a worker: models load and run synchronously, and
//! nothing touches UI or the platform loop.

pub mod envelope;
pub mod lyrics;
#[cfg(feature = "publish")]
pub mod publish;
pub mod sidecar;
pub mod synth;
pub mod timing;
#[cfg(feature = "transcribe")]
pub mod transcribe;
pub mod wav;

pub use lyrics::{Transcript, TranscriptLine};
pub use makepad_story_cut::{take_key, VoiceLine, VoiceSpec, Word};
pub use synth::{generate_line, Take, VoiceError};
pub use timing::{word_timings, ForcedAligner, TimingSource};

/// Whether `line` needs (re)generating in `voice`: it has no take, or its
/// take was made for other text or another voice (as with an app run's
/// render, a take is redone only when what it was made from changed).
pub fn stale(line: &VoiceLine, voice: &VoiceSpec) -> bool {
    line.stale(voice)
}
