//! Paged deck sources: ownership, page lifecycle and credited operations.
//!
//! **Status.** A [`PagedSource`] is prepared on an exact device (optionally
//! prefilled with pages and notes while the host still owns it), loaded with
//! `DeckCommand::LoadPaged`, swapped at a quiet point and returned whole with
//! the source acknowledgement. Page operations ([`PagedCommand`]: install,
//! retire, notes, frontier, seek, tail) travel on their own bounded lanes and
//! each is answered by exactly one routed receipt. The deck samples resident
//! current-serial PCM (two-point interpolation; the transport runs forward at
//! speed 0.25..4, a hand on the record either way up to `SCRATCH_RATE_MAX`, of the
//! source/device rate ratio). A durable gap is silence that advances; missing
//! data, a fault or an expired range is silence that holds the source
//! position while the device clock runs on. Readiness is what the callback
//! last found at the play head, never what a source acknowledgement implies.
//!
//! **Loop.** The deck's `loop_in`/`loop_out`/`loop_enabled` bank values after
//! scalar wire routes are the one canonical *requested* loop, validated whole
//! against the retained/committed frontier whenever the request, frontier,
//! serial or follow mode changes. A valid request becomes the effective loop
//! (the position folds into it, DJ modulo, both below IN and at/after OUT; a
//! Tail becomes History with a mode epoch); an invalid one is published as a
//! rejection and keeps the previous effective loop only while that is still
//! valid, else the deck blocks (`ExpiredLoop`) instead of resuming unlooped.
//! `OUT <= IN` means the committed end only on a terminal frontier; on an open
//! one it is refused (`OpenEnd`). A Tail seek while a loop is effective is
//! refused (`LoopActive`). There is no Loop command; scalars are requests.
//!
//! **Speed.** One output frame advances `speed × source_rate / device_rate`
//! source frames. The window planner's `MAX_WINDOW_SPEED` is a fixed maximum
//! (64), not a setting: the adapter must admit only sources whose
//! `source_rate / device_rate × speed` fits it, or surface the unsupported
//! window motion. This owner never clamps the factor silently.
//!
//! **Boundaries.** A finite end is reached exactly (the position becomes
//! `committed_end`, never a step past it); an open frontier holds there
//! without skipping frames it has not committed. An effective loop is
//! entered before any sample (an accepted seek or start outside it folds in,
//! DJ modulo) and every playback crossing of OUT counts once in
//! `boundary_epoch`, however many crossings one step spans. Reaching a
//! terminal end counts once when readiness first becomes `Ended` (also when
//! a Tail already waiting at the end sees the frontier turn terminal), and a
//! Tail becomes History in that same observation.
//!
//! **Stems.** Up to [`STEM_SLOTS`] [`PreparedStemBundle`]s ride in the
//! source, installed and retired exactly like pages (`InstallStems` names
//! the current seek serial; `RetireStems` names the old occupant exactly,
//! so a stale retire never removes a newer one; a refused or retired
//! bundle comes back in its receipt). Only current-serial bundles are
//! heard. Before each chunk the renderer checks, without changing any
//! state, every position the chunk may sample (the same pure step and loop
//! fold as playback) and each one's interpolation neighbour: only if all
//! four stems of a current-serial bundle hold every one of them is the
//! chunk rendered separated (`stems_available`), from the one playhead,
//! gain and dip of the mix. Otherwise the whole chunk is the mix alone. A
//! chunk touching the committed end, an expired range or anything no
//! bundle covers falls back to the mix; zeros inside a bundle are valid
//! stems (silence).
//!
//! **StemReady.** A separate policy from the raw follow mode (Raw
//! `PageOp::Tail` / `PagedStart::Tail` are unchanged and keep the mix
//! fallback). `PageOp::StemTail { expected_serial, cushion }` is credited
//! and applied at the quiet point like a seek: from the *current* serial's
//! installed bundles it takes the latest contiguous run `[s, e)` and moves
//! to `max(s, e - cushion)`; the serial becomes one greater, so every old
//! bundle is unheard until the adapter installs bundles for the new serial
//! (with their own receipts; nothing is restamped). While the policy is
//! `StemReady` the source clock only runs through what the stem path can
//! render: current-serial stem frames (neighbour and loop fold included)
//! and verified durable gaps. Each chunk renders its safe prefix up to the
//! first sample it cannot render, then silence with the source position
//! held (`PagedReadiness::StemWaiting`); it never plays the raw mix. It
//! resumes only when the playable horizon ahead of the play head (stems and
//! verified gaps) is at least `cushion` (or reaches a verified terminal
//! end), so 5.5 s publication batches do not chatter. The position never
//! moves back. An
//! accepted Seek, Cue or Raw Tail, and a newly effective loop, return the
//! policy to `Mixed`; `StemTail` is refused (`LoopActive`) while a loop is
//! effective, without touching the loop controls. Before `LoadPaged` the
//! host may `prefill_stems` and `prepare_stem_tail` on the exclusive Box.
//! A verified durable gap of the current serial (a `Gap` note, inside the
//! retained, committed range, where no current PCM is resident; PCM wins
//! over notes) is playable silence under StemReady: the clock advances
//! through it (a known-silence prefix is finished before any hold), a real
//! frame's gap neighbour interpolates to zero in all four stems, and the
//! cushion horizon runs on through it (to the next PCM page, which needs
//! its own stems). Faults, missing, uncommitted and expired
//! frames still hold, and `stem_ready` still reports only real stem
//! coverage; a chunk that only crosses a gap does not report stems.
//!
//! **Memory.** [`AudioDevice::lease_page_group`](crate::AudioDevice::lease_page_group)
//! reserves `PAGED_GROUP_PAGES × PAGE_BYTES` on the device's track budget and
//! moves that lease into the group, so it lives until the last group handle
//! and page are dropped (host, reader, pools). This bounds admitted PCM page
//! buffers only; a platform decoder's own working memory is not counted.

use crate::bundle::{PreparedStemBundle, STEM_SLOTS};
use crate::deck::TrackLease;
use crate::schedule::AudioBudget;
use makepad_stage_asset_stream::limits::MAX_TAIL_PREBUFFER_SECS;
use crate::deck::{plan_motion, signed_speed, DeckControls, Motion, DECK_DIP_FRAMES};
use makepad_stage_asset_stream::FollowMode;
use makepad_stage_asset_stream::window::{PagePlacement, MAX_WINDOW_SLOTS};
use makepad_stage_asset_stream::{
    ConsumerLoad, MediaIdentity, PageGroup, PcmPage, PublicationSeq, ReadTag, SampleRange, TimelineSnapshot, TimelineState,
    PAGE_BYTES,
};
use std::sync::Arc;

/// Pages one paged load may have allocated at once: 64 resident slots plus
/// staging. The whole group's bytes are reserved up front.
pub const PAGED_GROUP_PAGES: u32 = 80;
/// Notes one snapshot carries: the window planner's note ring.
pub const MAX_PAGED_NOTES: usize = 16;
/// Page operations the host may have outstanding for the whole owner, from
/// admission until its receipt is taken.
pub const PAGE_ROUTES: usize = 128;
/// Page operations the callback takes per device buffer.
pub const PAGE_OPS_PER_BUFFER: usize = 32;

/// The exact identity of one paged load on one deck. `source_sequence` is
/// the deck command sequence that loads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PagedSourceKey {
    pub source_sequence: u64,
    pub media: MediaIdentity,
    pub load: ConsumerLoad,
}

/// Whether the committed frontier can still move. Numeric only: a failure's
/// message stays with the host's timeline snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagedTerminal {
    Open,
    Complete,
    Failed,
}

/// A compact committed frontier: `Copy`, so the callback never replaces or
/// drops a snapshot `Arc`. Frames are at the source rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagedFrontier {
    pub publication: PublicationSeq,
    pub retained_start: u64,
    pub committed_end: u64,
    pub terminal: PagedTerminal,
}

impl PagedFrontier {
    /// Only from a structurally valid snapshot with audio.
    pub fn from_snapshot(snapshot: &TimelineSnapshot) -> Result<Self, String> {
        snapshot.validate().map_err(|error| format!("invalid timeline snapshot: {error:?}"))?;
        if snapshot.audio.is_none() {
            return Err("the timeline has no audio".into());
        }
        let terminal = match snapshot.state {
            TimelineState::Pending | TimelineState::Recording => PagedTerminal::Open,
            TimelineState::Complete => PagedTerminal::Complete,
            TimelineState::Failed(_) => PagedTerminal::Failed,
        };
        Ok(Self {
            publication: snapshot.publication,
            retained_start: snapshot.retained_start,
            committed_end: snapshot.committed_end,
            terminal,
        })
    }

    fn contains(&self, range: SampleRange) -> bool {
        range.start < range.end && range.start >= self.retained_start && range.end <= self.committed_end
    }
}

/// One operation on the loaded paged source. Install and Notes name the
/// current accepted seek serial (in the page tag or `serial`); Retire names
/// the old occupant exactly, whatever its serial.
pub enum PageOp {
    Install { placement: PagePlacement, page: Arc<PcmPage> },
    /// `old_tag`/`old_placement` from `PageWindow::retirement_target`;
    /// `retirement` is the window's new retirement ticket.
    Retire { slot: u8, old_tag: ReadTag, old_placement: u64, retirement: u64 },
    /// The complete note set for `serial`, replacing the previous one only
    /// if `revision` is newer and every range lies inside the current
    /// retained/committed frontier; otherwise nothing changes. One canonical
    /// authority per source and serial (the window planner's ring, through
    /// the adapter): send the latest set with at most one in flight, and
    /// treat `revision` exhaustion (checked increment) as an error. An
    /// accepted seek clears the notes and resets the revision to 0.
    Notes { serial: u64, revision: u64, notes: AudioNotes },
    /// Advance the committed frontier of a growing source.
    Frontier(PagedFrontier),
    /// Move to `frame` at the quiet point; the serial becomes one greater.
    Seek { expected_serial: u64, frame: u64 },
    /// Place a stem bundle for the current seek `serial` in its empty
    /// stem slot. `placement` is the caller's identity for the occupant.
    InstallStems { slot: u8, serial: u64, placement: u64, bundle: Arc<PreparedStemBundle> },
    /// Remove exactly the named occupant (`old_serial`, `old_placement`);
    /// `retirement` is the caller's ticket echoed in the outcome.
    RetireStems { slot: u8, old_serial: u64, old_placement: u64, retirement: u64 },
    /// Move to the committed end minus `prebuffer` frames at the quiet point
    /// and follow in Tail mode; `prebuffer` is at most
    /// [`tail_prebuffer_limit`] of the source rate. Refused on a completed
    /// or terminal source. A later frontier does not seek again.
    /// Refused (`LoopActive`) while a loop is effective: disable it first.
    Tail { expected_serial: u64, prebuffer: u64 },
    /// Move behind the latest installed stem coverage by `cushion` source
    /// frames (positive, at most [`stem_cushion_limit`]) and gate playback
    /// on installed stems. Refused `LoopActive` while a loop is effective,
    /// `NoStems` with no current-serial bundle.
    StemTail { expected_serial: u64, cushion: u64 },
}

/// Longest StemReady cushion, in seconds of source.
pub const MAX_STEM_CUSHION_SECS: u64 = 30;

/// Largest StemReady cushion at `sample_rate`, in source frames.
pub fn stem_cushion_limit(sample_rate: u32) -> u64 {
    MAX_STEM_CUSHION_SECS.saturating_mul(sample_rate as u64)
}

/// Whether the play head is gated on installed stems.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StemPolicy {
    /// Stems when a chunk is covered, the mix otherwise.
    #[default]
    Mixed,
    /// Only chunks the current serial's four stems cover; hold otherwise,
    /// resuming with at least `cushion` source frames of coverage ahead.
    StemReady { cushion: u64 },
}

/// Largest Tail prebuffer at `sample_rate`: `MAX_TAIL_PREBUFFER_SECS` of
/// source frames. The host admits and the callback applies this same bound.
pub fn tail_prebuffer_limit(sample_rate: u32) -> u64 {
    MAX_TAIL_PREBUFFER_SECS.saturating_mul(sample_rate as u64)
}

/// A page operation for the deck whose `source` endpoint it is sent to, as
/// the payload of an `OwnerCommand` for `stage.audio`.
///
/// `ticket` is the caller's receipt identity: it must be unique among the
/// caller's operations still outstanding (not yet taken with
/// `take_paged_return`) on the same source. A duplicate is refused before
/// its payload is taken. The owner's own cues are `PageOpKind::Cue`, a
/// separate namespace, so they never collide with a caller ticket.
pub struct PagedCommand {
    pub source: PagedSourceKey,
    /// The caller's own identity for the receipt.
    pub ticket: u64,
    pub op: PageOp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageOpKind {
    Install,
    Retire,
    Notes,
    Frontier,
    Seek,
    /// A cue pulse on the deck, sent by the owner itself.
    Cue,
    Tail,
    InstallStems,
    RetireStems,
    StemTail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageRefusal {
    /// The deck no longer has exactly that source (or instance/deck).
    StaleSource,
    StaleSerial,
    BadSlot,
    SlotOccupied,
    /// The slot does not hold exactly the named old occupant.
    WrongOccupant,
    NotAdmitted,
    TagMismatch,
    RateMismatch,
    RangeMismatch,
    OutsideCommitted,
    /// Overlaps a resident page of the current serial.
    Overlap,
    /// A note set not newer than the applied one.
    StaleRevision,
    /// An empty note range, or one outside the current frontier.
    InvalidNotes,
    /// A Tail while a loop is effective.
    LoopActive,
    NotGrowing,
    Terminal,
    Regressed,
    BadPrebuffer,
    SerialExhausted,
    /// A StemTail with no current-serial stem bundle to follow.
    NoStems,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageOutcome {
    Installed { slot: u8, placement: u64 },
    StemsInstalled { slot: u8, placement: u64 },
    /// The removed bundle travels with the receipt.
    StemsRetired { slot: u8, retirement: u64 },
    /// The removed page travels with the receipt.
    Retired { slot: u8, retirement: u64 },
    NotesApplied { revision: u64 },
    FrontierAdvanced,
    SeekAccepted { frame: u64, serial: u64 },
    /// A newer seek on the same deck replaced this one before it applied.
    Superseded,
    /// The source or its schedule left before the seek applied.
    Cancelled,
    Refused(PageRefusal),
}

/// Callback-side identity of one admitted operation. `route` indexes the
/// host's bounded ledger, which keeps the full session and node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageOrigin {
    pub route: u16,
    pub slot: u16,
    pub incarnation: u64,
    pub target: u32,
    pub continuity: u64,
    pub origin_generation: u64,
    pub source: PagedSourceKey,
    pub ticket: u64,
}

pub(crate) struct PagePacket {
    pub origin: PageOrigin,
    pub kind: PageOpKind,
    pub op: PageOp,
}

/// Exactly one per admitted operation. `page` is a refused install's own
/// page, or a retired page; `bundle` likewise for stems. The host drops or
/// recycles them, never the callback.
pub struct PageReturn {
    pub origin: PageOrigin,
    pub kind: PageOpKind,
    pub outcome: PageOutcome,
    pub page: Option<Arc<PcmPage>>,
    pub bundle: Option<Arc<PreparedStemBundle>>,
}

/// One installed stem bundle and the identity it was installed under.
struct StemSlot {
    serial: u64,
    placement: u64,
    bundle: Arc<PreparedStemBundle>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingSeek {
    pub origin: PageOrigin,
    pub kind: PageOpKind,
    pub expected_serial: u64,
    /// The destination frame (Seek, Cue) or the prebuffer (Tail).
    pub frames: u64,
}

/// Why a gap is permanent. Only a persisted, verified hole in the source
/// qualifies: page or queue pressure, decode misses and uncommitted ranges
/// never become gaps (they hold as missing data or a fault).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurableGap {
    Source,
    PersistedAdmission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioNoteKind {
    /// Silence that advances.
    Gap(DurableGap),
    /// A failed read: silence that holds until an explicit retry (a new serial).
    Fault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioNote {
    pub range: SampleRange,
    pub kind: AudioNoteKind,
}

/// A complete note set, in the planner's own ring order (holes kept).
pub type AudioNotes = [Option<AudioNote>; MAX_PAGED_NOTES];

/// Where a prepared source starts, fixed with serial 0 before any read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagedStart {
    History { frame: u64 },
    /// `committed_end - prebuffer` (not before `retained_start`), following
    /// the growing frontier.
    Tail { prebuffer: u64 },
}

/// What the callback last found at the play head.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PagedReadiness {
    /// Current-serial PCM is not resident there yet.
    #[default]
    Buffering,
    Ready,
    /// A durable gap: silence, advancing.
    Gap,
    Fault,
    /// Before the retained start: never read, never clamped.
    Expired,
    /// The effective loop left the frontier; blocked until a valid request,
    /// a disable, or a credited seek.
    ExpiredLoop,
    /// Tail at an open frontier, waiting for committed data.
    TailWaiting,
    /// StemReady: holding until installed stems cover the cushion ahead.
    StemWaiting,
    Ended(PagedTerminal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopRefusal {
    NotFinite,
    /// `OUT <= IN` (to the end) on an open, growing frontier.
    OpenEnd,
    OutsideCommitted,
    TooShort,
}

/// The requested loop tuple, as the bank held it after routes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopRequest {
    pub in_seconds: f32,
    pub out_seconds: f32,
    pub enabled: bool,
}

impl LoopRequest {
    fn bits(&self) -> (u32, u32, bool) {
        (self.in_seconds.to_bits(), self.out_seconds.to_bits(), self.enabled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopRejection {
    pub request: LoopRequest,
    pub reason: LoopRefusal,
}

/// The accepted loop, apart from the request.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PagedLoop {
    /// `[in, out)` in source frames.
    pub effective: Option<(u64, u64)>,
    /// The effective range is no longer inside the frontier: blocked.
    pub expired: bool,
    /// The latest refused request and why; cleared by an accepted one.
    pub rejected: Option<LoopRejection>,
    pub loop_epoch: u32,
    pub reject_epoch: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Frame {
    Pcm([f32; 2]),
    Gap,
    Missing,
    Fault,
    Expired,
    End,
}

enum Sample {
    Pcm([f32; 2]),
    Gap,
    Hold(PagedReadiness),
}

fn fold(position: f64, start: u64, end: u64) -> f64 {
    let (start, end) = (start as f64, end as f64);
    if position >= start && position < end { position } else { start + (position - start).rem_euclid(end - start) }
}

/// One prepared paged load. Constructible only by
/// [`AudioDevice::prepare_paged_at`](crate::AudioDevice::prepare_paged_at);
/// every field is private. Moves whole between host and callback (a `Box`);
/// the callback neither clones nor drops it, nor any page in it.
pub struct PagedSource {
    key: PagedSourceKey,
    sample_rate: u32,
    frontier: PagedFrontier,
    seek_serial: u64,
    /// The position the load starts at.
    start_frame: u64,
    slots: [Option<(PagePlacement, Arc<PcmPage>)>; MAX_WINDOW_SLOTS],
    /// Every occupied slot by page start, `order[..resident]` (any serial).
    order: [u8; MAX_WINDOW_SLOTS],
    resident: u8,
    /// Current-serial slots by page start, non-overlapping: what is sampled.
    current: [u8; MAX_WINDOW_SLOTS],
    current_len: u8,
    cursor: u8,
    notes: AudioNotes,
    notes_revision: u64,
    pending_seek: Option<PendingSeek>,
    /// History until a Tail is accepted; History again after an accepted
    /// Seek or Cue, a newly effective loop, or a terminal end.
    mode: FollowMode,
    mode_epoch: u32,
    seek_epoch: u32,
    cue_epoch: u32,
    tail_epoch: u32,
    looping: PagedLoop,
    /// The inputs the loop was last validated against.
    loop_key: Option<((u32, u32, bool), u64, u64, u64, u64, bool)>,
    readiness: PagedReadiness,
    ready_frames: u32,
    /// The last request the renderer saw (bank tuple after routes).
    requested: Option<LoopRequest>,
    boundary_epoch: u32,
    eligible: bool,
    advancing: bool,
    source_frames_per_output_frame: f64,
    stems: [Option<StemSlot>; STEM_SLOTS],
    /// Whether the last rendered chunk was separated (see the module docs).
    stems_available: bool,
    policy: StemPolicy,
    stem_waiting: bool,
    /// The current serial's contiguous installed stem run at the play head
    /// (else the latest one), as of the last chunk.
    stem_ready: Option<SampleRange>,
    /// This chunk's motion is a hand (or its release): no loop folds, and
    /// the retained start and committed end both stop the record.
    free: bool,
    /// The deck's loop stands aside for a hand: nothing folds, samples or
    /// plans against it, and observations report no effective loop.
    loop_suspended: bool,
    /// Declared last: pages drop before the group handle.
    group: PageGroup,
}

/// Read-only counts for observations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PagedCounts {
    pub resident: u8,
    pub current: u8,
    pub gaps: u8,
    pub faults: u8,
    pub pending_seek: bool,
    pub seek_epoch: u32,
    pub cue_epoch: u32,
    pub tail_epoch: u32,
    pub mode_epoch: u32,
    pub notes_revision: u64,
    /// Playback loop crossings and terminal ends reached.
    pub boundary_epoch: u32,
}

impl PagedSource {
    pub fn key(&self) -> PagedSourceKey {
        self.key
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn frontier(&self) -> PagedFrontier {
        self.frontier
    }

    pub fn seek_serial(&self) -> u64 {
        self.seek_serial
    }

    pub fn group(&self) -> &PageGroup {
        &self.group
    }

    pub fn mode(&self) -> FollowMode {
        self.mode
    }

    pub fn start_frame(&self) -> u64 {
        self.start_frame
    }

    /// The loop as observers see it: no effective loop while a hand has it
    /// suspended, so followers and the page planner move with the record.
    pub fn looping(&self) -> PagedLoop {
        let mut looping = self.looping;
        if self.loop_suspended {
            looping.effective = None;
        }
        looping
    }

    /// The loop playback, sampling and paging use now: the active loop,
    /// unless a hand has it suspended.
    fn sampling_loop(&self) -> Option<(u64, u64)> {
        self.active_loop().filter(|_| !self.loop_suspended)
    }

    /// Where a hand may put the head: the retained start to the last
    /// committed frame, never a frame (or successor) past the frontier.
    fn free_bounds(&self) -> (f64, f64) {
        let frontier = self.frontier;
        let last = frontier.committed_end.saturating_sub(1).max(frontier.retained_start);
        (frontier.retained_start as f64, last as f64)
    }

    /// The callback's last finding at the play head, the contiguous
    /// current-serial PCM from there (frames), and whether it was playing
    /// that PCM (play on, no dip pending, Ready).
    pub fn readiness(&self) -> (PagedReadiness, u32, bool) {
        (self.readiness, self.ready_frames, self.eligible)
    }

    /// The source frames one output frame advances in the last rendered
    /// buffer: `speed × source_rate / device_rate`, the exact value that
    /// buffer's steps used, sampled once per buffer and not re-read from the
    /// bank later. It is nominal: it stays set while paused, dipping, held,
    /// expired or ended, so it predicts motion only together with
    /// [`advancing`](Self::advancing), and only until the next publication.
    pub fn source_frames_per_output_frame(&self) -> f64 {
        self.source_frames_per_output_frame
    }

    /// Whether the source clock was advancing at the end of the last
    /// buffer: play on, no dip pending, and the play head on PCM (Ready) or
    /// on a verified durable gap (which advances in silence). False while
    /// paused, dipping, buffering, faulted, expired, blocked or ended. This
    /// is the renderer's own fact, apart from audibility and from the bank's
    /// play control; a follower must use it, not derive it from those.
    pub fn advancing(&self) -> bool {
        self.advancing
    }

    pub fn counts(&self) -> PagedCounts {
        let count = |gap: bool| self.notes.iter().flatten()
            .filter(|note| matches!(note.kind, AudioNoteKind::Gap(_)) == gap).count() as u8;
        PagedCounts {
            resident: self.resident,
            current: self.current_len,
            gaps: count(true),
            faults: count(false),
            pending_seek: self.pending_seek.is_some(),
            seek_epoch: self.seek_epoch,
            cue_epoch: self.cue_epoch,
            tail_epoch: self.tail_epoch,
            mode_epoch: self.mode_epoch,
            notes_revision: self.notes_revision,
            boundary_epoch: self.boundary_epoch,
        }
    }

    pub(crate) fn leased_from(&self, budget: &Arc<AudioBudget>) -> bool {
        group_leased_from(&self.group, budget)
    }

    /// Whether the last rendered chunk played the four stems.
    pub fn stems_available(&self) -> bool {
        self.stems_available
    }

    /// The accepted stem policy (not the request).
    pub fn stem_policy(&self) -> StemPolicy {
        self.policy
    }

    /// The current serial's contiguous installed stem coverage at the play
    /// head (else the latest run), as the last chunk found it. Separate from
    /// the raw committed frontier.
    pub fn stem_ready(&self) -> Option<SampleRange> {
        self.stem_ready
    }

    /// Before `LoadPaged` only: place a serial-0 stem bundle with exactly
    /// the checks a runtime `InstallStems` makes. Provisional until the load
    /// is acknowledged; refused, the bundle comes back.
    pub fn prefill_stems(&mut self, slot: u8, placement: u64, bundle: Arc<PreparedStemBundle>) -> Result<(), (PageRefusal, Arc<PreparedStemBundle>)> {
        self.install_stems(slot, self.seek_serial, placement, bundle).map(|_| ())
    }

    /// Before `LoadPaged` only: start StemReady at the latest prefilled
    /// contiguous stem run's end minus `cushion` (not before its start), in
    /// History mode. Returns the start frame.
    pub fn prepare_stem_tail(&mut self, cushion: u64) -> Result<u64, PageRefusal> {
        let frame = self.stem_tail_frame(cushion)?;
        self.start_frame = frame;
        if self.mode != FollowMode::History {
            self.mode = FollowMode::History;
        }
        self.policy = StemPolicy::StemReady { cushion };
        self.stem_waiting = true;
        Ok(frame)
    }

    /// Current-serial installed ranges merged into contiguous runs, sorted.
    /// One scan of the stem slots.
    fn stem_runs(&self) -> ([(u64, u64); STEM_SLOTS], usize) {
        let mut runs = [(0u64, 0u64); STEM_SLOTS];
        let mut count = 0;
        for held in self.stems.iter().flatten().filter(|held| held.serial == self.seek_serial) {
            // Only coverage inside the retained, committed frames counts.
            let range = held.bundle.range();
            let (start, end) = (range.start.max(self.frontier.retained_start), range.end.min(self.frontier.committed_end));
            if start >= end {
                continue;
            }
            let at = runs[..count].iter().position(|run| run.0 > start).unwrap_or(count);
            runs.copy_within(at..count, at + 1);
            runs[at] = (start, end);
            count += 1;
        }
        let mut merged = 0;
        for index in 0..count {
            if merged > 0 && runs[merged - 1].1 == runs[index].0 {
                runs[merged - 1].1 = runs[index].1;
            } else {
                runs[merged] = runs[index];
                merged += 1;
            }
        }
        (runs, merged)
    }

    /// Refresh the `stem_ready` readback at `position`; returns the run
    /// holding it.
    fn refresh_stem_ready(&mut self, position: f64) -> Option<(u64, u64)> {
        let (runs, count) = self.stem_runs();
        let at = position.max(0.0).floor() as u64;
        let run = runs[..count].iter().find(|run| run.0 <= at && at < run.1).copied();
        self.stem_ready = run.or(runs[..count].last().copied()).and_then(|(start, end)| SampleRange::new(start, end));
        run
    }

    /// Where a StemTail lands: the latest contiguous current-serial run
    /// `[s, e)`, at `max(s, e - cushion)`, inside the frontier.
    fn stem_tail_frame(&self, cushion: u64) -> Result<u64, PageRefusal> {
        if cushion == 0 || cushion > stem_cushion_limit(self.sample_rate) {
            return Err(PageRefusal::BadPrebuffer);
        }
        let (runs, count) = self.stem_runs();
        let (start, end) = *runs[..count].last().ok_or(PageRefusal::NoStems)?;
        let frame = end.saturating_sub(cushion).max(start);
        if frame < self.frontier.retained_start || frame >= self.frontier.committed_end {
            return Err(PageRefusal::OutsideCommitted);
        }
        Ok(frame)
    }

    /// Occupied stem slots, any serial.
    pub fn stem_slots(&self) -> u8 {
        self.stems.iter().flatten().count() as u8
    }

    /// Place a stem bundle for the current serial in its empty slot.
    /// Refused, the bundle comes back as it was.
    pub(crate) fn install_stems(
        &mut self,
        slot: u8,
        serial: u64,
        placement: u64,
        bundle: Arc<PreparedStemBundle>,
    ) -> Result<PageOutcome, (PageRefusal, Arc<PreparedStemBundle>)> {
        let index = slot as usize;
        let range = bundle.range();
        let same_budget = self.group.keepalive().downcast_ref::<TrackLease>().is_some_and(|lease| bundle.leased_from(lease.budget_arc()));
        let refusal = if index >= STEM_SLOTS {
            Some(PageRefusal::BadSlot)
        } else if self.stems[index].is_some() {
            Some(PageRefusal::SlotOccupied)
        } else if !same_budget {
            Some(PageRefusal::NotAdmitted)
        } else if bundle.media() != self.key.media {
            Some(PageRefusal::TagMismatch)
        } else if serial != self.seek_serial {
            Some(PageRefusal::StaleSerial)
        } else if bundle.sample_rate() != self.sample_rate {
            Some(PageRefusal::RateMismatch)
        } else if bundle.stem(0).len() as u64 != range.len() {
            Some(PageRefusal::RangeMismatch)
        } else if !self.frontier.contains(range) {
            Some(PageRefusal::OutsideCommitted)
        } else if self.stems.iter().flatten().any(|other| other.serial == serial && other.bundle.range().intersect(&range).is_some()) {
            Some(PageRefusal::Overlap)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            return Err((refusal, bundle));
        }
        self.stems[index] = Some(StemSlot { serial, placement, bundle });
        Ok(PageOutcome::StemsInstalled { slot, placement })
    }

    /// Remove exactly the named stem occupant; the bundle comes out.
    pub(crate) fn retire_stems(
        &mut self,
        slot: u8,
        old_serial: u64,
        old_placement: u64,
        retirement: u64,
    ) -> Result<(PageOutcome, Arc<PreparedStemBundle>), PageRefusal> {
        let entry = self.stems.get_mut(slot as usize).ok_or(PageRefusal::BadSlot)?;
        if !entry.as_ref().is_some_and(|held| held.serial == old_serial && held.placement == old_placement) {
            return Err(PageRefusal::WrongOccupant);
        }
        let held = entry.take().ok_or(PageRefusal::WrongOccupant)?;
        Ok((PageOutcome::StemsRetired { slot, retirement }, held.bundle))
    }

    /// The current-serial bundle holding `frame`, by slot index.
    fn stem_bundle(&self, frame: u64) -> Option<&PreparedStemBundle> {
        let serial = self.seek_serial;
        self.stems.iter().flatten().find(|held| held.serial == serial && held.bundle.range().contains(frame)).map(|held| &*held.bundle)
    }

    /// The interpolation neighbour `sample` reads after `index`.
    fn neighbour(&self, index: u64) -> u64 {
        match self.sampling_loop() {
            Some((start, end)) if index + 1 >= end => start,
            _ => index + 1,
        }
    }

    /// Whether current-serial PCM holds `frame`: a binary search of the
    /// sorted current view, without moving the sampling cursor.
    fn has_current_pcm(&self, frame: u64) -> bool {
        let len = self.current_len as usize;
        let slots = &self.slots;
        let after = self.current[..len].partition_point(|slot| slots[*slot as usize].as_ref().is_some_and(|(placement, _)| placement.range.start <= frame));
        after.checked_sub(1).is_some_and(|index| self.covers(index, frame))
    }

    /// The start of the first current-serial page after `frame`.
    fn next_current_pcm(&self, frame: u64) -> Option<u64> {
        let len = self.current_len as usize;
        let slots = &self.slots;
        let after = self.current[..len].partition_point(|slot| slots[*slot as usize].as_ref().is_some_and(|(placement, _)| placement.range.start <= frame));
        self.current[after..len].first().and_then(|slot| slots[*slot as usize].as_ref()).map(|(placement, _)| placement.range.start)
    }

    /// A verified durable gap at `frame`, exactly as `frame()` decides it:
    /// inside the retained, committed range, no current PCM (PCM wins over
    /// notes), and the first note holding it is a `Gap` (not a `Fault`).
    /// Never inferred from absence. Returns the end of that note.
    fn durable_gap(&self, frame: u64) -> Option<u64> {
        let frontier = self.frontier;
        if frame < frontier.retained_start || frame >= frontier.committed_end || self.has_current_pcm(frame) {
            return None;
        }
        let note = self.notes.iter().flatten().find(|note| note.range.contains(frame))?;
        matches!(note.kind, AudioNoteKind::Gap(_)).then_some(note.range.end)
    }

    /// Whether the stem path can render `position` truthfully, and whether
    /// it reads a real stem frame there (`Some(true)`) or only silence
    /// (`Some(false)`); `None` if it cannot.
    ///
    /// Only frames the source really has count: at a verified terminal end
    /// a position at or past `committed_end` is the silence after the end
    /// (not missing coverage), and a fractional neighbour there repeats the
    /// last frame, as the mix sampler does. An open frontier is never an
    /// end: a frame there is missing. With `gaps` (StemReady only), a
    /// verified durable gap is playable silence as well: at the position
    /// (the sampler reads no successor there) or as the zero neighbour of a
    /// real frame.
    fn stems_cover(&self, position: f64, gaps: bool) -> Option<bool> {
        let frontier = self.frontier;
        let terminal = frontier.terminal != PagedTerminal::Open;
        // Under StemReady a verified durable gap is silence even where a
        // (gap-aware) model bundle spans it: it outranks the bundle.
        let playable = |frame: u64| -> Option<bool> {
            if gaps && self.durable_gap(frame).is_some() {
                Some(false)
            } else if frame >= frontier.retained_start && frame < frontier.committed_end && self.stem_bundle(frame).is_some() {
                Some(true)
            } else {
                None
            }
        };
        let index = position.max(0.0).floor() as u64;
        if terminal && index >= frontier.committed_end {
            return Some(false);
        }
        let here = playable(index)?;
        if !here || position == index as f64 {
            return Some(here);
        }
        let next = self.neighbour(index);
        if terminal && next >= frontier.committed_end {
            return Some(true);
        }
        playable(next).map(|_| true)
    }

    /// Every position a chunk from `position` may sample, walked with the
    /// pure step (no epoch counted): how many leading frames the stem path
    /// covers (`frames` when all), and whether those read a real stem frame.
    fn stems_cover_chunk(&self, position: f64, steps: &[f64], moves: bool, frames: usize, gaps: bool) -> (usize, bool) {
        if !gaps && !self.stems.iter().flatten().any(|held| held.serial == self.seek_serial) {
            return (0, false);
        }
        let mut real = false;
        let mut at = position;
        for frame in 0..frames {
            match self.stems_cover(at, gaps) {
                Some(stem) => real |= stem,
                None => return (frame, real),
            }
            if !moves {
                break;
            }
            at = self.next_position(at, steps[frame]).0;
        }
        (frames, real)
    }

    /// While StemReady waits for real audio: how many leading frames of a
    /// chunk stay on verified durable gaps (or past a verified end), walked
    /// with the pure step. Known silence advances; nothing else is read.
    fn gap_prefix(&self, position: f64, steps: &[f64], moves: bool, frames: usize) -> usize {
        let frontier = self.frontier;
        let terminal = frontier.terminal != PagedTerminal::Open;
        let mut at = position;
        for frame in 0..frames {
            let index = at.max(0.0).floor() as u64;
            if !(terminal && index >= frontier.committed_end) && self.durable_gap(index).is_none() {
                return frame;
            }
            if !moves {
                return frames;
            }
            at = self.next_position(at, steps[frame]).0;
        }
        frames
    }

    /// StemReady's playable horizon from frame `at`: through contiguous
    /// current-serial stem runs and verified durable gaps (a gap only up to
    /// the next current PCM page, which needs its own stems). Internal only:
    /// `stem_ready` still reports genuine stem coverage. Returns the horizon
    /// and whether it reaches a verified terminal end. At most one pass per
    /// stem run and note.
    fn playable_horizon(&self, at: u64) -> (u64, bool) {
        let frontier = self.frontier;
        let terminal = frontier.terminal != PagedTerminal::Open;
        let (runs, count) = self.stem_runs();
        let mut horizon = at;
        for _ in 0..STEM_SLOTS + MAX_PAGED_NOTES + 2 {
            if terminal && horizon >= frontier.committed_end {
                return (horizon, true);
            }
            if let Some(run) = runs[..count].iter().find(|run| run.0 <= horizon && horizon < run.1) {
                horizon = run.1;
            } else if let Some(end) = self.durable_gap(horizon) {
                let next_pcm = self.next_current_pcm(horizon).unwrap_or(u64::MAX);
                horizon = end.min(next_pcm).min(frontier.committed_end);
            } else {
                break;
            }
        }
        (horizon, false)
    }

    /// The four stems at `position` (covered, checked by the preflight),
    /// interpolated exactly as the mix is.
    fn stem_values(&self, position: f64, gain: f32) -> [[f32; 2]; 4] {
        let index = position.max(0.0).floor() as u64;
        let t = (position - index as f64) as f32;
        let next = self.neighbour(index);
        let (Some(here), there) = (self.stem_bundle(index), self.stem_bundle(next)) else { return [[0.0; 2]; 4] };
        // A neighbour without stems is the verified end (the last frame
        // repeats, as the mix does) or, under StemReady, a durable gap: zero,
        // as the mix's gap successor is. The preflight admitted nothing else.
        // Under StemReady a verified durable gap neighbour is zero even if a
        // bundle spans it; Mixed keeps its bundle-first order.
        let frontier = self.frontier;
        let end = frontier.terminal != PagedTerminal::Open && next >= frontier.committed_end;
        let gap_next = t != 0.0 && matches!(self.policy, StemPolicy::StemReady { .. }) && self.durable_gap(next).is_some();
        std::array::from_fn(|lane| {
            let a = here.frame(lane, index);
            let b = match there {
                _ if t == 0.0 => a,
                _ if gap_next => [0.0; 2],
                Some(bundle) => bundle.frame(lane, next),
                None if end => a,
                None => [0.0; 2],
            };
            [(a[0] + (b[0] - a[0]) * t) * gain, (a[1] + (b[1] - a[1]) * t) * gain]
        })
    }

    /// Before `LoadPaged` only (the host or reader worker owns the Box, so
    /// nothing else can see it): place a serial-0 page with exactly the
    /// checks a runtime Install makes. Refused, the page comes back as it
    /// was. No receipt exists; the caller's placement stays provisional
    /// until the load is acknowledged. Not a readiness claim.
    pub fn prefill(&mut self, placement: PagePlacement, page: Arc<PcmPage>) -> Result<(), (PageRefusal, Arc<PcmPage>)> {
        self.install(placement, page).map(|_| ())
    }

    /// Before `LoadPaged` only: the initial canonical note set, validated as
    /// a runtime `Notes` operation is.
    pub fn prefill_notes(&mut self, revision: u64, notes: AudioNotes) -> Result<(), PageRefusal> {
        self.apply_notes(self.seek_serial, revision, notes).map(|_| ())
    }

    /// Place `page` in its empty slot. Refused, the page comes back as it was.
    pub(crate) fn install(&mut self, placement: PagePlacement, page: Arc<PcmPage>) -> Result<PageOutcome, (PageRefusal, Arc<PcmPage>)> {
        let index = placement.slot as usize;
        let refusal = if index >= MAX_WINDOW_SLOTS {
            Some(PageRefusal::BadSlot)
        } else if self.slots[index].is_some() {
            Some(PageRefusal::SlotOccupied)
        } else if !page.is_admitted_by(&self.group) {
            Some(PageRefusal::NotAdmitted)
        } else if page.tag() != placement.tag || placement.tag.media != self.key.media || placement.tag.load != self.key.load {
            Some(PageRefusal::TagMismatch)
        } else if placement.tag.seek_serial != self.seek_serial {
            Some(PageRefusal::StaleSerial)
        } else if page.sample_rate() != self.sample_rate {
            Some(PageRefusal::RateMismatch)
        } else if page.range() != placement.range {
            Some(PageRefusal::RangeMismatch)
        } else if !self.frontier.contains(placement.range) {
            Some(PageRefusal::OutsideCommitted)
        } else if self.current[..self.current_len as usize].iter().any(|slot| {
            self.slots[*slot as usize].as_ref().is_some_and(|(other, _)| other.range.intersect(&placement.range).is_some())
        }) {
            Some(PageRefusal::Overlap)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            return Err((refusal, page));
        }
        let start = |slots: &[Option<(PagePlacement, Arc<PcmPage>)>], slot: u8| slots[slot as usize].as_ref().map_or(u64::MAX, |(other, _)| other.range.start);
        let resident = self.resident as usize;
        let at = self.order[..resident].iter().position(|slot| start(&self.slots, *slot) > placement.range.start).unwrap_or(resident);
        self.order.copy_within(at..resident, at + 1);
        self.order[at] = placement.slot;
        self.resident += 1;
        let len = self.current_len as usize;
        let at = self.current[..len].iter().position(|slot| start(&self.slots, *slot) > placement.range.start).unwrap_or(len);
        self.current.copy_within(at..len, at + 1);
        self.current[at] = placement.slot;
        self.current_len += 1;
        self.slots[index] = Some((placement, page));
        Ok(PageOutcome::Installed { slot: placement.slot, placement: placement.ticket })
    }

    /// Remove exactly the named old occupant; the page comes out.
    pub(crate) fn retire(&mut self, slot: u8, old_tag: ReadTag, old_placement: u64, retirement: u64) -> Result<(PageOutcome, Arc<PcmPage>), PageRefusal> {
        let index = slot as usize;
        if index >= MAX_WINDOW_SLOTS {
            return Err(PageRefusal::BadSlot);
        }
        match &self.slots[index] {
            Some((placement, page)) if placement.ticket == old_placement && placement.tag == old_tag && page.tag() == old_tag => {}
            _ => return Err(PageRefusal::WrongOccupant),
        }
        let (_, page) = self.slots[index].take().expect("checked above");
        let resident = self.resident as usize;
        if let Some(at) = self.order[..resident].iter().position(|occupied| *occupied == slot) {
            self.order.copy_within(at + 1..resident, at);
            self.resident -= 1;
        }
        let len = self.current_len as usize;
        if let Some(at) = self.current[..len].iter().position(|occupied| *occupied == slot) {
            self.current.copy_within(at + 1..len, at);
            self.current_len -= 1;
            self.cursor = 0;
        }
        Ok((PageOutcome::Retired { slot, retirement }, page))
    }

    /// Replace the whole note set, or change nothing.
    pub(crate) fn apply_notes(&mut self, serial: u64, revision: u64, notes: AudioNotes) -> Result<u64, PageRefusal> {
        if serial != self.seek_serial {
            return Err(PageRefusal::StaleSerial);
        }
        if revision <= self.notes_revision {
            return Err(PageRefusal::StaleRevision);
        }
        if notes.iter().flatten().any(|note| !self.frontier.contains(note.range)) {
            return Err(PageRefusal::InvalidNotes);
        }
        self.notes = notes;
        self.notes_revision = revision;
        Ok(revision)
    }

    /// A growing source's frontier moves forward only; a terminal one never.
    pub(crate) fn advance_frontier(&mut self, next: PagedFrontier) -> Result<PageOutcome, PageRefusal> {
        let now = self.frontier;
        if !self.key.media.is_growing() {
            return Err(PageRefusal::NotGrowing);
        }
        if now.terminal != PagedTerminal::Open {
            return Err(PageRefusal::Terminal);
        }
        if next.publication <= now.publication || next.committed_end < now.committed_end
            || next.retained_start < now.retained_start || next.retained_start > next.committed_end
        {
            return Err(PageRefusal::Regressed);
        }
        self.frontier = next;
        Ok(PageOutcome::FrontierAdvanced)
    }

    /// A seek admitted now: the previous pending one, if any, comes out to
    /// be answered `Superseded`.
    pub(crate) fn hold_seek(&mut self, seek: PendingSeek) -> Option<PendingSeek> {
        self.pending_seek.replace(seek)
    }

    pub(crate) fn take_pending_seek(&mut self) -> Option<PendingSeek> {
        self.pending_seek.take()
    }

    /// Apply a pending seek at the quiet point: the serial becomes exactly
    /// one greater. Returns the destination frame and the new serial.
    pub(crate) fn accept_seek(&mut self, seek: &PendingSeek) -> Result<(u64, u64), PageRefusal> {
        if seek.origin.source != self.key {
            return Err(PageRefusal::StaleSource);
        }
        if seek.expected_serial != self.seek_serial {
            return Err(PageRefusal::StaleSerial);
        }
        // Every check runs before anything changes: a refused seek leaves
        // the serial, position, mode, notes and pages as they were.
        let frontier = self.frontier;
        let (frame, mode) = match seek.kind {
            PageOpKind::Tail => {
                // Only a loop that is (or is about to be) valid now blocks a
                // Tail: an expired one never does.
                let requested = self.requested.and_then(|request| self.loop_candidate(request).ok().flatten());
                if self.active_loop().is_some() || requested.is_some() {
                    return Err(PageRefusal::LoopActive);
                }
                (self.tail_frame(seek.frames)?, FollowMode::Tail { prebuffer_frames: seek.frames })
            }
            PageOpKind::StemTail => {
                let requested = self.requested.and_then(|request| self.loop_candidate(request).ok().flatten());
                if self.active_loop().is_some() || requested.is_some() {
                    return Err(PageRefusal::LoopActive);
                }
                // Derived from the old serial's coverage, before it changes.
                (self.stem_tail_frame(seek.frames)?, FollowMode::History)
            }
            _ => {
                if seek.frames < frontier.retained_start || seek.frames > frontier.committed_end {
                    return Err(PageRefusal::OutsideCommitted);
                }
                (seek.frames, FollowMode::History)
            }
        };
        let serial = self.seek_serial.checked_add(1).ok_or(PageRefusal::SerialExhausted)?;
        self.seek_serial = serial;
        if self.mode != mode {
            self.mode_epoch = self.mode_epoch.wrapping_add(1);
        }
        self.mode = mode;
        // Notes and the sampled page view belong to a serial; the new one
        // starts empty. Old pages stay resident until retired exactly.
        self.notes = [None; MAX_PAGED_NOTES];
        self.notes_revision = 0;
        self.current_len = 0;
        self.cursor = 0;
        // An effective loop that is no longer valid stops blocking: the
        // accepted seek is the recovery. The request is revalidated on the
        // new serial (and may stay reported as rejected).
        if self.looping.effective.is_some() && self.active_loop().is_none() {
            self.looping.effective = None;
            self.looping.expired = false;
            self.looping.loop_epoch = self.looping.loop_epoch.wrapping_add(1);
        }
        self.loop_key = None;
        match seek.kind {
            PageOpKind::Cue => self.cue_epoch = self.cue_epoch.wrapping_add(1),
            PageOpKind::Tail | PageOpKind::StemTail => self.tail_epoch = self.tail_epoch.wrapping_add(1),
            _ => self.seek_epoch = self.seek_epoch.wrapping_add(1),
        }
        // StemReady only by a StemTail; any other accepted move ends it. The
        // new serial holds until its own bundles are installed.
        if seek.kind == PageOpKind::StemTail {
            self.policy = StemPolicy::StemReady { cushion: seek.frames };
            self.stem_waiting = true;
        } else {
            self.policy = StemPolicy::Mixed;
            self.stem_waiting = false;
        }
        Ok((frame, serial))
    }

    fn tail_frame(&self, prebuffer: u64) -> Result<u64, PageRefusal> {
        let frontier = self.frontier;
        if !self.key.media.is_growing() {
            return Err(PageRefusal::NotGrowing);
        }
        if frontier.terminal != PagedTerminal::Open {
            return Err(PageRefusal::Terminal);
        }
        if prebuffer > tail_prebuffer_limit(self.sample_rate) {
            return Err(PageRefusal::BadPrebuffer);
        }
        if frontier.committed_end <= frontier.retained_start {
            return Err(PageRefusal::OutsideCommitted);
        }
        Ok(frontier.committed_end.saturating_sub(prebuffer).max(frontier.retained_start))
    }

    // --- sampling ---------------------------------------------------------

    fn covers(&self, index: usize, frame: u64) -> bool {
        self.slots[self.current[index] as usize].as_ref().is_some_and(|(placement, _)| placement.range.contains(frame))
    }

    /// The current-serial page index holding `frame`: the cached entry and
    /// its neighbours first, then a binary search of the sorted,
    /// non-overlapping current view.
    fn find(&mut self, frame: u64) -> Option<usize> {
        let len = self.current_len as usize;
        if len == 0 {
            return None;
        }
        let cursor = (self.cursor as usize).min(len - 1);
        let near = [cursor, cursor + 1, cursor.wrapping_sub(1)].into_iter().find(|index| *index < len && self.covers(*index, frame));
        let found = near.or_else(|| {
            let slots = &self.slots;
            let after = self.current[..len].partition_point(|slot| slots[*slot as usize].as_ref().is_some_and(|(placement, _)| placement.range.start <= frame));
            after.checked_sub(1).filter(|index| self.covers(*index, frame))
        })?;
        self.cursor = found as u8;
        Some(found)
    }

    fn pcm(&mut self, frame: u64) -> Option<[f32; 2]> {
        let index = self.find(frame)?;
        let (placement, page) = self.slots[self.current[index] as usize].as_ref()?;
        let value = page.frames().get((frame - placement.range.start) as usize)?;
        Some([value[0] as f32 / 32768.0, value[1] as f32 / 32768.0])
    }

    /// What `frame` holds, the frontier first: never unretained or
    /// uncommitted data, even if a page still holds it. Verified current PCM
    /// then wins over notes, as the window planner's coverage does.
    fn frame(&mut self, frame: u64) -> Frame {
        let frontier = self.frontier;
        if frame < frontier.retained_start {
            return Frame::Expired;
        }
        if frame >= frontier.committed_end {
            return if frontier.terminal == PagedTerminal::Open { Frame::Missing } else { Frame::End };
        }
        if let Some(value) = self.pcm(frame) {
            return Frame::Pcm(value);
        }
        for note in self.notes.iter().flatten() {
            if note.range.contains(frame) {
                return match note.kind {
                    AudioNoteKind::Gap(_) => Frame::Gap,
                    AudioNoteKind::Fault => Frame::Fault,
                };
            }
        }
        Frame::Missing
    }

    fn hold(&self, frame: Frame, at: u64) -> PagedReadiness {
        match frame {
            Frame::Fault => PagedReadiness::Fault,
            Frame::Expired => PagedReadiness::Expired,
            Frame::End => PagedReadiness::Ended(self.frontier.terminal),
            _ if at >= self.frontier.committed_end && matches!(self.mode, FollowMode::Tail { .. }) => PagedReadiness::TailWaiting,
            _ => PagedReadiness::Buffering,
        }
    }

    /// The sample at `position`: two-point interpolation to the successor
    /// (effective OUT wraps to IN; a terminal end repeats the last sample;
    /// a durable-gap successor is zero). A required frame that is missing,
    /// faulted or expired holds, with its reason.
    fn sample(&mut self, position: f64) -> Sample {
        let index = position.max(0.0).floor() as u64;
        let a = match self.frame(index) {
            Frame::Pcm(value) => value,
            Frame::Gap => return Sample::Gap,
            other => return Sample::Hold(self.hold(other, index)),
        };
        // Exactly on a frame: that frame alone, no successor required.
        let t = position - index as f64;
        if t == 0.0 {
            return Sample::Pcm(a);
        }
        let next = self.neighbour(index);
        let b = match self.frame(next) {
            Frame::Pcm(value) => value,
            Frame::Gap => [0.0; 2],
            Frame::End => a,
            other => return Sample::Hold(self.hold(other, next)),
        };
        let t = t as f32;
        Sample::Pcm([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t])
    }

    /// Readiness at `position`, and the contiguous current PCM from there
    /// only when that is Ready (0 otherwise). Once per chunk.
    fn survey(&mut self, position: f64) -> (PagedReadiness, u32) {
        if self.looping.expired {
            return (PagedReadiness::ExpiredLoop, 0);
        }
        let readiness = match self.sample(position) {
            Sample::Pcm(_) => PagedReadiness::Ready,
            Sample::Gap => PagedReadiness::Gap,
            Sample::Hold(reason) => reason,
        };
        if readiness != PagedReadiness::Ready {
            return (readiness, 0);
        }
        let index = position.max(0.0).floor() as u64;
        let mut frames = 0u64;
        if let Some(mut at) = self.find(index) {
            let mut from = index;
            let limit = self.frontier.committed_end;
            while at < self.current_len as usize && from < limit {
                let Some((placement, _)) = self.slots[self.current[at] as usize].as_ref() else { break };
                if placement.range.start > from {
                    break;
                }
                let end = placement.range.end.min(limit);
                frames += end.saturating_sub(from);
                from = end;
                at += 1;
            }
        }
        (readiness, frames.min(u32::MAX as u64) as u32)
    }

    /// The effective loop if it is valid against the current frontier.
    fn active_loop(&self) -> Option<(u64, u64)> {
        let frontier = self.frontier;
        self.looping.effective.filter(|(start, end)| !self.looping.expired && *start >= frontier.retained_start && *end <= frontier.committed_end)
    }

    /// One playback step: across the effective loop, arithmetically (each
    /// crossing of OUT counted once); otherwise never past the committed end,
    /// finite or open.
    fn step(&mut self, position: f64, advance: f64) -> f64 {
        let (next, crossings) = self.next_position(position, advance);
        self.boundary_epoch = self.boundary_epoch.wrapping_add(crossings);
        next
    }

    /// The pure part of [`step`](Self::step): the next position and the
    /// loop crossings it makes, changing nothing. Under a hand (`free`) the
    /// record moves either way, stopped at the retained start and at the
    /// committed end; the loop folds again once the transport takes over.
    fn next_position(&self, position: f64, advance: f64) -> (f64, u32) {
        let next = position + advance;
        if self.free {
            let (low, high) = self.free_bounds();
            return (next.clamp(low, high), 0);
        }
        if let Some((start, end)) = self.sampling_loop() {
            let (start, len) = (start as f64, (end - start) as f64);
            let span = next - start;
            if span >= len {
                let crossings = (span / len).floor();
                return (start + span.rem_euclid(len), crossings.min(u32::MAX as f64) as u32);
            }
            return (next, 0);
        }
        (next.min(self.frontier.committed_end as f64), 0)
    }

    /// Validate the requested loop whole when it or what it depends on
    /// changed; see the module docs.
    /// Take the deck's current loop controls (`request`) as the requested
    /// loop now, validating it whole, before a page operation is judged.
    pub(crate) fn apply_request(&mut self, request: LoopRequest, position: &mut f64, may_fold: bool) {
        self.requested = Some(request);
        self.update_loop(request, position, may_fold);
    }

    /// `may_fold`: false while a hand has the loop suspended (or is taking
    /// hold this chunk), so a newly valid loop never jumps the head there;
    /// the suspension's own quiet-point path folds it later.
    fn update_loop(&mut self, request: LoopRequest, position: &mut f64, may_fold: bool) {
        let frontier = self.frontier;
        let key = (request.bits(), frontier.publication.0, frontier.retained_start, frontier.committed_end, self.seek_serial,
            matches!(self.mode, FollowMode::Tail { .. }));
        if self.loop_key == Some(key) {
            return;
        }
        self.loop_key = Some(key);
        match self.loop_candidate(request) {
            Ok(range) => {
                if self.looping.rejected.take().is_some() {
                    self.looping.reject_epoch = self.looping.reject_epoch.wrapping_add(1);
                }
                if self.looping.effective != range || self.looping.expired {
                    self.looping.effective = range;
                    self.looping.expired = false;
                    self.looping.loop_epoch = self.looping.loop_epoch.wrapping_add(1);
                    if let Some((start, end)) = range {
                        if may_fold {
                            *position = fold(*position, start, end);
                        }
                        // An explicit loop is History with the mix fallback.
                        self.policy = StemPolicy::Mixed;
                        self.stem_waiting = false;
                        if matches!(self.mode, FollowMode::Tail { .. }) {
                            self.mode = FollowMode::History;
                            self.mode_epoch = self.mode_epoch.wrapping_add(1);
                        }
                    }
                }
            }
            Err(reason) => {
                let rejection = LoopRejection { request, reason };
                let same = self.looping.rejected.is_some_and(|old| old.reason == reason && old.request.bits() == request.bits());
                if !same {
                    self.looping.rejected = Some(rejection);
                    self.looping.reject_epoch = self.looping.reject_epoch.wrapping_add(1);
                }
                if let Some((start, end)) = self.looping.effective {
                    let valid = start >= frontier.retained_start && end <= frontier.committed_end;
                    if !valid && !self.looping.expired {
                        self.looping.expired = true;
                        self.looping.loop_epoch = self.looping.loop_epoch.wrapping_add(1);
                    }
                }
            }
        }
    }

    fn loop_candidate(&self, request: LoopRequest) -> Result<Option<(u64, u64)>, LoopRefusal> {
        if !request.enabled {
            return Ok(None);
        }
        let (start_s, end_s) = (request.in_seconds, request.out_seconds);
        if !start_s.is_finite() || !end_s.is_finite() || start_s < 0.0 || end_s < 0.0 {
            return Err(LoopRefusal::NotFinite);
        }
        let frontier = self.frontier;
        let rate = self.sample_rate as f64;
        let start = (start_s as f64 * rate).floor() as u64;
        let end = if end_s <= start_s {
            if frontier.terminal == PagedTerminal::Open {
                return Err(LoopRefusal::OpenEnd);
            }
            frontier.committed_end
        } else {
            (end_s as f64 * rate).ceil() as u64
        };
        if start < frontier.retained_start || end > frontier.committed_end {
            return Err(LoopRefusal::OutsideCommitted);
        }
        if end < start.saturating_add(2) {
            return Err(LoopRefusal::TooShort);
        }
        Ok(Some((start, end)))
    }

    /// One pass. `busy`: a load, eject or clip cue waits for the quiet
    /// point. Returns `(position seconds, committed end seconds)`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(&mut self, position: &mut f64, gain: &mut f32, motion: &mut Motion, busy: bool, controls: DeckControls, device_rate: f32, frames: usize, mut write: impl FnMut(usize, [[f32; 2]; 5])) -> (f32, f32) {
        let request = LoopRequest { in_seconds: controls.loop_in, out_seconds: controls.loop_out, enabled: controls.loop_enabled };
        self.requested = Some(request);
        self.update_loop(request, position, !motion.loop_suspended && !(controls.scratch && !busy));
        let frames = frames.min(crate::CHUNK);
        // The nominal ratio this chunk steps with, sampled once, before any
        // early return, so every rendered observation carries its own.
        let rate = self.sample_rate as f64;
        let nominal = rate / device_rate.max(1.0) as f64;
        self.source_frames_per_output_frame = nominal * signed_speed(controls.speed);
        let busy_now = busy || self.pending_seek.is_some();
        let mut steps = [0.0f64; crate::CHUNK];
        let plan = plan_motion(motion, *position, f64::INFINITY, &controls, nominal, rate, device_rate, busy_now, frames, &mut steps);
        self.free = plan.constant.is_none();
        if let Some(step) = plan.constant {
            steps[..frames].fill(step);
        } else if let Some(last) = steps[..frames].last() {
            // Under the hand the observation carries the signed step.
            self.source_frames_per_output_frame = *last;
        }
        if self.free {
            // A hand leaves the live edge: the record is History from here,
            // with the mix fallback, as after any accepted move.
            if matches!(self.mode, FollowMode::Tail { .. }) {
                self.mode = FollowMode::History;
                self.mode_epoch = self.mode_epoch.wrapping_add(1);
            }
            if self.policy != StemPolicy::Mixed {
                self.policy = StemPolicy::Mixed;
                self.stem_waiting = false;
            }
            // The head stays on committed frames: from the live edge (or past
            // a frontier that is still open) a hand retreats into resident audio.
            let (low, high) = self.free_bounds();
            *position = position.clamp(low, high);
        }
        // The loop stands aside from the first touch; after the hand it comes
        // back where the head already is inside it, else folded under a dip.
        let mut dip_for_loop = false;
        if !self.free && motion.loop_suspended {
            match self.active_loop() {
                Some((start, end)) if *position < start as f64 || *position >= end as f64 => {
                    if *gain <= 0.0 {
                        *position = fold(*position, start, end);
                        motion.loop_suspended = false;
                    } else {
                        dip_for_loop = true;
                    }
                }
                _ => motion.loop_suspended = false,
            }
        }
        if self.loop_suspended != motion.loop_suspended {
            self.loop_suspended = motion.loop_suspended;
            // Observers see the effective loop leave and come back.
            self.looping.loop_epoch = self.looping.loop_epoch.wrapping_add(1);
        }
        let seconds = |position: f64, end: u64| ((position / rate) as f32, (end as f64 / rate) as f32);
        if self.looping.expired {
            // Blocked at once: no PCM from a range that is no longer valid.
            *gain = 0.0;
            for frame in 0..frames {
                write(frame, [[0.0; 2]; 5]);
            }
            self.readiness = PagedReadiness::ExpiredLoop;
            self.ready_frames = 0;
            self.stems_available = false;
            self.refresh_stem_ready(*position);
            self.eligible = false;
            self.advancing = false;
            return seconds(*position, self.frontier.committed_end);
        }
        // Enter the effective loop before any sample: after an accepted seek
        // or start outside it, no out-of-loop PCM is read. A hand holds the
        // record wherever it is; the fold waits for the transport.
        if let Some((start, end)) = self.sampling_loop().filter(|_| !self.free) {
            *position = fold(*position, start, end);
        }
        let step = 1.0 / DECK_DIP_FRAMES as f32;
        let target = if dip_for_loop { 0.0 } else { plan.target };
        let moving = plan.moves && target > 0.0;
        // Key lock or a key shift: the head moves at the transport's step,
        // the grains read at the pitch's, folded into the loop.
        let grain_step = crate::deck::stretch(&mut motion.grains, &controls, plan.constant.filter(|_| moving), *position, rate);
        let grain_loop = self.sampling_loop();
        // Decided for the whole chunk before any sample: all four stems at
        // every position (under StemReady a verified durable gap counts as
        // playable silence), or the mix alone. Only a chunk that reads a real
        // stem frame reports stems available.
        let gaps = matches!(self.policy, StemPolicy::StemReady { .. });
        let (prefix, real) = self.stems_cover_chunk(*position, &steps, moving, frames, gaps);
        // Mixed: the whole chunk from stems, or the mix alone.
        let mut separated = prefix == frames;
        self.stems_available = separated && real;
        // StemReady: frames rendered before holding (all, unless starved).
        let mut limit = frames;
        let mut waiting_gap = false;
        let at = position.max(0.0).floor() as u64;
        self.refresh_stem_ready(*position);
        // At a verified end the gate steps aside, so the ordinary path
        // reports `Ended` (counted once, on the transition).
        let at_end = self.frontier.terminal != PagedTerminal::Open && at >= self.frontier.committed_end;
        if let (StemPolicy::StemReady { cushion }, false) = (self.policy, at_end) {
            // Playable ahead: stems and verified durable gaps, so a gap is
            // crossed rather than faded out before, and never counted as
            // model coverage in `stem_ready`.
            let (horizon, to_end) = self.playable_horizon(at);
            let ahead = horizon.saturating_sub(at);
            // Real-audio resume keeps the cushion (or a verified end).
            if self.stem_waiting && prefix > 0 && (ahead >= cushion || to_end) {
                self.stem_waiting = false;
            }
            if prefix == 0 {
                self.stem_waiting = true;
            }
            // Still waiting for real audio's cushion: verified silence at the
            // play head still advances (a gap-only prefix); real audio does not.
            let gap_only = if self.stem_waiting { self.gap_prefix(*position, &steps, moving, frames) } else { 0 };
            if self.stem_waiting && gap_only == 0 {
                // Held: silence, the source clock stopped, no mix fallback.
                *gain = 0.0;
                for frame in 0..frames {
                    write(frame, [[0.0; 2]; 5]);
                }
                self.stems_available = false;
                self.readiness = PagedReadiness::StemWaiting;
                self.ready_frames = 0;
                self.eligible = false;
                self.advancing = false;
                return seconds(*position, self.frontier.committed_end);
            }
            if self.stem_waiting {
                // Known silence only, then the hold; the latch stays set.
                limit = gap_only;
                separated = false;
                self.stems_available = false;
                waiting_gap = true;
            } else {
                // The safe prefix: real stem PCM and verified gaps up to the
                // first sample the stem path cannot render; from there
                // silence, the source position held, never the raw mix.
                limit = prefix;
                separated = true;
                self.stems_available = real;
            }
        }
        let mut held = None;
        for frame in 0..frames {
            if frame >= limit && held.is_none() {
                *gain = 0.0;
                self.stem_waiting = true;
                held = Some(PagedReadiness::StemWaiting);
            }
            if held.is_some() {
                write(frame, [[0.0; 2]; 5]);
                continue;
            }
            *gain = if *gain < target { (*gain + step).min(target) } else { (*gain - step).max(target) };
            let mut values = [[0.0; 2]; 5];
            let moves = moving;
            // The hand's own envelope, 1 whenever the transport moves the record.
            let level = if !self.free && moves { motion.level_up() } else { motion.level };
            let heard = *gain * level;
            if *gain > 0.0 || moves {
                match self.sample(*position) {
                    Sample::Pcm(value) => match (motion.grains.as_mut(), grain_step) {
                        (Some(grains), Some(grain_step)) => {
                            // A tap on a frame not yet here is silent; the
                            // head's own frame decides readiness.
                            let place = |x: f64| grain_loop.map_or(x, |(start, end)| fold(x, start, end));
                            let mono = |x: f64| match self.sample(place(x)) {
                                Sample::Pcm(value) => (value[0] + value[1]) * 0.5,
                                _ => 0.0,
                            };
                            let taps = grains.next_aligned(*position, grain_step, crate::deck::STRETCH_ALIGN_SECONDS * rate, mono);
                            for (x, weight) in taps {
                                let x = place(x);
                                let weight = heard * weight;
                                if let Sample::Pcm(value) = self.sample(x) {
                                    values[0] = [values[0][0] + value[0] * weight, values[0][1] + value[1] * weight];
                                }
                                if separated {
                                    for (lane, value) in self.stem_values(x, weight).into_iter().enumerate() {
                                        values[lane + 1] = [values[lane + 1][0] + value[0], values[lane + 1][1] + value[1]];
                                    }
                                }
                            }
                        }
                        _ => {
                            values[0] = [value[0] * heard, value[1] * heard];
                            if separated {
                                let stems = self.stem_values(*position, heard);
                                values[1..].copy_from_slice(&stems);
                            }
                        }
                    },
                    Sample::Gap => {}
                    Sample::Hold(reason) => {
                        // Hard stop: silence, source position held, and a
                        // fade-in when data returns. The device clock runs on.
                        *gain = 0.0;
                        held = Some(reason);
                    }
                }
            }
            write(frame, values);
            if held.is_none() && moves {
                let before = *position;
                *position = self.step(*position, steps[frame]);
                if self.free {
                    if *position != before + steps[frame] {
                        // A free bound stopped the record: it stands there.
                        motion.velocity = 0.0;
                    }
                    motion.follow_level(*position - before, nominal);
                }
            } else if self.free {
                motion.follow_level(0.0, nominal);
            }
        }
        if held.is_some() && self.free {
            // Data behind (or ahead of) the hand is missing: the record
            // stops there until the pages arrive; the hand takes it again.
            motion.velocity = 0.0;
        }
        // The final, normalized position decides this observation.
        let (mut readiness, mut ready) = match held {
            Some(reason) => (reason, 0),
            None => self.survey(*position),
        };
        // A gap-only buffer that ends on real audio still below its cushion
        // reports the wait, not the PCM it has not played.
        if waiting_gap && readiness == PagedReadiness::Ready {
            (readiness, ready) = (PagedReadiness::StemWaiting, 0);
        }
        if let PagedReadiness::Ended(_) = readiness {
            if !matches!(self.readiness, PagedReadiness::Ended(_)) {
                self.boundary_epoch = self.boundary_epoch.wrapping_add(1);
            }
            // The owner's own terminal boundary: a drained Tail is History.
            if matches!(self.mode, FollowMode::Tail { .. }) {
                self.mode = FollowMode::History;
                self.mode_epoch = self.mode_epoch.wrapping_add(1);
            }
        }
        self.readiness = readiness;
        self.ready_frames = ready;
        // The readback follows the post-buffer position, as readiness does.
        self.refresh_stem_ready(*position);
        self.eligible = target > 0.0 && readiness == PagedReadiness::Ready;
        // Under the hand the record advances while it actually moves.
        let driven = if self.free { motion.velocity != 0.0 } else { controls.play };
        self.advancing = target > 0.0 && driven && matches!(readiness, PagedReadiness::Ready | PagedReadiness::Gap);
        seconds(*position, self.frontier.committed_end)
    }
}

pub(crate) fn group_leased_from(group: &PageGroup, budget: &Arc<AudioBudget>) -> bool {
    group.keepalive().downcast_ref::<TrackLease>()
        .is_some_and(|lease| lease.is_on(budget) && lease.bytes() >= group.capacity_bytes())
}

pub(crate) fn lease_group(budget: &Arc<AudioBudget>) -> Result<PageGroup, String> {
    let lease = TrackLease::reserve(budget, PAGED_GROUP_PAGES as usize * PAGE_BYTES)?;
    PageGroup::new(PAGED_GROUP_PAGES, Box::new(lease)).map_err(|error| format!("page group: {error:?}"))
}

/// Why a paged source was not prepared; the group comes back unchanged.
pub struct PrepareRefusal {
    pub reason: String,
    pub group: PageGroup,
}

pub(crate) fn prepare(budget: &Arc<AudioBudget>, group: PageGroup, source_sequence: u64, load: ConsumerLoad, snapshot: &TimelineSnapshot, start: PagedStart) -> Result<Box<PagedSource>, PrepareRefusal> {
    let refuse = |reason: String, group: PageGroup| Err(PrepareRefusal { reason, group });
    if !group_leased_from(&group, budget) {
        return refuse("the page group is not leased from this audio device's budget for its whole capacity".into(), group);
    }
    if group.capacity() > PAGED_GROUP_PAGES {
        return refuse(format!("a page group holds at most {PAGED_GROUP_PAGES} pages"), group);
    }
    if group.outstanding() != 0 {
        return refuse("the page group already has pages; prepare a source on a fresh group".into(), group);
    }
    if source_sequence == 0 {
        return refuse("a source sequence is at least 1".into(), group);
    }
    let frontier = match PagedFrontier::from_snapshot(snapshot) {
        Ok(frontier) => frontier,
        Err(reason) => return refuse(reason, group),
    };
    let sample_rate = snapshot.audio.map(|audio| audio.sample_rate).unwrap_or(0);
    if !(8_000..=384_000).contains(&sample_rate) {
        return refuse(format!("source rate {sample_rate} Hz is outside 8 kHz to 384 kHz"), group);
    }
    let mut source = Box::new(PagedSource {
        key: PagedSourceKey { source_sequence, media: snapshot.media, load },
        sample_rate,
        frontier,
        seek_serial: 0,
        start_frame: frontier.retained_start,
        slots: std::array::from_fn(|_| None),
        order: [0; MAX_WINDOW_SLOTS],
        resident: 0,
        current: [0; MAX_WINDOW_SLOTS],
        current_len: 0,
        cursor: 0,
        notes: [None; MAX_PAGED_NOTES],
        notes_revision: 0,
        pending_seek: None,
        mode: FollowMode::History,
        mode_epoch: 0,
        seek_epoch: 0,
        cue_epoch: 0,
        tail_epoch: 0,
        looping: PagedLoop::default(),
        loop_key: None,
        requested: None,
        boundary_epoch: 0,
        readiness: PagedReadiness::Buffering,
        ready_frames: 0,
        eligible: false,
        advancing: false,
        source_frames_per_output_frame: 0.0,
        stems: std::array::from_fn(|_| None),
        stems_available: false,
        policy: StemPolicy::Mixed,
        stem_waiting: false,
        stem_ready: None,
        free: false,
        loop_suspended: false,
        group,
    });
    // The start is fixed with serial 0 before any page can be read for it.
    let chosen = match start {
        PagedStart::History { frame } if frame >= frontier.retained_start && frame <= frontier.committed_end => Ok((frame, FollowMode::History)),
        PagedStart::History { .. } => Err("the start frame is outside the retained and committed range".to_string()),
        PagedStart::Tail { prebuffer } => source.tail_frame(prebuffer)
            .map(|frame| (frame, FollowMode::Tail { prebuffer_frames: prebuffer }))
            .map_err(|refusal| format!("a tail start is not possible here: {refusal:?}")),
    };
    match chosen {
        Ok((frame, mode)) => {
            source.start_frame = frame;
            source.mode = mode;
            Ok(source)
        }
        Err(reason) => {
            let PagedSource { group, .. } = *source;
            Err(PrepareRefusal { reason, group })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::{DeckSource, DeckState, MotionState};
    use makepad_stage_asset_stream::{AudioCaps, ConsumerId, Origin, PageRef, PublicationSeq, TimeBase, TimelineId};

    fn source(tail: bool) -> Box<PagedSource> {
        source_in(tail, TimelineState::Recording)
    }

    fn source_in(tail: bool, state: TimelineState) -> Box<PagedSource> {
        let budget = Arc::new(AudioBudget::default());
        let group = lease_group(&budget).unwrap();
        let media = MediaIdentity::Growing { asset: makepad_asset_data::AssetId::from_bytes([7; 16]), incarnation: 1, timeline: TimelineId(1) };
        let snapshot = TimelineSnapshot {
            media,
            publication: PublicationSeq(1),
            time_base: TimeBase::sample_rate(48_000),
            retained_start: 48_000,
            committed_end: 48_000 * 10,
            audio: Some(AudioCaps { sample_rate: 48_000, source_channels: 2 }),
            video: None,
            origin: Origin::Indexed { store: std::path::PathBuf::from("/nonexistent"), root: Some(PageRef { digest: makepad_asset_data::BlobId::from_bytes([9; 32]), bytes: 1, range: SampleRange { start: 48_000, end: 48_000 * 10 }, height: 0, segments: 1 }) },
            state,
        };
        let start = if tail { PagedStart::Tail { prebuffer: 4_800 } } else { PagedStart::History { frame: 48_000 * 5 } };
        let load = ConsumerLoad { consumer: ConsumerId(1), load: 1 };
        prepare(&budget, group, 1, load, &snapshot, start).map_err(|refusal| refusal.reason).unwrap()
    }

    pub(super) fn source_in_for(state: TimelineState) -> Box<PagedSource> {
        source_in(false, state)
    }

    fn controls(play: bool, scratch: bool, scratch_at: f32) -> DeckControls {
        DeckControls { play, speed: 1.0, loop_in: 0.0, loop_out: 0.0, loop_enabled: false, scratch, scratch_at, brake: false, fling: 0.0, key: 0.0, keylock: false }
    }

    #[test]
    fn a_free_record_stops_at_the_retained_start_and_the_committed_end() {
        let mut paged = source(false);
        paged.free = true;
        assert_eq!(paged.next_position(48_010.0, -50.0).0, 48_000.0);
        // The last committed frame, never the frontier itself.
        assert_eq!(paged.next_position(479_990.0, 50.0).0, 479_999.0);
        assert_eq!(paged.next_position(100_000.0, -2.5).0, 99_997.5);
        paged.free = false;
        // The transport keeps its old arithmetic.
        assert_eq!(paged.next_position(100_000.0, 2.5).0, 100_002.5);
    }

    #[test]
    fn a_hand_leaves_the_live_edge_and_holds_where_pages_are_missing() {
        let mut state = DeckState::fresh();
        let paged = source(true);
        state.position = paged.start_frame() as f64;
        state.track = Some(DeckSource::Paged(paged));
        state.sequence = 1;
        let hand = (state.position / 48_000.0) as f32;
        let (mut left, mut right) = ([0.0f32; crate::CHUNK], [0.0f32; crate::CHUNK]);
        let before = state.position;
        crate::deck::render(&mut state, controls(true, true, hand - 0.5), 48_000.0, crate::CHUNK, &mut left, &mut right);
        let Some(DeckSource::Paged(paged)) = &state.track else { unreachable!() };
        assert_eq!(paged.mode(), FollowMode::History, "a hand leaves the live edge");
        assert!(matches!(state.motion.state, MotionState::Scratch { .. }));
        // No page is resident: silence, the head held, the hand's speed reset.
        assert!(left.iter().all(|value| *value == 0.0));
        assert_eq!(state.position, before);
        assert_eq!(state.motion.velocity, 0.0);
        assert_eq!(paged.readiness().0, PagedReadiness::Buffering);
    }
}

#[cfg(test)]
mod end_tests {
    use super::*;
    use crate::deck::{DeckSource, DeckState};

    #[test]
    fn a_hand_pulls_the_record_back_off_the_end() {
        let mut state = DeckState::fresh();
        let paged = super::tests::source_in_for(TimelineState::Complete);
        let end = paged.frontier().committed_end as f64;
        state.position = end;
        state.track = Some(DeckSource::Paged(paged));
        state.sequence = 1;
        let controls = |scratch_at: f32| DeckControls { play: false, speed: 1.0, loop_in: 0.0, loop_out: 0.0, loop_enabled: false, scratch: true, scratch_at, brake: false, fling: 0.0, key: 0.0, keylock: false };
        let (mut left, mut right) = ([0.0f32; crate::CHUNK], [0.0f32; crate::CHUNK]);
        let hand = (end / 48_000.0) as f32;
        crate::deck::render(&mut state, controls(hand), 48_000.0, crate::CHUNK, &mut left, &mut right);
        for step in 1..8 {
            crate::deck::render(&mut state, controls(hand - step as f32 * 0.1), 48_000.0, crate::CHUNK, &mut left, &mut right);
        }
        assert!(state.position < end, "the head left the end: {} of {end}", state.position);
    }

    #[test]
    fn a_hand_retreats_from_the_open_edge_and_never_reads_past_it() {
        let mut state = DeckState::fresh();
        let paged = super::tests::source_in_for(TimelineState::Recording);
        let end = paged.frontier().committed_end;
        state.position = end as f64;
        state.track = Some(DeckSource::Paged(paged));
        state.sequence = 1;
        let controls = |scratch_at: f32| DeckControls { play: true, speed: 1.0, loop_in: 0.0, loop_out: 0.0, loop_enabled: false, scratch: true, scratch_at, brake: false, fling: 0.0, key: 0.0, keylock: false };
        let (mut left, mut right) = ([0.0f32; crate::CHUNK], [0.0f32; crate::CHUNK]);
        crate::deck::render(&mut state, controls((end as f64 / 48_000.0) as f32 - 1.0), 48_000.0, crate::CHUNK, &mut left, &mut right);
        assert!(state.position <= (end - 1) as f64, "the head is on committed frames: {}", state.position);
    }

    #[test]
    fn a_suspended_loop_is_not_reported_or_sampled() {
        let mut paged = super::tests::source_in_for(TimelineState::Complete);
        paged.looping.effective = Some((100_000, 200_000));
        assert_eq!(paged.neighbour(199_999), 100_000);
        paged.loop_suspended = true;
        assert_eq!(paged.neighbour(199_999), 200_000);
        assert_eq!(paged.looping().effective, None);
        assert_eq!(paged.sampling_loop(), None);
    }


    #[test]
    fn a_loop_becoming_valid_under_a_hand_never_jumps_the_head() {
        let mut paged = super::tests::source_in_for(TimelineState::Complete);
        let mut position = 300_000.0;
        let request = LoopRequest { in_seconds: 2.0, out_seconds: 3.0, enabled: true };
        paged.update_loop(request, &mut position, false);
        assert_eq!(position, 300_000.0, "held by the hand");
        assert!(paged.looping.effective.is_some());
        let mut position = 300_000.0;
        paged.loop_key = None;
        paged.looping.effective = None;
        paged.update_loop(request, &mut position, true);
        assert!((96_000.0..144_000.0).contains(&position), "without a hand the loop folds: {position}");
    }


    #[test]
    fn a_tail_after_a_loop_turned_off_is_judged_against_the_current_controls() {
        let mut paged = super::tests::source_in_for(TimelineState::Recording);
        let mut position = 150_000.0;
        let on = LoopRequest { in_seconds: 2.0, out_seconds: 3.0, enabled: true };
        paged.apply_request(on, &mut position, true);
        assert!(paged.active_loop().is_some());
        let origin = PageOrigin { route: 0, slot: 0, incarnation: 1, target: 0, continuity: 0, origin_generation: 1, source: paged.key(), ticket: 1 };
        let tail = PendingSeek { origin, kind: PageOpKind::Tail, expected_serial: paged.seek_serial(), frames: 4_800 };
        // Judged against the last rendered request, the loop is still on.
        assert_eq!(paged.accept_seek(&tail), Err(PageRefusal::LoopActive));
        // The loop was turned off before the Tail was sent: judged now.
        let off = LoopRequest { enabled: false, ..on };
        paged.apply_request(off, &mut position, true);
        assert!(paged.accept_seek(&tail).is_ok());
    }

}
