//! What file video costs, in one budget: decoder slots and the frame memory
//! of every source that exists, the one playing and the one being brought up
//! beside it during a replacement alike.
//!
//! A [`VideoLease`] taken from the one [`VideoBudget`] lives exactly as long
//! as its source is referenced: by the decoder on the worker, by every frame
//! on its way to the drawing thread, and by the unit whose texture shows it.
//! It is released only when the last of those is gone, so a replaced file's
//! memory is not counted free while its frames are still in flight. What the
//! platform's codec holds internally (the reader, hardware surfaces) is
//! outside this quota and is said to be.
//!
//! The VJ's live frame owner and the edit renderer draw against the same
//! budget; a reader source ([`ReaderVideoReservation`], feature `reader`) is
//! reserved here and opened on the [`AvReader`](crate::AvReader).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(feature = "reader")]
use crate::{ConsumerLoad, FrameGroup, MediaIdentity, TimelineSnapshot, UploadFrame};
#[cfg(feature = "reader")]
use std::sync::atomic::AtomicBool;

/// Decoders open at once, across every session.
pub const MAX_VIDEO_SOURCES: usize = 8;
/// The longer side of a picture, landscape or portrait (UHD 4K). A file
/// larger is refused at open.
pub const MAX_VIDEO_LONG_SIDE: u32 = 3840;
/// The shorter side of a picture.
pub const MAX_VIDEO_SHORT_SIDE: u32 = 2160;

/// Whether a picture of `width` x `height` can be played: even, non-zero
/// sides within [`MAX_VIDEO_LONG_SIDE`] x [`MAX_VIDEO_SHORT_SIDE`] either way round.
pub fn video_size_allowed(width: u32, height: u32) -> bool {
    let even = |side: u32| side > 0 && side % 2 == 0;
    even(width) && even(height) && width.max(height) <= MAX_VIDEO_LONG_SIDE && width.min(height) <= MAX_VIDEO_SHORT_SIDE
}
/// BGRA frame buffers per source: one free on the worker, one on its way,
/// one inside the texture. Never more exist for a source.
pub const FRAME_RING: usize = 3;
/// The application checks a file against this before it submits it.
pub const MAX_VIDEO_FILE_BYTES: u64 = 2 << 30;
/// Bytes per pixel a source is charged: the ring of BGRA frames, plus the
/// decoder's one NV12 frame in flight (1.5 bytes, counted as 2).
const BYTES_PER_PIXEL: usize = 4 * FRAME_RING + 2;

/// Frame memory one source of `width` x `height` is charged.
pub fn source_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * BYTES_PER_PIXEL
}

/// What a source is charged before its size is known: the largest allowed.
pub fn worst_case_source_bytes() -> usize {
    source_bytes(MAX_VIDEO_LONG_SIDE, MAX_VIDEO_SHORT_SIDE)
}

/// Bytes per pixel a READER source is charged, with `P = width * height`
/// and `N = 3P / 2` (one NV12 picture): three decoded frames on the reader
/// worker (3N), two upload buffers wherever they are (worker, results,
/// texture, returns: 2N), the packed NV12 texture on the GPU (N) and the
/// BGRA conversion target (4P): `6N + 4P = 13P`. Not the legacy formula.
pub const READER_BYTES_PER_PIXEL: usize = 13;

/// Frame memory one reader source of `width` x `height` is charged.
pub fn reader_source_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * READER_BYTES_PER_PIXEL
}

/// The aggregate budget of file video: decoder slots and frame memory.
pub struct VideoBudget {
    max_sources: usize,
    max_bytes: usize,
    sources: AtomicUsize,
    bytes: AtomicUsize,
}

impl VideoBudget {
    pub fn new(max_sources: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self { max_sources: max_sources.max(1), max_bytes, sources: AtomicUsize::new(0), bytes: AtomicUsize::new(0) })
    }

    /// Take one decoder slot and the worst-case frame memory. The lease is
    /// shrunk to the file's real size once it is open. `Err` names what is
    /// short; the caller keeps its intent and asks again later.
    pub fn reserve(self: &Arc<Self>) -> Result<VideoLease, String> {
        self.reserve_source(worst_case_source_bytes())
    }

    #[cfg(feature = "reader")]
    /// Take one decoder slot and the worst case of a READER source
    /// ([`reader_source_bytes`] at the largest size), for exactly `media`
    /// opened as `load`. Open it with the reservation's
    /// [`group`](ReaderVideoReservation::group) and, once the reader says
    /// `Opened`, [`finish`](ReaderVideoReservation::finish) it.
    pub fn reserve_reader(self: &Arc<Self>, media: MediaIdentity, load: ConsumerLoad) -> Result<ReaderVideoReservation, String> {
        let lease = Arc::new(self.reserve_source(reader_source_bytes(MAX_VIDEO_LONG_SIDE, MAX_VIDEO_SHORT_SIDE))?);
        let keep = ReaderKeep(lease.clone());
        // Either orientation opens; `finish` holds the size to the limits.
        let group = FrameGroup::new(media, load, MAX_VIDEO_LONG_SIDE, MAX_VIDEO_LONG_SIDE, Box::new(keep))
            .map_err(|error| format!("the reader frame group could not be made: {error:?}"))?;
        Ok(ReaderVideoReservation { lease, group })
    }

    fn reserve_source(self: &Arc<Self>, bytes: usize) -> Result<VideoLease, String> {
        let mut sources = self.sources.load(Ordering::Acquire);
        loop {
            if sources >= self.max_sources {
                return Err(format!("{} video sources are open; the limit is {}", sources, self.max_sources));
            }
            match self.sources.compare_exchange(sources, sources + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(now) => sources = now,
            }
        }
        let mut used = self.bytes.load(Ordering::Acquire);
        loop {
            if used.checked_add(bytes).is_none_or(|total| total > self.max_bytes) {
                self.sources.fetch_sub(1, Ordering::AcqRel);
                return Err(format!(
                    "{} MiB of video frame memory is in use and a source needs {} MiB more; the limit is {} MiB",
                    used >> 20,
                    bytes >> 20,
                    self.max_bytes >> 20
                ));
            }
            match self.bytes.compare_exchange(used, used + bytes, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(now) => used = now,
            }
        }
        Ok(VideoLease { budget: self.clone(), bytes: AtomicUsize::new(bytes) })
    }

    /// Charge frame memory held outside every source's ring (the GPU copy
    /// of a decoded frame) to the same hard quota. Takes no decoder slot
    /// and never waits; `Err` names what is short.
    pub fn reserve_frame_bytes(self: &Arc<Self>, bytes: usize) -> Result<VideoFrameLease, String> {
        let mut used = self.bytes.load(Ordering::Acquire);
        loop {
            let Some(total) = used.checked_add(bytes).filter(|total| *total <= self.max_bytes) else {
                return Err(format!(
                    "{} MiB of video frame memory is in use and a frame needs {} MiB more; the limit is {} MiB",
                    used >> 20,
                    bytes >> 20,
                    self.max_bytes >> 20
                ));
            };
            match self.bytes.compare_exchange(used, total, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(now) => used = now,
            }
        }
        Ok(VideoFrameLease { budget: self.clone(), bytes })
    }

    pub fn used_sources(&self) -> usize {
        self.sources.load(Ordering::Relaxed)
    }

    pub fn used_bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn max_sources(&self) -> usize {
        self.max_sources
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

/// One decoder slot and its frame memory, held while the source exists.
pub struct VideoLease {
    budget: Arc<VideoBudget>,
    bytes: AtomicUsize,
}

impl VideoLease {
    /// The file is open at `width` x `height`: give back what the worst
    /// case charged beyond it. Never grows.
    pub fn shrink_to(&self, width: u32, height: u32) {
        self.shrink_to_bytes(source_bytes(width, height));
    }

    fn shrink_to_bytes(&self, real: usize) {
        let held = self.bytes.load(Ordering::Acquire);
        if real < held {
            self.bytes.store(real, Ordering::Release);
            self.budget.bytes.fetch_sub(held - real, Ordering::AcqRel);
        }
    }

    /// Whether this lease was taken from `budget`.
    pub fn from(&self, budget: &Arc<VideoBudget>) -> bool {
        Arc::ptr_eq(&self.budget, budget)
    }
}

impl Drop for VideoLease {
    fn drop(&mut self) {
        self.budget.bytes.fetch_sub(self.bytes.load(Ordering::Acquire), Ordering::AcqRel);
        self.budget.sources.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Frame memory charged apart from any source's ring, held while the
/// memory exists. It holds no decoder slot.
pub struct VideoFrameLease {
    budget: Arc<VideoBudget>,
    bytes: usize,
}

impl VideoFrameLease {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for VideoFrameLease {
    fn drop(&mut self) {
        self.budget.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(feature = "reader")]
/// The keepalive of a reader source's frame group: the lease, so the
/// budget is released only after the last buffer of the group is gone. Only
/// this crate makes one, which is how a group proves it is ours.
struct ReaderKeep(Arc<VideoLease>);

#[cfg(feature = "reader")]
/// A reader source reserved against the video budget, not opened yet.
/// Dropping it releases the reservation (once the group's last handle and
/// buffer are gone).
pub struct ReaderVideoReservation {
    lease: Arc<VideoLease>,
    group: FrameGroup,
}

#[cfg(feature = "reader")]
/// Why [`ReaderVideoReservation::finish`] refused, with the reservation back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReaderSourceError {
    /// The load or the media is not the group's.
    WrongLoad,
    WrongMedia,
    /// The snapshot itself is malformed (`TimelineSnapshot::validate`: time
    /// base, frontier, streams, state); the owner clock could never start
    /// from it.
    InvalidTimeline,
    /// The snapshot has no picture.
    NoVideo,
    /// A side is zero or odd, or the picture is larger than
    /// [`MAX_VIDEO_LONG_SIDE`] x [`MAX_VIDEO_SHORT_SIDE`] ([`video_size_allowed`]).
    Size { width: u32, height: u32 },
    /// The reader has not opened the group's load at exactly this size, or
    /// has let it go already.
    NotOpened,
    Finished,
}

#[cfg(feature = "reader")]
impl ReaderVideoReservation {
    /// The group to open the load with (`AvReader::open_video_admitted`).
    pub fn group(&self) -> FrameGroup {
        self.group.clone()
    }

    /// The source, once the reader answered `Opened { load, snapshot }` for
    /// exactly this group's load: the snapshot's picture size must be the
    /// one the reader opened, even, non-zero and within
    /// [`video_size_allowed`]. The charge shrinks to
    /// [`reader_source_bytes`] of that size.
    pub fn finish(self, load: ConsumerLoad, snapshot: &TimelineSnapshot) -> Result<Arc<ReaderVideoSource>, (ReaderSourceError, Self)> {
        let group = &self.group;
        if group.load() != load {
            return Err((ReaderSourceError::WrongLoad, self));
        }
        if snapshot.media != *group.media() {
            return Err((ReaderSourceError::WrongMedia, self));
        }
        // It becomes the source's canonical timeline: a malformed one is
        // refused here, before any charge changes, not accepted as a source
        // whose clock can never start.
        if snapshot.validate().is_err() {
            return Err((ReaderSourceError::InvalidTimeline, self));
        }
        let Some(caps) = snapshot.video else { return Err((ReaderSourceError::NoVideo, self)) };
        if !video_size_allowed(caps.width, caps.height) {
            return Err((ReaderSourceError::Size { width: caps.width, height: caps.height }, self));
        }
        if group.opened_size() != Some((caps.width, caps.height)) || !group.claimed() {
            return Err((ReaderSourceError::NotOpened, self));
        }
        if group.worker_finished() {
            return Err((ReaderSourceError::Finished, self));
        }
        // The group's keepalive is this very lease: made here, never forged.
        let ours = group.keepalive().downcast_ref::<ReaderKeep>().is_some_and(|keep| Arc::ptr_eq(&keep.0, &self.lease));
        if !ours {
            return Err((ReaderSourceError::NotOpened, self));
        }
        self.lease.shrink_to_bytes(reader_source_bytes(caps.width, caps.height));
        let Self { lease, group } = self;
        Ok(Arc::new(ReaderVideoSource {
            media: group.media().clone(),
            load,
            width: caps.width,
            height: caps.height,
            timeline: Arc::new(snapshot.clone()),
            lease,
            group,
            claimed: AtomicBool::new(false),
        }))
    }
}

#[cfg(feature = "reader")]
/// An opened reader load prepared for a `FileVideo`, with its lease and its
/// frame group. It attaches once (like a file source of the live frame owner loads once).
pub struct ReaderVideoSource {
    media: MediaIdentity,
    load: ConsumerLoad,
    width: u32,
    height: u32,
    /// The canonical timeline publication the source was finished with:
    /// time base, caps and state its owner clock starts from. Metadata only.
    timeline: Arc<TimelineSnapshot>,
    lease: Arc<VideoLease>,
    group: FrameGroup,
    claimed: AtomicBool,
}

#[cfg(feature = "reader")]
impl ReaderVideoSource {
    pub fn media(&self) -> &MediaIdentity {
        &self.media
    }

    pub fn timeline(&self) -> &Arc<TimelineSnapshot> {
        &self.timeline
    }

    pub fn load(&self) -> ConsumerLoad {
        self.load
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn group(&self) -> &FrameGroup {
        &self.group
    }

    pub fn leased_from(&self, budget: &Arc<VideoBudget>) -> bool {
        self.lease.from(budget)
    }

    /// Whether an attach of this source was admitted already.
    pub fn claimed(&self) -> bool {
        self.claimed.load(Ordering::Acquire)
    }

    /// Take the one attach this source pays for. `false` when it was taken.
    pub fn claim(&self) -> bool {
        self.claimed.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    /// Whether a frame belongs to exactly this source and fits it.
    pub fn owns(&self, frame: &UploadFrame) -> bool {
        let info = frame.info();
        frame.is_of(&self.group) && *frame.media() == self.media && frame.load() == self.load && (info.width, info.height) == (self.width, self.height)
    }

    /// Nothing this source paid for exists any more, as far as the reader
    /// and this crate can tell: its load is finished on the reader and no
    /// decoded frame or upload buffer of it is left.
    pub fn settled(&self) -> bool {
        self.group.settled()
    }
}

#[cfg(feature = "reader")]
impl std::fmt::Debug for ReaderVideoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderVideoSource").field("load", &self.load).field("size", &(self.width, self.height)).finish()
    }
}
