//! The realtime half: one engine, owned by the device callback, mixes every
//! live instance's schedule into the device buffer.
//!
//! Inside [`AudioEngine::render`] nothing allocates, locks, waits, hashes,
//! parses or frees. A schedule that is replaced or removed travels back to
//! the host through the retire ring and is dropped there. When that ring is
//! full the engine keeps the old schedule and tries again on the next
//! buffer; it never drops one here. Every frame of every device buffer is
//! rendered, in passes of at most [`CHUNK`] frames.

use relax_attr::relax;
use crate::deck::{self, DeckControls, DeckSource, DeckState};
use crate::paged::{LoopRequest, PageOp, PageOrigin, PageOutcome, PagePacket, PageRefusal, PageReturn, PendingSeek, PAGE_OPS_PER_BUFFER};
use crate::schedule::{pending_returns, DeckOp, NodeState, Op, Schedule, SPAN};
pub use makepad_audio_engine::rest::SILENCE as REST_SILENCE;
use makepad_audio_engine::run::{rests, run_awake, RunExt};
use makepad_audio_engine::schedule::ControlBank;
use crate::CHUNK;
use makepad_flow::live::{AudioProbe, AUDIO_PROBE_BINS};
use makepad_lock_free::{Consumer, Producer, Writer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Frames of the dip around a swap that cannot carry its sound over.
const DIP_FRAMES: u32 = 256;
/// Commands and note packets one device buffer takes at most.
const MAX_COMMANDS: usize = 8;
const MAX_PACKETS: usize = 64;
pub(crate) const AUDIO_PROBE_QUEUE: usize = 64;
const AUDIO_PROBES_PER_ROUND: usize = 16;

#[derive(Clone, Copy)]
pub(crate) struct AudioProbePacket {
    pub slot: u16,
    pub incarnation: u64,
    pub generation: u64,
    pub port: u16,
    pub value: AudioProbe,
}

pub enum Command {
    Install { slot: u16, schedule: Box<Schedule> },
    Remove { slot: u16, incarnation: u64 },
    /// A deck's next clip (`None` ejects), for the target of `continuity`.
    Track { slot: u16, incarnation: u64, target: u32, continuity: u64, sequence: u64, track: Option<DeckSource> },
    /// Exchange what two decks play, whole: source, place, motion and their
    /// transport controls, at the point both dips reach silence. Answered in
    /// the slot's telemetry (`SlotTelemetry::exchange`) by `serial`.
    Exchange { slot: u16, incarnation: u64, a: (u32, u64), b: (u32, u64), serial: u64 },
    /// Withdraw exchange `serial` if it has not been applied yet: both decks
    /// are freed and it is answered not applied. An exchange already
    /// answered keeps its answer.
    CancelExchange { slot: u16, incarnation: u64, serial: u64 },
    /// More slots; see [`Grow`]. Answered by a `Retired` carrying the old.
    Grow(Box<Grow>),
}

/// What the engine hands back. The host drops the schedule and the track.
pub struct Retired {
    pub slot: u16,
    pub incarnation: u64,
    pub rejected: Option<Rejection>,
    pub schedule: Option<Box<Schedule>>,
    /// The slot is empty now: the answer to a `Remove`.
    pub removed: bool,
    /// The answer to a `Track` command.
    pub track: Option<TrackReturn>,
    /// The answer to a `Grow`: the slots and writer replaced, to drop here.
    pub grown: Option<Box<Grow>>,
}

/// The acknowledgement of one `Track` command. Accepted (`Retired.rejected`
/// is `None`): `payload` is the clip the deck played before, with its own
/// `old_sequence`. Rejected: `payload` is the command's own clip, untouched.
pub struct TrackReturn {
    pub target: u32,
    pub continuity: u64,
    /// The command's sequence.
    pub sequence: u64,
    /// The sequence of a returned old clip (`0` for none).
    pub old_sequence: u64,
    pub payload: Option<DeckSource>,
}

#[derive(Clone, Copy, Debug)]
pub enum Rejection {
    StaleBase, StaleFormat, StaleSession,
    /// No deck of that target and continuity runs in the slot.
    StaleTarget,
    /// A newer track command for the same deck arrived before the dip ended.
    Superseded,
}

/// What travels on the packet ring, in order: cues for decks.
pub enum Packet {
    /// A contiguous-clip cue pinned to the clip `sequence` accepted when it
    /// was sent; checked when taken and again at the quiet point.
    Seek { slot: u16, incarnation: u64, target: u32, continuity: u64, sequence: u64, seconds: f64 },
}

/// One Transport's clock at the end of the last pass a callback actually
/// rendered: the callback-owned beat, tempo and play state, never a bank
/// target.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClockTelemetry {
    pub beats: f64,
    pub bpm: f64,
    pub playing: bool,
    pub located: u32,
}

/// What a deck is doing, as the callback saw it after rendering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeckStatus {
    #[default]
    Empty,
    /// A contiguous clip eligible to render: its dip is open and the
    /// position is inside the clip. Says nothing about the PCM's level.
    ClipActive,
    /// A contiguous clip dipped out (paused, swapping, cueing) or at its end.
    ClipInactive,
    /// A paged source; see the observation's `readiness` for what the
    /// callback last found at its play head.
    Paged,
}

/// One deck's coherent state at the end of the last rendered buffer. A
/// numeric observation, independent of any timestamp; it acknowledges no
/// operation (sources are acknowledged through the source resource state).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeckObservation {
    /// The loaded source's command sequence, 0 for none.
    pub sequence: u64,
    pub paged: Option<crate::paged::PagedSourceKey>,
    pub frontier: Option<crate::paged::PagedFrontier>,
    /// Paged: the follow mode; a clip is always `History`.
    pub follow: Option<makepad_stage_asset_stream::FollowMode>,
    /// Accepted paged seek serial (0 for clips).
    pub seek_serial: u64,
    /// Source frames.
    pub position: f64,
    pub source_rate: u32,
    /// A load or eject waiting for the quiet point, 0 for none.
    pub pending_sequence: u64,
    pub status: DeckStatus,
    /// Accepted clip cues, or accepted paged seeks.
    pub seek_epoch: u32,
    /// Paged: accepted cues and tails, installed pages, gap and fault notes,
    /// and whether a credited seek waits for the quiet point.
    pub cue_epoch: u32,
    pub tail_epoch: u32,
    pub resident: u8,
    pub gaps: u8,
    pub faults: u8,
    pub pending_seek: bool,
    /// Paged: the callback's finding at the play head after this buffer,
    /// contiguous current PCM from there (frames), and whether it was
    /// playing it. Separate from the source acknowledgement.
    pub readiness: Option<crate::paged::PagedReadiness>,
    pub ready_frames: u32,
    pub render_eligible: bool,
    /// Whether the source clock was advancing at the end of this buffer; see
    /// `PagedSource::advancing` (PCM or a verified durable gap alike) and
    /// `ClipTelemetry::advancing`. Not derived from the play control or gain.
    /// `None` with no source, and for a clip until a chunk of it was
    /// rendered since its load or its last accepted cue.
    pub advancing: Option<bool>,
    /// The nominal source frames per output frame this buffer stepped with
    /// (`speed × source_rate / device_rate`), positive under the transport;
    /// signed (reverse included) while a hand scratches or releases the
    /// record. Kept while paused, held or ended; motion is this ratio only
    /// when `advancing` is true. Same `None` rule as `advancing`.
    pub source_frames_per_output_frame: Option<f64>,
    /// The accepted loop apart from the request, with its epochs. For a
    /// clip: the `[in, out)` its sampler used (`None` when off), `loop_epoch`
    /// changing with it; never expired or rejected (`reject_epoch` 0). Same
    /// `None` rule as `advancing`.
    pub looping: Option<crate::paged::PagedLoop>,
    pub mode_epoch: u32,
    pub notes_revision: u64,
    /// Playback loop crossings and ends reached (paged: terminal ends; clip:
    /// the clip end), each counted once.
    pub boundary_epoch: u32,
    /// Paged: current-serial pages being sampled (of `resident`).
    pub current: u8,
    /// Paged: the accepted stem policy, the current serial's contiguous
    /// installed stem run at the play head (else the latest), and whether
    /// the last chunk played stems. Apart from the raw `frontier`.
    pub stem_policy: Option<crate::paged::StemPolicy>,
    pub stem_ready: Option<makepad_stage_asset_stream::SampleRange>,
    pub stems_available: bool,
    pub stem_slots: u8,
    /// Clip only: the loaded clip's exact PCM length in source frames
    /// (`None` for paged sources, which report `frontier`).
    pub source_frames: Option<u64>,
}

impl DeckObservation {
    pub const EMPTY: Self = Self {
        sequence: 0, paged: None, frontier: None, follow: None, seek_serial: 0, position: 0.0, source_rate: 0,
        pending_sequence: 0, status: DeckStatus::Empty, seek_epoch: 0,
        cue_epoch: 0, tail_epoch: 0, resident: 0, gaps: 0, faults: 0, pending_seek: false,
        readiness: None, ready_frames: 0, render_eligible: false, advancing: None, source_frames_per_output_frame: None, looping: None, mode_epoch: 0, notes_revision: 0, boundary_epoch: 0, current: 0,
        stem_policy: None, stem_ready: None, stems_available: false, stem_slots: 0, source_frames: None,
    };

    fn of(deck: &crate::deck::DeckState) -> Self {
        let mut out = Self { sequence: deck.sequence, position: deck.position, seek_epoch: deck.seek_epoch,
            pending_sequence: deck.next.as_ref().map_or(0, |(sequence, _)| *sequence), ..Self::EMPTY };
        match &deck.track {
            None => {}
            Some(DeckSource::Clip(track)) => {
                out.source_rate = track.pcm().sample_rate;
                // Eligible to render: not dipped out and inside the clip.
                // Not a claim that the PCM there is audible.
                let inside = deck.position < track.pcm().frames.len() as f64;
                out.status = if deck.gain > 0.0 && inside { DeckStatus::ClipActive } else { DeckStatus::ClipInactive };
                out.source_frames = Some(track.pcm().frames.len() as u64);
                // A clip is a fixed history played forward.
                out.follow = Some(makepad_stage_asset_stream::FollowMode::History);
                let clip = deck.clip;
                out.boundary_epoch = clip.boundary_epoch;
                if clip.rendered {
                    out.advancing = Some(clip.advancing);
                    out.source_frames_per_output_frame = Some(clip.source_frames_per_output_frame);
                    // The paged loop shape, holding only what a clip has:
                    // no rejection, never expired, no reject epoch.
                    out.looping = Some(crate::paged::PagedLoop {
                        effective: clip.effective_loop,
                        expired: false,
                        rejected: None,
                        loop_epoch: clip.loop_epoch,
                        reject_epoch: 0,
                    });
                }
            }
            Some(DeckSource::Feed(feed)) => {
                out.source_rate = crate::feed::FEED_RATE;
                out.status = if deck.gain > 0.0 { DeckStatus::ClipActive } else { DeckStatus::ClipInactive };
                let _ = feed;
            }
            Some(DeckSource::Paged(source)) => {
                let counts = source.counts();
                out.paged = Some(source.key());
                out.frontier = Some(source.frontier());
                out.seek_serial = source.seek_serial();
                out.follow = Some(source.mode());
                out.source_rate = source.sample_rate();
                out.status = DeckStatus::Paged;
                let (readiness, ready_frames, eligible) = source.readiness();
                out.readiness = Some(readiness);
                out.ready_frames = ready_frames;
                out.render_eligible = eligible;
                out.advancing = Some(source.advancing());
                out.source_frames_per_output_frame = Some(source.source_frames_per_output_frame());
                out.looping = Some(source.looping());
                out.mode_epoch = counts.mode_epoch;
                out.notes_revision = counts.notes_revision;
                out.boundary_epoch = counts.boundary_epoch;
                out.current = counts.current;
                out.seek_epoch = counts.seek_epoch;
                out.cue_epoch = counts.cue_epoch;
                out.tail_epoch = counts.tail_epoch;
                out.resident = counts.resident;
                out.gaps = counts.gaps;
                out.faults = counts.faults;
                out.pending_seek = counts.pending_seek;
                out.stem_policy = Some(source.stem_policy());
                out.stem_ready = source.stem_ready();
                out.stems_available = source.stems_available();
                out.stem_slots = source.stem_slots();
            }
        }
        out
    }
}

/// A slot's facts each callback. What its schedule publishes (probes,
/// clocks, decks) is on that schedule's own board ([`crate::board`]).
#[derive(Clone, Copy)]
pub struct SlotTelemetry {
    pub incarnation: u64,
    /// `0` for an empty slot.
    pub generation: u64,
    /// The schedule was prepared for another device rate and is silent
    /// until the host prepares it again.
    pub rate_mismatch: bool,
    /// The newest deck exchange answered in this slot: `(serial, applied)`.
    /// `(0, false)` before any.
    pub exchange: (u64, bool),
    /// The schedule is resting: silent with nothing running, its graph is
    /// not run (`makepad_audio_engine::rest`).
    pub resting: bool,
}

/// Everything the engine tells the host, published once per device buffer
/// (in place: its slots are as many as the engine has). Newest wins;
/// nothing here can be relied on to arrive.
#[derive(Clone)]
pub struct Telemetry {
    pub callbacks: u64,
    pub frames: u64,
    pub rate: u32,
    pub slots: Box<[SlotTelemetry]>,
    /// Buffers in which a swap waited because the retire ring was full.
    pub retire_waits: u32,
    /// Note packets for a slot or a synth that is not there any more.
    pub stale_packets: u32,
    /// Schedules installed without state transfer because their base was
    /// not the generation that ran.
    pub base_mismatches: u32,
}

impl SlotTelemetry {
    pub const EMPTY: Self = Self { incarnation: 0, generation: 0, rate_mismatch: false, exchange: (0, false), resting: false };
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::with_slots(INITIAL_SLOTS)
    }
}

impl Telemetry {
    pub fn with_slots(slots: usize) -> Self {
        Self { callbacks: 0, frames: 0, rate: 0, slots: vec![SlotTelemetry::EMPTY; slots].into_boxed_slice(), retire_waits: 0, stale_packets: 0, base_mismatches: 0 }
    }

    /// Everything of `other`, into this one's own buffers (as many slots).
    fn copy_from(&mut self, other: &Telemetry) {
        self.callbacks = other.callbacks;
        self.frames = other.frames;
        self.rate = other.rate;
        self.slots.copy_from_slice(&other.slots);
        self.retire_waits = other.retire_waits;
        self.stale_packets = other.stale_packets;
        self.base_mismatches = other.base_mismatches;
    }
}

/// Live instances an engine starts with room for; it grows on demand
/// ([`Command::Grow`]).
pub const INITIAL_SLOTS: usize = 4;

/// More room for live instances, all prepared off the audio thread: the
/// engine moves its slots into `slots` (so many more), starts publishing
/// through `telemetry` (its slots as many), and hands what it had back.
pub struct Grow {
    slots: Box<[Slot]>,
    state: Telemetry,
    telemetry: Writer<Telemetry>,
}

impl Grow {
    /// Room for `slots` instances, published through `telemetry` (made
    /// with `Telemetry::with_slots(slots)`).
    pub fn new(slots: usize, telemetry: Writer<Telemetry>) -> Box<Self> {
        Box::new(Self { slots: (0..slots).map(|_| Slot::default()).collect(), state: Telemetry::with_slots(slots), telemetry })
    }
}

/// A deck exchange admitted in a slot, waiting for both dips: the two deck
/// states and the generation they belong to.
#[derive(Clone, Copy)]
struct PendingExchange {
    serial: u64,
    generation: u64,
    states: (u16, u16),
}

#[derive(Default)]
struct Slot {
    exchange: Option<PendingExchange>,
    incarnation: Option<u64>,
    return_credit: bool,
    active: Option<Box<Schedule>>,
    next: Option<Box<Schedule>>,
    removing: bool,
    /// Output gain of the slot and where it is heading, for the dip.
    gain: f32,
    target: f32,
}

pub struct AudioEngine {
    /// Host-installed timestamp source for rendered clocks; `None` publishes
    /// clocks without a time (`clock_at: None`). See `set_timestamp_sampler`.
    timestamp_sampler: Option<fn() -> f64>,
    commands: Consumer<Command>,
    retired: Producer<Retired>,
    packets: Consumer<Packet>,
    /// Page operations and their receipts: separate from the source/schedule
    /// return ring, with their own reservation count.
    page_commands: Consumer<PagePacket>,
    page_returns: Producer<PageReturn>,
    /// Page receipts promised: one per taken operation not yet answered
    /// (a pending seek keeps its own). `page_returns.free() >= page_reserved`.
    page_reserved: usize,
    /// A receipt whose push failed (unreachable under the reservation); its
    /// credit stays reserved until it is pushed. Nothing is dropped here.
    held_page_return: Option<PageReturn>,
    telemetry: Writer<Telemetry>,
    audio_probes: Producer<AudioProbePacket>,
    probe_after: u64,
    probe_cursor: (usize, usize),
    /// The device rate, for a prepare that has not heard from a callback.
    rate: Arc<AtomicU64>,
    slots: Box<[Slot]>,
    /// A command taken from the ring that cannot complete yet.
    held: Option<Command>,
    state: Telemetry,
    mix: [f32; SPAN],
    slot_mix: [f32; SPAN],
    /// The headphone cue's mix (CueOutput nodes), beside the program's.
    cue_mix: [f32; SPAN],
    slot_cue: [f32; SPAN],
    reserved_returns: usize,
    format: u64,
    /// Whether a silent schedule rests. On unless the host turns it off, to
    /// compare against an engine that always runs its graph.
    resting_enabled: bool,
    /// Passes skipped because their schedule rested, since the engine was
    /// made (a plain field: only the audio callback touches it).
    rested_passes: u64,
}

impl AudioEngine {
    /// Turn resting on or off for this engine (on by default).
    pub fn set_resting(&mut self, on: bool) {
        self.resting_enabled = on;
    }

    /// Passes (one per active slot per buffer) skipped because the schedule
    /// rested, since the engine was made.
    pub fn rested_passes(&self) -> u64 {
        self.rested_passes
    }

    /// Install before moving the engine into the device callback. The
    /// sampler is called at most once per callback that actually rendered a
    /// schedule's frames, to stamp that callback's clocks; it must be
    /// supported on the target, realtime-safe (no blocking, allocation or
    /// lock) and in the `Instant` domain every consumer compares against.
    /// The engine never reads a clock itself: with `None` (the default, and
    /// the only choice where `Instant::now` is unsupported, such as
    /// `wasm32-unknown-unknown`) beats, tempo, play state, generation, frame,
    /// rate and publication are still published, only without a time.
    pub fn set_timestamp_sampler(&mut self, sampler: Option<fn() -> f64>) {
        self.timestamp_sampler = sampler;
    }

    pub(crate) fn new(
        commands: Consumer<Command>,
        retired: Producer<Retired>,
        packets: Consumer<Packet>,
        telemetry: Writer<Telemetry>,
        audio_probes: Producer<AudioProbePacket>,
        rate: Arc<AtomicU64>,
        page_commands: Consumer<PagePacket>,
        page_returns: Producer<PageReturn>,
    ) -> Self {
        Self {
            commands, retired, packets, telemetry, audio_probes, rate, page_commands, page_returns,
            page_reserved: 0,
            held_page_return: None,
            probe_after: 0,
            timestamp_sampler: None,
            probe_cursor: (0, 0),
            slots: (0..INITIAL_SLOTS).map(|_| Slot::default()).collect(),
            held: None,
            state: Telemetry::default(),
            mix: [0.0; SPAN],
            slot_mix: [0.0; SPAN],
            cue_mix: [0.0; SPAN],
            slot_cue: [0.0; SPAN],
            reserved_returns: 0,
            format: 0,
            resting_enabled: true,
            rested_passes: 0,
        }
    }

    /// Fill one device buffer. `left` and `right` have the same length; the
    /// engine writes every frame of both.
    #[relax(O3, always)]
    pub fn render(&mut self, rate: f64, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        self.render_with(rate, frames, |offset, l, r, _, _| {
            left[offset..offset + l.len()].copy_from_slice(l);
            right[offset..offset + r.len()].copy_from_slice(r);
        });
    }

    /// Render one whole device callback, lending each finished stereo chunk
    /// to the host for channel mapping: `(offset, left, right, cue_left,
    /// cue_right)`, the program and the headphone cue. Lifecycle budgets run
    /// once, even for mono/multichannel buffers larger than CHUNK. The sink
    /// must be RT-safe.
    #[relax(O3, always)]
    pub fn render_with(&mut self, rate: f64, frames: usize, mut sink: impl FnMut(usize, &[f32], &[f32], &[f32], &[f32])) {
        let rate = rate.round().clamp(0.0, u32::MAX as f64) as u32;
        self.state.callbacks = self.state.callbacks.wrapping_add(1);
        self.state.frames = self.state.frames.wrapping_add(frames as u64);
        self.state.rate = rate;
        self.update_format(rate);
        self.take_commands();
        self.take_packets();
        self.take_page_commands();
        for slot in &mut self.slots {
            if let Some(schedule) = &mut slot.active { schedule.last_frames = 0; }
        }
        let mut offset = 0;
        while offset < frames {
            let count = (frames - offset).min(CHUNK);
            self.mix[..count].fill(0.0);
            self.mix[CHUNK..CHUNK + count].fill(0.0);
            self.cue_mix[..count].fill(0.0);
            self.cue_mix[CHUNK..CHUNK + count].fill(0.0);
            for index in 0..self.slots.len() { self.pass(index, rate, count); }
            for frame in 0..count {
                for mix in [&mut self.mix, &mut self.cue_mix] {
                    mix[frame] = mix[frame].clamp(-1.0, 1.0);
                    mix[CHUNK + frame] = mix[CHUNK + frame].clamp(-1.0, 1.0);
                }
            }
            sink(offset, &self.mix[..count], &self.mix[CHUNK..CHUNK + count], &self.cue_mix[..count], &self.cue_mix[CHUNK..CHUNK + count]);
            offset += count;
        }
        self.publish_audio_probes();
        self.publish(true);
    }

    /// Preserve lifecycle/format progress when a host cannot expose channels.
    #[relax(O3, always)]
    pub fn render_silent(&mut self, rate: f64, frames: usize) {
        self.render_with(rate, frames, |_, _, _, _, _| {});
    }

    fn update_format(&mut self, rate: u32) {
        if self.format as u32 != rate || self.format == 0 {
            let epoch = (self.format >> 32).wrapping_add(1) as u32;
            self.format = ((epoch as u64) << 32) | rate as u64;
            self.probe_after = 0;
        }
        self.rate.store(self.format, Ordering::Release);
    }

    /// What a host without a device calls instead of `render`: commands are
    /// taken and acknowledged, nothing sounds.
    pub fn idle(&mut self) {
        self.format = self.rate.load(Ordering::Acquire);
        self.take_commands();
        self.take_packets();
        self.take_page_commands();
        for index in 0..self.slots.len() {
            self.settle(index, true);
            self.settle_decks(index, true);
            self.settle_exchange(index);
        }
        // Idle renders nothing: clocks keep their last provenance and time.
        self.publish(false);
    }

    /// `rendered`: this is the end of a device callback. Only a schedule
    /// that actually rendered frames in it (not silent for a rate mismatch,
    /// not swapped in after its passes) publishes clocks, tagged with its
    /// own generation, this callback and one timestamp, sampled once and only
    /// when a sampler is installed and some schedule rendered.
    fn publish(&mut self, rendered: bool) {
        let sampler = self.timestamp_sampler;
        let mut at: Option<Option<f64>> = None;
        let (callbacks, frames, rate) = (self.state.callbacks, self.state.frames, self.state.rate);
        for (slot, telemetry) in self.slots.iter_mut().zip(self.state.slots.iter_mut()) {
            match &mut slot.active {
                Some(schedule) => {
                    telemetry.incarnation = schedule.incarnation;
                    telemetry.generation = schedule.generation;
                    if rendered && schedule.last_frames > 0 && !telemetry.rate_mismatch {
                        let stamp = *at.get_or_insert_with(|| sampler.map(|sample| sample()));
                        let frames_done = schedule.clock_frames;
                        let schedule = &mut **schedule;
                        let (clocks, probes, decks, states) = (&schedule.clocks, &schedule.probes, &schedule.decks, &schedule.states);
                        if let Some(board) = schedule.board.as_mut() {
                            board.update(|values| {
                                values.publication = callbacks;
                                values.frame = frames;
                                values.rate = rate;
                                values.at = stamp;
                                for (out, clock) in values.clocks.iter_mut().zip(clocks.iter()) {
                                    *out = ClockTelemetry { beats: clock.tempo.end_beat(frames_done), bpm: clock.bpm as f64, playing: clock.playing, located: clock.located };
                                }
                                values.probes.copy_from_slice(probes);
                                for (out, state) in values.decks.iter_mut().zip(decks.iter()) {
                                    if let Some(NodeState::Ext(deck)) = states.get(*state as usize) {
                                        *out = DeckObservation::of(deck);
                                    }
                                }
                            });
                        }
                    }
                }
                None => *telemetry = SlotTelemetry::EMPTY,
            }
        }
        let state = &self.state;
        self.telemetry.update(|published| published.copy_from(state));
    }

    /// Visual packets are deliberately lossy and separate from callback-wide
    /// scalar telemetry. At most sixteen real last-chunk observations per round.
    fn publish_audio_probes(&mut self) {
        if self.state.frames < self.probe_after || self.state.rate == 0 { return; }
        self.probe_after = self.state.frames.saturating_add((self.state.rate as u64 / 30).max(1));
        let mut sent = 0;
        // Round robin over every slot's spans, as many as each schedule has.
        let ports = |slot: &Slot| slot.active.as_ref().map_or(1, |schedule| (schedule.arena.len() / SPAN).max(1));
        let rounds: usize = self.slots.iter().map(ports).sum();
        for _ in 0..rounds {
            let (slot_index, port) = self.probe_cursor;
            self.probe_cursor = if port + 1 < ports(&self.slots[slot_index]) { (slot_index, port + 1) } else { ((slot_index + 1) % self.slots.len(), 0) };
            let Some(schedule) = self.slots[slot_index].active.as_ref() else { continue };
            let frames = schedule.last_frames as usize;
            if schedule.rate != self.state.rate || frames == 0 || port * SPAN >= schedule.arena.len() { continue; }
            let span = &schedule.arena[port * SPAN..][..SPAN];
            let mut value = AudioProbe {
                sequence: self.state.frames, sample_rate: self.state.rate,
                frames: frames as u16, bins: [[0.0; 4]; AUDIO_PROBE_BINS], peak: 0.0,
            };
            for (bin, range) in value.bins.iter_mut().enumerate() {
                let start = bin * frames / AUDIO_PROBE_BINS;
                let end = ((bin + 1) * frames / AUDIO_PROBE_BINS).max(start + 1).min(frames);
                let mut bounds = [span[start], span[start], span[CHUNK + start], span[CHUNK + start]];
                for at in start..end {
                    let (left, right) = (span[at], span[CHUNK + at]);
                    bounds[0] = bounds[0].min(left);
                    bounds[1] = bounds[1].max(left);
                    bounds[2] = bounds[2].min(right);
                    bounds[3] = bounds[3].max(right);
                    value.peak = value.peak.max(left.abs()).max(right.abs());
                }
                *range = bounds;
            }
            let packet = AudioProbePacket { slot: slot_index as u16, incarnation: schedule.incarnation, generation: schedule.generation, port: port as u16, value };
            // Failure drops only a fixed-size Copy visual packet.
            if self.audio_probes.push(packet).is_err() { break; }
            sent += 1;
            if sent == AUDIO_PROBES_PER_ROUND { break; }
        }
    }

    fn take_commands(&mut self) {
        for _ in 0..MAX_COMMANDS {
            let Some(command) = self.held.take().or_else(|| self.commands.pop()) else { return };
            match command {
                Command::Grow(mut grow) => {
                    if self.retired.free() <= self.reserved_returns {
                        self.held = Some(Command::Grow(grow));
                        return;
                    }
                    // The slots move over whole; the new state starts as
                    // the old one, then the old parts go back to the host.
                    for (new, old) in grow.slots.iter_mut().zip(self.slots.iter_mut()) {
                        std::mem::swap(new, old);
                    }
                    std::mem::swap(&mut grow.slots, &mut self.slots);
                    grow.state.slots[..self.state.slots.len()].copy_from_slice(&self.state.slots);
                    std::mem::swap(&mut grow.state.slots, &mut self.state.slots);
                    std::mem::swap(&mut grow.telemetry, &mut self.telemetry);
                    let back = Retired { slot: 0, incarnation: 0, schedule: None, removed: false, rejected: None, track: None, grown: Some(grow) };
                    if let Err(back) = self.retired.push(back) {
                        // Unreachable under the free check above; kept rather than dropped.
                        self.held = None;
                        drop(back);
                    }
                }
                Command::Install { slot, schedule } => {
                    let valid = self.slots.get(slot as usize).is_some_and(|target| {
                        target.incarnation.is_none_or(|incarnation| incarnation == schedule.incarnation)
                    });
                    if !valid {
                        if self.retired.free() <= self.reserved_returns {
                            self.held = Some(Command::Install { slot, schedule });
                            return;
                        }
                        let back = Retired { slot, incarnation: schedule.incarnation, schedule: Some(schedule), removed: false, rejected: Some(Rejection::StaleSession), track: None, grown: None };
                        if let Err(mut back) = self.retired.push(back) {
                            self.held = Some(Command::Install { slot, schedule: back.schedule.take().expect("install payload") });
                            return;
                        }
                        continue;
                    }
                    let target = &mut self.slots[slot as usize];
                    if target.next.is_some() || target.removing {
                        self.held = Some(Command::Install { slot, schedule });
                        return;
                    }
                    target.incarnation = Some(schedule.incarnation);
                    target.next = Some(schedule);
                }
                Command::Remove { slot, incarnation } => {
                    if let Some(target) = self.slots.get_mut(slot as usize) {
                        if target.incarnation.is_none_or(|current| current == incarnation) {
                            target.incarnation = Some(incarnation);
                            target.removing = true;
                        }
                    }
                }
                Command::Exchange { slot, incarnation, a, b, serial } => {
                    let index = slot as usize;
                    let states = self.slots.get(index).filter(|entry| !entry.removing && entry.exchange.is_none()).and_then(|entry| entry.active.as_ref()).and_then(|schedule| {
                        if schedule.incarnation != incarnation { return None; }
                        let find = |(target, continuity): (u32, u64)| {
                            let at = schedule.triggers.binary_search_by_key(&target, |entry| entry.0).ok()?;
                            let (_, state, held) = schedule.triggers[at];
                            (held == continuity && matches!(schedule.states.get(state as usize), Some(NodeState::Ext(_)))).then_some(state)
                        };
                        let (first, second) = (find(a)?, find(b)?);
                        (first != second).then_some((schedule.generation, first, second))
                    });
                    match states {
                        Some((generation, first, second)) => {
                            let entry = &mut self.slots[index];
                            let schedule = entry.active.as_mut().expect("found above");
                            for state in [first, second] {
                                if let NodeState::Ext(deck) = &mut schedule.states[state as usize] {
                                    deck.exchange = Some(serial);
                                }
                            }
                            entry.exchange = Some(PendingExchange { serial, generation, states: (first, second) });
                        }
                        None => {
                            if let Some(telemetry) = self.state.slots.get_mut(index) {
                                telemetry.exchange = (serial, false);
                            }
                        }
                    }
                }
                Command::CancelExchange { slot, incarnation, serial } => {
                    let index = slot as usize;
                    let Some(entry) = self.slots.get_mut(index) else { continue };
                    let pending = entry.exchange.filter(|pending| pending.serial == serial);
                    let matches = entry.active.as_ref().is_some_and(|schedule| schedule.incarnation == incarnation);
                    if let (Some(_), true) = (pending, matches) {
                        if let Some(schedule) = entry.active.as_mut() {
                            for state in schedule.states.iter_mut() {
                                if let NodeState::Ext(deck) = state { deck.exchange = None; }
                            }
                            // The swap ended every gesture on the decks: whatever
                            // its outcome, no hand stays on a record.
                            release_hands(schedule);
                        }
                        entry.exchange = None;
                        self.state.slots[index].exchange = (serial, false);
                    }
                }
                Command::Track { slot, incarnation, target, continuity, sequence, track } => {
                    // The return slot is reserved before the command is
                    // consumed: whatever happens, the clip has a way back.
                    if self.retired.free() <= self.reserved_returns {
                        self.held = Some(Command::Track { slot, incarnation, target, continuity, sequence, track });
                        return;
                    }
                    self.reserved_returns += 1;
                    let deck = self.slots.get_mut(slot as usize).filter(|entry| !entry.removing).and_then(|entry| entry.active.as_mut()).and_then(|schedule| {
                        if schedule.incarnation != incarnation { return None; }
                        let at = schedule.triggers.binary_search_by_key(&target, |entry| entry.0).ok()?;
                        let (_, index, held) = schedule.triggers[at];
                        if held != continuity { return None; }
                        match schedule.states.get_mut(index as usize) {
                            Some(NodeState::Ext(deck)) => Some(deck),
                            _ => None,
                        }
                    });
                    // A swap still waiting is superseded: its clip goes back
                    // on its own reserved slot, this one takes its place.
                    let outcome = match deck {
                        Some(deck) => Ok(deck.next.replace((sequence, track))),
                        None => Err(track),
                    };
                    match outcome {
                        Err(track) => {
                            let back = Retired { slot, incarnation, schedule: None, removed: false, rejected: Some(Rejection::StaleTarget), track: Some(TrackReturn { target, continuity, sequence, old_sequence: 0, payload: track }), grown: None };
                            self.return_track(back);
                        }
                        Ok(Some((old_sequence, payload))) => {
                            let back = Retired { slot, incarnation, schedule: None, removed: false, rejected: Some(Rejection::Superseded), track: Some(TrackReturn { target, continuity, sequence: old_sequence, old_sequence: 0, payload }), grown: None };
                            self.return_track(back);
                        }
                        Ok(None) => {}
                    }
                }
            }
        }
    }

    /// Push an acknowledgement on the slot reserved for it. The reservation
    /// guarantees success; a broken invariant keeps the clip in a held
    /// command rather than dropping it here.
    fn return_track(&mut self, back: Retired) {
        match self.retired.push(back) {
            Ok(()) => self.reserved_returns -= 1,
            Err(mut back) => {
                let track = back.track.take().expect("a track acknowledgement");
                self.reserved_returns -= 1;
                if let Some(payload) = track.payload {
                    self.held = Some(Command::Track { slot: back.slot, incarnation: back.incarnation, target: track.target, continuity: track.continuity, sequence: track.sequence, track: Some(payload) });
                }
            }
        }
    }

    fn take_packets(&mut self) {
        for _ in 0..MAX_PACKETS {
            let Some(packet) = self.packets.pop() else { return };
            let (slot, incarnation, target, continuity) = match &packet {
                Packet::Seek { slot, incarnation, target, continuity, .. } => (*slot, *incarnation, *target, *continuity),
            };
            let schedule = self.slots.get_mut(slot as usize).and_then(|slot| slot.active.as_mut());
            let state = schedule.and_then(|schedule| {
                if schedule.incarnation != incarnation { return None; }
                let at = schedule.triggers.binary_search_by_key(&target, |entry| entry.0).ok()?;
                let (_, index, held) = schedule.triggers[at];
                if held != continuity { return None; }
                schedule.states.get_mut(index as usize)
            });
            match (state, packet) {
                (Some(NodeState::Ext(deck)), Packet::Seek { sequence, seconds, .. })
                    if deck.sequence == sequence && matches!(deck.track, Some(DeckSource::Clip(_))) => deck.seek = Some((sequence, seconds)),
                _ => self.state.stale_packets = self.state.stale_packets.wrapping_add(1),
            }
        }
    }

    /// Apply a pending deck exchange once both decks are silent: their whole
    /// states trade places (two boxes; nothing allocated or dropped) and so do
    /// their transport controls; both hands are let go. A schedule that
    /// changed under it answers it not applied, and frees both decks.
    fn settle_exchange(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        let Some(pending) = slot.exchange else { return };
        // A replacement on its way was prepared from the bank as it was before
        // the exchange: it would put the old transports back under the traded
        // sources. The host never stages one during an exchange; if one is
        // here anyway, the exchange is answered not applied.
        let replacing = slot.next.is_some();
        let Some(schedule) = slot.active.as_mut().filter(|schedule| schedule.generation == pending.generation && !replacing) else {
            // Another schedule runs now: whatever carried the flag over is freed.
            if let Some(schedule) = slot.active.as_mut() {
                for state in schedule.states.iter_mut() {
                    if let NodeState::Ext(deck) = state { deck.exchange = None; }
                }
                release_hands(schedule);
            }
            slot.exchange = None;
            self.state.slots[index].exchange = (pending.serial, false);
            return;
        };
        let (first, second) = (pending.states.0 as usize, pending.states.1 as usize);
        let ready = |state: &NodeState| matches!(state, NodeState::Ext(deck) if deck.quiet() && deck.next.is_none());
        if !ready(&schedule.states[first]) || !ready(&schedule.states[second]) {
            return;
        }
        let (low, high) = (first.min(second), first.max(second));
        let (head, tail) = schedule.states.split_at_mut(high);
        if let (NodeState::Ext(a), NodeState::Ext(b)) = (&mut head[low], &mut tail[0]) {
            std::mem::swap(a, b);
            a.exchange = None;
            b.exchange = None;
        }
        // Each deck's transport goes with its source.
        let controls = |state: usize| schedule.ops.iter().find_map(|op| match op {
            Op::Ext(DeckOp { state: at, play, speed, loop_in, loop_out, loop_enabled, scratch, scratch_at, key, keylock, cue_at, .. }) if *at as usize == state =>
                Some(([*play, *speed, *loop_in, *loop_out, *loop_enabled, *cue_at, *key, *keylock], [*scratch, *scratch_at])),
            _ => None,
        });
        if let (Some((a, hand_a)), Some((b, hand_b))) = (controls(first), controls(second)) {
            for (x, y) in a.into_iter().zip(b) {
                schedule.controls.exchange(x, y);
            }
            for hand in [hand_a, hand_b] {
                schedule.controls.set_manual(hand[0], 0.0);
            }
        }
        slot.exchange = None;
        self.state.slots[index].exchange = (pending.serial, true);
    }

    /// Apply every deck swap or cue whose dip reached silence. A swap
    /// returns the old clip on the slot reserved when its command was taken.
    /// On a pass that renders nothing (`silent`: no device, or a schedule
    /// prepared for another rate) every deck is silent by definition, so
    /// its dip is complete and its swap applies; otherwise pending swaps
    /// could hold their return slots for as long as the mismatch lasts.
    fn settle_decks(&mut self, index: usize, silent: bool) {
        let slot = &mut self.slots[index];
        let Some(schedule) = slot.active.as_mut() else { return };
        let (slot_index, incarnation) = (index as u16, schedule.incarnation);
        let controls = &schedule.controls;
        for op in schedule.ops.iter() {
            let Op::Ext(DeckOp { state, loop_in, loop_out, loop_enabled, .. }) = op else { continue };
            // The loop as the deck's controls hold it now: a page operation is
            // judged against it, not against the last rendered request.
            let request = LoopRequest { in_seconds: controls.get(*loop_in), out_seconds: controls.get(*loop_out), enabled: controls.get(*loop_enabled) >= 0.5 };
            let NodeState::Ext(deck) = &mut schedule.states[*state as usize] else { continue };
            if silent { deck.gain = 0.0; }
            if !deck.quiet() { continue; }
            if deck.next.is_some() {
                // The outgoing paged source's pending seek is answered before
                // the swap; with a receipt parked, neither happens this pass.
                if let Some(DeckSource::Paged(source)) = deck.track.as_mut() {
                    if source.counts().pending_seek {
                        if !page_room(&self.page_returns, &self.held_page_return, 1) { continue; }
                        if let Some(seek) = source.take_pending_seek() {
                            let back = PageReturn { origin: seek.origin, kind: seek.kind, outcome: PageOutcome::Cancelled, page: None, bundle: None };
                            push_page(&mut self.page_returns, &mut self.page_reserved, &mut self.held_page_return, back);
                        }
                        // Answered in the ring, or parked with its credit;
                        // either way no swap happens on a parked receipt.
                        if self.held_page_return.is_some() { continue; }
                    }
                }
                let (position, seek, clip) = (deck.position, deck.seek, deck.clip);
                let (target, continuity) = schedule.triggers.iter().find(|entry| entry.1 == *state).map(|entry| (entry.0, entry.2)).unwrap_or((0, 0));
                let Some((sequence, old_sequence, payload)) = deck.apply_swap() else { continue };
                let back = Retired { slot: slot_index, incarnation, schedule: None, removed: false, rejected: None, track: Some(TrackReturn { target, continuity, sequence, old_sequence, payload }), grown: None };
                match self.retired.push(back) {
                    Ok(()) => self.reserved_returns -= 1,
                    Err(mut back) => {
                        // Not reachable with the reservation; undo the swap
                        // and keep it pending rather than lose a clip.
                        let track = back.track.take().expect("a track acknowledgement");
                        let new = std::mem::replace(&mut deck.track, track.payload);
                        deck.sequence = old_sequence;
                        deck.next = Some((sequence, new));
                        deck.position = position;
                        deck.seek = seek;
                        deck.clip = clip;
                    }
                }
            }
            deck.apply_seek();
            accept_page_seek(deck, request, &mut self.page_returns, &mut self.page_reserved, &mut self.held_page_return);
        }
    }

    /// Take page operations, at most `PAGE_OPS_PER_BUFFER`, each only with
    /// a receipt slot reserved first; otherwise it stays in the ring as sent.
    fn take_page_commands(&mut self) {
        let Self { slots, page_commands, page_returns, page_reserved, held_page_return, .. } = self;
        if let Some(back) = held_page_return.take() {
            match page_returns.push(back) {
                Ok(()) => *page_reserved -= 1,
                Err(back) => { *held_page_return = Some(back); return; }
            }
        }
        for _ in 0..PAGE_OPS_PER_BUFFER {
            if held_page_return.is_some() || page_returns.free() <= *page_reserved { return; }
            let Some(PagePacket { origin, kind, op }) = page_commands.pop() else { return };
            *page_reserved += 1;
            let source = paged_deck(slots, &origin).and_then(|deck| match deck.track.as_mut() {
                Some(DeckSource::Paged(source)) if source.key() == origin.source => Some(source),
                _ => None,
            });
            let refused = |refusal| PageOutcome::Refused(refusal);
            let (outcome, page, bundle) = match (source, op) {
                (None, PageOp::Install { page, .. }) => (refused(PageRefusal::StaleSource), Some(page), None),
                (None, PageOp::InstallStems { bundle, .. }) => (refused(PageRefusal::StaleSource), None, Some(bundle)),
                (None, _) => (refused(PageRefusal::StaleSource), None, None),
                (Some(source), PageOp::Install { placement, page }) => match source.install(placement, page) {
                    Ok(outcome) => (outcome, None, None),
                    Err((refusal, page)) => (refused(refusal), Some(page), None),
                },
                (Some(source), PageOp::Retire { slot, old_tag, old_placement, retirement }) => match source.retire(slot, old_tag, old_placement, retirement) {
                    Ok((outcome, page)) => (outcome, Some(page), None),
                    Err(refusal) => (refused(refusal), None, None),
                },
                (Some(source), PageOp::InstallStems { slot, serial, placement, bundle }) => match source.install_stems(slot, serial, placement, bundle) {
                    Ok(outcome) => (outcome, None, None),
                    Err((refusal, bundle)) => (refused(refusal), None, Some(bundle)),
                },
                (Some(source), PageOp::RetireStems { slot, old_serial, old_placement, retirement }) => {
                    match source.retire_stems(slot, old_serial, old_placement, retirement) {
                        Ok((outcome, bundle)) => (outcome, None, Some(bundle)),
                        Err(refusal) => (refused(refusal), None, None),
                    }
                }
                (Some(source), PageOp::Notes { serial, revision, notes }) => {
                    (source.apply_notes(serial, revision, notes).map(|revision| PageOutcome::NotesApplied { revision }).unwrap_or_else(refused), None, None)
                }
                (Some(source), PageOp::Frontier(frontier)) => (source.advance_frontier(frontier).unwrap_or_else(refused), None, None),
                (Some(source), PageOp::Seek { expected_serial, frame: frames } | PageOp::Tail { expected_serial, prebuffer: frames } | PageOp::StemTail { expected_serial, cushion: frames }) => {
                    if expected_serial != source.seek_serial() {
                        (refused(PageRefusal::StaleSerial), None, None)
                    } else {
                        // Held with this credit until the quiet point. A seek
                        // it replaces is answered on that seek's own credit.
                        if let Some(old) = source.hold_seek(PendingSeek { origin, kind, expected_serial, frames }) {
                            let back = PageReturn { origin: old.origin, kind: old.kind, outcome: PageOutcome::Superseded, page: None, bundle: None };
                            push_page(page_returns, page_reserved, held_page_return, back);
                        }
                        continue;
                    }
                }
            };
            push_page(page_returns, page_reserved, held_page_return, PageReturn { origin, kind, outcome, page, bundle });
        }
    }

    /// Reserve return capacity before a replacement dip. Other slots may
    /// only spend unreserved capacity; the host consumer can only free it.
    fn settle(&mut self, index: usize, silent: bool) {
        let slot = &mut self.slots[index];
        let quiet = silent || slot.gain <= 0.0 || slot.active.is_none();
        if slot.removing {
            slot.target = 0.0;
            if !quiet { return; }
            // Its pending page seeks are answered before the schedule leaves.
            let leaving = if slot.next.is_some() { slot.next.as_deref_mut() } else { slot.active.as_deref_mut() };
            if let Some(schedule) = leaving {
                if !cancel_page_seeks(&mut self.page_returns, &mut self.page_reserved, &mut self.held_page_return, schedule, &[]) {
                    self.state.retire_waits = self.state.retire_waits.wrapping_add(1);
                    return;
                }
            }
            // The swaps still pending inside the schedule that leaves hold
            // return slots of their own; the schedule may spend them, since
            // it carries those clips back itself.
            let from_next = slot.next.is_some();
            let pending = if from_next { slot.next.as_deref() } else { slot.active.as_deref() }.map(pending_returns).unwrap_or(0);
            let reserved_elsewhere = self.reserved_returns - usize::from(slot.return_credit) - pending;
            if self.retired.free() <= reserved_elsewhere {
                self.state.retire_waits = self.state.retire_waits.wrapping_add(1);
                return;
            }
            let schedule = if from_next { slot.next.take() } else { slot.active.take() };
            let last = slot.next.is_none() && slot.active.is_none();
            let back = Retired { slot: index as u16, incarnation: slot.incarnation.unwrap_or(0), schedule, removed: last, rejected: None, track: None, grown: None };
            match self.retired.push(back) {
                Ok(()) => {
                    if slot.return_credit { self.reserved_returns -= 1; slot.return_credit = false; }
                    self.reserved_returns -= pending;
                    slot.removing = !last;
                    if last { slot.incarnation = None; }
                }
                Err(back) => {
                    if from_next { slot.next = back.schedule; } else { slot.active = back.schedule; }
                }
            }
            return;
        }
        let Some(next) = slot.next.as_ref() else { slot.target = 1.0; return; };
        let rejection = if next.format != self.format {
            Some(Rejection::StaleFormat)
        } else if next.base != slot.active.as_ref().map(|active| active.generation) {
            Some(Rejection::StaleBase)
        } else { None };
        if let Some(reason) = rejection {
            slot.target = 1.0;
            // Never active, so it holds no page seek; kept exact regardless.
            if let Some(schedule) = slot.next.as_deref_mut() {
                if !cancel_page_seeks(&mut self.page_returns, &mut self.page_reserved, &mut self.held_page_return, schedule, &[]) { return; }
            }
            let pending = slot.next.as_deref().map(pending_returns).unwrap_or(0);
            let reserved_elsewhere = self.reserved_returns - usize::from(slot.return_credit) - pending;
            if self.retired.free() <= reserved_elsewhere { return; }
            let schedule = slot.next.take();
            let back = Retired { slot: index as u16, incarnation: slot.incarnation.unwrap_or(0), schedule, removed: false, rejected: Some(reason), track: None, grown: None };
            match self.retired.push(back) {
                Ok(()) => {
                    if slot.return_credit { self.reserved_returns -= 1; slot.return_credit = false; }
                    self.reserved_returns -= pending;
                    // A rejected first install never became active. Once
                    // its sole payload is owned by the return queue, this
                    // slot has no callback-owned state of that incarnation.
                    // A later host-side abort may therefore reuse the slot.
                    // A replacement rejection must keep the old claim.
                    if slot.active.is_none() && slot.next.is_none() { slot.incarnation = None; }
                    self.state.base_mismatches = self.state.base_mismatches.wrapping_add(1);
                }
                Err(back) => slot.next = back.schedule,
            }
            return;
        }
        if slot.active.is_none() {
            slot.active = slot.next.take();
            slot.gain = 0.0;
            slot.target = 1.0;
            return;
        }
        if !slot.return_credit {
            if self.retired.free() <= self.reserved_returns {
                self.state.retire_waits = self.state.retire_waits.wrapping_add(1);
                slot.target = 1.0;
                return;
            }
            self.reserved_returns += 1;
            slot.return_credit = true;
        }
        if next.dip && !quiet { slot.target = 0.0; return; }
        // Page seeks of old deck states that do not migrate are cancelled on
        // their own credits; room for all of them is required up front.
        // Their credits are released before any state moves, so a failure
        // leaves the swap undone and nothing half-migrated.
        if let (Some(next), Some(old)) = (slot.next.as_deref(), slot.active.as_deref_mut()) {
            let staying = if next.rate == old.rate { &next.migrate[..] } else { &next.migrate_decks[..] };
            if !cancel_page_seeks(&mut self.page_returns, &mut self.page_reserved, &mut self.held_page_return, old, staying) {
                self.state.retire_waits = self.state.retire_waits.wrapping_add(1);
                return;
            }
        }
        let mut next = slot.next.take().expect("checked above");
        let mut old = slot.active.take().expect("checked above");
        if next.rate == old.rate {
            next.take_over(&mut old);
        } else {
            // A deck's position is in source frames: it survives the rate.
            for (from, to) in next.migrate_decks.iter() {
                std::mem::swap(&mut old.states[*from as usize], &mut next.states[*to as usize]);
            }
        }
        // Every state left in `old` had its page seek cancelled above;
        // migrated states took theirs (and their credits) along.
        // The reservation guarantees success: no other producer exists.
        // Keep ownership even if that invariant is broken by a future edit.
        let pending = pending_returns(&old);
        let back = Retired { slot: index as u16, incarnation: old.incarnation, schedule: Some(old), removed: false, rejected: None, track: None, grown: None };
        if let Err(back) = self.retired.push(back) {
            slot.next = back.schedule;
        }
        self.reserved_returns -= 1 + pending;
        slot.return_credit = false;
        slot.active = Some(next);
        slot.target = 1.0;
    }

    fn pass(&mut self, index: usize, rate: u32, frames: usize) {
        self.settle(index, false);
        let mismatch = self.slots[index].active.as_ref().is_some_and(|schedule| schedule.rate != rate);
        self.settle_decks(index, mismatch);
        self.settle_exchange(index);
        let slot = &mut self.slots[index];
        let Some(schedule) = slot.active.as_mut() else { return };
        self.state.slots[index].rate_mismatch = mismatch;
        if mismatch {
            // Prepared for another rate: silent, and quiet for a swap.
            slot.gain = 0.0;
            self.state.slots[index].resting = false;
            return;
        }
        let (mut gain, step) = (slot.gain, 1.0 / DIP_FRAMES as f32);
        let target = slot.target;
        if self.resting_enabled && rests(schedule, &DeckRun) {
            self.rested_passes += 1;
            // Silent, nothing running, nothing waiting: the graph is not
            // run (its spans hold the silence of the last pass). The dip
            // goes on, and the buffer is still reported as rendered.
            schedule.last_frames = frames as u16;
            slot.gain = if gain < target { (gain + step * frames as f32).min(target) } else { (gain - step * frames as f32).max(target) };
            self.state.slots[index].resting = true;
            return;
        }
        self.state.slots[index].resting = false;
        self.slot_mix[..frames].fill(0.0);
        self.slot_mix[CHUNK..CHUNK + frames].fill(0.0);
        self.slot_cue[..frames].fill(0.0);
        self.slot_cue[CHUNK..CHUNK + frames].fill(0.0);
        run_awake(schedule, frames, &mut |cue, left, right| {
            let mix = if cue { &mut self.slot_cue } else { &mut self.slot_mix };
            for frame in 0..frames {
                mix[frame] += left[frame];
                mix[CHUNK + frame] += right[frame];
            }
        }, &mut DeckRun, self.resting_enabled);
        self.state.slots[index].resting = self.resting_enabled && schedule.rest.asleep();
        // Once per sample, including muted/absent outputs and all sinks.
        let mut dip = [0.0f32; CHUNK];
        for frame in 0..frames {
            gain = if gain < target { (gain + step).min(target) } else { (gain - step).max(target) };
            dip[frame] = gain;
            self.mix[frame] += self.slot_mix[frame] * gain;
            self.mix[CHUNK + frame] += self.slot_mix[CHUNK + frame] * gain;
            self.cue_mix[frame] += self.slot_cue[frame] * gain;
            self.cue_mix[CHUNK + frame] += self.slot_cue[CHUNK + frame] * gain;
        }
        for sink in &schedule.sinks {
            let span = &mut schedule.arena[*sink as usize * SPAN..][..SPAN];
            for frame in 0..frames {
                span[frame] *= dip[frame];
                span[CHUNK + frame] *= dip[frame];
            }
        }
        slot.gain = gain;
    }
}

/// The deck's pass: its clip rendered into its five spans, and its probes.
struct DeckRun;

impl RunExt<Box<DeckState>, DeckOp> for DeckRun {
    /// A deck runs while it plays, a hand or its release moves it, a load,
    /// cue or swap waits for its dip, or it reads a live feed.
    fn running(&self, op: &DeckOp, states: &[NodeState], controls: &ControlBank) -> bool {
        let NodeState::Ext(deck) = &states[op.state as usize] else { return false };
        // A clip that ran to its end without a loop is not playing any more,
        // whatever its play control says.
        let playing = controls.get(op.play) >= 0.5
            && match deck.track.as_ref() {
                None => false,
                Some(DeckSource::Clip(track)) => controls.get(op.loop_enabled) >= 0.5 || deck.position < track.pcm().frames.len() as f64,
                // (a feed plays only what is written: below)
                Some(DeckSource::Feed(_)) => false,
                Some(DeckSource::Paged(_)) => true,
            };
        playing
            || controls.get(op.scratch) >= 0.5
            || deck.motion.state != deck::MotionState::Transport
            || deck.next.is_some()
            || deck.seek.is_some()
            || deck.exchange.is_some()
            || match deck.track.as_ref() {
                Some(DeckSource::Feed(feed)) => feed.pending(),
                Some(DeckSource::Paged(source)) => source.counts().pending_seek,
                _ => false,
            }
    }

    fn run(&mut self, op: &DeckOp, states: &mut [NodeState], arena: &mut [f32], controls: &ControlBank, probes: &mut [f32], rate: f32, frames: usize) {
        let DeckOp { state, out, play, speed, loop_in, loop_out, loop_enabled, scratch, scratch_at, brake, fling, key, keylock, probes: (probe_pos, probe_end, probe_stems), .. } = op;
        let NodeState::Ext(deck) = &mut states[*state as usize] else { return };
        let span = &mut arena[*out as usize * SPAN..][..SPAN * 5];
        let read = DeckControls {
            play: controls.get(*play) >= 0.5,
            speed: controls.get(*speed),
            loop_in: controls.get(*loop_in),
            loop_out: controls.get(*loop_out),
            loop_enabled: controls.get(*loop_enabled) >= 0.5,
            scratch: controls.get(*scratch) >= 0.5,
            scratch_at: controls.get(*scratch_at),
            brake: controls.get(*brake) >= 0.5,
            fling: controls.get(*fling),
            key: controls.get(*key),
            keylock: controls.get(*keylock) >= 0.5,
        };
        let (position, end) = deck::render_bundle(deck, read, rate, frames, span);
        probes[*probe_pos as usize] = position;
        probes[*probe_end as usize] = end;
        // A clip's stems cover it whole; a paged source's cover only
        // the chunks its bundles hold, decided by the render above.
        let stems = match deck.track.as_ref() {
            Some(DeckSource::Clip(track)) => track.has_stems(),
            Some(DeckSource::Paged(source)) => source.stems_available(),
            Some(DeckSource::Feed(_)) | None => false,
        };
        probes[*probe_stems as usize] = if stems { 1.0 } else { 0.0 };
    }
}

/// Room to push `count` page receipts now: nothing parked and at least
/// `count` free slots. The callback is the ring's only producer and the host
/// only frees slots, so pushes made right after this check succeed.
fn page_room(returns: &Producer<PageReturn>, held: &Option<PageReturn>, count: usize) -> bool {
    held.is_none() && returns.free() >= count
}

/// Push one page receipt on its reserved credit.
///
/// Precondition, checked at every call site immediately before its single
/// push: nothing is parked, and the ring has a free slot (`page_room`, or a
/// command taken only with `free > reserved`). The callback is the ring's
/// only producer and the host only frees slots, so the push succeeds. The
/// parking branch exists for a broken invariant only; under the
/// precondition it never replaces a parked receipt.
fn push_page(returns: &mut Producer<PageReturn>, reserved: &mut usize, held: &mut Option<PageReturn>, back: PageReturn) {
    match returns.push(back) {
        Ok(()) => *reserved -= 1,
        Err(back) => *held = Some(back),
    }
}

/// The deck an admitted page operation names, if it still runs there.
fn paged_deck<'a>(slots: &'a mut [Slot], origin: &PageOrigin) -> Option<&'a mut DeckState> {
    let slot = slots.get_mut(origin.slot as usize).filter(|slot| !slot.removing)?;
    let schedule = slot.active.as_mut().filter(|schedule| schedule.incarnation == origin.incarnation)?;
    let named = schedule.triggers.binary_search_by_key(&origin.target, |entry| entry.0).ok()
        .map(|at| schedule.triggers[at]).filter(|(_, _, held)| *held == origin.continuity).map(|(_, index, _)| index as usize);
    // An operation follows its source: after a deck exchange it reaches the
    // deck that plays that source now, whichever deck it was addressed to.
    let owns = |state: &NodeState| matches!(state, NodeState::Ext(deck) if deck.track.as_ref().and_then(DeckSource::paged).is_some_and(|source| source.key() == origin.source));
    let index = match named {
        Some(index) if schedule.states.get(index).is_some_and(owns) => index,
        _ => match schedule.states.iter().position(owns) {
            Some(index) => index,
            None => named?,
        },
    };
    match schedule.states.get_mut(index)? {
        NodeState::Ext(deck) => Some(deck),
        _ => None,
    }
}

/// Answer `Cancelled` every pending page seek of a leaving schedule, except
/// in states that migrate (`staying`, as `(from, to)` state pairs). Room for
/// all of them is required first; false changes nothing then. Seeks not yet
/// answered (only after a broken invariant parked one) stay pending.
fn cancel_page_seeks(returns: &mut Producer<PageReturn>, reserved: &mut usize, held: &mut Option<PageReturn>, schedule: &mut Schedule, staying: &[(u16, u16)]) -> bool {
    let leaving = |index: usize| !staying.iter().any(|(from, _)| *from as usize == index);
    let count = schedule.states.iter().enumerate().filter(|(index, state)| leaving(*index) && matches!(state, NodeState::Ext(deck)
        if deck.track.as_ref().and_then(DeckSource::paged).is_some_and(|source| source.counts().pending_seek))).count();
    if count > 0 && !page_room(returns, held, count) { return false; }
    for (index, state) in schedule.states.iter_mut().enumerate() {
        if !leaving(index) { continue; }
        let NodeState::Ext(deck) = state else { continue };
        let Some(DeckSource::Paged(source)) = deck.track.as_mut() else { continue };
        if !source.counts().pending_seek { continue; }
        if held.is_some() { return false; }
        if let Some(seek) = source.take_pending_seek() {
            push_page(returns, reserved, held, PageReturn { origin: seek.origin, kind: seek.kind, outcome: PageOutcome::Cancelled, page: None, bundle: None });
        }
    }
    held.is_none()
}

/// At the quiet point: apply the deck's pending paged seek, cue or tail and
/// answer it. The serial moves only on acceptance, by exactly one.
/// Let go of every hand on the schedule's decks (their `scratch` controls,
/// remembered and effective). A deck swap ends every gesture, applied or not.
fn release_hands(schedule: &Schedule) {
    for op in schedule.ops.iter() {
        if let Op::Ext(DeckOp { scratch, .. }) = op {
            schedule.controls.set_manual(*scratch, 0.0);
        }
    }
}

/// `request`: the deck's loop controls as they are now. A loop turned off (or
/// on) by a write the callback has not rendered yet is taken into account
/// before the operation is judged, so an earlier write is never overtaken by
/// a later seek, tail or stem tail.
fn accept_page_seek(deck: &mut DeckState, request: LoopRequest, returns: &mut Producer<PageReturn>, reserved: &mut usize, held: &mut Option<PageReturn>) {
    if !page_room(returns, held, 1) { return; }
    let Some(DeckSource::Paged(source)) = deck.track.as_mut() else { return };
    if !source.counts().pending_seek { return; }
    source.apply_request(request, &mut deck.position, !deck.motion.loop_suspended);
    let Some(seek) = source.take_pending_seek() else { return };
    let (outcome, frame) = match source.accept_seek(&seek) {
        Ok((frame, serial)) => (PageOutcome::SeekAccepted { frame, serial }, Some(frame)),
        Err(refusal) => (PageOutcome::Refused(refusal), None),
    };
    push_page(returns, reserved, held, PageReturn { origin: seek.origin, kind: seek.kind, outcome, page: None, bundle: None });
    if let Some(frame) = frame {
        deck.position = frame as f64;
        // A hand still on the record takes hold again at the new place.
        deck.motion = crate::deck::Motion::default();
    }
}

#[cfg(test)]
mod exchange_tests {
    use super::*;
    use crate::deck::{DeckTrack, TrackLease};
    use crate::paged::{PagePacket, PageReturn, PAGE_ROUTES};
    use crate::schedule::{compile, trigger_id, AudioBudget, Compiled, ControlBank};
    use makepad_flow::document::ident::DNode;
    use makepad_flow::document::types::TypeKey;
    use makepad_flow::live::plan::{LiveNode, OwnerSlice};
    use makepad_lock_free::{ring, triple_buffer, Consumer, Producer};
    use makepad_flow::Literal;
    use makepad_stage_audio_media::pcm::TrackPcm;
    use std::sync::atomic::AtomicU64;

    const FORMAT: u64 = (1u64 << 32) | 48_000;

    fn deck(id: &str) -> LiveNode {
        let params = [("play", 0.0), ("speed", 1.0), ("loop_in", 0.0), ("loop_out", 0.0), ("loop_enabled", 0.0), ("cue_at", 0.0), ("scratch", 0.0), ("scratch_at", 0.0), ("brake", 0.0), ("fling", 0.0), ("key", 0.0), ("keylock", 0.0)];
        let audio = ["audio", "vocals", "drums", "bass", "other"].map(|port| (port.to_string(), TypeKey::new(crate::nodes::AUDIO)));
        let level = ["position", "end", "stems_available"].map(|port| (port.to_string(), TypeKey::new(crate::nodes::LEVEL)));
        LiveNode {
            node: DNode::root(id),
            id: id.into(),
            type_name: "AudioDeck".into(),
            params: params.iter().map(|(name, value)| (name.to_string(), Literal::Num(*value))).collect(),
            inputs: Vec::new(),
            outputs: audio.into_iter().chain(level).collect(),
        }
    }

    struct Rig {
        engine: AudioEngine,
        commands: Producer<Command>,
        retired: Consumer<Retired>,
        budget: Arc<AudioBudget>,
        compiled: Option<Compiled>,
        bank: Arc<ControlBank>,
        layout: Arc<crate::schedule::Layout>,
        _packets: Producer<Packet>,
        _pages: Producer<PagePacket>,
        _page_returns: Consumer<PageReturn>,
    }

    impl Rig {
        fn new() -> Self {
            let (commands, command_queue) = ring::<Command>(64);
            let (retire, retired) = ring::<Retired>(64);
            let (packets, packet_queue) = ring::<Packet>(64);
            let (writer, _telemetry) = triple_buffer(Telemetry::default());
            let (audio_writer, _audio) = ring::<AudioProbePacket>(AUDIO_PROBE_QUEUE);
            let (pages, page_queue) = ring::<PagePacket>(PAGE_ROUTES);
            let (page_writer, page_returns) = ring::<PageReturn>(PAGE_ROUTES);
            let engine = AudioEngine::new(command_queue, retire, packet_queue, writer, audio_writer, Arc::new(AtomicU64::new(FORMAT)), page_queue, page_writer);
            let budget = Arc::new(AudioBudget::default());
            let (compiled, _board) = compile(&OwnerSlice { owner: crate::nodes::OWNER.into(), nodes: vec![deck("deck_a"), deck("deck_b")] }, 1, 1, FORMAT, None, &budget).unwrap();
            let (bank, layout) = (compiled.schedule.controls.clone(), compiled.layout.clone());
            let mut rig = Self { engine, commands, retired, budget, compiled: Some(compiled), bank, layout, _packets: packets, _pages: pages, _page_returns: page_returns };
            let schedule = rig.compiled.take().unwrap().schedule;
            rig.send(Command::Install { slot: 0, schedule });
            rig.idle();
            rig
        }

        fn send(&mut self, command: Command) {
            assert!(self.commands.push(command).is_ok());
        }

        fn idle(&mut self) {
            for _ in 0..4 {
                self.engine.idle();
                while self.retired.pop().is_some() {}
            }
        }

        fn deck_key(&self, node: &str) -> (u32, u64) {
            let (_, continuity) = self.layout.triggers[node];
            (trigger_id(node), continuity)
        }

        fn load(&mut self, node: &str, sequence: u64, frames: usize) {
            let pcm = TrackPcm { frames: vec![[sequence as i16, 0]; frames], sample_rate: 48_000 };
            let lease = TrackLease::reserve(&self.budget, frames * 4).unwrap();
            let track: Arc<DeckTrack> = lease.finish(pcm).unwrap();
            let (target, continuity) = self.deck_key(node);
            self.send(Command::Track { slot: 0, incarnation: 1, target, continuity, sequence, track: Some(DeckSource::Clip(track)) });
            self.idle();
        }

        fn state(&mut self, node: &str) -> &mut DeckState {
            let (target, _) = self.deck_key(node);
            let schedule = self.engine.slots[0].active.as_mut().unwrap();
            let at = schedule.triggers.binary_search_by_key(&target, |entry| entry.0).unwrap();
            let index = schedule.triggers[at].1 as usize;
            match &mut schedule.states[index] {
                NodeState::Ext(deck) => deck,
                _ => unreachable!(),
            }
        }

        fn control(&self, name: &str) -> u16 {
            self.layout.controls[name].0
        }

        fn exchange(&mut self, serial: u64) {
            let (a, b) = (self.deck_key("deck_a"), self.deck_key("deck_b"));
            self.send(Command::Exchange { slot: 0, incarnation: 1, a, b, serial });
        }
    }

    #[test]
    fn a_deck_ending_mid_pass_rests_on_the_same_grid_frame() {
        // The rig has no output wired, so the mix is quiet while the deck
        // runs. All three hosts start a pass here, past the one-second hold.
        const START: usize = 2 * 256 * 97;
        for pass in [256, 97, 1] {
            let mut rig = Rig::new();
            rig.load("deck_a", 1, START + 100);
            rig.bank.set_manual(rig.control("deck_a.play"), 1.0);
            rig.engine.render_silent(48_000.0, START);
            assert_eq!(rig.state("deck_a").position, START as f64);
            assert!(!rig.engine.state.slots[0].resting, "a playing deck keeps the quiet graph awake");

            for offset in (0..256).step_by(pass) {
                rig.engine.render_silent(48_000.0, pass.min(256 - offset));
            }
            assert_eq!(rig.state("deck_a").position, (START + 100) as f64);
            let schedule = rig.engine.slots[0].active.as_ref().unwrap();
            assert!(schedule.rest.asleep(), "clip ended, pass {pass}");
            assert_eq!(schedule.frame, (START + 128) as u64, "first grid frame after the clip end, pass {pass}");
        }
    }

    #[test]
    fn an_exchange_trades_sources_places_and_transports_and_lets_go_of_hands() {
        let mut rig = Rig::new();
        rig.load("deck_a", 1, 48_000);
        rig.load("deck_b", 2, 96_000);
        rig.state("deck_a").position = 1_000.0;
        rig.state("deck_b").position = 5_000.0;
        for (name, value) in [("deck_a.play", 1.0), ("deck_a.speed", 1.5), ("deck_a.loop_enabled", 1.0), ("deck_a.loop_in", 2.0), ("deck_a.loop_out", 3.0), ("deck_a.cue_at", 4.0), ("deck_b.scratch", 1.0)] {
            rig.bank.set_manual(rig.control(name), value);
        }
        rig.exchange(7);
        rig.idle();
        assert_eq!(rig.engine.state.slots[0].exchange, (7, true));
        assert_eq!((rig.state("deck_a").sequence, rig.state("deck_a").position), (2, 5_000.0));
        assert_eq!((rig.state("deck_b").sequence, rig.state("deck_b").position), (1, 1_000.0));
        assert!(rig.state("deck_a").exchange.is_none() && rig.state("deck_b").exchange.is_none());
        let get = |rig: &Rig, name: &str| (rig.bank.get(rig.control(name)), rig.bank.manual(rig.control(name)));
        assert_eq!(get(&rig, "deck_b.play"), (1.0, 1.0));
        assert_eq!(get(&rig, "deck_a.play"), (0.0, 0.0));
        assert_eq!(get(&rig, "deck_b.speed"), (1.5, 1.5));
        assert_eq!(get(&rig, "deck_b.loop_in"), (2.0, 2.0));
        assert_eq!(get(&rig, "deck_b.cue_at"), (4.0, 4.0));
        assert_eq!(get(&rig, "deck_a.loop_enabled"), (0.0, 0.0));
        assert_eq!(get(&rig, "deck_a.scratch"), (0.0, 0.0));
        assert_eq!(get(&rig, "deck_b.scratch"), (0.0, 0.0));
    }

    #[test]
    fn a_write_before_the_exchange_moves_with_it_and_one_after_stays_put() {
        let mut rig = Rig::new();
        rig.load("deck_a", 1, 48_000);
        rig.load("deck_b", 2, 48_000);
        // The user pauses A (the host admitted it before the exchange).
        rig.bank.set_manual(rig.control("deck_a.play"), 1.0);
        rig.bank.set_manual(rig.control("deck_b.play"), 1.0);
        rig.bank.set_manual(rig.control("deck_a.play"), 0.0);
        rig.exchange(8);
        rig.idle();
        // A's source (paused) is on B now; B's (playing) on A.
        assert_eq!(rig.bank.get(rig.control("deck_b.play")), 0.0);
        assert_eq!(rig.bank.get(rig.control("deck_a.play")), 1.0);
        assert_eq!(rig.state("deck_b").sequence, 1);
        // A write after the answer addresses the deck as it is now.
        rig.bank.set_manual(rig.control("deck_a.play"), 0.0);
        rig.idle();
        assert_eq!(rig.bank.get(rig.control("deck_b.play")), 0.0);
        assert_eq!(rig.state("deck_a").sequence, 2);
    }

    #[test]
    fn a_replacement_schedule_during_an_exchange_answers_it_not_applied() {
        let mut rig = Rig::new();
        rig.load("deck_a", 1, 48_000);
        rig.load("deck_b", 2, 48_000);
        let (next, _board) = compile(&OwnerSlice { owner: crate::nodes::OWNER.into(), nodes: vec![deck("deck_a"), deck("deck_b")] }, 2, 1, FORMAT, Some(&rig.layout), &rig.budget).unwrap();
        rig.exchange(9);
        rig.send(Command::Install { slot: 0, schedule: next.schedule });
        rig.idle();
        assert_eq!(rig.engine.state.slots[0].exchange, (9, false));
        // Nothing traded, and neither deck is held busy by it.
        assert_eq!(rig.state("deck_a").sequence, 1);
        assert!(rig.state("deck_a").exchange.is_none() && rig.state("deck_b").exchange.is_none());
    }

    #[test]
    fn an_exchange_naming_a_missing_deck_is_refused() {
        let mut rig = Rig::new();
        let a = rig.deck_key("deck_a");
        rig.send(Command::Exchange { slot: 0, incarnation: 1, a, b: (trigger_id("nope"), 0), serial: 10 });
        rig.idle();
        assert_eq!(rig.engine.state.slots[0].exchange, (10, false));
        assert!(rig.state("deck_a").exchange.is_none());
    }

    #[test]
    fn a_cancel_before_the_quiet_point_answers_not_applied_and_frees_both_decks() {
        let mut rig = Rig::new();
        rig.load("deck_a", 1, 48_000);
        rig.load("deck_b", 2, 48_000);
        // The device stopped: nothing runs between these two commands.
        rig.exchange(11);
        rig.send(Command::CancelExchange { slot: 0, incarnation: 1, serial: 11 });
        // It runs again: the cancel is taken with the exchange, before it applies.
        rig.idle();
        assert_eq!(rig.engine.state.slots[0].exchange, (11, false));
        assert_eq!(rig.state("deck_a").sequence, 1);
        assert!(rig.state("deck_a").exchange.is_none() && rig.state("deck_b").exchange.is_none());
    }

    #[test]
    fn a_cancel_after_the_exchange_applied_keeps_its_answer() {
        let mut rig = Rig::new();
        rig.load("deck_a", 1, 48_000);
        rig.load("deck_b", 2, 48_000);
        rig.exchange(12);
        rig.idle();
        rig.send(Command::CancelExchange { slot: 0, incarnation: 1, serial: 12 });
        rig.idle();
        assert_eq!(rig.engine.state.slots[0].exchange, (12, true));
        assert_eq!(rig.state("deck_a").sequence, 2);
    }


    #[test]
    fn a_withdrawn_swap_still_lets_go_of_a_hand() {
        let mut rig = Rig::new();
        rig.load("deck_a", 1, 48_000);
        rig.load("deck_b", 2, 48_000);
        rig.bank.set_manual(rig.control("deck_a.scratch"), 1.0);
        rig.exchange(13);
        rig.send(Command::CancelExchange { slot: 0, incarnation: 1, serial: 13 });
        rig.idle();
        assert_eq!(rig.engine.state.slots[0].exchange, (13, false));
        assert_eq!((rig.bank.get(rig.control("deck_a.scratch")), rig.bank.manual(rig.control("deck_a.scratch"))), (0.0, 0.0));
    }

    #[test]
    fn a_swap_not_applied_under_a_replacement_still_lets_go_of_a_hand() {
        let mut rig = Rig::new();
        rig.load("deck_a", 1, 48_000);
        rig.load("deck_b", 2, 48_000);
        rig.bank.set_manual(rig.control("deck_b.scratch"), 1.0);
        let (next, _board) = compile(&OwnerSlice { owner: crate::nodes::OWNER.into(), nodes: vec![deck("deck_a"), deck("deck_b")] }, 2, 1, FORMAT, Some(&rig.layout), &rig.budget).unwrap();
        rig.exchange(14);
        rig.send(Command::Install { slot: 0, schedule: next.schedule });
        rig.idle();
        assert_eq!(rig.engine.state.slots[0].exchange, (14, false));
        // Whichever schedule answered it, its hands are let go.
        let active = rig.engine.slots[0].active.as_ref().unwrap().controls.clone();
        let index = rig.control("deck_b.scratch");
        assert_eq!(active.get(index), 0.0);
    }


    /// No fixed number of live instances: the engine grows its slots on a
    /// command prepared off the audio thread, keeps what ran, and installs
    /// past the old count.
    #[test]
    fn the_engine_grows_its_slots_and_keeps_what_runs() {
        let mut rig = Rig::new();
        assert_eq!(rig.engine.slots.len(), INITIAL_SLOTS);
        let (writer, _reader) = triple_buffer(Telemetry::with_slots(INITIAL_SLOTS * 2));
        rig.send(Command::Grow(Grow::new(INITIAL_SLOTS * 2, writer)));
        let (compiled, _board) = compile(&OwnerSlice { owner: crate::nodes::OWNER.into(), nodes: vec![deck("deck_a")] }, 1, 9, FORMAT, None, &rig.budget).unwrap();
        let slot = (INITIAL_SLOTS + 2) as u16;
        rig.send(Command::Install { slot, schedule: compiled.schedule });
        let mut grown = false;
        for _ in 0..4 {
            rig.engine.idle();
            while let Some(back) = rig.retired.pop() {
                grown |= back.grown.is_some();
            }
        }
        assert_eq!(rig.engine.slots.len(), INITIAL_SLOTS * 2);
        assert_eq!(rig.engine.state.slots.len(), INITIAL_SLOTS * 2);
        assert!(rig.engine.slots[0].active.is_some(), "slot 0 kept its schedule");
        assert_eq!(rig.engine.state.slots[slot as usize].incarnation, 9);
        assert!(grown, "the old slots came back to the host");
    }
}
