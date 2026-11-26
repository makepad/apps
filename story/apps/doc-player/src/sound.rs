//! The document's sound through Stage's audio engine.
//!
//! Playing: the cut's whole mix made into a clip ([`ClipBuilder`]), which
//! the viewer's deck plays, scratches and flings ([`crate::transport`]).
//!
//! Deterministic: the export's mixdown (`Mixdown`: the cut's mix fed into a
//! deck, rendered offline, sample for sample the render's sound), one
//! frame's share per frame; hashed, and written as a WAV when asked.

use makepad_stage_audio_media::pcm::TrackPcm;
use makepad_story_cut::mix::SAMPLE_RATE;
use makepad_story_edit_render::audio::frame_sample;
use makepad_story_edit_render::mixdown::Mixdown;
use makepad_story_cut::mix::AudioMix;
use makepad_story_edit_render::CutEngine;
use makepad_widgets::*;
use std::sync::Arc;

/// Frames of the mix one slice job mixes (a second).
const SLICE_FRAMES: usize = SAMPLE_RATE as usize;

/// The cut's whole sound becoming one clip, a slice at a time, for the
/// viewer's deck: it reads the clip anywhere, as a hand on the record
/// wants. The sources decode once (a job), then the mix is mixed at its
/// gain a second per job (one per display frame without threads), so the
/// first seconds are playable long before the whole is
/// ([`Self::snapshot`]). Its cost is the clip, 4 bytes a frame (192 KB a
/// second at 48 kHz); while it is made, the decoded sources and the deck's
/// copy of the playable part too.
pub struct ClipBuilder {
    decode: Option<FrameJob<Result<AudioMix, String>>>,
    mix: Option<Arc<AudioMix>>,
    frames: Vec<[i16; 2]>,
    total: usize,
    slice: Option<FrameJob<Vec<i16>>>,
}

impl ClipBuilder {
    /// Once the cut has loaded: its sound `seconds` long (None: the cut
    /// has no sound job).
    pub fn new(cx: &mut Cx, engine: &CutEngine, seconds: f64) -> Result<Option<Self>, String> {
        let Some(job) = engine.sound_job() else { return Ok(None) };
        let decode = cx.task_pool().submit_frame_job(Class::Background, job).map_err(|error| format!("{error:?}"))?;
        let total = (seconds.max(0.0) * SAMPLE_RATE as f64).ceil() as usize + 1;
        Ok(Some(Self { decode: Some(decode), mix: None, frames: Vec::new(), total, slice: None }))
    }

    /// Take what is done and start what is next. Call once per frame.
    pub fn pump(&mut self, cx: &mut Cx) -> Result<(), String> {
        if let Some(job) = &mut self.decode {
            match job.try_take() {
                None => return Ok(()),
                Some(result) => {
                    self.decode = None;
                    let mix = result.map_err(|error| format!("the sound's decode: {error}"))??;
                    self.mix = Some(Arc::new(mix));
                    self.frames.reserve_exact(self.total);
                }
            }
        }
        if let Some(job) = &mut self.slice {
            match job.try_take() {
                None => return Ok(()),
                Some(result) => {
                    self.slice = None;
                    let block = result.map_err(|error| format!("the sound's mix: {error}"))?;
                    self.frames.extend(block.chunks_exact(2).map(|lr| [lr[0], lr[1]]));
                }
            }
        }
        if let (Some(mix), true) = (&self.mix, self.frames.len() < self.total) {
            let (mix, first) = (mix.clone(), self.frames.len());
            let count = (self.total - first).min(SLICE_FRAMES);
            self.slice = Some(cx.task_pool().submit_frame_job(Class::Background, move || mix.mix(first as u64, count)).map_err(|error| format!("{error:?}"))?);
        }
        Ok(())
    }

    /// Seconds of the sound mixed so far, from the start.
    pub fn built(&self) -> f64 {
        self.frames.len() as f64 / SAMPLE_RATE as f64
    }

    pub fn complete(&self) -> bool {
        self.mix.is_some() && self.frames.len() >= self.total
    }

    /// The whole clip's length, the part mixed so far and silence after it.
    pub fn snapshot(&self) -> TrackPcm {
        let mut frames = Vec::new();
        frames.reserve_exact(self.total);
        frames.extend_from_slice(&self.frames);
        frames.resize(self.total, [0, 0]);
        TrackPcm { frames, sample_rate: SAMPLE_RATE }
    }

    /// The whole clip, once [`Self::complete`].
    pub fn finish(self) -> TrackPcm {
        TrackPcm { frames: self.frames, sample_rate: SAMPLE_RATE }
    }
}

/// The deterministic sound: frame `n`'s share of the export's mixdown.
pub struct Mixed {
    mixdown: Mixdown,
    fps: (u32, u32),
    pub hash: u64,
    pub samples: u64,
    wav: Option<Vec<i16>>,
}

impl Mixed {
    pub fn new(mix: makepad_story_cut::mix::AudioMix, fps: (u32, u32), keep: bool) -> Result<Self, String> {
        Ok(Self { mixdown: Mixdown::new(Arc::new(mix))?, fps, hash: 0xcbf2_9ce4_8422_2325, samples: 0, wav: keep.then(Vec::new) })
    }

    pub fn frame(&mut self, n: u64) {
        let (a, b) = (frame_sample(n, self.fps), frame_sample(n + 1, self.fps));
        let pcm = self.mixdown.mix(a, (b - a) as usize);
        for value in &pcm {
            for byte in value.to_le_bytes() {
                self.hash ^= byte as u64;
                self.hash = self.hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        self.samples += (b - a) as u64;
        if let Some(wav) = &mut self.wav {
            wav.extend_from_slice(&pcm);
        }
    }

    /// The sound so far as a 16-bit stereo WAV at the mix's rate.
    pub fn write_wav(&self, path: &std::path::Path) -> Result<(), String> {
        let pcm = self.wav.as_deref().unwrap_or(&[]);
        let data = (pcm.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        out.extend_from_slice(&(SAMPLE_RATE * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data.to_le_bytes());
        for value in pcm {
            out.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
    }
}
