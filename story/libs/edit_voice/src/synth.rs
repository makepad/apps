//! A voiceover line's take: the line's text spoken by its
//! voice, with word times. Kokoro-82M by default (Apache-2.0 weights, 24
//! kHz); IndexTTS voice cloning behind the `indextts` feature (bilibili
//! Model Use License: restricted, review before production or commercial
//! use).
//!
//! The speaker runs in-process on the calling (worker) thread, like the AI
//! Hub's `kokoro` backend, which wraps the same engine: an offline take is
//! one synchronous call, not a live TTS session. A [`Synthesizer`] keeps
//! the loaded voice between lines.

use crate::lyrics::Transcript;
use crate::timing::{word_timings, ForcedAligner, TimingSource};
use makepad_story_cut::{take_key, VoiceEngine, VoiceSpec, Word};
use std::path::PathBuf;

/// A generated take: mono PCM and its word times.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub pcm: Vec<f32>,
    pub rate: u32,
    /// One per whitespace word of the text, seconds from the take's start.
    pub words: Vec<Word>,
    pub timing: TimingSource,
    /// [`take_key`] of the text and voice it was made from: the line's
    /// `key` once the take is pinned.
    pub key: String,
    /// Every phoneme (IPA in `text`), seconds from the take's start, when
    /// the engine timed them (Kokoro does); empty otherwise. Lip sync reads
    /// them.
    pub phones: Vec<Word>,
}

impl Take {
    pub fn duration(&self) -> f64 {
        if self.rate == 0 { 0.0 } else { self.pcm.len() as f64 / self.rate as f64 }
    }

    /// The take's `vj-lyrics` v5 document.
    pub fn lyrics(&self, text: &str, voice: &VoiceSpec) -> Transcript {
        let mut doc = Transcript::of_take(text, self.words.clone(), self.duration(), self.timing, voice.engine.id(), &voice.id, voice.language());
        doc.phones = self.phones.clone();
        doc
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum VoiceError {
    /// The line has no words.
    EmptyText,
    /// This build has no such engine (its cargo feature is off).
    EngineUnavailable(VoiceEngine),
    /// The engine's weights, a voice pack or a reference are missing.
    Weights(String),
    /// The engine ran and failed.
    Synthesis(String),
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyText => f.write_str("the line has no words"),
            Self::EngineUnavailable(engine) => write!(f, "this build has no `{}` voice engine", engine.id()),
            Self::Weights(message) => write!(f, "voice weights: {message}"),
            Self::Synthesis(message) => write!(f, "speech synthesis: {message}"),
        }
    }
}

impl std::error::Error for VoiceError {}

/// Where the engines' weights are. Unset paths fall back to the
/// `MAKEPAD_ASSET_AI_CACHE` weight directory's `tts/` and `indextts/` (the
/// AI Hub's layout), then the speech crate's own conventions (working
/// directory, next to the executable).
#[derive(Clone, Debug, Default)]
pub struct VoicePaths {
    /// The converted Kokoro weights (`kokoro-v1_0.mktts`).
    pub kokoro_model: Option<PathBuf>,
    /// The directory of Kokoro voice packs (`<voice>.mkvoice`).
    pub kokoro_voices: Option<PathBuf>,
    /// The IndexTTS weight directory (the AI Hub's service layout).
    pub indextts: Option<PathBuf>,
}

impl VoicePaths {
    /// Paths inside one weight directory laid out as the AI Hub keeps it.
    pub fn in_weights_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self { kokoro_model: Some(dir.join("tts/kokoro-v1_0.mktts")), kokoro_voices: Some(dir.join("tts")), indextts: Some(dir.join("indextts")) }
    }

    fn from_env() -> Self {
        std::env::var_os("MAKEPAD_ASSET_AI_CACHE").map_or_else(Self::default, Self::in_weights_dir)
    }

    #[cfg_attr(not(feature = "kokoro"), allow(dead_code))]
    fn kokoro_model(&self) -> Option<PathBuf> {
        let explicit = self.kokoro_model.clone().or_else(|| Self::from_env().kokoro_model).filter(|path| path.is_file());
        #[cfg(feature = "kokoro")]
        let explicit = explicit.or_else(|| makepad_ai_speech::kokoro::model_path_if_present().map(PathBuf::from));
        explicit
    }

    #[cfg_attr(not(feature = "kokoro"), allow(dead_code))]
    fn kokoro_voice(&self, id: &str) -> Option<PathBuf> {
        let file = format!("{id}.mkvoice");
        let explicit = self.kokoro_voices.clone().or_else(|| Self::from_env().kokoro_voices).map(|dir| dir.join(&file)).filter(|path| path.is_file());
        #[cfg(feature = "kokoro")]
        let explicit = explicit.or_else(|| makepad_ai_speech::kokoro::named_voice_path_if_present(&file).map(PathBuf::from));
        explicit
    }
}

/// Makes takes. Holds the loaded speaker (one Kokoro voice at a time; the
/// IndexTTS pipeline once loaded) and an optional forced aligner for word
/// times.
#[derive(Default)]
pub struct Synthesizer {
    pub paths: VoicePaths,
    aligner: Option<Box<dyn ForcedAligner + Send>>,
    #[cfg(feature = "kokoro")]
    kokoro: Option<(String, makepad_ai_speech::kokoro::KokoroSpeaker)>,
    #[cfg(feature = "indextts")]
    indextts: Option<makepad_ai_speech::indextts_pipeline::IndexTtsPipeline>,
    /// Prepared IndexTTS voices by reference asset: preparing one runs the
    /// speaker encoders, so each reference is prepared once.
    #[cfg(feature = "indextts")]
    indextts_voices: std::collections::HashMap<makepad_story_cut::AssetRef, makepad_ai_speech::indextts_pipeline::IndexTtsVoice>,
}

// Workers own a Synthesizer: a model type that stops being Send fails here.
const _: () = {
    fn send<T: Send>() {}
    let _ = send::<Synthesizer>;
};

impl Synthesizer {
    pub fn new(paths: VoicePaths) -> Self {
        Self { paths, ..Self::default() }
    }

    /// Word times from forced alignment when the engine gives none.
    pub fn with_aligner(mut self, aligner: Box<dyn ForcedAligner + Send>) -> Self {
        self.aligner = Some(aligner);
        self
    }

    /// Speak `text` in `voice`. IndexTTS needs its reference recording:
    /// use [`Self::generate_cloned`].
    pub fn generate_line(&mut self, voice: &VoiceSpec, text: &str) -> Result<Take, VoiceError> {
        self.generate(voice, text, None)
    }

    /// Speak `text` in an IndexTTS `voice` cloned from the decoded mono
    /// `reference` recording (the voice's `reference` asset) at its rate.
    pub fn generate_cloned(&mut self, voice: &VoiceSpec, text: &str, reference: (&[f32], u32)) -> Result<Take, VoiceError> {
        self.generate(voice, text, Some(reference))
    }

    fn generate(&mut self, voice: &VoiceSpec, text: &str, reference: Option<(&[f32], u32)>) -> Result<Take, VoiceError> {
        if text.split_whitespace().next().is_none() {
            return Err(VoiceError::EmptyText);
        }
        let (pcm, rate, tts, phones) = match voice.engine {
            VoiceEngine::Kokoro => self.kokoro(voice, text)?,
            VoiceEngine::IndexTts => {
                let (pcm, rate, tts) = self.indextts(voice, text, reference)?;
                (pcm, rate, tts, Vec::new())
            }
        };
        let aligner = self.aligner.as_deref_mut().map(|aligner| aligner as &mut dyn ForcedAligner);
        let (words, timing) = word_timings(text, &pcm, rate, voice.language(), tts, aligner);
        // Phones belong with the engine's own word times; aligned or
        // estimated words would disagree with them.
        let phones = if timing == TimingSource::Tts { phones } else { Vec::new() };
        Ok(Take { pcm, rate, words, timing, key: take_key(voice, text), phones })
    }

    #[cfg(feature = "kokoro")]
    fn kokoro(&mut self, voice: &VoiceSpec, text: &str) -> Result<(Vec<f32>, u32, Option<Vec<Word>>, Vec<Word>), VoiceError> {
        use makepad_ai_speech::kokoro::KokoroSpeaker;
        if self.kokoro.as_ref().map_or(true, |(id, _)| id != &voice.id) {
            self.kokoro = None;
            let model = self.paths.kokoro_model().ok_or_else(|| VoiceError::Weights("kokoro-v1_0.mktts not found (set MAKEPAD_ASSET_AI_CACHE or VoicePaths)".into()))?;
            let pack = self.paths.kokoro_voice(&voice.id).ok_or_else(|| VoiceError::Weights(format!("voice pack {}.mkvoice not found", voice.id)))?;
            let speaker = KokoroSpeaker::load_with_voice(&model.to_string_lossy(), &pack.to_string_lossy()).map_err(|error| VoiceError::Weights(format!("{error:?}")))?;
            self.kokoro = Some((voice.id.clone(), speaker));
        }
        let (_, speaker) = self.kokoro.as_mut().unwrap();
        let audio = speaker.synthesize_with_speed(text, voice.speed as f32).map_err(|error| VoiceError::Synthesis(format!("{error:?}")))?;
        #[cfg(feature = "tts-timings")]
        let tts = audio.timings.as_ref().map(|timings| timings.iter().map(|t| Word { text: t.word.clone(), start: t.start, end: t.end }).collect());
        #[cfg(not(feature = "tts-timings"))]
        let tts = None;
        let phones = audio.phones.as_ref().map_or_else(Vec::new, |phones| phones.iter().map(|p| Word { text: p.phone.clone(), start: p.start, end: p.end }).collect());
        Ok((audio.samples, audio.sample_rate, tts, phones))
    }

    #[cfg(not(feature = "kokoro"))]
    fn kokoro(&mut self, _: &VoiceSpec, _: &str) -> Result<(Vec<f32>, u32, Option<Vec<Word>>, Vec<Word>), VoiceError> {
        Err(VoiceError::EngineUnavailable(VoiceEngine::Kokoro))
    }

    #[cfg(feature = "indextts")]
    fn indextts(&mut self, voice: &VoiceSpec, text: &str, reference: Option<(&[f32], u32)>) -> Result<(Vec<f32>, u32, Option<Vec<Word>>), VoiceError> {
        use makepad_ai_speech::indextts::INDEXTTS_SAMPLE_RATE;
        use makepad_ai_speech::indextts_pipeline::{IndexTtsPipeline, IndexTtsSynthesisParams, IndexTtsWeightPaths};
        let (samples, rate) = reference.ok_or_else(|| VoiceError::Weights("IndexTTS clones a reference recording: use generate_cloned".into()))?;
        if self.indextts.is_none() {
            let dir = self.paths.indextts.clone().or_else(|| VoicePaths::from_env().indextts).ok_or_else(|| VoiceError::Weights("no IndexTTS weight directory".into()))?;
            let pipeline = IndexTtsPipeline::load(&IndexTtsWeightPaths::service_layout(&dir), None).map_err(|error| VoiceError::Weights(error.to_string()))?;
            self.indextts = Some(pipeline);
        }
        let pipeline = self.indextts.as_ref().unwrap();
        let prepare = || pipeline.prepare_voice(samples, rate, None).map_err(|error| VoiceError::Synthesis(error.to_string()));
        let fresh;
        let prepared = match &voice.reference {
            Some(reference) => {
                if !self.indextts_voices.contains_key(reference) {
                    self.indextts_voices.insert(reference.clone(), prepare()?);
                }
                &self.indextts_voices[reference]
            }
            None => {
                fresh = prepare()?;
                &fresh
            }
        };
        let params = IndexTtsSynthesisParams { speed: voice.speed as f32, ..IndexTtsSynthesisParams::default() };
        let pcm = pipeline.synthesize(prepared, text, &params, None).map_err(|error| VoiceError::Synthesis(error.to_string()))?;
        Ok((pcm, INDEXTTS_SAMPLE_RATE, None))
    }

    #[cfg(not(feature = "indextts"))]
    fn indextts(&mut self, _: &VoiceSpec, _: &str, _: Option<(&[f32], u32)>) -> Result<(Vec<f32>, u32, Option<Vec<Word>>), VoiceError> {
        Err(VoiceError::EngineUnavailable(VoiceEngine::IndexTts))
    }
}

/// One take with default weight paths and no aligner. A caller making
/// several keeps a [`Synthesizer`], so the voice loads once.
pub fn generate_line(voice: &VoiceSpec, text: &str) -> Result<Take, VoiceError> {
    Synthesizer::default().generate_line(voice, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_lines_and_missing_engines_fail_cleanly() {
        let mut synth = Synthesizer::new(VoicePaths { kokoro_model: Some("/nonexistent/k.mktts".into()), ..VoicePaths::default() });
        assert_eq!(synth.generate_line(&VoiceSpec::default(), "  "), Err(VoiceError::EmptyText));
        let indextts = VoiceSpec { engine: VoiceEngine::IndexTts, ..VoiceSpec::default() };
        assert!(matches!(synth.generate_line(&indextts, "hi"), Err(VoiceError::EngineUnavailable(VoiceEngine::IndexTts) | VoiceError::Weights(_))));
    }

    #[test]
    fn weight_paths_follow_the_hub_layout() {
        let paths = VoicePaths::in_weights_dir("/w");
        assert_eq!(paths.kokoro_model, Some(PathBuf::from("/w/tts/kokoro-v1_0.mktts")));
        assert_eq!(paths.kokoro_voices, Some(PathBuf::from("/w/tts")));
        assert_eq!(paths.indextts, Some(PathBuf::from("/w/indextts")));
    }
}
