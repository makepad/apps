//! The page window of one paged consumer: which exact PCM pages a playing
//! deck needs resident, what to read, what to retire.
//!
//! A pure, bounded planner. It holds metadata only — ranges, tags, tickets
//! and an `Arc` of the immutable timeline snapshot — and never owns a PCM
//! payload. It runs no clock: the caller hands it the authoritative
//! position and speed ([`WindowMotion`]) each plan.
//!
//! Desired CELLS are a grid; physical PAGES are what the reader returned
//! (a cell may take several pages across segment boundaries and gaps).
//! Every range (page or missing run) has a physical KEY: first whether it
//! is REQUIRED (it intersects the playhead's interpolation neighbourhood) —
//! a required run outranks every optional range regardless of cells — then
//! its cell's desired priority, then its distance from the neighbourhood (0
//! when overlapping it; frames ahead in the direction of travel count once,
//! frames behind twice). Required runs are also offered before any optional
//! one. When every slot is held and a
//! missing run has a smaller key, the plan retires ONE page per plan — the
//! largest-key page outside the neighbourhood whose key is strictly larger
//! than the smallest blocked key, and only when no retirement is already
//! under way. The freed slot serves exactly that run, so at unchanged
//! motion every eviction replaces a key by a strictly smaller one and
//! evictions cannot cycle (throughput: one eviction per retire receipt). `starved` means the neighbourhood itself
//! is missing with no read or retirement under way (audible buffering);
//! `prefetch_pressure` means only lower-priority prefetch did not fit. The
//! window never invents samples.
//!
//! Three states are kept apart:
//!
//! * **desired**: page-grid cells near the playhead (in the direction of
//!   travel, with interpolation neighbours) and near both loop boundaries,
//!   clipped to the committed range, recomputed each [`PageWindow::plan`];
//! * **in flight**: at most [`MAX_WINDOW_READS`] reader requests, each
//!   admitted explicitly ([`PageWindow::sent`]); a request the reader queue
//!   refused is derived again by the next plan, never duplicated;
//! * **resident**: fixed slots whose state changes only on the consumer's
//!   callback receipts. A page accepted from the reader becomes a
//!   `PendingInstall` [`PagePlacement`] with a fresh ticket; only the
//!   matching [`InstallReceipt`] makes it `Resident`. Retirement likewise
//!   goes `Retiring` → `Empty` only on the matching [`RetireReceipt`]. A slot
//!   is never assigned while occupied, so nothing is overwritten behind the
//!   consumer's back.
//!
//! Every asynchronous admission checks the exact [`ReadTag`] (media, load,
//! seek serial). After [`PageWindow::seek`] every earlier page, gap,
//! placement and read is stale: stale pages are refused, stale install
//! receipts turn into retirements, and every resident page of the old
//! serial is retired over the following plans (at most
//! [`MAX_PLAN_RETIREMENTS`] each). The identity is fixed per planner: new
//! media needs a new planner.
//!
//! Commands that need an exact realtime receipt: installing a
//! [`PagePlacement`] (answered by [`InstallReceipt`]) and retiring a
//! [`PageRetirement`] (answered by [`RetireReceipt`]). Everything else is
//! host-side.

use crate::identity::{ConsumerLoad, PublicationSeq, ReadTag};
use crate::limits::MAX_PAGE_FRAMES;
use crate::payload::PcmPage;
use crate::timeline::{GapReason, SampleRange, TimelineSnapshot};
use crate::transport::{LoopDefinition, LoopShape};
use std::sync::Arc;

/// Physical page slots a window may use.
pub const MAX_WINDOW_SLOTS: usize = 64;
/// Reader requests in flight (and returned by one plan) at most.
pub const MAX_WINDOW_READS: usize = 4;
/// Retirements one plan asks for at most.
pub const MAX_PLAN_RETIREMENTS: usize = 4;
/// Gap and failure notes kept for the current serial.
pub const MAX_GAP_NOTES: usize = 16;
/// Frames either side of the playhead an interpolating resampler reads.
pub const INTERPOLATION_NEIGHBOURS: u64 = 4;
/// Largest speed accepted, in source frames per output frame.
pub const MAX_WINDOW_SPEED: f64 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowPolicy {
    /// Page grid size; one read covers at most one grid cell.
    pub page_frames: u32,
    /// Slots the consumer preallocated (1..=64).
    pub slots: u8,
    /// Frames kept ahead of / behind the playhead in the direction of travel.
    pub ahead_frames: u64,
    pub behind_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowError {
    BadPolicy(&'static str),
    BadMotion(&'static str),
    /// The snapshot is not the reader-verified successor of the current one.
    NotASuccessor(&'static str),
    /// A seek serial must strictly increase.
    SerialNotNewer,
    /// A ticket, request or admission counter would overflow.
    Exhausted,
    /// The snapshot does not validate or has no sound.
    BadSnapshot(&'static str),
    /// `sent` named no request awaiting admission.
    UnknownRequest,
}

impl WindowPolicy {
    pub fn validate(&self) -> Result<(), WindowError> {
        if self.page_frames < 256 || self.page_frames as usize > MAX_PAGE_FRAMES {
            return Err(WindowError::BadPolicy("page_frames must be 256..=16384"));
        }
        if self.slots == 0 || self.slots as usize > MAX_WINDOW_SLOTS {
            return Err(WindowError::BadPolicy("slots must be 1..=64"));
        }
        let capacity = self.slots as u64 * self.page_frames as u64;
        if self.ahead_frames > capacity || self.behind_frames > capacity {
            return Err(WindowError::BadPolicy("ahead/behind exceed the slot capacity"));
        }
        Ok(())
    }
}

/// The authoritative motion of the consumer this plan: where it plays and
/// how fast, signed (negative travels backwards on the current leg), and
/// the loop it is in, if any. The planner integrates nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowMotion {
    pub position: f64,
    pub source_frames_per_output_frame: f64,
    pub looping: Option<LoopDefinition>,
    /// For a bounce: whether the current leg runs towards the loop's end.
    pub leg_forward: bool,
}

/// Read `range` with exactly this `tag`, then report [`PageWindow::sent`].
///
/// An offer is EPHEMERAL: it is valid only until the next
/// [`PageWindow::plan`], which cancels every unsent request. Submit it to the
/// reader and call `sent` synchronously, before planning again; if the
/// reader queue is full, drop the offer and take whatever the next plan
/// offers. Never keep an offer and submit it after a later plan. (Placements
/// and retirements are different: they stay valid and are retried as held.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    pub id: u64,
    pub tag: ReadTag,
    pub range: SampleRange,
}

/// Install this page (the one just accepted) into `slot`. The consumer
/// answers with an [`InstallReceipt`] carrying the same slot and ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagePlacement {
    pub slot: u8,
    pub ticket: u64,
    pub tag: ReadTag,
    pub range: SampleRange,
}

/// Retire whatever occupies `slot` under `ticket`; answered by a
/// [`RetireReceipt`]. The payload goes back to the reader for its final drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRetirement {
    pub slot: u8,
    pub ticket: u64,
    pub range: SampleRange,
}

/// From the consumer's callback: the page of `ticket` is now in `slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallReceipt {
    pub slot: u8,
    pub ticket: u64,
}

/// From the consumer's callback: `slot` retired under `ticket` is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetireReceipt {
    pub slot: u8,
    pub ticket: u64,
}

/// Why a reader page was not placed. In every case the caller recycles the
/// payload; nothing was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageRefusal {
    /// Another media, load or seek serial.
    StaleTag,
    WrongRate,
    Malformed,
    /// No in-flight request expects a page starting here.
    Unrequested,
    /// Already resident or being installed.
    Duplicate,
    /// No empty slot; the request is re-read after settling.
    NoFreeSlot,
    /// The request was wanted when planned but the page is no longer in
    /// the desired window.
    OutOfWindow,
    /// The ticket counter is exhausted.
    Exhausted,
}

/// What an install receipt meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The page is resident for the current serial.
    Resident,
    /// The page arrived for a superseded serial: retire it at once.
    Stale(PageRetirement),
    /// The receipt names no pending install: a protocol error of the caller.
    Unknown,
}

/// What a `ReaderResult::Done` settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleOutcome {
    /// The request is answered in full; everything was placed or noted.
    Complete,
    /// Part of it was not placed (per-read bound, no slot, out of window):
    /// the rest is planned again from `resume` if still desired.
    Partial { resume: u64, replanned: bool },
    /// `reached` disagrees with what this window saw of the request: the
    /// request is dropped and planned afresh.
    Mismatch,
    /// No request of the current serial was waiting for an answer.
    Unexpected,
}

/// What the window knows about one frame, for buffering decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameCoverage {
    Resident { slot: u8 },
    Installing,
    /// The reader reported a gap here (source, admission or decode).
    Gap(GapReason),
    /// Reading failed here for the current serial.
    Failed,
    /// Not committed, or not read yet: buffer.
    Missing,
}

/// One plan: reads to send and retirements to issue. Fixed capacity, no
/// allocation. The reads are ephemeral offers, derived and reprioritized by
/// every plan (see [`ReadRequest`]); the retirements are exact and held
/// until their receipt or explicit refusal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowPlan {
    reads: [Option<ReadRequest>; MAX_WINDOW_READS],
    retirements: [Option<PageRetirement>; MAX_PLAN_RETIREMENTS],
}

impl WindowPlan {
    pub fn reads(&self) -> impl Iterator<Item = &ReadRequest> {
        self.reads.iter().flatten()
    }
    pub fn retirements(&self) -> impl Iterator<Item = &PageRetirement> {
        self.retirements.iter().flatten()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowStatus {
    pub resident: u8,
    pub installing: u8,
    pub retiring: u8,
    pub in_flight: u8,
    pub seek_serial: u64,
    pub closing: bool,
    /// The playhead's interpolation neighbourhood is not covered (resident,
    /// installing or a noted gap) and no read, install or retirement is under
    /// way that could cover it: audible buffering until something changes.
    pub starved: bool,
    /// Some desired run did not fit this plan beyond the one slot being
    /// freed for the most important of them (at most one eviction per
    /// plan). Playback of what is resident is unaffected.
    pub prefetch_pressure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Empty,
    PendingInstall { ticket: u64, tag: ReadTag, range: SampleRange },
    Resident { ticket: u64, tag: ReadTag, range: SampleRange },
    Retiring { ticket: u64, range: SampleRange, was: Option<(u64, ReadTag)> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InFlight {
    id: u64,
    range: SampleRange,
    /// `None` while planned but not admitted by the reader queue; the
    /// admission ordinal once `sent` — the reader answers in this order.
    ordinal: Option<u64>,
    /// How far the reader's answer stream for this request has come (every
    /// page and gap seen, placed or not).
    cursor: u64,
    /// The first frame the reader answered that was NOT placed or noted
    /// (refused for lack of a slot, out of window, ...): where a partial
    /// request resumes.
    unplaced: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Note {
    Gap(GapReason),
    Failed,
}

/// What a window noted of a range it did not get as audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowNoteKind {
    /// The reader reported a gap there, for this reason.
    Gap(GapReason),
    /// The read failed there.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowNote {
    pub range: SampleRange,
    pub kind: WindowNoteKind,
}

/// A copy of a window's notes for one exact read ([`PageWindow::notes_snapshot`]).
///
/// `notes` is the window's own ring of [`MAX_GAP_NOTES`] slots exactly as it
/// stands: empty slots stay `None`, and the order is the slots' physical
/// order (not age), which is the order [`PageWindow::coverage`] consults
/// them in, so for overlapping notes the first in `notes` is the one that
/// answers. The notes are raw: `NotCommitted` and `Decode` gaps and notes
/// outside the current committed frontier are included unchanged. Nothing
/// here ages or changes on its own; a caller that mirrors it replaces its
/// whole copy with each new snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowNotes {
    /// The read the notes belong to: the window's current tag.
    pub tag: ReadTag,
    pub notes: [Option<WindowNote>; MAX_GAP_NOTES],
}

pub struct PageWindow {
    snapshot: Arc<TimelineSnapshot>,
    load: ConsumerLoad,
    policy: WindowPolicy,
    tag: ReadTag,
    slots: [Slot; MAX_WINDOW_SLOTS],
    reads: [Option<InFlight>; MAX_WINDOW_READS],
    notes: [Option<(SampleRange, Note)>; MAX_GAP_NOTES],
    next_note: usize,
    /// The cells the last plan wanted, highest priority first.
    desired: [u64; MAX_WINDOW_SLOTS],
    desired_count: usize,
    next_ticket: u64,
    next_request: u64,
    next_ordinal: u64,
    closing: bool,
    starved: bool,
    prefetch_pressure: bool,
}

fn overlaps(a: &SampleRange, b: &SampleRange) -> bool {
    a.start < b.end && b.start < a.end
}

fn bump(counter: &mut u64) -> Result<u64, WindowError> {
    let value = *counter;
    *counter = value.checked_add(1).ok_or(WindowError::Exhausted)?;
    Ok(value)
}

/// Desired cells in priority order, deduplicated, at most `budget`.
struct Cells {
    cells: [u64; MAX_WINDOW_SLOTS],
    count: usize,
    budget: usize,
}

impl Cells {
    fn push(&mut self, cell: u64) {
        if self.count < self.budget && !self.cells[..self.count].contains(&cell) {
            self.cells[self.count] = cell;
            self.count += 1;
        }
    }
    /// Push cells in order until `limit` cells are held in total.
    fn extend(&mut self, cells: impl Iterator<Item = u64>, limit: usize) {
        for cell in cells {
            if self.count >= limit.min(self.budget) {
                break;
            }
            self.push(cell);
        }
    }
}

impl PageWindow {
    /// A window over a validated snapshot for exactly this load, starting at
    /// seek serial 0 (the reader's initial serial for a newly opened load).
    pub fn new(snapshot: Arc<TimelineSnapshot>, load: ConsumerLoad, policy: WindowPolicy) -> Result<Self, WindowError> {
        policy.validate()?;
        snapshot.validate().map_err(|_| WindowError::BadSnapshot("the snapshot does not validate"))?;
        if snapshot.audio.is_none() {
            return Err(WindowError::BadSnapshot("the timeline has no sound"));
        }
        let tag = ReadTag { media: snapshot.media, load, seek_serial: 0 };
        Ok(Self {
            snapshot,
            load,
            policy,
            tag,
            slots: [Slot::Empty; MAX_WINDOW_SLOTS],
            reads: [None; MAX_WINDOW_READS],
            notes: [None; MAX_GAP_NOTES],
            next_note: 0,
            desired: [0; MAX_WINDOW_SLOTS],
            desired_count: 0,
            next_ticket: 1,
            next_request: 1,
            next_ordinal: 1,
            closing: false,
            starved: false,
            prefetch_pressure: false,
        })
    }

    pub fn tag(&self) -> ReadTag {
        self.tag
    }

    pub fn snapshot(&self) -> &Arc<TimelineSnapshot> {
        &self.snapshot
    }

    /// A copy of the current tag and of every note slot, holes and physical
    /// order kept (see [`WindowNotes`]). Read-only; the window keeps no
    /// revision of its notes.
    pub fn notes_snapshot(&self) -> WindowNotes {
        let notes = self.notes.map(|note| {
            note.map(|(range, note)| WindowNote {
                range,
                kind: match note {
                    Note::Gap(reason) => WindowNoteKind::Gap(reason),
                    Note::Failed => WindowNoteKind::Failed,
                },
            })
        });
        WindowNotes { tag: self.tag, notes }
    }

    fn slot_count(&self) -> usize {
        self.policy.slots as usize
    }

    /// Adopt a newer publication. `advanced` must be the publication of the
    /// reader's `ReaderResult::Advanced` for this exact load: the reader has
    /// verified the index extension; this only checks the structure matches.
    pub fn advance(&mut self, snapshot: Arc<TimelineSnapshot>, load: ConsumerLoad, advanced: PublicationSeq) -> Result<(), WindowError> {
        if load != self.load || advanced != snapshot.publication {
            return Err(WindowError::NotASuccessor("not the reader-verified publication of this load"));
        }
        self.snapshot.check_successor(&snapshot).map_err(|_| WindowError::NotASuccessor("structural successor check failed"))?;
        self.snapshot = snapshot;
        // Frames that were not committed may be now.
        for note in &mut self.notes {
            if matches!(note, Some((_, Note::Gap(GapReason::NotCommitted)))) {
                *note = None;
            }
        }
        Ok(())
    }

    /// Everything read, placed or noted so far belongs to the old serial.
    /// Reads, notes and the desired set are dropped; slots keep their
    /// physical state and are retired over the following plans; stale
    /// install receipts retire. Results of old-serial reads fail the tag
    /// check wherever they arrive.
    pub fn seek(&mut self, seek_serial: u64) -> Result<(), WindowError> {
        if seek_serial <= self.tag.seek_serial {
            return Err(WindowError::SerialNotNewer);
        }
        self.tag.seek_serial = seek_serial;
        self.reads = [None; MAX_WINDOW_READS];
        self.notes = [None; MAX_GAP_NOTES];
        self.desired_count = 0;
        Ok(())
    }

    /// Stop wanting anything: later plans only retire. Idle when every slot
    /// is empty and no read is in flight.
    pub fn close(&mut self) {
        self.closing = true;
        self.reads = [None; MAX_WINDOW_READS];
        self.desired_count = 0;
    }

    pub fn is_idle(&self) -> bool {
        self.reads.iter().all(Option::is_none) && self.slots.iter().all(|slot| *slot == Slot::Empty)
    }

    pub fn status(&self) -> WindowStatus {
        let mut status = WindowStatus { seek_serial: self.tag.seek_serial, closing: self.closing, starved: self.starved,
            prefetch_pressure: self.prefetch_pressure,
            ..WindowStatus::default()
        };
        for slot in &self.slots {
            match slot {
                Slot::Resident { .. } => status.resident += 1,
                Slot::PendingInstall { .. } => status.installing += 1,
                Slot::Retiring { .. } => status.retiring += 1,
                Slot::Empty => {}
            }
        }
        status.in_flight = self.reads.iter().flatten().count() as u8;
        status
    }

    // -----------------------------------------------------------------------
    // coverage
    // -----------------------------------------------------------------------

    pub fn coverage(&self, frame: u64) -> FrameCoverage {
        for (index, slot) in self.slots.iter().enumerate() {
            match slot {
                Slot::Resident { tag, range, .. } if *tag == self.tag && range.contains(frame) => {
                    return FrameCoverage::Resident { slot: index as u8 }
                }
                Slot::PendingInstall { tag, range, .. } if *tag == self.tag && range.contains(frame) => return FrameCoverage::Installing,
                _ => {}
            }
        }
        for (range, note) in self.notes.iter().flatten() {
            if range.contains(frame) {
                return match note {
                    Note::Gap(reason) => FrameCoverage::Gap(*reason),
                    Note::Failed => FrameCoverage::Failed,
                };
            }
        }
        FrameCoverage::Missing
    }

    /// The end of the covered run starting at `at` (current-serial slots and
    /// notes), or `at` itself when nothing covers it. Bounded: at most one
    /// step per slot and note.
    fn covered_to(&self, mut at: u64, end: u64) -> u64 {
        for _ in 0..(MAX_WINDOW_SLOTS + MAX_GAP_NOTES) {
            if at >= end {
                break;
            }
            let next = self
                .slots
                .iter()
                .filter_map(|slot| match slot {
                    Slot::Resident { tag, range, .. } | Slot::PendingInstall { tag, range, .. } if *tag == self.tag => Some(*range),
                    _ => None,
                })
                .chain(self.notes.iter().flatten().map(|(range, _)| *range))
                .filter(|range| range.contains(at))
                .map(|range| range.end)
                .max();
            match next {
                Some(next) => at = next,
                None => break,
            }
        }
        at
    }

    fn is_desired(&self, range: &SampleRange) -> bool {
        let pf = self.policy.page_frames as u64;
        let (first, last) = (range.start / pf, range.end.saturating_sub(1) / pf);
        self.desired[..self.desired_count].iter().any(|cell| *cell >= first && *cell <= last)
    }

    // -----------------------------------------------------------------------
    // planning
    // -----------------------------------------------------------------------

    fn cell_range(&self, cell: u64) -> Option<SampleRange> {
        let pf = self.policy.page_frames as u64;
        let start = cell.checked_mul(pf)?;
        let range = SampleRange { start, end: start.checked_add(pf)? };
        range.intersect(&self.snapshot.committed())
    }

    /// Check the motion against the current snapshot; nothing is retargeted.
    fn check_motion(&self, motion: &WindowMotion) -> Result<(), WindowError> {
        let speed = motion.source_frames_per_output_frame;
        if !motion.position.is_finite() || !speed.is_finite() {
            return Err(WindowError::BadMotion("position and speed must be finite"));
        }
        if speed.abs() > MAX_WINDOW_SPEED {
            return Err(WindowError::BadMotion("speed is out of range"));
        }
        let committed = self.snapshot.committed();
        // The committed end itself is a valid (end-of-file) position.
        if motion.position < committed.start as f64 || motion.position > committed.end as f64 {
            return Err(WindowError::BadMotion("the position is outside the committed range"));
        }
        if let Some(looping) = &motion.looping {
            if looping.range.is_empty() || !looping.range.within(&committed) {
                return Err(WindowError::BadMotion("the loop is empty or outside the committed range"));
            }
            if motion.position < looping.range.start as f64 || motion.position > looping.range.end as f64 {
                return Err(WindowError::BadMotion("the position is outside the loop"));
            }
        }
        // The critical set — the playhead's cell, its interpolation
        // neighbours' cells and, with a loop, both boundary cells — must fit
        // the slots; otherwise the motion is refused rather than planned
        // with a boundary silently missing.
        let (_, critical) = self.critical_cells(motion);
        if critical > self.slot_count() {
            return Err(WindowError::BadMotion("the playhead neighbourhood and loop boundaries need more slots than the policy has"));
        }
        Ok(())
    }

    /// The frame the plan is anchored on: a position at the end asks for the
    /// last frame, never beyond.
    fn playhead(&self, motion: &WindowMotion) -> u64 {
        let bounds = motion.looping.map_or(self.snapshot.committed(), |looping| looping.range);
        (motion.position as u64).min(bounds.end.saturating_sub(1)).max(bounds.start)
    }

    /// The frames an interpolating reader needs around the playhead.
    fn neighbourhood(&self, motion: &WindowMotion) -> SampleRange {
        let bounds = motion.looping.map_or(self.snapshot.committed(), |looping| looping.range);
        let at = self.playhead(motion);
        SampleRange {
            start: at.saturating_sub(INTERPOLATION_NEIGHBOURS).max(bounds.start),
            end: at.saturating_add(INTERPOLATION_NEIGHBOURS + 1).min(bounds.end),
        }
    }

    /// Distinct critical cells (at most four) and how many there are.
    fn critical_cells(&self, motion: &WindowMotion) -> ([u64; 4], usize) {
        let pf = self.policy.page_frames as u64;
        let near = self.neighbourhood(motion);
        let mut cells = [0u64; 4];
        let mut count = 0;
        let mut add = |cell: u64| {
            if !cells[..count].contains(&cell) {
                cells[count] = cell;
                count += 1;
            }
        };
        add(self.playhead(motion) / pf);
        add(near.start / pf);
        add(near.end.saturating_sub(1).max(near.start) / pf);
        if let Some(looping) = motion.looping {
            add(looping.range.start / pf);
            add((looping.range.end - 1) / pf);
        }
        (cells, count)
    }

    /// Desired cells, highest priority first:
    /// 1. the playhead's cell and its interpolation neighbours;
    /// 2. with a loop, the cell of the boundary the motion approaches and of
    ///    the other boundary;
    /// 3. the directional window ahead, leaving room for
    /// 4. a bounded window at each loop boundary (approached one first);
    /// 5. the rest of the ahead window, then behind.
    ///
    /// Both boundaries are always in, however large the loop; the loop is
    /// never required to fit.
    fn desired(&self, motion: &WindowMotion) -> Cells {
        let budget = self.slot_count();
        let mut cells = Cells { cells: [0; MAX_WINDOW_SLOTS], count: 0, budget };
        let committed = self.snapshot.committed();
        if self.closing || committed.is_empty() {
            return cells;
        }
        let pf = self.policy.page_frames as u64;
        let bounds = motion.looping.map_or(committed, |looping| looping.range);
        let forward = motion.source_frames_per_output_frame >= 0.0;
        // A position at the end asks for the last frame, never beyond.
        let position = (motion.position as u64).min(bounds.end - 1).max(bounds.start);
        let cell_of = |frame: u64| frame / pf;
        let here = cell_of(position);
        let span = |frames: u64| (frames + INTERPOLATION_NEIGHBOURS).div_ceil(pf) as usize;
        cells.push(here);
        cells.push(cell_of(position.saturating_add(INTERPOLATION_NEIGHBOURS).min(bounds.end - 1)));
        cells.push(cell_of(position.saturating_sub(INTERPOLATION_NEIGHBOURS).max(bounds.start)));
        let (ahead_frames, behind_frames) = (self.policy.ahead_frames + INTERPOLATION_NEIGHBOURS, self.policy.behind_frames + INTERPOLATION_NEIGHBOURS);
        let up_to = |frames: u64| cell_of(position.saturating_add(frames).min(bounds.end - 1));
        let down_to = |frames: u64| cell_of(position.saturating_sub(frames).max(bounds.start));
        let forward_cells = |frames: u64| (here + 1)..=up_to(frames);
        let backward_cells = |frames: u64| (down_to(frames)..here).rev();
        let mut edge = 0;
        let mut boundaries: [Option<(u64, u64, bool)>; 2] = [None, None];
        if let Some(looping) = motion.looping {
            let range = looping.range;
            let out_first = match looping.shape {
                LoopShape::Bounce => motion.leg_forward,
                _ => forward,
            };
            // (first cell, last cell, walking down from OUT)
            let out = (cell_of(range.end - 1), cell_of(range.end.saturating_sub(ahead_frames).max(range.start)), true);
            let inn = (cell_of(range.start), cell_of(range.start.saturating_add(ahead_frames).min(range.end - 1)), false);
            boundaries = if out_first { [Some(out), Some(inn)] } else { [Some(inn), Some(out)] };
            for (first, _, _) in boundaries.iter().flatten() {
                cells.push(*first);
            }
            edge = span(self.policy.ahead_frames).max(1).min((budget.saturating_sub(1) / 4).max(1));
        }
        let reserve = 2 * edge;
        if forward {
            cells.extend(forward_cells(ahead_frames), budget.saturating_sub(reserve));
        } else {
            cells.extend(backward_cells(ahead_frames), budget.saturating_sub(reserve));
        }
        for (first, last, down) in boundaries.iter().flatten() {
            let limit = cells.count + edge;
            if *down {
                cells.extend((*last..=*first).rev(), limit);
            } else {
                cells.extend(*first..=*last, limit);
            }
        }
        if forward {
            cells.extend(forward_cells(ahead_frames), budget);
            cells.extend(backward_cells(behind_frames), budget);
        } else {
            cells.extend(backward_cells(ahead_frames), budget);
            cells.extend(forward_cells(behind_frames), budget);
        }
        cells
    }

    /// One plan. Closing: retire up to [`MAX_PLAN_RETIREMENTS`] slots and
    /// nothing else; the motion is ignored (it may no longer be valid).
    /// Otherwise: validate the motion, remember the desired cells, drop every
    /// unsent request (they are derived afresh, so an obsolete partial never
    /// holds capacity ahead of a newly missing current span; sent requests
    /// are never touched), retire old-serial and undesired slots, offer reads
    /// for missing runs in key order while physical capacity allows, and evict
    /// for the most important run that did not fit.
    pub fn plan(&mut self, motion: WindowMotion) -> Result<WindowPlan, WindowError> {
        let mut plan = WindowPlan::default();
        let mut retire = 0;
        if self.closing {
            for index in 0..self.slot_count() {
                if retire == MAX_PLAN_RETIREMENTS {
                    break;
                }
                let Slot::Resident { ticket, tag, range } = self.slots[index] else { continue };
                let retire_ticket = bump(&mut self.next_ticket)?;
                self.slots[index] = Slot::Retiring { ticket: retire_ticket, range, was: Some((ticket, tag)) };
                plan.retirements[retire] = Some(PageRetirement { slot: index as u8, ticket: retire_ticket, range });
                retire += 1;
            }
            self.starved = false;
            self.prefetch_pressure = false;
            return Ok(plan);
        }
        self.check_motion(&motion)?;
        let cells = self.desired(&motion);
        self.desired = cells.cells;
        self.desired_count = cells.count;
        for read in &mut self.reads {
            if read.is_some_and(|read| read.ordinal.is_none()) {
                *read = None;
            }
        }
        let near = self.neighbourhood(&motion);
        let forward = motion.source_frames_per_output_frame >= 0.0;
        // Retirements: old serial first, then no longer wanted.
        for pass in 0..2 {
            for index in 0..self.slot_count() {
                if retire == MAX_PLAN_RETIREMENTS {
                    break;
                }
                let Slot::Resident { ticket, tag, range } = self.slots[index] else { continue };
                let stale = tag != self.tag;
                if (pass == 0 && stale) || (pass == 1 && !stale && !self.is_desired(&range)) {
                    let retire_ticket = bump(&mut self.next_ticket)?;
                    self.slots[index] = Slot::Retiring { ticket: retire_ticket, range, was: Some((ticket, tag)) };
                    plan.retirements[retire] = Some(PageRetirement { slot: index as u8, ticket: retire_ticket, range });
                    retire += 1;
                }
            }
        }
        // Physical capacity: empty slots not owed to a sent request (each
        // expects at least one page).
        let slots = self.slot_count();
        let empty = self.slots[..slots].iter().filter(|slot| **slot == Slot::Empty).count();
        let sent = self.reads.iter().flatten().count();
        let mut free = empty.saturating_sub(sent);
        // Missing runs in desired order (one per cell, anchored at the
        // neighbourhood); the smallest key that did not fit, and how many.
        let mut need: Option<(u8, usize, u64)> = None;
        let mut deficit = 0usize;
        let mut offered = 0;
        // Pass 0: only runs that intersect the playhead's interpolation
        // neighbourhood (required for playback), whatever their cell's rank.
        // Pass 1: everything else (optional prefetch). One run per cell per
        // pass; a required run is never counted twice.
        for (pass, priority) in (0..2u8).flat_map(|pass| (0..self.desired_count).map(move |priority| (pass, priority))) {
            if self.reads.iter().flatten().count() >= MAX_WINDOW_READS {
                break;
            }
            let Some(range) = self.cell_range(self.desired[priority]) else { continue };
            let anchor = near.start.clamp(range.start, range.end);
            let Some(run) = self.uncovered_run(range, anchor) else { continue };
            let required = overlaps(&run, &near);
            if required != (pass == 0) {
                continue;
            }
            if self.reads.iter().flatten().any(|read| overlaps(&read.range, &run)) {
                continue;
            }
            if free == 0 {
                let key = (pass, priority, Self::distance(&run, &near, forward));
                need = Some(need.map_or(key, |need| need.min(key)));
                deficit += 1;
                continue;
            }
            free -= 1;
            let id = bump(&mut self.next_request)?;
            let slot = self.reads.iter().position(Option::is_none).expect("fewer than MAX_WINDOW_READS in flight");
            self.reads[slot] = Some(InFlight { id, range: run, ordinal: None, cursor: run.start, unplaced: None });
            plan.reads[offered] = Some(ReadRequest { id, tag: self.tag, range: run });
            offered += 1;
        }
        // Eviction for progress, one at a time: only when a run is blocked
        // and no slot is already on its way back (a retirement under way is
        // capacity arriving). The single victim is the largest-key page
        // outside the neighbourhood whose key is strictly larger than the
        // smallest blocked key — the run the freed slot will serve, since
        // reads are offered in key order (required first, then cell rank,
        // one run per cell). So every eviction swaps a key for a strictly
        // smaller one: at unchanged motion evictions cannot cycle. The cost
        // is throughput: at most one eviction per retire receipt.
        let mut unserved = deficit;
        if let Some(need) = need {
            let retiring = self.slots[..slots].iter().filter(|slot| matches!(slot, Slot::Retiring { .. })).count();
            if retiring > 0 {
                unserved -= 1;
            } else if retire < MAX_PLAN_RETIREMENTS {
                let victim = (0..slots)
                    .filter_map(|index| match self.slots[index] {
                        Slot::Resident { tag, range, .. } if tag == self.tag && !overlaps(&range, &near) => {
                            // Never overlapping the neighbourhood, so always
                            // optional (class 1): any required run outranks it.
                            let key = (1u8, self.priority_of(&range).unwrap_or(usize::MAX), Self::distance(&range, &near, forward));
                            (key > need).then_some((key, index))
                        }
                        _ => None,
                    })
                    .max();
                if let Some((_, index)) = victim {
                    if let Slot::Resident { ticket, tag, range } = self.slots[index] {
                        let retire_ticket = bump(&mut self.next_ticket)?;
                        self.slots[index] = Slot::Retiring { ticket: retire_ticket, range, was: Some((ticket, tag)) };
                        plan.retirements[retire] = Some(PageRetirement { slot: index as u8, ticket: retire_ticket, range });
                        unserved -= 1;
                    }
                }
            }
        }
        // Truthful status: starvation is about the neighbourhood only.
        let near_covered = self.covered_to(near.start, near.end) >= near.end;
        let near_reading = self.reads.iter().flatten().any(|read| overlaps(&read.range, &near));
        let under_way = self.slots[..slots].iter().any(|slot| matches!(slot, Slot::Retiring { .. }));
        self.starved = !near_covered && !near_reading && !under_way;
        self.prefetch_pressure = unserved > 0;
        Ok(plan)
    }

    /// Distance of `range` from the neighbourhood: 0 when overlapping;
    /// frames ahead in the direction of travel count once, behind twice.
    fn distance(range: &SampleRange, near: &SampleRange, forward: bool) -> u64 {
        if overlaps(range, near) {
            return 0;
        }
        let (gap, ahead) = if range.start >= near.end {
            (range.start - near.end + 1, forward)
        } else {
            (near.start - range.end + 1, !forward)
        };
        if ahead {
            gap
        } else {
            gap.saturating_mul(2)
        }
    }

    /// Priority of a range: the best (lowest) desired index among the cells
    /// it overlaps; `None` when not desired.
    fn priority_of(&self, range: &SampleRange) -> Option<usize> {
        let pf = self.policy.page_frames as u64;
        let (first, last) = (range.start / pf, range.end.saturating_sub(1) / pf);
        self.desired[..self.desired_count].iter().position(|cell| *cell >= first && *cell <= last)
    }

    /// The first uncovered run of `cell` at or after `anchor`, else the first
    /// before it. Bounded by the slot and note counts.
    fn uncovered_run(&self, cell: SampleRange, anchor: u64) -> Option<SampleRange> {
        let run_from = |start: u64| {
            let end = self
                .slots
                .iter()
                .filter_map(|slot| match slot {
                    Slot::Resident { tag, range, .. } | Slot::PendingInstall { tag, range, .. } if *tag == self.tag => Some(range.start),
                    _ => None,
                })
                .chain(self.notes.iter().flatten().map(|(range, _)| range.start))
                .filter(|at| *at > start && *at < cell.end)
                .min()
                .unwrap_or(cell.end);
            SampleRange { start, end }
        };
        let at = self.covered_to(anchor, cell.end);
        if at < cell.end {
            return Some(run_from(at));
        }
        let at = self.covered_to(cell.start, anchor);
        // Everything from the anchor on is covered, so this run ends at or
        // before the anchor, where the covered part begins.
        (at < anchor).then(|| run_from(at))
    }

    /// The reader queue took request `id`: it gets the next admission
    /// ordinal, the order the reader will answer in. Call only after the
    /// queue accepted it; a refused send needs no call. Only the reads of the
    /// LATEST plan may be sent: unsent requests are derived afresh by every
    /// plan (never duplicated, since the reader never saw them), and an id
    /// from an older plan answers `UnknownRequest`.
    pub fn sent(&mut self, id: u64) -> Result<(), WindowError> {
        let index = self
            .reads
            .iter()
            .position(|read| read.is_some_and(|read| read.id == id && read.ordinal.is_none()))
            .ok_or(WindowError::UnknownRequest)?;
        let ordinal = bump(&mut self.next_ordinal)?;
        if let Some(read) = self.reads[index].as_mut() {
            read.ordinal = Some(ordinal);
        }
        Ok(())
    }

    /// The request the reader is answering now: the sent one with the
    /// smallest admission ordinal.
    fn head(&self) -> Option<usize> {
        (0..MAX_WINDOW_READS)
            .filter(|index| self.reads[*index].is_some_and(|read| read.ordinal.is_some()))
            .min_by_key(|index| self.reads[*index].and_then(|read| read.ordinal).unwrap_or(u64::MAX))
    }

    // -----------------------------------------------------------------------
    // reader results
    // -----------------------------------------------------------------------

    /// A reader page: it must carry the exact tag and rate and continue the
    /// answer of the head request. It is placed into an EMPTY slot as
    /// `PendingInstall` only if still in the desired window; resident only
    /// after its receipt. A refused page still advances the head's answer
    /// cursor (the reader moved on) and marks where the request resumes.
    pub fn accept(&mut self, page: &PcmPage) -> Result<PagePlacement, PageRefusal> {
        if page.tag() != self.tag {
            return Err(PageRefusal::StaleTag);
        }
        if Some(page.sample_rate()) != self.snapshot.audio.map(|audio| audio.sample_rate) {
            return Err(PageRefusal::WrongRate);
        }
        let frames = page.frames().len() as u64;
        let end = page.start().checked_add(frames).ok_or(PageRefusal::Malformed)?;
        let range = SampleRange { start: page.start(), end };
        if frames == 0 || frames as usize > MAX_PAGE_FRAMES || !range.within(&self.snapshot.committed()) {
            return Err(PageRefusal::Malformed);
        }
        let Some(head) = self.head() else {
            return Err(PageRefusal::Unrequested);
        };
        let read = self.reads[head].unwrap();
        if range.start != read.cursor || range.end > read.range.end {
            return Err(PageRefusal::Unrequested);
        }
        let refusal = {
            let tag = self.tag;
            let duplicate = self.slots.iter().any(|slot| match slot {
                Slot::Resident { tag: held, range: at, .. } | Slot::PendingInstall { tag: held, range: at, .. } => *held == tag && overlaps(at, &range),
                _ => false,
            });
            if duplicate {
                Some(PageRefusal::Duplicate)
            } else if !self.is_desired(&range) {
                Some(PageRefusal::OutOfWindow)
            } else if !self.slots[..self.slot_count()].contains(&Slot::Empty) {
                Some(PageRefusal::NoFreeSlot)
            } else {
                None
            }
        };
        let ticket = match refusal {
            None => bump(&mut self.next_ticket).map_err(|_| PageRefusal::Exhausted),
            Some(refusal) => Err(refusal),
        };
        let read = self.reads[head].as_mut().unwrap();
        read.cursor = range.end;
        let ticket = match ticket {
            Ok(ticket) => ticket,
            // A duplicate is already covered; anything else is not placed.
            Err(PageRefusal::Duplicate) => return Err(PageRefusal::Duplicate),
            Err(refusal) => {
                read.unplaced.get_or_insert(range.start);
                return Err(refusal);
            }
        };
        let slot = self.slots[..self.slot_count()].iter().position(|slot| *slot == Slot::Empty).unwrap();
        self.slots[slot] = Slot::PendingInstall { ticket, tag: self.tag, range };
        Ok(PagePlacement { slot: slot as u8, ticket, tag: self.tag, range })
    }

    /// A reader gap: admitted only for the current serial as the next part of
    /// the head request's answer, within its range. It is noted (metadata,
    /// never sound) and the answer cursor moves past it. `NotCommitted`
    /// means buffering; that note is dropped when a newer publication is
    /// adopted. `false` for a gap that answers nothing this window asked.
    pub fn gap(&mut self, tag: ReadTag, range: SampleRange, reason: GapReason) -> bool {
        if tag != self.tag || range.is_empty() {
            return false;
        }
        let Some(head) = self.head() else { return false };
        let read = self.reads[head].as_mut().unwrap();
        if range.start != read.cursor || range.end > read.range.end {
            return false;
        }
        read.cursor = range.end;
        self.note(range, Note::Gap(reason));
        true
    }

    fn note(&mut self, range: SampleRange, note: Note) {
        self.notes[self.next_note] = Some((range, note));
        self.next_note = (self.next_note + 1) % MAX_GAP_NOTES;
    }

    /// `ReaderResult::Done` for the current serial settles the head request.
    /// `reached` must equal what this window saw of the answer (its cursor)
    /// and lie within the request. Everything placed or noted is done; the
    /// first frame not placed (or not answered, for a per-read bound) is
    /// where the rest is planned again, if still desired.
    pub fn settle(&mut self, tag: ReadTag, reached: u64) -> SettleOutcome {
        if tag != self.tag {
            return SettleOutcome::Unexpected;
        }
        let Some(head) = self.head() else { return SettleOutcome::Unexpected };
        let read = self.reads[head].take().unwrap();
        if reached != read.cursor || reached < read.range.start || reached > read.range.end {
            return SettleOutcome::Mismatch;
        }
        let resume = read.unplaced.unwrap_or(read.cursor).min(read.cursor);
        if resume >= read.range.end {
            return SettleOutcome::Complete;
        }
        let rest = SampleRange { start: resume, end: read.range.end };
        let replanned = self.is_desired(&rest);
        if replanned {
            // Unsent again, keeping its slot in the request array (the one
            // just freed): offered by the next plan, admitted again by `sent`.
            self.reads[head] = Some(InFlight { id: read.id, range: rest, ordinal: None, cursor: resume, unplaced: None });
        }
        SettleOutcome::Partial { resume, replanned }
    }

    /// `ReaderResult::Failed` with this tag ends the head request; what it
    /// did not answer is noted failed until the next seek, so it is not
    /// fetched again and again. `false` when no request was waiting.
    pub fn failed(&mut self, tag: ReadTag) -> bool {
        if tag != self.tag {
            return false;
        }
        let Some(head) = self.head() else { return false };
        let read = self.reads[head].take().unwrap();
        let resume = read.unplaced.unwrap_or(read.cursor).min(read.cursor);
        if resume < read.range.end {
            self.note(SampleRange { start: resume, end: read.range.end }, Note::Failed);
        }
        true
    }

    // -----------------------------------------------------------------------
    // consumer receipts
    // -----------------------------------------------------------------------

    /// The placement will not be installed (explicitly refused, or the caller
    /// discards it after a seek): its slot is free again. A queue-full refusal
    /// needs no call — keep the placement and send it again.
    pub fn install_refused(&mut self, placement: &PagePlacement) {
        let index = placement.slot as usize;
        if matches!(self.slots.get(index), Some(Slot::PendingInstall { ticket, .. }) if *ticket == placement.ticket) {
            self.slots[index] = Slot::Empty;
        }
    }

    /// Whether a held placement still belongs to the current serial.
    pub fn is_current(&self, placement: &PagePlacement) -> bool {
        placement.tag == self.tag && !self.closing
    }

    /// The consumer installed the page of `receipt.ticket`. A page of an old
    /// serial (or after close) is retired at once: the slot is freed only on
    /// that retirement's receipt.
    pub fn installed(&mut self, receipt: InstallReceipt) -> Result<InstallOutcome, WindowError> {
        let index = receipt.slot as usize;
        let Some(Slot::PendingInstall { ticket, tag, range }) = self.slots.get(index).copied() else {
            return Ok(InstallOutcome::Unknown);
        };
        if ticket != receipt.ticket {
            return Ok(InstallOutcome::Unknown);
        }
        if tag == self.tag && !self.closing {
            self.slots[index] = Slot::Resident { ticket, tag, range };
            return Ok(InstallOutcome::Resident);
        }
        let retire_ticket = bump(&mut self.next_ticket)?;
        self.slots[index] = Slot::Retiring { ticket: retire_ticket, range, was: Some((ticket, tag)) };
        Ok(InstallOutcome::Stale(PageRetirement { slot: receipt.slot, ticket: retire_ticket, range }))
    }

    /// The consumer emptied the slot of `receipt.ticket`. `false` for a
    /// receipt that names no retirement in progress.
    pub fn retired(&mut self, receipt: RetireReceipt) -> bool {
        let index = receipt.slot as usize;
        match self.slots.get(index) {
            Some(Slot::Retiring { ticket, .. }) if *ticket == receipt.ticket => {
                self.slots[index] = Slot::Empty;
                true
            }
            _ => false,
        }
    }

    /// The exact page a pending retirement names: its OLD installed
    /// `ReadTag` and OLD placement ticket — never the new retirement ticket.
    ///
    /// `Some` only while `retirement` is still pending exactly as issued: its
    /// slot lies within the policy's slots, the slot is `Retiring` under
    /// `retirement.ticket`, and the stored range equals `retirement.range`.
    /// Works for ordinary plan retirements and for [`InstallOutcome::Stale`]
    /// ones alike. After the retire receipt, a refusal, or anything else that
    /// ended the retirement, it is `None`. Read-only: nothing is inferred
    /// from the current seek serial.
    ///
    /// Snapshot this target while the retirement is pending and build the
    /// exact retire command from it. Never use the retirement ticket as the
    /// old placement: that could remove whatever occupies the slot next.
    pub fn retirement_target(&self, retirement: &PageRetirement) -> Option<(ReadTag, u64)> {
        let index = retirement.slot as usize;
        if index >= self.slot_count() {
            return None;
        }
        match self.slots[index] {
            Slot::Retiring { ticket, range, was: Some((old_ticket, tag)) } if ticket == retirement.ticket && range == retirement.range => Some((tag, old_ticket)),
            _ => None,
        }
    }

    /// The retirement was explicitly refused: the page is still installed.
    /// It becomes resident again under its old ticket and serial (a stale
    /// serial or undesired range is retired again by the next plan). A
    /// queue-full refusal needs no call — keep the retirement, send again.
    pub fn retire_refused(&mut self, retirement: &PageRetirement) {
        let index = retirement.slot as usize;
        if let Some(Slot::Retiring { ticket, range, was: Some((old, tag)) }) = self.slots.get(index).copied() {
            if ticket == retirement.ticket {
                self.slots[index] = Slot::Resident { ticket: old, tag, range };
            }
        }
    }
}
