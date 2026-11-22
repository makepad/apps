//! Audio from accepted time points: the run's read on its grid ([`Band`]: the polyline it
//! stepped, band-limited to the run's Nyquist, less the grid's own corners where it did not
//! resolve), as it is at the run's own rate and through a band-limited rate change at another
//! ([`Resample`]; a recording as it plays, [`rate_change`]). A tap never told the run's period
//! hears exact period averages of the linear segments between the points.
//! The worker waits when a listener falls behind. The consumer only copies atomic sample slots;
//! it never allocates, locks or waits. Storage is fixed when the ring is made.
//!
//! A tap mixes voices (the Scope's monitored lanes): each its own net read at the device's rate
//! as above, AC coupled and scaled by its level, summed, scaled by the master level and soft
//! limited, so a biased net or a rail never blasts. The levels, a mute and the meters live in a [`TapMix`] of atomics the UI
//! writes and reads while the tap runs. The consumer is a [`Player`]: as the samples come while
//! the run keeps up, else (below real time) the run's recording ([`crate::recording`]), a window
//! round while the next renders, so a slow run is heard whole and never as crackle.
//! Realtime always plays the live samples in order, fading to silence when they run short.

use crate::recording::{links, window_frames, Delivery, PlayerLinks, RunContent, RunSlot, SpentBin, Take, Tape, TapeShelf, UiLinks, WindowKey, Windows, CHUNK, MAX_RATE};
use crate::budget::KEEP_BYTES;
use crate::stamp::UnknownInfo;
use crate::{Sink, SinkState};
use std::collections::VecDeque;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a waiting worker watches an open ring that nobody drains before it takes the
/// consumer for stopped (a device that went away): it then stops waiting until the consumer
/// takes samples again, and drops what does not fit, so the worker never hangs on a listener
/// that is gone.
const STALL: Duration = Duration::from_millis(250);

/// A fixed sample queue shared by one producer and one consumer. Allocation costs O(capacity);
/// copying costs O(samples), and all other operations take constant time.
pub struct SampleRing {
    slots: Box<[AtomicU32]>,
    mask: usize,
    write: AtomicUsize,
    read: AtomicUsize,
    producer: AtomicBool,
    open: AtomicBool,
    running: AtomicBool,
    /// The transport is paused or stopped. The hand can still scratch a recording.
    hushed: AtomicBool,
    realtime: AtomicBool,
    scrub: AtomicU64,
    /// A time the player's head goes to at its next buffer (NaN: none).
    seek: AtomicU64,
    looping: AtomicBool,
    /// The run rests at the end of its take (`Sink::ended`): live play that runs dry goes on in
    /// the recording, round from the take's start.
    ended: AtomicBool,
    device_rate: AtomicU32,
    underruns: AtomicU64,
    /// How many samples the producer keeps ahead of the consumer: the tap's start, then the
    /// player's measure; zero until a tap or the player sets it.
    ahead: AtomicUsize,
    /// The tap's measure of the run's speed while it steps (audio seconds a second, f32 bits; 0
    /// before the first), and the recording's window the player loops (its take packed, `NONE`
    /// for none, and its index), which the tap's recorder keeps at most one window ahead of.
    tap_speed: AtomicU32,
    heard_take: AtomicU64,
    heard_window: AtomicU64,
    /// While the recording plays, the run's time the listener hears (f64 bits; NaN for none).
    heard_at: AtomicU64,
    /// Where the sound is, as one coherent record the player writes each buffer (a seqlock: an
    /// odd count while it writes): `HEARD_*` index the fields, each f64 bits but the take.
    heard_seq: AtomicU64,
    heard_rec: [AtomicU64; HEARD_FIELDS],
    /// The player's state, for a status line (f32 bits): playing the recording, the run's speed,
    /// the seconds of the window looped (0 for none), of the one rendering and of what it needs
    /// before it plays, a window's, and the lead plus a device buffer; and whether the window
    /// rendering is of a newer take than the one looped.
    rendered: AtomicBool,
    new_take: AtomicBool,
    speed: AtomicU32,
    loop_s: AtomicU32,
    ready_s: AtomicU32,
    start_s: AtomicU32,
    window_s: AtomicU32,
    latency_s: AtomicU32,
    buffer_s: AtomicU32,
    /// Frames the live samples' read waits behind the run ([`Band`]; 0 for the period average).
    read_frames: AtomicU32,
    read_s: AtomicU32,
}

/// No window heard.
const NONE: u64 = u64::MAX;

/// The heard record's fields ([`SampleRing::heard_now`]): the take heard (packed, `NONE` for
/// none), the clock's time the record was written at (`clock_now`), the run's time heard then and
/// the run's seconds a second from there, after how many seconds the next stretch starts (a loop's
/// wrap, a move to another window, a wait), that stretch's time and rate, and the seconds after
/// the stamp past which nothing is known (the device stopped asking): the time holds there.
const HEARD_TAKE: usize = 0;
const HEARD_WALL: usize = 1;
const HEARD_T0: usize = 2;
const HEARD_RATE0: usize = 3;
const HEARD_SWITCH: usize = 4;
const HEARD_T1: usize = 5;
const HEARD_RATE1: usize = 6;
const HEARD_UNTIL: usize = 7;
const HEARD_FIELDS: usize = 8;

/// Seconds on the clock the player stamps the heard record with: a monotonic clock from the
/// process's first reading. Readers compare against it ([`SampleRing::heard_now`]). Constant
/// time; no allocation.
pub fn clock_now() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Where the sound is: the run's time heard, and whether it moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeardNow {
    /// The run's seconds heard at the clock time asked for.
    pub t: f64,
    /// Whether the heard position is moving in either direction.
    pub moving: bool,
    /// When the player wrote the record this reads (`clock_now`'s seconds): how old it is.
    pub stamped: f64,
}

impl SampleRing {
    /// Allocates the fixed storage, rounding capacity up to a power of two, at least two.
    /// Takes O(capacity) time and space. The ring starts closed, with no known device rate.
    pub fn new(capacity: usize) -> Arc<SampleRing> {
        let capacity = capacity.max(2).checked_next_power_of_two().expect("sample capacity fits");
        Arc::new(Self {
            slots: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            mask: capacity - 1,
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            producer: AtomicBool::new(false),
            open: AtomicBool::new(false),
            running: AtomicBool::new(false),
            hushed: AtomicBool::new(false),
            realtime: AtomicBool::new(false),
            scrub: AtomicU64::new(f64::NAN.to_bits()),
            seek: AtomicU64::new(f64::NAN.to_bits()),
            looping: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            device_rate: AtomicU32::new(0),
            underruns: AtomicU64::new(0),
            ahead: AtomicUsize::new(0),
            tap_speed: AtomicU32::new(0),
            heard_take: AtomicU64::new(NONE),
            heard_window: AtomicU64::new(0),
            heard_at: AtomicU64::new(f64::NAN.to_bits()),
            heard_seq: AtomicU64::new(0),
            heard_rec: std::array::from_fn(|i| AtomicU64::new(if i == HEARD_TAKE { NONE } else { 0 })),
            rendered: AtomicBool::new(false),
            new_take: AtomicBool::new(false),
            speed: AtomicU32::new(0),
            loop_s: AtomicU32::new(0),
            ready_s: AtomicU32::new(0),
            start_s: AtomicU32::new(0),
            window_s: AtomicU32::new(0),
            latency_s: AtomicU32::new(0),
            buffer_s: AtomicU32::new(0),
            read_frames: AtomicU32::new(0),
            read_s: AtomicU32::new(0),
        })
    }

    /// The number of sample slots; constant time.
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Samples waiting to be read; constant time. A third thread sees an estimate, bounded by
    /// capacity, because the producer and consumer can both move between the two loads.
    pub fn fill(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        let write = self.write.load(Ordering::Acquire);
        write.wrapping_sub(read).min(self.capacity())
    }

    /// Claims the sole producer, or returns `None` while it is held. Constant time, no allocation;
    /// dropping the returned handle releases the claim.
    pub fn claim(self: &Arc<Self>) -> Option<RingProducer> {
        self.producer
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| RingProducer { ring: Arc::clone(self) })
    }

    /// Copies available samples in order into the start of `out`, returning the count. Only one
    /// consumer may call this at a time. O(samples copied), with no allocation, lock or wait.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let read = self.read.load(Ordering::Relaxed);
        let write = self.write.load(Ordering::Acquire);
        let count = out.len().min(write.wrapping_sub(read));
        for (i, sample) in out[..count].iter_mut().enumerate() {
            *sample = f32::from_bits(self.slots[read.wrapping_add(i) & self.mask].load(Ordering::Relaxed));
        }
        self.read.store(read.wrapping_add(count), Ordering::Release);
        count
    }

    /// Says whether a consumer drains the ring. A closed ring never makes the worker wait.
    /// Constant time.
    pub fn set_open(&self, open: bool) {
        self.open.store(open, Ordering::Release);
    }

    /// Whether a consumer is listening; constant time.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    /// Publishes the consumer's sample rate in Hz; zero means unknown. Constant time.
    pub fn set_device_rate(&self, hz: u32) {
        self.device_rate.store(hz, Ordering::Release);
    }

    /// The live samples' read waits this many frames behind the run (the producer says it);
    /// constant time.
    pub fn set_read_delay(&self, frames: u32) {
        self.read_frames.store(frames, Ordering::Relaxed);
    }

    /// The consumer's sample rate in Hz, or zero while unknown; constant time.
    pub fn device_rate(&self) -> u32 {
        self.device_rate.load(Ordering::Acquire)
    }

    /// Whether the producer says samples are flowing; false while paused, settled or unclaimed.
    /// Constant time.
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Whether the transport stopped or paused normal playback.
    pub fn hushed(&self) -> bool {
        self.hushed.load(Ordering::Relaxed)
    }

    /// Pauses or stops normal playback with a fade. Scratching remains audible.
    pub fn set_hushed(&self, on: bool) {
        self.hushed.store(on, Ordering::Relaxed);
    }

    /// Selects direct playback even when the run falls behind.
    pub fn set_realtime(&self, on: bool) {
        self.realtime.store(on, Ordering::Relaxed);
    }

    /// Whether the player should stay on the live samples.
    pub fn realtime(&self) -> bool {
        self.realtime.load(Ordering::Relaxed)
    }

    /// Sets the time held by the hand, in simulated seconds. None releases the head.
    pub fn set_scrub(&self, t: Option<f64>) {
        self.scrub.store(t.unwrap_or(f64::NAN).to_bits(), Ordering::Relaxed);
    }

    /// The simulated time held by the hand.
    pub fn scrub(&self) -> Option<f64> {
        let t = f64::from_bits(self.scrub.load(Ordering::Relaxed));
        (!t.is_nan()).then_some(t)
    }

    /// Moves the recording's play head to the run's time `t` at the player's next buffer (Play
    /// from where the person is): it plays or holds there as the transport does. Realtime and a
    /// time no recorded window covers leave the head where it is. Constant time.
    pub fn seek(&self, t: f64) {
        self.seek.store(t.to_bits(), Ordering::Relaxed);
    }

    /// The time asked of [`SampleRing::seek`] since the player's last buffer, taken.
    fn take_seek(&self) -> Option<f64> {
        let t = f64::from_bits(self.seek.swap(f64::NAN.to_bits(), Ordering::Relaxed));
        (!t.is_nan()).then_some(t)
    }

    /// Selects finite Loop playback after the hand releases the head.
    pub fn set_looping(&self, on: bool) {
        self.looping.store(on, Ordering::Relaxed);
    }

    /// Whether the transport is looping a finite take.
    pub fn looping(&self) -> bool {
        self.looping.load(Ordering::Relaxed)
    }

    /// Whether the run rests at the end of its take; constant time.
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Says whether the run rests at the end of its take (the tap's `Sink::ended`, cleared by its
    /// next point or a new take); constant time.
    pub fn set_ended(&self, on: bool) {
        self.ended.store(on, Ordering::Release);
    }

    /// Counts one consumer underrun if samples are meant to be flowing. Constant time.
    pub fn note_underrun(&self) {
        if self.running() {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The number of underruns counted while running; constant time.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// How many samples the consumer has taken in all; constant time. It moves while a consumer
    /// drains the ring.
    pub fn consumed(&self) -> usize {
        self.read.load(Ordering::Acquire)
    }

    /// How many samples the producer has queued in all; constant time.
    pub fn produced(&self) -> usize {
        self.write.load(Ordering::Acquire)
    }

    /// How many samples the producer keeps ahead of the consumer (zero: not set yet); constant
    /// time. A press reaches the speakers this much later, plus a device buffer.
    pub fn ahead(&self) -> usize {
        self.ahead.load(Ordering::Relaxed)
    }

    /// Sets the lead, at least one sample and at most half the ring; constant time.
    pub fn set_ahead(&self, samples: usize) {
        self.ahead.store(samples.clamp(1, self.capacity() / 2), Ordering::Relaxed);
    }

    /// The tap's measure of the run's speed while it steps (0 before the first); constant time.
    pub fn tap_speed(&self) -> f32 {
        f32::from_bits(self.tap_speed.load(Ordering::Relaxed))
    }

    /// The tap's measure; constant time.
    pub fn set_tap_speed(&self, speed: f32) {
        self.tap_speed.store(speed.to_bits(), Ordering::Relaxed);
    }

    /// Whether the recording owns the output. The tap feeds live samples when this is false.
    pub fn plays_recording(&self) -> bool {
        self.rendered.load(Ordering::Relaxed)
    }

    /// The window the player loops, or none; constant time.
    pub fn set_heard(&self, key: Option<WindowKey>) {
        match key {
            Some((take, window)) => {
                self.heard_window.store(window, Ordering::Relaxed);
                self.heard_take.store(pack(take), Ordering::Release);
            }
            None => self.heard_take.store(NONE, Ordering::Release),
        }
    }

    /// The window of `take` the player loops, or None (it loops another take's, or none);
    /// constant time.
    pub fn heard(&self, take: Take) -> Option<u64> {
        (self.heard_take.load(Ordering::Acquire) == pack(take)).then(|| self.heard_window.load(Ordering::Relaxed))
    }

    /// While the player plays the recording, the run's time of the sample heard now: its play
    /// head less a device buffer, in the run's seconds (an edit made now is heard from there).
    /// None while it plays live or nothing; constant time.
    pub fn heard_at(&self) -> Option<f64> {
        let t = f64::from_bits(self.heard_at.load(Ordering::Acquire));
        t.is_finite().then_some(t)
    }

    /// Where the sound is at `now` (`clock_now`'s seconds): the run's time heard, from the record
    /// the player wrote at its last buffer, carried on at its rate (across the next stretch's
    /// start when that is due), or None while it plays live or nothing. The latency between the
    /// player and the ear is taken off once, in the player. Constant time: a few atomic loads,
    /// read again only if the player wrote the record meanwhile; no lock, no wait.
    pub fn heard_now(&self, now: f64) -> Option<HeardNow> {
        let r = loop {
            let s = self.heard_seq.load(Ordering::Acquire);
            if s & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let r: [u64; HEARD_FIELDS] = std::array::from_fn(|i| self.heard_rec[i].load(Ordering::Relaxed));
            std::sync::atomic::fence(Ordering::Acquire);
            if self.heard_seq.load(Ordering::Relaxed) == s {
                break r;
            }
        };
        if r[HEARD_TAKE] == NONE {
            return None;
        }
        let f = |i: usize| f64::from_bits(r[i]);
        let until = f(HEARD_UNTIL);
        let dt = (now - f(HEARD_WALL)).clamp(0.0, until.max(0.0));
        let held = now - f(HEARD_WALL) > until;
        let switch = f(HEARD_SWITCH);
        let (t, rate, dt) = if dt >= switch { (f(HEARD_T1), f(HEARD_RATE1), dt - switch) } else { (f(HEARD_T0), f(HEARD_RATE0), dt) };
        Some(HeardNow { t: t + rate * dt, moving: rate != 0.0 && !held, stamped: f(HEARD_WALL) })
    }

    /// Writes the heard record (the player's thread only): a few atomic stores.
    fn write_heard(&self, rec: [u64; HEARD_FIELDS]) {
        let s = self.heard_seq.load(Ordering::Relaxed);
        self.heard_seq.store(s.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        for (slot, v) in self.heard_rec.iter().zip(rec) {
            slot.store(v, Ordering::Relaxed);
        }
        self.heard_seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// The latest measurements from the device callback.
    pub fn play_status(&self) -> PlayStatus {
        let f = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        let speed = f(&self.speed);
        let latency_s = f(&self.latency_s);
        let buffer_s = f(&self.buffer_s);
        let read_s = f(&self.read_s);
        PlayStatus {
            rendered: self.rendered.load(Ordering::Relaxed),
            new_take: self.new_take.load(Ordering::Relaxed),
            speed,
            behind: speed > 0.0 && speed < 1.0,
            loop_s: f(&self.loop_s),
            ready_s: f(&self.ready_s),
            start_s: f(&self.start_s),
            window_s: f(&self.window_s),
            latency_s,
            lead_ms: (latency_s - buffer_s - read_s).max(0.0) * 1000.0,
            buffer_ms: buffer_s * 1000.0,
        }
    }
}

/// A take as one atomic word (the tap's low 32 bits; taps are numbered from 1).
fn pack(take: Take) -> u64 {
    (take.tap << 32) | u64::from(take.take)
}

/// What a [`Player`] plays, for a status line.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayStatus {
    /// Below real time: the recording plays, a window round while the next renders.
    pub rendered: bool,
    /// The window rendering is of a newer take than the one looped: the run started over (an
    /// edit) and its first window renders while the last take's plays on.
    pub new_take: bool,
    /// The run's speed while it steps, as the tap measures it (audio seconds a second; 0 before
    /// the first measure).
    pub speed: f32,
    /// The last measured speed was below one simulated second per second.
    pub behind: bool,
    /// Seconds of the window looped (0: none yet), of the window rendering, of what that one
    /// needs before it plays (the whole window until the speed is known), and of a window.
    pub loop_s: f32,
    pub ready_s: f32,
    pub start_s: f32,
    pub window_s: f32,
    /// Live, how late a change in the run is heard: the samples the ring held as the device last
    /// asked, a device buffer and the read's own wait behind the run.
    pub latency_s: f32,
    /// The live sample lead in milliseconds: what the ring held as the device last asked.
    pub lead_ms: f32,
    /// The device buffer used to size the lead, in milliseconds.
    pub buffer_ms: f32,
}

/// The sole writer's claim on a sample ring. Copying takes O(samples), without allocation
/// or waiting. Dropping it marks the ring stopped before releasing the claim.
pub struct RingProducer {
    ring: Arc<SampleRing>,
}

impl RingProducer {
    /// Writes the prefix that fits within `limit` queued samples, even when the ring's
    /// storage rounded up. Publishes the whole prefix with one store.
    pub fn push_slice(&mut self, samples: &[f32], limit: usize) -> usize {
        let write = self.ring.write.load(Ordering::Relaxed);
        let read = self.ring.read.load(Ordering::Acquire);
        let free = limit.min(self.ring.capacity()).saturating_sub(write.wrapping_sub(read));
        let count = samples.len().min(free);
        for (i, sample) in samples[..count].iter().enumerate() {
            self.ring.slots[write.wrapping_add(i) & self.ring.mask].store(sample.to_bits(), Ordering::Relaxed);
        }
        self.ring.write.store(write.wrapping_add(count), Ordering::Release);
        count
    }

    /// The number of free sample slots; constant time.
    pub fn free(&self) -> usize {
        self.ring.capacity() - self.ring.fill()
    }

    /// Writes one sample, returning false if full without changing the queue. Constant time.
    pub fn push(&mut self, sample: f32) -> bool {
        let write = self.ring.write.load(Ordering::Relaxed);
        let read = self.ring.read.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= self.ring.capacity() {
            return false;
        }
        self.ring.slots[write & self.ring.mask].store(sample.to_bits(), Ordering::Relaxed);
        self.ring.write.store(write.wrapping_add(1), Ordering::Release);
        true
    }

    /// Says whether this producer is running; constant time.
    pub fn set_running(&self, on: bool) {
        self.ring.running.store(on, Ordering::Release);
    }
}

impl Drop for RingProducer {
    fn drop(&mut self) {
        self.set_running(false);
        self.ring.producer.store(false, Ordering::Release);
    }
}

/// One voice of a tap: a voltage and its full scale.
#[derive(Clone, Debug, PartialEq)]
pub struct TapVoice {
    /// The voltage unknown's name.
    pub net: String,
    /// The reference unknown's name; `None` hears the voltage against ground.
    pub reference: Option<String>,
    /// Full scale per volt (before its level, the master and the limiter).
    pub gain: f32,
}

/// One voice's own settings as a key (`Tape::keys`): its net, reference, gain and the tap's DC
/// block. A voice's samples depend on these and its run alone (each voice has its own grid and
/// DC block), so two taps' columns of the same key from the same run are bit for bit equal. O(1).
pub fn voice_key(voice: &TapVoice, dc_block_hz: f64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&voice.net, &voice.reference, voice.gain.to_bits(), dc_block_hz.to_bits(), BAND_READ).hash(&mut h);
    h.finish()
}

/// The voices a tap mixes and its audio settings. Only the names own storage; the tap resolves
/// them once per `begin`, then uses constant space per point.
pub struct TapSpec {
    pub voices: Vec<TapVoice>,
    /// Seconds held ahead of the consumer before waiting at the start, limited by the ring's
    /// capacity; the player then sizes the lead by measure ([`SampleRing::ahead`]). Values below
    /// one sample keep one sample ahead.
    pub latency: f64,
    /// Each voice's one-pole DC block corner in Hz; zero turns it off.
    pub dc_block_hz: f64,
    /// The levels, mute and meters the UI shares with the tap (made for `voices.len()` voices).
    pub mix: Arc<TapMix>,
}

/// What a run's Listen and its recorders share, made without an audio device: the ring a heard
/// tap writes and the player reads, the queues between the worker's recorders, the player and
/// the UI, the shelf of takes, and the run starting now. One an app (`SimLive`), made by whoever
/// needs it first; the player's ends wait here until an output opens and takes them.
pub struct ListenBus {
    pub ring: Arc<SampleRing>,
    pub links: UiLinks,
    pub tapes: Arc<TapeShelf>,
    pub runs: Arc<RunSlot>,
    /// The ring an attached process's speaker is heard through
    /// ([`crate::device_audio::DeviceTap`]), and its level (voice 0 and the master) and mute, which
    /// the output applies as it sums it with the net ring's sound.
    pub device_ring: Arc<SampleRing>,
    pub device_mix: Arc<TapMix>,
    player: Mutex<Option<PlayerLinks>>,
}

/// Seconds of a device's sound its ring holds at 48 kHz: room for a stretch of a run below real
/// time to gather before it plays.
pub const DEVICE_RING_SECONDS: f64 = 2.0;

impl ListenBus {
    /// A bus with a ring of `ring_frames` and the queues' fixed storage, stamping takes from
    /// `runs`.
    pub fn new(ring_frames: usize, runs: Arc<RunSlot>) -> ListenBus {
        let (ui, player) = links();
        let device_ring = SampleRing::new((DEVICE_RING_SECONDS * 48_000.0) as usize);
        ListenBus { ring: SampleRing::new(ring_frames), links: ui, tapes: Arc::default(), runs, device_ring, device_mix: TapMix::new(1), player: Mutex::new(Some(player)) }
    }

    /// The player's ends, to the output that plays the ring and the recordings (once: None after).
    pub fn take_player(&self) -> Option<PlayerLinks> {
        self.player.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// A recording through this bus: tap `tap` (the player's shape names it), its takes in shelf
    /// slot `slot`, windows of `window_s`.
    pub fn record(&self, tap: u64, slot: u64, window_s: f64) -> RecordSpec {
        RecordSpec { tap, slot, window_s, chunks: self.links.chunks.clone(), spent: self.links.spent.clone(), tapes: self.tapes.clone(), runs: self.runs.clone(), bound: KEEP_BYTES }
    }
}

/// A tap's recording: its number (the player's shape names it), the shelf slot its takes go up
/// in (the sink's id, `TapeShelf::current`), its window's length, the queue
/// to the player, what the player lets go (the recorder drops it as it records), where each
/// take's tape goes up, and the run starting now (`RunSlot`), whose content a take that starts
/// at its run's start carries.
pub struct RecordSpec {
    pub tap: u64,
    pub slot: u64,
    pub window_s: f64,
    pub chunks: SyncSender<Delivery>,
    pub spent: SpentBin,
    pub tapes: Arc<TapeShelf>,
    pub runs: Arc<RunSlot>,
    /// The bytes a take nobody hears tapes before it stops taping ([`AudioTap::recorder`]):
    /// [`KEEP_BYTES`], the shelf's budget.
    pub bound: u64,
}

/// The tap's recording half: every voice's sample before its level, in chunks numbered from the
/// take's start, sent in order. While the player plays the recording it sends none more than one
/// window past the one looped, and holds the run until the player moves on (`Sink::holds`); what
/// that rule or a full queue refuses waits here, in order. Live, the player only keeps the newest
/// windows, so a full queue keeps the newest two here and lets older ones go. A chunk is one
/// allocation on the worker. Every chunk also goes on the take's tape (made at its first chunk),
/// whole, so the recording can be saved; a tape that cannot be written stops taping, and the
/// shelf says why. A live run that goes back to a snapshot (an edit behind its front) goes back
/// with its take ([`Recorder::rewind`]): the take goes on, its chunks from there made and sent
/// again under the same numbers.
struct Recorder {
    spec: RecordSpec,
    take: u32,
    index: u64,
    buf: Vec<f32>,
    unsent: VecDeque<Delivery>,
    /// Chunks a window holds at the take's rate, and that rate (its run's recording rate).
    slots: u64,
    rate: f64,
    /// The take's period grid, at its rate with its own DC blocks: the recording's samples.
    grid: Grid,
    /// The grid records the run's read ([`Band`]): as it is at the run's rate, else through `u`.
    banded: bool,
    u: Option<Resample>,
    /// What the take's run simulates, as the take began (`RunSlot`): its rate is the take's.
    run: Option<Arc<RunContent>>,
    tape: Option<Arc<Tape>>,
    /// The frames queued for the take's tape: where the next chunk goes.
    taped: u64,
    taping: bool,
    /// The time of the take's first point: a take whose first point is within its run's first
    /// audio period (a run's first point is its first step's, just past 0) starts with its run.
    t0: f64,
    /// The window of the take's last chunk sent.
    sent: Option<u64>,
    /// A kept take of the same content this take replays, its whole clean chunks, the next of
    /// them to send, and a chunk's read buffer.
    replay: Option<(Arc<Tape>, u64)>,
    served: u64,
    cached: Vec<f32>,
    /// Held ([`AudioTap::play_recorded`]): the replayed tape's column of each voice, in the
    /// tap's order (None: the tape is this tap's own, whole chunks). A held take follows the
    /// tape as its recorder makes it: served at rest, ahead of the player's switch to it, and
    /// through the run's edits as the recorder tapes them again; a new run ends it (a new take).
    pick: Option<Arc<[usize]>>,
    /// The held take went in over a take at rest: the install's `begin` says the run rests at its
    /// end (the player moves into the recording from there), once.
    at_rest: bool,
    /// The tap's settings as a key (`Tape::source`), and each voice's (`Tape::keys`).
    source: u64,
    keys: Arc<[u64]>,
    /// Only tapes ([`AudioTap::recorder`]): sends nothing, and its take stops taping at the bound
    /// (`at_bound`, until the next take).
    tape_only: bool,
    at_bound: bool,
}

impl Recorder {
    fn take(&self) -> Take {
        Take { tap: self.spec.tap, take: self.take }
    }

    /// A take begins at `t0` (`again`: the run started over, a new take, whose tape replaces
    /// this one's at its first chunk), at its run's recording rate, or the device's (`live`) for a run
    /// that has none: the device changing its rate later changes nothing here.
    fn begin_take(&mut self, t0: f64, again: bool, live: f64, voices: usize, period: Option<f64>) {
        if again {
            (self.take, self.index, self.sent, self.replay, self.served, self.pick) = (self.take + 1, 0, None, None, 0, None);
            self.at_bound = false;
            self.buf.clear();
            self.unsent.clear();
            self.tape = None;
        }
        self.t0 = t0;
        self.run = self.spec.runs.current();
        self.rate = self.run.as_ref().map(|r| r.recording_rate).filter(|hz| hz.is_finite() && *hz > 0.0).unwrap_or(live);
        let most = window_frames(self.spec.window_s, MAX_RATE);
        self.slots = (window_frames(self.spec.window_s, self.rate).min(most) / CHUNK) as u64;
        self.grid.set_rate(self.rate);
        self.banded = period.is_some();
        // a take at another rate than the run's records the read through a rate change, made anew
        // only when either rate changed
        let want = period.filter(|p| (p * self.rate - 1.0).abs() >= 1e-9);
        if self.u.as_ref().map(|u| (u.period, u.rate)) != want.map(|p| (p, self.rate)) {
            self.u = want.map(|p| Resample::new(voices, p, self.rate));
        }
        self.buf.reserve(CHUNK * voices);
    }

    /// The take's period ended: every voice's sample before its level goes in, and a whole chunk
    /// out.
    /// The run rests at its take's end (`Sink::ended`) a few frames short of the take's last
    /// window: the grid began at the run's first point, a step after the take's start, so it ends
    /// that step late. Those frames hold the last values and the window goes out whole. Only a
    /// shortfall under a chunk is closed (a take's end is always a window's); a run that goes on
    /// after an edit rewinds the recorder first, so nothing held stays in the take. O(a chunk).
    fn close_take(&mut self, last_t: f64, voices: &[Voice], ring: &SampleRing) {
        let window = self.slots.max(1) * CHUNK as u64;
        let frames = self.index * CHUNK as u64 + (self.buf.len() / voices.len().max(1)) as u64;
        let short = (window - frames % window) % window;
        if short == 0 || short >= CHUNK as u64 {
            return;
        }
        if self.banded {
            // the read's last samples are out (`AudioTap::flush_band`): the rest hold the last values
            for _ in 0..short {
                let mut i = 0;
                self.grid.close(|_| {
                    i += 1;
                    voices[i - 1].last
                });
                self.emit(voices, ring);
            }
            return;
        }
        let end = self.grid.origin + (self.grid.periods + short) as f64 / self.grid.rate;
        self.grid.segment(voices, 0.0);
        let mut at = last_t;
        while self.grid.cross(&mut at, last_t, end, voices) {
            self.emit(voices, ring);
        }
    }

    /// The take's rate change's outputs that are ready go to its grid (`all`: up to the read's
    /// last instant), `out` holding their values.
    fn drain_u(&mut self, out: &mut [f64], all: bool, voices: &[Voice], ring: &SampleRing) {
        while self.u.as_mut().is_some_and(|u| u.pop(out, all)) {
            self.grid.put(out);
            self.emit(voices, ring);
        }
    }

    fn emit(&mut self, voices: &[Voice], ring: &SampleRing) {
        for (i, v) in voices.iter().enumerate() {
            self.buf.push(self.grid.full(i, v) as f32);
        }
        self.end_frame(voices.len(), ring);
    }

    /// Queues a chunk for the take's tape (the tape writer writes it; this never waits on the
    /// disk), making the tape at the take's first chunk.
    fn tape(&mut self, chunk: &Arc<[f32]>, voices: usize) {
        if !self.taping || self.at_bound {
            return;
        }
        if self.tape.is_none() {
            let content = (self.t0 >= 0.0 && self.t0 < 1.0 / self.rate).then(|| self.run.clone()).flatten();
            let tape = Arc::new(Tape::create(self.take(), voices, self.rate, content, self.source, self.keys.clone(), self.t0));
            self.replay = self.spec.tapes.start(self.spec.slot, tape.clone());
            (self.tape, self.taped) = (Some(tape), 0);
        }
        let Some(tape) = &self.tape else { return };
        if let Some(why) = tape.error() {
            return self.stop_taping(why.to_owned());
        }
        let frames = (chunk.len() / voices.max(1)) as u64;
        // a take nobody hears stops at the bound: its start stays on the shelf, keyed, and the
        // run goes on
        if self.tape_only && (self.taped + frames) * 4 * voices as u64 > self.spec.bound {
            self.at_bound = true;
            self.spec.tapes.bounded.fetch_add(1, Ordering::Relaxed);
            return;
        }
        tape.write_later(self.taped, chunk.clone());
        self.taped += frames;
    }

    fn stop_taping(&mut self, why: String) {
        self.taping = false;
        self.tape = None;
        self.spec.tapes.set_error(Some(why));
    }

    /// One frame's voices are in `buf`: a whole chunk goes out.
    fn end_frame(&mut self, voices: usize, ring: &SampleRing) {
        if self.buf.len() < CHUNK * voices.max(1) {
            return;
        }
        let chunk: Arc<[f32]> = Arc::from(&self.buf[..]);
        self.buf.clear();
        self.tape(&chunk, voices);
        if self.tape_only {
            self.index += 1;
            return;
        }
        let index = self.index;
        self.index += 1;
        // a chunk the kept take already gave the player is checked against it, not sent again
        if index < self.served {
            self.check(index, &chunk);
        } else {
            // the chunk's first sample began a chunk of periods before the grid's boundary now
            let t = self.grid.origin + (self.grid.periods as f64 - CHUNK as f64) / self.grid.rate;
            self.unsent.push_back(Delivery { take: self.take(), index, chunk, rate: self.rate, t });
        }
        self.spec.spent.drain();
        self.flush(ring);
    }

    /// Sends what waits, in order, while the queue takes it and, when the player loops the
    /// recording, up to one window past the one it loops. A take the player does not hear yet (a
    /// new one while the last plays round) gets no further than the window it has begun, so the
    /// player's free table keeps the window it will move to rather than a later one.
    fn flush(&mut self, ring: &SampleRing) {
        if self.tape_only {
            return;
        }
        let slots = self.slots.max(1);
        let recording = ring.plays_recording();
        let begun = self.sent.or(self.unsent.front().map(|d| d.index / slots)).unwrap_or(0);
        let allowed = if recording { ring.heard(self.take()).map_or(begun, |h| h + 1) } else { u64::MAX };
        while let Some(d) = self.unsent.pop_front() {
            if d.index / slots > allowed {
                self.unsent.push_front(d);
                return;
            }
            let window = d.index / slots;
            match self.spec.chunks.try_send(d) {
                Ok(()) => self.sent = Some(window),
                Err(TrySendError::Full(d)) => {
                    self.unsent.push_front(d);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.unsent.clear();
                    return;
                }
            }
        }
        // a held take is served at rest too: the player switches to it once its window is whole
        if (recording || self.pick.is_some()) && self.unsent.is_empty() {
            self.serve(slots, allowed);
        }
        if !recording {
            let keep_from = (self.index / slots).saturating_sub(1) * slots;
            while self.unsent.front().is_some_and(|d| d.index < keep_from) {
                self.unsent.pop_front();
            }
        }
    }
}

impl Recorder {
    /// The run went back to `at`, where the take was as `k` keeps it: the take goes on from there.
    /// The chunks made since come off the tape and out of what waits to be sent (those before stay,
    /// in order); the partial chunk and the grid are as they were, so the chunks made again are
    /// sent again under the same numbers, in order, from the first the rewind reaches. The run is
    /// its content's alone no further than `at` (`RunContent::taint`), so a kept take replays no
    /// further. O(chunks waiting + a chunk).
    fn rewind(&mut self, at: f64, k: &TakeState) {
        self.index = k.index;
        self.buf.clear();
        self.buf.extend_from_slice(&k.buf);
        self.grid.clone_from(&k.grid);
        self.u.clone_from(&k.u);
        self.unsent.retain(|d| d.index < k.index);
        let slots = self.slots.max(1);
        self.sent = self.sent.map(|w| w.min(k.index / slots));
        (self.served, self.replay, self.pick) = (self.served.min(k.index), None, None);
        if let Some(tape) = &self.tape {
            tape.cut_later(k.index * CHUNK as u64);
            self.taped = self.taped.min(k.index * CHUNK as u64);
            // a take that reached its bound since the snapshot tapes again from there
            self.at_bound &= self.taped < k.index * CHUNK as u64;
        }
        if let Some(run) = &self.run {
            run.taint(at);
        }
    }

    /// The run went back to `at` under a held take (`AudioTap::rewind`): what was served from the
    /// chunk the run went back into goes again as the recorder tapes it anew, under the same
    /// numbers. From the chunk before it, which is never past the recorder's cut (a chunk sent
    /// again unchanged is the same chunk). O(chunks waiting).
    fn follow_back(&mut self, at: f64) {
        let frame = ((at - self.t0) * self.rate).max(0.0) as u64;
        let from = (frame / CHUNK as u64).saturating_sub(1);
        self.served = self.served.min(from);
        self.unsent.retain(|d| d.index < from);
        let slots = self.slots.max(1);
        self.sent = self.sent.map(|w| w.min(from / slots));
    }

    /// Sends the kept take's chunks past what is rendered, in order, under the same window rule
    /// and while the queue takes them (a refused one is read again later). A held take's chunk is
    /// its voices' columns of the tape's (`pick`). O(a chunk x the tape's voices) a chunk.
    fn serve(&mut self, slots: u64, allowed: u64) {
        let Some((tape, chunks)) = &self.replay else { return };
        // a held take reads what its recorder taped last: what is written, and nothing while a
        // cut it queued waits (past the cut the frames are the old take's); O(1) when it waits
        let ready = |tape: &Tape, index: u64| tape.settled() && (index + 1) * CHUNK as u64 <= tape.frames();
        if self.pick.is_some() && !ready(tape, self.served.max(self.index)) {
            return;
        }
        let (tape, chunks) = (tape.clone(), *chunks);
        loop {
            let index = self.served.max(self.index);
            if index >= chunks || index / slots > allowed || (self.pick.is_some() && !ready(&tape, index)) {
                return;
            }
            let mut buf = vec![0.0f32; CHUNK * tape.voices];
            if !matches!(tape.read(index * CHUNK as u64, &mut buf), Ok(n) if n == CHUNK) {
                self.replay = None;
                return;
            }
            if let Some(pick) = &self.pick {
                buf = buf.chunks_exact(tape.voices).flat_map(|frame| pick.iter().map(move |&c| frame[c])).collect();
            }
            let t = self.t0 + (index * CHUNK as u64) as f64 / tape.rate;
            match self.spec.chunks.try_send(Delivery { take: self.take(), index, chunk: Arc::from(buf), rate: tape.rate, t }) {
                Ok(()) => {
                    (self.served, self.sent) = (index + 1, Some(index / slots));
                    self.spec.tapes.replayed.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Full(_)) => return,
                Err(TrySendError::Disconnected(_)) => {
                    self.replay = None;
                    return;
                }
            }
        }
    }

    /// A rendered chunk the kept take played: counted when its samples differ, and how far
    /// (`TapeShelf::worst`; a magnitude's f32 bits order as its value does).
    fn check(&mut self, index: u64, chunk: &[f32]) {
        let Some((tape, _)) = &self.replay else { return };
        self.cached.resize(chunk.len(), 0.0);
        let read = matches!(tape.read(index * CHUNK as u64, &mut self.cached), Ok(n) if n * tape.voices == chunk.len());
        if read && self.cached.iter().zip(chunk).all(|(a, b)| a.to_bits() == b.to_bits()) {
            return;
        }
        let tapes = &self.spec.tapes;
        tapes.differed.fetch_add(1, Ordering::Relaxed);
        if read {
            let worst = self.cached.iter().zip(chunk).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            tapes.worst.fetch_max(worst.to_bits(), Ordering::Relaxed);
        }
    }
}

/// The time the tap measures the run's speed over: stepping, and a paced run's waits for its
/// clock (which the speed leaves out).
const SPEED_BUSY_SECONDS: f64 = 0.25;

/// The mix's controls and meters, shared by the UI and a running tap: per voice a level, a
/// master level and a mute (f32 bits and a flag in atomics), and per voice and for the mix a peak
/// and an RMS. The tap reads the controls per sample and writes the meters every
/// [`METER_EVERY`] samples; every operation is O(1), with no lock or allocation.
pub struct TapMix {
    levels: Box<[AtomicU32]>,
    master: AtomicU32,
    muted: AtomicBool,
    /// Per voice, then the mix: the largest magnitude since the last read, and the RMS over about
    /// [`METER_SECONDS`] (f32 bits).
    peaks: Box<[AtomicU32]>,
    rms: Box<[AtomicU32]>,
}

/// Samples between two meter writes (about 5 ms at 48 kHz).
pub const METER_EVERY: u32 = 256;

/// The RMS meter's time constant.
pub const METER_SECONDS: f64 = 0.05;

/// A meter reading: full scale is 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Meter {
    pub peak: f32,
    pub rms: f32,
}

impl TapMix {
    /// Controls for `voices` voices at level 1, master 1, unmuted, meters at rest. O(voices).
    pub fn new(voices: usize) -> Arc<TapMix> {
        let bits = |n: usize, v: f32| -> Box<[AtomicU32]> { (0..n).map(|_| AtomicU32::new(v.to_bits())).collect() };
        Arc::new(TapMix { levels: bits(voices, 1.0), master: AtomicU32::new(1f32.to_bits()), muted: AtomicBool::new(false), peaks: bits(voices + 1, 0.0), rms: bits(voices + 1, 0.0) })
    }

    pub fn voices(&self) -> usize {
        self.levels.len()
    }

    /// A voice's level (linear; 1 is its full scale); 1 for a voice it has no control for.
    pub fn level(&self, voice: usize) -> f32 {
        self.levels.get(voice).map_or(1.0, |l| f32::from_bits(l.load(Ordering::Relaxed)))
    }

    pub fn set_level(&self, voice: usize, level: f32) {
        if let Some(l) = self.levels.get(voice) {
            l.store(level.max(0.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub fn master(&self) -> f32 {
        f32::from_bits(self.master.load(Ordering::Relaxed))
    }

    pub fn set_master(&self, level: f32) {
        self.master.store(level.max(0.0).to_bits(), Ordering::Relaxed);
    }

    /// Muted, the tap queues silence: the run and its pacing go on.
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Voice `i`'s meter (`i == voices()`: the mix's, after the limiter); reading takes the peak.
    pub fn meter(&self, i: usize) -> Meter {
        let peak = self.peaks.get(i).map_or(0, |p| p.swap(0, Ordering::Relaxed));
        let rms = self.rms.get(i).map_or(0, |r| r.load(Ordering::Relaxed));
        Meter { peak: f32::from_bits(peak), rms: f32::from_bits(rms) }
    }

    pub(crate) fn note(&self, i: usize, peak: f32, rms: f32) {
        // magnitudes are non-negative, where f32 bits order as the values do
        self.peaks[i].fetch_max(peak.to_bits(), Ordering::Relaxed);
        self.rms[i].store(rms.to_bits(), Ordering::Relaxed);
    }
}

/// Where the soft limiter leaves the straight line: below it the mix passes untouched.
pub const LIMIT_KNEE: f64 = 0.8;

/// The mix's soft limiter: the identity up to [`LIMIT_KNEE`], then a tanh shoulder that meets it
/// with the same slope and approaches full scale, never past it. O(1).
pub fn soft_limit(x: f64) -> f64 {
    let a = x.abs();
    if a <= LIMIT_KNEE {
        x
    } else {
        let room = 1.0 - LIMIT_KNEE;
        x.signum() * (LIMIT_KNEE + room * ((a - LIMIT_KNEE) / room).tanh())
    }
}

/// Cumulative work since the tap was made. Updating or reading a counter takes constant time.
#[derive(Default)]
pub struct TapStats {
    /// Samples successfully queued.
    pub samples: u64,
    /// Waiting episodes, counted once even when an episode sleeps several times.
    pub waits: u64,
    /// Samples discarded because the ring was full and closed.
    pub dropped_closed: u64,
    /// Times the open ring's consumer took nothing for `STALL` while the worker waited.
    pub stalls: u64,
    /// Samples discarded because the ring was full and its consumer had stalled.
    pub dropped_stalled: u64,
}

/// A voice's running state: its unknowns, its value at the last point and at this one, and its
/// live meter.
struct Voice {
    spec: TapVoice,
    at: Option<(usize, usize)>,
    last: f64,
    now: f64,
    ms: f64,
    peak: f64,
}

/// A sample stream's period grid: periods of `1 / rate` from an origin and, per voice, the area
/// of the period so far, the segment's value at the grid's cursor, its one-pole DC block and the
/// last period's sample. The tap plays on one at the device's rate and each take records on its
/// own at its run's. Clone: a take's state can be kept with it.
#[derive(Clone, Debug)]
pub(crate) struct Grid {
    rate: f64,
    origin: f64,
    periods: u64,
    dc_hz: f64,
    dc_a: f64,
    /// The DC blocks have seen no sample since the last reset: they start at rest on the first one.
    fresh: bool,
    voices: Vec<GridVoice>,
}

#[derive(Clone, Copy, Debug, Default)]
struct GridVoice {
    area: f64,
    cursor: f64,
    dc_x: f64,
    dc_y: f64,
    /// The last period's sample: the voice's average, AC coupled.
    out: f64,
}

impl Grid {
    fn new(voices: usize, rate: f64, dc_hz: f64) -> Grid {
        let mut grid = Grid { rate: 0.0, origin: 0.0, periods: 0, dc_hz, dc_a: 1.0, fresh: true, voices: vec![GridVoice::default(); voices] };
        grid.set_rate(rate);
        grid
    }

    /// Periods of `1 / rate` from the current period's start on (its area so far kept), and the
    /// DC block's corner at that rate.
    fn set_rate(&mut self, rate: f64) {
        if rate == self.rate {
            return;
        }
        if self.periods > 0 {
            self.origin += self.periods as f64 / self.rate;
            self.periods = 0;
        }
        self.rate = rate;
        self.dc_a = 1.0 / (1.0 + TAU * self.dc_hz / rate);
    }

    /// Starts over: no area, the DC blocks at rest.
    fn reset(&mut self) {
        (self.origin, self.periods, self.fresh) = (0.0, 0, true);
        self.voices.fill(GridVoice::default());
    }

    /// The first period starts at `t`.
    fn start(&mut self, t: f64) {
        (self.origin, self.periods) = (t, 0);
    }

    /// A segment from the last point begins, integrated from `along` of the way to this one (0:
    /// from the last point): each voice's cursor at its value there.
    fn segment(&mut self, voices: &[Voice], along: f64) {
        for (g, v) in self.voices.iter_mut().zip(voices) {
            g.cursor = if along == 0.0 { v.last } else { v.last + (v.now - v.last) * along };
        }
    }

    /// Integrates the segment from `last_t` to `t` (each voice from `last` to `now`, linearly) from
    /// `*at` to the next period boundary or `t`, whichever comes first: true when a period ended
    /// there, each voice's sample in `out`. Boundaries come from the origin rather than from
    /// accumulated rounded periods. O(voices).
    fn cross(&mut self, at: &mut f64, last_t: f64, t: f64, voices: &[Voice]) -> bool {
        if *at >= t {
            return false;
        }
        let boundary = self.origin + (self.periods as f64 + 1.0) / self.rate;
        let end = t.min(boundary);
        let along = (end - last_t) / (t - last_t);
        for (g, v) in self.voices.iter_mut().zip(voices) {
            let end_v = if end == t { v.now } else { v.last + (v.now - v.last) * along };
            g.area += 0.5 * (g.cursor + end_v) * (end - *at);
            g.cursor = end_v;
        }
        *at = end;
        if end != boundary {
            return false;
        }
        let rate = self.rate;
        self.close(|g| std::mem::take(&mut g.area) * rate);
        true
    }

    /// A period ends with each voice's value from `volts` (its area, or the run's read at the
    /// period's instant, `put`): through its DC block into its sample. O(voices).
    fn close(&mut self, mut volts: impl FnMut(&mut GridVoice) -> f64) {
        self.periods += 1;
        let (fresh, dc_on, dc_a) = (self.fresh, self.dc_hz > 0.0, self.dc_a);
        self.fresh = false;
        for g in &mut self.voices {
            let volts = volts(g);
            g.out = if dc_on {
                // a net resting at a few volts starts silent, not with a full-scale step
                if fresh {
                    g.dc_x = volts;
                }
                g.dc_y = dc_a * (g.dc_y + volts - g.dc_x);
                g.dc_x = volts;
                g.dc_y
            } else {
                volts
            };
        }
    }

    /// The next period's sample from the run's read ([`Band`]): each voice's value at the
    /// period's instant, in `volts`. O(voices).
    fn put(&mut self, volts: &[f64]) {
        let mut i = 0;
        self.close(|_| {
            i += 1;
            volts.get(i - 1).copied().unwrap_or(0.0)
        });
    }

    /// Voice `i`'s last sample at full scale (its gain), silent for a voice not resolved.
    fn full(&self, i: usize, v: &Voice) -> f64 {
        if v.at.is_some() {
            self.voices[i].out * f64::from(v.spec.gain)
        } else {
            0.0
        }
    }
}

/// How far the run's read ([`Band`]) reaches either side of a jump or a corner, in periods: its
/// kernel spans this many each way, and the read lags the run by as many.
pub const BAND_HALF: usize = 16;
/// Table points a period in the read's kernel tables.
const BAND_RES: usize = 1024;
/// The read's identity and its rate change's ([`Resample`]), in the keys of what they shape
/// (`voice_key`, `AudioTap::source`).
const BAND_READ: (&str, usize, usize) = ("band, u", BAND_HALF, BAND_RES);

/// The run's read's kernel: a windowed sinc, -6 dB at the run's Nyquist (a Kaiser window, beta
/// 7.857, over [`BAND_HALF`] periods each way: flat to 0.417 of the rate, about 80 dB down from
/// 0.583), as two tables over -16..0 periods: the band-limited step less the step (`r`, a jump's
/// residual, +1/2 at the jump: the value there is the one before it) and its running integral
/// (`big`, a corner's, in periods), and the kernel itself (`h`, 1 at its instant: a band-limited
/// rate change's weights, [`rate_change`], in periods of the lower rate). All
/// are zero past 16 periods; `r` is odd about 0, `big` and `h` even, so half of each is kept. Made
/// once, on first use: 3 x 16k values.
struct BandKernel {
    r: Vec<f64>,
    big: Vec<f64>,
    h: Vec<f64>,
}

fn band_kernel() -> &'static BandKernel {
    static KERNEL: OnceLock<BandKernel> = OnceLock::new();
    KERNEL.get_or_init(|| {
        const BETA: f64 = 7.857;
        let half = BAND_HALF as f64;
        // the modified Bessel function I0, by its series
        let i0 = |x: f64| {
            let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
            loop {
                term *= (0.5 * x / k) * (0.5 * x / k);
                sum += term;
                if term < 1e-17 * sum {
                    return sum;
                }
                k += 1.0;
            }
        };
        let norm = i0(BETA);
        let h = |u: f64| {
            let w = 1.0 - (u / half) * (u / half);
            if w <= 0.0 {
                return 0.0;
            }
            let sinc = if u == 0.0 { 1.0 } else { (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u) };
            sinc * i0(BETA * w.sqrt()) / norm
        };
        let n = BAND_HALF * BAND_RES;
        let du = 1.0 / BAND_RES as f64;
        let (mut r, mut big) = (vec![0.0; n + 1], vec![0.0; n + 1]);
        for i in 0..n {
            let a = -half + i as f64 * du;
            let (ha, hm, hb) = (h(a), h(a + 0.5 * du), h(a + du));
            // the step's rise over the cell (Simpson), and the corner's (the trapezoid less its end
            // correction: the step's slope is the kernel)
            r[i + 1] = r[i] + du * (ha + 4.0 * hm + hb) / 6.0;
            big[i + 1] = big[i] + 0.5 * du * (r[i] + r[i + 1]) - du * du * (hb - ha) / 12.0;
        }
        // the band-limited step is half risen at its instant, exactly
        let scale = 0.5 / r[n];
        r.iter_mut().for_each(|v| *v *= scale);
        big.iter_mut().for_each(|v| *v *= scale);
        let h = (0..=n).map(|i| h(-half + i as f64 * du)).collect();
        BandKernel { r, big, h }
    })
}

impl BandKernel {
    fn at(table: &[f64], u: f64) -> f64 {
        let x = (u + BAND_HALF as f64) * BAND_RES as f64;
        if x <= 0.0 {
            return 0.0;
        }
        let i = x as usize;
        if i + 1 >= table.len() {
            return table[table.len() - 1];
        }
        table[i] + (table[i + 1] - table[i]) * (x - i as f64)
    }

    /// A unit jump's residual `u` periods after it (the value before it at 0).
    fn step(&self, u: f64) -> f64 {
        if u <= 0.0 {
            Self::at(&self.r, u)
        } else {
            -Self::at(&self.r, -u)
        }
    }

    /// A unit change of slope's residual `u` periods from it, in periods.
    fn corner(&self, u: f64) -> f64 {
        Self::at(&self.big, -u.abs())
    }

    /// The kernel `u` periods from its instant: a rate change's weight there.
    fn weight(&self, u: f64) -> f64 {
        Self::at(&self.h, -u.abs())
    }
}

/// How many periods a sample of the run's read waits behind the run's newest instant: a residual
/// reaches [`BAND_HALF`] periods back, and the grid's own corner at an instant is known one
/// period later.
pub const BAND_LAG: i64 = BAND_HALF as i64 + 1;

/// What the banded read saw on the run's grid. A rewind restores these counts with the read.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadCounts {
    /// Each step after the initial point contributes the number of sample instants it reaches.
    pub instants: u64,
    /// An instant counts once if a free step in either adjacent period kept its corner.
    pub beside_free: u64,
    /// An instant counts once if the read took back a nonzero corner for any voice.
    pub taken_back: u64,
    /// A step counts if it ends between instants and is neither free nor a jump.
    pub inner_points: u64,
    /// An instant counts if the read took the chord inside a step that was not free.
    pub off_grid: u64,
}

/// The run's read on its own grid, its instants `n * period` from 0 ([`Sink::sample_period`]): the
/// polyline the run stepped, band-limited ([`BandKernel`]), less the grid's own corners where the
/// run did not resolve. Sample `n` is
///
/// `x(t_n) + sum over points c_p T corner(t_n - t_p) + sum over jumps J step(t_n - t_j)
///  - sum over sample instants k of out_k corner(n - k)`
///
/// - `x(t_n)`: the motion at the instant: the point there, else the chord of the step it falls in;
///   at a jump on it, the value before.
/// - `c_p`: every point's change of slope, whatever its steps' kinds. Slopes carry across a jump
///   (an edge point, or equal times): its corner is between the step before it and the step after.
/// - An edge point (a jump's second point) carries the first step's end values for every voice: a
///   voice whose step after it holds that value to the bit jumps by what the step after the held
///   one extrapolates back to the edge, no more than the whole jump, and ramps over the held step
///   ([`Band::release`]). So the read holds an edge point until the second point after it.
/// - `out_k`: at a sample instant, the polyline's own corner there (`c_k T`; 0 with no point on
///   it), limited to between 0 and `D_k`, the second difference of the instants' values with the
///   jumps taken out (the corner the chords from instant to instant make): the corner an
///   unresolved chord itself put into the polyline there.
/// - A sample instant: neither period that meets at it holds a free step ([`Sink::chosen`]); a free
///   step that passes over an instant vouches for both periods. Where both periods are one step
///   each the two terms are one number and neither is added: a run on its grid reads as its
///   points, by bits.
///
/// So no fine corner is left without its partner, and none is taken out that the polyline did not
/// make: a run that resolves its periods reads as its motion through the kernel, a run on its grid
/// as its points, an edge inside a period it did not resolve as the band-limited edge, and smooth
/// motion beside such a period with the unresolved chords' share of the corners taken back. A
/// point's kind arrives
/// after it, so its step is taken at the next point; a sample is final once the run's instants are
/// [`BAND_LAG`] periods past it. Per voice a ring of the pending samples (about 35; it grows only
/// for a step longer than a period). A point costs O(voices), a corner, a jump or a sample
/// instant's corner O(voices x 33). Clone: kept with a snapshot, so a rewind reads sample for
/// sample.
#[derive(Clone, Debug)]
struct Band {
    period: f64,
    /// The index of each ring's front (the oldest pending instant), the instant the read began on,
    /// and the next instant whose motion is not in yet.
    first: i64,
    start: i64,
    based: i64,
    /// The last point whose step is taken, and the newest, whose kind is still arriving.
    last_t: Option<f64>,
    next_t: Option<f64>,
    next_free: bool,
    /// The last step taken runs from one instant to the next and is not free.
    last_whole: bool,
    /// The periods (each by its first instant) from `free_from` on that hold a free step.
    free: VecDeque<bool>,
    free_from: i64,
    /// The instant whose point's corner and grid corner are one number: neither is added.
    same: Option<i64>,
    /// An edge point held until the second point after it, and the end of the step after it:
    /// each one's time and kind ([`Band::release`]).
    edge: Option<(f64, bool)>,
    edge_next: Option<(f64, bool)>,
    voices: Vec<BandVoice>,
    counts: ReadCounts,
}

#[derive(Clone, Debug, Default)]
struct BandVoice {
    /// The pending samples, from `Band::first` on.
    ring: VecDeque<f64>,
    last: f64,
    next: f64,
    /// The last step's slope (none before the first), the jumps taken so far, and the two newest
    /// instants' values less them.
    slope: Option<f64>,
    jumps: f64,
    y: [f64; 2],
    /// The newest instant that had a point on it, and the polyline's corner there (`c_k T`).
    corner: (i64, f64),
    /// A held edge point's value, the held step's end, and the point after it ([`Band::release`]).
    edge: f64,
    held: f64,
    after: f64,
}

impl Band {
    fn new(voices: usize, period: f64) -> Band {
        let voice = BandVoice { ring: VecDeque::with_capacity(4 * BAND_HALF), ..BandVoice::default() };
        Band {
            period,
            first: 0,
            start: 0,
            based: 0,
            last_t: None,
            next_t: None,
            next_free: false,
            last_whole: false,
            free: VecDeque::with_capacity(8),
            free_from: 0,
            same: None,
            edge: None,
            edge_next: None,
            voices: vec![voice; voices],
            counts: ReadCounts::default(),
        }
    }

    fn reset(&mut self) {
        (self.last_t, self.next_t, self.next_free) = (None, None, false);
        (self.edge, self.edge_next) = (None, None);
        for v in &mut self.voices {
            v.ring.clear();
        }
    }

    fn instant(&self, n: i64) -> f64 {
        n as f64 * self.period
    }

    /// Times within this of each other are one instant: a jump's two points.
    fn tol(&self, t: f64) -> f64 {
        4.0 * f64::EPSILON * t.abs().max(self.period)
    }

    /// A point this near an instant is the run's point there: the stepper lands a step on its
    /// grid to within a rounding, and takes a point this near a sample as on it (`Stepper::step`).
    fn near(&self) -> f64 {
        1e-6 * self.period
    }

    /// The first instant at `t` or after it.
    fn first_at(&self, t: f64) -> i64 {
        ((t - self.near()) / self.period).ceil() as i64
    }

    /// A point, each voice's value in turn: the step to the point before it is taken.
    fn point(&mut self, t: f64, now: impl Iterator<Item = f64>) {
        self.take();
        for (v, x) in self.voices.iter_mut().zip(now) {
            v.next = x;
        }
        (self.next_t, self.next_free) = (Some(t), false);
    }

    /// The newest point's step is free.
    fn chosen(&mut self, at: f64) {
        if self.next_t == Some(at) {
            self.next_free = true;
        }
    }

    fn is_free(&self, p: i64) -> bool {
        p >= self.free_from && self.free.get((p - self.free_from) as usize).copied().unwrap_or(false)
    }

    /// Adds `size` times `kernel` at each pending instant within reach of `at`, for one voice.
    fn residual(first: i64, period: f64, v: &mut BandVoice, at: f64, size: f64, kernel: impl Fn(f64) -> f64) {
        let lo = ((at / period) - BAND_HALF as f64).ceil() as i64;
        let hi = ((at / period) + BAND_HALF as f64).floor() as i64;
        let lo = lo.max(first);
        if hi < lo {
            return;
        }
        let len = (hi - first + 1) as usize;
        if v.ring.len() < len {
            v.ring.resize(len, 0.0);
        }
        for n in lo..=hi {
            v.ring[(n - first) as usize] += size * kernel((n as f64 * period - at) / period);
        }
    }

    /// Takes the newest point (its kind is in): an edge point is held until the second point after
    /// it ([`Band::release`]); every other point's step is taken.
    fn take(&mut self) {
        self.take_point(true);
    }

    /// At a rest: takes the newest point and an edge point held, as they came.
    fn take_all(&mut self) {
        self.take_point(false);
    }

    fn take_point(&mut self, hold: bool) {
        let Some(t) = self.next_t.take() else { return };
        let free = std::mem::take(&mut self.next_free);
        let Some(t0) = self.last_t else {
            // the run's first point: the read starts at the first instant at it or after it
            let n0 = self.first_at(t);
            (self.first, self.start, self.based, self.free_from) = (n0, n0, n0, n0);
            (self.last_whole, self.same) = (false, None);
            self.free.clear();
            let on = (self.instant(n0) - t).abs() <= self.near();
            for v in &mut self.voices {
                (v.last, v.slope, v.jumps, v.y, v.corner) = (v.next, None, 0.0, [0.0, v.next], (i64::MIN, 0.0));
                v.ring.clear();
                if on {
                    v.ring.push_back(v.next);
                }
            }
            if on {
                self.based += 1;
            }
            self.last_t = Some(t);
            return;
        };
        let tol = self.tol(t);
        if let Some((te, _)) = self.edge {
            match self.edge_next {
                // the step after the edge point: its end held
                None if hold && t - te > tol => {
                    for v in &mut self.voices {
                        v.held = v.next;
                    }
                    self.edge_next = Some((t, free));
                    return;
                }
                // the step after the held one: not a jump, so it can mend the edge
                Some((t1, _)) if hold && t - t1 > tol => self.release(Some(t)),
                _ => self.release(None),
            }
        }
        if hold && t - self.last_t.unwrap_or(t0) <= tol {
            // an edge point: held until the second point after it
            for v in &mut self.voices {
                v.edge = v.next;
            }
            self.edge = Some((t, free));
            return;
        }
        self.step(t, free);
    }

    /// Takes the held edge point and the held step after it. With `after` (the time of the point
    /// after the held step, its values in each voice's `next`), a voice whose held step kept the
    /// edge point's value to the bit, and whose step after that is not a hold, did not jump by all
    /// of it at the edge: the edge point carries the first step's end. Its jump is what the step
    /// after the held one extrapolates back to the edge, `x1 - m2 h1 - x0`, limited to between 0
    /// and the whole jump, and the rest a ramp over the held step; a voice the edge drives keeps its
    /// jump (its level after is flat or slow). Else, and at a rest, they are taken as they came.
    fn release(&mut self, after: Option<f64>) {
        let Some((te, fe)) = self.edge.take() else { return };
        let held = self.edge_next.take();
        for v in &mut self.voices {
            v.after = v.next;
        }
        if let (Some(t2), Some((t1, _))) = (after, held) {
            for v in &mut self.voices {
                let whole = v.edge - v.last;
                if whole != 0.0 && v.held.to_bits() == v.edge.to_bits() && v.after != v.held {
                    let m2 = (v.after - v.held) / (t2 - t1);
                    let jump = (v.edge - m2 * (t1 - te) - v.last).clamp(whole.min(0.0), whole.max(0.0));
                    v.edge = v.last + jump;
                }
            }
        }
        for v in &mut self.voices {
            v.next = v.edge;
        }
        self.step(te, fe);
        if let Some((t1, f1)) = held {
            for v in &mut self.voices {
                v.next = v.held;
            }
            self.step(t1, f1);
        }
        for v in &mut self.voices {
            v.next = v.after;
        }
    }

    /// Takes the step from the last point taken to the point at `t` (each voice's `next`): its
    /// jump, or the corner at its start; then the instants it reaches get their motion, and the
    /// instant before each the polyline's own corner taken back where it is a sample instant.
    fn step(&mut self, t: f64, free: bool) {
        let Some(t0) = self.last_t else { return };
        let (tol, near, period) = (self.tol(t), self.near(), self.period);
        let k = band_kernel();
        let jump = t - t0 <= tol;
        let first = self.first;
        if jump {
            // its corner waits for the step after it: slopes carry across
            for v in &mut self.voices {
                let size = v.next - v.last;
                if size != 0.0 {
                    Self::residual(first, period, v, t0, size, |u| k.step(u));
                }
                v.jumps += size;
            }
            self.last_whole = false;
        } else {
            if free {
                // a free step vouches for every period it passes over
                let lo = ((t0 + near) / period).floor() as i64;
                let hi = ((t - near) / period).ceil() as i64 - 1;
                for p in lo.max(self.free_from)..=hi {
                    let i = (p - self.free_from) as usize;
                    if self.free.len() <= i {
                        self.free.resize(i + 1, false);
                    }
                    self.free[i] = true;
                }
            }
            let m = (t0 / period).round() as i64;
            let whole = !free && (t0 - self.instant(m)).abs() <= near && (t - self.instant(m + 1)).abs() <= near;
            if !free && !whole && (t - self.instant((t / period).round() as i64)).abs() > near {
                self.counts.inner_points += 1;
            }
            // a point on an instant between two whole steps: its corner is the grid's there
            let same = whole && self.last_whole;
            if same {
                self.same = Some(m);
            }
            // a point on an instant: its corner is the most a sample instant there takes back
            let on = (t0 - self.instant(m)).abs() <= near;
            for v in &mut self.voices {
                let slope = (v.next - v.last) / (t - t0);
                let change = v.slope.map_or(0.0, |before| slope - before);
                if change != 0.0 && !same {
                    Self::residual(first, period, v, t0, change * period, |u| k.corner(u));
                }
                if on {
                    v.corner = (m, change * period);
                }
                v.slope = Some(slope);
            }
            self.last_whole = whole;
        }
        while self.instant(self.based) <= t + near {
            let tn = self.instant(self.based);
            let i = (self.based - self.first) as usize;
            let on = jump || (tn - t).abs() <= near;
            if !on && !free {
                self.counts.off_grid += 1;
            }
            let along = (tn - t0) / (t - t0);
            // the instant before this one: its grid corner is known now, and where neither period
            // beside it holds a free step the polyline's own corner there is taken back, no more
            // than the grid's
            let kk = self.based - 1;
            let beside_free = kk > self.start && (self.is_free(kk - 1) || self.is_free(kk));
            let out = kk > self.start && !beside_free && self.same != Some(kk);
            let at = self.instant(kk);
            let mut taken_back = false;
            for v in &mut self.voices {
                let x = if on { v.next } else { v.last + (v.next - v.last) * along };
                if v.ring.len() <= i {
                    v.ring.resize(i + 1, 0.0);
                }
                v.ring[i] += x;
                let y = x - v.jumps;
                let d = y - 2.0 * v.y[1] + v.y[0];
                v.y = [v.y[1], y];
                let own = if v.corner.0 == kk { v.corner.1 } else { 0.0 };
                let back = if d >= 0.0 { own.max(0.0).min(d) } else { own.min(0.0).max(d) };
                if out && back != 0.0 {
                    taken_back = true;
                    Self::residual(first, period, v, at, -back, |u| k.corner(u));
                }
            }
            self.counts.taken_back += u64::from(taken_back);
            self.counts.beside_free += u64::from(beside_free);
            self.counts.instants += 1;
            self.based += 1;
        }
        for v in &mut self.voices {
            v.last = v.next;
        }
        // flags no instant to come asks for
        while self.free_from + 3 < self.based {
            self.free.pop_front();
            self.free_from += 1;
        }
        self.last_t = Some(t);
    }

    /// The oldest pending sample, each voice's in `out`, once it is final (the run's instants
    /// [`BAND_LAG`] periods past it) or, `all`, once its motion is in: its instant's index.
    fn pop(&mut self, out: &mut [f64], all: bool) -> Option<i64> {
        self.last_t?;
        if self.first >= self.based || !all && self.first + BAND_LAG >= self.based {
            return None;
        }
        for (v, o) in self.voices.iter().zip(out.iter_mut()) {
            *o = v.ring.front().copied().unwrap_or(0.0);
        }
        for v in &mut self.voices {
            v.ring.pop_front();
        }
        self.first += 1;
        Some(self.first - 1)
    }
}

/// The run's read at another rate (`U`): a band-limited rate change of the read's samples
/// ([`Band`], instants `n * period`) onto instants `origin + m / rate` ([`rate_change`]: the read's
/// own kernel at the lower of the two Nyquists, [`RATE_HALF`] periods of the lower rate each way,
/// the weights summed to 1, so a constant stays exact). Before
/// the read's first sample it reads that sample, and at a rest (`all`) past its last the last one.
/// An output is made once the read's samples it reaches are in, so it lags the read by its reach
/// ([`Resample::delay_frames`] counts both waits). Per voice a ring of the read's samples one reach
/// each way. An output costs O(voices x taps), with `2 x reach + 1` taps: 33 when the output is at
/// the read's rate or faster, about 33 x the ratio when it is slower. Clone: kept with a take's
/// snapshot.
#[derive(Clone, Debug)]
struct Resample {
    /// The read's period and the output's rate.
    period: f64,
    rate: f64,
    /// The rate change's reach each way in the read's periods: [`RATE_HALF`] periods of the
    /// lower rate.
    reach: f64,
    /// The index of the rings' front sample, and of the newest (None before the first).
    first: i64,
    last: Option<i64>,
    rings: Vec<VecDeque<f64>>,
    /// The output instants: the first's time, and the next's index.
    origin: f64,
    next: i64,
}

impl Resample {
    fn new(voices: usize, period: f64, rate: f64) -> Resample {
        let reach = RATE_HALF as f64 / (rate * period).min(1.0);
        let taps = 2 * reach.ceil() as usize + 3;
        Resample {
            period,
            rate,
            reach,
            first: 0,
            last: None,
            rings: vec![VecDeque::with_capacity(2 * taps); voices],
            origin: 0.0,
            next: 0,
        }
    }

    /// The outputs begin at `origin`, with none of the read's samples in.
    fn start(&mut self, origin: f64) {
        (self.origin, self.next, self.last) = (origin, 0, None);
        for r in &mut self.rings {
            r.clear();
        }
    }

    /// Output frames an output waits behind the run's newest instant: the read's own wait
    /// ([`BAND_LAG`]) and the reach, rounded up.
    fn delay_frames(&self) -> u32 {
        ((BAND_LAG as f64 + self.reach) * self.period * self.rate).ceil() as u32
    }

    /// The read's next sample, at its instant `n`, each voice's in `x`. O(voices).
    fn push(&mut self, n: i64, x: &[f64]) {
        if self.last.is_none() {
            // before the read's first sample, that sample: as many as an output reaches back
            let before = self.reach.ceil() as usize + 1;
            self.first = n - before as i64;
            for (r, &v) in self.rings.iter_mut().zip(x) {
                r.clear();
                r.extend(std::iter::repeat_n(v, before));
            }
        }
        for (r, &v) in self.rings.iter_mut().zip(x) {
            r.push_back(v);
        }
        self.last = Some(n);
    }

    /// The next output, each voice's in `out`, once the read's samples it reaches are in or,
    /// `all`, once its instant is (past the read's last sample, that sample). O(voices x taps).
    fn pop(&mut self, out: &mut [f64], all: bool) -> bool {
        let Some(last) = self.last else { return false };
        // the output's instant in the read's periods
        let c = (self.origin + self.next as f64 / self.rate) / self.period;
        let hi = (c + self.reach).floor() as i64;
        if hi > last && !(all && c <= last as f64) {
            return false;
        }
        out.fill(0.0);
        let (rings, first) = (&self.rings, self.first);
        let sum = rate_change(c, 1.0 / (self.rate * self.period), first, last, |j, w| {
            for (r, o) in rings.iter().zip(out.iter_mut()) {
                *o += w * r[(j - first) as usize];
            }
            true
        });
        if let Some(sum) = sum {
            out.iter_mut().for_each(|o| *o /= sum);
        }
        self.next += 1;
        // what no later output reaches goes (the newest stays)
        let keep = ((self.origin + self.next as f64 / self.rate) / self.period - self.reach).ceil() as i64;
        while self.first < keep.min(last) {
            for r in &mut self.rings {
                r.pop_front();
            }
            self.first += 1;
        }
        true
    }
}

/// The read's band-limited rate change at one output instant: the input samples around `pos` (in
/// the input's own indices; `step`, the output's period over the input's) through the read's
/// kernel at the lower of the two Nyquists over [`RATE_HALF`] periods of the lower rate each way:
/// `2 x 16 x max(1, step) + 1` samples. `take(j, w)` adds input sample `j` at the kernel's weight
/// `w` there (a few are below 0), each sample once and in order, and says whether it is there
/// (false stops at the first missing, and the result is None). A sample before `first` or after
/// `last` reads that end sample. The result is the weights' sum: what `take` added, divided by it,
/// is the output, so a constant stays exact. Each weight is made once. Equal rates at a whole
/// sample read that sample alone (at weight 1). The run's read at another rate uses it (a take at
/// another recording rate, the live grid at another device rate), and so does a take played at
/// another device rate. Table reads only once the kernel is made ([`rate_kernel`]): no
/// allocation, no lock. O(32 x max(1, step)).
pub fn rate_change(pos: f64, step: f64, first: i64, last: i64, mut take: impl FnMut(i64, f64) -> bool) -> Option<f64> {
    if last < first {
        return None;
    }
    if step == 1.0 && pos.fract() == 0.0 {
        return take((pos as i64).clamp(first, last), 1.0).then_some(1.0);
    }
    let k = band_kernel();
    // the kernel's scale in the input's samples (the lower rate over the input's) and its reach
    let scale = if step > 1.0 { 1.0 / step } else { 1.0 };
    let reach = RATE_HALF as f64 / scale;
    let (lo, hi) = ((pos - reach).ceil() as i64, (pos + reach).floor() as i64);
    let weight = |j: i64| k.weight((pos - j as f64) * scale);
    // the samples there are; those past the ends fold onto the end samples
    let (a, b) = (lo.clamp(first, last), hi.clamp(first, last));
    if hi < first || lo > last {
        // the whole reach past one end: that end sample alone
        let w: f64 = (lo..=hi).map(weight).sum();
        return (w == 0.0 || take(a, w)).then_some(w);
    }
    let (before, after): (f64, f64) = ((lo..a).map(weight).sum(), (b + 1..=hi).map(weight).sum());
    let wa = weight(a) + before + if a == b { after } else { 0.0 };
    let mut sum = wa;
    if wa != 0.0 && !take(a, wa) {
        return None;
    }
    if b > a {
        for j in a + 1..b {
            let w = weight(j);
            sum += w;
            if w != 0.0 && !take(j, w) {
                return None;
            }
        }
        let wb = weight(b) + after;
        sum += wb;
        if wb != 0.0 && !take(b, wb) {
            return None;
        }
    }
    Some(sum)
}

/// Makes the read's kernel tables now (they are made once): a caller that must not allocate (the
/// audio callback) has them made ahead.
pub fn rate_kernel() {
    band_kernel();
}

/// How far the read's rate change reaches each way ([`rate_change`]), in periods of the lower rate.
pub const RATE_HALF: usize = BAND_HALF;

/// A sink that mixes its voices as the run's read gives them ([`Band`], 17 periods behind the
/// run) at the device's rate, following it when it changes: as it is where the device runs at
/// the run's rate, else through a band-limited rate change ([`Resample`]); and records them at its
/// run's recording rate the same way ([`crate::recording`]). A tap never told the run's period
/// plays and records its voices' averages over each period of the accepted points' linear
/// segments. A live run that goes back to a snapshot it saved (`Sink::save`, `Sink::rewind`) goes
/// on with the same take, sample for sample as if the run had never gone past it, while live play
/// goes on from where it was heard to and never repeats. `begin` searches the unknown names; a
/// point costs O(voices x (samples emitted + 1)) and a rate change's output O(voices x taps),
/// excluding worker waits. Nothing allocates after construction but a take's start and a change
/// of the device's rate or the run's period, which make a rate change anew. Only the worker may
/// call its sink methods.
pub struct AudioTap {
    voices: Vec<Voice>,
    latency: f64,
    dc_block_hz: f64,
    mix: Arc<TapMix>,
    ring: Arc<SampleRing>,
    /// The ring's paced push and the speed measure.
    paced: PacedProducer,
    /// The live period grid, at the device's rate.
    live: Grid,
    ahead: usize,
    /// The consumer's rate was known at `begin`; until it is, the first point waits for it.
    rate_known: bool,
    /// The RMS meters' smoothing per sample.
    ms_a: f64,
    /// The last point's time; None before the first.
    last_t: Option<f64>,
    /// The mix's meter state, and samples since the meters were written.
    mix_ms: f64,
    mix_peak: f64,
    metered: u32,
    rec: Option<Recorder>,
    /// It makes the live samples (false: it only records, [`AudioTap::recorder`]).
    live_on: bool,
    /// The newest point's time over every run: an earlier one starts a new take.
    front_t: Option<f64>,
    /// The time the live grid has played to: a run that went back (`Sink::rewind`) is heard
    /// live again only past it, so live play never repeats.
    live_to: f64,
    /// The run's read on its grid, once the run says its period ([`Sink::sample_period`]), and a
    /// sample's values from it.
    band: Option<Band>,
    band_out: Vec<f64>,
    /// The live grid plays the read (the run told its period), through `live_u` where the device
    /// runs at another rate; the read's next instant it takes (live play never repeats one).
    live_banded: bool,
    live_u: Option<Resample>,
    live_n: i64,
    /// A rate change's output's values.
    u_out: Vec<f64>,
    /// In a burst of rows (`Sink::rows_begin`), and whether its pacing clock was read for it: the
    /// burst's points come within microseconds, so one read stands for them all.
    in_rows: bool,
    rows_timed: bool,
}

impl AudioTap {
    /// Takes ownership of the settings and shared ring in O(voices). `begin` resolves the names
    /// and reads the device rate, using 48 000 Hz when it is unknown.
    pub fn new(spec: TapSpec, ring: Arc<SampleRing>) -> AudioTap {
        let live = Grid::new(spec.voices.len(), 48_000.0, spec.dc_block_hz);
        let voices = spec.voices.into_iter().map(|spec| Voice { spec, at: None, last: 0.0, now: 0.0, ms: 0.0, peak: 0.0 }).collect();
        Self {
            voices,
            latency: spec.latency,
            dc_block_hz: spec.dc_block_hz,
            mix: spec.mix,
            paced: PacedProducer::new(ring.clone()),
            ring,
            live,
            ahead: 1,
            rate_known: false,
            ms_a: 1.0,
            last_t: None,
            mix_ms: 0.0,
            mix_peak: 0.0,
            metered: 0,
            rec: None,
            live_on: true,
            front_t: None,
            live_to: f64::NEG_INFINITY,
            band: None,
            band_out: Vec::new(),
            live_banded: false,
            live_u: None,
            live_n: i64::MIN,
            u_out: Vec::new(),
            in_rows: false,
            rows_timed: false,
        }
    }

    /// Instants of the run's grid read inside a step that is not free (no point there: the step's
    /// chord is read), over the tap's life. O(1).
    pub fn off_grid(&self) -> u64 {
        self.band.as_ref().map_or(0, |b| b.counts.off_grid)
    }

    /// Returns the run's banded read counts, or `None` while the tap reads period averages. O(1).
    pub fn read_counts(&self) -> Option<ReadCounts> {
        self.band.as_ref().map(|b| b.counts)
    }

    /// The run's read feeds a grid at `rate`: the run's own.
    fn on_band(&self, rate: f64) -> bool {
        self.band.as_ref().is_some_and(|b| (b.period * rate - 1.0).abs() < 1e-9)
    }

    /// The run's read takes its newest point (its kind is in) and an edge point it holds, as they
    /// came, and sends every sample whose motion is in, as it is: at a rest, where no point follows
    /// before a command.
    fn flush_band(&mut self) {
        if let Some(band) = &mut self.band {
            band.take_all();
        }
        self.drain_band(true);
    }

    /// The run's read's final samples (`all`: every one whose motion is in, as it is) go to the
    /// grids: live from its next instant on, and the take's unless it follows a held take's tape;
    /// each as it is at the run's rate, else through its rate change (`all`: up to the read's last
    /// instant). O(voices) a sample, O(voices x taps) a rate change's output.
    fn drain_band(&mut self, all: bool) {
        let following = self.held();
        while let Some(n) = self.band.as_mut().and_then(|b| b.pop(&mut self.band_out, all)) {
            if self.live_on && self.live_banded && n >= self.live_n {
                self.live_n = n + 1;
                if let Some(u) = self.live_u.as_mut() {
                    u.push(n, &self.band_out);
                } else {
                    self.live.put(&self.band_out);
                    self.emit_live();
                }
                self.drain_live(false);
            }
            if let Some(rec) = self.rec.as_mut().filter(|rec| rec.banded && !following) {
                if let Some(u) = rec.u.as_mut() {
                    u.push(n, &self.band_out);
                } else {
                    rec.grid.put(&self.band_out);
                    rec.emit(&self.voices, &self.ring);
                }
                rec.drain_u(&mut self.u_out, false, &self.voices, &self.ring);
            }
        }
        if all {
            self.drain_live(true);
            if let Some(rec) = self.rec.as_mut().filter(|rec| rec.banded && !following) {
                rec.drain_u(&mut self.u_out, true, &self.voices, &self.ring);
            }
        }
    }

    /// The live rate change's outputs that are ready go to the live grid (`all`: up to the read's
    /// last instant).
    fn drain_live(&mut self, all: bool) {
        while self.live_u.as_mut().is_some_and(|u| u.pop(&mut self.u_out, all)) {
            self.live.put(&self.u_out);
            self.emit_live();
        }
    }

    /// The live grid plays the run's read once the run told its period: as it is at the run's
    /// rate, else through a rate change from the grid's next instant, made anew only when either
    /// rate changed. The latency read-out counts the wait.
    fn live_read(&mut self) {
        self.live_banded = self.band.is_some();
        let want = self.band.as_ref().filter(|_| !self.on_band(self.live.rate)).map(|b| (b.period, self.live.rate));
        if self.live_u.as_ref().map(|u| (u.period, u.rate)) != want {
            let (voices, next) = (self.voices.len(), self.live.origin + self.live.periods as f64 / self.live.rate);
            self.live_u = want.map(|(period, rate)| {
                let mut u = Resample::new(voices, period, rate);
                u.start(next);
                u
            });
        }
        if self.live_on {
            let frames = match &self.live_u {
                Some(u) => u.delay_frames(),
                None if self.live_banded => BAND_LAG as u32,
                None => 0,
            };
            self.ring.set_read_delay(frames);
        }
    }

    /// The tap records too: every voice before its level, for the player to play while the run is
    /// below real time ([`crate::recording`]).
    pub fn recording(mut self, spec: RecordSpec) -> AudioTap {
        spec.tapes.set_error(None);
        let (buf, unsent) = (Vec::new(), VecDeque::new());
        let (rate, grid) = (self.live.rate, Grid::new(self.voices.len(), self.live.rate, self.dc_block_hz));
        let source = self.source();
        let keys = self.voices.iter().map(|v| voice_key(&v.spec, self.dc_block_hz)).collect();
        self.rec = Some(Recorder { spec, take: 0, index: 0, buf, unsent, slots: 1, rate, grid, banded: false, run: None, tape: None, taped: 0, taping: true, t0: f64::NAN, sent: None, replay: None, served: 0, cached: Vec::new(), pick: None, at_rest: false, source, keys, tape_only: false, at_bound: false, u: None });
        self
    }

    /// A tap that only records (a board's output jack, taped from its run's start whether anyone
    /// hears it or not): no live samples, so it never claims the ring, measures the speed, paces
    /// or holds the run, and it sends the player nothing: its takes go to the tape alone, in the
    /// shelf's slot `record.slot`, each until its tape reaches `record.bound` (counted in
    /// `TapeShelf::bounded`; the run goes on). `ring` gives the take's rate only for a run with
    /// none of its own. Per point O(voices), per chunk one append to the tape.
    pub fn recorder(spec: TapSpec, ring: Arc<SampleRing>, record: RecordSpec) -> AudioTap {
        let mut tap = AudioTap::new(spec, ring).recording(record);
        tap.live_on = false;
        if let Some(rec) = &mut tap.rec {
            rec.tape_only = true;
        }
        tap
    }

    /// Held: a take played from another recording's columns, no run's points since.
    fn held(&self) -> bool {
        self.rec.as_ref().is_some_and(|rec| rec.pick.is_some())
    }

    /// The tap's settings that shape its samples, as a key: its voices (nets, references, gains)
    /// and DC block. O(voices).
    fn source(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for v in &self.voices {
            (&v.spec.net, &v.spec.reference, v.spec.gain.to_bits()).hash(&mut h);
        }
        (self.dc_block_hz.to_bits(), BAND_READ).hash(&mut h);
        h.finish()
    }

    /// The cumulative sample and waiting counts; constant time.
    pub fn stats(&self) -> &TapStats {
        self.paced.stats()
    }

    /// Takes the consumer's rate (zero: unknown, 48 000 Hz until it is) and what depends on it:
    /// the live grid (its DC blocks), the lead (the ring keeps the player's once set) and the
    /// meters' smoothing. A take keeps its own rate, its run's.
    fn set_rate(&mut self, hz: u32) {
        self.rate_known = hz > 0;
        let rate = if hz > 0 { f64::from(hz) } else { 48_000.0 };
        self.live.set_rate(rate);
        self.live_read();
        self.ahead = (self.latency * rate).ceil().max(1.0).min(self.ring.capacity() as f64) as usize;
        if self.rate_known && self.ring.ahead() == 0 {
            self.ring.set_ahead(self.ahead);
        }
        self.ms_a = 1.0 - (-1.0 / (METER_SECONDS * rate)).exp();
    }

    /// Before the first period: a consumer that has not said its rate yet (a device just opened)
    /// is waited for on the worker, at most [`STALL`], so its samples are made at its rate.
    fn await_rate(&mut self) {
        let since = Instant::now();
        while !self.rate_known && self.ring.is_open() && since.elapsed() < STALL {
            match self.ring.device_rate() {
                0 => std::thread::sleep(Duration::from_millis(1)),
                hz => self.set_rate(hz),
            }
        }
        self.rate_known = true;
    }

    fn reset(&mut self) {
        (self.last_t, self.live_to, self.live_n) = (None, f64::NEG_INFINITY, i64::MIN);
        self.live.reset();
        if let Some(band) = &mut self.band {
            band.reset();
        }
        if let Some(u) = &mut self.live_u {
            u.start(0.0);
        }
        if let Some(rec) = &mut self.rec {
            rec.grid.reset();
            if let Some(u) = &mut rec.u {
                u.start(0.0);
            }
        }
    }

    /// The live period's sample: each voice's at its level and metered, the sum scaled by the
    /// master, soft limited and metered, or silence while muted.
    fn emit_live(&mut self) {
        let ms_a = self.ms_a;
        let mut sum = 0.0;
        for (i, v) in self.voices.iter_mut().enumerate() {
            let y = self.live.full(i, v) * f64::from(self.mix.level(i));
            v.ms += ms_a * (y * y - v.ms);
            v.peak = v.peak.max(y.abs());
            sum += y;
        }
        // the recording plays: the player mixes and meters it, and the ring waits for live play
        if self.ring.plays_recording() {
            return;
        }
        let out = if self.mix.muted() { 0.0 } else { soft_limit(sum * f64::from(self.mix.master())) };
        self.mix_ms += ms_a * (out * out - self.mix_ms);
        self.mix_peak = self.mix_peak.max(out.abs());
        self.metered += 1;
        if self.metered >= METER_EVERY {
            self.metered = 0;
            for (i, v) in self.voices.iter_mut().enumerate() {
                self.mix.note(i, v.peak as f32, v.ms.sqrt() as f32);
                v.peak = 0.0;
            }
            self.mix.note(self.voices.len(), self.mix_peak as f32, self.mix_ms.sqrt() as f32);
            self.mix_peak = 0.0;
        }
        self.paced.queue(out as f32, self.ahead);
    }
}

/// The live push into a ring both kinds of tap use (a net tap's [`AudioTap`], a device's
/// [`crate::device_audio::DeviceTap`]): the sole producer's claim, the wait that paces the worker
/// on the consumer (it waits while the ring holds the lead, and never on a closed ring or one whose
/// consumer stopped taking for [`STALL`]), the counts, and the measure of the run's speed it
/// publishes on the ring. O(1) per sample but the wait; no allocation.
pub struct PacedProducer {
    ring: Arc<SampleRing>,
    producer: Option<RingProducer>,
    stats: TapStats,
    /// The consumer's `consumed` count when it was taken for stalled; cleared once it moves.
    stalled_at: Option<usize>,
    /// The speed measure: the wall time of the last point, stepping time, simulated time and the
    /// waits for the clock since the last reading, time this point waited on the ring and for the
    /// clock, and whether the run rested since.
    wall: Option<Instant>,
    busy: f64,
    stepped: f64,
    clock: f64,
    waited: Duration,
    rested: bool,
}

impl PacedProducer {
    pub fn new(ring: Arc<SampleRing>) -> PacedProducer {
        PacedProducer { ring, producer: None, stats: TapStats::default(), stalled_at: None, wall: None, busy: 0.0, stepped: 0.0, clock: 0.0, waited: Duration::ZERO, rested: true }
    }

    /// The cumulative sample and waiting counts; constant time.
    pub fn stats(&self) -> &TapStats {
        &self.stats
    }

    /// Whether it holds the producer of an open ring: a listener hears what it queues. O(1).
    pub fn listened(&self) -> bool {
        self.producer.is_some() && self.ring.is_open()
    }

    /// Claims the ring's producer unless it holds it; whether it holds it now. O(1).
    pub fn claim(&mut self) -> bool {
        if self.producer.is_none() {
            self.producer = self.ring.claim();
        }
        self.producer.is_some()
    }

    /// Says whether samples flow, when it holds the producer. O(1).
    pub fn set_running(&self, on: bool) {
        if let Some(producer) = &self.producer {
            producer.set_running(on);
        }
    }

    /// A run that starts over is measured afresh: no speed until its own first reading.
    pub fn restart_speed(&mut self) {
        (self.wall, self.busy, self.stepped, self.clock, self.waited) = (None, 0.0, 0.0, 0.0, Duration::ZERO);
        self.ring.set_tap_speed(0.0);
    }

    /// The speed is the run's from here: neither its start nor its first plan counts.
    pub fn started(&mut self) {
        (self.rested, self.busy, self.stepped, self.clock) = (true, 0.0, 0.0, 0.0);
    }

    /// The run rested (it settled, or held for the player): the wall time until its next point
    /// is not stepping.
    pub fn rest(&mut self) {
        self.rested = true;
    }

    /// A point while it steps: the wall time since the last, less the ring's waits, the clock's
    /// and the rests, is stepping; every [`SPEED_BUSY_SECONDS`] of it and of the clock's waits
    /// publishes a reading, simulated time over stepping: what the run can do, so a paced run
    /// with time to spare reads above real time.
    pub fn point(&mut self) {
        let now = Instant::now();
        if let (Some(prev), false) = (self.wall, self.rested) {
            self.busy += (now - prev).saturating_sub(self.waited).as_secs_f64();
        }
        (self.wall, self.rested, self.waited) = (Some(now), false, Duration::ZERO);
        if self.busy + self.clock >= SPEED_BUSY_SECONDS && self.busy > 0.0 {
            self.ring.set_tap_speed((self.stepped / self.busy) as f32);
            (self.busy, self.stepped, self.clock) = (0.0, 0.0, 0.0);
        }
    }

    /// The run waited `wall` for its clock: not stepping (`Sink::clock_wait`).
    pub fn clock_wait(&mut self, wall: Duration) {
        if self.wall.is_some() && !self.rested {
            self.waited += wall;
            self.clock += wall.as_secs_f64();
        }
    }

    /// Simulated seconds stepped since the last point.
    pub fn stepped(&mut self, dt: f64) {
        self.stepped += dt;
    }

    /// Pushes one sample, waiting while the open ring holds the player's lead (`lead` until the
    /// player sets one). Only while it holds the producer.
    pub fn queue(&mut self, sample: f32, lead: usize) {
        let producer = self.producer.as_mut().expect("a paced tap holds its producer");
        if self.stalled_at.is_some_and(|at| self.ring.consumed() != at) {
            self.stalled_at = None;
        }
        // the wait's start and the consumer's count then; a count that moves restarts the watch
        let mut watch: Option<(Instant, usize)> = None;
        loop {
            let open = self.ring.is_open();
            // the player's measured lead once it set one
            let ahead = match self.ring.ahead() {
                0 => lead,
                n => n,
            };
            if open && self.stalled_at.is_none() && (self.ring.fill() >= ahead || producer.free() == 0) {
                let consumed = self.ring.consumed();
                match watch {
                    None => {
                        self.stats.waits += 1;
                        watch = Some((Instant::now(), consumed));
                    }
                    Some((_, seen)) if seen != consumed => watch = Some((Instant::now(), consumed)),
                    Some((since, _)) if since.elapsed() >= STALL => {
                        self.stats.stalls += 1;
                        self.stalled_at = Some(consumed);
                        continue;
                    }
                    Some(_) => {}
                }
                let slept = Instant::now();
                std::thread::sleep(Duration::from_millis(1));
                self.waited += slept.elapsed();
                continue;
            }
            if producer.push(sample) {
                self.stats.samples += 1;
                return;
            }
            // Closing releases a waiting worker. If listening resumed since the failed push,
            // retry instead of losing the sample.
            if !open {
                self.stats.dropped_closed += 1;
                return;
            }
            if self.stalled_at.is_some() {
                self.stats.dropped_stalled += 1;
                return;
            }
        }
    }
}

impl Sink for AudioTap {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.idle();
        self.reset();
        // a run that starts over is measured afresh: no speed until its own first reading
        self.paced.restart_speed();
        // a held take goes in over a take at rest, at its end: the player moves into it from dry
        // live play once its window is whole
        let ended = self.rec.as_mut().is_some_and(|rec| rec.pick.is_some() && std::mem::take(&mut rec.at_rest));
        self.ring.set_ended(ended);
        let find = |name: &str| unknowns.iter().position(|u| u.name == name);
        for v in &mut self.voices {
            let reference = match &v.spec.reference {
                Some(name) => find(name),
                None => Some(0),
            };
            v.at = find(&v.spec.net).zip(reference);
        }
        self.set_rate(self.ring.device_rate());
        if self.live_on {
            self.paced.claim();
        }
    }

    fn clock_wait(&mut self, wall: std::time::Duration) {
        if self.live_on {
            self.paced.clock_wait(wall);
        }
    }

    fn listened(&self) -> bool {
        self.live_on && self.paced.listened()
    }

    /// Its resolved voices and their references, live or only recorded. O(voices).
    fn heard(&mut self, out: &mut Vec<u32>) {
        for (net, reference) in self.voices.iter().filter_map(|v| v.at) {
            out.extend([net as u32, reference as u32]);
        }
    }

    fn point(&mut self, t: f64, _outer: Option<f64>, x: &[f64]) {
        // the run went on past where it rested (an edit sent it back, or its end moved later)
        if self.ring.ended() {
            self.ring.set_ended(false);
        }
        // nothing to hear: no voice resolved
        if !self.voices.iter().any(|v| v.at.is_some()) {
            return;
        }
        if self.live_on {
            if !self.paced.claim() {
                return;
            }
            self.paced.set_running(true);
            // the run's speed while it steps: once a burst of rows
            if !(self.in_rows && self.rows_timed) {
                self.paced.point();
                self.rows_timed = self.in_rows;
            }
        }
        // the first point, and a run that starts over, begin a take of the recording
        let take = match self.front_t {
            Some(front) if t < front => Some(true),
            None => Some(false),
            Some(_) => None,
        };
        // a held take follows its recorder's tape and records nothing (live play goes on); a run
        // that starts over with no rewind (a board without snapshots) ends it: a new take
        let following = self.held() && take != Some(true);
        self.front_t = Some(t);
        for v in &mut self.voices {
            v.now = v.at.map_or(0.0, |(net, reference)| x[net] - x[reference]);
        }
        if self.last_t.is_some_and(|last_t| t < last_t) {
            self.reset();
        }
        // a device that changed its rate is played at the new one from here; a take keeps its own
        if self.rate_known {
            let hz = self.ring.device_rate();
            if hz > 0 && f64::from(hz) != self.live.rate {
                self.set_rate(hz);
            }
        }
        let Some(last_t) = self.last_t else {
            if self.live_on && !self.rate_known {
                self.await_rate();
            }
            let (live, voices) = (self.live.rate, self.voices.len());
            // a grid that plays the run's read starts at its first instant
            let period = self.band.as_ref().map(|b| b.period);
            let first = self.band.as_ref().map_or(t, |b| b.instant(b.first_at(t)));
            self.live.start(if self.live_banded { first } else { t });
            if let Some(u) = &mut self.live_u {
                u.start(first);
            }
            if let Some(rec) = self.rec.as_mut().filter(|_| !following) {
                if let Some(again) = take {
                    rec.begin_take(t, again, live, voices, period);
                }
                rec.grid.start(if rec.banded { first } else { t });
                if let Some(u) = &mut rec.u {
                    u.start(first);
                }
            }
            self.last_t = Some(t);
            for v in &mut self.voices {
                v.last = v.now;
            }
            if let Some(band) = &mut self.band {
                band.point(t, self.voices.iter().map(|v| v.now));
            }
            return;
        };
        if self.live_on {
            self.paced.stepped(t - last_t);
            // live play goes on from where it was heard to: after a rewind, past the time it
            // played
            let from = last_t.max(self.live_to);
            if from < t && !self.live_banded {
                self.live.segment(&self.voices, (from - last_t) / (t - last_t));
                let mut at = from;
                while self.live.cross(&mut at, last_t, t, &self.voices) {
                    self.emit_live();
                }
            }
        }
        self.live_to = self.live_to.max(t);
        if let Some(rec) = &mut self.rec {
            if following {
                rec.flush(&self.ring);
            } else if !rec.banded {
                rec.grid.segment(&self.voices, 0.0);
                let mut at = last_t;
                while rec.grid.cross(&mut at, last_t, t, &self.voices) {
                    rec.emit(&self.voices, &self.ring);
                }
            }
        }
        // Equal times are jumps: no area, but the next segment starts at the new value.
        self.last_t = Some(t);
        for v in &mut self.voices {
            v.last = v.now;
        }
        if let Some(band) = &mut self.band {
            band.point(t, self.voices.iter().map(|v| v.now));
            self.drain_band(false);
        }
    }

    fn chosen(&mut self, at: f64) {
        if let Some(band) = &mut self.band {
            band.chosen(at);
        }
    }

    /// The run's read starts on the run's grid: the live grid plays it and a take records it, as
    /// it is at the run's rate and through a rate change at another. Told after `begin`: nothing of
    /// it is pending.
    fn sample_period(&mut self, period: f64) {
        self.band = (period > 0.0 && period.is_finite()).then(|| match self.band.take() {
            Some(band) if band.period == period => band,
            _ => Band::new(self.voices.len(), period),
        });
        self.band_out.resize(self.voices.len(), 0.0);
        self.u_out.resize(self.voices.len(), 0.0);
        self.live_read();
    }

    fn started(&mut self) {
        // the speed is the run's from here: neither its start nor its first plan counts
        self.paced.started();
    }

    fn takes_values(&self) -> bool {
        false
    }

    fn rows_begin(&mut self) {
        (self.in_rows, self.rows_timed) = (true, false);
    }

    fn rows_end(&mut self) {
        self.in_rows = false;
    }

    fn idle(&mut self) {
        self.paced.rest();
        self.paced.set_running(false);
        if let Some(rec) = &mut self.rec {
            rec.flush(&self.ring);
        }
    }

    fn steady(&mut self, _at: f64, _x: &[f64]) {
        // the read's pending samples go out as they are: no point comes before a command
        self.flush_band();
        self.idle();
    }

    fn ended(&mut self, _at: f64) {
        self.flush_band();
        // a held take's end is its recorder's
        if let (Some(rec), Some(last_t)) = (self.rec.as_mut().filter(|rec| rec.pick.is_none()), self.last_t) {
            rec.close_take(last_t, &self.voices, &self.ring);
        }
        self.ring.set_ended(true);
    }

    /// A held take not served whole yet: the resting run asks again (`idle`) until it is.
    fn pending(&self) -> bool {
        self.rec.as_ref().is_some_and(|rec| rec.pick.is_some() && (!rec.unsent.is_empty() || rec.replay.as_ref().is_some_and(|(_, chunks)| rec.served < *chunks)))
    }

    fn progress(&self) -> u64 {
        self.rec.as_ref().map_or(0, |rec| rec.served)
    }

    /// The take plays from the recording on `shelf`'s slot when it has every voice of this tap:
    /// this run's take from its start (its content this run's), at the rate this tap would record
    /// it at, no error, and a whole window of this tap's long once the run is at rest (queued), or
    /// room for one while it renders (its bound). The tap then serves those columns as its own
    /// take (`serve`) as the recorder tapes them, from the run's points and the resting run's
    /// passes, and records nothing: it follows the tape through the rest of the render and the
    /// run's edits (`rewind`, `point`), until a new run, which it records itself. Called before
    /// the tap goes in, on the thread that holds it; no I/O. O(voices x the tape's voices).
    fn play_recorded(&mut self, shelf: &TapeShelf, slot: u64, at_rest: bool) -> Result<(), String> {
        let Some(rec) = &mut self.rec else { return Err("this Listen records nothing".to_owned()) };
        let Some(tape) = shelf.current(slot) else { return Err("nothing was recorded in this take".to_owned()) };
        if let Some(why) = tape.error() {
            return Err(format!("the tape failed ({why})"));
        }
        let run = rec.spec.runs.current();
        if !tape.content.as_ref().zip(run.as_ref()).is_some_and(|(a, b)| Arc::ptr_eq(a, b)) {
            return Err("the tape started after the take did".to_owned());
        }
        // the rate `begin_take` takes: the run's recording rate, else the device's
        let rate = run.as_ref().map(|r| r.recording_rate).filter(|hz| hz.is_finite() && *hz > 0.0).unwrap_or_else(|| f64::from(self.ring.device_rate()));
        if tape.rate != rate {
            return Err(format!("the tape's rate differs ({} Hz, {} Hz needed)", tape.rate, rate));
        }
        let most = window_frames(rec.spec.window_s, MAX_RATE);
        let frames = window_frames(rec.spec.window_s, rate).min(most);
        if at_rest && tape.promised() < frames as u64 {
            return Err(format!("the tape stopped short ({:.3} of {:.3} s)", tape.promised() as f64 / rate, frames as f64 / rate));
        }
        // a tape nobody hears stops at its bound (`Recorder::tape`): one that will, before the
        // take's end, is no take to follow
        if !at_rest && frames as u64 * 4 * tape.voices as u64 > KEEP_BYTES {
            return Err(format!("the tape stops short of the take ({:.3} s)", frames as f64 / rate));
        }
        let mut pick = Vec::with_capacity(self.voices.len());
        for v in &self.voices {
            let key = voice_key(&v.spec, self.dc_block_hz);
            match tape.keys.iter().position(|&k| k == key) {
                Some(column) => pick.push(column),
                None => return Err(format!("voice not recorded in this take ({})", v.spec.net)),
            }
        }
        rec.slots = (frames / CHUNK) as u64;
        (rec.rate, rec.t0, rec.run, rec.index, rec.served, rec.sent) = (rate, tape.t0, run, 0, 0, None);
        rec.unsent.clear();
        // at rest the run rests at the take's end: a run's point before it (a new run) is a new
        // take; while it renders its points go on from its front
        self.front_t = at_rest.then(|| tape.t0 + frames as f64 / rate);
        rec.at_rest = at_rest;
        rec.replay = Some((tape, (frames / CHUNK) as u64));
        rec.pick = Some(pick.into());
        Ok(())
    }

    /// The recording is a window past the one heard (or the player's queue is full): the run
    /// waits until the player moves on. Never while nobody listens.
    fn holds(&mut self) -> bool {
        let Some(rec) = &mut self.rec else { return false };
        rec.spec.spent.drain();
        rec.flush(&self.ring);
        let held = self.ring.is_open() && self.ring.plays_recording() && !rec.unsent.is_empty();
        if held {
            self.paced.rest();
        }
        held
    }

    /// The last point's time and each voice's value there, and the take's state: so a rewind to
    /// this snapshot goes on with the same take, sample for sample. O(voices + a chunk).
    fn save(&self) -> Option<Box<dyn SinkState>> {
        let t = self.last_t?;
        let take = self.rec.as_ref().map(|rec| TakeState { take: rec.take, index: rec.index, buf: rec.buf.clone(), grid: rec.grid.clone(), u: rec.u.clone() });
        Some(Box::new(TapState { t, lasts: self.voices.iter().map(|v| v.last).collect(), take, band: self.band.clone() }))
    }

    /// The run went back to a snapshot this tap saved: its take goes on from there (`Recorder::
    /// rewind`), and live play goes on from where it was heard to. Without a state of this take
    /// (the tap came after the snapshot, or a take began since), it begins again: a new take. A
    /// held take follows its recorder back (`Recorder::follow_back`), its take and numbers kept.
    fn rewind(&mut self, at: f64, saved: Option<&dyn SinkState>, unknowns: &[UnknownInfo]) {
        if let Some(rec) = self.rec.as_mut().filter(|rec| rec.pick.is_some()) {
            // back before the take's first point, its recorder takes the take anew on a new tape:
            // the hold ends, and the tap records the take itself from there, a new take
            if at < rec.t0 {
                (rec.pick, rec.replay) = (None, None);
                self.begin(unknowns);
                self.front_t = Some(f64::INFINITY);
                return;
            }
            rec.follow_back(at);
            let find = |name: &str| unknowns.iter().position(|u| u.name == name);
            for v in &mut self.voices {
                let reference = v.spec.reference.as_deref().map_or(Some(0), find);
                v.at = find(&v.spec.net).zip(reference);
            }
            (self.last_t, self.front_t) = (None, None);
            self.live.reset();
            if let Some(band) = &mut self.band {
                band.reset();
            }
            if let Some(u) = &mut self.live_u {
                u.start(0.0);
            }
            return;
        }
        let take = self.rec.as_ref().map(|rec| rec.take);
        let saved = saved.and_then(|s| s.downcast_ref::<TapState>()).filter(|s| s.lasts.len() == self.voices.len() && s.take.as_ref().map(|k| k.take) == take);
        let Some(s) = saved else { return self.begin(unknowns) };
        let find = |name: &str| unknowns.iter().position(|u| u.name == name);
        for (v, &last) in self.voices.iter_mut().zip(&s.lasts) {
            let reference = v.spec.reference.as_deref().map_or(Some(0), find);
            (v.at, v.last) = (find(&v.spec.net).zip(reference), last);
        }
        (self.last_t, self.front_t) = (Some(s.t), Some(s.t));
        // the read goes on as it stood (one told its period since begins at the next point)
        match (&mut self.band, &s.band) {
            (Some(band), Some(saved)) if band.period == saved.period => band.clone_from(saved),
            (Some(band), _) => band.reset(),
            (None, _) => {}
        }
        if let (Some(rec), Some(k)) = (&mut self.rec, &s.take) {
            rec.rewind(at, k);
        }
    }
}

/// An [`AudioTap`]'s state at a live run's snapshot ([`Sink::save`]).
#[derive(Clone)]
struct TapState {
    /// The last point's time and each voice's value there.
    t: f64,
    lasts: Vec<f64>,
    take: Option<TakeState>,
    band: Option<Band>,
}

/// A take's state at a snapshot: which take, the chunks made, the partial chunk, its grid and its
/// rate change.
#[derive(Clone)]
struct TakeState {
    take: u32,
    index: u64,
    buf: Vec<f32>,
    grid: Grid,
    u: Option<Resample>,
}

/// How long a fade or a crossfade takes: when the ring runs short, at a switch, and at a window's
/// seam.
pub const FADE_SECONDS: f64 = 0.003;

/// A recording's window when none is asked: a run below real time is heard in windows this long,
/// each looped until the next can play to its end.
pub const LOOP_SECONDS: f64 = 4.0;

/// How long the player reads the run's speed over.
const SPEED_SECONDS: f64 = 0.5;

/// Measures over real time a rendering run must go before it is heard live again.
const LIVE_AGAIN: u32 = 2;

/// A live run measured this fast keeps up: an underrun then is a stall of the worker, heard as a
/// short fade while the lead doubles, not a reason to render. Also where a live run reads as
/// behind: a run paced to real time measures just under one, and that is keeping up.
pub const KEEPS_UP: f32 = 0.97;

/// Quiet measures of live play before the lead is sized down to what the worker's stalls need.
const QUIET_MEASURES: u32 = 8;

/// A window starts before it is whole once what is left renders in this share of the time it
/// takes to play, at the measured speed.
const START_MARGIN: f32 = 0.9;

/// The consumer of a ring, for the audio device's callback or a silent clock. While the run keeps
/// up it plays the ring's samples as they come (live). When the run falls behind (an underrun of a
/// run measured below real time) it plays the run's recording instead ([`crate::recording`]): the
/// newest window it can play to its end without catching up with the render, looped until the
/// next can, which it moves to at the seam. The window that is not whole yet starts once what is
/// left renders in time at the measured speed, so Play starts sounding as soon as it can. Its
/// voices mix at their levels as it plays. A run measured faster than real time is heard live
/// again. Every switch, every seam and a start crossfade from the last sample heard
/// ([`FADE_SECONDS`]), so none clicks. In Realtime the player stays live, fading through
/// gaps and resuming as soon as one device buffer is ready (two at a run's start, before its first
/// speed reading: a start on one can run dry, a click). In the other modes live play primes to
/// the lead at a start, after a pause and after an underrun, and the hand can scratch a recording
/// in either direction, even while the transport is paused.
///
/// Live, it sizes the ring's lead ([`SampleRing::ahead`]) in either mode: two device buffers from
/// the device's first, an underrun of a run that keeps up (the worker stalled) doubles it, and
/// after [`QUIET_MEASURES`] measures without one it comes down to twice the deepest the worker
/// fell behind the lead, plus a device buffer, and never below two buffers. Its state goes to the ring's [`PlayStatus`]. `fill` is O(samples), with no
/// lock, wait or allocation after `new`; what it lets go goes back to the UI.
pub struct Player {
    realtime: bool,
    scrub: Scrub,
    rendered: bool,
    primed: bool,
    /// The fade to silence: the last sample heard from a source and the samples the fade has left.
    last: f32,
    fade_left: usize,
    links: Option<PlayerLinks>,
    win: Windows,
    /// The play head reached a window's render front: it waits there, silent.
    waiting: bool,
    /// The speed reading: device samples since the last.
    window: usize,
    fast: u32,
    speed: f32,
    /// The last sample out, and the crossfade from it: its start value and samples left.
    heard: f32,
    from: f32,
    blend: usize,
    /// The lead's measure: the device's largest buffer (in frames and seconds), the least the
    /// ring held before a live buffer, and the quiet measures since the lead last changed.
    frames: usize,
    buffer_s: f64,
    low: usize,
    quiet: u32,
    /// What the ring held as live play last asked for a buffer (before it took one): a change
    /// pushed now waits behind that much (`PlayStatus::latency_s`).
    queued: usize,
    /// Samples since the recording's meters were last written, and whether they read silence.
    metered: u32,
    meters_quiet: bool,
    /// The recording's residual: at a swap under the play head or a render's front, the
    /// difference from what played goes on, decaying to nothing over a fade
    /// ([`FADE_SECONDS`]): its start (the voices' sum) and the samples since. With the voices'
    /// sum last played.
    resid: f64,
    resid_n: usize,
    played: f64,
    /// The device frames handed out so far, and where they came from: the newest stretches of the
    /// run's time they play (`HEARD_SEGS` of them, round), for the heard record.
    out_at: u64,
    segs: [HeardSeg; HEARD_SEGS],
    seg_n: usize,
}

/// The stretches of output a player remembers: enough for the device's buffers in flight.
const HEARD_SEGS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scrub {
    Off,
    Dragging,
    Released,
}

/// A stretch of the device's output: from frame `from` on, the run's time `t` moving `dt` seconds a
/// frame (0: held, silent or waiting), of take `key` (packed).
#[derive(Clone, Copy, Debug, Default)]
struct HeardSeg {
    from: u64,
    t: f64,
    dt: f64,
    key: u64,
}

impl Player {
    /// A player. With `links` a run below real time is heard as its recording; without, it is
    /// silent until the run keeps up again.
    pub fn new(links: Option<PlayerLinks>) -> Player {
        Player {
            realtime: false,
            scrub: Scrub::Off,
            rendered: false,
            primed: false,
            last: 0.0,
            fade_left: 0,
            links,
            win: Windows::default(),
            waiting: false,
            window: 0,
            fast: 0,
            speed: 0.0,
            heard: 0.0,
            from: 0.0,
            blend: 0,
            frames: 0,
            buffer_s: 0.0,
            low: usize::MAX,
            quiet: 0,
            queued: 0,
            metered: 0,
            meters_quiet: false,
            resid: 0.0,
            resid_n: 0,
            played: 0.0,
            out_at: 0,
            segs: [HeardSeg::default(); HEARD_SEGS],
            seg_n: 0,
        }
    }

    /// From device frame `at` on, the output plays the run's time `t` moving `dt` a frame, of the
    /// heard table's take; nothing new when it carries on the stretch before. O(1).
    fn mark(&mut self, at: u64, t: f64, dt: f64) {
        let key = self.win.heard.and_then(|h| self.win.tabs[h].key).map_or(NONE, |(take, _)| pack(take));
        if let Some(last) = self.seg_n.checked_sub(1).map(|i| &mut self.segs[i % HEARD_SEGS]) {
            let carried = last.t + at.saturating_sub(last.from) as f64 * last.dt;
            if last.key == key && last.dt == dt && (carried - t).abs() <= 1e-9 * t.abs().max(1.0) {
                return;
            }
            if last.from == at {
                *last = HeardSeg { from: at, t, dt, key };
                return;
            }
        }
        self.segs[self.seg_n % HEARD_SEGS] = HeardSeg { from: at, t, dt, key };
        self.seg_n += 1;
    }

    /// The play head held where it is (silent: paused, waiting, nothing to play) from frame `at`.
    fn mark_held(&mut self, at: u64) {
        if let Some(t) = self.win.heard.and_then(|h| self.win.time_at(h, self.win.pos)) {
            self.mark(at, t, 0.0);
        }
    }

    /// Fills `out` (one channel at `rate` Hz) from `ring` or the recording (each take played at
    /// `rate` whatever its own).
    pub fn fill(&mut self, ring: &SampleRing, out: &mut [f32], rate: f64) {
        let fade = ((rate * FADE_SECONDS) as usize).max(1);
        if let Some(links) = &self.links {
            self.win.step = self.win.heard.map_or(0.0, |h| self.win.tabs[h].rate / rate);
            self.win.take(links);
        }
        // closed, what the ring still holds is drained unheard, so a later Listen starts fresh
        if !ring.is_open() {
            while ring.pop(out) > 0 {}
            out.fill(0.0);
            self.stop(ring);
            self.publish(ring, rate);
            return;
        }
        let realtime = ring.realtime();
        let first_buffer = self.frames == 0;
        self.frames = self.frames.max(out.len());
        self.buffer_s = self.frames as f64 / rate.max(1.0);
        // the lead starts on two device buffers in either mode, whatever was asked before the
        // device's first; an underrun of a run that keeps up doubles it and quiet measures bring
        // it back down (`size_lead`)
        let floor = 2 * self.frames;
        if floor > 0 && (realtime != self.realtime || first_buffer || ring.ahead() < floor) {
            ring.set_ahead(floor);
        }
        if realtime && !self.realtime {
            self.resume_live(ring);
            ring.rendered.store(false, Ordering::Relaxed);
        }
        self.realtime = realtime;
        let hushed = ring.hushed();
        let target = if realtime { None } else { ring.scrub() };
        let previous = self.scrub;
        if target.is_some() {
            self.scrub = Scrub::Dragging;
        } else if previous == Scrub::Dragging {
            if hushed || (ring.looping() && self.win.heard.is_some_and(|h| self.win.whole_at(h))) {
                self.scrub = Scrub::Released;
            } else {
                self.resume_live(ring);
            }
        }
        if let Some(t) = ring.take_seek().filter(|_| !realtime && target.is_none()) {
            self.seek_to(ring, t, fade);
        }
        self.measure(ring, out.len(), rate);
        if let Some(t) = target {
            self.scratch(ring, out, rate, fade, t, previous != Scrub::Dragging);
        } else if hushed {
            // the transport stopped or paused: what plays fades out; live play primes again at
            // Play, and the recording's play head holds there and fades back in
            self.primed = false;
            if self.rendered {
                self.hold_recording(out, fade);
            } else {
                self.fade_out(out, fade);
            }
        } else if self.rendered {
            self.recorded(ring, out, rate, fade);
        } else {
            self.live(ring, out, fade);
        }
        self.out_at += out.len() as u64;
        self.publish(ring, rate);
    }

    fn stop(&mut self, ring: &SampleRing) {
        self.realtime = false;
        self.scrub = Scrub::Off;
        (self.rendered, self.primed, self.last, self.fade_left, self.waiting) = (false, false, 0.0, 0, false);
        (self.window, self.fast, self.speed, self.heard, self.blend) = (0, 0, 0.0, 0.0, 0);
        (self.frames, self.low, self.quiet) = (0, usize::MAX, 0);
        (self.win.heard, self.win.pos) = (None, 0.0);
        ring.set_heard(None);
        self.seg_n = 0;
    }

    /// The next sample out starts a crossfade from the last one.
    /// The crossfade covers whatever the play head moved to: no residual or seam goes on.
    fn jump(&mut self, fade: usize) {
        (self.from, self.blend) = (self.heard, fade);
        (self.resid, self.win.seam, self.win.swapped) = (0.0, None, 0.0);
    }

    /// The residual's value at the next sample (`fade`: its length in samples).
    fn resid_now(&self, fade: usize) -> f64 {
        if self.resid_n < fade {
            self.resid * (1.0 - self.resid_n as f64 / fade as f64)
        } else {
            0.0
        }
    }

    /// A sample out, through the crossfade while one runs.
    fn emit(&mut self, s: f32, fade: usize) -> f32 {
        let s = if self.blend > 0 {
            let w = self.blend as f32 / fade as f32;
            self.blend -= 1;
            self.from * w + s * (1.0 - w)
        } else {
            s
        };
        self.heard = s;
        s
    }

    /// A source's sample out: a fade to silence starts from it if the source stops.
    fn play(&mut self, s: f32, fade: usize) -> f32 {
        let s = self.emit(s, fade);
        (self.last, self.fade_left) = (s, fade);
        s
    }

    /// Reads the run's speed (the tap's measure of its stepping) every [`SPEED_SECONDS`]: a run
    /// heard as its recording that goes faster than real time is heard live again.
    fn measure(&mut self, ring: &SampleRing, frames: usize, rate: f64) {
        // a run that started over has no reading yet: the last run's goes at once
        if ring.tap_speed() == 0.0 {
            self.speed = 0.0;
        }
        self.window += frames;
        if (self.window as f64) < SPEED_SECONDS * rate {
            return;
        }
        self.window = 0;
        self.speed = ring.tap_speed();
        let can_resume = match self.scrub {
            Scrub::Off => true,
            Scrub::Dragging => false,
            // A paused head or a loop at rest still has the worker's old speed reading.
            Scrub::Released => !ring.hushed() && ring.running(),
        };
        self.fast = if self.rendered && can_resume && self.speed > 1.1 { self.fast + 1 } else { 0 };
        if self.fast >= LIVE_AGAIN {
            self.resume_live(ring);
        }
        if !self.realtime || self.speed >= KEEPS_UP {
            self.size_lead(ring);
        } else {
            (self.low, self.quiet) = (usize::MAX, 0);
        }
    }

    fn resume_live(&mut self, ring: &SampleRing) {
        self.scrub = Scrub::Off;
        (self.rendered, self.primed, self.fast, self.waiting) = (false, false, 0, false);
        (self.win.heard, self.win.pos) = (None, 0.0);
        ring.set_heard(None);
        self.seg_n = 0;
        (self.low, self.quiet) = (usize::MAX, 0);
    }

    /// After quiet measures of live play, the lead comes down to what the worker's stalls needed:
    /// twice the deepest the ring fell below it, plus a device buffer; at least two buffers.
    fn size_lead(&mut self, ring: &SampleRing) {
        let ahead = ring.ahead();
        if self.rendered || ahead == 0 || self.low == usize::MAX {
            return;
        }
        self.quiet += 1;
        if self.quiet < QUIET_MEASURES {
            return;
        }
        let need = 2 * ahead.saturating_sub(self.low) + self.frames;
        let sized = need.max(2 * self.frames).max(3 * ahead / 4);
        if sized < ahead {
            ring.set_ahead(sized);
        }
        (self.low, self.quiet) = (usize::MAX, 0);
    }

    fn live(&mut self, ring: &SampleRing, out: &mut [f32], fade: usize) {
        let was = self.primed;
        let ahead = ring.ahead();
        let fill = ring.fill();
        self.queued = fill;
        let running = ring.running();
        self.primed &= running;
        self.primed |= if self.realtime {
            // A run's start (no speed reading yet) waits for two device buffers: its first rounds
            // can step a little under real time, and one buffer would run dry, a click. A stopped
            // producer can have a final partial buffer waiting.
            let buffers = if ring.tap_speed() == 0.0 { 2 } else { 1 };
            fill > 0 && (!running || fill >= (buffers * out.len()).min(ahead))
        } else {
            // the lead, as the worker's stalls measured it: a paced run keeps what play primed, so
            // what is heard waits behind no more than the stalls need
            fill > 0 && fill >= ahead
        };
        if self.primed && !was {
            self.jump(fade);
        }
        if self.primed && was {
            self.low = self.low.min(fill);
        }
        let got = if self.primed { ring.pop(out) } else { 0 };
        for s in &mut out[..got] {
            *s = self.play(*s, fade);
        }
        if got < out.len() {
            if !self.realtime && !ring.running() && ring.ended() && self.win.whole() {
                // the run rests at its take's end and the take is whole: it plays round from
                // its start, in the recording
                (self.rendered, self.fast, self.meters_quiet) = (true, 0, false);
            } else if self.primed && ring.running() {
                ring.note_underrun();
                self.primed = false;
                if self.speed >= KEEPS_UP && ahead > 0 {
                    // the run keeps up and the worker stalled: a short fade, a longer lead
                    ring.set_ahead(2 * ahead.max(self.frames));
                    (self.low, self.quiet) = (usize::MAX, 0);
                } else if !self.realtime {
                    // the run fell behind: its recording plays instead of crackle
                    (self.rendered, self.fast, self.meters_quiet) = (true, 0, false);
                }
            }
            self.fade_out(&mut out[got..], fade);
        }
    }

    /// Whether table `i`'s window can play from its start to its end without the play head
    /// reaching the render: whole, or what is left renders in time at the measured speed.
    fn playable(&self, i: usize) -> bool {
        let (ready, window) = (self.win.ready_frames(i), self.win.window(i));
        ready >= window || ready >= self.start_frames(window, self.win.tabs[i].rate)
    }

    /// The newest window not heard, newer than the heard one, that can play to its end.
    fn next_playable(&self) -> Option<usize> {
        let heard = self.win.heard.and_then(|h| self.win.tabs[h].key);
        (0..2)
            .filter(|&i| Some(i) != self.win.heard && self.win.tabs[i].key.is_some() && self.win.tabs[i].key > heard)
            .filter(|&i| self.playable(i))
            .max_by_key(|&i| self.win.tabs[i].key)
    }

    /// The frames a window of `window` frames of a take at `rate` Hz needs rendered before it
    /// starts (the whole window while the speed is not known).
    fn start_frames(&self, window: usize, rate: f64) -> usize {
        if self.speed <= 0.0 {
            return window;
        }
        let left = (window as f32 * (1.0 - START_MARGIN * self.speed)).max(0.0) as usize;
        (left + (2.0 * self.buffer_s * rate) as usize).min(window)
    }

    /// Follows the hand through the recorded frames over one device buffer.
    fn scratch(&mut self, ring: &SampleRing, out: &mut [f32], rate: f64, fade: usize, t: f64, entering: bool) {
        if entering {
            self.rendered = true;
            self.fast = 0;
            ring.rendered.store(true, Ordering::Relaxed);
        }
        // Live samples queued before the switch are stale when the hand releases.
        ring.read.store(ring.write.load(Ordering::Acquire), Ordering::Release);
        let Some((h, target)) = self.win.scrub_position(t) else {
            self.hold_recording(out, fade);
            return;
        };
        let hz = self.win.tabs[h].rate;
        if entering || self.win.heard != Some(h) {
            // At a window change, carry the old head into the part the new window covers.
            let previous = self.win.heard.and_then(|i| self.win.time_at(i, self.win.pos));
            self.win.pos = if entering { target } else {
                previous.map_or(target, |old| (target + (old - t) * hz).clamp(0.0, self.win.window(h) as f64))
            };
            self.win.heard = Some(h);
            ring.set_heard(self.win.tabs[h].key);
            self.jump(fade);
        }
        let normal = hz / rate;
        let distance = target - self.win.pos;
        if distance.abs() < 0.1 * normal || out.is_empty() {
            self.hold_recording(out, fade);
            return;
        }
        let step = (distance / out.len() as f64).clamp(-4.0 * normal, 4.0 * normal);
        let start = self.win.pos;
        if let Some(time) = self.win.time_at(h, start) {
            self.mark(self.out_at, time, step / hz);
        }
        if self.win.swapped != 0.0 {
            self.jump(fade);
        }
        let ms_a = (1.0 - (-1.0 / (METER_SECONDS * rate)).exp()) as f32;
        for (i, sample) in out.iter_mut().enumerate() {
            let next = start + (i + 1) as f64 * step;
            match self.win.mix_at(h, self.win.pos, step.abs(), 0.0, ms_a, &mut self.metered) {
                Some((s, sum)) => *sample = self.play_recorded(s, sum, fade),
                None => {
                    self.waiting = true;
                    self.fade_out(std::slice::from_mut(sample), fade);
                    self.quiet_meters();
                }
            }
            self.win.pos = next;
        }
        self.mark_held(self.out_at + out.len() as u64);
    }

    /// The head goes to the run's time `t` in the recording (crossfaded), and goes on from there
    /// as after a scrub's release; nothing changes when no window covers `t`.
    fn seek_to(&mut self, ring: &SampleRing, t: f64, fade: usize) {
        let Some((h, pos)) = self.win.scrub_position(t) else { return };
        self.rendered = true;
        self.fast = 0;
        ring.rendered.store(true, Ordering::Relaxed);
        // live samples queued before the switch are stale
        ring.read.store(ring.write.load(Ordering::Acquire), Ordering::Release);
        (self.win.heard, self.win.pos) = (Some(h), pos);
        ring.set_heard(self.win.tabs[h].key);
        self.jump(fade);
        self.scrub = Scrub::Released;
    }

    fn hold_recording(&mut self, out: &mut [f32], fade: usize) {
        self.waiting = true;
        self.mark_held(self.out_at);
        self.fade_out(out, fade);
        self.quiet_meters();
    }

    fn play_recorded(&mut self, s: f32, sum: f64, fade: usize) -> f32 {
        (self.played, self.resid_n) = (sum, self.resid_n.saturating_add(1));
        self.meters_quiet = false;
        if self.waiting {
            self.waiting = false;
            self.jump(fade);
        }
        self.play(s, fade)
    }

    /// Plays the recording at the device's `rate`: the heard window round, moving to a newer one
    /// at the seam when it can play to its end; silent until one can, and where the play head
    /// reaches the render (unless a newer take's window can play: a take the run started over from
    /// renders no further). The play head moves through a take's frames at its rate over the
    /// device's, each sample out the take at its instant through the read's rate change
    /// ([`rate_change`]).
    fn recorded(&mut self, ring: &SampleRing, out: &mut [f32], rate: f64, fade: usize) {
        // the run feeds the ring only while live: what it fed before is stale
        while ring.pop(out) > 0 {}
        let ms_a = (1.0 - (-1.0 / (METER_SECONDS * rate)).exp()) as f32;
        let mut i = 0;
        // the stretch the next sample starts is marked (the first, and the first after a move)
        let mut fresh = true;
        while i < out.len() {
            let heard = match self.win.heard {
                Some(h) if self.win.pos < self.win.window(h) as f64 => h,
                // the seam, or nothing heard yet: the newer window if it can play to its end
                _ => match self.next_playable().or(self.win.heard) {
                    Some(h) => {
                        // a play head past the seam by part of a frame starts the next that far in
                        let tabs = &self.win.tabs;
                        let carry = self.win.heard.map_or(0.0, |o| (self.win.pos - self.win.window(o) as f64).max(0.0) * tabs[h].rate / tabs[o].rate);
                        (self.win.heard, self.win.pos, self.waiting) = (Some(h), carry, false);
                        ring.set_heard(self.win.tabs[h].key);
                        self.jump(fade);
                        fresh = true;
                        h
                    }
                    None => {
                        self.mark_held(self.out_at + i as u64);
                        self.fade_out(&mut out[i..], fade);
                        self.quiet_meters();
                        return;
                    }
                },
            };
            let step = self.win.tabs[heard].rate / rate;
            // a render's front reached, or a chunk swapped under the head: what played goes on,
            // its difference from the new decaying
            if let Some(d) = self.win.cross(heard, self.win.pos, step, self.played) {
                (self.resid, self.resid_n) = (d, 0);
            }
            if self.win.swapped != 0.0 {
                (self.resid, self.resid_n, self.win.swapped) = (self.resid_now(fade) + self.win.swapped, 0, 0.0);
            }
            let (resid, metered) = (self.resid_now(fade), &mut self.metered);
            match self.win.mix_at(heard, self.win.pos, step, resid, ms_a, metered) {
                Some((s, sum)) => {
                    if fresh {
                        fresh = false;
                        if let Some(t) = self.win.time_at(heard, self.win.pos) {
                            self.mark(self.out_at + i as u64, t, 1.0 / rate);
                        }
                    }
                    out[i] = self.play_recorded(s, sum, fade);
                    self.win.pos += step;
                    i += 1;
                }
                None => {
                    // the play head reached the render. A newer take's window that can play (the
                    // run started over, so this take renders no further) takes over from its start
                    if let Some(n) = self.next_playable().filter(|&n| self.win.newer_take(heard, n)) {
                        (self.win.heard, self.win.pos, self.waiting) = (Some(n), 0.0, false);
                        ring.set_heard(self.win.tabs[n].key);
                        self.jump(fade);
                        fresh = true;
                        continue;
                    }
                    // else the render slowed: wait there, silent
                    self.mark_held(self.out_at + i as u64);
                    self.waiting = true;
                    self.fade_out(&mut out[i..], fade);
                    self.quiet_meters();
                    return;
                }
            }
        }
    }

    /// The meters read silence once, until the recording plays again (the live tap stopped
    /// writing them at the switch).
    fn quiet_meters(&mut self) {
        if !self.meters_quiet {
            self.meters_quiet = true;
            self.win.quiet_meters();
        }
    }

    fn fade_out(&mut self, out: &mut [f32], fade: usize) {
        for s in out {
            let v = if self.fade_left > 0 {
                self.fade_left -= 1;
                self.last * self.fade_left as f32 / fade as f32
            } else {
                0.0
            };
            *s = self.emit(v, fade);
        }
    }

    /// The play state in seconds: a window's frames at its take's rate, the lead at the device's.
    fn publish(&self, ring: &SampleRing, rate: f64) {
        let secs = |n: usize, hz: f64| (n as f64 / hz.max(1.0)) as f32;
        let tab = |i: usize| (self.win.window(i), self.win.tabs[i].rate);
        let heard = self.win.heard.filter(|_| self.rendered);
        // the window rendering after the heard one (none yet while the heard one finishes), or
        // the newest while none is heard
        let newer = if self.realtime { None } else { self.win.newer() };
        let ready = newer.map_or(0.0, |r| secs(self.win.ready_frames(r).min(self.win.window(r)), self.win.tabs[r].rate));
        // the window that one renders in (or the heard one; with neither, a window as asked)
        let shown = if self.realtime { (0, rate) } else {
            newer.or(heard).map(tab).or_else(|| self.win.shape.as_ref().map(|s| (s.slots(rate) * CHUNK, rate))).unwrap_or((0, rate))
        };
        ring.rendered.store(self.rendered, Ordering::Relaxed);
        let new_take = heard.zip(newer).is_some_and(|(h, n)| self.win.newer_take(h, n));
        ring.new_take.store(new_take, Ordering::Relaxed);
        ring.speed.store(self.speed.to_bits(), Ordering::Relaxed);
        ring.loop_s.store(heard.map_or(0.0, |h| secs(self.win.window(h), self.win.tabs[h].rate)).to_bits(), Ordering::Relaxed);
        ring.ready_s.store(ready.to_bits(), Ordering::Relaxed);
        ring.window_s.store(secs(shown.0, shown.1).to_bits(), Ordering::Relaxed);
        ring.start_s.store(secs(self.start_frames(shown.0, shown.1), shown.1).to_bits(), Ordering::Relaxed);
        let read = ring.read_frames.load(Ordering::Relaxed) as usize;
        // what the ring held, not the lead it may hold: a paced run keeps what its player primed,
        // however far a stall doubled the lead
        ring.latency_s.store(secs(self.queued + self.frames + read, rate).to_bits(), Ordering::Relaxed);
        ring.read_s.store(secs(read, rate).to_bits(), Ordering::Relaxed);
        ring.buffer_s.store(secs(self.frames, rate).to_bits(), Ordering::Relaxed);
        // what the device plays now: the frame a buffer behind the newest handed out
        let rec = heard.and_then(|_| self.heard_record(rate));
        let at = rec.map(|r| f64::from_bits(r[HEARD_T0]));
        ring.heard_at.store(at.unwrap_or(f64::NAN).to_bits(), Ordering::Release);
        let mut none = [0u64; HEARD_FIELDS];
        none[HEARD_TAKE] = NONE;
        ring.write_heard(rec.unwrap_or(none));
    }

    /// The heard record as of now: the stretch the frame a device buffer behind the newest plays,
    /// and the next one after it. O(`HEARD_SEGS`).
    fn heard_record(&self, rate: f64) -> Option<[u64; HEARD_FIELDS]> {
        let rate = rate.max(1.0);
        let behind = self.frames as u64;
        let at = self.out_at.saturating_sub(behind);
        let kept = self.seg_n.min(HEARD_SEGS);
        let seg = |k: usize| self.segs[(self.seg_n - 1 - k) % HEARD_SEGS];
        // the newest stretch from at or before `at`, and the one after it (both newest first)
        let (mut cur, mut next) = (None, None);
        for k in 0..kept {
            let s = seg(k);
            if s.from <= at {
                cur = Some(s);
                break;
            }
            next = Some(s);
        }
        let (t0, r0, key) = match (cur, next) {
            (Some(c), _) => (c.t + (at - c.from) as f64 * c.dt, c.dt * rate, c.key),
            // the sound has not reached the first stretch yet: held at its start until it does
            (None, Some(n)) => (n.t, 0.0, n.key),
            (None, None) => return None,
        };
        let (switch, t1, r1) = match next {
            Some(n) => ((n.from - at) as f64 / rate, n.t, n.dt * rate),
            None => (f64::INFINITY, t0, r0),
        };
        let mut rec = [0u64; HEARD_FIELDS];
        rec[HEARD_TAKE] = key;
        rec[HEARD_WALL] = clock_now().to_bits();
        rec[HEARD_T0] = t0.to_bits();
        rec[HEARD_RATE0] = r0.to_bits();
        rec[HEARD_SWITCH] = switch.to_bits();
        rec[HEARD_T1] = t1.to_bits();
        rec[HEARD_RATE1] = r1.to_bits();
        // two buffers on from the stamp, the next has come or the device stopped asking
        rec[HEARD_UNTIL] = ((2 * behind.max(1)) as f64 / rate).to_bits();
        Some(rec).filter(|_| key != NONE)
    }
}
