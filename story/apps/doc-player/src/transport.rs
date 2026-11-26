//! The viewer's sound, and the film's clock: the cut's whole sound as a
//! clip on a deck of Stage's audio engine (one `AudioDeck` into the
//! device), the old film player's transport (`story/apps/player/src/audio.rs`)
//! with the cut's mix in place of its song.
//!
//! A finger on the picture is a hand on the record ([`crate::scratch`]): a
//! touch brakes it to a stop (`brake`), a drag moves it with the finger
//! (`scratch`, `scratch_at`), and a release lets it go back into play: at
//! the fling's speed and direction (`fling`, `speed` ±1), easing to normal
//! play forwards or backwards, or plainly forwards, unless the user paused.
//! The deck stops softly at the clip's start. Scratching needs the sound at
//! random access, so the clip is the whole mix, made once up front
//! ([`crate::sound::clip_job`]); a cut without sound plays a silent clip.
//!
//! The picture shows what is heard: the deck's position as the engine last
//! rendered it, moved on by its speed since that position is heard (both on
//! the engine's clock). A device that says when a block is heard
//! (`AudioTime::presented_in`: the browser without threads, whose UI thread
//! renders tens of milliseconds ahead of a worklet) puts the rendered
//! position ahead of the ear by that much; one that does not counts as heard
//! when rendered.
//! Where no device runs (before the browser's first gesture, a muted
//! instance) the engine renders silently here, so the film moves the same
//! way.

use makepad_stage_audio_media::pcm::TrackPcm;
use makepad_stage_live_audio::clock;
use makepad_stage_live_audio::embedded::{embedded, EmbeddedAudio};
use makepad_stage_live_audio::{AudioEngine, DeckCommand};
use makepad_widgets::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const FLOW: &str = r#"
use mod.flow.*
let deck = AudioDeck{ play: 0 speed: 1 }
let out = AudioOutput{ enabled: 1 audio: deck.out(@audio) }
Flow{ label: "Player sound" deck, out }
"#;
const DECK: &str = "deck";
/// A cut without sound plays a silent clip at this rate.
const SILENT_RATE: u32 = 8000;
/// The most the rendered position is taken to be ahead of the ear (s).
const HEARD_AHEAD_MAX: f64 = 1.0;

pub struct Transport {
    host: Option<EmbeddedAudio>,
    engine: Arc<Mutex<Option<AudioEngine>>>,
    /// When the device last rendered (engine clock, f64 bits).
    device_at: Arc<AtomicU64>,
    /// When the end of what was last rendered is heard (engine clock, f64 bits).
    heard_at: Arc<AtomicU64>,
    device: bool,
    /// The clip's deck sequence (0: none yet).
    loaded: u64,
    /// Seconds of the clip from its start that hold the sound (the rest is
    /// still being mixed: silence).
    built: f64,
    duration: f64,
    /// The user's own pause (a tap, the button, space).
    paused: bool,
    /// Where the hand took hold (film seconds).
    anchor: f64,
    held: bool,
    /// Released into backwards play.
    backwards: bool,
    t: f64,
    /// The deck as last observed: its position (film seconds) and speed,
    /// for a test harness's media clock.
    rendered: (f64, f64),
    /// A cue sent, with the deck's cue count then and when: observations
    /// wait for it.
    cue: Option<(u32, f64)>,
    rendered_at: f64,
}

impl Default for Transport {
    fn default() -> Self {
        let (host, engine) = match embedded(FLOW, "player/transport.splash", Vec::new()) {
            Ok((host, engine)) => (Some(host), Some(engine)),
            Err(e) => {
                error!("makepad-story-doc-player: the audio flow: {e}");
                (None, None)
            }
        };
        Transport {
            host,
            engine: Arc::new(Mutex::new(engine)),
            device_at: Arc::new(AtomicU64::new(0)),
            heard_at: Arc::new(AtomicU64::new(0)),
            device: false,
            loaded: 0,
            built: 0.0,
            duration: 0.0,
            paused: true,
            anchor: 0.0,
            held: false,
            backwards: false,
            t: 0.0,
            rendered: (0.0, 0.0),
            cue: None,
            rendered_at: 0.0,
        }
    }
}

impl Transport {
    /// Open the device (once): the engine renders in its callback. A muted
    /// or hidden instance (tests) opens none.
    fn open(&mut self, cx: &mut Cx) {
        if self.device {
            return;
        }
        self.device = true;
        if makepad_story_edit_render::sound::muted() {
            log!("makepad-story-doc-player: muted (MAKEPAD_MUTE / hidden windows): no audio device");
            return;
        }
        let engine = self.engine.clone();
        let device_at = self.device_at.clone();
        let heard_at = self.heard_at.clone();
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
            let now = clock::now();
            // The block's end is heard its length after its start is presented.
            let ahead = info.time.and_then(|time| time.presented_in()).map_or(0.0, |presented| presented + buffer.frame_count() as f64 / info.sample_rate.max(1.0));
            device_at.store(now.to_bits(), Ordering::Release);
            heard_at.store((now + ahead).to_bits(), Ordering::Release);
        });
    }

    /// Whether the clip is on the deck.
    pub fn attached(&self) -> bool {
        self.loaded != 0
    }

    /// Seconds from the start the clip holds the sound for.
    pub fn built(&self) -> f64 {
        self.built
    }

    /// The cut's sound as a clip whose first `built` seconds hold it (or
    /// none: a silent clip `duration` long), once: the clock plays to
    /// `duration` (the timeline's end).
    pub fn attach(&mut self, cx: &mut Cx, pcm: Option<TrackPcm>, built: f64, duration: f64) {
        if self.loaded != 0 || duration <= 0.0 {
            return;
        }
        self.duration = duration;
        self.built = if pcm.is_some() { built } else { duration };
        let Some(host) = &mut self.host else { return };
        let mut pcm = pcm.unwrap_or(TrackPcm { frames: Vec::new(), sample_rate: SILENT_RATE });
        // The deck stops at its clip's end: as long as the film at least.
        let frames = (duration * pcm.sample_rate as f64).ceil() as usize + 1;
        if pcm.frames.len() < frames {
            pcm.frames.reserve_exact(frames - pcm.frames.len());
            pcm.frames.resize(frames, [0, 0]);
        }
        pcm.frames.shrink_to_fit();
        let track = host.reserve_track(pcm.frames.capacity() * 4).and_then(|lease| lease.finish(pcm));
        match track.and_then(|track| host.load(DECK, DeckCommand::Load { sequence: 1, track })) {
            Ok(()) => self.loaded = 1,
            Err(e) => error!("makepad-story-doc-player: the sound: {e}"),
        }
        // Where the film was cued before the clip came (`--from`).
        let at = self.t;
        self.seek(at);
        self.open(cx);
    }

    /// The same clip with more of it mixed (as long, `built` seconds of it
    /// holding the sound now): it takes over where the deck is, under the
    /// deck's dip. Not while a hand is on the record (a swap lets go of it).
    /// Call only when [`Self::can_extend`].
    pub fn extend(&mut self, mut pcm: TrackPcm, built: f64) {
        let Some(host) = &mut self.host else { return };
        pcm.frames.shrink_to_fit();
        let sequence = self.loaded + 1;
        let track = host.reserve_track(pcm.frames.capacity() * 4).and_then(|lease| lease.finish_continuing(pcm, None, Some(self.loaded)));
        match track.and_then(|track| host.load(DECK, DeckCommand::Load { sequence, track })) {
            Ok(()) => {
                self.loaded = sequence;
                self.built = built;
            }
            Err(e) => error!("makepad-story-doc-player: the sound: {e}"),
        }
    }

    /// A longer clip may take over now: one is on the deck, the deck plays
    /// the last one sent (a clip continues only the one the deck plays), and
    /// no hand is on the record.
    pub fn can_extend(&mut self) -> bool {
        let loaded = self.loaded;
        !self.held && loaded != 0 && self.host.as_mut().and_then(|host| host.deck(DECK)).is_some_and(|d| d.deck.sequence == loaded)
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // The web opens the browser's output on the app's ask (the first
        // gesture starts it).
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

    /// The deck as last observed: (its position in film seconds at the end
    /// of what was rendered, its speed): with the film's time, a test
    /// harness's media clock.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn test_clock(&self) -> (f64, f64) {
        self.rendered
    }

    /// The film's time now: the deck as last rendered, moved on by its speed
    /// since. Renders silently here while no device runs.
    pub fn tick(&mut self, dt: f64) -> f64 {
        let now = clock::now();
        let Some(host) = &mut self.host else { return self.t };
        if let Err(e) = host.pump() {
            error!("makepad-story-doc-player: {e}");
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
            self.heard_at.store(now.to_bits(), Ordering::Release);
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
                // Negative while the rendered end is still ahead of the ear.
                let since = (now - f64::from_bits(self.heard_at.load(Ordering::Acquire))).clamp(-HEARD_AHEAD_MAX, 0.1);
                self.rendered = (d.deck.position / rate, speed);
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
