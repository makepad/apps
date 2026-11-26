//! Admitted video: pictures a consumer uploads, under a fixed budget.
//!
//! A [`FrameGroup`] is made for exactly one load (its media and consumer
//! load are fixed at construction) and serves one open of it. Its load is
//! opened with [`AvReader::open_video_admitted`](super::AvReader::open_video_admitted)
//! and reads only uploads: decoded pictures never leave the video worker.
//!
//! Two kinds of allocation exist for it, each counted by the group:
//!
//! * DECODED frames, confined to the worker: at most
//!   [`VIDEO_DECODE_CREDITS`] (the last frame, the next one, and one being
//!   decoded). A credit is taken before the decoder is asked for a frame
//!   and stays with that frame's buffer until the buffer is dropped; the
//!   buffer's capacity (not its length) must fit the opened size. The
//!   platform decoder's own internal allocations are not counted here.
//! * UPLOAD buffers, one exact copy of a decoded frame: at most
//!   [`VIDEO_UPLOAD_PERMITS`]. The consumer takes a permit
//!   ([`FrameGroup::try_ticket`]) BEFORE it asks for a frame; the worker
//!   allocates the copy only under that permit, and the permit stays with
//!   the buffer through the result, the consumer (also while a texture
//!   holds the buffer, see [`UploadStub`]) and the way back
//!   ([`UploadReturn`]). It is released only after the buffer is dropped.
//!
//! The group's `keepalive` (its creator's budget lease) lives as long as
//! the last group handle, credit and permit: the budget is released only
//! after every buffer paid by it is gone. [`FrameGroup::settled`] says when
//! that point is reached as far as this reader is concerned: the worker let
//! the load go and no credit or permit is left.

use crate::identity::{ConsumerLoad, MediaIdentity, ReadTag};
use crate::limits::{MAX_VIDEO_HEIGHT, MAX_VIDEO_WIDTH};
use crate::payload::BufOrigin;
use makepad_platform::video_file::VideoColor;
use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Weak};

/// Decoded frames of one admitted load, at most: the last, the next, and
/// one being decoded.
pub const VIDEO_DECODE_CREDITS: u32 = 3;
/// Upload buffers of one admitted load, at most, wherever they are.
pub const VIDEO_UPLOAD_PERMITS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameGroupError {
    /// Each side must be even and `1..=MAX_VIDEO_WIDTH` / `MAX_VIDEO_HEIGHT`.
    Size,
}

struct FrameGroupInner {
    media: MediaIdentity,
    load: ConsumerLoad,
    max_width: u32,
    max_height: u32,
    /// `width << 32 | height` once the worker opened the load; 0 before.
    opened: AtomicU64,
    decodes: AtomicU32,
    uploads: AtomicU32,
    /// FREE, RESERVED (an owner is handing an open of it to a worker right
    /// now) or CLAIMED (a worker admitted an open of it: it serves no other).
    claim: AtomicU8,
    /// The worker let the load go (or no worker will ever hold it).
    finished: AtomicBool,
    keepalive: Box<dyn Any + Send + Sync>,
}

/// The budget of one admitted video load. Cloning shares the group.
#[derive(Clone)]
pub struct FrameGroup(Arc<FrameGroupInner>);

impl std::fmt::Debug for FrameGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameGroup")
            .field("load", &self.0.load)
            .field("max", &(self.0.max_width, self.0.max_height))
            .field("decodes", &self.decodes_outstanding())
            .field("uploads", &self.uploads_outstanding())
            .field("finished", &self.worker_finished())
            .finish()
    }
}

impl FrameGroup {
    /// A group for exactly `media` opened as `load`, whose pictures may be
    /// at most `max_width` x `max_height` (each even). `keepalive` is held
    /// until the last handle, credit and permit are gone: pass the budget
    /// lease that paid for them.
    pub fn new(
        media: MediaIdentity,
        load: ConsumerLoad,
        max_width: u32,
        max_height: u32,
        keepalive: Box<dyn Any + Send + Sync>,
    ) -> Result<Self, FrameGroupError> {
        let side = |value: u32, max: u32| value > 0 && value <= max && value % 2 == 0;
        if !side(max_width, MAX_VIDEO_WIDTH) || !side(max_height, MAX_VIDEO_HEIGHT) {
            return Err(FrameGroupError::Size);
        }
        Ok(Self(Arc::new(FrameGroupInner {
            media,
            load,
            max_width,
            max_height,
            opened: AtomicU64::new(0),
            decodes: AtomicU32::new(0),
            uploads: AtomicU32::new(0),
            claim: AtomicU8::new(FREE),
            finished: AtomicBool::new(false),
            keepalive,
        })))
    }

    pub fn media(&self) -> &MediaIdentity {
        &self.0.media
    }

    pub fn load(&self) -> ConsumerLoad {
        self.0.load
    }

    pub fn max_size(&self) -> (u32, u32) {
        (self.0.max_width, self.0.max_height)
    }

    /// Bytes of one picture at the largest size: an upload or decoded
    /// buffer is never larger.
    pub fn max_frame_bytes(&self) -> usize {
        nv12_bytes(self.0.max_width, self.0.max_height)
    }

    /// The keepalive, for its creator to prove it is its own lease.
    pub fn keepalive(&self) -> &(dyn Any + Send + Sync) {
        &*self.0.keepalive
    }

    /// Whether both handles are the same group (the same allocation).
    pub fn same(&self, other: &FrameGroup) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// The picture size the worker opened the load at: set before its
    /// `Opened` is sent, never changed after.
    pub fn opened_size(&self) -> Option<(u32, u32)> {
        match self.0.opened.load(Ordering::Acquire) {
            0 => None,
            packed => Some(((packed >> 32) as u32, packed as u32)),
        }
    }

    /// Decoded frames that exist (on the worker).
    pub fn decodes_outstanding(&self) -> u32 {
        self.0.decodes.load(Ordering::Acquire)
    }

    /// Upload permits that exist: tickets, buffers anywhere, stubs, returns.
    pub fn uploads_outstanding(&self) -> u32 {
        self.0.uploads.load(Ordering::Acquire)
    }

    /// Whether an open of this group was admitted (handed to a worker). It
    /// serves no other open, even after its load closed. A group being
    /// handed over right now by some reader is not claimed yet: an open of
    /// it elsewhere meanwhile is refused as busy, not as claimed.
    pub fn claimed(&self) -> bool {
        self.0.claim.load(Ordering::Acquire) == CLAIMED
    }

    /// Whether the worker let the load go (closed, failed to open, or the
    /// worker ended), or no worker ever will hold it.
    pub fn worker_finished(&self) -> bool {
        self.0.finished.load(Ordering::Acquire)
    }

    /// The load is gone from the reader and nothing it paid for exists:
    /// finished, no decoded frame, no upload permit.
    pub fn settled(&self) -> bool {
        self.worker_finished() && self.decodes_outstanding() == 0 && self.uploads_outstanding() == 0
    }

    /// Take one upload permit, BEFORE asking for a frame. Never waits;
    /// `None` when all [`VIDEO_UPLOAD_PERMITS`] exist.
    pub fn try_ticket(&self) -> Option<UploadTicket> {
        take(&self.0.uploads, VIDEO_UPLOAD_PERMITS)?;
        Some(UploadTicket(UploadPermit(self.0.clone())))
    }

    pub(crate) fn identity(&self) -> FrameGroupIdentity {
        FrameGroupIdentity(Arc::downgrade(&self.0))
    }

    /// Reserve the group for one open, atomically across every reader that
    /// holds a clone, BEFORE the open is queued.
    pub(crate) fn reserve(&self) -> Reservation {
        match self.0.claim.compare_exchange(FREE, RESERVED, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Reservation::Reserved,
            Err(CLAIMED) => Reservation::Claimed,
            Err(_) => Reservation::Busy,
        }
    }

    /// The reserved open was admitted: the group is claimed for good.
    pub(crate) fn commit(&self) {
        self.0.claim.store(CLAIMED, Ordering::Release);
    }

    /// The reserved open was not queued: the group is free again.
    pub(crate) fn release(&self) {
        let _ = self.0.claim.compare_exchange(RESERVED, FREE, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn mark_finished(&self) {
        self.0.finished.store(true, Ordering::Release);
    }

    pub(crate) fn set_opened(&self, width: u32, height: u32) {
        self.0.opened.store((width as u64) << 32 | height as u64, Ordering::Release);
    }

    pub(crate) fn try_decode_credit(&self) -> Option<DecodeCredit> {
        take(&self.0.decodes, VIDEO_DECODE_CREDITS)?;
        Some(DecodeCredit(self.0.clone()))
    }

    /// The worker's hold of the group for its load: marks it finished when
    /// dropped, after everything the load held on the worker.
    pub(crate) fn hold(self) -> GroupHold {
        GroupHold(self)
    }
}

const FREE: u8 = 0;
const RESERVED: u8 = 1;
const CLAIMED: u8 = 2;

/// What [`FrameGroup::reserve`] found (crate-private).
pub(crate) enum Reservation {
    /// Reserved by this call: commit or release it.
    Reserved,
    /// Another reader is handing an open of it over right now.
    Busy,
    /// An open of it was admitted before.
    Claimed,
}

/// Bytes of a tight NV12 picture of even size.
pub(crate) fn nv12_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 3 / 2
}

fn take(count: &AtomicU32, max: u32) -> Option<()> {
    count.try_update(Ordering::AcqRel, Ordering::Acquire, |taken| (taken < max).then_some(taken + 1)).ok().map(|_| ())
}

/// A group's identity without keeping its budget (crate-private).
pub(crate) struct FrameGroupIdentity(Weak<FrameGroupInner>);

impl FrameGroupIdentity {
    pub(crate) fn owns(&self, ticket: &UploadTicket) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&(ticket.0).0))
    }
}

/// The worker's hold of a group (crate-private).
pub(crate) struct GroupHold(FrameGroup);

impl GroupHold {
    pub(crate) fn group(&self) -> &FrameGroup {
        &self.0
    }
}

impl Drop for GroupHold {
    fn drop(&mut self) {
        self.0.mark_finished();
    }
}

/// The right of one decoded frame buffer to exist (crate-private).
pub(crate) struct DecodeCredit(Arc<FrameGroupInner>);

impl Drop for DecodeCredit {
    fn drop(&mut self) {
        self.0.decodes.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The right of one upload buffer to exist.
struct UploadPermit(Arc<FrameGroupInner>);

impl Drop for UploadPermit {
    fn drop(&mut self) {
        self.0.uploads.fetch_sub(1, Ordering::AcqRel);
    }
}

/// An upload permit taken before a read: the read's buffer is paid by it.
/// Not clonable; dropping it releases the permit.
pub struct UploadTicket(UploadPermit);

impl UploadTicket {
    pub fn is_of(&self, group: &FrameGroup) -> bool {
        Arc::ptr_eq(&(self.0).0, &group.0)
    }
}

/// What an upload shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadInfo {
    /// Timeline tick of the frame's own presentation time (not the time
    /// asked for).
    pub at: i64,
    /// The frame's container timestamp (100 ns).
    pub local_pts_100ns: i64,
    pub width: u32,
    pub height: u32,
    /// The colour of the frame's bytes (what the decoder delivered).
    pub color: VideoColor,
}

/// One admitted picture: a tight NV12 copy (`width * height * 3 / 2`
/// bytes, allocated at exactly that capacity) under its group's permit.
/// Only the reader makes one.
pub struct UploadFrame {
    // Declared first: dropped before the permit that paid for it.
    nv12: Vec<u8>,
    origin: BufOrigin,
    tag: ReadTag,
    request: u64,
    requested_at: u64,
    info: UploadInfo,
    permit: UploadPermit,
}

impl UploadFrame {
    pub(crate) fn new(nv12: Vec<u8>, origin: BufOrigin, tag: ReadTag, request: u64, requested_at: u64, info: UploadInfo, ticket: UploadTicket) -> Self {
        Self { nv12, origin, tag, request, requested_at, info, permit: ticket.0 }
    }

    pub fn media(&self) -> &MediaIdentity {
        &self.permit.0.media
    }

    pub fn load(&self) -> ConsumerLoad {
        self.permit.0.load
    }

    pub fn tag(&self) -> ReadTag {
        self.tag
    }

    /// The request ordinal it answers.
    pub fn request(&self) -> u64 {
        self.request
    }

    /// The timeline tick the request asked for.
    pub fn requested_at(&self) -> u64 {
        self.requested_at
    }

    pub fn info(&self) -> UploadInfo {
        self.info
    }

    pub fn is_of(&self, group: &FrameGroup) -> bool {
        Arc::ptr_eq(&self.permit.0, &group.0)
    }

    pub fn nv12(&self) -> &[u8] {
        &self.nv12
    }

    /// The bytes the buffer holds allocated.
    pub fn capacity_bytes(&self) -> usize {
        self.nv12.capacity()
    }

    /// Give it back whole, unshown.
    pub fn into_return(self) -> UploadReturn {
        let Self { nv12, origin, permit, .. } = self;
        UploadReturn { nv12, origin, permit }
    }

    /// Hand the buffer to a texture. The permit stays in the stub; the
    /// buffer comes back only through [`UploadStub::reunite`]. A library
    /// handoff (the frame renderer), not for applications.
    #[doc(hidden)]
    pub fn detach(self) -> (Vec<u8>, UploadStub) {
        let Self { nv12, origin, info, permit, .. } = self;
        let stub = UploadStub { origin, info, ptr: nv12.as_ptr() as usize, capacity: nv12.capacity(), len: nv12.len(), permit: Some(permit) };
        (nv12, stub)
    }
}

/// The permit of an upload whose buffer a texture holds.
///
/// Only the exact buffer detached from it (same allocation address,
/// capacity and length) is taken back by [`Self::reunite`]; anything else
/// is handed back with the stub. A stub dropped without being reunited
/// never releases its permit: the permit, and with it the group's budget
/// lease, is kept for the life of the process, so a buffer that may still
/// exist is never uncharged. That is a leak by design, and only misuse
/// causes it: the legitimate path always reunites. A library handoff (the
/// frame renderer), not for applications.
#[doc(hidden)]
pub struct UploadStub {
    origin: BufOrigin,
    info: UploadInfo,
    ptr: usize,
    capacity: usize,
    len: usize,
    permit: Option<UploadPermit>,
}

impl UploadStub {
    pub fn info(&self) -> UploadInfo {
        self.info
    }

    pub fn is_of(&self, group: &FrameGroup) -> bool {
        self.permit.as_ref().is_some_and(|permit| Arc::ptr_eq(&permit.0, &group.0))
    }

    /// The buffer back from its texture, with its permit, for return.
    pub fn reunite(mut self, nv12: Vec<u8>) -> Result<UploadReturn, (UploadStub, Vec<u8>)> {
        if nv12.as_ptr() as usize != self.ptr || nv12.capacity() != self.capacity || nv12.len() != self.len {
            return Err((self, nv12));
        }
        let permit = self.permit.take().expect("a stub holds its permit until reunited");
        Ok(UploadReturn { nv12, origin: self.origin, permit })
    }
}

impl Drop for UploadStub {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            // Never reunited: its buffer may still exist, so its permit is
            // kept (see the type docs).
            std::mem::forget(permit);
        }
    }
}

/// An upload buffer with its permit, on its way back to the worker that
/// made it ([`AvReader::return_upload`](super::AvReader::return_upload)).
/// Dropped anywhere else, the buffer goes first, then the permit.
pub struct UploadReturn {
    nv12: Vec<u8>,
    origin: BufOrigin,
    permit: UploadPermit,
}

impl UploadReturn {
    pub(crate) fn origin(&self) -> BufOrigin {
        self.origin
    }

    pub fn is_of(&self, group: &FrameGroup) -> bool {
        Arc::ptr_eq(&self.permit.0, &group.0)
    }

    pub fn capacity_bytes(&self) -> usize {
        self.nv12.capacity()
    }
}

/// Why [`AvReader::read_video_upload`](super::AvReader::read_video_upload)
/// refused, with the ticket handed back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadRefusal {
    /// The video queue is full: the exact request may be retried.
    QueueFull,
    Stopped,
    UnknownLoad,
    /// Not an admitted video load.
    NotAdmitted,
    /// Its `Opened` was not taken yet.
    NotOpened,
    /// Being closed, closed, or its worker ended.
    Closing,
    /// The ticket is of another group.
    ForeignTicket,
    /// Not the load's current seek serial, or not a newer request ordinal
    /// within it.
    StaleRequest,
    /// A request of the current serial is still unanswered.
    Outstanding,
}
