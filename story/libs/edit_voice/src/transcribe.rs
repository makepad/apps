//! Words of a source without a script: local whisper
//! (large-v3-turbo), through the karaoke alignment core of
//! `makepad-audio-lyrics` (cross-attention DTW, teacher-forced windows,
//! onset snapping), keeping every word's end. Speech needs no source
//! separation: the mono mixdown goes in as the "vocals".
//!
//! This is `LyricsBaker::bake_with` with the aligned words kept: the baker
//! returns lines with word starts only and keeps its model private, so the
//! same passes run here on a model of our own.

use crate::lyrics::{Transcript, TranscriptLine};
#[cfg(feature = "align")]
use crate::timing::ForcedAligner;
use makepad_ai_speech::whisper::{WhisperModel, WhisperParams, WhisperState};
use makepad_audio_lyrics::align::{self, OnsetPreset, PipelineConfig};
use makepad_story_cut::Word;
use std::path::{Path, PathBuf};

// Workers own these: a model type that stops being Send fails here.
const _: () = {
    fn send<T: Send>() {}
    let _ = send::<Whisper>;
};

/// The whisper checkpoint Stage uses (as `engine/src/models.rs`).
pub const WHISPER_MODEL_FILE: &str = "ggml-large-v3-turbo.bin";

/// A loaded whisper model and its decode state, reused across sources.
pub struct Whisper {
    model: Box<WhisperModel>,
    state: WhisperState,
    model_name: String,
}

impl Whisper {
    /// `MAKEPAD_VOICE_MODEL`, for tests and tools. An app passes the path
    /// its model installer vouches for (Stage: `engine::models::
    /// whisper_model_path()`, which checks the pinned size) to [`Self::open`];
    /// this crate does not guess where a model is installed.
    pub fn env_model_path() -> Option<PathBuf> {
        std::env::var_os("MAKEPAD_VOICE_MODEL").map(PathBuf::from).filter(|path| path.is_file())
    }

    pub fn open(path: &Path) -> Result<Whisper, String> {
        let text = path.to_string_lossy().to_string();
        let model = WhisperModel::load_file(&text).map_err(|error| format!("whisper model: {error}"))?;
        let state = WhisperState::new(&model);
        let model_name = path.file_name().map_or(text, |name| name.to_string_lossy().to_string());
        Ok(Whisper { model: Box::new(model), state, model_name })
    }

    /// Transcribe mono `pcm` at `rate` Hz spoken in `language` (a whisper
    /// code, `"en"`): lines with word starts and ends in seconds of the
    /// source. `None` when nothing was said.
    pub fn transcribe(&mut self, pcm: &[f32], rate: u32, language: &str) -> Option<Transcript> {
        if pcm.is_empty() || rate == 0 {
            return None;
        }
        let duration = pcm.len() as f64 / rate as f64;
        let samples_16k = align::resample(pcm, rate as f64, align::WHISPER_RATE);
        let analysis = align::analyze_vocals(pcm, rate as f64, OnsetPreset::Snapping);
        let mut params = WhisperParams::default();
        params.language = language.to_string();
        params.no_timestamps = false;
        params.single_segment = false;
        params.temperature = 0.0;
        params.suppress_blank = true;
        let aligned = self.state.transcribe_aligned(&self.model, &samples_16k, &params);
        let config = PipelineConfig { language: language.to_string(), force: true, snap: true, max_tokens: 0 };
        let (segments, lines) = align::refine(&mut self.state, &self.model, &samples_16k, aligned, &analysis, duration, &config);
        if lines.is_empty() {
            return None;
        }
        // `assemble_lines` hands every aligned word, in order, to exactly
        // one line: walk them again to recover the ends it dropped.
        let mut aligned_words = segments.iter().flat_map(|segment| segment.words.iter());
        // Each line's words are checked against its text; a line that does
        // not match keeps its starts only, so a mismatch never shifts the
        // ends of later lines onto the wrong words.
        let lines = lines.into_iter().map(|line| {
            let texts = line.text.split_whitespace().collect::<Vec<_>>();
            let taken = aligned_words.by_ref().take(line.words.len()).collect::<Vec<_>>();
            let matches = taken.len() == texts.len()
                && taken.iter().map(|word| word.text.trim()).collect::<Vec<_>>().join(" ") == texts.join(" ");
            let words = if matches && !texts.is_empty() {
                texts.iter().zip(&taken).map(|(text, word)| Word { text: text.to_string(), start: word.start, end: word.end.max(word.start) }).collect()
            } else if line.words.len() == texts.len() {
                // Starts only: a word lasts until the next starts.
                texts.iter().enumerate().map(|(i, text)| Word { text: text.to_string(), start: line.words[i], end: line.words.get(i + 1).copied().unwrap_or(line.end) }).collect()
            } else {
                Vec::new()
            };
            TranscriptLine { start: line.start, end: line.end, text: line.text, words, confident: line.confident }
        }).collect();
        Some(Transcript { backend: "whisper".into(), model: self.model_name.clone(), language: language.to_string(), duration_secs: duration, timing: "whisper".into(), lines, phones: Vec::new() })
    }
}

/// Forced alignment of known text, through
/// `WhisperState::align_text`.
#[cfg(feature = "align")]
impl ForcedAligner for Whisper {
    fn align(&mut self, text: &str, pcm: &[f32], rate: u32, language: &str) -> Result<Vec<Word>, String> {
        let samples_16k = align::resample(pcm, rate as f64, align::WHISPER_RATE);
        let words = self.state.align_text(&self.model, &samples_16k, text, language).map_err(|error| error.to_string())?;
        Ok(words.into_iter().map(|word| Word { text: word.text, start: word.start, end: word.end }).collect())
    }
}
