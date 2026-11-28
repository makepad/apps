//! The preview's sound: the cut's mix ([`AudioMix`]) played while the CUT
//! page plays, through the host's audio engine.
//!
//! The mix evaluates the cut (it allocates, it reads documents), so it
//! never runs on the device's thread: a feeder (a long-lived worker fed by
//! a bounded channel) mixes the cut a little ahead into a [`FeedRing`],
//! and a deck of the host's audio engine plays that ring (`DeckCommand::
//! LoadFeed`: the host loads [`ring`] into its `cut_sound` deck once). A
//! start restarts the ring: the deck dips out of what it played and begins
//! exactly where the new start begins, so a seek never plays stale sound;
//! a stop restarts it empty (silence). The deck publishes how far it has
//! played, the sound's clock ([`FeedRing::played`]).
//!
//! [`CutSound`] is the player a host drives: the mix decoding on the pool
//! (the preview's own `CutEngine::sound_job`, or `engine::sound_mix` for
//! a host with no picture, the motion desk), then started, followed and
//! stopped with the host's playhead.
//!
//! A muted process (`MAKEPAD_MUTE`) is fed the same mix: the platform opens no device for it and its taps hear the mix.

use relax_attr::relax;
use makepad_story_cut::mix::AudioMix;
use makepad_stage_live_audio::feed::{FeedRing, FEED_RATE};
use makepad_widgets::*;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Ring room, in stereo frames at [`FEED_RATE`]: half a second.
const RING_FRAMES: usize = 24_000;
/// How far ahead of the deck the feeder keeps the ring.
const AHEAD_FRAMES: u64 = 4_800;
/// Frames the feeder mixes at a time.
const BLOCK_FRAMES: usize = 960;

/// The process's preview feed, for the host's `cut_sound` deck.
pub fn ring() -> &'static Arc<FeedRing> {
    static RING: OnceLock<Arc<FeedRing>> = OnceLock::new();
    RING.get_or_init(|| FeedRing::new(RING_FRAMES))
}

enum Feed {
    Mix(Arc<AudioMix>),
    /// Play from this output sample on.
    Play(u64),
    Stop,
}

/// The UI side: the mix once decoded, and the feeder worker.
pub struct PreviewSound {
    feeder: Feeding,
    /// A command the full queue handed back, sent again on the next call.
    held: Vec<Feed>,
    /// When the current start was asked, and from which edit time.
    started: Option<f64>,
}

impl PreviewSound {
    #[cfg(not(all(target_arch = "wasm32", not(target_feature = "atomics"))))]
    pub fn new(cx: &mut Cx) -> Result<Self, String> {
        let (commands, receiver) = std::sync::mpsc::sync_channel(8);
        let options = ThreadOptions { name: Some(Arc::from("edit-sound")), ..ThreadOptions::default() };
        let worker = cx.thread_spawner().spawn_worker(options, move || feed(receiver)).map_err(|e| format!("edit sound: {e:?}"))?;
        Ok(Self { feeder: Feeding::Worker { commands, _worker: worker }, held: Vec::new(), started: None })
    }

    /// Without threads the UI thread feeds: a bounded slice per frame
    /// ([`Self::pump`]), ahead of the device.
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    pub fn new(_cx: &mut Cx) -> Result<Self, String> {
        Ok(Self { feeder: Feeding::Here(Feeder::new()), held: Vec::new(), started: None })
    }

    fn send(&mut self, feed: Feed) {
        let commands = match &mut self.feeder {
            Feeding::Worker { commands, .. } => commands,
            Feeding::Here(feeder) => {
                feeder.command(feed);
                return;
            }
        };
        self.held.push(feed);
        let mut held = std::mem::take(&mut self.held).into_iter();
        while let Some(feed) = held.next() {
            match commands.try_send(feed) {
                Ok(()) => {}
                Err(TrySendError::Full(feed)) => {
                    log!("edit sound: the feeder's queue is full; retried on the next call");
                    self.held.push(feed);
                    self.held.extend(held);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }

    pub fn set_mix(&mut self, mix: Arc<AudioMix>) {
        self.send(Feed::Mix(mix));
    }

    /// Sound from cut time `t` on (real-time forward play only).
    pub fn play(&mut self, t: f64) {
        self.started = Some(t);
        self.send(Feed::Play((t.max(0.0) * FEED_RATE as f64).round() as u64));
    }

    pub fn stop(&mut self) {
        if self.started.take().is_some() {
            self.send(Feed::Stop);
        }
    }

    pub fn playing(&self) -> bool {
        self.started.is_some()
    }

    /// The cut time the sound has reached, while it plays.
    pub fn clock(&self) -> Option<f64> {
        self.started.map(|t| t + ring().played() as f64 / FEED_RATE as f64)
    }

    /// Commands the queue handed back; without threads, the feed's slice
    /// for this frame.
    pub fn pump(&mut self) {
        if let Feeding::Here(feeder) = &mut self.feeder {
            feeder.fill();
        }
        if !self.held.is_empty() {
            let held = std::mem::take(&mut self.held);
            for feed in held {
                self.send(feed);
            }
        }
    }
}

/// The cut's sound for a host's playhead: its mix decoding on the pool,
/// then played through [`PreviewSound`].
pub struct CutSound {
    sound: PreviewSound,
    decode: Option<FrameJob<Result<AudioMix, String>>>,
}

/// How far the sound may be from the playhead before it starts again there.
const FOLLOW_SLACK: f64 = 0.25;

impl CutSound {
    /// Decode the mix `job` makes (on the pool) and get ready to play it.
    pub fn new(cx: &mut Cx, job: impl FnOnce() -> Result<AudioMix, String> + Send + 'static) -> Result<Self, String> {
        let sound = PreviewSound::new(cx)?;
        let decode = Some(cx.task_pool().submit_frame_job(Class::Background, job).map_err(|error| format!("edit sound: {error:?}"))?);
        Ok(Self { sound, decode })
    }

    /// The decoded mix once it is there, and commands the queue handed
    /// back. Call often (every event or pump).
    pub fn pump(&mut self) {
        if let Some(task) = &mut self.decode {
            match task.try_take() {
                None => {}
                Some(Ok(Ok(mix))) => {
                    self.decode = None;
                    self.sound.set_mix(Arc::new(mix));
                }
                Some(Ok(Err(error))) => {
                    self.decode = None;
                    log!("edit sound: the sound did not decode: {error}");
                }
                Some(Err(error)) => {
                    self.decode = None;
                    log!("edit sound: the sound did not decode: {error:?}");
                }
            }
        }
        self.sound.pump();
    }

    /// Play along with a playhead at edit time `t` (`None`: silent). The
    /// sound starts again only when the playhead left it (a loop, a seek).
    pub fn follow(&mut self, t: Option<f64>) {
        match t {
            Some(t) => {
                if self.sound.clock().is_none_or(|clock| (clock - t).abs() > FOLLOW_SLACK) {
                    self.sound.play(t);
                }
            }
            None => self.sound.stop(),
        }
    }

    /// Sound from cut time `t` on, from now (a scrub's touch).
    pub fn play(&mut self, t: f64) {
        self.sound.play(t);
    }

    /// The cut time the sound has reached, while it plays.
    pub fn clock(&self) -> Option<f64> {
        self.sound.clock()
    }
}

impl Drop for PreviewSound {
    fn drop(&mut self) {
        // Silent from here; the worker ends when its channel goes.
        ring().restart();
    }
}

/// Who feeds the ring: the feeder worker over its queue, or (without
/// threads) this thread, once per frame.
enum Feeding {
    #[cfg_attr(all(target_arch = "wasm32", not(target_feature = "atomics")), allow(dead_code))]
    Worker { commands: SyncSender<Feed>, _worker: TaskHandle<()> },
    #[cfg_attr(not(all(target_arch = "wasm32", not(target_feature = "atomics"))), allow(dead_code))]
    Here(Feeder),
}

/// The feed's state: the mix, where the next block starts, the meter.
struct Feeder {
    ring: &'static FeedRing,
    mix: Option<Arc<AudioMix>>,
    next: Option<u64>,
    meter: Option<FeedMeter>,
}

impl Feeder {
    fn new() -> Self {
        Self { ring: ring(), mix: None, next: None, meter: FeedMeter::new() }
    }

    fn command(&mut self, command: Feed) {
        match command {
            Feed::Mix(new) => self.mix = Some(new),
            Feed::Play(from) => {
                self.next = Some(from);
                self.ring.restart();
            }
            Feed::Stop => {
                self.next = None;
                self.ring.restart();
            }
        }
    }

    /// Mix ahead of the deck while the ring has room ([`AHEAD_FRAMES`]).
    #[relax(O3, always)]
    fn fill(&mut self) {
        let ring = self.ring;
        let (Some(at), Some(mix)) = (self.next.as_mut(), self.mix.as_ref()) else { return };
        if let Some(meter) = self.meter.as_mut() {
            meter.woke(ring);
        }
        while ring.queued() < AHEAD_FRAMES && ring.room() > BLOCK_FRAMES as u64 {
            // Timed only for the meter (wasm32 has no Instant).
            let t0 = self.meter.is_some().then(std::time::Instant::now);
            let gain = mix.gain as f32;
            let mut block = mix.mix_f32(*at, BLOCK_FRAMES);
            for v in &mut block {
                *v = (*v * gain).clamp(-1.0, 1.0);
            }
            ring.push(&block);
            *at += BLOCK_FRAMES as u64;
            if let (Some(meter), Some(t0)) = (self.meter.as_mut(), t0) {
                meter.mixed(t0.elapsed());
            }
        }
    }
}

/// The feeder worker: commands as they come, the ring kept full meanwhile.
#[cfg_attr(all(target_arch = "wasm32", not(target_feature = "atomics")), allow(dead_code))]
#[relax(O3, always)]
fn feed(commands: Receiver<Feed>) {
    let mut feeder = Feeder::new();
    loop {
        let command = if feeder.next.is_some() {
            match commands.recv_timeout(Duration::from_millis(4)) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        };
        if let Some(command) = command {
            feeder.command(command);
        }
        feeder.fill();
    }
}

/// `MAKEPAD_AUDIO_DEADLINE=1`: every 5 s the feeder logs how fast it mixes
/// against real time, how full it found the ring when it woke, and how many
/// frames the deck played as silence because they were not written yet.
struct FeedMeter {
    since: std::time::Instant,
    blocks: u64,
    busy: Duration,
    worst: Duration,
    low: u64,
    starved: u64,
}

impl FeedMeter {
    fn new() -> Option<FeedMeter> {
        std::env::var("MAKEPAD_AUDIO_DEADLINE").is_ok_and(|v| v == "1").then(|| FeedMeter {
            since: std::time::Instant::now(),
            blocks: 0,
            busy: Duration::ZERO,
            worst: Duration::ZERO,
            low: u64::MAX,
            starved: ring().starved(),
        })
    }

    fn woke(&mut self, ring: &FeedRing) {
        self.low = self.low.min(ring.queued());
        if self.since.elapsed() < Duration::from_secs(5) {
            return;
        }
        let starved = ring.starved();
        let block = Duration::from_secs_f64(BLOCK_FRAMES as f64 / FEED_RATE as f64);
        let mean = if self.blocks > 0 { self.busy / self.blocks as u32 } else { Duration::ZERO };
        log!(
            "audio-feed edit-sound: {} blocks mixed, {:.1}% of real time (mean {:.2} ms, worst {:.2} ms per {:.0} ms block); ring low {} frames; starved {} frames ({} in all)",
            self.blocks,
            self.busy.as_secs_f64() / (block.as_secs_f64() * self.blocks.max(1) as f64) * 100.0,
            mean.as_secs_f64() * 1e3,
            self.worst.as_secs_f64() * 1e3,
            block.as_secs_f64() * 1e3,
            if self.low == u64::MAX { 0 } else { self.low },
            starved - self.starved,
            starved
        );
        *self = FeedMeter { since: std::time::Instant::now(), blocks: 0, busy: Duration::ZERO, worst: Duration::ZERO, low: u64::MAX, starved };
    }

    fn mixed(&mut self, took: Duration) {
        self.blocks += 1;
        self.busy += took;
        self.worst = self.worst.max(took);
    }
}
