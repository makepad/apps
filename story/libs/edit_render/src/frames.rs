//! Exact clip frames through the Stage AV reader: `ReadVideoPair{at}`
//! answers the frame at or before `at` and the one after it, so each answer
//! covers the span of time its frame shows. One load per
//! clip node, opened ahead of its clip and closed after it. A read is
//! never cancelled: the newest want is sent once the reads in flight are
//! answered, then the frames next to it are read ahead in the direction the
//! wants move, so playback finds its next frames here and a scrub always
//! makes progress (and has [`ClipFrames::nearest`] to show meanwhile).
//!
//! Loads are admitted by rank (the engine ranks clips visible at `t`
//! before preroll ones), never by the order they were asked for, and each
//! holds a slot of the host's decoder budget while the reader has it.

use makepad_asset_data::{AssetRevisionRef, BlobId};
use makepad_stage_asset_stream::{
    AvReader, ConsumerId, ConsumerLoad, Container, MediaIdentity, Payload, ReadSource, ReadTag, ReaderCommand, ReaderResult,
    TimelineSnapshot, VideoFramePayload,
};
use makepad_stage_asset_stream::VideoBudget;
use makepad_widgets::*;
use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

/// What a want of one source at one time has come to.
#[derive(Clone)]
pub enum FrameState {
    /// Asked, or not asked yet: poll again.
    Pending,
    /// The frame at or before the time; `None` when the source shows
    /// nothing there (before its first frame).
    Ready(Option<Arc<VideoFramePayload>>),
    Failed(String),
}

/// Decoder slots the host shares with the cut (Stage: the budget of the
/// VJ decks). A slot is held from admission until the reader
/// has let the load go.
pub trait DecoderBudget {
    /// One slot, released when the returned lease drops; `Err` says what is
    /// short, and the load waits for a later try.
    fn reserve(&self) -> Result<Box<dyn Any>, String>;
}

impl DecoderBudget for Arc<VideoBudget> {
    fn reserve(&self) -> Result<Box<dyn Any>, String> {
        VideoBudget::reserve(self).map(|lease| Box::new(lease) as Box<dyn Any>)
    }
}

struct Load {
    load: ConsumerLoad,
    /// Admission order: lower first.
    rank: usize,
    lease: Option<Box<dyn Any>>,
    identity: MediaIdentity,
    path: PathBuf,
    /// Waiting to be admitted by the reader (it holds at most eight).
    queued: bool,
    snapshot: Option<Arc<TimelineSnapshot>>,
    failed: Option<String>,
    /// Reads sent and not answered, oldest first (the reader answers in
    /// order). A read is never superseded: it finishes, and what it walked
    /// the reader keeps, so a scrub always makes progress.
    asked: VecDeque<u64>,
    /// What the answers cover (see [`Span`]), at most [`KEPT_SPANS`].
    spans: Vec<Span>,
    /// The newest wanted tick, and which way the wants move (-1, 0, 1).
    want: Option<u64>,
    motion: i8,
    opened_at: std::time::Instant,
}

/// One answer: `frame` is what shows from `start` until `end` (the next
/// frame's tick; `None`: to the end of the source). `frame` is `None`
/// before the source's first frame.
struct Span {
    start: u64,
    end: Option<u64>,
    frame: Option<Arc<VideoFramePayload>>,
}

impl Span {
    fn covers(&self, at: u64) -> bool {
        self.start <= at && self.end.map_or(true, |end| at < end)
    }
}

/// Answers a load keeps: the frames around the playhead, so small moves
/// and the next frames of a playback need no read.
const KEPT_SPANS: usize = 16;
/// Reads a load keeps in flight at once: its want, then read-ahead (in the
/// direction the wants move) of up to [`READ_AHEAD`] frames.
const MAX_ASKED: usize = 3;
const READ_AHEAD: usize = 8;

impl Load {
    fn cover(&self, at: u64) -> Option<&Span> {
        cover(&self.spans, at)
    }

    /// The next tick to read ahead of the want, if any: the first frame
    /// past the known ones in the direction of motion, within reach. While
    /// the want itself is still being read, the frames after it are guessed
    /// one frame step on each (a scrub or a play moves on before its read
    /// is back), so a moving want finds its frame already read.
    fn ahead(&self) -> Option<u64> {
        // A frame's span is one frame step (the shortest, for a variable rate).
        let step = self.spans.iter().filter(|span| span.frame.is_some()).filter_map(|span| span.end.map(|end| end.saturating_sub(span.start))).filter(|step| *step > 0).min();
        read_ahead(&self.spans, &self.asked, self.want?, self.motion, step)
    }

}

/// The span that covers `at`, if any.
fn cover(spans: &[Span], at: u64) -> Option<&Span> {
    spans.iter().find(|span| span.covers(at))
}

/// The next tick to read ahead of `want`: the first frame past the known
/// ones in the direction of `motion` (-1 back, else forward), within
/// [`READ_AHEAD`] frames and not already `asked`. While the want itself is
/// still being read, the frames after it are guessed one frame step on each
/// (a scrub or a play moves on before its read is back), so a moving want
/// finds its frame already read. `step` is one frame's ticks, when known.
fn read_ahead(spans: &[Span], asked: &VecDeque<u64>, want: u64, motion: i8, step: Option<u64>) -> Option<u64> {
    let mut at = want;
    for _ in 0..READ_AHEAD {
        let next = match cover(spans, at) {
            Some(span) if motion < 0 => span.start.checked_sub(1)?,
            Some(span) => span.end?,
            None => {
                let step = step?;
                if motion < 0 { at.checked_sub(step)? } else { at + step }
            }
        };
        if cover(spans, next).is_none() && !asked.contains(&next) {
            return Some(next);
        }
        at = next;
    }
    None
}

pub struct ClipFrames {
    /// None without threads (a single-threaded web build): no reader runs
    /// there, and a clip's load fails saying so.
    reader: Option<AvReader>,
    consumer: ConsumerId,
    next_load: u64,
    /// By clip node: two clips of one file read it independently.
    loads: HashMap<String, Load>,
    /// Payloads the reader could not take back yet.
    recycle: Vec<Payload>,
    /// Closes to send again (the reader's queue was full), with the
    /// decoder slot each still holds.
    closing: Vec<(ConsumerLoad, Option<Box<dyn Any>>)>,
    /// Closes sent, holding their slot until the reader's `Closed`.
    closed: Vec<(ConsumerLoad, Option<Box<dyn Any>>)>,
    budget: Option<Box<dyn DecoderBudget>>,
    /// The last reason a load could not be admitted.
    waiting: Option<String>,
}

impl ClipFrames {
    /// `keep_bytes`: decoded frames each load's reader keeps for reads back
    /// and forth ([`AvReader::spawn_keeping`]); 0 for a straight render.
    pub fn spawn(cx: &mut Cx, name: &str, keep_bytes: usize) -> Result<Self, String> {
        let reader = match AvReader::spawn_keeping(&cx.thread_spawner(), name, keep_bytes) {
            Ok(reader) => Some(reader),
            // The page without threads has no reader: a cut without video
            // clips plays, a clip says why it cannot.
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Err(SpawnError::Unsupported) => None,
            Err(e) => return Err(format!("edit frame reader: {e:?}")),
        };
        Ok(Self {
            reader, consumer: ConsumerId(1), next_load: 0, loads: HashMap::new(), recycle: Vec::new(), closing: Vec::new(),
            closed: Vec::new(), budget: None, waiting: None,
        })
    }

    /// Hold a slot of `budget` for every admitted load.
    pub fn set_budget(&mut self, budget: Box<dyn DecoderBudget>) {
        self.budget = Some(budget);
    }

    /// Why the last load could not be admitted yet, if one could not.
    pub fn waiting(&self) -> Option<&str> {
        self.waiting.as_deref()
    }

    /// Make clip `clip`'s file (a finished MP4) readable, admitted at
    /// `rank` (lower first). Asking again only updates the rank.
    pub fn open(&mut self, clip: &str, path: PathBuf, revision: AssetRevisionRef, content: BlobId, rank: usize) {
        if let Some(load) = self.loads.get_mut(clip) {
            load.rank = rank;
            return;
        }
        self.next_load += 1;
        let load = ConsumerLoad { consumer: self.consumer, load: self.next_load };
        let identity = MediaIdentity::Completed { revision, content };
        self.loads.insert(clip.to_string(), Load {
            load, rank, lease: None, identity, path, queued: true, snapshot: None, failed: None, asked: VecDeque::new(), spans: Vec::new(),
            want: None, motion: 0, opened_at: std::time::Instant::now(),
        });
        self.admit();
    }

    /// Change a load's admission rank.
    pub fn rerank(&mut self, clip: &str, rank: usize) {
        if let Some(load) = self.loads.get_mut(clip) {
            load.rank = rank;
        }
    }

    /// Why clip `clip`'s wanted frame is not here, for a person: its file
    /// opening, or its read under way and for how long.
    pub fn why(&self, clip: &str) -> String {
        let Some(load) = self.loads.get(clip) else { return "not opened".into() };
        if let Some(error) = &load.failed {
            return error.clone();
        }
        if load.snapshot.is_none() {
            return format!("opening its file ({:.0} ms)", load.opened_at.elapsed().as_secs_f64() * 1000.0);
        }
        match load.asked.len() {
            0 => "its read is being sent".into(),
            n => format!("decoding ({n} reads in flight)"),
        }
    }

    pub fn is_open(&self, clip: &str) -> bool {
        self.loads.contains_key(clip)
    }

    /// Asked for and not yet admitted by the reader.
    pub fn queued(&self, clip: &str) -> bool {
        self.loads.get(clip).is_some_and(|load| load.queued)
    }

    /// Every clip with a load, admitted or queued.
    pub fn clips(&self) -> Vec<String> {
        self.loads.keys().cloned().collect()
    }

    /// Let clip `clip` go: its reader load closes and its frames go back.
    pub fn close(&mut self, clip: &str) {
        let Some(mut load) = self.loads.remove(clip) else { return };
        for frame in load.spans.drain(..).filter_map(|span| span.frame) {
            self.give_back(Payload::Frame(frame));
        }
        if !load.queued {
            self.closing.push((load.load, load.lease.take()));
        }
        self.retry_closes();
        self.admit();
    }

    /// Closes the reader's full queue handed back, tried again.
    fn retry_closes(&mut self) {
        for (load, lease) in std::mem::take(&mut self.closing) {
            if self.reader.as_mut().is_some_and(|reader| reader.close(load).is_ok()) {
                self.closed.push((load, lease));
            } else {
                self.closing.push((load, lease));
            }
        }
    }

    /// Hand queued loads to the reader in rank order while it, and the
    /// decoder budget, take them.
    fn admit(&mut self) {
        let mut queued: Vec<_> =
            self.loads.iter().filter(|(_, load)| load.queued).map(|(clip, load)| (load.rank, load.load.load, clip.clone())).collect();
        queued.sort();
        self.waiting = None;
        let Some(reader) = &mut self.reader else {
            for (_, _, clip) in queued {
                let load = self.loads.get_mut(&clip).expect("queued load");
                load.failed = Some(format!("{}: video clips need the frame reader's thread, which this page has not", load.path.display()));
            }
            return;
        };
        for (_, _, clip) in queued {
            let lease = match &self.budget {
                Some(budget) => match budget.reserve() {
                    Ok(lease) => Some(lease),
                    Err(why) => {
                        self.waiting = Some(why);
                        break;
                    }
                },
                None => None,
            };
            let load = self.loads.get_mut(&clip).expect("queued load");
            let source = ReadSource::CompletedFile { media: load.identity, path: load.path.clone(), container: Container::Mp4 };
            match reader.open_video(load.load, source) {
                Ok(()) => {
                    load.queued = false;
                    load.lease = lease;
                }
                // The reader holds its eight loads, or its queue is full:
                // this one waits for a close (the slot goes back).
                Err((refusal, _)) => {
                    self.waiting = Some(format!("the reader refused a load: {refusal:?}"));
                    break;
                }
            }
        }
    }

    /// The source's picture size, once its load opened.
    pub fn size(&self, clip: &str) -> Option<(u32, u32)> {
        let video = self.loads.get(clip)?.snapshot.as_ref()?.video?;
        Some((video.width, video.height))
    }

    /// Ask for the frame of `media` at or before `hns` (source time in
    /// 100 ns ticks, the plan's `source_ticks`: integer, so a frame whose
    /// timestamp is exactly the wanted time is that frame), and say how far
    /// that has come. The newest want is read as soon as the load's reads
    /// in flight are answered; frames next to it are read ahead.
    pub fn want(&mut self, clip: &str, hns: i64) -> FrameState {
        let Some(load) = self.loads.get_mut(clip) else { return FrameState::Failed("clip not opened".into()) };
        if let Some(error) = &load.failed {
            return FrameState::Failed(error.clone());
        }
        let Some(snapshot) = &load.snapshot else { return FrameState::Pending };
        // The reader's own tick of that time; past the end, the last frame.
        let Some(ticks) = snapshot.time_base.hns_to_ticks(hns.max(0)) else {
            return FrameState::Failed("source time out of range".into());
        };
        let ticks = ticks.max(0) as u64;
        let committed = snapshot.committed();
        let at = ticks.clamp(committed.start, committed.end.saturating_sub(1).max(committed.start));
        if let Some(previous) = load.want {
            if at != previous {
                load.motion = if at > previous { 1 } else { -1 };
            }
        }
        load.want = Some(at);
        let state = match load.cover(at) {
            Some(span) => FrameState::Ready(span.frame.clone()),
            None => FrameState::Pending,
        };
        Self::ask(self.reader.as_mut(), load);
        state
    }

    /// For a frame blend: the source frame after the one [`Self::want`]
    /// gives for `hns`, and how far `hns` is from that one to it (0..1).
    /// `None` on a frame's own time, at the source's end, or before the
    /// wanted frame is here; `Pending` while the next is still read (the
    /// read-ahead brings it).
    pub fn following(&mut self, clip: &str, hns: i64) -> Option<(FrameState, f32)> {
        let load = self.loads.get_mut(clip)?;
        let snapshot = load.snapshot.as_ref()?;
        let at = snapshot.time_base.hns_to_ticks(hns.max(0))?.max(0) as u64;
        let span = load.cover(at)?;
        let end = span.end?;
        let fraction = at.saturating_sub(span.start) as f64 / end.saturating_sub(span.start).max(1) as f64;
        if fraction <= 1e-6 {
            return None;
        }
        let state = match load.cover(end) {
            Some(next) => FrameState::Ready(next.frame.clone()),
            None => FrameState::Pending,
        };
        Self::ask(self.reader.as_mut(), load);
        Some((state, fraction as f32))
    }

    /// The frame shown nearest `hns` among those already here (not
    /// necessarily the exact one): what a scrub shows while the exact
    /// frame is read.
    pub fn nearest(&self, clip: &str, hns: i64) -> Option<Arc<VideoFramePayload>> {
        let load = self.loads.get(clip)?;
        let at = load.snapshot.as_ref()?.time_base.hns_to_ticks(hns.max(0))?.max(0) as u64;
        load.spans
            .iter()
            .filter_map(|span| span.frame.as_ref().map(|frame| (if span.covers(at) { 0 } else { span.start.abs_diff(at) }, frame)))
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, frame)| frame.clone())
    }

    /// Send the load's want if nothing covers it, else read ahead, while
    /// it has room for reads in flight. A full queue: asked again on the
    /// next poll.
    fn ask(reader: Option<&mut AvReader>, load: &mut Load) {
        let Some(reader) = reader else { return };
        if load.snapshot.is_none() || load.failed.is_some() {
            return;
        }
        while load.asked.len() < MAX_ASKED {
            let next = match load.want {
                // The want first; nothing else goes ahead of it.
                Some(want) if load.cover(want).is_none() && !load.asked.contains(&want) => {
                    if !load.asked.is_empty() {
                        return;
                    }
                    want
                }
                _ => match load.ahead() {
                    Some(next) => next,
                    None => return,
                },
            };
            let tag = ReadTag { media: load.identity, load: load.load, seek_serial: 0 };
            match reader.send(ReaderCommand::ReadVideoPair { tag, at: next }) {
                Ok(()) => load.asked.push_back(next),
                Err(_) => return,
            }
        }
    }

    /// Take every result the reader has. Call on every event while wanting.
    pub fn poll(&mut self) {
        for payload in std::mem::take(&mut self.recycle) {
            self.give_back(payload);
        }
        self.retry_closes();
        while let Some(result) = self.reader.as_mut().and_then(|reader| reader.try_recv()) {
            match result {
                ReaderResult::Opened { load, snapshot } => {
                    if let Some(entry) = self.entry(load) {
                        entry.snapshot = Some(snapshot);
                    }
                }
                ReaderResult::OpenFailed { load, error } => {
                    if let Some(entry) = self.entry(load) {
                        entry.failed = Some(format!("{}: {error:?}", entry.path.display()));
                    }
                }
                ReaderResult::Video { frame, .. } => self.give_back(Payload::Frame(frame)),
                ReaderResult::NoFrame { tag, at } => {
                    self.answer(tag, at, Span { start: at, end: Some(at + 1), frame: None });
                }
                ReaderResult::Failed { load, error, .. } => {
                    if let Some(entry) = self.entry(load) {
                        entry.failed = Some(format!("{}: {error:?}", entry.path.display()));
                    }
                }
                ReaderResult::VideoPair { tag, at, before, after } => {
                    let span = match (&before, &after) {
                        (Some(before), after) => Span { start: before.at.max(0) as u64, end: after.as_ref().map(|after| after.at.max(0) as u64), frame: Some(before.clone()) },
                        // Before the source's first frame (a B-frame file's
                        // first presentation time is a few ticks in): its
                        // first frame, as an editor shows a clip's start.
                        (None, Some(after)) => Span { start: 0, end: Some(after.at.max(0) as u64), frame: Some(after.clone()) },
                        (None, None) => Span { start: at, end: Some(at + 1), frame: None },
                    };
                    self.answer(tag, at, span);
                    for frame in [before, after].into_iter().flatten() {
                        self.give_back(Payload::Frame(frame));
                    }
                }
                ReaderResult::Audio(page) => self.give_back(Payload::Pcm(page)),
                ReaderResult::Closed { load } => {
                    // Its decoder slot goes back now, not at the close.
                    self.closed.retain(|(closed, _)| *closed != load);
                    self.admit();
                }
                ReaderResult::Done { .. } | ReaderResult::Gap { .. } | ReaderResult::Advanced { .. } | ReaderResult::VideoUpload { .. } => {}
            }
        }
        // A slot the host's budget refused may have come back since.
        if self.loads.values().any(|load| load.queued) {
            self.admit();
        }
        // The newest wants, and read-ahead, as answers made room.
        for load in self.loads.values_mut() {
            Self::ask(self.reader.as_mut(), load);
        }
    }

    fn entry(&mut self, load: ConsumerLoad) -> Option<&mut Load> {
        self.loads.values_mut().find(|entry| entry.load == load)
    }

    /// File an answer; the spans farthest from the want go when there
    /// are too many.
    fn answer(&mut self, tag: ReadTag, at: u64, span: Span) {
        let Some(entry) = self.loads.values_mut().find(|entry| entry.load == tag.load) else { return };
        let Some(index) = entry.asked.iter().position(|asked| *asked == at) else { return };
        entry.asked.remove(index);
        entry.spans.retain(|kept| kept.start != span.start);
        entry.spans.push(span);
        let want = entry.want.unwrap_or(at);
        let mut dropped = Vec::new();
        while entry.spans.len() > KEPT_SPANS {
            let far = entry.spans.iter().enumerate().max_by_key(|(_, span)| if span.covers(want) { 0 } else { span.start.abs_diff(want) }).map(|(index, _)| index);
            let Some(far) = far else { break };
            if let Some(frame) = entry.spans.swap_remove(far).frame {
                dropped.push(frame);
            }
        }
        for frame in dropped {
            self.give_back(Payload::Frame(frame));
        }
    }

    fn give_back(&mut self, payload: Payload) {
        let Some(reader) = &mut self.reader else { return };
        if !reader.owns(&payload) {
            return;
        }
        if let Err(payload) = reader.recycle(payload) {
            self.recycle.push(payload);
        }
    }

    /// Stop the reader's workers; call when the cut is done with.
    pub fn stop(&mut self) {
        self.loads.clear();
        self.recycle.clear();
        self.closing.clear();
        self.closed.clear();
        if let Some(reader) = &mut self.reader {
            reader.stop();
        }
    }
}

impl Drop for ClipFrames {
    fn drop(&mut self) {
        if let Some(reader) = &mut self.reader {
            reader.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(ranges: &[(u64, u64)]) -> Vec<Span> {
        ranges.iter().map(|&(start, end)| Span { start, end: Some(end), frame: None }).collect()
    }

    #[test]
    fn playing_forward_reads_the_first_frame_past_the_known_ones() {
        let known = spans(&[(100, 110), (110, 120), (120, 130)]);
        assert_eq!(read_ahead(&known, &VecDeque::new(), 105, 1, Some(10)), Some(130));
        // Already asked: the one after it (a guess, one step on).
        assert_eq!(read_ahead(&known, &VecDeque::from([130]), 105, 1, Some(10)), Some(140));
    }

    #[test]
    fn scrubbing_back_reads_the_frame_before_the_known_ones() {
        let known = spans(&[(100, 110), (110, 120)]);
        assert_eq!(read_ahead(&known, &VecDeque::new(), 115, -1, Some(10)), Some(99));
        // At the source's start there is nothing before.
        let first = spans(&[(0, 10)]);
        assert_eq!(read_ahead(&first, &VecDeque::new(), 5, -1, Some(10)), None);
    }

    #[test]
    fn a_want_still_being_read_is_followed_by_guesses_one_step_on() {
        let asked = VecDeque::from([200]);
        assert_eq!(read_ahead(&[], &asked, 200, 1, Some(10)), Some(210));
        // With no frame step known yet, nothing is guessed.
        assert_eq!(read_ahead(&[], &asked, 200, 1, None), None);
    }

    #[test]
    fn read_ahead_stops_within_reach() {
        let known: Vec<Span> = (0..20).map(|i| Span { start: i * 10, end: Some(i * 10 + 10), frame: None }).collect();
        assert_eq!(read_ahead(&known, &VecDeque::new(), 0, 1, Some(10)), None);
        assert_eq!(read_ahead(&known, &VecDeque::new(), 150, 1, Some(10)), Some(200));
    }

    #[test]
    fn the_source_s_last_span_reads_nothing_past_it() {
        let known = vec![Span { start: 100, end: None, frame: None }];
        assert_eq!(read_ahead(&known, &VecDeque::new(), 150, 1, Some(10)), None);
    }
}
