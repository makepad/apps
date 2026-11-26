//! Immutable PCM payloads and their admission.
//!
//! A [`PcmPage`] is an exact range of stereo i16 frames of a source
//! timeline. Its buffer is ONE fixed allocation of [`PAGE_FRAMES`] frames
//! ([`PAGE_BYTES`] bytes), whatever its length, so memory is counted by
//! allocation, never by `len`.
//!
//! A [`PageGroup`] bounds how many such buffers one paged consumer may hold
//! at once. A buffer of the group is allocated only after a [`PagePermit`]
//! was taken, and the permit stays attached to that physical buffer through
//! every state (the reader's page being filled, the immutable `Arc` payload,
//! the consumer, commands and returns, the reader's per-group pool). The
//! permit is released only together with the allocation.
//!
//! The group holds an opaque `keepalive` (the budget lease its creator
//! reserved). It lives as long as the last group handle: every permit
//! (hence every page and pooled buffer of the group) and every clone of the
//! group itself. So the lease is released only after all of them are gone.
//!
//! Pages from a plain (unpaged) reader load carry no permit:
//! [`PcmPage::is_admitted`] is `false` and [`PcmPage::is_admitted_by`] is
//! `false` for every group. Nothing outside this crate can construct a page,
//! and group identity is the identity of the shared allocation, never a
//! number that could be forged.

use crate::identity::ReadTag;
use crate::limits::MAX_PAGE_FRAMES;
use crate::timeline::SampleRange;
use std::any::Any;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
#[cfg(feature = "reader")]
use std::sync::Weak;

/// Frames in every page buffer.
pub const PAGE_FRAMES: usize = MAX_PAGE_FRAMES;
/// Bytes of every page buffer: its actual allocation.
pub const PAGE_BYTES: usize = PAGE_FRAMES * std::mem::size_of::<[i16; 2]>();
/// Largest group: 1024 buffers, 64 MiB.
pub const MAX_GROUP_PAGES: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageGroupError {
    /// The capacity must be `1..=MAX_GROUP_PAGES`.
    Capacity,
}

struct GroupInner {
    capacity: u32,
    /// Permits outstanding: buffers of this group that exist.
    taken: AtomicU32,
    /// The creator's budget lease; dropped with the last group handle.
    keepalive: Box<dyn Any + Send + Sync>,
}

/// The allocation budget of one paged consumer. Cloning shares the group.
#[derive(Clone)]
pub struct PageGroup(Arc<GroupInner>);

impl std::fmt::Debug for PageGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageGroup").field("capacity", &self.0.capacity).field("taken", &self.0.taken.load(Ordering::Relaxed)).finish()
    }
}

impl PageGroup {
    /// A group of at most `capacity_pages` buffers. `keepalive` is held until
    /// the last buffer, pooled buffer and group handle are gone: pass the
    /// budget lease that paid for `capacity_pages * PAGE_BYTES`.
    pub fn new(capacity_pages: u32, keepalive: Box<dyn Any + Send + Sync>) -> Result<Self, PageGroupError> {
        if capacity_pages == 0 || capacity_pages > MAX_GROUP_PAGES {
            return Err(PageGroupError::Capacity);
        }
        Ok(Self(Arc::new(GroupInner { capacity: capacity_pages, taken: AtomicU32::new(0), keepalive })))
    }

    pub fn capacity(&self) -> u32 {
        self.0.capacity
    }

    /// Buffers of this group that currently exist.
    pub fn outstanding(&self) -> u32 {
        self.0.taken.load(Ordering::Acquire)
    }

    /// Bytes this group may hold at most: its hard memory cap.
    pub fn capacity_bytes(&self) -> usize {
        self.0.capacity as usize * PAGE_BYTES
    }

    /// Whether both handles are the same group (the same allocation).
    pub fn same(&self, other: &PageGroup) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// The keepalive, for its creator to prove it is its own lease.
    pub fn keepalive(&self) -> &(dyn Any + Send + Sync) {
        &*self.0.keepalive
    }

    /// The group's identity, for a reader to tell whether it already serves
    /// a load. Holds no permit and keeps no keepalive: only the allocation
    /// of the group's record lives on, so the identity is never mistaken for
    /// a later group's.
    #[cfg(feature = "reader")]
    pub(crate) fn identity(&self) -> GroupIdentity {
        GroupIdentity(Arc::downgrade(&self.0))
    }

    /// Take one permit, if the group has room. Never waits.
    #[cfg(feature = "reader")]
    pub(crate) fn try_permit(&self) -> Option<PagePermit> {
        let inner = &self.0;
        inner
            .taken
            .try_update(Ordering::AcqRel, Ordering::Acquire, |taken| (taken < inner.capacity).then_some(taken + 1))
            .ok()?;
        Some(PagePermit(self.0.clone()))
    }
}

/// A group's identity without its budget (crate-private).
#[cfg(feature = "reader")]
pub(crate) struct GroupIdentity(Weak<GroupInner>);

#[cfg(feature = "reader")]
impl GroupIdentity {
    pub(crate) fn is(&self, group: &PageGroup) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&group.0))
    }
}

/// Which reader worker allocated a buffer (crate-private).
#[cfg(feature = "reader")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    Audio,
    Video,
}

/// Where a buffer was allocated: the reader and its worker. Fixed at the
/// allocation and never changed, so a buffer returned late (after its load
/// closed, or its group was reopened) still goes back to the worker that
/// made it, and to no other.
#[cfg(feature = "reader")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufOrigin {
    pub reader: u64,
    pub lane: Lane,
}

/// The right of one physical page buffer to exist. Not clonable; released
/// only when the buffer it is attached to is dropped.
pub struct PagePermit(Arc<GroupInner>);

impl Drop for PagePermit {
    fn drop(&mut self) {
        self.0.taken.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PagePermit {
    fn is_of(&self, group: &PageGroup) -> bool {
        Arc::ptr_eq(&self.0, &group.0)
    }
}

/// A page buffer being filled (crate-private): one fixed allocation, its
/// length, the permit that paid for it (`None` for unpaged loads) and the
/// worker that allocated it.
pub(crate) struct PageBuf {
    buf: Box<[[i16; 2]]>,
    len: usize,
    permit: Option<PagePermit>,
    #[cfg(feature = "reader")]
    origin: BufOrigin,
}

/// Producing a buffer exists only with the reader; the read-only side
/// (admission checks) is always there.
#[cfg(feature = "reader")]
impl PageBuf {
    /// A fresh buffer. For a paged load the permit must have been taken
    /// BEFORE this allocation.
    pub(crate) fn allocate(permit: Option<PagePermit>, origin: BufOrigin) -> Self {
        // A heap allocation of exactly PAGE_FRAMES frames; no stack temporary.
        let buf = vec![[0i16; 2]; PAGE_FRAMES].into_boxed_slice();
        Self { buf, len: 0, permit, origin }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn room(&self) -> usize {
        PAGE_FRAMES - self.len
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    /// Append as much of `frames` as fits; returns how many were taken.
    pub(crate) fn extend(&mut self, frames: &[[i16; 2]]) -> usize {
        let take = self.room().min(frames.len());
        self.buf[self.len..self.len + take].copy_from_slice(&frames[..take]);
        self.len += take;
        take
    }
}

impl PageBuf {
    pub(crate) fn is_admitted(&self) -> bool {
        self.permit.is_some()
    }

    pub(crate) fn is_of(&self, group: &PageGroup) -> bool {
        self.permit.as_ref().is_some_and(|permit| permit.is_of(group))
    }
}

/// An exact range of stereo frames of the source timeline. Immutable once
/// built; only this crate builds one.
///
/// Its identity (tag, start, rate) is private: a uniquely held page cannot
/// be relabelled (for instance as a newer seek serial) through `Arc::get_mut`.
pub struct PcmPage {
    tag: ReadTag,
    start: u64,
    sample_rate: u32,
    buf: PageBuf,
}

#[cfg(feature = "reader")]
impl PcmPage {
    pub(crate) fn from_buf(tag: ReadTag, start: u64, sample_rate: u32, buf: PageBuf) -> Self {
        Self { tag, start, sample_rate, buf }
    }

    pub(crate) fn into_buf(self) -> PageBuf {
        self.buf
    }

    pub(crate) fn tag_ref(&self) -> &ReadTag {
        &self.tag
    }

    /// The worker that allocated this page's buffer.
    pub(crate) fn origin(&self) -> BufOrigin {
        self.buf.origin
    }
}

impl PcmPage {
    /// The exact read (media, load, seek serial) this page answers.
    pub fn tag(&self) -> ReadTag {
        self.tag
    }

    /// Timeline frame of the first frame.
    pub fn start(&self) -> u64 {
        self.start
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn frames(&self) -> &[[i16; 2]] {
        &self.buf.buf[..self.buf.len]
    }

    pub fn range(&self) -> SampleRange {
        SampleRange { start: self.start, end: self.start + self.buf.len as u64 }
    }

    /// The bytes this page actually holds allocated (always [`PAGE_BYTES`]).
    pub fn capacity_bytes(&self) -> usize {
        self.buf.buf.len() * std::mem::size_of::<[i16; 2]>()
    }

    /// Whether this page was admitted under some group's permit.
    pub fn is_admitted(&self) -> bool {
        self.buf.is_admitted()
    }

    /// Whether this page's buffer was paid for by exactly `group`.
    pub fn is_admitted_by(&self, group: &PageGroup) -> bool {
        self.buf.is_of(group)
    }
}
