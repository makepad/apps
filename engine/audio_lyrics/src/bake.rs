//! The whole lyrics bake as one callable: vocals stem in, [`TrackLyrics`]
//! out. The shared implementation behind Stage's music bake and
//! (via the same align core) the VJ's live bake.
//!
//! Whisper transcribes with cross-attention capture, then [`crate::align`]
//! refines: teacher-forced windows, onset snapping, the sanity layer.
//! Confidence lands in the document exactly as the audit defends it — the
//! producer declares, readers decide how to display.

use crate::align::{self, ForceWindow, OnsetPreset, PipelineConfig};
use makepad_ai_speech::whisper::{AlignedSegment, WordSpan};
use crate::schema::{LyricLine, OnsetStats, TrackLyrics};
use std::path::Path;

/// A loaded whisper model plus its decode state, reusable across tracks.
pub struct LyricsBaker {
    model: Box<makepad_ai_speech::whisper::WhisperModel>,
    state: makepad_ai_speech::whisper::WhisperState,
    model_name: String,
}

impl LyricsBaker {
    /// Load the checkpoint at `path` (a ggml whisper file).
    pub fn open(path: &Path) -> Result<LyricsBaker, String> {
        let text = path.to_string_lossy().to_string();
        let model = makepad_ai_speech::whisper::WhisperModel::load_file(&text)
            .map_err(|error| format!("whisper model: {error}"))?;
        let state = makepad_ai_speech::whisper::WhisperState::new(&model);
        let model_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or(text);
        Ok(LyricsBaker { model: Box::new(model), state, model_name })
    }

    /// Bake lyrics from a mono vocals stem at `rate` Hz. `duration_secs` is
    /// the full track length (lines are clamped to it). Returns `None` when
    /// nothing was sung.
    pub fn bake(
        &mut self,
        vocals_mono: &[f32],
        rate: f64,
        duration_secs: f64,
        language: &str,
    ) -> Option<TrackLyrics> {
        let config =
            PipelineConfig { language: language.to_string(), force: true, snap: true, max_tokens: 0 };
        self.bake_with(vocals_mono, rate, duration_secs, &config)
    }

    /// A known lyric sheet timed against the song that sings it
    /// ([`crate::sung`]): `mono` the vocal (or the mix) at `rate` Hz.
    pub fn align_sheet(
        &mut self,
        mono: &[f32],
        rate: f64,
        duration_secs: f64,
        sheet: &crate::sung::Sheet,
        language: &str,
    ) -> crate::sung::SungLyrics {
        crate::sung::align_sheet(&mut self.state, &self.model, mono, rate, duration_secs, sheet, language)
    }

    /// [`bake`](Self::bake) with the refinement passes chosen: a caller
    /// working against the clock (live karaoke) may skip the teacher-forced
    /// windows (`force: false`), which re-run the decoder per segment.
    pub fn bake_with(
        &mut self,
        vocals_mono: &[f32],
        rate: f64,
        duration_secs: f64,
        config: &PipelineConfig,
    ) -> Option<TrackLyrics> {
        let language = config.language.as_str();
        let samples_16k = align::resample(vocals_mono, rate, align::WHISPER_RATE);
        let analysis = align::analyze_vocals(vocals_mono, rate, OnsetPreset::Snapping);
        let mut params = makepad_ai_speech::whisper::WhisperParams::default();
        params.language = language.to_string();
        params.no_timestamps = false;
        params.single_segment = false;
        params.temperature = 0.0;
        params.suppress_blank = true;
        params.max_tokens = config.max_tokens;
        let aligned = self.state.transcribe_aligned(&self.model, &samples_16k, &params);
        let (segments, timed_lines) = align::refine(
            &mut self.state,
            &self.model,
            &samples_16k,
            aligned,
            &analysis,
            duration_secs,
            config,
        );
        document(&segments, timed_lines, &self.model_name, language, duration_secs)
    }
}

/// A whisper in another process (the AI hub): the bake's two model passes
/// as calls, the rest of the pipeline here, identical to [`LyricsBaker`]'s.
pub trait RemoteWhisper {
    /// Pass 1: the word-timed transcription of the whole 16 kHz mixdown
    /// (greedy, no prompt, the model's own token limit).
    fn transcribe_aligned(&mut self, samples_16k: &[f32], language: &str) -> Result<Vec<AlignedSegment>, String>;
    /// Pass 2: every window's tokens forced over its audio, in order.
    fn force_align(&mut self, samples_16k: &[f32], windows: &[ForceWindow], language: &str) -> Result<Vec<Option<Vec<WordSpan>>>, String>;
}

/// [`LyricsBaker::bake`] against a [`RemoteWhisper`] named `model_name`.
/// Returns `None` when nothing was sung.
pub fn bake_remote(
    whisper: &mut dyn RemoteWhisper,
    model_name: &str,
    vocals_mono: &[f32],
    rate: f64,
    duration_secs: f64,
    language: &str,
) -> Result<Option<TrackLyrics>, String> {
    let config = PipelineConfig { language: language.to_string(), force: true, snap: true, max_tokens: 0 };
    let samples_16k = align::resample(vocals_mono, rate, align::WHISPER_RATE);
    let analysis = align::analyze_vocals(vocals_mono, rate, OnsetPreset::Snapping);
    let aligned = whisper.transcribe_aligned(&samples_16k, language)?;
    let (segments, timed_lines) = align::refine_with(samples_16k.len(), aligned, &analysis, duration_secs, &config, &mut |windows| {
        whisper.force_align(&samples_16k, windows, language)
    })?;
    Ok(document(&segments, timed_lines, model_name, language, duration_secs))
}

/// The lyrics document of a refined transcription, with the audit stats of
/// what the onset snap moved; `None` when no line was timed.
fn document(
    segments: &[align::SegmentWords],
    timed_lines: Vec<align::TimedLine>,
    model_name: &str,
    language: &str,
    duration_secs: f64,
) -> Option<TrackLyrics> {
    if timed_lines.is_empty() {
        return None;
    }
    let mut snapped = 0usize;
    let mut total_ms = 0.0f64;
    let mut max_ms = 0.0f64;
    for segment in segments {
        for word in &segment.words {
            if let Some(delta) = word.snap {
                snapped += 1;
                total_ms += delta.abs() * 1000.0;
                max_ms = max_ms.max(delta.abs() * 1000.0);
            }
        }
    }
    let lines: Vec<LyricLine> = timed_lines
        .into_iter()
        .map(|line| LyricLine {
            start_secs: line.start,
            end_secs: line.end,
            text: line.text,
            words: line.words,
            syllables: line.syllables,
            // The producer's confidence, undiluted: display policy (hop
            // vs sweep) belongs to readers.
            confident: line.confident,
        })
        .collect();
    Some(TrackLyrics {
        backend: "whisper".into(),
        model: model_name.to_string(),
        language: language.to_string(),
        duration_secs,
        onset: OnsetStats { snapped, mean_ms: if snapped > 0 { total_ms / snapped as f64 } else { 0.0 }, max_ms },
        lines,
    })
}
