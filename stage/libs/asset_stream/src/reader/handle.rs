//! The owner's side of the reader: one handle, the load registry, and the
//! two workers' queues.
//!
//! Every load the handle admitted is in its registry, whatever state it is
//! in (being described, waiting to be handed to its worker, opening, open,
//! closing, or holding its final answer ready), until its `OpenFailed` or
//! `Closed` is taken, so [`MAX_READER_CONSUMERS`] counts all of them. A load
//! lives on ONE worker and every command for it goes through that worker's
//! queue in order: a close never overtakes the open it closes.
//!
//! A strict audio load of a finished file is described on the video worker
//! first. While that runs its page group waits here: no buffer of it exists
//! until the audio worker opens the load. A close meanwhile is remembered
//! and answered only when the description comes back; the description is
//! then dropped and never opens anything. A description that finds the
//! audio queue full waits here (at most one command per load) and is handed
//! over on the next call; a close of it then answers at once, since no
//! worker holds anything of it.
//!
//! **A worker that ends** while the handle lives (it can only have
//! panicked) is noticed once every result it sent was taken and its task
//! has finished, so everything it held is gone. Then each load it served
//! gets exactly one final answer, held in the registry: `Closed` if a close
//! was sent, `OpenFailed` if its `Opened` was never taken, and otherwise a
//! `Failed` notice, after which a close is answered `Closed` here. New opens
//! on that worker are admitted and answered `OpenFailed` the same way, never
//! refused as busy for ever. Its payloads can no longer be given back:
//! [`AvReader::recycle`] hands them back, and the owner drops them off any
//! realtime thread ([`AvReader::worker_ended`] says when).
//!
//! **Stopping** is a global cancellation. [`AvReader::stop`] (and dropping
//! the reader) ends both workers without waiting; no load is promised a
//! `Closed` after it. From then on every open, command, close, recycle and
//! seek update is refused (handed back), nothing waiting is handed to a
//! worker, and [`AvReader::is_open`] is `false` for every load. The owner
//! treats all its loads as cancelled, waits for [`AvReader::stopped`], and
//! may still drain [`AvReader::try_recv`] to dispose of what was queued: it
//! then yields payloads and failures, never `Opened` or `Advanced` (nothing
//! is adopted after a stop), and a description that comes back late is
//! dropped, never opened. Everything a worker holds (open decoders, pools,
//! results not yet handed over) is dropped on that worker. Results handed
//! over and not taken, and whatever the registry still holds (a waiting
//! description, a group with no buffer), are dropped where the reader is
//! dropped, on the owner's thread: an owner that must keep final drops of
//! payloads elsewhere drains and disposes of them before it drops the
//! reader.

use super::upload::{FrameGroupIdentity, Reservation};
use super::{
    lane_result_bytes, Command, FrameGroup, LaneResult, OpenRefusal, OpenSource, Payload, Purpose, ReadSource, ReaderCommand, ReaderError,
    ReaderResult, TimelineSnapshot, UploadRefusal, UploadReturn, UploadTicket, Worker, AUDIO_LANE_COMMANDS, AUDIO_LANE_RESULTS, AUDIO_LANE_RESULT_BYTES, LOCAL_RESULTS, VIDEO_LANE_COMMANDS,
    VIDEO_LANE_RESULTS, VIDEO_LANE_RESULT_BYTES,
};
use crate::identity::{ConsumerLoad, ReadTag};
use crate::limits::MAX_READER_CONSUMERS;
use crate::payload::{BufOrigin, GroupIdentity, Lane, PageGroup};
use makepad_platform::thread::{SpawnError, TaskHandle, ThreadOptions, ThreadSpawner};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;

/// Where the next result is taken from, in turn. A source with nothing
/// passes its turn, so each one with a result waiting is served within one
/// round of six takes: sound gets three of them, pictures two, the handle's
/// own answers one.
const TURNS: [Source; 6] = [Source::Audio, Source::Video, Source::Audio, Source::Local, Source::Audio, Source::Video];

#[derive(Clone, Copy)]
enum Source {
    Audio,
    Video,
    /// Final answers held in the registry, then the handle's own answers.
    Local,
}

static NEXT_READER: AtomicU64 = AtomicU64::new(1);

/// The error a load is answered with when its worker ended under it.
fn worker_lost() -> ReaderError {
    ReaderError::Unsupported("the reader worker ended")
}

/// One worker, from the owner's side.
struct LaneLink {
    /// `None` once stopped: the worker's `recv` then ends.
    commands: Option<SyncSender<Command>>,
    results: Receiver<LaneResult>,
    in_flight: Arc<AtomicUsize>,
    task: TaskHandle<()>,
    /// The worker ended while the reader lived, and its loads were settled.
    gone: bool,
}

struct LaneSpec {
    lane: Lane,
    suffix: &'static str,
    commands: usize,
    results: usize,
    budget: usize,
    /// Decoded bytes a raw file video load keeps (video lane only).
    keep: usize,
}

enum Take {
    Result(LaneResult),
    Empty,
    /// Every result was taken and the worker's task has finished.
    Ended,
}

impl LaneLink {
    fn spawn(spawner: &ThreadSpawner, name: &str, reader: u64, spec: LaneSpec, alive: &Arc<AtomicBool>) -> Result<Self, SpawnError> {
        let (commands, take) = sync_channel(spec.commands);
        let (give, results) = sync_channel(spec.results);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let worker_in_flight = in_flight.clone();
        let worker_alive = alive.clone();
        let origin = BufOrigin { reader, lane: spec.lane };
        let (budget, keep) = (spec.budget, spec.keep);
        let options = ThreadOptions { name: Some(Arc::from(format!("{name}-{}", spec.suffix))), ..ThreadOptions::default() };
        let task = spawner.spawn_worker(options, move || Worker::new(origin, budget, keep, give, worker_in_flight, worker_alive).run(take))?;
        Ok(Self { commands: Some(commands), results, in_flight, task, gone: false })
    }

    fn try_send(&self, command: Command) -> Result<(), Command> {
        match &self.commands {
            None => Err(command),
            Some(commands) => commands.try_send(command).map_err(|error| match error {
                TrySendError::Full(command) | TrySendError::Disconnected(command) => command,
            }),
        }
    }

    fn take(&self) -> Take {
        match self.results.try_recv() {
            Ok(result) => {
                // The worker added exactly this before sending it.
                self.in_flight.fetch_sub(lane_result_bytes(&result), Ordering::AcqRel);
                Take::Result(result)
            }
            Err(TryRecvError::Empty) => Take::Empty,
            // The worker drops its sender before its loads; only a finished
            // task proves they are gone.
            Err(TryRecvError::Disconnected) if self.task.is_finished() => Take::Ended,
            Err(TryRecvError::Disconnected) => Take::Empty,
        }
    }
}

/// One admitted load.
struct Entry {
    serial: Arc<AtomicU64>,
    lane: Lane,
    /// The page group this load holds, reserved from admission until the
    /// worker let it go (its `OpenFailed` or `Closed` taken, or the worker
    /// ended). `None` for an unpaged load and for one refused a group
    /// another load holds.
    group: Option<GroupIdentity>,
    /// Opened with a strict entry point ([`AvReader::open_audio_paged`],
    /// [`AvReader::open_video`]): its reads and advances are handed back
    /// until its `Opened` was taken. Legacy loads keep their queue order
    /// (an open, then reads behind it) instead.
    strict: bool,
    /// An admitted video load's group and request state.
    admitted: Option<Admitted>,
    stage: Stage,
}

/// What the handle keeps of an admitted video load's upload requests.
struct Admitted {
    group: FrameGroupIdentity,
    /// `(seek serial, request)` of the last request handed to the worker.
    last: Option<(u64, u64)>,
    /// The one request of that serial not answered yet.
    outstanding: Option<(u64, u64)>,
}

enum Stage {
    /// Being described on the video worker. `closing`: closed meanwhile.
    Describing { group: PageGroup, closing: bool },
    /// Described; the audio open waits for room in the audio queue.
    Forwarding(Command),
    /// Handed to its worker. `opened`: its `Opened` was taken; `closing`: a
    /// close was sent.
    Live { opened: bool, closing: bool },
    /// Its worker ended under it while open. `notice` (a `Failed`) is still
    /// to be taken; then the load waits for its close, answered here.
    Lost { notice: Option<ReaderResult> },
    /// Its final answer (`OpenFailed` or `Closed`), ready. Taken, the load is
    /// gone.
    Settled(ReaderResult),
}

/// The one reader handle. See the module docs of [`crate::reader`].
///
/// Which calls isolate what:
///
/// * [`Self::open_audio_paged`] — the only load whose reads are never
///   queued behind video work. It is the one a realtime audio consumer
///   uses.
/// * [`Self::open_video`] — pictures only; no Stage audio decoder is opened.
/// * [`Self::open`], [`Self::open_paged`] — legacy combined loads, run whole
///   on the video worker exactly as before (audio-only and picture-only
///   files included). Their sound waits behind their own and every other
///   load's video work.
pub struct AvReader {
    reader: u64,
    audio: LaneLink,
    video: LaneLink,
    /// At most [`MAX_READER_CONSUMERS`] entries.
    loads: HashMap<ConsumerLoad, Entry>,
    /// Answers for loads the handle does not hold, at most [`LOCAL_RESULTS`].
    local: VecDeque<ReaderResult>,
    turn: usize,
    alive: Arc<AtomicBool>,
}

impl AvReader {
    /// Spawn the two workers. Call once; feed it for the life of the app.
    /// If the second cannot start, the first ends as soon as it sees its
    /// queue gone (it has nothing to do yet).
    pub fn spawn(spawner: &ThreadSpawner, name: &str) -> Result<Self, SpawnError> {
        Self::spawn_keeping(spawner, name, 0)
    }

    /// [`Self::spawn`], and every raw file video load ([`Self::open_video`])
    /// keeps up to `keep_bytes` of the frames its reads decode, so reads
    /// back and forth over what was already walked (scrubbing) are answered
    /// without seeking or decoding again. The kept frames are no part of
    /// the in-flight budget; they live on the video worker until evicted
    /// (farthest from the newest read first) or their load closes.
    pub fn spawn_keeping(spawner: &ThreadSpawner, name: &str, keep_bytes: usize) -> Result<Self, SpawnError> {
        let reader = NEXT_READER.fetch_add(1, Ordering::Relaxed);
        let alive = Arc::new(AtomicBool::new(true));
        let audio_spec =
            LaneSpec { lane: Lane::Audio, suffix: "audio", commands: AUDIO_LANE_COMMANDS, results: AUDIO_LANE_RESULTS, budget: AUDIO_LANE_RESULT_BYTES, keep: 0 };
        let video_spec =
            LaneSpec { lane: Lane::Video, suffix: "video", commands: VIDEO_LANE_COMMANDS, results: VIDEO_LANE_RESULTS, budget: VIDEO_LANE_RESULT_BYTES, keep: keep_bytes };
        let audio = LaneLink::spawn(spawner, name, reader, audio_spec, &alive)?;
        let video = match LaneLink::spawn(spawner, name, reader, video_spec, &alive) {
            Ok(video) => video,
            Err(error) => {
                alive.store(false, Ordering::Release);
                // Dropping `audio` drops its queue: its worker's `recv` ends.
                drop(audio);
                return Err(error);
            }
        };
        let loads = HashMap::with_capacity(MAX_READER_CONSUMERS);
        Ok(Self { reader, audio, video, loads, local: VecDeque::with_capacity(LOCAL_RESULTS), turn: 0, alive })
    }

    /// Open a LEGACY combined load (sound and pictures) on the video worker;
    /// its seek serial starts at 0. No isolation of its sound from video
    /// work: see [`Self::open_audio_paged`].
    pub fn open(&mut self, load: ConsumerLoad, source: ReadSource) -> Result<(), (OpenRefusal, ReadSource)> {
        self.open_read(load, source, Lane::Video, Purpose::Combined)
    }

    /// Open a LEGACY combined load whose audio pages are ADMITTED under
    /// `group`: every page buffer is allocated only after one of the group's
    /// permits was taken, and carries it until the buffer is dropped. When
    /// the group has no permit left, a read stops early at what it actually
    /// delivered (`Done::reached`) — backpressure, never a gap or a failure.
    /// Runs whole on the video worker, like [`Self::open`]. A refusal hands
    /// the source and the group back. A group another load (on either
    /// worker) holds is answered `OpenFailed`, and gets no pool.
    pub fn open_paged(&mut self, load: ConsumerLoad, source: ReadSource, group: PageGroup) -> Result<(), (OpenRefusal, ReadSource, PageGroup)> {
        self.pump();
        if let Err(refusal) = self.admit(load) {
            return Err((refusal, source, group));
        }
        match self.open_on(load, OpenSource::Read(source), Some(group), Lane::Video, Purpose::Combined) {
            Ok(()) => Ok(()),
            Err((source, group)) => Err((OpenRefusal::QueueFull, read_source(source), group.expect("sent with a group"))),
        }
    }

    /// Open a pictures-only load on the video worker: no Stage audio cursor
    /// is made for it (on Apple the platform's full opener may still set up
    /// the file's sound on its own). A finished file the full opener refuses
    /// is refused (`OpenFailed`), never taken for one without a picture.
    /// Its reads are handed back until its `Opened` was taken. Refusals as
    /// [`Self::open`].
    pub fn open_video(&mut self, load: ConsumerLoad, source: ReadSource) -> Result<(), (OpenRefusal, ReadSource)> {
        self.open_read(load, source, Lane::Video, Purpose::Video)
    }

    /// Open a strict sound-only load on the audio worker, its pages admitted
    /// under `group` as for [`Self::open_paged`]; a refusal hands back
    /// exactly what was given.
    ///
    /// A growing (indexed) timeline opens directly. A finished file is first
    /// described on the video worker with the full opener (that may delay
    /// THIS open behind video work, never reads of loads already open); a
    /// file the full opener refuses is answered `OpenFailed` with its error,
    /// never guessed to have no picture, so an audio-only finished file may
    /// not open here. Then only its sound is opened on the audio worker and
    /// checked against the description (rate, channels, duration),
    /// `OpenFailed(FormatChanged)` if they differ. `Opened` carries the
    /// description. Until its `Opened` was taken, its reads and advances are
    /// handed back as if the queue were full.
    ///
    /// This isolates the load from video work only: its own decoder, index
    /// reads and sidecar checks may still stall, so a realtime consumer
    /// prebuffers.
    pub fn open_audio_paged(&mut self, load: ConsumerLoad, source: ReadSource, group: PageGroup) -> Result<(), (OpenRefusal, ReadSource, PageGroup)> {
        self.pump();
        if let Err(refusal) = self.admit(load) {
            return Err((refusal, source, group));
        }
        match source {
            ReadSource::CompletedFile { .. } if self.video.gone || self.audio.gone => {
                // Nothing can describe or open it: answered, not refused.
                self.settle_new(load, Lane::Audio);
                Ok(())
            }
            ReadSource::CompletedFile { media, path, container } if self.holder_of(&group).is_none() => {
                match self.video.try_send(Command::Describe { load, media, path, container }) {
                    Ok(()) => {
                        let entry = Entry {
                            serial: Arc::new(AtomicU64::new(0)),
                            lane: Lane::Audio,
                            group: Some(group.identity()),
                            strict: true,
                            admitted: None,
                            stage: Stage::Describing { group, closing: false },
                        };
                        self.loads.insert(load, entry);
                        Ok(())
                    }
                    Err(Command::Describe { media, path, container, .. }) => {
                        Err((OpenRefusal::QueueFull, ReadSource::CompletedFile { media, path, container }, group))
                    }
                    Err(_) => unreachable!("a describe command comes back as itself"),
                }
            }
            // A timeline, or a group already held (refused on the audio
            // worker, as `open_paged` refuses it).
            source => match self.open_on(load, OpenSource::Read(source), Some(group), Lane::Audio, Purpose::Audio) {
                Ok(()) => Ok(()),
                Err((source, group)) => Err((OpenRefusal::QueueFull, read_source(source), group.expect("sent with a group"))),
            },
        }
    }

    /// Open an ADMITTED video load on the video worker: pictures are read
    /// only as uploads ([`Self::read_video_upload`]), decoded frames never
    /// leave the worker, and every buffer is paid by `group`'s credits and
    /// permits. The finished file is probed strictly (a full opener failure
    /// is its failure), as for [`Self::open_video`].
    ///
    /// `group` must have been made for exactly `load` and never have served
    /// an open: otherwise the open is answered `OpenFailed`, and the group
    /// is left as it was. A picture larger than the group's size, or of an
    /// odd side, is answered `OpenFailed(OverLimit)`; so is a file without a
    /// picture (`Unsupported`). Before `Opened` is sent the group's
    /// [`FrameGroup::opened_size`] is set. The worker checks again that the
    /// group's media and load are exactly the opened ones.
    ///
    /// The group is reserved atomically (across every reader holding a
    /// clone) before the open is queued, and claimed for good only when the
    /// open is admitted. A refusal hands back exactly what was given, the
    /// reservation undone, so the group stays good for a retry;
    /// `QueueFull` also answers a group some other reader is handing over
    /// at this very moment.
    pub fn open_video_admitted(&mut self, load: ConsumerLoad, source: ReadSource, group: FrameGroup) -> Result<(), (OpenRefusal, ReadSource, FrameGroup)> {
        self.pump();
        if let Err(refusal) = self.admit(load) {
            return Err((refusal, source, group));
        }
        // A group made for another load is refused without touching its
        // reservation (it may be good for its own load).
        let refuse = if group.load() != load {
            Some(ReaderError::Unsupported("a frame group serves the load it was made for"))
        } else {
            match group.reserve() {
                Reservation::Reserved => None,
                Reservation::Claimed => Some(ReaderError::Unsupported("a frame group serves one open")),
                Reservation::Busy => return Err((OpenRefusal::QueueFull, source, group)),
            }
        };
        if self.video.gone {
            // Nothing can open it: answered, not refused. A group reserved
            // here is spent, since no worker will ever hold it.
            if refuse.is_none() {
                group.commit();
                group.mark_finished();
            }
            self.settle_new(load, Lane::Video);
            return Ok(());
        }
        let own = refuse.is_none().then(|| group.clone());
        let serial = Arc::new(AtomicU64::new(0));
        let command = Command::Open {
            load,
            source: OpenSource::Read(source),
            serial: serial.clone(),
            group: None,
            frames: Some(group),
            purpose: Purpose::Admitted,
            refuse,
        };
        match self.video.try_send(command) {
            Ok(()) => {
                let admitted = own.map(|group| {
                    group.commit();
                    Admitted { group: group.identity(), last: None, outstanding: None }
                });
                let stage = Stage::Live { opened: false, closing: false };
                self.loads.insert(load, Entry { serial, lane: Lane::Video, group: None, strict: true, admitted, stage });
                Ok(())
            }
            Err(Command::Open { source, frames, .. }) => {
                if let Some(group) = own {
                    group.release();
                }
                Err((OpenRefusal::QueueFull, read_source(source), frames.expect("sent with a group")))
            }
            Err(_) => unreachable!("an open command comes back as itself"),
        }
    }

    /// Ask an admitted load for the frame shown at timeline tick `at`, its
    /// buffer paid by `ticket` (taken from the load's group BEFORE asking).
    /// `tag` must be the load's current seek serial and `request` newer than
    /// the last request handed over in it. A refusal hands the ticket back
    /// and records nothing: the exact request may be retried.
    ///
    /// Handed over, it is answered by exactly one `ReaderResult::VideoUpload`
    /// of this tag and request, whatever happens to it: a frame, no frame,
    /// an error, or `Cancelled` when a newer seek serial or a close moved
    /// past it (only a stop or its worker ending leave it unanswered). Only
    /// that exact answer clears it, and until it is taken the load refuses
    /// every other request (`Outstanding`), whatever its serial: one
    /// physical read per load at a time.
    pub fn read_video_upload(&mut self, tag: ReadTag, at: u64, request: u64, ticket: UploadTicket) -> Result<(), (UploadRefusal, UploadTicket)> {
        if !self.alive.load(Ordering::Acquire) {
            return Err((UploadRefusal::Stopped, ticket));
        }
        self.pump();
        let Some(entry) = self.loads.get_mut(&tag.load) else {
            return Err((UploadRefusal::UnknownLoad, ticket));
        };
        let Some(admitted) = entry.admitted.as_mut() else {
            return Err((UploadRefusal::NotAdmitted, ticket));
        };
        match entry.stage {
            Stage::Live { opened: true, closing: false } => {}
            Stage::Live { opened: false, closing: false } => return Err((UploadRefusal::NotOpened, ticket)),
            _ => return Err((UploadRefusal::Closing, ticket)),
        }
        let current = entry.serial.load(Ordering::Acquire);
        if tag.seek_serial != current {
            return Err((UploadRefusal::StaleRequest, ticket));
        }
        if !admitted.group.owns(&ticket) {
            return Err((UploadRefusal::ForeignTicket, ticket));
        }
        // Every request handed over gets exactly one answer (`Cancelled`
        // if a newer serial moved past it): until it is taken, no other
        // request is handed over, whatever its serial.
        if admitted.outstanding.is_some() {
            return Err((UploadRefusal::Outstanding, ticket));
        }
        if admitted.last.is_some_and(|(serial, last)| serial == current && request <= last) {
            return Err((UploadRefusal::StaleRequest, ticket));
        }
        match self.video.try_send(Command::ReadUpload { tag, at, request, ticket }) {
            Ok(()) => {
                admitted.last = Some((current, request));
                admitted.outstanding = Some((current, request));
                Ok(())
            }
            Err(Command::ReadUpload { ticket, .. }) => Err((UploadRefusal::QueueFull, ticket)),
            Err(_) => unreachable!("an upload read comes back as itself"),
        }
    }

    /// Give an upload buffer back for its final drop on the video worker
    /// that made it, whatever became of its load. One of another reader, a
    /// full queue, a stop or an ended worker hands it back: drop it then
    /// off any realtime thread.
    pub fn return_upload(&mut self, upload: UploadReturn) -> Result<(), UploadReturn> {
        let origin = upload.origin();
        if !self.alive.load(Ordering::Acquire) || origin.reader != self.reader || origin.lane != Lane::Video {
            return Err(upload);
        }
        self.video.try_send(Command::ReturnUpload(upload)).map_err(|command| match command {
            Command::ReturnUpload(upload) => upload,
            _ => unreachable!("a returned upload comes back as itself"),
        })
    }

    /// Whether `load` is admitted (in any state until its `OpenFailed` or
    /// `Closed` is taken) and the reader is not stopped.
    pub fn is_open(&self, load: ConsumerLoad) -> bool {
        self.alive.load(Ordering::Acquire) && self.loads.contains_key(&load)
    }

    /// Whether a worker ended while the reader lived (see the module docs).
    pub fn worker_ended(&self) -> bool {
        self.audio.gone || self.video.gone
    }

    /// Whether `payload` was made by this reader. A payload of another is
    /// never taken by [`Self::recycle`]; give it back to its own reader.
    pub fn owns(&self, payload: &Payload) -> bool {
        payload_origin(payload).reader == self.reader
    }

    /// Declare the consumer's current seek serial: in-flight work and queued
    /// results of any other serial are abandoned on its worker. Ignored for
    /// a load being closed, settled or lost, and after a stop: their
    /// cancellation stands.
    pub fn set_seek_serial(&self, load: ConsumerLoad, serial: u64) {
        if !self.alive.load(Ordering::Acquire) {
            return;
        }
        let Some(entry) = self.loads.get(&load) else { return };
        let current = matches!(
            entry.stage,
            Stage::Describing { closing: false, .. } | Stage::Forwarding(_) | Stage::Live { closing: false, .. }
        );
        if current {
            entry.serial.store(serial, Ordering::Release);
        }
    }

    /// Close a load. Its work stops at once; `Closed` comes when its worker
    /// has let everything of it go (for a load being described: when the
    /// description is back). A load the handle does not hold is answered
    /// `Closed` here. `Err` only when the close finds no room: try again
    /// after taking results, and after a stop, which cancelled every load.
    pub fn close(&mut self, load: ConsumerLoad) -> Result<(), ReaderCommand> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(ReaderCommand::Close { load });
        }
        self.pump();
        let Some(entry) = self.loads.get_mut(&load) else {
            return self.answer(ReaderResult::Closed { load }).map_err(|_| ReaderCommand::Close { load });
        };
        match &mut entry.stage {
            Stage::Describing { closing, .. } => {
                *closing = true;
                entry.serial.store(u64::MAX, Ordering::Release);
            }
            Stage::Forwarding(_) | Stage::Lost { .. } => {
                // No worker holds anything of it: the description, the group
                // (which has no buffer) or the notice go here, and the answer
                // waits in the load's own slot.
                entry.group = None;
                entry.serial.store(u64::MAX, Ordering::Release);
                entry.stage = Stage::Settled(ReaderResult::Closed { load });
            }
            // Its close is queued already; one `Closed` will come.
            Stage::Live { closing: true, .. } => {}
            Stage::Live { closing, .. } => {
                let lane = match entry.lane {
                    Lane::Audio => &self.audio,
                    Lane::Video => &self.video,
                };
                lane.try_send(Command::Public(ReaderCommand::Close { load })).map_err(|_| ReaderCommand::Close { load })?;
                *closing = true;
                entry.serial.store(u64::MAX, Ordering::Release);
            }
            // Its final answer is already waiting.
            Stage::Settled(_) => {}
        }
        Ok(())
    }

    /// Queue a command without waiting; a full queue hands it back.
    ///
    /// Reads and advances go to their load's worker. Of a load the handle
    /// does not hold (or whose worker ended), they are answered `Failed`
    /// here; of a strict audio load not yet on the audio worker, they are
    /// handed back. A payload goes back to the worker of THIS reader that
    /// made it, whatever became of its load; one made by another reader is
    /// handed back untouched (see [`Self::owns`]). A read or advance of a
    /// load being closed is handed back; after a stop, everything is.
    pub fn send(&mut self, command: ReaderCommand) -> Result<(), ReaderCommand> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(command);
        }
        self.pump();
        let load = match &command {
            ReaderCommand::Recycle(payload) => {
                let origin = payload_origin(payload);
                if origin.reader != self.reader {
                    return Err(command);
                }
                return self.link(origin.lane).try_send(Command::Public(command)).map_err(public);
            }
            ReaderCommand::Close { load } => return self.close(*load),
            ReaderCommand::Advance { load, .. } => *load,
            ReaderCommand::ReadAudio { tag, .. } | ReaderCommand::ReadVideo { tag, .. } | ReaderCommand::ReadVideoPair { tag, .. } => tag.load,
        };
        let read = !matches!(command, ReaderCommand::Advance { .. });
        let error = match self.loads.get(&load) {
            // Raw reads of an admitted video load: handed back whole.
            Some(Entry { admitted: Some(_), .. }) if read => return Err(command),
            // Being closed, or strict and not yet `Opened`: handed back.
            Some(Entry { stage: Stage::Live { closing: true, .. }, .. }) | Some(Entry { strict: true, stage: Stage::Live { opened: false, .. }, .. }) => {
                return Err(command)
            }
            Some(Entry { lane, stage: Stage::Live { .. }, .. }) => return self.link(*lane).try_send(Command::Public(command)).map_err(public),
            Some(Entry { stage: Stage::Describing { .. } | Stage::Forwarding(_), .. }) => return Err(command),
            Some(Entry { stage: Stage::Lost { .. }, .. }) => worker_lost(),
            Some(Entry { stage: Stage::Settled(_), .. }) | None => ReaderError::UnknownLoad,
        };
        let tag = match &command {
            ReaderCommand::ReadAudio { tag, .. } | ReaderCommand::ReadVideo { tag, .. } | ReaderCommand::ReadVideoPair { tag, .. } => Some(*tag),
            _ => None,
        };
        self.answer(ReaderResult::Failed { load, tag, error }).map_err(|_| command)
    }

    /// Give a payload back for its final drop on the worker that made it. A
    /// full queue, a payload of another reader, or a worker that ended hands
    /// it back: keep it and retry (or, for the last two, drop it off any
    /// realtime thread), never drop it on a realtime thread.
    pub fn recycle(&mut self, payload: Payload) -> Result<(), Payload> {
        match self.send(ReaderCommand::Recycle(payload)) {
            Ok(()) => Ok(()),
            Err(ReaderCommand::Recycle(payload)) => Err(payload),
            Err(_) => unreachable!("a recycle command comes back as itself"),
        }
    }

    /// The next result, taken from the audio worker, the video worker and
    /// the handle's own answers in a fixed round (see `TURNS`): none of them
    /// waits more than one round while it has one.
    pub fn try_recv(&mut self) -> Option<ReaderResult> {
        self.pump();
        for _ in 0..TURNS.len() {
            let source = TURNS[self.turn];
            self.turn = (self.turn + 1) % TURNS.len();
            let result = match source {
                Source::Audio => self.take_lane(Lane::Audio),
                Source::Video => self.take_lane(Lane::Video),
                Source::Local => self.take_local(),
            };
            if result.is_some() {
                return result;
            }
        }
        None
    }

    /// Stop both workers: no new work is taken, every load's work is stale,
    /// and a worker waiting for a command wakes and ends. Never waits. A
    /// worker inside a decoder or file call ends only when that returns;
    /// [`Self::stopped`] says when both have. See the module docs for where
    /// payloads still queued are dropped.
    pub fn stop(&mut self) {
        self.alive.store(false, Ordering::Release);
        for entry in self.loads.values() {
            entry.serial.store(u64::MAX, Ordering::Release);
        }
        self.audio.commands = None;
        self.video.commands = None;
    }

    /// Whether both workers have actually ended.
    pub fn stopped(&self) -> bool {
        self.audio.task.is_finished() && self.video.task.is_finished()
    }

    // -----------------------------------------------------------------------

    fn link(&self, lane: Lane) -> &LaneLink {
        match lane {
            Lane::Audio => &self.audio,
            Lane::Video => &self.video,
        }
    }

    fn admit(&self, load: ConsumerLoad) -> Result<(), OpenRefusal> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(OpenRefusal::QueueFull);
        }
        if self.loads.contains_key(&load) {
            return Err(OpenRefusal::AlreadyOpen);
        }
        if self.loads.len() >= MAX_READER_CONSUMERS {
            return Err(OpenRefusal::TooManyConsumers);
        }
        Ok(())
    }

    /// Register an admitted open whose worker ended: its answer is ready.
    fn settle_new(&mut self, load: ConsumerLoad, lane: Lane) {
        let entry = Entry {
            serial: Arc::new(AtomicU64::new(u64::MAX)),
            lane,
            group: None,
            strict: true,
            admitted: None,
            stage: Stage::Settled(ReaderResult::OpenFailed { load, error: worker_lost() }),
        };
        self.loads.insert(load, entry);
    }

    /// The load that holds `group`, on either worker, in any state.
    fn holder_of(&self, group: &PageGroup) -> Option<ConsumerLoad> {
        self.loads.iter().find(|(_, entry)| entry.group.as_ref().is_some_and(|held| held.is(group))).map(|(load, _)| *load)
    }

    fn open_read(&mut self, load: ConsumerLoad, source: ReadSource, lane: Lane, purpose: Purpose) -> Result<(), (OpenRefusal, ReadSource)> {
        self.pump();
        if let Err(refusal) = self.admit(load) {
            return Err((refusal, source));
        }
        self.open_on(load, OpenSource::Read(source), None, lane, purpose).map_err(|(source, _)| (OpenRefusal::QueueFull, read_source(source)))
    }

    /// Hand an admitted open to its worker and register the load. A group
    /// another load holds is sent to be refused there, and is not reserved.
    fn open_on(
        &mut self,
        load: ConsumerLoad,
        source: OpenSource,
        group: Option<PageGroup>,
        lane: Lane,
        purpose: Purpose,
    ) -> Result<(), (OpenSource, Option<PageGroup>)> {
        if self.link(lane).gone {
            // The source and the group (no buffer of it exists) go here.
            self.settle_new(load, lane);
            return Ok(());
        }
        let refuse = group
            .as_ref()
            .filter(|group| self.holder_of(group).is_some())
            .map(|_| ReaderError::Unsupported("a page group serves one open load at a time"));
        let identity = group.as_ref().filter(|_| refuse.is_none()).map(PageGroup::identity);
        let serial = Arc::new(AtomicU64::new(0));
        match self.link(lane).try_send(Command::Open { load, source, serial: serial.clone(), group, frames: None, purpose, refuse }) {
            Ok(()) => {
                let strict = purpose != Purpose::Combined;
                let entry = Entry { serial, lane, group: identity, strict, admitted: None, stage: Stage::Live { opened: false, closing: false } };
                self.loads.insert(load, entry);
                Ok(())
            }
            Err(Command::Open { source, group, .. }) => Err((source, group)),
            Err(_) => unreachable!("an open command comes back as itself"),
        }
    }

    /// Hand waiting described opens to the audio worker, without waiting.
    fn pump(&mut self) {
        if !self.alive.load(Ordering::Acquire) {
            return;
        }
        let audio = &self.audio;
        for entry in self.loads.values_mut() {
            if matches!(entry.stage, Stage::Forwarding(_)) {
                let Stage::Forwarding(command) = std::mem::replace(&mut entry.stage, Stage::Live { opened: false, closing: false }) else {
                    unreachable!()
                };
                if let Err(command) = audio.try_send(command) {
                    entry.stage = Stage::Forwarding(command);
                }
            }
        }
    }

    /// One result of a worker, for the owner. The worker's internal answers
    /// (descriptions) are handled here and the next is taken, up to the
    /// queue's bound.
    fn take_lane(&mut self, lane: Lane) -> Option<ReaderResult> {
        loop {
            match self.link(lane).take() {
                Take::Empty => return None,
                Take::Ended => {
                    self.lane_ended(lane);
                    return None;
                }
                Take::Result(LaneResult::Public(result)) => {
                    if !self.alive.load(Ordering::Acquire) && matches!(result, ReaderResult::Opened { .. } | ReaderResult::Advanced { .. }) {
                        // Nothing is adopted after a stop.
                        continue;
                    }
                    self.observe(&result);
                    return Some(result);
                }
                Take::Result(LaneResult::Described { load, result }) => {
                    if let Some(answer) = self.described(load, result) {
                        return Some(answer);
                    }
                }
            }
        }
    }

    /// A final answer held in the registry, else one of the handle's own.
    fn take_local(&mut self) -> Option<ReaderResult> {
        let ready = self.loads.iter().find(|(_, entry)| matches!(entry.stage, Stage::Settled(_) | Stage::Lost { notice: Some(_) })).map(|(load, _)| *load);
        if let Some(load) = ready {
            let entry = self.loads.get_mut(&load)?;
            if let Stage::Lost { notice } = &mut entry.stage {
                return notice.take();
            }
            let Stage::Settled(result) = self.loads.remove(&load)?.stage else { unreachable!() };
            return Some(result);
        }
        self.local.pop_front()
    }

    /// Keep the registry in step with what a worker answered.
    fn observe(&mut self, result: &ReaderResult) {
        match result {
            ReaderResult::Opened { load, .. } => {
                if let Some(Entry { stage: Stage::Live { opened, .. }, .. }) = self.loads.get_mut(load) {
                    *opened = true;
                }
            }
            ReaderResult::OpenFailed { load, .. } | ReaderResult::Closed { load } => {
                if matches!(self.loads.get(load), Some(Entry { stage: Stage::Live { .. }, .. })) {
                    self.loads.remove(load);
                }
            }
            // Only the exact answer clears its request: an older one never
            // clears a newer.
            ReaderResult::VideoUpload { tag, request, .. } => {
                if let Some(Entry { admitted: Some(admitted), .. }) = self.loads.get_mut(&tag.load) {
                    if admitted.outstanding == Some((tag.seek_serial, *request)) {
                        admitted.outstanding = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// A worker ended while the reader lives, every result it sent taken and
    /// its task finished: each load it served gets its final answer, once.
    fn lane_ended(&mut self, lane: Lane) {
        if !self.alive.load(Ordering::Acquire) || self.link(lane).gone {
            return;
        }
        match lane {
            Lane::Audio => self.audio.gone = true,
            Lane::Video => self.video.gone = true,
        }
        for (load, entry) in self.loads.iter_mut() {
            let load = *load;
            let served = match &entry.stage {
                Stage::Live { .. } => entry.lane == lane,
                Stage::Describing { .. } => lane == Lane::Video,
                Stage::Forwarding(_) => lane == Lane::Audio,
                Stage::Lost { .. } | Stage::Settled(_) => false,
            };
            if !served {
                continue;
            }
            entry.group = None;
            entry.stage = match std::mem::replace(&mut entry.stage, Stage::Lost { notice: None }) {
                Stage::Describing { closing: true, .. } | Stage::Live { closing: true, .. } => Stage::Settled(ReaderResult::Closed { load }),
                Stage::Live { opened: true, .. } => Stage::Lost { notice: Some(ReaderResult::Failed { load, tag: None, error: worker_lost() }) },
                _ => Stage::Settled(ReaderResult::OpenFailed { load, error: worker_lost() }),
            };
        }
    }

    /// A description is back. Returns what the owner is to be told now, if
    /// anything.
    fn described(&mut self, load: ConsumerLoad, result: Result<Arc<TimelineSnapshot>, ReaderError>) -> Option<ReaderResult> {
        // The entry stays in `Describing` until exactly this answer.
        let entry = self.loads.get_mut(&load)?;
        if !matches!(entry.stage, Stage::Describing { .. }) {
            return None;
        }
        let Stage::Describing { group, closing } = std::mem::replace(&mut entry.stage, Stage::Lost { notice: None }) else { unreachable!() };
        if !self.alive.load(Ordering::Acquire) {
            // Stopped: the description is dropped, never opened, and no
            // answer is promised.
            drop(group);
            self.loads.remove(&load);
            return None;
        }
        // Closed meanwhile, failed, or with no audio worker to open it:
        // nothing is opened, and the group never had a buffer.
        let settled = match result {
            _ if closing => ReaderResult::Closed { load },
            Err(error) => ReaderResult::OpenFailed { load, error },
            Ok(_) if self.audio.gone => ReaderResult::OpenFailed { load, error: worker_lost() },
            Ok(snapshot) => {
                let command = Command::Open {
                    load,
                    source: OpenSource::Described(snapshot),
                    serial: entry.serial.clone(),
                    group: Some(group),
                    frames: None,
                    purpose: Purpose::Audio,
                    refuse: None,
                };
                entry.stage = match self.audio.try_send(command) {
                    Ok(()) => Stage::Live { opened: false, closing: false },
                    Err(command) => Stage::Forwarding(command),
                };
                return None;
            }
        };
        self.loads.remove(&load);
        Some(settled)
    }

    /// Queue an answer for a load the handle does not hold.
    fn answer(&mut self, result: ReaderResult) -> Result<(), ReaderResult> {
        if self.local.len() >= LOCAL_RESULTS {
            return Err(result);
        }
        self.local.push_back(result);
        Ok(())
    }
}

impl Drop for AvReader {
    fn drop(&mut self) {
        self.stop();
    }
}

fn payload_origin(payload: &Payload) -> BufOrigin {
    match payload {
        Payload::Pcm(page) => page.origin(),
        Payload::Frame(frame) => frame.origin(),
    }
}

fn public(command: Command) -> ReaderCommand {
    match command {
        Command::Public(command) => command,
        _ => unreachable!("a public command comes back as itself"),
    }
}

fn read_source(source: OpenSource) -> ReadSource {
    match source {
        OpenSource::Read(source) => source,
        OpenSource::Described(_) => unreachable!("only the handle sends a description, never back to the owner"),
    }
}

impl std::fmt::Debug for AvReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvReader").field("reader", &self.reader).field("loads", &self.loads.len()).finish()
    }
}
