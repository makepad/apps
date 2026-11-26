//! The reader: one owner-side handle, two long-lived workers.
//!
//! The application spawns it once through the platform [`ThreadSpawner`];
//! [`AvReader`] starts exactly two workers, never another:
//!
//! * the AUDIO worker reads only sound, and only for strict audio loads
//!   ([`AvReader::open_audio_paged`]);
//! * the VIDEO worker reads pictures ([`AvReader::open_video`]), describes
//!   finished files before a strict audio load of one opens, and runs every
//!   legacy combined load ([`AvReader::open`], [`AvReader::open_paged`])
//!   whole, exactly as the single worker used to.
//!
//! So no video open, seek, decode or file description is ever queued ahead
//! of a strict audio load's work. That isolates the sound from VIDEO work,
//! not from its own: an audio decoder open or seek, an index page read or a
//! sidecar's digest check still takes as long as it takes, so an audio
//! consumer prebuffers and reports starvation honestly. A legacy combined
//! load has no such isolation at all.
//!
//! Each worker has its own bounded command queue, result queue and result
//! byte cap (see the lane constants below; together they stay within
//! [`READER_COMMANDS`], [`READER_RESULTS`] and [`MAX_IN_FLIGHT_BYTES`]). The
//! handle never waits and shares no lock with either; a full queue hands a
//! command back.
//!
//! Backpressure is strict. A worker takes its next command only after every
//! result of the previous one was handed over, so its outbox never holds
//! more than one request's results: at most [`MAX_RESULTS_PER_READ`] pages
//! and gaps (a read that would need more stops early and reports how far it
//! got in `Done::reached`), plus that request's one final `Done` or failure,
//! so [`MAX_RESULTS_PER_READ`] + 1 in all. Payload bytes handed over
//! but not yet taken are capped per worker; the largest result a worker can
//! produce fits its cap (checked at compile time), so there is no oversized
//! exception, and past the cap the WORKER waits, never the UI. While waiting
//! it drops results a newer seek serial or a close made stale, so their
//! final drop happens there. The caps count results QUEUED for the owner,
//! and nothing else: not a worker's outbox (at most one request's results,
//! [`MAX_RESULTS_PER_READ`] + 1, held while it waits), not the frames the video
//! worker keeps for its next read (the last and next picture of each open
//! cursor), not payloads the owner took and keeps. They are no bound on
//! resident memory.
//!
//! Every result is tagged ([`ReadTag`]). Payloads are immutable `Arc`s; a
//! consumer gives them back with [`AvReader::recycle`], and each goes back
//! to the worker that allocated it (a page carries that origin from its
//! allocation), so the final drop and buffer reuse happen there, never on a
//! realtime callback. Audio pages are exact frame ranges of the source
//! timeline, stereo i16; what is not there is reported as a gap, never
//! invented.

mod handle;
mod upload;

pub use handle::AvReader;
pub use upload::{
    FrameGroup, FrameGroupError, UploadFrame, UploadInfo, UploadRefusal, UploadReturn, UploadStub, UploadTicket, VIDEO_DECODE_CREDITS,
    VIDEO_UPLOAD_PERMITS,
};
use upload::{nv12_bytes, DecodeCredit, GroupHold};

use crate::identity::{ConsumerLoad, MediaIdentity, PublicationSeq, ReadTag};
use crate::index::{FileName, IndexStore, PageRef, SegmentEntry, SegmentKind, VideoSegmentRef};
use crate::limits::*;
use crate::payload::{BufOrigin, PAGE_BYTES};
use crate::sidecar::{SidecarFile, SIDECAR_READ_FRAMES};
use crate::timeline::{AudioCaps, Container, GapReason, Origin, SampleRange, TimeBase, TimelineSnapshot, TimelineState, VideoCaps};
use makepad_asset_data::MediaType;
use makepad_platform::video_file::{DecodedAudioChunk, VideoColor, VideoFileDecoder};
use makepad_stage_audio_media::decode::DecoderInput;
use makepad_stage_audio_media::dsp_math::stereo_pair;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// lanes
// ---------------------------------------------------------------------------

/// Commands queued for the audio worker. With [`VIDEO_LANE_COMMANDS`] and
/// the one described open per load the handle may hold waiting for room
/// ([`MAX_READER_CONSUMERS`]), within [`READER_COMMANDS`].
pub const AUDIO_LANE_COMMANDS: usize = (READER_COMMANDS - MAX_READER_CONSUMERS) / 2;
pub const VIDEO_LANE_COMMANDS: usize = READER_COMMANDS - MAX_READER_CONSUMERS - AUDIO_LANE_COMMANDS;
/// Results the handle answers itself for a load it does not hold (a read,
/// advance or close of it). They carry no payload; with none left, the
/// command is handed back.
pub const LOCAL_RESULTS: usize = 8;
/// Results queued for the owner from the audio worker. With
/// [`VIDEO_LANE_RESULTS`], [`LOCAL_RESULTS`] and the one final answer per
/// load the handle may hold ready ([`MAX_READER_CONSUMERS`]), within
/// [`READER_RESULTS`].
pub const AUDIO_LANE_RESULTS: usize = (READER_RESULTS - MAX_READER_CONSUMERS - LOCAL_RESULTS) / 2;
pub const VIDEO_LANE_RESULTS: usize = READER_RESULTS - MAX_READER_CONSUMERS - LOCAL_RESULTS - AUDIO_LANE_RESULTS;
/// Payload bytes the audio worker has handed over and the owner has not
/// taken yet.
pub const AUDIO_LANE_RESULT_BYTES: usize = 16 << 20;
/// The same for the video worker.
pub const VIDEO_LANE_RESULT_BYTES: usize = 64 << 20;
/// Bytes of the largest picture: NV12 at the largest size.
pub const MAX_VIDEO_FRAME_BYTES: usize = MAX_VIDEO_WIDTH as usize * (MAX_VIDEO_HEIGHT as usize + MAX_VIDEO_HEIGHT.div_ceil(2) as usize);

// The two byte caps fit the reader's one cap, and the largest result each
// worker can produce fits its own: the audio worker hands over one page at
// a time, the video worker at most a pair of pictures or (legacy) one page.
const _: () = assert!(AUDIO_LANE_RESULT_BYTES + VIDEO_LANE_RESULT_BYTES <= MAX_IN_FLIGHT_BYTES);
// Every queue and ready slot of both kinds fits the reader's declared total.
const _: () = assert!(AUDIO_LANE_COMMANDS > 0 && VIDEO_LANE_COMMANDS > 0);
const _: () = assert!(AUDIO_LANE_COMMANDS + VIDEO_LANE_COMMANDS + MAX_READER_CONSUMERS <= READER_COMMANDS);
const _: () = assert!(AUDIO_LANE_RESULTS > 0 && VIDEO_LANE_RESULTS > 0);
const _: () = assert!(AUDIO_LANE_RESULTS + VIDEO_LANE_RESULTS + MAX_READER_CONSUMERS + LOCAL_RESULTS <= READER_RESULTS);
const _: () = assert!(PAGE_BYTES <= AUDIO_LANE_RESULT_BYTES);
const _: () = assert!(2 * MAX_VIDEO_FRAME_BYTES <= VIDEO_LANE_RESULT_BYTES && PAGE_BYTES <= VIDEO_LANE_RESULT_BYTES);

// ---------------------------------------------------------------------------
// payloads
// ---------------------------------------------------------------------------

/// Kept at its old path for existing callers; defined in [`crate::payload`].
pub use crate::payload::PcmPage;
use crate::payload::{PageBuf, PageGroup};

/// Largest decoded audio chunk (frames) the worker converts. The platform
/// decoder allocates the chunk itself; this bounds what Stage copies from it.
pub const MAX_DECODED_CHUNK_FRAMES: usize = 192_000;
/// Channels a decoded chunk may carry.
pub const MAX_DECODED_CHANNELS: u16 = 8;

/// One decoded picture, tightly packed NV12, placed on the source timeline.
/// Untagged: one frame may answer several requests; results carry the tag.
pub struct VideoFramePayload {
    /// Timeline tick of the frame's presentation time.
    pub at: i64,
    /// The frame's own container timestamp (100 ns units).
    pub local_pts_100ns: i64,
    pub width: u32,
    pub height: u32,
    pub nv12: Vec<u8>,
    /// The colour of the bytes, as the decoder delivered them.
    pub color: VideoColor,
    /// The reader worker that decoded it; fixed.
    origin: BufOrigin,
    /// An admitted load's decode credit for this buffer, released after it.
    _credit: Option<DecodeCredit>,
}

impl VideoFramePayload {
    pub(crate) fn origin(&self) -> BufOrigin {
        self.origin
    }
}

pub enum Payload {
    Pcm(Arc<PcmPage>),
    Frame(Arc<VideoFramePayload>),
}

// ---------------------------------------------------------------------------
// commands and results
// ---------------------------------------------------------------------------

pub enum ReadSource {
    /// A finished container; the worker probes it and answers `Opened` with
    /// its fixed-frontier snapshot. `media` must be `Completed`.
    CompletedFile { media: MediaIdentity, path: PathBuf, container: Container },
    /// A published, indexed timeline snapshot (growing or final).
    Timeline(Arc<TimelineSnapshot>),
}

/// What a consumer asks of the worker. Opening goes through
/// [`AvReader::open`], which owns the load's cancellation serial.
pub enum ReaderCommand {
    /// A newer publication of the same growing timeline; checked against the
    /// index pages (it must extend the loaded root) before it is adopted.
    Advance { load: ConsumerLoad, snapshot: Arc<TimelineSnapshot> },
    /// Frames `range` of the timeline, at most [`MAX_READ_FRAMES`].
    ReadAudio { tag: ReadTag, range: SampleRange },
    /// The frame shown at timeline tick `at`: the last at or before it.
    ReadVideo { tag: ReadTag, at: u64 },
    /// The decoded frames chronologically bracketing `at`: `before.at <= at
    /// < after.at`. `after` may come from the next committed segment; it
    /// never wraps to the start. For reverse play the caller simply asks
    /// with decreasing `at`; the pair is always in time order.
    ReadVideoPair { tag: ReadTag, at: u64 },
    Recycle(Payload),
    Close { load: ConsumerLoad },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReaderError {
    TooManyConsumers,
    UnknownLoad,
    AlreadyOpen,
    NotCompleted,
    Unsupported(&'static str),
    Snapshot(String),
    Codec(String),
    Index(String),
    OverLimit(&'static str),
    FormatChanged,
    /// An admitted upload read its load's seek serial (or close) moved past:
    /// its one answer, with no frame.
    Cancelled,
}

/// Why [`AvReader::open`] refused, with the source handed back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenRefusal {
    QueueFull,
    AlreadyOpen,
    TooManyConsumers,
}

pub enum ReaderResult {
    Opened { load: ConsumerLoad, snapshot: Arc<TimelineSnapshot> },
    /// The open failed; the load is forgotten and may not be used.
    OpenFailed { load: ConsumerLoad, error: ReaderError },
    Advanced { load: ConsumerLoad, publication: PublicationSeq },
    Audio(Arc<PcmPage>),
    Gap { tag: ReadTag, range: SampleRange, reason: GapReason },
    Video { tag: ReadTag, at: u64, frame: Arc<VideoFramePayload> },
    VideoPair { tag: ReadTag, at: u64, before: Option<Arc<VideoFramePayload>>, after: Option<Arc<VideoFramePayload>> },
    /// No frame is shown at `at` (before the first frame, a gap, no video).
    NoFrame { tag: ReadTag, at: u64 },
    /// A request finished. For audio, `reached` is where its answer ends:
    /// less than the requested end when a per-read bound stopped it early.
    Done { tag: ReadTag, reached: u64 },
    Failed { load: ConsumerLoad, tag: Option<ReadTag>, error: ReaderError },
    Closed { load: ConsumerLoad },
    /// The one answer to an admitted upload read
    /// ([`AvReader::read_video_upload`]), for every read handed over,
    /// current or not: `Some` the frame shown at `at`, `None` no frame
    /// there, or why not (`Cancelled` when a newer seek serial or a close
    /// moved past it; it then carries no frame). No `Done` or `Failed`
    /// follows it. Only a stop, or its worker ending, leaves it unanswered.
    VideoUpload { tag: ReadTag, request: u64, at: u64, outcome: Result<Option<UploadFrame>, ReaderError> },
}

/// Payload bytes a result carries, as the in-flight budget counts them.
fn result_bytes(result: &ReaderResult) -> usize {
    let frame = |frame: &Option<Arc<VideoFramePayload>>| frame.as_ref().map_or(0, |frame| frame.nv12.len());
    match result {
        // The allocation, not the length: every page buffer is PAGE_BYTES.
        ReaderResult::Audio(page) => page.capacity_bytes(),
        ReaderResult::Video { frame, .. } => frame.nv12.len(),
        ReaderResult::VideoPair { before, after, .. } => frame(before) + frame(after),
        ReaderResult::VideoUpload { outcome: Ok(Some(frame)), .. } => frame.capacity_bytes(),
        _ => 0,
    }
}

fn result_tag(result: &ReaderResult) -> Option<&ReadTag> {
    match result {
        ReaderResult::Audio(page) => Some(page.tag_ref()),
        ReaderResult::Gap { tag, .. }
        | ReaderResult::Video { tag, .. }
        | ReaderResult::VideoPair { tag, .. }
        | ReaderResult::NoFrame { tag, .. }
        | ReaderResult::Done { tag, .. }
        | ReaderResult::VideoUpload { tag, .. } => Some(tag),
        ReaderResult::Failed { tag, .. } => tag.as_ref(),
        _ => None,
    }
}

/// What a load reads: sound and pictures (legacy), or only one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    Combined,
    Audio,
    Video,
    /// Pictures as admitted uploads only ([`AvReader::open_video_admitted`]).
    Admitted,
}

enum OpenSource {
    Read(ReadSource),
    /// A finished file as the video worker described it: the audio worker
    /// opens only its sound and checks it against this description.
    Described(Arc<TimelineSnapshot>),
}

enum Command {
    /// `refuse`: the owner already knows the open must fail (its group
    /// serves another load); the worker answers `OpenFailed` and drops the
    /// group, which never gets a pool.
    /// `frames`: an admitted load's group, fixed to that load.
    Open {
        load: ConsumerLoad,
        source: OpenSource,
        serial: Arc<AtomicU64>,
        group: Option<PageGroup>,
        frames: Option<FrameGroup>,
        purpose: Purpose,
        refuse: Option<ReaderError>,
    },
    /// An admitted upload read; the ticket pays for its buffer.
    ReadUpload { tag: ReadTag, at: u64, request: u64, ticket: UploadTicket },
    /// An upload buffer back for its final drop here.
    ReturnUpload(UploadReturn),
    /// Describe a finished file (video worker), for a strict audio load.
    Describe { load: ConsumerLoad, media: MediaIdentity, path: PathBuf, container: Container },
    Public(ReaderCommand),
}

/// What a worker hands the owner.
enum LaneResult {
    Public(ReaderResult),
    /// The answer to `Describe`: the file's full timeline, or why not.
    Described { load: ConsumerLoad, result: Result<Arc<TimelineSnapshot>, ReaderError> },
}

fn lane_result_bytes(result: &LaneResult) -> usize {
    match result {
        LaneResult::Public(result) => result_bytes(result),
        LaneResult::Described { .. } => 0,
    }
}

// ---------------------------------------------------------------------------
// the worker
// ---------------------------------------------------------------------------

struct Worker {
    /// Which reader and which of its workers this is: stamped on every
    /// buffer allocated here.
    origin: BufOrigin,
    /// This worker's result byte cap.
    budget: usize,
    /// Decoded bytes each raw (unadmitted) file video load keeps for later
    /// reads ([`AvReader::spawn_keeping`]); 0 keeps none.
    keep: usize,
    results: SyncSender<LaneResult>,
    /// Bytes handed over and not taken. This worker is its only producer
    /// (it adds before sending); only the owner takes away.
    in_flight: Arc<AtomicUsize>,
    alive: Arc<AtomicBool>,
    outbox: VecDeque<LaneResult>,
    consumers: Vec<Consumer>,
    /// Buffers of unpaged loads (no permit), at most [`PCM_POOL`].
    pool: Vec<PageBuf>,
    /// Stereo conversion of one decoded chunk, at most
    /// [`MAX_DECODED_CHUNK_FRAMES`] frames (checked before the copy).
    scratch: Vec<[i16; 2]>,
}

struct Consumer {
    load: ConsumerLoad,
    serial: Arc<AtomicU64>,
    snapshot: Arc<TimelineSnapshot>,
    audio: Option<AudioCursor>,
    video: Option<VideoCursor>,
    /// An admitted load's hold of its frame group: dropped after the video
    /// cursor (so after every decoded frame), it marks the group finished.
    frames: Option<GroupHold>,
    /// The admission group of a paged load, and its recycled buffers (each
    /// still holding its permit, so bounded by the group's capacity). Both
    /// are dropped on this worker when the load closes.
    group: Option<PageGroup>,
    pool: Vec<PageBuf>,
    /// Declared last so every decoder above is dropped before the typed
    /// path it opened is released.
    _input: Option<DecoderInput>,
}

enum AudioCursor {
    File(FileAudio),
    Sidecar(Option<(FileName, SidecarFile)>),
}

struct FileAudio {
    decoder: VideoFileDecoder,
    /// Timeline frame the next decoded chunk is expected to start at.
    expected: Option<u64>,
    /// Decoded frames not yet delivered, starting at `carry_start`.
    carry: Vec<[i16; 2]>,
    carry_start: u64,
}

enum VideoCursor {
    File(FileVideo),
    Segments(SegmentVideo),
}

struct FileVideo {
    decoder: VideoFileDecoder,
    /// Last decoded frame at or before the last target.
    last: Option<Arc<VideoFramePayload>>,
    /// First decoded frame after it.
    ahead: Option<Arc<VideoFramePayload>>,
    ended: bool,
    /// An admitted load's group: every decoded frame takes one of its
    /// credits first.
    credits: Option<FrameGroup>,
    /// Frames kept for later reads (none unless the reader keeps frames).
    kept: KeptFrames,
}

#[derive(Default)]
struct SegmentVideo {
    /// The open segment; its decoder is dropped before its input.
    current: Option<(VideoSegmentRef, SampleRange, FileVideo, DecoderInput)>,
    /// The first frame of the committed segment after `current` (pair
    /// reads only; never for an admitted load).
    next_first: Option<(VideoSegmentRef, Option<Arc<VideoFramePayload>>)>,
    /// An admitted load's group, for each segment's decoder.
    credits: Option<FrameGroup>,
}

enum Flow {
    Continue,
    /// A per-read bound was reached; `Done::reached` says where.
    Stopped,
    Stale,
}

enum Delivery {
    Drained,
    Waiting,
    Gone,
}

impl Worker {
    fn new(origin: BufOrigin, budget: usize, keep: usize, results: SyncSender<LaneResult>, in_flight: Arc<AtomicUsize>, alive: Arc<AtomicBool>) -> Self {
        Self { origin, budget, keep, results, in_flight, alive, outbox: VecDeque::new(), consumers: Vec::new(), pool: Vec::new(), scratch: Vec::new() }
    }

    fn run(mut self, commands: Receiver<Command>) {
        while self.alive.load(Ordering::Acquire) {
            match self.deliver() {
                Delivery::Gone => return,
                Delivery::Waiting => {
                    // The consumer has not drained yet; no new work is taken.
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Delivery::Drained => {}
            }
            match commands.recv() {
                Ok(command) => self.handle(command),
                Err(_) => return,
            }
        }
    }

    fn is_stale(&self, result: &LaneResult) -> bool {
        let LaneResult::Public(result) = result else { return false };
        let Some(tag) = result_tag(result) else { return false };
        match self.consumers.iter().find(|consumer| consumer.load == tag.load) {
            Some(consumer) => consumer.serial.load(Ordering::Acquire) != tag.seek_serial,
            None => true,
        }
    }

    fn deliver(&mut self) -> Delivery {
        while let Some(front) = self.outbox.front() {
            if self.is_stale(front) {
                match self.outbox.front_mut() {
                    // An upload read always gets its one answer: gone stale,
                    // it is cancelled, and a frame it carried (with its
                    // permit) is dropped here, before its bytes are counted.
                    Some(LaneResult::Public(ReaderResult::VideoUpload { outcome, .. })) => {
                        if !matches!(outcome, Err(ReaderError::Cancelled)) {
                            drop(std::mem::replace(outcome, Err(ReaderError::Cancelled)));
                        }
                    }
                    _ => {
                        // Final drop here, on the worker.
                        drop(self.outbox.pop_front());
                        continue;
                    }
                }
            }
            let Some(front) = self.outbox.front() else { break };
            // Every result fits the cap on its own (see the lane
            // constants), so an empty queue always takes it.
            let size = lane_result_bytes(front);
            if self.in_flight.load(Ordering::Acquire).saturating_add(size) > self.budget {
                return Delivery::Waiting;
            }
            let result = self.outbox.pop_front().unwrap();
            self.in_flight.fetch_add(size, Ordering::AcqRel);
            match self.results.try_send(result) {
                Ok(()) => {}
                Err(TrySendError::Full(result)) => {
                    self.in_flight.fetch_sub(size, Ordering::AcqRel);
                    self.outbox.push_front(result);
                    return Delivery::Waiting;
                }
                Err(TrySendError::Disconnected(_)) => return Delivery::Gone,
            }
        }
        Delivery::Drained
    }

    fn emit(&mut self, result: ReaderResult) {
        self.outbox.push_back(LaneResult::Public(result));
    }

    fn consumer_index(&self, load: &ConsumerLoad) -> Option<usize> {
        self.consumers.iter().position(|consumer| consumer.load == *load)
    }

    fn handle(&mut self, command: Command) {
        let command = match command {
            Command::Open { load, refuse: Some(error), .. } => {
                // The group (if any) and the source are dropped here; the
                // group never had a pool on this worker. A frame group sent
                // with a refusal is not this load's (or is already claimed):
                // it is dropped, not marked finished.
                self.emit(ReaderResult::OpenFailed { load, error });
                return;
            }
            Command::Describe { load, media, path, container } => {
                let result = describe_completed(media, path, container).map(Arc::new);
                self.outbox.push_back(LaneResult::Described { load, result });
                return;
            }
            Command::Open { load, source, serial, group, frames, purpose, refuse: None } => {
                // An open that fails drops its group handle here, on the
                // worker; no buffer of it exists yet. A frame group's hold
                // marks it finished when it goes, with the open or later
                // with the load.
                match self.open(load, source, serial, group, frames.map(FrameGroup::hold), purpose) {
                    Ok(snapshot) => self.emit(ReaderResult::Opened { load, snapshot }),
                    Err(error) => self.emit(ReaderResult::OpenFailed { load, error }),
                }
                return;
            }
            Command::ReadUpload { tag, at, request, ticket } => {
                // Every upload read gets exactly one answer.
                let outcome = self.read_upload(tag, at, request, ticket);
                self.emit(ReaderResult::VideoUpload { tag, request, at, outcome });
                return;
            }
            Command::ReturnUpload(upload) => {
                // The final drop, here: the buffer, then its permit.
                drop(upload);
                return;
            }
            Command::Public(command) => command,
        };
        match command {
            ReaderCommand::Recycle(Payload::Pcm(page)) => {
                // Only a uniquely owned page can be reused. Otherwise this
                // drops just our reference; the remaining holders keep the
                // buffer, and with it its permit, until they drop it.
                let Ok(page) = Arc::try_unwrap(page) else { return };
                if page.origin() != self.origin {
                    // Allocated by another reader: dropped, never pooled.
                    return;
                }
                let load = page.tag().load;
                let mut buf = page.into_buf();
                buf.clear();
                if buf.is_admitted() {
                    // Back to the pool of exactly the load that produced it
                    // AND exactly its group, permit attached. A closed load
                    // (even if its group was reopened for another load) has
                    // no pool: the buffer and its permit drop here, on the
                    // worker. A group serves one open load at a time, so this
                    // is the only pool the buffer could belong to.
                    if let Some(consumer) = self
                        .consumers
                        .iter_mut()
                        .find(|consumer| consumer.load == load && consumer.group.as_ref().is_some_and(|group| buf.is_of(group)))
                    {
                        consumer.pool.push(buf);
                    }
                } else if self.pool.len() < PCM_POOL {
                    self.pool.push(buf);
                }
            }
            ReaderCommand::Recycle(Payload::Frame(frame)) => drop(frame),
            ReaderCommand::Advance { load, snapshot } => match self.advance(load, snapshot) {
                Ok(publication) => self.emit(ReaderResult::Advanced { load, publication }),
                Err(error) => self.emit(ReaderResult::Failed { load, tag: None, error }),
            },
            ReaderCommand::Close { load } => {
                // The consumer's decoders, group handle and pooled buffers
                // (with their permits) are dropped here, on the worker.
                // Results of it still queued are stale and dropped by
                // `deliver`; pages already delivered keep their permits
                // until their holders drop or recycle them.
                if let Some(index) = self.consumer_index(&load) {
                    self.consumers.swap_remove(index);
                }
                self.emit(ReaderResult::Closed { load });
            }
            ReaderCommand::ReadAudio { tag, range } => {
                let result = self.read_audio(tag, range);
                self.finish(tag, result);
            }
            ReaderCommand::ReadVideo { tag, at } => {
                let result = self.read_video(tag, at, false).map(|flow| (flow, at));
                self.finish(tag, result);
            }
            ReaderCommand::ReadVideoPair { tag, at } => {
                let result = self.read_video(tag, at, true).map(|flow| (flow, at));
                self.finish(tag, result);
            }
        }
    }

    fn finish(&mut self, tag: ReadTag, result: Result<(Flow, u64), ReaderError>) {
        match result {
            Ok((Flow::Stale, _)) => {}
            Ok((_, reached)) => self.emit(ReaderResult::Done { tag, reached }),
            Err(error) => self.emit(ReaderResult::Failed { load: tag.load, tag: Some(tag), error }),
        }
    }

    fn consumer_for(&self, tag: &ReadTag) -> Result<usize, ReaderError> {
        let index = self.consumer_index(&tag.load).ok_or(ReaderError::UnknownLoad)?;
        if self.consumers[index].snapshot.media != tag.media {
            return Err(ReaderError::UnknownLoad);
        }
        Ok(index)
    }

    fn open(
        &mut self,
        load: ConsumerLoad,
        source: OpenSource,
        serial: Arc<AtomicU64>,
        group: Option<PageGroup>,
        frames: Option<GroupHold>,
        purpose: Purpose,
    ) -> Result<Arc<TimelineSnapshot>, ReaderError> {
        if self.consumer_index(&load).is_some() {
            return Err(ReaderError::AlreadyOpen);
        }
        if self.consumers.len() >= MAX_READER_CONSUMERS {
            return Err(ReaderError::TooManyConsumers);
        }
        // One group, one open load: two loads sharing a group could starve
        // each other through each other's pools. Checked before any probe.
        if let Some(group) = &group {
            if self.consumers.iter().any(|consumer| consumer.group.as_ref().is_some_and(|held| held.same(group))) {
                return Err(ReaderError::Unsupported("a page group serves one open load at a time"));
            }
        }
        let consumer = match source {
            // A strict audio load of a finished file is described first,
            // on the video worker; the owner never sends it here.
            OpenSource::Read(ReadSource::CompletedFile { .. }) if purpose == Purpose::Audio => {
                return Err(ReaderError::Unsupported("a finished file is described before its sound is opened"))
            }
            OpenSource::Read(ReadSource::CompletedFile { media, path, container }) => open_completed(load, serial, media, path, container, purpose)?,
            OpenSource::Described(snapshot) if purpose == Purpose::Audio => open_described_audio(load, serial, snapshot)?,
            OpenSource::Described(_) => return Err(ReaderError::Unsupported("only a strict audio load opens a described file")),
            OpenSource::Read(ReadSource::Timeline(snapshot)) => {
                snapshot.validate().map_err(|error| ReaderError::Snapshot(format!("{error:?}")))?;
                match &snapshot.origin {
                    Origin::Indexed { root: Some(root), .. } => root.validate().map_err(index_error)?,
                    Origin::Indexed { root: None, .. } => {}
                    Origin::SingleFile { .. } => return Err(ReaderError::Unsupported("single-file snapshots are opened as CompletedFile")),
                }
                Consumer {
                    load,
                    serial,
                    audio: snapshot.audio.filter(|_| matches!(purpose, Purpose::Combined | Purpose::Audio)).map(|_| AudioCursor::Sidecar(None)),
                    video: snapshot.video.filter(|_| purpose != Purpose::Audio).map(|_| VideoCursor::Segments(SegmentVideo::default())),
                    frames: None,
                    snapshot,
                    group: None,
                    pool: Vec::new(),
                    _input: None,
                }
            }
        };
        let mut consumer = consumer;
        consumer.group = group;
        if let Some(hold) = frames {
            if let Err(error) = admit_frames(&mut consumer, &hold) {
                // Its decoders go first; then the hold marks the group
                // finished.
                drop(consumer);
                drop(hold);
                return Err(error);
            }
            consumer.frames = Some(hold);
        }
        let snapshot = consumer.snapshot.clone();
        self.consumers.push(consumer);
        Ok(snapshot)
    }

    fn advance(&mut self, load: ConsumerLoad, snapshot: Arc<TimelineSnapshot>) -> Result<PublicationSeq, ReaderError> {
        let index = self.consumer_index(&load).ok_or(ReaderError::UnknownLoad)?;
        let consumer = &mut self.consumers[index];
        consumer.snapshot.check_successor(&snapshot).map_err(|error| ReaderError::Snapshot(format!("{error:?}")))?;
        let (Origin::Indexed { store, root: old }, Origin::Indexed { root: new, .. }) = (&consumer.snapshot.origin, &snapshot.origin) else {
            return Err(ReaderError::Unsupported("only indexed timelines grow"));
        };
        IndexStore::existing(store.clone()).verify_extends(old.as_ref(), new.as_ref()).map_err(index_error)?;
        let publication = snapshot.publication;
        consumer.snapshot = snapshot;
        Ok(publication)
    }

    // -----------------------------------------------------------------------
    // audio
    // -----------------------------------------------------------------------

    fn read_audio(&mut self, tag: ReadTag, range: SampleRange) -> Result<(Flow, u64), ReaderError> {
        if range.start >= range.end || range.len() > MAX_READ_FRAMES {
            return Err(ReaderError::OverLimit("audio read range"));
        }
        let index = self.consumer_for(&tag)?;
        let rate = self.consumers[index].snapshot.audio.ok_or(ReaderError::Unsupported("no audio stream"))?.sample_rate;
        let committed = self.consumers[index].snapshot.committed();
        // A paged load fills buffers from its own pool and its group's
        // permits; an unpaged one from the shared bounded pool.
        let (pool, group) = match &mut self.consumers[index] {
            Consumer { group: Some(group), pool, .. } => (std::mem::take(pool), Some(group.clone())),
            _ => (std::mem::take(&mut self.pool), None),
        };
        let mut out = Emitter::new(tag, rate, pool, group, self.origin, range.start);
        let mut flow = Ok(Flow::Continue);
        // Outside the committed range: said so, never read.
        if range.start < committed.start && !out.gap(SampleRange { start: range.start, end: range.end.min(committed.start) }, GapReason::NotCommitted) {
            flow = Ok(Flow::Stopped);
        }
        if matches!(flow, Ok(Flow::Continue)) {
            if let Some(inside) = range.intersect(&committed) {
                let consumer = &mut self.consumers[index];
                let serial = consumer.serial.clone();
                let stale = move || serial.load(Ordering::Acquire) != tag.seek_serial;
                flow = match consumer.audio.as_mut() {
                    None => Err(ReaderError::Unsupported("no audio stream")),
                    Some(AudioCursor::File(file)) => read_file_audio(file, rate, inside, &mut out, &mut self.scratch, &stale),
                    Some(AudioCursor::Sidecar(open)) => match &consumer.snapshot.origin {
                        Origin::Indexed { store, root: Some(root) } => {
                            read_sidecars(&IndexStore::existing(store.clone()), root, open, rate, inside, &mut out, &stale)
                        }
                        _ => Ok(Flow::Continue),
                    },
                };
            }
        }
        if matches!(flow, Ok(Flow::Continue))
            && range.end > committed.end
            && !out.gap(SampleRange { start: range.start.max(committed.end), end: range.end }, GapReason::NotCommitted)
        {
            flow = Ok(Flow::Stopped);
        }
        let reached = out.reached;
        let (results, pool) = out.finish();
        match &mut self.consumers[index] {
            Consumer { group: Some(_), pool: own, .. } => *own = pool,
            _ => {
                self.pool = pool;
                self.pool.truncate(PCM_POOL);
            }
        }
        match flow {
            Ok(Flow::Stale) => Ok((Flow::Stale, reached)),
            Ok(flow) => {
                self.outbox.extend(results.into_iter().map(LaneResult::Public));
                Ok((flow, reached))
            }
            Err(error) => Err(error),
        }
    }

    // -----------------------------------------------------------------------
    // video
    // -----------------------------------------------------------------------

    fn read_video(&mut self, tag: ReadTag, at: u64, pair: bool) -> Result<Flow, ReaderError> {
        let origin = self.origin;
        let keep = self.keep;
        let index = self.consumer_for(&tag)?;
        let consumer = &mut self.consumers[index];
        if consumer.frames.is_some() {
            // Decoded frames of an admitted load never leave this worker.
            return Err(ReaderError::Unsupported("an admitted video load reads uploads only"));
        }
        let caps = consumer.snapshot.video.ok_or(ReaderError::Unsupported("no video stream"))?;
        let time_base = consumer.snapshot.time_base;
        let committed = consumer.snapshot.committed();
        if !committed.contains(at) {
            self.emit(ReaderResult::NoFrame { tag, at });
            return Ok(Flow::Continue);
        }
        let serial = consumer.serial.clone();
        let stale = move || serial.load(Ordering::Acquire) != tag.seek_serial;
        let around = match consumer.video.as_mut() {
            None => return Err(ReaderError::Unsupported("no video stream")),
            Some(VideoCursor::File(file)) => {
                let target = to_hns(time_base, at)?;
                file.kept.budget = keep;
                frames_around(file, caps, origin, target, &stale, &|pts| to_ticks(time_base, pts))?
            }
            Some(VideoCursor::Segments(segments)) => {
                let Origin::Indexed { store, root: Some(root) } = &consumer.snapshot.origin else {
                    self.emit(ReaderResult::NoFrame { tag, at });
                    return Ok(Flow::Continue);
                };
                let store = IndexStore::existing(store.clone());
                segment_frames_around(&store, root, segments, caps, origin, time_base, committed, at, pair, &stale)?
            }
        };
        match around {
            Around::Stale => return Ok(Flow::Stale),
            Around::Pair(None, None) => self.emit(ReaderResult::NoFrame { tag, at }),
            Around::Pair(before, after) if pair => self.emit(ReaderResult::VideoPair { tag, at, before, after }),
            Around::Pair(Some(frame), _) => self.emit(ReaderResult::Video { tag, at, frame }),
            Around::Pair(None, Some(_)) => self.emit(ReaderResult::NoFrame { tag, at }),
        }
        Ok(Flow::Continue)
    }

    /// An admitted upload read: the frame shown at `at` (the last at or
    /// before it), copied once into a buffer paid by `ticket`. A request its
    /// load's seek serial (or close) already moved past is `Cancelled`.
    /// Whatever the outcome, it is the request's one answer; the ticket is
    /// dropped here unless it travels in the frame.
    fn read_upload(&mut self, tag: ReadTag, at: u64, request: u64, ticket: UploadTicket) -> Result<Option<UploadFrame>, ReaderError> {
        let origin = self.origin;
        let index = match self.consumer_for(&tag) {
            Ok(index) => index,
            Err(error) => return Err(error),
        };
        let consumer = &mut self.consumers[index];
        let Some(hold) = consumer.frames.as_ref() else {
            return Err(ReaderError::Unsupported("not an admitted video load"));
        };
        if !ticket.is_of(hold.group()) {
            return Err(ReaderError::Unsupported("an upload ticket of another group"));
        }
        let serial = consumer.serial.clone();
        let stale = move || serial.load(Ordering::Acquire) != tag.seek_serial;
        if stale() {
            return Err(ReaderError::Cancelled);
        }
        let Some(caps) = consumer.snapshot.video else {
            return Err(ReaderError::Unsupported("no video stream"));
        };
        let time_base = consumer.snapshot.time_base;
        let committed = consumer.snapshot.committed();
        if !committed.contains(at) {
            return Ok(None);
        }
        let around = match consumer.video.as_mut() {
            None => Err(ReaderError::Unsupported("no video stream")),
            Some(VideoCursor::File(file)) => {
                to_hns(time_base, at).and_then(|target| frames_around(file, caps, origin, target, &stale, &|pts| to_ticks(time_base, pts)))
            }
            // No pair is read, so no next segment's frame is cached.
            Some(VideoCursor::Segments(segments)) => match &consumer.snapshot.origin {
                Origin::Indexed { store, root: Some(root) } => {
                    segment_frames_around(&IndexStore::existing(store.clone()), root, segments, caps, origin, time_base, committed, at, false, &stale)
                }
                _ => return Ok(None),
            },
        };
        let frame = match around {
            Err(error) => return Err(error),
            Ok(Around::Stale) => return Err(ReaderError::Cancelled),
            Ok(Around::Pair(Some(frame), _)) => frame,
            // Asked before the first picture (a stream whose first frame is
            // stamped after its start): that first picture.
            Ok(Around::Pair(None, Some(frame))) => frame,
            Ok(Around::Pair(None, None)) => return Ok(None),
        };
        // The one copy, at exactly the frame's size, under the permit
        // already taken.
        let bytes = frame.nv12.len();
        let mut nv12 = Vec::with_capacity(bytes);
        nv12.extend_from_slice(&frame.nv12);
        if nv12.capacity() != bytes {
            return Err(ReaderError::OverLimit("upload allocation"));
        }
        let info = UploadInfo { at: frame.at, local_pts_100ns: frame.local_pts_100ns, width: frame.width, height: frame.height, color: frame.color };
        drop(frame);
        Ok(Some(UploadFrame::new(nv12, origin, tag, request, at, info, ticket)))
    }
}

/// Make an opened load admitted: its picture size must be present, even and
/// within the group's, every video cursor decodes under the group's credits,
/// and the size is published before `Opened` is sent.
fn admit_frames(consumer: &mut Consumer, hold: &GroupHold) -> Result<(), ReaderError> {
    let group = hold.group().clone();
    // The group was made for exactly this media opened as exactly this load.
    if consumer.load != group.load() || consumer.snapshot.media != *group.media() {
        return Err(ReaderError::Unsupported("a frame group of another media or load"));
    }
    let caps = consumer.snapshot.video.ok_or(ReaderError::Unsupported("no video stream"))?;
    let (max_width, max_height) = group.max_size();
    let side = |value: u32, max: u32| value > 0 && value % 2 == 0 && value <= max;
    if !side(caps.width, max_width) || !side(caps.height, max_height) {
        return Err(ReaderError::OverLimit("video dimensions"));
    }
    match consumer.video.as_mut() {
        Some(VideoCursor::File(file)) => file.credits = Some(group.clone()),
        Some(VideoCursor::Segments(segments)) => segments.credits = Some(group.clone()),
        None => return Err(ReaderError::Unsupported("no video stream")),
    }
    group.set_opened(caps.width, caps.height);
    Ok(())
}

fn codec(error: impl std::fmt::Display) -> ReaderError {
    ReaderError::Codec(error.to_string())
}

fn index_error(error: crate::index::IndexError) -> ReaderError {
    ReaderError::Index(error.to_string())
}

fn to_hns(time_base: TimeBase, ticks: u64) -> Result<i64, ReaderError> {
    i64::try_from(ticks).ok().and_then(|ticks| time_base.ticks_to_100ns(ticks)).ok_or(ReaderError::OverLimit("time conversion"))
}

fn to_ticks(time_base: TimeBase, hns: i64) -> Result<i64, ReaderError> {
    time_base.hns_to_ticks(hns).ok_or(ReaderError::OverLimit("time conversion"))
}

/// A finished container probed: its full timeline and what the probe
/// opened. Fields drop in order, so the decoder goes before its input.
struct Probe {
    video: Option<VideoFileDecoder>,
    snapshot: TimelineSnapshot,
    input: DecoderInput,
}

fn media_type(container: Container) -> MediaType {
    match container {
        Container::Mp4 => MediaType::Mp4,
    }
}

/// Probe with the full opener. `video` is the full decoder when the file
/// has a picture.
///
/// `guess`: LEGACY combined loads only. A file the full opener refuses, for
/// whatever reason, is then opened for its sound alone and taken for one
/// with no picture, as the single worker always did. That is a guess, not
/// proof of absence, so strict loads never make it: for them the full
/// opener's failure is the answer, and `video: None` means only that the
/// full opener opened the file and reported no picture. (So a strict load
/// of an audio-only finished file may be refused, where the full opener
/// refuses such files.)
///
/// On Apple the full opener may set up the file's sound as well; that is
/// the platform's, and no Stage audio cursor is made from it.
fn probe_completed(media: MediaIdentity, path: PathBuf, container: Container, guess: bool) -> Result<Probe, ReaderError> {
    if !matches!(media, MediaIdentity::Completed { .. }) {
        return Err(ReaderError::NotCompleted);
    }
    let input = DecoderInput::prepare(&path, media_type(container)).map_err(ReaderError::Codec)?;
    let (video, info) = match VideoFileDecoder::open(&input.path) {
        Ok(decoder) => {
            let info = decoder.info().clone();
            (Some(decoder), info)
        }
        Err(_) if guess => {
            let decoder = VideoFileDecoder::open_audio(&input.path).map_err(codec)?;
            let info = decoder.info().clone();
            (None, info)
        }
        Err(error) => return Err(codec(error)),
    };
    if info.width > MAX_VIDEO_WIDTH || info.height > MAX_VIDEO_HEIGHT {
        return Err(ReaderError::OverLimit("video dimensions"));
    }
    let audio_caps = (info.has_audio && info.audio_sample_rate > 0)
        .then_some(AudioCaps { sample_rate: info.audio_sample_rate, source_channels: info.audio_channels.max(1) });
    let video_caps = video.as_ref().filter(|_| info.width > 0 && info.height > 0).map(|_| VideoCaps {
        width: info.width,
        height: info.height,
        fps_num: info.fps_num,
        fps_den: info.fps_den,
    });
    // The timeline is clocked by the sound when there is one; a silent clip
    // runs on a 100 ns base.
    let time_base = audio_caps.map_or(TimeBase { num: 1, den: 10_000_000 }, |audio| TimeBase::sample_rate(audio.sample_rate));
    let committed_end = to_ticks(time_base, info.duration_100ns.max(0))?.max(0) as u64;
    let snapshot = TimelineSnapshot {
        media,
        publication: PublicationSeq(1),
        time_base,
        retained_start: 0,
        committed_end,
        audio: audio_caps,
        video: video_caps,
        origin: Origin::SingleFile { path, container },
        state: TimelineState::Complete,
    };
    snapshot.validate().map_err(|error| ReaderError::Snapshot(format!("{error:?}")))?;
    let video = video.filter(|_| video_caps.is_some());
    Ok(Probe { video, snapshot, input })
}

/// The full timeline of a finished file, for a strict audio load: `video`
/// is `None` only when the file has no picture, never "not probed". The
/// probe's decoders are dropped here, on the video worker.
fn describe_completed(media: MediaIdentity, path: PathBuf, container: Container) -> Result<TimelineSnapshot, ReaderError> {
    let Probe { video, snapshot, input } = probe_completed(media, path, container, false)?;
    drop(video);
    drop(input);
    Ok(snapshot)
}

/// A finished file for a legacy combined load (sound and pictures, as the
/// single worker always did, including its guess for a file the full opener
/// refuses) or a video load (pictures only, strictly probed: no audio
/// decoder is opened for it, and a full opener failure is its failure).
fn open_completed(
    load: ConsumerLoad,
    serial: Arc<AtomicU64>,
    media: MediaIdentity,
    path: PathBuf,
    container: Container,
    purpose: Purpose,
) -> Result<Consumer, ReaderError> {
    let mut probe = probe_completed(media, path, container, purpose == Purpose::Combined)?;
    let audio = match probe.snapshot.audio {
        Some(_) if purpose == Purpose::Combined => Some(AudioCursor::File(FileAudio {
            decoder: VideoFileDecoder::open_audio(&probe.input.path).map_err(codec)?,
            expected: Some(0),
            carry: Vec::new(),
            carry_start: 0,
        })),
        _ => None,
    };
    let video = probe.video.take().map(|decoder| VideoCursor::File(FileVideo { decoder, last: None, ahead: None, ended: false, credits: None, kept: KeptFrames::default() }));
    let Probe { snapshot, input, .. } = probe;
    Ok(Consumer { load, serial, snapshot: Arc::new(snapshot), audio, video, frames: None, group: None, pool: Vec::new(), _input: Some(input) })
}

/// The sound of a finished file the video worker described. Only the audio
/// opener runs here. It reports no picture (zero size and rate), which says
/// nothing of the file's video: the description keeps its own. What it does
/// report of the sound must agree with the description exactly: rate,
/// channels, and the duration on the description's time base.
fn open_described_audio(load: ConsumerLoad, serial: Arc<AtomicU64>, snapshot: Arc<TimelineSnapshot>) -> Result<Consumer, ReaderError> {
    let (path, container) = match (&snapshot.media, &snapshot.origin, &snapshot.state) {
        (MediaIdentity::Completed { .. }, Origin::SingleFile { path, container }, TimelineState::Complete) => (path.clone(), *container),
        _ => return Err(ReaderError::Unsupported("a described file is a completed single file")),
    };
    snapshot.validate().map_err(|error| ReaderError::Snapshot(format!("{error:?}")))?;
    let (audio, input) = match snapshot.audio {
        // A file with no sound: open, with nothing to read.
        None => (None, None),
        Some(caps) => {
            if snapshot.time_base != TimeBase::sample_rate(caps.sample_rate) {
                return Err(ReaderError::FormatChanged);
            }
            let input = DecoderInput::prepare(&path, media_type(container)).map_err(ReaderError::Codec)?;
            let decoder = VideoFileDecoder::open_audio(&input.path).map_err(codec)?;
            let info = decoder.info();
            let end = to_ticks(snapshot.time_base, info.duration_100ns.max(0))?.max(0) as u64;
            let agrees = info.has_audio
                && info.audio_sample_rate == caps.sample_rate
                && info.audio_channels.max(1) == caps.source_channels
                && end == snapshot.committed_end;
            if !agrees {
                return Err(ReaderError::FormatChanged);
            }
            let file = FileAudio { decoder, expected: Some(0), carry: Vec::new(), carry_start: 0 };
            (Some(AudioCursor::File(file)), Some(input))
        }
    };
    Ok(Consumer { load, serial, snapshot, audio, video: None, frames: None, group: None, pool: Vec::new(), _input: input })
}

/// Collects one read's results: contiguous pages of at most
/// [`MAX_PAGE_FRAMES`] and coalesced gaps, at most [`MAX_RESULTS_PER_READ`]
/// in all (the worker then adds the request's one final `Done` or failure).
/// `reached` is the end of what was accepted. A paged load's page
/// buffer comes from its pool or, only after a permit was taken, a new
/// allocation; with no permit left, `push` accepts nothing more and the
/// caller stops at `reached` (backpressure, no gap, no invented frames).
struct Emitter {
    tag: ReadTag,
    rate: u32,
    origin: BufOrigin,
    pool: Vec<PageBuf>,
    group: Option<PageGroup>,
    page: Option<(u64, PageBuf)>,
    results: Vec<ReaderResult>,
    reached: u64,
}

impl Emitter {
    fn new(tag: ReadTag, rate: u32, pool: Vec<PageBuf>, group: Option<PageGroup>, origin: BufOrigin, start: u64) -> Self {
        Self { tag, rate, origin, pool, group, page: None, results: Vec::new(), reached: start }
    }

    fn flush(&mut self) {
        if let Some((start, buf)) = self.page.take() {
            if buf.len() == 0 {
                self.pool.push(buf);
            } else {
                self.results.push(ReaderResult::Audio(Arc::new(PcmPage::from_buf(self.tag, start, self.rate, buf))));
            }
        }
    }

    /// A buffer to fill: pooled, or newly allocated (paged: permit first).
    fn buffer(&mut self) -> Option<PageBuf> {
        if let Some(mut buf) = self.pool.pop() {
            buf.clear();
            return Some(buf);
        }
        match &self.group {
            Some(group) => group.try_permit().map(|permit| PageBuf::allocate(Some(permit), self.origin)),
            None => Some(PageBuf::allocate(None, self.origin)),
        }
    }

    /// Accept frames starting at `start` (== `reached`); returns how many
    /// were accepted before the result bound or the permits ran out.
    fn push(&mut self, start: u64, frames: &[[i16; 2]]) -> usize {
        let mut done = 0;
        while done < frames.len() {
            let at = start + done as u64;
            let contiguous = matches!(&self.page, Some((page_start, page)) if page_start + page.len() as u64 == at && page.room() > 0);
            if !contiguous {
                self.flush();
                if self.results.len() >= MAX_RESULTS_PER_READ {
                    break;
                }
                let Some(buf) = self.buffer() else { break };
                self.page = Some((at, buf));
            }
            let (_, page) = self.page.as_mut().unwrap();
            done += page.extend(&frames[done..]);
            self.reached = start + done as u64;
        }
        done
    }

    /// Report a gap, merged into the previous one when contiguous with the
    /// same reason. `false` when the result bound was hit.
    fn gap(&mut self, range: SampleRange, reason: GapReason) -> bool {
        if range.is_empty() {
            return true;
        }
        self.flush();
        if let Some(ReaderResult::Gap { range: last, reason: last_reason, .. }) = self.results.last_mut() {
            if *last_reason == reason && last.end == range.start {
                last.end = range.end;
                self.reached = range.end;
                return true;
            }
        }
        if self.results.len() >= MAX_RESULTS_PER_READ {
            return false;
        }
        self.results.push(ReaderResult::Gap { tag: self.tag, range, reason });
        self.reached = range.end;
        true
    }

    /// The results, and the unused buffers back (each still with its permit
    /// for a paged load; the caller bounds the unpaged pool).
    fn finish(mut self) -> (Vec<ReaderResult>, Vec<PageBuf>) {
        self.flush();
        (self.results, self.pool)
    }
}

/// Checked before the copy: the chunk's channel count and sample count are
/// sane and its frames within [`MAX_DECODED_CHUNK_FRAMES`], so `out` (the
/// worker scratch) stays bounded whatever the platform decoder hands over.
fn check_chunk(chunk: &DecodedAudioChunk) -> Result<(), ReaderError> {
    let channels = chunk.channels as usize;
    if chunk.channels == 0 || chunk.channels > MAX_DECODED_CHANNELS || chunk.samples.len() % channels != 0 {
        return Err(ReaderError::Codec("decoded audio chunk has an invalid channel layout".into()));
    }
    if chunk.samples.len() / channels > MAX_DECODED_CHUNK_FRAMES {
        return Err(ReaderError::OverLimit("decoded audio chunk"));
    }
    Ok(())
}

/// Converts into the worker scratch, whose capacity is reserved once at
/// [`MAX_DECODED_CHUNK_FRAMES`] frames (750 KiB) and never grows: the chunk
/// was checked not to exceed it.
fn to_stereo(chunk: &DecodedAudioChunk, out: &mut Vec<[i16; 2]>) {
    out.clear();
    if out.capacity() < MAX_DECODED_CHUNK_FRAMES {
        out.reserve_exact(MAX_DECODED_CHUNK_FRAMES);
    }
    for frame in chunk.samples.chunks_exact(chunk.channels.max(1) as usize) {
        if let Some(pair) = stereo_pair(frame) {
            out.push(pair);
        }
    }
}

/// The carry's capacity is reserved once at [`MAX_CARRY_FRAMES`] frames
/// (64 KiB per open file) and never grows.
fn keep_carry(file: &mut FileAudio, start: u64, frames: &[[i16; 2]]) {
    file.carry.clear();
    if file.carry.capacity() < MAX_CARRY_FRAMES {
        file.carry.reserve_exact(MAX_CARRY_FRAMES);
    }
    file.carry.extend_from_slice(&frames[..frames.len().min(MAX_CARRY_FRAMES)]);
    file.carry_start = start;
}

/// Exact frames `range` of a finished container. Sequential reads continue
/// the decoder; anything else seeks EARLIER than the range and trims by
/// chunk timestamps, because the platform seek drops a chunk that starts
/// before its target even when it overlaps it.
fn read_file_audio(
    file: &mut FileAudio,
    rate: u32,
    range: SampleRange,
    out: &mut Emitter,
    scratch: &mut Vec<[i16; 2]>,
    stale: &dyn Fn() -> bool,
) -> Result<Flow, ReaderError> {
    let time_base = TimeBase::sample_rate(rate);
    let mut pos = range.start;
    let carry_end = file.carry_start + file.carry.len() as u64;
    if file.carry_start <= pos && pos < carry_end {
        let from = (pos - file.carry_start) as usize;
        let to = (range.end.min(carry_end) - file.carry_start) as usize;
        let accepted = out.push(pos, &file.carry[from..to]);
        pos += accepted as u64;
        if accepted < to - from {
            file.carry.drain(..from + accepted);
            file.carry_start = pos;
            return Ok(Flow::Stopped);
        }
        if pos >= range.end {
            return Ok(Flow::Continue);
        }
    }
    file.carry.clear();
    let sequential = file.expected.is_some_and(|expected| expected <= pos && pos - expected <= rate as u64 * SEQUENTIAL_SKIP_SECS);
    if !sequential {
        let target = to_hns(time_base, pos)?.saturating_sub(AUDIO_SEEK_PREROLL_100NS);
        file.decoder.seek(target.max(0)).map_err(codec)?;
        file.expected = None;
    }
    while pos < range.end {
        if stale() {
            return Ok(Flow::Stale);
        }
        let Some(chunk) = file.decoder.next_audio().map_err(codec)? else {
            file.expected = None;
            return Ok(if out.gap(SampleRange { start: pos, end: range.end }, GapReason::Decode) { Flow::Continue } else { Flow::Stopped });
        };
        if chunk.sample_rate != rate {
            return Err(ReaderError::FormatChanged);
        }
        check_chunk(&chunk)?;
        to_stereo(&chunk, scratch);
        let mut start = to_ticks(time_base, chunk.pts_100ns)?.max(0) as u64;
        // Timestamps are rounded to 100 ns; a chunk within the slack of
        // where the last one ended is the continuation of it.
        if let Some(expected) = file.expected {
            if start.abs_diff(expected) <= AUDIO_PTS_SLACK_FRAMES {
                start = expected;
            }
        }
        let end = start + scratch.len() as u64;
        file.expected = Some(end);
        if end <= pos {
            continue;
        }
        if start > pos {
            if !out.gap(SampleRange { start: pos, end: start.min(range.end) }, GapReason::Decode) {
                keep_carry(file, start, &scratch[..]);
                return Ok(Flow::Stopped);
            }
            pos = start.min(range.end);
            if pos >= range.end {
                keep_carry(file, start, &scratch[..]);
                return Ok(Flow::Continue);
            }
        }
        let from = (pos - start) as usize;
        let to = (range.end.min(end) - start) as usize;
        let accepted = out.push(pos, &scratch[from..to]);
        pos += accepted as u64;
        if accepted < to - from {
            keep_carry(file, pos, &scratch[from + accepted..]);
            return Ok(Flow::Stopped);
        }
        if end > range.end {
            keep_carry(file, range.end, &scratch[to..]);
        }
    }
    Ok(Flow::Continue)
}

/// Exact frames `range` of an indexed timeline, segment by segment through
/// the bounded index, reading the verified PCM sidecars directly.
fn read_sidecars(
    store: &IndexStore,
    root: &PageRef,
    open: &mut Option<(FileName, SidecarFile)>,
    rate: u32,
    range: SampleRange,
    out: &mut Emitter,
    stale: &dyn Fn() -> bool,
) -> Result<Flow, ReaderError> {
    let mut pos = range.start;
    // One sidecar chunk at a time: fixed at SIDECAR_READ_FRAMES (64 KiB),
    // freed when this read ends.
    let mut frames = Vec::with_capacity(SIDECAR_READ_FRAMES);
    for _ in 0..MAX_SEGMENTS_PER_READ {
        if pos >= range.end {
            return Ok(Flow::Continue);
        }
        if stale() {
            return Ok(Flow::Stale);
        }
        let Some(SegmentEntry { range: segment, kind }) = store.find(root, pos).map_err(index_error)? else {
            return Ok(if out.gap(SampleRange { start: pos, end: range.end }, GapReason::NotCommitted) { Flow::Continue } else { Flow::Stopped });
        };
        let end = segment.end.min(range.end);
        let accepted = match kind {
            SegmentKind::Gap(reason) => out.gap(SampleRange { start: pos, end }, reason),
            SegmentKind::Media { pcm: None, .. } => out.gap(SampleRange { start: pos, end }, GapReason::Source),
            SegmentKind::Media { pcm: Some(pcm), .. } => {
                if open.as_ref().map(|(name, _)| name) != Some(&pcm.file) {
                    *open = None;
                    let file = SidecarFile::open(&store.file_path(&pcm.file), rate, &pcm, segment.len()).map_err(index_error)?;
                    *open = Some((pcm.file.clone(), file));
                }
                let (_, file) = open.as_mut().unwrap();
                while pos < end {
                    let count = ((end - pos) as usize).min(SIDECAR_READ_FRAMES);
                    frames.clear();
                    file.read(pos - segment.start, count, &mut frames).map_err(index_error)?;
                    let accepted = out.push(pos, &frames);
                    pos += accepted as u64;
                    if accepted < frames.len() {
                        return Ok(Flow::Stopped);
                    }
                }
                true
            }
        };
        if !accepted {
            return Ok(Flow::Stopped);
        }
        pos = end;
    }
    Ok(if pos >= range.end { Flow::Continue } else { Flow::Stopped })
}

enum Around {
    Pair(Option<Arc<VideoFramePayload>>, Option<Arc<VideoFramePayload>>),
    Stale,
}

fn decode_frame(
    file: &mut FileVideo,
    caps: VideoCaps,
    origin: BufOrigin,
    to_tick: &dyn Fn(i64) -> Result<i64, ReaderError>,
) -> Result<Option<Arc<VideoFramePayload>>, ReaderError> {
    // An admitted load: the credit is taken BEFORE the decoder allocates,
    // and the buffer it returns must fit the opened size by capacity.
    let credit = match &file.credits {
        Some(group) => Some(group.try_decode_credit().ok_or(ReaderError::OverLimit("video decode credits"))?),
        None => None,
    };
    let Some(frame) = file.decoder.next_frame().map_err(codec)? else {
        file.ended = true;
        return Ok(None);
    };
    if frame.width != caps.width || frame.height != caps.height {
        return Err(ReaderError::FormatChanged);
    }
    validate_nv12(frame.width, frame.height, frame.nv12.len())?;
    if credit.is_some() && frame.nv12.capacity() > nv12_bytes(caps.width, caps.height) {
        return Err(ReaderError::OverLimit("decoded frame allocation"));
    }
    Ok(Some(Arc::new(VideoFramePayload {
        at: to_tick(frame.pts_100ns)?,
        local_pts_100ns: frame.pts_100ns,
        width: frame.width,
        height: frame.height,
        nv12: frame.nv12,
        color: frame.color,
        origin,
        _credit: credit,
    })))
}

fn validate_nv12(width: u32, height: u32, bytes: usize) -> Result<(), ReaderError> {
    // Bounds every picture result by MAX_VIDEO_FRAME_BYTES.
    if width > MAX_VIDEO_WIDTH || height > MAX_VIDEO_HEIGHT {
        return Err(ReaderError::OverLimit("video dimensions"));
    }
    let (w, h) = (width as usize, height as usize);
    // VideoFileDecoder strips platform row padding from both planes to
    // `width` bytes per row, including a cropped odd-width picture.
    let expected = w * (h + h.div_ceil(2));
    if bytes != expected {
        return Err(ReaderError::Codec("decoded frame has the wrong NV12 size".into()));
    }
    Ok(())
}

/// The decoded frames bracketing local time `target`: the last at or before
/// it and the first after it. Sequential forward reads continue; anything
/// else seeks a little earlier and walks forward (one frame's work on
/// keyframe-only material).
///
/// With a keep budget ([`AvReader::spawn_keeping`]) every frame a walk
/// decodes is kept, linked to the one decoded after it, so a later read
/// anywhere in what was walked (a scrub back, a jog, a second pass) is
/// answered without touching the decoder; a seek back then walks a whole
/// [`KEEP_SEEK_PREROLL_100NS`] so the frames before the target come with it.
fn frames_around(
    file: &mut FileVideo,
    caps: VideoCaps,
    origin: BufOrigin,
    target: i64,
    stale: &dyn Fn() -> bool,
    to_tick: &dyn Fn(i64) -> Result<i64, ReaderError>,
) -> Result<Around, ReaderError> {
    if let Some(last) = &file.last {
        if last.local_pts_100ns <= target {
            match &file.ahead {
                Some(ahead) if ahead.local_pts_100ns > target => return Ok(Around::Pair(Some(last.clone()), Some(ahead.clone()))),
                None if file.ended => return Ok(Around::Pair(Some(last.clone()), None)),
                _ => {}
            }
        }
    }
    if let Some(pair) = file.kept.pair(target) {
        return Ok(pair);
    }
    let forward = file.last.as_ref().is_some_and(|last| last.local_pts_100ns <= target && target - last.local_pts_100ns <= VIDEO_SEQUENTIAL_100NS);
    if !forward {
        // Kept frames: land as far back as the budget can keep (a walk
        // past that would be decoded only to be evicted), at most
        // KEEP_SEEK_PREROLL_100NS, never less than a plain seek's preroll.
        let preroll = match file.kept.budget {
            0 => VIDEO_SEEK_PREROLL_100NS,
            budget => {
                let frame_bytes = (caps.width as usize * (caps.height as usize + caps.height.div_ceil(2) as usize)).max(1);
                let frames = (budget / frame_bytes) as i64;
                let fits = frames * 10_000_000 * caps.fps_den.max(1) as i64 / caps.fps_num.max(1) as i64;
                fits.clamp(VIDEO_SEEK_PREROLL_100NS, KEEP_SEEK_PREROLL_100NS)
            }
        };
        file.decoder.seek(target.saturating_sub(preroll).max(0)).map_err(codec)?;
        file.last = None;
        file.ahead = None;
        file.ended = false;
    }
    loop {
        if stale() {
            return Ok(Around::Stale);
        }
        let next = match file.ahead.take() {
            Some(frame) => Some(frame),
            None if file.ended => None,
            None => {
                let frame = decode_frame(file, caps, origin, to_tick)?;
                if let Some(frame) = &frame {
                    let before = file.last.as_ref().map(|last| last.local_pts_100ns);
                    file.kept.insert(before, frame.clone(), target);
                }
                frame
            }
        };
        match next {
            Some(frame) if frame.local_pts_100ns > target => {
                file.ahead = Some(frame);
                break;
            }
            Some(frame) => file.last = Some(frame),
            None => break,
        }
    }
    if file.ended {
        if let Some(last) = &file.last {
            file.kept.mark_last(last.local_pts_100ns);
        }
    }
    Ok(Around::Pair(file.last.clone(), file.ahead.clone()))
}

/// How far before its target a seek lands when frames are kept: the walk
/// from there is kept, so a scrub back finds this much already decoded.
pub const KEEP_SEEK_PREROLL_100NS: i64 = 10_000_000;

/// Decoded frames a file cursor keeps (up to `budget` bytes), by local
/// pts, each linked to the frame decoded right after it. A frame whose
/// successor is known answers every target between the two.
#[derive(Default)]
struct KeptFrames {
    budget: usize,
    bytes: usize,
    /// pts → the frame and the pts of the frame decoded after it.
    frames: std::collections::BTreeMap<i64, (Arc<VideoFramePayload>, Option<i64>)>,
    /// The pts of the file's last frame, once a walk reached the end.
    last: Option<i64>,
}

impl KeptFrames {
    /// The kept frame at or before `target` and the one after it, when
    /// both are known (or the one before is the file's last).
    fn pair(&self, target: i64) -> Option<Around> {
        let (pts, (frame, next)) = self.frames.range(..=target).next_back()?;
        match next {
            Some(next) if *next > target => {
                let (after, _) = self.frames.get(next)?;
                Some(Around::Pair(Some(frame.clone()), Some(after.clone())))
            }
            None if self.last == Some(*pts) => Some(Around::Pair(Some(frame.clone()), None)),
            _ => None,
        }
    }

    /// Keep `frame`, decoded right after the frame at `before` (if that is
    /// known); over budget, the frames farthest from `target` go.
    fn insert(&mut self, before: Option<i64>, frame: Arc<VideoFramePayload>, target: i64) {
        if self.budget == 0 {
            return;
        }
        let pts = frame.local_pts_100ns;
        if let Some(before) = before {
            if let Some((_, next)) = self.frames.get_mut(&before) {
                *next = Some(pts);
            }
        }
        self.bytes += frame.nv12.len();
        if let Some((old, _)) = self.frames.insert(pts, (frame, None)) {
            self.bytes -= old.nv12.len();
        }
        while self.bytes > self.budget {
            let (first, last) = match (self.frames.keys().next(), self.frames.keys().next_back()) {
                (Some(first), Some(last)) => (*first, *last),
                _ => break,
            };
            let drop = if target - first >= last - target { first } else { last };
            if let Some((old, _)) = self.frames.remove(&drop) {
                self.bytes -= old.nv12.len();
            }
        }
    }

    fn mark_last(&mut self, pts: i64) {
        if self.budget > 0 {
            self.last = Some(pts);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn segment_frames_around(
    store: &IndexStore,
    root: &PageRef,
    segments: &mut SegmentVideo,
    caps: VideoCaps,
    made_by: BufOrigin,
    time_base: TimeBase,
    committed: SampleRange,
    at: u64,
    pair: bool,
    stale: &dyn Fn() -> bool,
) -> Result<Around, ReaderError> {
    let Some(SegmentEntry { range, kind }) = store.find(root, at).map_err(index_error)? else {
        return Ok(Around::Pair(None, None));
    };
    let SegmentKind::Media { video: Some(video), .. } = kind else {
        return Ok(Around::Pair(None, None));
    };
    if segments.current.as_ref().map(|(segment, ..)| segment) != Some(&video) {
        segments.current = None;
        let (decoder, input) = open_segment(store, &video, caps)?;
        let credits = segments.credits.clone();
        segments.current = Some((video.clone(), range, FileVideo { decoder, last: None, ahead: None, ended: false, credits, kept: KeptFrames::default() }, input));
    }
    let (segment, range, file, _) = segments.current.as_mut().unwrap();
    let (base, range) = (segment.pts_base_100ns, *range);
    let origin = to_hns(time_base, range.start)?;
    let target = to_hns(time_base, at)?
        .checked_sub(origin)
        .and_then(|offset| offset.checked_add(base))
        .ok_or(ReaderError::OverLimit("time conversion"))?;
    let to_tick = move |pts: i64| {
        pts.checked_sub(base)
            .and_then(|offset| offset.checked_add(origin))
            .ok_or(ReaderError::OverLimit("time conversion"))
            .and_then(|hns| to_ticks(time_base, hns))
    };
    let around = frames_around(file, caps, made_by, target, stale, &to_tick)?;
    let ended = file.ended;
    let Around::Pair(before, None) = around else { return Ok(around) };
    // The segment ran out: the frame after is the first of the next
    // committed segment, never a wrap to the start.
    if !pair || !ended || range.end >= committed.end {
        return Ok(Around::Pair(before, None));
    }
    let Some(SegmentEntry { range: next_range, kind: SegmentKind::Media { video: Some(next), .. } }) = store.find(root, range.end).map_err(index_error)? else {
        return Ok(Around::Pair(before, None));
    };
    if segments.next_first.as_ref().map(|(segment, _)| segment) != Some(&next) {
        let (mut decoder, input) = open_segment(store, &next, caps)?;
        let next_origin = to_hns(time_base, next_range.start)?;
        let next_base = next.pts_base_100ns;
        let frame = match decoder.next_frame().map_err(codec)? {
            Some(frame) if frame.width == caps.width && frame.height == caps.height => {
                validate_nv12(frame.width, frame.height, frame.nv12.len())?;
                let hns = frame
                    .pts_100ns
                    .checked_sub(next_base)
                    .and_then(|offset| offset.checked_add(next_origin))
                    .ok_or(ReaderError::OverLimit("time conversion"))?;
                Some(Arc::new(VideoFramePayload {
                    at: to_ticks(time_base, hns)?,
                    local_pts_100ns: frame.pts_100ns,
                    width: frame.width,
                    height: frame.height,
                    nv12: frame.nv12,
                    color: frame.color,
                    origin: made_by,
                    _credit: None,
                }))
            }
            Some(_) => return Err(ReaderError::FormatChanged),
            None => None,
        };
        drop(decoder);
        drop(input);
        segments.next_first = Some((next, frame));
    }
    Ok(Around::Pair(before, segments.next_first.as_ref().and_then(|(_, frame)| frame.clone())))
}

/// Verify a segment against its index entry (length and digest, bounded
/// blocks), then open it. The decoder must be dropped before the input.
fn open_segment(store: &IndexStore, video: &VideoSegmentRef, caps: VideoCaps) -> Result<(VideoFileDecoder, DecoderInput), ReaderError> {
    store.verify_file(&video.file, video.bytes, &video.digest).map_err(index_error)?;
    let input = DecoderInput::prepare(&store.file_path(&video.file), MediaType::Mp4).map_err(ReaderError::Codec)?;
    let decoder = VideoFileDecoder::open(&input.path).map_err(codec)?;
    let info = decoder.info();
    if info.width != caps.width || info.height != caps.height {
        return Err(ReaderError::FormatChanged);
    }
    Ok((decoder, input))
}

#[cfg(test)]
mod kept_frames_tests {
    use super::*;
    use crate::payload::Lane;

    fn frame(pts: i64) -> Arc<VideoFramePayload> {
        Arc::new(VideoFramePayload {
            at: pts,
            local_pts_100ns: pts,
            width: 2,
            height: 2,
            nv12: vec![0; 6],
            color: VideoColor::BT709,
            origin: BufOrigin { reader: 0, lane: Lane::Video },
            _credit: None,
        })
    }

    fn pts(around: Option<Around>) -> Option<(Option<i64>, Option<i64>)> {
        match around? {
            Around::Pair(before, after) => Some((before.map(|f| f.local_pts_100ns), after.map(|f| f.local_pts_100ns))),
            Around::Stale => None,
        }
    }

    /// A walk of frames at 0, 10, 20, 30 kept in decode order.
    fn walk(budget: usize) -> KeptFrames {
        let mut kept = KeptFrames { budget, ..KeptFrames::default() };
        let mut before = None;
        for p in [0, 10, 20, 30] {
            kept.insert(before, frame(p), 30);
            before = Some(p);
        }
        kept
    }

    #[test]
    fn a_walk_answers_every_time_between_its_frames() {
        let kept = walk(1 << 20);
        assert_eq!(pts(kept.pair(0)), Some((Some(0), Some(10))));
        assert_eq!(pts(kept.pair(15)), Some((Some(10), Some(20))));
        assert_eq!(pts(kept.pair(29)), Some((Some(20), Some(30))));
        // The last walked frame has no known successor, before the target.
        assert!(kept.pair(-1).is_none());
        assert!(kept.pair(30).is_none());
    }

    #[test]
    fn the_file_s_last_frame_answers_past_it() {
        let mut kept = walk(1 << 20);
        kept.mark_last(30);
        assert_eq!(pts(kept.pair(1_000)), Some((Some(30), None)));
    }

    #[test]
    fn frames_decoded_after_a_seek_are_not_linked_to_the_ones_before() {
        let mut kept = walk(1 << 20);
        // A seek: 50 is decoded with no known predecessor.
        kept.insert(None, frame(50), 50);
        assert!(kept.pair(40).is_none());
        assert_eq!(pts(kept.pair(25)), Some((Some(20), Some(30))));
    }

    #[test]
    fn over_budget_the_frames_farthest_from_the_target_go() {
        // Room for three 6-byte frames; the target is at 30.
        let kept = walk(18);
        assert_eq!(kept.frames.len(), 3);
        assert!(!kept.frames.contains_key(&0));
        assert_eq!(kept.bytes, 18);
    }

    #[test]
    fn without_a_budget_nothing_is_kept() {
        let kept = walk(0);
        assert!(kept.frames.is_empty());
        assert!(kept.pair(15).is_none());
    }
}
