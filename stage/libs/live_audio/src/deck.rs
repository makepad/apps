//! The deck: one prepared stereo clip played at a variable speed with a
//! loop, a cue point and a source that is swapped by an owner command.
//!
//! Nothing here reads a file or decodes. A caller reserves PCM memory from
//! the shared track budget *before* it produces a clip ([`AudioDevice::reserve_track`]),
//! finishes the lease with the clip ([`TrackLease::finish`]), and sends the
//! resulting [`DeckTrack`] to a deck's `source` endpoint as a
//! [`DeckCommand`] through the host's owner-command path. The lease lives
//! as long as the track: pending, active, or on its way back.
//!
//! On the callback the deck allocates nothing, clones and drops no `Arc`,
//! and looks nothing up: a swap moves the old clip into a return slot the
//! engine had reserved before it took the command, and the host drops it.
//! Every change of what is heard (a load, an eject, a cue, pausing) is
//! wrapped in a short gain dip inside this one op.

use relax_attr::relax;
use crate::paged::PagedSource;
use crate::schedule::AudioBudget;
use makepad_audio_dsp::grain::Grains;
use makepad_stage_audio_media::pcm::TrackPcm;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Frames of the dip around a swap, a cue or a pause, at the device rate.
pub const DECK_DIP_FRAMES: u32 = 256;
pub const SPEED_MIN: f64 = 0.25;
pub const SPEED_MAX: f64 = 4.0;
/// How far `key` shifts the pitch, in semitones either way: an octave.
pub const KEY_SHIFT_MAX: f64 = 12.0;
/// A time-stretched deck's grains, in seconds of the source: long enough
/// to hold a bass period, short enough that a beat lands inside one.
pub const STRETCH_GRAIN_SECONDS: f64 = 0.04;
/// How far a grain may move from the head to start in phase with the
/// other, in seconds of the source: half a period of 80 Hz.
pub const STRETCH_ALIGN_SECONDS: f64 = 0.006;
/// The pitch, against the head's rate, within this of unity plays plain
/// varispeed (an untouched deck stays sample-exact).
const STRETCH_EPSILON: f64 = 1e-4;
/// Seconds a cue or loop point may name.
pub const MAX_SECONDS: f64 = 36_000.0;
/// Fastest a hand may move the record, in multiples of the source's own
/// rate, either way; see [`scratch_step_limit`] for the bound in the units
/// the renderers and the page planner share.
pub const SCRATCH_RATE_MAX: f64 = 16.0;
/// The hand-follow spring's natural frequency: critically damped, so the
/// record never overshoots the hand and its speed changes without steps.
const SCRATCH_FOLLOW_HZ: f64 = 14.0;
/// Seconds the platter takes, whatever its speed, from the hand's speed to
/// the transport's (or to rest) once the hand lets go.
const SCRATCH_RELEASE_SECONDS: f64 = 0.12;
/// The hand's level rises continuously from 0 at standstill to 1 at this
/// share of the source's own speed, so a held sample never becomes DC and
/// a slow hand fades rather than gating.
const STANDSTILL_SHARE: f64 = 0.05;
/// A braking touch (`brake`) stops the record in this long, fastest at
/// first (a hand pressed on a spinning platter), and a hand that moves
/// takes over from the brake within `BRAKE_HANDOVER_SECONDS`.
pub const BRAKE_SECONDS: f64 = 0.3;
const BRAKE_HANDOVER_SECONDS: f64 = 0.004;
/// A flung record (`fling`) eases from the fling's speed to the transport's
/// with this time constant, and covers the last of it at no less than
/// `COAST_MIN_SLEW` of the source's speed per second, landing exactly.
const COAST_SECONDS: f64 = 0.6;
const COAST_MIN_SLEW: f64 = 0.1;
/// A freely moving record (a coast, the reverse transport) stops softly at
/// the clip's ends: it decelerates at most at this many times the source's
/// speed per second.
const END_DECEL: f64 = 24.0;
/// The spring lands exactly on the hand once this close (source frames) and
/// this slow (source frames per output frame); no endless tail, no denormals.
const SETTLE_FRAMES: f64 = 1e-3;
const SETTLE_SPEED: f64 = 1e-5;

/// The largest step a hand may take, in source frames per output frame:
/// [`SCRATCH_RATE_MAX`] times the source/device ratio, and never more than
/// half the paged window planner's own limit, whatever that ratio is (a
/// 192 kHz source on a 44.1 kHz device would otherwise outrun its paging).
pub fn scratch_step_limit(nominal: f64) -> f64 {
    (SCRATCH_RATE_MAX * nominal).min(makepad_stage_asset_stream::window::MAX_WINDOW_SPEED * 0.5)
}

/// A reservation of track bytes, taken before any PCM is produced. Send
/// it to the worker that produces the clip; dropping it releases the bytes.
pub struct TrackLease {
    budget: Arc<AudioBudget>,
    bytes: usize,
}

impl TrackLease {
    pub(crate) fn reserve(budget: &Arc<AudioBudget>, bytes: usize) -> Result<Self, String> {
        if bytes == 0 {
            return Err("a track reservation is at least one byte".to_string());
        }
        // The machine is the limit: a clip that cannot get its memory is
        // refused here, before it is decoded. The budget only accounts.
        if Vec::<u8>::new().try_reserve_exact(bytes).is_err() {
            return Err(format!("not enough memory for {} MiB of track PCM", bytes >> 20));
        }
        budget.tracks.fetch_add(bytes, Ordering::AcqRel);
        Ok(Self { budget: budget.clone(), bytes })
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn budget_arc(&self) -> &Arc<AudioBudget> {
        &self.budget
    }

    /// Whether this lease is on exactly `budget`.
    pub(crate) fn is_on(&self, budget: &Arc<AudioBudget>) -> bool {
        Arc::ptr_eq(&self.budget, budget)
    }

    /// Reserve another clip on this exact device budget before decoding it.
    /// Failure is immediate; optional stems must be released instead of waited on.
    pub fn reserve_related(&self, bytes: usize) -> Result<Self, String> {
        Self::reserve(&self.budget, bytes)
    }

    /// The clip, checked against the lease: a valid rate, at least one frame,
    /// and the frames' *capacity* within the reservation. The lease then
    /// shrinks to that capacity (the worst case reserved for decoding is
    /// given back at once, without copying) and travels inside the track
    /// from here on. A refused clip is dropped by the caller, on its own
    /// thread.
    pub fn finish(self, pcm: TrackPcm) -> Result<Arc<DeckTrack>, String> {
        self.finish_with_stems(pcm, None)
    }

    /// Finish one acknowledged source package. Stem order is vocals, drums,
    /// bass, other. The caller proves common time origin and source revision;
    /// this boundary checks duration, budget ownership and bounded nesting.
    pub fn finish_with_stems(self, pcm: TrackPcm, stems: Option<[Arc<DeckTrack>; 4]>) -> Result<Arc<DeckTrack>, String> {
        self.finish_continuing(pcm, stems, None)
    }

    /// [`Self::finish_with_stems`] for a clip that `continues` the clip the
    /// deck plays under that sequence: the same audio, now with its stems.
    /// It takes over at that clip's position instead of the start (see
    /// [`DeckState::apply_swap`]); any other clip it replaces starts at 0.
    pub fn finish_continuing(mut self, pcm: TrackPcm, stems: Option<[Arc<DeckTrack>; 4]>, continues: Option<u64>) -> Result<Arc<DeckTrack>, String> {
        if !(8_000..=384_000).contains(&pcm.sample_rate) {
            return Err(format!("track sample rate {} Hz is outside 8 kHz to 384 kHz", pcm.sample_rate));
        }
        if pcm.frames.is_empty() {
            return Err("the track has no frames".to_string());
        }
        if let Some(stems) = &stems {
            for stem in stems {
                if !stem.leased_from(&self.budget) || stem.stems.is_some() {
                    return Err("stems must be plain clips leased from this device".to_string());
                }
                if (stem.seconds() - pcm.seconds()).abs() > 0.1 + 1e-9 {
                    return Err("stem duration differs from the mixed track by more than 100 ms".to_string());
                }
            }
        }
        let capacity = pcm.frames.capacity().saturating_mul(std::mem::size_of::<[i16; 2]>());
        if capacity > self.bytes {
            return Err(format!("the track occupies {capacity} bytes but {} were reserved", self.bytes));
        }
        if capacity < self.bytes {
            self.budget.tracks.fetch_sub(self.bytes - capacity, Ordering::Release);
            self.bytes = capacity;
        }
        Ok(Arc::new(DeckTrack { pcm, stems, continues, _lease: self }))
    }
}

impl TrackLease {
    /// Finish an aggregate lease reserved before the four outputs were
    /// allocated: a valid rate, a non-empty `range`, every stem exactly
    /// `range.len()` frames, and their summed *capacities* within the lease,
    /// which then shrinks to them. Stem order is vocals, drums, bass, other.
    /// Refused, the stems drop here, on the calling worker.
    pub fn finish_stem_bundle(
        mut self,
        media: makepad_stage_asset_stream::MediaIdentity,
        model: crate::bundle::StemModel,
        sample_rate: u32,
        range: makepad_stage_asset_stream::SampleRange,
        stems: [Vec<[i16; 2]>; 4],
    ) -> Result<Arc<crate::bundle::PreparedStemBundle>, String> {
        if !(8_000..=384_000).contains(&sample_rate) {
            return Err(format!("stem sample rate {sample_rate} Hz is outside 8 kHz to 384 kHz"));
        }
        if range.is_empty() {
            return Err("a stem bundle covers at least one frame".to_string());
        }
        if stems.iter().any(|stem| stem.len() as u64 != range.len()) {
            return Err("every stem must hold exactly the bundle range's frames".to_string());
        }
        let capacity = stems.iter().try_fold(0usize, |sum, stem| {
            stem.capacity().checked_mul(std::mem::size_of::<[i16; 2]>()).and_then(|bytes| sum.checked_add(bytes))
        });
        let Some(capacity) = capacity.filter(|capacity| *capacity <= self.bytes) else {
            return Err(format!("the stems occupy more than the {} bytes reserved", self.bytes));
        };
        if capacity < self.bytes {
            self.budget.tracks.fetch_sub(self.bytes - capacity, Ordering::Release);
            self.bytes = capacity;
        }
        Ok(Arc::new(crate::bundle::PreparedStemBundle::new(media, model, sample_rate, range, stems, self)))
    }
}

impl Drop for TrackLease {
    fn drop(&mut self) {
        self.budget.tracks.fetch_sub(self.bytes, Ordering::Release);
    }
}

/// An immutable clip with its budget lease. Shared between the caller (who
/// may keep it to send again) and the engine; the engine's reference is
/// never the last one dropped on the callback: it is returned first.
pub struct DeckTrack {
    /// Private: what `finish` validated (rate, frames, capacity) must stay
    /// so; `Arc::get_mut` on a sole owner could otherwise change it.
    pcm: TrackPcm,
    stems: Option<[Arc<DeckTrack>; 4]>,
    /// The deck sequence of the clip this one continues (same audio, same
    /// rate and length): it takes over at that clip's position.
    continues: Option<u64>,
    _lease: TrackLease,
}

impl DeckTrack {
    pub fn pcm(&self) -> &TrackPcm {
        &self.pcm
    }

    pub fn seconds(&self) -> f64 {
        self.pcm.seconds()
    }

    pub fn has_stems(&self) -> bool { self.stems.is_some() }

    pub fn stems(&self) -> Option<&[Arc<DeckTrack>; 4]> { self.stems.as_ref() }

    /// Reserve another clip on this clip's device budget before decoding it
    /// (the stems that follow a clip already handed to the deck).
    pub fn reserve_related(&self, bytes: usize) -> Result<TrackLease, String> {
        self._lease.reserve_related(bytes)
    }

    /// Whether the clip's lease is on `budget`: a clip leased from another
    /// device's budget is refused, so no budget is bypassed through it.
    pub(crate) fn leased_from(&self, budget: &Arc<AudioBudget>) -> bool {
        Arc::ptr_eq(&self._lease.budget, budget)
    }
}

/// What a deck's `source` endpoint takes, as the payload of an
/// `OwnerCommand` for `stage.audio`. `sequence` is the caller's, strictly
/// increasing per deck; acknowledgements name it.
pub enum DeckCommand {
    Load { sequence: u64, track: Arc<DeckTrack> },
    /// A paged source prepared on this device. `sequence` must equal the
    /// source key's `source_sequence`. See [`crate::paged`] for what a
    /// paged source does at this checkpoint (it is not rendered yet).
    LoadPaged { sequence: u64, source: Box<PagedSource> },
    /// A feed ([`crate::feed`]): frames a writer mixes ahead. `ramp`:
    /// starts and stops dip (live); without, the frames play as written.
    LoadFeed { sequence: u64, ring: Arc<crate::feed::FeedRing>, ramp: bool },
    Eject { sequence: u64 },
}

impl DeckCommand {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Load { sequence, .. } | Self::LoadPaged { sequence, .. } | Self::LoadFeed { sequence, .. } | Self::Eject { sequence } => *sequence,
        }
    }
}

/// What a deck plays: a contiguous clip or a paged source. Moved whole; the
/// callback never clones or drops either.
pub enum DeckSource {
    Clip(Arc<DeckTrack>),
    Paged(Box<PagedSource>),
    Feed(Box<crate::feed::FeedSource>),
}

impl DeckSource {
    pub fn clip(&self) -> Option<&Arc<DeckTrack>> {
        match self { Self::Clip(track) => Some(track), _ => None }
    }

    pub fn paged(&self) -> Option<&PagedSource> {
        match self { Self::Paged(source) => Some(source), _ => None }
    }
}

/// What a deck keeps between passes. Moved whole between generations, so
/// carrying it over allocates and frees nothing; `position` is in source
/// frames, so it survives a device-rate change unconverted.
pub struct DeckState {
    pub track: Option<DeckSource>,
    /// The sequence of the loaded source, `0` for none.
    pub sequence: u64,
    /// A load (`Some`) or eject (`None`) waiting for the dip to reach silence.
    pub next: Option<(u64, Option<DeckSource>)>,
    /// A contiguous-clip cue waiting for the dip: `(clip sequence, seconds)`.
    /// Pinned to the clip accepted when it was sent; it never applies to a
    /// source loaded after it. Paged sources take credited seeks instead.
    pub seek: Option<(u64, f64)>,
    /// Source frames.
    pub position: f64,
    pub gain: f32,
    /// Accepted cues on this deck, for observations.
    pub seek_epoch: u32,
    /// What the clip renderer itself did, for observations (paged sources
    /// keep their own); reset with every source swap.
    pub clip: ClipTelemetry,
    /// The record under the hand: transport, scratch or the release back.
    pub motion: Motion,
    /// A swap with another deck waiting for both dips to reach silence,
    /// by its serial; the deck is busy (silent, no hand) until it applies.
    pub exchange: Option<u64>,
}

/// Whether the transport, a hand or the release after it moves the record.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum MotionState {
    /// Play and speed decide, exactly as without scratching.
    #[default]
    Transport,
    /// The hand holds the record: the position follows `scratch_at`, plus
    /// the source frames between the head and the hand when it took hold.
    Scratch { offset: f64 },
    /// The hand let go: the speed goes from where the hand left it to the
    /// transport's (or to rest) in `left` more output frames.
    Release { left: u32 },
    /// The hand let go with a fling: the record runs on from the fling's
    /// speed and eases to the transport's (or to rest), see `COAST_SECONDS`.
    Coast,
}

/// The record's motion between passes. `velocity` is signed source frames
/// per output frame, the step last applied. `level` is the hand's own
/// envelope (1 while the record moves, fading to 0 while it stands still);
/// `loop_suspended` holds the loop off from the first touch until the deck
/// is back inside it (or folded into it under a dip).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub state: MotionState,
    pub velocity: f64,
    pub level: f32,
    pub loop_suspended: bool,
    /// A braking touch: the speed the record had when touched, the output
    /// frames since, the hand's place then and the brake's speed now (the
    /// hand's anchor runs on at it). All 0 without a brake.
    pub brake_from: f64,
    pub brake_frames: u32,
    pub brake_hand: f32,
    pub brake: f64,
    /// Time-stretched (key lock, key shift): the grains around the head.
    pub grains: Option<Grains>,
}

impl Default for Motion {
    fn default() -> Self {
        Self { state: MotionState::Transport, velocity: 0.0, level: 1.0, loop_suspended: false, brake_from: 0.0, brake_frames: 0, brake_hand: 0.0, brake: 0.0, grains: None }
    }
}

impl Motion {
    /// One frame of the hand's level after the record moved `moved` source
    /// frames, at `nominal` source frames per output frame for speed 1.
    pub(crate) fn follow_level(&mut self, moved: f64, nominal: f64) -> f32 {
        // A continuous curve from standstill (exactly 0) to full level at
        // `STANDSTILL_SHARE` of the source's speed: slow hands fade, never gate.
        let target = (moved.abs() / (STANDSTILL_SHARE * nominal).max(f64::MIN_POSITIVE)).min(1.0) as f32;
        let step = 1.0 / DECK_DIP_FRAMES as f32;
        self.level = if self.level < target { (self.level + step).min(target) } else { (self.level - step).max(target) };
        self.level
    }

    /// Under the transport: the level returns to 1 only while the record
    /// sounds from the dip (a paused deck keeps whatever it faded to).
    pub(crate) fn level_up(&mut self) -> f32 {
        self.level = (self.level + 1.0 / DECK_DIP_FRAMES as f32).min(1.0);
        self.level
    }
}

/// What one pass's motion plan decided.
pub(crate) struct MotionPlan {
    /// The dip's target: 1 sounds, 0 fades out.
    pub target: f32,
    /// Whether the position advances this pass at all.
    pub moves: bool,
    /// `Some(step)`: every frame advances by that same step, the transport's
    /// exact arithmetic. `None`: `steps[..frames]` hold each frame's step.
    pub constant: Option<f64>,
}

/// Plan one pass of motion. `position` is the head in source frames,
/// `nominal` the source frames one output frame is at speed 1, `busy` a
/// load, eject or cue waiting for the dip. Writes each frame's signed step
/// into `steps` unless the plan is constant. Pure arithmetic, no allocation.
/// `end` is the clip's last frame (`f64::INFINITY` where unknown).
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_motion(motion: &mut Motion, position: f64, end: f64, controls: &DeckControls, nominal: f64, source_rate: f64, device_rate: f32, busy: bool, frames: usize, steps: &mut [f64]) -> MotionPlan {
    let transport = nominal * signed_speed(controls.speed);
    let running = controls.play && !busy;
    let device_rate_f = device_rate.max(1.0) as f64;
    let release_frames = (SCRATCH_RELEASE_SECONDS * device_rate_f).round().max(1.0) as u32;
    let limit = scratch_step_limit(nominal);
    // The most a free record may move per frame here and still stop by an
    // end: a constant deceleration over the room left.
    let decel = END_DECEL * nominal / device_rate_f;
    let soft = |x: f64, v: f64| -> f64 {
        let room = if v < 0.0 { x } else { end - x };
        let most = (2.0 * decel * room.max(0.0)).sqrt();
        if v.abs() > most { most.copysign(v) } else { v }
    };
    // The hand takes and lets go of the record between passes.
    match (motion.state, controls.scratch && !busy) {
        (MotionState::Scratch { .. }, true) => {}
        (_, true) => {
            let hand = controls.scratch_at.max(0.0) as f64 * source_rate;
            motion.state = MotionState::Scratch { offset: position - hand };
            // A braking touch keeps the record's speed and brakes it; the
            // hand's anchor runs on with the brake.
            let from = if controls.brake { motion.velocity } else { 0.0 };
            motion.brake_from = from;
            motion.brake = from;
            motion.brake_frames = 0;
            motion.brake_hand = controls.scratch_at;
            // The loop steps aside from the first touch.
            motion.loop_suspended = true;
            // A still (paused) record stays silent until the hand moves it; a
            // moving one keeps sounding, so neither engagement pulses.
            if motion.velocity == 0.0 {
                motion.level = 0.0;
            }
        }
        (MotionState::Scratch { .. }, false) => {
            motion.brake = 0.0;
            motion.brake_from = 0.0;
            motion.state = if controls.fling != 0.0 {
                motion.velocity = (controls.fling as f64 * nominal).clamp(-limit, limit);
                MotionState::Coast
            } else {
                MotionState::Release { left: release_frames }
            };
        }
        (_, false) => {}
    }
    match motion.state {
        MotionState::Transport if transport < 0.0 => {
            // Backwards the record moves freely, and stops softly at the start.
            let mut x = position;
            let mut v = 0.0;
            for step in steps.iter_mut().take(frames) {
                v = if running { soft(x, transport) } else { 0.0 };
                *step = v;
                x += v;
            }
            motion.velocity = v;
            MotionPlan { target: if running { 1.0 } else { 0.0 }, moves: running, constant: None }
        }
        MotionState::Transport => {
            motion.velocity = if running { transport } else { 0.0 };
            MotionPlan { target: if running { 1.0 } else { 0.0 }, moves: running, constant: Some(transport) }
        }
        MotionState::Scratch { mut offset } => {
            let hand = controls.scratch_at.max(0.0) as f64 * source_rate;
            let w = std::f64::consts::TAU * SCRATCH_FOLLOW_HZ / device_rate_f;
            let brake_frames = (BRAKE_SECONDS * device_rate_f).max(1.0);
            let handover = 1.0 / (BRAKE_HANDOVER_SECONDS * device_rate_f).max(1.0);
            // Once the hand moves, it has the record: the brake hands over.
            if controls.scratch_at != motion.brake_hand {
                motion.brake_from = 0.0;
            }
            let (mut x, mut v) = (position, motion.velocity);
            for step in steps.iter_mut().take(frames) {
                if motion.brake != 0.0 {
                    if motion.brake_from == 0.0 {
                        motion.brake -= motion.brake * handover;
                    } else {
                        motion.brake_frames = motion.brake_frames.saturating_add(1);
                        let s = (1.0 - motion.brake_frames as f64 / brake_frames).max(0.0);
                        motion.brake = motion.brake_from * s * s;
                    }
                    if motion.brake.abs() < 1e-9 {
                        motion.brake = 0.0;
                    }
                    offset += motion.brake;
                }
                let goal = hand + offset;
                let d = goal - x;
                v = if d.abs() < SETTLE_FRAMES && v.abs() < SETTLE_SPEED {
                    // Land exactly: the next frames stand still at zero.
                    d
                } else {
                    (v + w * w * d - 2.0 * w * v).clamp(-limit, limit)
                };
                if v.abs() < 1e-12 {
                    v = 0.0;
                }
                *step = v;
                x += v;
            }
            motion.velocity = v;
            motion.state = MotionState::Scratch { offset };
            MotionPlan { target: 1.0, moves: true, constant: None }
        }
        MotionState::Coast => {
            let rest = if running { transport } else { 0.0 };
            let k = 1.0 - (-1.0 / (COAST_SECONDS * device_rate_f)).exp();
            let slew = COAST_MIN_SLEW * nominal / device_rate_f;
            let (mut x, mut v) = (position, motion.velocity);
            for step in steps.iter_mut().take(frames) {
                let d = rest - v;
                let by = (d.abs() * k).max(slew);
                v = if by >= d.abs() { rest } else { v + by.copysign(d) };
                v = soft(x, v);
                *step = v;
                x += v;
            }
            motion.velocity = v;
            if v == rest {
                motion.state = MotionState::Transport;
            }
            let spinning = frames == 0 || steps[..frames].iter().any(|step| *step != 0.0);
            let target = if !busy && (running || spinning) { 1.0 } else { 0.0 };
            MotionPlan { target, moves: !busy, constant: None }
        }
        MotionState::Release { mut left } => {
            let rest = if running { transport } else { 0.0 };
            let mut v = motion.velocity;
            // Whatever the hand's speed, the platter reaches `rest` in the
            // release time: a step of the remaining difference per frame.
            for step in steps.iter_mut().take(frames) {
                if left > 0 {
                    v += (rest - v) / left as f64;
                    left -= 1;
                } else {
                    v = rest;
                }
                if (v - rest).abs() < 1e-12 {
                    v = rest;
                }
                *step = v;
            }
            motion.velocity = v;
            motion.state = if left == 0 { MotionState::Transport } else { MotionState::Release { left } };
            // Audible until the platter settled; a paused deck then dips. A
            // load, eject or cue waiting dips at once, as it always did.
            let spinning = frames == 0 || steps[..frames].iter().any(|step| *step != 0.0);
            let target = if !busy && (running || spinning) { 1.0 } else { 0.0 };
            MotionPlan { target, moves: !busy, constant: None }
        }
    }
}

/// Facts of the contiguous-clip renderer, fixed size and `Copy`, written
/// only by a rendered chunk of the loaded clip. The transport plays forward;
/// a hand on the record ([`Motion`]) moves it either way.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClipTelemetry {
    /// A chunk of this clip was rendered since it was loaded or last cued;
    /// until then the motion facts below are not known.
    pub rendered: bool,
    /// Source frames one output frame advanced in the last rendered chunk
    /// (`speed × source_rate / device_rate`), positive; nominal, so it stays
    /// set while paused or ended. Under a hand (or its release) it is the
    /// signed step the last frame applied instead.
    pub source_frames_per_output_frame: f64,
    /// Whether the source clock was advancing at the end of that chunk:
    /// play on, no load, eject or cue pending, and not held at the clip end.
    pub advancing: bool,
    /// `[in, out)` source frames the sampler used for its loop, `None` off.
    pub effective_loop: Option<(u64, u64)>,
    /// Changes whenever the effective loop changes.
    pub loop_epoch: u32,
    /// Playback crossings of the loop OUT (each counted once, however many
    /// one step spans) and reaching the clip end.
    pub boundary_epoch: u32,
}

impl DeckState {
    pub fn fresh() -> Self {
        Self { track: None, sequence: 0, next: None, seek: None, position: 0.0, gain: 0.0, seek_epoch: 0, clip: ClipTelemetry::default(), motion: Motion::default(), exchange: None }
    }

    /// Whether the pending swap or cue may be applied now: the output is
    /// silent, or nothing was sounding.
    pub fn quiet(&self) -> bool {
        self.gain <= 0.0 || self.track.is_none()
    }

    /// Whether a load, eject, cue or swap waits for this deck's dip.
    fn busy(&self) -> bool {
        self.next.is_some() || self.seek.is_some() || self.exchange.is_some()
    }

    /// Apply a due cue. Needs no return slot.
    pub fn apply_seek(&mut self) {
        let Some((sequence, seconds)) = self.seek.take() else { return };
        if sequence != self.sequence { return; }
        if let Some(DeckSource::Clip(track)) = &self.track {
            let frames = track.pcm.frames.len() as f64;
            self.position = (seconds.max(0.0) * track.pcm.sample_rate as f64).min(frames);
            self.seek_epoch = self.seek_epoch.wrapping_add(1);
            // Motion is known again only from the next rendered chunk.
            self.clip.rendered = false;
            // A hand still on the record takes hold again at the new place.
            self.motion = Motion::default();
        }
    }

    /// Apply the due swap: the old track comes out for the return ring.
    /// Returns `(sequence of the command, old sequence, old track)`.
    pub fn apply_swap(&mut self) -> Option<(u64, u64, Option<DeckSource>)> {
        let (sequence, track) = self.next.take()?;
        let old = std::mem::replace(&mut self.track, track);
        let old_sequence = std::mem::replace(&mut self.sequence, sequence);
        // A paged source starts at its validated prepared initial cursor
        // (`start_frame`); a clip at 0 unless it continues the one it replaces.
        self.position = match (&self.track, &old) {
            (Some(DeckSource::Paged(source)), _) => source.start_frame() as f64,
            // The same audio again, now with its stems: it plays on from
            // where the clip it continues was, so the swap is only the dip.
            (Some(DeckSource::Clip(new)), Some(DeckSource::Clip(was)))
                if new.continues == Some(old_sequence) && new.pcm.sample_rate == was.pcm.sample_rate && new.pcm.frames.len() == was.pcm.frames.len() =>
            {
                self.position
            }
            _ => 0.0,
        };
        self.seek = None;
        // Nothing of the old source's rendering belongs to the new one.
        self.clip = ClipTelemetry::default();
        self.motion = Motion::default();
        Some((sequence, old_sequence, old))
    }
}

/// Controls of one pass, already read from the bank.
#[derive(Clone, Copy)]
pub struct DeckControls {
    pub play: bool,
    pub speed: f32,
    pub loop_in: f32,
    pub loop_out: f32,
    pub loop_enabled: bool,
    /// The hand is on the record: the head follows `scratch_at`.
    pub scratch: bool,
    /// Where the hand holds the record, in seconds of the source.
    pub scratch_at: f32,
    /// A touch brakes the record (over `BRAKE_SECONDS`) rather than
    /// stopping it dead under the hand.
    pub brake: bool,
    /// The release flings the record at this signed speed (multiples of
    /// the source's own); 0: the plain release.
    pub fling: f32,
    /// Key shift in semitones (pitch without tempo).
    pub key: f32,
    /// Speed changes keep the key (the tempo moves, the pitch does not).
    pub keylock: bool,
}

/// How far the pitch stands from the head's rate under `controls`: 1 plays
/// varispeed; otherwise the grains read `ratio` times the head's step.
pub fn pitch_ratio(controls: &DeckControls) -> f64 {
    let key = if controls.key.is_finite() { (controls.key as f64).clamp(-KEY_SHIFT_MAX, KEY_SHIFT_MAX) } else { 0.0 };
    let shift = (key / 12.0).exp2();
    if controls.keylock { shift / signed_speed(controls.speed).abs() } else { shift }
}

/// The grains for this pass of a transport moving `advance` source frames a
/// frame: kept from the last pass, started at `head` when the deck begins to
/// stretch, gone when it plays varispeed (`None`, with the grains' step).
pub(crate) fn stretch(grains: &mut Option<Grains>, controls: &DeckControls, advance: Option<f64>, head: f64, source_rate: f64) -> Option<f64> {
    let ratio = pitch_ratio(controls);
    let step = match advance {
        Some(advance) if advance != 0.0 && (ratio - 1.0).abs() > STRETCH_EPSILON => advance * ratio,
        _ => {
            *grains = None;
            return None;
        }
    };
    grains.get_or_insert_with(|| Grains::new(STRETCH_GRAIN_SECONDS * source_rate, head, step));
    Some(step)
}

/// The transport's speed with its sign (negative: backwards), its size
/// within `SPEED_MIN..=SPEED_MAX`.
pub fn signed_speed(speed: f32) -> f64 {
    let size = speed.abs().clamp(SPEED_MIN as f32, SPEED_MAX as f32) as f64;
    if speed < 0.0 { -size } else { size }
}

/// One pass: `frames` stereo frames into `left`/`right` at `rate`. Returns
/// `(position seconds, end seconds)` for the probes. No allocation.
#[relax(O3, always)]
pub fn render(state: &mut DeckState, controls: DeckControls, rate: f32, frames: usize, left: &mut [f32], right: &mut [f32]) -> (f32, f32) {
    render_with(state, controls, rate, frames, |frame, values| {
        left[frame] = values[0][0];
        right[frame] = values[0][1];
    })
}

/// Five contiguous stereo spans: mixed fallback, vocals, drums, bass, other.
/// Every stem follows the mixed track's single source-time playhead and dip.
#[relax(O3, always)]
pub fn render_bundle(state: &mut DeckState, controls: DeckControls, rate: f32, frames: usize, out: &mut [f32]) -> (f32, f32) {
    render_with(state, controls, rate, frames, |frame, values| {
        for (lane, value) in values.iter().enumerate() {
            let at = lane * crate::schedule::SPAN;
            out[at + frame] = value[0];
            out[at + crate::CHUNK + frame] = value[1];
        }
    })
}

#[relax(O3, always)]
fn render_with(state: &mut DeckState, controls: DeckControls, rate: f32, frames: usize, mut write: impl FnMut(usize, [[f32; 2]; 5])) -> (f32, f32) {
    // Paged sampling: the mix, and the four stems for chunks their installed
    // bundles cover whole (see `crate::paged`). A load, eject or clip cue
    // waiting dips it, as for a clip.
    let busy = state.busy();
    if let DeckState { track: Some(DeckSource::Paged(source)), position, gain, motion, .. } = state {
        return source.render(position, gain, motion, busy, controls, rate, frames, &mut write);
    }
    // A feed: what its writer mixed, under the play control and the dip.
    if let DeckState { track: Some(DeckSource::Feed(feed)), position, gain, .. } = state {
        let frames = frames.min(crate::CHUNK);
        feed.render(gain, controls.play && !busy, rate, frames, |frame, value| {
            let mut values = [[0.0; 2]; 5];
            values[0] = value;
            write(frame, values);
        });
        *position = feed.seconds() * crate::feed::FEED_RATE as f64;
        return ((feed.seconds()) as f32, 0.0);
    }
    let step = 1.0 / DECK_DIP_FRAMES as f32;
    let track = match state.track.as_ref() {
        Some(DeckSource::Clip(track)) => track,
        // Rendered above; a paged source or a feed never reaches this match.
        Some(DeckSource::Paged(_) | DeckSource::Feed(_)) | None => {
            for frame in 0..frames { write(frame, [[0.0; 2]; 5]); }
            state.gain = 0.0;
            return (0.0, 0.0);
        }
    };
    let pcm = &track.pcm;
    let len = pcm.frames.len();
    let source_rate = pcm.sample_rate as f64;
    let nominal = source_rate / rate as f64;
    let mut steps = [0.0f64; crate::CHUNK];
    let frames = frames.min(crate::CHUNK);
    let plan = plan_motion(&mut state.motion, state.position, len.saturating_sub(1) as f64, &controls, nominal, source_rate, rate, busy, frames, &mut steps);
    let mut target = plan.target;
    // A hand or a release moves the record freely: the ends stop it, and the
    // loop is suspended until the deck is back inside it.
    let free = plan.constant.is_none();
    let advance = plan.constant.unwrap_or(0.0);
    // The loop is one integer interval of source frames, `[start, end)`,
    // used both to wrap the position and to wrap the interpolation
    // neighbour, so what is heard is exactly that interval. An end at or
    // before the start, or an interval shorter than two frames, is "to
    // the end of the clip".
    let start = ((controls.loop_in.max(0.0) as f64) * source_rate).floor().min(len as f64) as usize;
    let mut end = ((controls.loop_out.max(0.0) as f64) * source_rate).ceil().min(len as f64) as usize;
    if end < start + 2 {
        end = len;
    }
    let wanted = controls.loop_enabled && end >= start + 2;
    let (start_f, end_f) = (start as f64, end as f64);
    if !free && state.motion.loop_suspended {
        if !wanted || (state.position >= start_f && state.position < end_f) {
            state.motion.loop_suspended = false;
        } else if state.gain <= 0.0 {
            // Silent now: fold into the loop, and fade in from there.
            state.position = start_f + (state.position - start_f).rem_euclid(end_f - start_f);
            state.motion.loop_suspended = false;
        } else {
            // Outside the loop after a scratch: dip before folding, as a cue.
            target = 0.0;
        }
    }
    let looping = wanted && !state.motion.loop_suspended;
    let stem_bounds: [(f64, usize, usize); 4] = std::array::from_fn(|lane| {
        let Some(stems) = &track.stems else { return (1.0, 0, 0) };
        let stem = &stems[lane].pcm;
        let ratio = stem.sample_rate as f64 / source_rate;
        // Preserve the mixed loop's time boundary even if codec padding made
        // this stem shorter. Missing tail samples are silence, not an early wrap.
        (ratio, (start_f * ratio).floor() as usize, (end_f * ratio).ceil() as usize)
    });
    let wrap = |position: f64| -> f64 {
        if looping {
            // Into `[start, end)` from anywhere: a cue outside the loop, a
            // loop enabled around a position, or several spans overshot.
            start_f + (position - start_f).rem_euclid(end_f - start_f)
        } else {
            position.clamp(0.0, len as f64)
        }
    };
    // Under a hand the record stays on real frames: first to last sample,
    // so an end is a standstill that fades, never a jump to silence.
    let last = len.saturating_sub(1) as f64;
    let mut gain = state.gain;
    let mut position = if free { state.position.clamp(0.0, last) } else { wrap(state.position) };
    let moves = plan.moves && target > 0.0;
    // Key lock or a key shift: the head moves at the transport's step, the
    // grains read at the pitch's. A hand is varispeed either way.
    let grain_step = stretch(&mut state.motion.grains, &controls, plan.constant.filter(|_| moves), position, source_rate);
    let mut grains = state.motion.grains.take();
    // A grain's read point onto real frames: into the loop, or the clip.
    let place = |x: f64| if looping { start_f + (x - start_f).rem_euclid(end_f - start_f) } else { x.clamp(0.0, last) };
    let mut crossings = 0u32;
    let mut last_step = advance;
    for frame in 0..frames {
        gain = if gain < target { (gain + step).min(target) } else { (gain - step).max(target) };
        // The hand's own envelope, 1 whenever the transport moves the record.
        let level = if !free && moves { state.motion.level_up() } else { state.motion.level };
        let heard = gain * level;
        let mut values = [[0.0; 2]; 5];
        if let (Some(grains), Some(grain_step), true) = (grains.as_mut(), grain_step, position < len as f64) {
            // Every tap is read either way: the grains move on through silence.
            let mono = |x: f64| {
                let value = sample(pcm, place(x), looping, start, end, 1.0);
                (value[0] + value[1]) * 0.5
            };
            for (x, weight) in grains.next_aligned(position, grain_step, STRETCH_ALIGN_SECONDS * source_rate, mono) {
                let (x, weight) = (place(x), heard * weight);
                let value = sample(pcm, x, looping, start, end, weight);
                values[0] = [values[0][0] + value[0], values[0][1] + value[1]];
                if let Some(stems) = &track.stems {
                    for (lane, stem) in stems.iter().enumerate() {
                        let (ratio, start, end) = stem_bounds[lane];
                        let value = sample(&stem.pcm, x * ratio, looping, start, end, weight);
                        values[lane + 1] = [values[lane + 1][0] + value[0], values[lane + 1][1] + value[1]];
                    }
                }
            }
        } else if heard > 0.0 && position < len as f64 {
            values[0] = sample(pcm, position, looping, start, end, heard);
            if let Some(stems) = &track.stems {
                for (lane, stem) in stems.iter().enumerate() {
                    let (ratio, start, end) = stem_bounds[lane];
                    values[lane + 1] = sample(&stem.pcm, position * ratio, looping, start, end, heard);
                }
            }
        }
        write(frame, values);
        if moves && free {
            // Either way, stopped exactly on the clip's first and last frames.
            last_step = steps[frame];
            let next = (position + last_step).clamp(0.0, last);
            if next != position + last_step {
                state.motion.velocity = 0.0;
            }
            state.motion.follow_level(next - position, nominal);
            position = next;
        } else if free {
            state.motion.follow_level(0.0, nominal);
        } else if moves {
            let next = position + advance;
            // Counted around the unchanged step: each playback crossing of
            // the loop OUT once, and the clip end once when reached.
            let crossed = if looping {
                if next >= end_f { ((next - start_f) / (end_f - start_f)).floor().min(u32::MAX as f64) as u32 } else { 0 }
            } else {
                u32::from(position < len as f64 && next >= len as f64)
            };
            crossings = crossings.saturating_add(crossed);
            position = wrap(next);
        }
    }
    state.gain = gain;
    state.position = position;
    state.motion.grains = grains;
    if frames > 0 {
        let effective = looping.then_some((start as u64, end as u64));
        let telemetry = &mut state.clip;
        if telemetry.effective_loop != effective {
            telemetry.loop_epoch = telemetry.loop_epoch.wrapping_add(1);
        }
        telemetry.effective_loop = effective;
        telemetry.boundary_epoch = telemetry.boundary_epoch.wrapping_add(crossings);
        if free {
            // Under the hand: the signed step the last frame applied.
            telemetry.source_frames_per_output_frame = last_step;
            telemetry.advancing = moves && last_step != 0.0 && state.motion.velocity != 0.0;
        } else {
            telemetry.source_frames_per_output_frame = advance;
            telemetry.advancing = moves && (looping || position < len as f64);
        }
        telemetry.rendered = true;
    }
    ((position / source_rate) as f32, pcm.seconds() as f32)
}

fn sample(pcm: &TrackPcm, position: f64, looping: bool, start: usize, end: usize, gain: f32) -> [f32; 2] {
    let index = position as usize;
    if index >= pcm.frames.len() { return [0.0; 2]; }
    let frac = (position - index as f64) as f32;
    let neighbour = if looping && end > start && index + 1 >= end && start < pcm.frames.len() { start }
        else if index + 1 >= pcm.frames.len() { index } else { index + 1 };
    let a = pcm.frame_f32(index);
    let b = pcm.frame_f32(neighbour);
    [(a[0] + (b[0] - a[0]) * frac) * gain, (a[1] + (b[1] - a[1]) * frac) * gain]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CHUNK;

    const RATE: f32 = 48_000.0;

    fn clip(frames: Vec<[i16; 2]>) -> DeckState {
        let budget = Arc::new(AudioBudget::default());
        let bytes = frames.capacity() * std::mem::size_of::<[i16; 2]>();
        let track = TrackLease::reserve(&budget, bytes).unwrap().finish(TrackPcm { frames, sample_rate: 48_000 }).unwrap();
        let mut state = DeckState::fresh();
        state.track = Some(DeckSource::Clip(track));
        state.sequence = 1;
        state
    }

    fn ramp(len: usize) -> Vec<[i16; 2]> {
        (0..len).map(|i| { let v = (i % 30_000) as i16; [v, -v] }).collect()
    }

    fn sine(len: usize, hz: f64) -> Vec<[i16; 2]> {
        (0..len).map(|i| { let v = ((i as f64 * hz / 48_000.0 * std::f64::consts::TAU).sin() * 16_000.0) as i16; [v, v] }).collect()
    }

    fn controls(play: bool, scratch: bool, scratch_at: f32) -> DeckControls {
        DeckControls { play, speed: 1.0, loop_in: 0.0, loop_out: 0.0, loop_enabled: false, scratch, scratch_at, brake: false, fling: 0.0, key: 0.0, keylock: false }
    }

    /// Render one chunk, returning the left channel.
    fn chunk(state: &mut DeckState, controls: DeckControls) -> Vec<f32> {
        let (mut left, mut right) = (vec![0.0; CHUNK], vec![0.0; CHUNK]);
        render(state, controls, RATE, CHUNK, &mut left, &mut right);
        left
    }

    #[test]
    fn a_clip_that_continues_the_playing_one_takes_over_at_its_position() {
        let budget = Arc::new(AudioBudget::default());
        let finish = |continues: Option<u64>, len: usize| {
            let frames = ramp(len);
            let bytes = frames.capacity() * std::mem::size_of::<[i16; 2]>();
            TrackLease::reserve(&budget, bytes).unwrap().finish_continuing(TrackPcm { frames, sample_rate: 48_000 }, None, continues).unwrap()
        };
        let mut state = clip(ramp(48_000));
        state.position = 12_345.0;
        // The same audio, continuing sequence 1: plays on from there.
        state.next = Some((2, Some(DeckSource::Clip(finish(Some(1), 48_000)))));
        assert_eq!(state.apply_swap().map(|(sequence, old, _)| (sequence, old)), Some((2, 1)));
        assert_eq!(state.position, 12_345.0);
        // Continuing another sequence, or of another length: from the start.
        state.next = Some((3, Some(DeckSource::Clip(finish(Some(1), 48_000)))));
        state.apply_swap();
        assert_eq!(state.position, 0.0);
        state.position = 500.0;
        state.next = Some((4, Some(DeckSource::Clip(finish(Some(3), 47_000)))));
        state.apply_swap();
        assert_eq!(state.position, 0.0);
    }

    #[test]
    fn transport_without_a_hand_is_the_old_arithmetic() {
        let mut state = clip(ramp(48_000));
        state.gain = 1.0;
        state.position = 100.25;
        chunk(&mut state, controls(true, false, 0.0));
        assert_eq!(state.position, 100.25 + CHUNK as f64);
        assert_eq!(state.motion.state, MotionState::Transport);
        assert_eq!(state.clip.source_frames_per_output_frame, 1.0);
    }

    #[test]
    fn a_hand_plays_backwards_sample_exact() {
        let mut state = clip(ramp(48_000));
        state.gain = 1.0;
        state.position = 24_000.0;
        let pcm = state.track.as_ref().and_then(DeckSource::clip).unwrap().pcm().frames.clone();
        let pcm = TrackPcm { frames: pcm, sample_rate: 48_000 };
        // The hand pulls the record half a second back over a few chunks.
        let mut hand = 0.5f32;
        let mut reversed = false;
        // The hand's level, modelled independently of the renderer: a paused
        // record taken by the hand starts silent.
        let mut envelope = Motion { level: 0.0, ..Motion::default() };
        for pass in 0..40 {
            if pass < 10 { hand -= 0.05; }
            let before = state.position;
            let mut steps = [0.0; CHUNK];
            let c = controls(false, true, hand);
            // The reference path, planned independently of the render.
            let mut probe = state.motion;
            if pass == 0 { probe = Motion::default(); }
            plan_motion(&mut probe, before, (pcm.frames.len() - 1) as f64, &c, 1.0, 48_000.0, RATE, false, CHUNK, &mut steps);
            let out = chunk(&mut state, c);
            let mut at = before;
            let last = (pcm.frames.len() - 1) as f64;
            for frame in 0..CHUNK {
                let expected = sample(&pcm, at, false, 0, pcm.frames.len(), envelope.level)[0];
                assert!((out[frame] - expected).abs() < 1e-6, "pass {pass} frame {frame}: {} vs {expected}", out[frame]);
                let next = (at + steps[frame]).clamp(0.0, last);
                envelope.follow_level(next - at, 1.0);
                at = next;
                reversed |= steps[frame] < 0.0;
            }
            assert!((state.position - at).abs() < 1e-9);
        }
        assert!(reversed, "the hand moved the record backwards");
        // Settled on the hand: it took hold at 0.45 s with the head at 0.5 s,
        // so 0.45 s of travel back leaves the head 2400 frames (0.05 s) in.
        assert!((state.position - 2_400.0).abs() < 48.0, "settled at {}", state.position);
    }

    #[test]
    fn engaging_moving_and_letting_go_never_clicks() {
        // A 110 Hz sine: the largest step at 1x is 2π·110/48000·0.49 ≈ 0.007.
        let mut state = clip(sine(48_000 * 20, 110.0));
        state.position = 48_000.0 * 5.0;
        let mut last = None;
        let mut worst = 0.0f32;
        let mut hand = 5.0f32;
        for pass in 0..600 {
            let (play, scratch) = match pass {
                0..=99 => (true, false),
                100..=399 => (true, true),
                _ => (true, false),
            };
            if (100..400).contains(&pass) {
                // Back and forth, one second each way at 2x.
                let t = (pass - 100) as f32 * CHUNK as f32 / RATE;
                hand = 5.0 + 0.5 + 2.0 * (t % 2.0 - 1.0).abs() - 2.0;
            }
            for value in chunk(&mut state, controls(play, scratch, hand)) {
                if let Some(previous) = last { worst = worst.max((value - previous as f32).abs()); }
                last = Some(value);
            }
        }
        // Worst step allowed: the sine's own slope at the 16x cap.
        let bound = (std::f64::consts::TAU * 110.0 / 48_000.0 * 0.49 * SCRATCH_RATE_MAX) as f32;
        assert!(worst < bound, "largest sample step {worst} exceeds {bound}");
        assert_eq!(state.motion.state, MotionState::Transport, "the release reached the transport");
        assert_eq!(state.motion.velocity, 1.0);
    }

    #[test]
    fn a_paused_deck_scratches_then_brakes_and_dips() {
        let mut state = clip(ramp(48_000 * 4));
        state.position = 48_000.0;
        let mut out = chunk(&mut state, controls(false, true, 1.0));
        assert!(state.gain > 0.0, "a hand sounds on a paused deck");
        for _ in 0..20 { out = chunk(&mut state, controls(false, true, 1.2)); }
        assert!(state.position > 48_000.0 * 1.1, "the record followed the hand forward");
        assert!(out.iter().any(|v| *v != 0.0));
        // Let go on a paused deck: the platter stops, then the dip ends it.
        for _ in 0..200 { chunk(&mut state, controls(false, false, 1.2)); }
        assert_eq!(state.motion.state, MotionState::Transport);
        assert_eq!(state.gain, 0.0);
        let rest = state.position;
        chunk(&mut state, controls(false, false, 1.2));
        assert_eq!(state.position, rest, "a released paused deck holds still");
    }

    #[test]
    fn the_start_stops_a_reverse_hand() {
        let mut state = clip(ramp(48_000));
        state.gain = 1.0;
        state.position = 1_000.0;
        chunk(&mut state, controls(false, true, 1.0));
        for _ in 0..40 { chunk(&mut state, controls(false, true, 0.0)); }
        assert_eq!(state.position, 0.0);
        assert!(state.motion.velocity <= 0.0);
    }

    #[test]
    fn a_cue_resets_the_hand() {
        let mut state = clip(ramp(48_000));
        state.gain = 1.0;
        chunk(&mut state, controls(false, true, 0.1));
        state.seek = Some((1, 0.5));
        state.apply_seek();
        assert_eq!(state.motion, Motion::default());
        assert_eq!(state.position, 24_000.0);
    }

    fn dc(len: usize, value: i16) -> Vec<[i16; 2]> {
        vec![[value, value]; len]
    }

    #[test]
    fn the_release_takes_its_fixed_time_from_any_speed() {
        let mut state = clip(ramp(48_000 * 60));
        state.position = 48_000.0 * 30.0;
        // Fling the record back fast, then let go on a playing deck.
        chunk(&mut state, controls(true, true, 30.0));
        for pass in 1..30 { chunk(&mut state, controls(true, true, 30.0 - pass as f32 * 0.5)); }
        assert!(state.motion.velocity < -10.0, "the hand was fast: {}", state.motion.velocity);
        let release = (SCRATCH_RELEASE_SECONDS * RATE as f64).round() as usize;
        let mut frames = 0;
        while state.motion.state != MotionState::Transport {
            chunk(&mut state, controls(true, false, 0.0));
            frames += CHUNK;
            assert!(frames <= release + CHUNK, "still releasing after {frames} frames");
        }
        assert!(frames >= release, "released in {frames} frames, less than {release}");
        assert_eq!(state.motion.velocity, 1.0);
    }

    #[test]
    fn a_held_record_on_a_nonzero_sample_fades_to_silence_and_stays() {
        let mut state = clip(dc(48_000 * 4, 20_000));
        state.position = 48_000.0;
        state.gain = 1.0;
        let mut last = None;
        let mut worst = 0.0f32;
        let mut tail = Vec::new();
        for pass in 0..400 {
            let out = chunk(&mut state, controls(false, true, 1.0));
            for value in &out {
                if let Some(previous) = last { worst = worst.max((value - previous as f32).abs()); }
                last = Some(*value);
            }
            if pass > 20 { tail.extend(out); }
        }
        // 20000/32768 over the 256-frame envelope is the largest allowed step.
        assert!(worst <= 20_000.0 / 32_768.0 / DECK_DIP_FRAMES as f32 + 1e-6, "step {worst}");
        assert!(tail.iter().all(|value| *value == 0.0), "no DC under a still hand");
        assert_eq!(state.motion.velocity, 0.0, "the spring settled exactly");
        assert_eq!(state.position, 48_000.0);
    }

    #[test]
    fn a_hand_driving_into_a_nonzero_end_fades_instead_of_jumping() {
        let len = 48_000;
        let mut state = clip(dc(len, 20_000));
        state.position = len as f64 - 4_800.0;
        state.gain = 1.0;
        let mut last = None;
        let mut worst = 0.0f32;
        for pass in 0..200 {
            // The hand keeps pulling past the end.
            let hand = 0.9 + pass as f32 * 0.01;
            for value in chunk(&mut state, controls(false, true, hand)) {
                if let Some(previous) = last { worst = worst.max((value - previous as f32).abs()); }
                last = Some(value);
            }
        }
        assert_eq!(state.position, (len - 1) as f64, "stopped on the last real frame");
        assert!(worst <= 20_000.0 / 32_768.0 / DECK_DIP_FRAMES as f32 + 1e-6, "step {worst}");
        assert_eq!(last, Some(0.0), "standing at the end is silent");
    }

    #[test]
    fn the_scratch_cap_stays_below_the_page_planner_for_any_rate_ratio() {
        assert_eq!(scratch_step_limit(1.0), SCRATCH_RATE_MAX);
        let fast = scratch_step_limit(192_000.0 / 44_100.0);
        assert!(fast < makepad_stage_asset_stream::window::MAX_WINDOW_SPEED, "{fast}");
    }

    #[test]
    fn a_scratch_suspends_the_loop_and_folds_back_under_a_dip() {
        let mut state = clip(ramp(48_000 * 10));
        state.position = 48_000.0 * 2.5;
        state.gain = 1.0;
        let looped = |scratch, at| DeckControls { play: true, speed: 1.0, loop_in: 2.0, loop_out: 3.0, loop_enabled: true, scratch, scratch_at: at, brake: false, fling: 0.0, key: 0.0, keylock: false };
        chunk(&mut state, looped(false, 0.0));
        assert!(state.clip.effective_loop.is_some());
        // The hand drags the record past OUT: no wrap, no loop reported.
        chunk(&mut state, looped(true, 2.5));
        for pass in 1..60 { chunk(&mut state, looped(true, 2.5 + pass as f32 * 0.02)); }
        assert!(state.position > 48_000.0 * 3.0, "outside the loop at {}", state.position);
        assert_eq!(state.clip.effective_loop, None);
        // Let go outside the loop: it dips to silence, folds in, fades in.
        let mut silent = false;
        for _ in 0..200 {
            chunk(&mut state, looped(false, 0.0));
            silent |= state.gain == 0.0;
        }
        assert!(silent, "the fold happened under a dip");
        assert!(!state.motion.loop_suspended);
        assert!(state.position >= 48_000.0 * 2.0 && state.position < 48_000.0 * 3.0);
        assert!(state.clip.effective_loop.is_some());
    }


    #[test]
    fn engaging_a_paused_record_makes_no_pulse() {
        // A held nonzero sample under a paused deck: the hand takes hold and
        // keeps still for a while, then moves.
        let mut state = clip(dc(48_000 * 4, 20_000));
        state.position = 48_000.0;
        for _ in 0..4 { chunk(&mut state, controls(false, false, 0.0)); }
        assert_eq!(state.gain, 0.0);
        let mut loudest = 0.0f32;
        for _ in 0..40 {
            for value in chunk(&mut state, controls(false, true, 1.0)) { loudest = loudest.max(value.abs()); }
        }
        assert_eq!(loudest, 0.0, "a still hand on a paused record is silent");
    }

    #[test]
    fn a_slow_hand_fades_without_gating() {
        let mut motion = Motion { level: 0.0, ..Motion::default() };
        // A ripple around 2% of the source speed.
        let mut low = f32::MAX;
        let mut high = 0.0f32;
        for frame in 0..48_000 {
            let moved = if frame % 800 < 400 { 0.019 } else { 0.021 };
            let level = motion.follow_level(moved, 1.0);
            if frame > 4_800 { low = low.min(level); high = high.max(level); }
        }
        assert!(high - low < 0.1, "slow speed ripple stays small in level: {low}..{high}");
        assert!(low > 0.3, "a slow hand is heard, attenuated: {low}");
        assert_eq!(motion.follow_level(0.0, 1.0) < high, true);
    }

    fn hand(play: bool, scratch: bool, scratch_at: f32, brake: bool, fling: f32, speed: f32) -> DeckControls {
        DeckControls { play, speed, loop_in: 0.0, loop_out: 0.0, loop_enabled: false, scratch, scratch_at, brake, fling, key: 0.0, keylock: false }
    }

    /// Chunks for `seconds`, the largest sample-to-sample step of the left
    /// channel across them (continuing from `last`).
    fn run(state: &mut DeckState, c: DeckControls, seconds: f64, last: &mut Option<f32>) -> f32 {
        let mut worst = 0.0f32;
        for _ in 0..(seconds * RATE as f64 / CHUNK as f64).round() as usize {
            for value in chunk(state, c) {
                if let Some(previous) = *last { worst = worst.max((value - previous).abs()); }
                *last = Some(value);
            }
        }
        worst
    }

    #[test]
    fn a_braking_touch_slows_the_record_to_a_stop() {
        let mut state = clip(sine(48_000 * 20, 110.0));
        state.position = 48_000.0 * 5.0;
        let mut last = None;
        run(&mut state, hand(true, false, 0.0, true, 0.0, 1.0), 0.5, &mut last);
        let touched = state.position;
        // Touched and held still: the record runs on, slowing, for the brake.
        let worst = run(&mut state, hand(true, true, 3.0, true, 0.0, 1.0), 0.1, &mut last);
        let v = state.motion.velocity;
        assert!(v > 0.25 && v < 0.6, "braking at 0.1 s: {v}");
        run(&mut state, hand(true, true, 3.0, true, 0.0, 1.0), 0.4, &mut last);
        assert!(state.motion.velocity.abs() < 1e-3, "stopped: {}", state.motion.velocity);
        // A (1 - t/T)^2 brake from 1x covers T/3 of a second.
        let travelled = (state.position - touched) / 48_000.0;
        assert!((travelled - BRAKE_SECONDS / 3.0).abs() < 0.02, "a brake of {travelled} s");
        let bound = (std::f64::consts::TAU * 110.0 / 48_000.0 * 0.49 * 1.2) as f32;
        assert!(worst < bound, "the brake clicked: {worst}");
        // Without `brake`, the same touch takes hold where the record is.
        let mut plain = clip(sine(48_000 * 20, 110.0));
        plain.position = 48_000.0 * 5.0;
        let mut last = None;
        run(&mut plain, hand(true, false, 0.0, false, 0.0, 1.0), 0.5, &mut last);
        let at = plain.position;
        run(&mut plain, hand(true, true, 3.0, false, 0.0, 1.0), 0.5, &mut last);
        assert!((plain.position - at).abs() < 48_000.0 * 0.02, "the plain hand holds near the touch: {}", (plain.position - at) / 48_000.0);
    }

    #[test]
    fn a_moving_hand_takes_over_from_the_brake() {
        let mut state = clip(sine(48_000 * 20, 110.0));
        state.position = 48_000.0 * 5.0;
        let mut last = None;
        run(&mut state, hand(true, false, 0.0, true, 0.0, 1.0), 0.5, &mut last);
        run(&mut state, hand(true, true, 3.0, true, 0.0, 1.0), 0.05, &mut last);
        let at = state.position;
        // The hand drags one second back over half a second.
        for i in 1..=30 {
            run(&mut state, hand(true, true, 3.0 - i as f32 / 30.0, true, 0.0, 1.0), 0.5 / 30.0, &mut last);
        }
        run(&mut state, hand(true, true, 2.0, true, 0.0, 1.0), 0.3, &mut last);
        assert_eq!(state.motion.brake, 0.0);
        let moved = (state.position - at) / 48_000.0;
        assert!((moved + 1.0).abs() < 0.05, "the record followed the hand back: {moved}");
    }

    #[test]
    fn a_fling_eases_to_the_transport_either_way() {
        let mut state = clip(sine(48_000 * 60, 110.0));
        state.position = 48_000.0 * 30.0;
        let mut last = None;
        run(&mut state, hand(true, true, 0.0, true, 0.0, 1.0), 0.4, &mut last);
        // Flung backwards: the transport plays backwards at 1x.
        let worst = run(&mut state, hand(true, false, 0.0, true, -8.0, -1.0), 0.1, &mut last);
        assert_eq!(state.motion.state, MotionState::Coast);
        assert!(state.motion.velocity < -5.0, "coasting: {}", state.motion.velocity);
        run(&mut state, hand(true, false, 0.0, true, -8.0, -1.0), 6.0, &mut last);
        assert_eq!(state.motion.state, MotionState::Transport);
        assert_eq!(state.motion.velocity, -1.0);
        let bound = (std::f64::consts::TAU * 110.0 / 48_000.0 * 0.49 * 8.5) as f32;
        assert!(worst < bound, "the fling clicked: {worst}");
        // Flung forwards: back to 1x forward.
        run(&mut state, hand(true, true, 0.0, true, 0.0, -1.0), 0.4, &mut last);
        run(&mut state, hand(true, false, 0.0, true, 6.0, 1.0), 6.0, &mut last);
        assert_eq!(state.motion.velocity, 1.0);
        assert_eq!(state.motion.state, MotionState::Transport);
    }

    #[test]
    fn backwards_play_stops_softly_at_the_start() {
        let mut state = clip(sine(48_000 * 4, 110.0));
        state.position = 48_000.0 * 2.0;
        state.gain = 1.0;
        let mut positions = Vec::new();
        for _ in 0..(3.0 * RATE as f64 / CHUNK as f64) as usize {
            chunk(&mut state, hand(true, false, 0.0, false, 0.0, -1.0));
            positions.push(state.position);
        }
        assert_eq!(state.position, 0.0);
        assert_eq!(state.motion.velocity, 0.0);
        let steps: Vec<f64> = positions.windows(2).map(|w| w[1] - w[0]).collect();
        let jerk = steps.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        // A hard stop from 1x would change the per-chunk step by a whole chunk.
        assert!(jerk < CHUNK as f64 * 0.2, "a hard stop: {jerk}");
        // Silent standing there.
        let out = chunk(&mut state, hand(true, false, 0.0, false, 0.0, -1.0));
        for _ in 0..40 { chunk(&mut state, hand(true, false, 0.0, false, 0.0, -1.0)); }
        let out2 = chunk(&mut state, hand(true, false, 0.0, false, 0.0, -1.0));
        assert!(out2.iter().all(|v| v.abs() < 1e-4), "standing at the start is silent ({})", out[0]);
    }

    /// Upward zero crossings in `out`: the pitch, counted.
    fn crossings(out: &[f32]) -> usize {
        out.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
    }

    fn render_secs(state: &mut DeckState, controls: DeckControls, chunks: usize) -> Vec<f32> {
        (0..chunks).flat_map(|_| chunk(state, controls)).collect()
    }

    #[test]
    fn key_lock_moves_the_tempo_and_keeps_the_pitch() {
        let mut state = clip(sine(48_000 * 4, 440.0));
        let fast = DeckControls { speed: 1.25, keylock: true, ..controls(true, false, 0.0) };
        let out = render_secs(&mut state, fast, 48_000 / CHUNK);
        let heard = &out[out.len() / 2..];
        let hz = crossings(heard) as f64 / (heard.len() as f64 / 48_000.0);
        assert!((hz - 440.0).abs() < 12.0, "the key held at 1.25x: {hz} Hz");
        let moved = state.position / 48_000.0;
        let frames = (48_000 / CHUNK * CHUNK) as f64 / 48_000.0;
        assert!((moved - 1.25 * frames).abs() < 1e-6, "the tempo moved: {moved} s");
        // Without the lock the pitch follows.
        let mut plain = clip(sine(48_000 * 4, 440.0));
        let out = render_secs(&mut plain, DeckControls { speed: 1.25, ..controls(true, false, 0.0) }, 48_000 / CHUNK);
        let heard = &out[out.len() / 2..];
        let hz = crossings(heard) as f64 / (heard.len() as f64 / 48_000.0);
        assert!((hz - 550.0).abs() < 12.0, "varispeed: {hz} Hz");
    }

    #[test]
    fn a_key_shift_moves_the_pitch_and_not_the_tempo() {
        let mut state = clip(sine(48_000 * 4, 440.0));
        let up = DeckControls { key: 12.0, ..controls(true, false, 0.0) };
        let out = render_secs(&mut state, up, 48_000 / CHUNK);
        let heard = &out[out.len() / 2..];
        let hz = crossings(heard) as f64 / (heard.len() as f64 / 48_000.0);
        assert!((hz - 880.0).abs() < 20.0, "an octave up: {hz} Hz");
        let frames = (48_000 / CHUNK * CHUNK) as f64;
        assert!((state.position - frames).abs() < 1e-6, "at its own tempo");
        assert!(state.motion.grains.is_some());
        // Back to no shift: plain varispeed, the grains gone.
        chunk(&mut state, controls(true, false, 0.0));
        assert!(state.motion.grains.is_none());
    }
}
