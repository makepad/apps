//! The song, and the film's clock: the song on a deck of Stage's audio
//! engine (`audio.splash`: one `AudioDeck` into the device), the same
//! engine, deck and decoder on the desktop and in the browser (there in the
//! platform's AudioWorklet, a threaded build).
//!
//! A finger on the picture is a hand on the record ([`crate::scratch`]): a
//! touch brakes it to a stop (`brake`), a drag moves it with the finger
//! (`scratch`, `scratch_at`), and a release lets it go back into play: at
//! the fling's speed and direction (`fling`, `speed` ±1), easing to normal
//! play forwards or backwards, or plainly forwards, unless the user paused.
//! The deck stops softly at the song's start.
//!
//! The picture shows what is heard: the deck's position as the engine last
//! rendered it, moved on by its speed since (both on the engine's clock).
//! Where no device runs (before the browser's first tap, a muted instance)
//! the engine renders silently here, so the film moves the same way.

use crate::job::Job;
use makepad_stage_audio_media::pcm::TrackPcm;
use makepad_stage_live_audio::clock;
use makepad_stage_live_audio::embedded::{embedded, EmbeddedAudio};
use makepad_stage_live_audio::{AudioEngine, DeckCommand};
use makepad_widgets::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const FLOW: &str = include_str!("audio.splash");
const DECK: &str = "deck";
/// A film without a song plays a silent clip at this rate.
const SILENT_RATE: u32 = 8000;

/// The decoded song, until the transport takes it.
pub struct Song {
    pub pcm: Option<TrackPcm>,
}

/// Decode the song's bytes with Stage's decoder, off the UI thread (a
/// worker in the browser: seconds of work for a song).
pub fn decode(cx: &mut Cx, bytes: crate::bundle::Bytes) -> Job<Result<TrackPcm, String>> {
    Job::run(cx, move || {
        let audio = makepad_audio_decode::decode_any(&bytes).map_err(|e| format!("the song: {e:?}"))?;
        let mut frames = Vec::with_capacity(audio.frames());
        makepad_stage_audio_media::decode::push_interleaved(&mut frames, &audio.pcm_interleaved_f32, audio.channels.max(1) as usize);
        Ok(TrackPcm { frames, sample_rate: audio.rate.max(1) })
    })
}

pub struct Transport {
    host: Option<EmbeddedAudio>,
    engine: Arc<Mutex<Option<AudioEngine>>>,
    /// When the device last rendered (engine clock, f64 bits).
    device_at: Arc<AtomicU64>,
    device: bool,
    loaded: u64,
    duration: f64,
    /// The user's own pause (a tap, the button, space).
    paused: bool,
    /// Where the hand took hold (film seconds).
    anchor: f64,
    held: bool,
    /// Released into backwards play.
    backwards: bool,
    t: f64,
    /// A cue sent, with the deck's cue count then and when: observations
    /// wait for it.
    cue: Option<(u32, f64)>,
    rendered_at: f64,
}

impl Default for Transport {
    fn default() -> Self {
        let (host, engine) = match embedded(FLOW, "film/audio.splash", Vec::new()) {
            Ok((host, engine)) => (Some(host), Some(engine)),
            Err(e) => {
                error!("player: the audio flow: {e}");
                (None, None)
            }
        };
        Transport {
            host,
            engine: Arc::new(Mutex::new(engine)),
            device_at: Arc::new(AtomicU64::new(0)),
            device: false,
            loaded: 0,
            duration: 0.0,
            paused: true,
            anchor: 0.0,
            held: false,
            backwards: false,
            t: 0.0,
            cue: None,
            rendered_at: 0.0,
        }
    }
}

impl Transport {
    /// Open the device (once): the engine renders in its callback. A muted
    /// instance (MAKEPAD_MUTE=1: hidden test runs) opens none.
    fn open(&mut self, cx: &mut Cx) {
        if self.device {
            return;
        }
        self.device = true;
        if std::env::var("MAKEPAD_MUTE").is_ok_and(|v| v == "1") {
            log!("player: muted (MAKEPAD_MUTE=1): no audio device");
            return;
        }
        let engine = self.engine.clone();
        let device_at = self.device_at.clone();
        cx.audio_output(0, move |info, buffer| {
            // Never waits: the UI holds the engine only while no device runs.
            let Ok(mut engine) = engine.try_lock() else {
                buffer.zero();
                return;
            };
            let Some(engine) = engine.as_mut() else {
                buffer.zero();
                return;
            };
            if buffer.channel_count() >= 2 {
                let (left, right) = buffer.stereo_mut();
                engine.render(info.sample_rate, left, right);
            } else {
                engine.render_silent(info.sample_rate, buffer.frame_count());
                buffer.zero();
            }
            device_at.store(clock::now().to_bits(), Ordering::Release);
        });
    }

    /// The film's song (or none: a silent clip as long as the film), once
    /// the film's length is known.
    pub fn attach(&mut self, cx: &mut Cx, song: Option<&mut Song>, duration: f64) {
        if self.loaded != 0 || duration <= 0.0 {
            return;
        }
        let Some(host) = &mut self.host else { return };
        let mut pcm = song.and_then(|song| song.pcm.take()).unwrap_or(TrackPcm { frames: Vec::new(), sample_rate: SILENT_RATE });
        // The deck stops at its clip's end: as long as the film at least.
        let frames = (duration * pcm.sample_rate as f64).ceil() as usize + 1;
        // Grown to its final length at once (a resize from an exact
        // capacity doubled it: a 57 MB buffer and a copy back for a
        // 156 s song).
        if pcm.frames.len() < frames {
            pcm.frames.reserve_exact(frames - pcm.frames.len());
            pcm.frames.resize(frames, [0, 0]);
        }
        pcm.frames.shrink_to_fit();
        let track = host.reserve_track(pcm.frames.capacity() * 4).and_then(|lease| lease.finish(pcm));
        match track.and_then(|track| host.load(DECK, DeckCommand::Load { sequence: 1, track })) {
            Ok(()) => self.loaded = 1,
            Err(e) => error!("player: the song: {e}"),
        }
        self.duration = duration;
        self.open(cx);
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // The web opens the browser's output on the app's ask.
        #[cfg(target_arch = "wasm32")]
        if let Event::AudioDevices(devices) = event {
            cx.use_audio_outputs(&devices.default_output());
        }
        let _ = (cx, event);
    }

    fn set(&mut self, param: &str, value: f32) {
        if let Some(host) = &mut self.host {
            host.set(DECK, param, value);
        }
    }

    /// Playing (or meaning to: the user has not paused).
    pub fn playing(&self) -> bool {
        !self.paused
    }

    pub fn play(&mut self) {
        if self.duration > 0.0 && self.t >= self.duration - 0.05 {
            self.seek(0.0);
        }
        self.paused = false;
        self.held = false;
        self.backwards = false;
        self.set("speed", 1.0);
        self.set("fling", 0.0);
        self.set("scratch", 0.0);
        self.set("play", 1.0);
    }

    pub fn pause(&mut self) {
        self.paused = true;
        self.set("play", 0.0);
    }

    pub fn seek(&mut self, t: f64) {
        let t = if self.duration > 0.0 { t.clamp(0.0, self.duration) } else { t.max(0.0) };
        self.t = t;
        if let Some(host) = &mut self.host {
            let epoch = host.deck(DECK).map_or(0, |d| d.deck.seek_epoch);
            if host.cue(DECK, t).is_ok() {
                self.cue = Some((epoch, clock::now()));
            }
        }
        if self.held {
            self.anchor = t;
            self.set("scratch_at", t as f32);
        }
    }

    /// A finger down: on the picture the record brakes (`brake`), on the
    /// timeline it stops dead under the hand.
    pub fn press(&mut self, brake: bool) {
        self.held = true;
        self.anchor = self.t;
        self.set("brake", if brake { 1.0 } else { 0.0 });
        self.set("scratch_at", self.anchor as f32);
        self.set("scratch", 1.0);
    }

    /// The finger `offset` film seconds from where it went down.
    pub fn drag(&mut self, offset: f64) {
        if self.held {
            self.set("scratch_at", (self.anchor + offset).max(0.0) as f32);
        }
    }

    /// The finger up. A tap (no drag) plays or pauses; after a drag the
    /// record goes back into play (at a fling's speed and direction, easing
    /// to ±1, else forwards) unless the user paused.
    pub fn release(&mut self, dragged: bool, fling: Option<f64>) {
        self.held = false;
        if !dragged {
            self.paused = !self.paused;
        }
        let fling = fling.filter(|_| dragged).unwrap_or(0.0);
        let backwards = fling < 0.0 && !self.paused;
        self.backwards = backwards;
        self.set("fling", fling as f32);
        self.set("speed", if backwards { -1.0 } else { 1.0 });
        if !self.paused && !backwards && self.duration > 0.0 && self.t >= self.duration - 0.05 {
            self.seek(0.0);
        }
        self.set("play", if self.paused { 0.0 } else { 1.0 });
        self.set("scratch", 0.0);
    }

    /// The film's time now: the deck as last rendered, moved on by its speed
    /// since. Renders silently here while no device runs.
    pub fn tick(&mut self, dt: f64) -> f64 {
        let now = clock::now();
        let Some(host) = &mut self.host else { return self.t };
        if let Err(e) = host.pump() {
            error!("player: {e}");
        }
        if now - f64::from_bits(self.device_at.load(Ordering::Acquire)) > 0.25 {
            // At the device's rate once it ran, so the flow is not prepared
            // again for every switch.
            let rate = host.rate().unwrap_or(48000) as f64;
            let seconds = if self.rendered_at > 0.0 { now - self.rendered_at } else { dt };
            let frames = (seconds.clamp(0.0, 0.1) * rate).round().max(1.0) as usize;
            if let Ok(mut engine) = self.engine.try_lock() {
                if let Some(engine) = engine.as_mut() {
                    engine.render_silent(rate, frames);
                }
            }
            self.rendered_at = now;
            let _ = host.pump();
        }
        if let Some(d) = host.deck(DECK).filter(|d| self.loaded != 0 && d.deck.sequence == self.loaded && d.deck.source_rate > 0) {
            let waiting = self.cue.is_some_and(|(epoch, sent)| d.deck.seek_epoch == epoch && now - sent < 0.5);
            if !waiting {
                self.cue = None;
                let rate = d.deck.source_rate as f64;
                let speed = match (d.deck.advancing, d.deck.source_frames_per_output_frame) {
                    (Some(true), Some(step)) => step * d.rate as f64 / rate,
                    _ => 0.0,
                };
                let since = d.at.map_or(0.0, |at| (now - at).clamp(0.0, 0.1));
                self.t = d.deck.position / rate + speed * since;
                // Backwards to the start: paused there.
                if self.backwards && !self.paused && !self.held && self.t <= 1e-3 {
                    self.pause();
                    self.backwards = false;
                    self.set("speed", 1.0);
                }
            }
        }
        if self.duration > 0.0 {
            self.t = self.t.clamp(0.0, self.duration);
            // Played to the end: paused there.
            if !self.paused && !self.held && self.t >= self.duration - 1e-3 {
                self.pause();
            }
        }
        self.t
    }
}
