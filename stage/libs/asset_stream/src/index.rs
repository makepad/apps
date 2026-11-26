//! The segment index of a growing timeline: an append-only tree of
//! immutable pages on disk.
//!
//! Leaves list at most [`MAX_LEAF_SEGMENTS`] segments in timeline order;
//! interior pages list at most [`MAX_FANOUT`] children with the range each
//! covers; the tree has at most [`MAX_INDEX_LEVELS`] levels and a full tree
//! refuses the next append with [`IndexError::Full`]. An append rewrites
//! only the right spine (at most `MAX_INDEX_LEVELS` small pages) and never
//! touches a page an older snapshot reads: every page is named by the digest
//! of its bytes, written to a fresh temporary, synced and linked into place
//! without ever replacing an existing file. A lookup reads at
//! most one page per level. Nothing here loads the history.
//!
//! This is a Stage-private, self-versioned codec (`MPSTIDX`, version 1).
//! Page bytes and digests are exposed so a content-addressed publisher can
//! later retain them as explicit dependencies.

use crate::limits::*;
use crate::timeline::{GapReason, SampleRange};
use makepad_asset_data::{BlobId, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"MPSTIDX\0";
const VERSION: u16 = 1;
const KIND_LEAF: u8 = 1;
const KIND_INTERIOR: u8 = 2;

static TEMP_TICKET: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexError {
    Io(String),
    Malformed(&'static str),
    Version(u16),
    OverLimit(&'static str),
    DigestMismatch,
    RefMismatch(&'static str),
    NotContiguous { expected: u64, found: u64 },
    Full,
    BadFileName,
    /// A different file already holds this immutable name.
    Clobber,
    /// A later index does not extend an earlier one.
    NotAnExtension(&'static str),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

fn io(error: std::io::Error) -> IndexError {
    IndexError::Io(error.to_string())
}

/// A file name inside a store directory: no separators, no leading dot,
/// bounded, so an index can never point outside its store.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileName(String);

impl FileName {
    pub fn new(name: impl Into<String>) -> Result<Self, IndexError> {
        let name = name.into();
        let ok = !name.is_empty()
            && name.len() <= MAX_FILE_NAME_BYTES
            && !name.starts_with('.')
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
        if ok { Ok(Self(name)) } else { Err(IndexError::BadFileName) }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An exact-PCM sidecar: canonical 44-byte-header WAV, 16-bit stereo at the
/// timeline rate, exactly `range.len()` frames.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SidecarRef {
    pub file: FileName,
    pub digest: BlobId,
    pub bytes: u64,
}

/// A finalized, keyframe-only MP4 segment. `pts_base_100ns` is the local
/// presentation time that corresponds to the segment's `range.start`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VideoSegmentRef {
    pub file: FileName,
    pub digest: BlobId,
    pub bytes: u64,
    pub pts_base_100ns: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    Media { pcm: Option<SidecarRef>, video: Option<VideoSegmentRef> },
    Gap(GapReason),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SegmentEntry {
    pub range: SampleRange,
    pub kind: SegmentKind,
}

impl SegmentEntry {
    pub fn validate(&self) -> Result<(), IndexError> {
        if self.range.is_empty() || self.range.len() > MAX_SEGMENT_FRAMES {
            return Err(IndexError::OverLimit("segment frames"));
        }
        match &self.kind {
            SegmentKind::Media { pcm: None, video: None } => return Err(IndexError::Malformed("media segment without streams")),
            SegmentKind::Gap(GapReason::NotCommitted) => return Err(IndexError::Malformed("uncommitted gap")),
            SegmentKind::Gap(_) => {}
            SegmentKind::Media { pcm, video } => {
                if let Some(pcm) = pcm {
                    if !pcm.file.as_str().ends_with(".wav") {
                        return Err(IndexError::BadFileName);
                    }
                    if pcm.bytes != crate::sidecar::WAV_HEADER_BYTES + self.range.len() * 4 {
                        return Err(IndexError::RefMismatch("sidecar length"));
                    }
                }
                if let Some(video) = video {
                    if !video.file.as_str().ends_with(".mp4") {
                        return Err(IndexError::BadFileName);
                    }
                    if video.bytes == 0 || video.bytes > MAX_SEGMENT_FILE_BYTES {
                        return Err(IndexError::OverLimit("video segment bytes"));
                    }
                }
            }
        }
        Ok(())
    }
}

/// A reference to one immutable page: its digest, byte length, the range
/// and height of the subtree, and the number of segments under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageRef {
    pub digest: BlobId,
    pub bytes: u32,
    pub range: SampleRange,
    /// `0` for a leaf.
    pub height: u8,
    pub segments: u64,
}

/// Most segments a subtree of `height` can hold.
pub fn capacity_at(height: u8) -> u64 {
    let mut capacity = MAX_LEAF_SEGMENTS as u64;
    for _ in 0..height {
        capacity = capacity.saturating_mul(MAX_FANOUT as u64);
    }
    capacity
}

impl PageRef {
    /// Bounds a reference must satisfy before its page is read.
    pub fn validate(&self) -> Result<(), IndexError> {
        if self.bytes == 0 || self.bytes as usize > MAX_PAGE_BYTES {
            return Err(IndexError::OverLimit("page bytes"));
        }
        if self.height as usize >= MAX_INDEX_LEVELS {
            return Err(IndexError::OverLimit("height"));
        }
        if self.range.is_empty() {
            return Err(IndexError::Malformed("empty page range"));
        }
        if self.segments == 0 || self.segments > capacity_at(self.height) {
            return Err(IndexError::OverLimit("segment count"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexPage {
    Leaf(Vec<SegmentEntry>),
    Interior { height: u8, children: Vec<PageRef> },
}

impl IndexPage {
    fn height(&self) -> u8 {
        match self {
            Self::Leaf(_) => 0,
            Self::Interior { height, .. } => *height,
        }
    }

    fn range(&self) -> Option<SampleRange> {
        match self {
            Self::Leaf(entries) => Some(SampleRange { start: entries.first()?.range.start, end: entries.last()?.range.end }),
            Self::Interior { children, .. } => Some(SampleRange { start: children.first()?.range.start, end: children.last()?.range.end }),
        }
    }

    fn segments(&self) -> Result<u64, IndexError> {
        match self {
            Self::Leaf(entries) => Ok(entries.len() as u64),
            Self::Interior { children, .. } => children
                .iter()
                .try_fold(0u64, |sum, child| sum.checked_add(child.segments))
                .ok_or(IndexError::OverLimit("segment count")),
        }
    }

    /// Structure rules every page obeys: bounded, non-empty, contiguous,
    /// children one level down.
    pub fn validate(&self) -> Result<(), IndexError> {
        let mut end = None;
        let mut check = |range: SampleRange| {
            if let Some(end) = end {
                if range.start != end {
                    return Err(IndexError::NotContiguous { expected: end, found: range.start });
                }
            }
            end = Some(range.end);
            Ok(())
        };
        match self {
            Self::Leaf(entries) => {
                if entries.is_empty() || entries.len() > MAX_LEAF_SEGMENTS {
                    return Err(IndexError::OverLimit("leaf entries"));
                }
                for entry in entries {
                    entry.validate()?;
                    check(entry.range)?;
                }
            }
            Self::Interior { height, children } => {
                if *height == 0 || *height as usize >= MAX_INDEX_LEVELS {
                    return Err(IndexError::OverLimit("height"));
                }
                if children.is_empty() || children.len() > MAX_FANOUT {
                    return Err(IndexError::OverLimit("interior children"));
                }
                for child in children {
                    child.validate()?;
                    if child.height.checked_add(1) != Some(*height) {
                        return Err(IndexError::Malformed("child height"));
                    }
                    check(child.range)?;
                }
                self.segments()?;
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, IndexError> {
        self.validate()?;
        let mut w = Writer(Vec::with_capacity(1024));
        w.0.extend_from_slice(MAGIC);
        w.u16(VERSION);
        match self {
            Self::Leaf(entries) => {
                w.u8(KIND_LEAF);
                w.u16(entries.len() as u16);
                for entry in entries {
                    w.u64(entry.range.start);
                    w.u64(entry.range.end);
                    match &entry.kind {
                        SegmentKind::Gap(reason) => {
                            w.u8(0);
                            w.u8(match reason {
                                GapReason::Source => 0,
                                GapReason::Admission => 1,
                                GapReason::Decode => 2,
                                GapReason::NotCommitted => unreachable!("validated"),
                            });
                        }
                        SegmentKind::Media { pcm, video } => {
                            w.u8(1);
                            match pcm {
                                None => w.u8(0),
                                Some(pcm) => {
                                    w.u8(1);
                                    w.name(&pcm.file);
                                    w.0.extend_from_slice(pcm.digest.as_bytes());
                                    w.u64(pcm.bytes);
                                }
                            }
                            match video {
                                None => w.u8(0),
                                Some(video) => {
                                    w.u8(1);
                                    w.name(&video.file);
                                    w.0.extend_from_slice(video.digest.as_bytes());
                                    w.u64(video.bytes);
                                    w.u64(video.pts_base_100ns as u64);
                                }
                            }
                        }
                    }
                }
            }
            Self::Interior { height, children } => {
                w.u8(KIND_INTERIOR);
                w.u8(*height);
                w.u16(children.len() as u16);
                for child in children {
                    w.0.extend_from_slice(child.digest.as_bytes());
                    w.u32(child.bytes);
                    w.u64(child.range.start);
                    w.u64(child.range.end);
                    w.u8(child.height);
                    w.u64(child.segments);
                }
            }
        }
        if w.0.len() > MAX_PAGE_BYTES {
            return Err(IndexError::OverLimit("page bytes"));
        }
        Ok(w.0)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, IndexError> {
        if bytes.len() > MAX_PAGE_BYTES {
            return Err(IndexError::OverLimit("page bytes"));
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take(8)? != MAGIC {
            return Err(IndexError::Malformed("magic"));
        }
        let version = r.u16()?;
        if version != VERSION {
            return Err(IndexError::Version(version));
        }
        let page = match r.u8()? {
            KIND_LEAF => {
                let count = r.u16()? as usize;
                if count == 0 || count > MAX_LEAF_SEGMENTS {
                    return Err(IndexError::OverLimit("leaf entries"));
                }
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let range = SampleRange::new(r.u64()?, r.u64()?).ok_or(IndexError::Malformed("range"))?;
                    let kind = match r.u8()? {
                        0 => SegmentKind::Gap(match r.u8()? {
                            0 => GapReason::Source,
                            1 => GapReason::Admission,
                            2 => GapReason::Decode,
                            _ => return Err(IndexError::Malformed("gap reason")),
                        }),
                        1 => {
                            let pcm = match r.u8()? {
                                0 => None,
                                1 => Some(SidecarRef { file: r.name()?, digest: r.digest()?, bytes: r.u64()? }),
                                _ => return Err(IndexError::Malformed("pcm flag")),
                            };
                            let video = match r.u8()? {
                                0 => None,
                                1 => Some(VideoSegmentRef {
                                    file: r.name()?,
                                    digest: r.digest()?,
                                    bytes: r.u64()?,
                                    pts_base_100ns: r.u64()? as i64,
                                }),
                                _ => return Err(IndexError::Malformed("video flag")),
                            };
                            SegmentKind::Media { pcm, video }
                        }
                        _ => return Err(IndexError::Malformed("segment kind")),
                    };
                    entries.push(SegmentEntry { range, kind });
                }
                Self::Leaf(entries)
            }
            KIND_INTERIOR => {
                let height = r.u8()?;
                let count = r.u16()? as usize;
                if count == 0 || count > MAX_FANOUT {
                    return Err(IndexError::OverLimit("interior children"));
                }
                let mut children = Vec::with_capacity(count);
                for _ in 0..count {
                    let digest = r.digest()?;
                    let bytes = r.u32()?;
                    let range = SampleRange::new(r.u64()?, r.u64()?).ok_or(IndexError::Malformed("range"))?;
                    children.push(PageRef { digest, bytes, range, height: r.u8()?, segments: r.u64()? });
                }
                Self::Interior { height, children }
            }
            _ => return Err(IndexError::Malformed("page kind")),
        };
        if r.at != bytes.len() {
            return Err(IndexError::Malformed("trailing bytes"));
        }
        page.validate()?;
        Ok(page)
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn name(&mut self, name: &FileName) {
        self.u8(name.0.len() as u8);
        self.0.extend_from_slice(name.0.as_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], IndexError> {
        let end = self.at.checked_add(len).filter(|end| *end <= self.bytes.len()).ok_or(IndexError::Malformed("truncated"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8, IndexError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, IndexError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, IndexError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, IndexError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn digest(&mut self) -> Result<BlobId, IndexError> {
        Ok(BlobId::from_bytes(self.take(32)?.try_into().unwrap()))
    }
    fn name(&mut self) -> Result<FileName, IndexError> {
        let len = self.u8()? as usize;
        let bytes = self.take(len)?;
        FileName::new(std::str::from_utf8(bytes).map_err(|_| IndexError::BadFileName)?)
    }
}

// ---------------------------------------------------------------------------
// the store
// ---------------------------------------------------------------------------

/// One timeline's directory: pages under `pages/`, segment files beside.
#[derive(Clone, Debug)]
pub struct IndexStore {
    dir: PathBuf,
}

fn digest_hex(digest: &BlobId) -> String {
    let text = digest.to_string();
    text.strip_prefix(BlobId::PREFIX).unwrap_or(&text).to_string()
}

impl IndexStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, IndexError> {
        let dir = dir.into();
        std::fs::create_dir_all(dir.join("pages")).map_err(io)?;
        Ok(Self { dir })
    }

    /// A store read by a reader: no directories are created.
    pub fn existing(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn page_path(&self, digest: &BlobId) -> PathBuf {
        self.dir.join("pages").join(format!("{}.page", digest_hex(digest)))
    }

    pub fn file_path(&self, name: &FileName) -> PathBuf {
        self.dir.join(name.as_str())
    }

    /// Write one page immutably. A page with the same digest already on disk
    /// is the same bytes and is left alone.
    pub fn write_page(&self, page: &IndexPage) -> Result<PageRef, IndexError> {
        let bytes = page.encode()?;
        let digest = BlobId::hash_of(&bytes);
        let path = self.page_path(&digest);
        if !path.is_file() {
            write_atomic(&path, &bytes)?;
        }
        Ok(PageRef {
            digest,
            bytes: bytes.len() as u32,
            range: page.range().ok_or(IndexError::Malformed("empty page"))?,
            height: page.height(),
            segments: page.segments()?,
        })
    }

    /// The exact bytes of a page, bounded and verified against its digest.
    pub fn page_bytes(&self, page: &PageRef) -> Result<Vec<u8>, IndexError> {
        page.validate()?;
        let mut file = std::fs::File::open(self.page_path(&page.digest)).map_err(io)?;
        let len = file.metadata().map_err(io)?.len();
        if len != page.bytes as u64 {
            return Err(IndexError::RefMismatch("page length"));
        }
        let mut bytes = vec![0u8; page.bytes as usize];
        file.read_exact(&mut bytes).map_err(io)?;
        if BlobId::hash_of(&bytes) != page.digest {
            return Err(IndexError::DigestMismatch);
        }
        Ok(bytes)
    }

    pub fn read_page(&self, page: &PageRef) -> Result<IndexPage, IndexError> {
        let decoded = IndexPage::decode(&self.page_bytes(page)?)?;
        if decoded.height() != page.height || decoded.range() != Some(page.range) || decoded.segments()? != page.segments {
            return Err(IndexError::RefMismatch("page summary"));
        }
        Ok(decoded)
    }

    /// The segment containing `frame`, reading at most one page per level.
    pub fn find(&self, root: &PageRef, frame: u64) -> Result<Option<SegmentEntry>, IndexError> {
        if !root.range.contains(frame) {
            return Ok(None);
        }
        let mut at = *root;
        for _ in 0..MAX_INDEX_LEVELS {
            match self.read_page(&at)? {
                IndexPage::Leaf(entries) => {
                    let slot = entries.partition_point(|entry| entry.range.end <= frame);
                    return Ok(entries.get(slot).filter(|entry| entry.range.contains(frame)).cloned());
                }
                IndexPage::Interior { children, .. } => {
                    let slot = children.partition_point(|child| child.range.end <= frame);
                    match children.get(slot).filter(|child| child.range.contains(frame)) {
                        Some(child) => at = *child,
                        None => return Ok(None),
                    }
                }
            }
        }
        Err(IndexError::OverLimit("height"))
    }

    /// Hash a finished segment file in bounded blocks: what its reference
    /// in the index carries.
    pub fn digest_file(&self, name: &FileName) -> Result<(BlobId, u64), IndexError> {
        hash_file(&self.file_path(name), MAX_SEGMENT_FILE_BYTES)
    }

    /// The file `name` exists with exactly `bytes` bytes hashing to `digest`,
    /// read in bounded blocks.
    pub fn verify_file(&self, name: &FileName, bytes: u64, digest: &BlobId) -> Result<(), IndexError> {
        verify_file(&self.file_path(name), bytes, digest)
    }

    /// `new` is `old` with segments appended and nothing else changed:
    /// the same start, and every page of `old` either kept by reference or
    /// replaced by a page whose content has it as a prefix. Reads at most
    /// two pages per level.
    pub fn verify_extends(&self, old: Option<&PageRef>, new: Option<&PageRef>) -> Result<(), IndexError> {
        let (old, new) = match (old, new) {
            (None, _) => return Ok(()),
            (Some(_), None) => return Err(IndexError::NotAnExtension("root removed")),
            (Some(old), Some(new)) => (*old, *new),
        };
        old.validate()?;
        new.validate()?;
        if new.range.start != old.range.start || new.range.end < old.range.end || new.height < old.height || new.segments < old.segments {
            return Err(IndexError::NotAnExtension("root summary"));
        }
        // The old root's counterpart is the leftmost page at its height.
        let mut at = new;
        while at.height > old.height {
            match self.read_page(&at)? {
                IndexPage::Interior { children, .. } => at = children[0],
                IndexPage::Leaf(_) => return Err(IndexError::NotAnExtension("height")),
            }
        }
        self.page_extends(&old, &at)
    }

    fn page_extends(&self, old: &PageRef, new: &PageRef) -> Result<(), IndexError> {
        if old == new {
            return Ok(());
        }
        if new.range.start != old.range.start || new.range.end < old.range.end || new.height != old.height {
            return Err(IndexError::NotAnExtension("page summary"));
        }
        match (self.read_page(old)?, self.read_page(new)?) {
            (IndexPage::Leaf(old), IndexPage::Leaf(new)) => {
                if new.len() < old.len() || new[..old.len()] != old[..] {
                    return Err(IndexError::NotAnExtension("leaf prefix"));
                }
                Ok(())
            }
            (IndexPage::Interior { children: old, .. }, IndexPage::Interior { children: new, .. }) => {
                let last = old.len() - 1;
                if new.len() < old.len() || new[..last] != old[..last] {
                    return Err(IndexError::NotAnExtension("interior prefix"));
                }
                self.page_extends(&old[last], &new[last])
            }
            _ => Err(IndexError::NotAnExtension("page kind")),
        }
    }
}

fn hash_file(path: &Path, max_bytes: u64) -> Result<(BlobId, u64), IndexError> {
    let mut file = std::fs::File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    if len > max_bytes {
        return Err(IndexError::OverLimit("file bytes"));
    }
    let mut hasher = Sha256::new();
    let mut block = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut block).map_err(io)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > len {
            return Err(IndexError::RefMismatch("file grew while hashed"));
        }
        hasher.update(&block[..read]);
    }
    if total != len {
        return Err(IndexError::RefMismatch("file shrank while hashed"));
    }
    Ok((BlobId::from_bytes(hasher.finalize()), total))
}

/// Exact length first (cheap), then the digest in bounded blocks.
pub fn verify_file(path: &Path, bytes: u64, digest: &BlobId) -> Result<(), IndexError> {
    let len = std::fs::metadata(path).map_err(io)?.len();
    if len != bytes {
        return Err(IndexError::RefMismatch("file length"));
    }
    let (actual, total) = hash_file(path, bytes)?;
    if total != bytes || actual != *digest {
        return Err(IndexError::DigestMismatch);
    }
    Ok(())
}

/// Admit `bytes` under the immutable name `path`: a fresh temporary
/// (`create_new`), synced, then hard-linked to the final name, which fails
/// rather than replacing anything already there. An existing file is
/// accepted only if it holds exactly these bytes. The directory is synced
/// so the name survives a crash.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), IndexError> {
    let parent = path.parent().ok_or(IndexError::Malformed("path without parent"))?;
    let temp = parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_TICKET.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    let linked = std::fs::hard_link(&temp, path);
    let _ = std::fs::remove_file(&temp);
    match linked {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return verify_file(path, bytes.len() as u64, &BlobId::hash_of(bytes)).map_err(|_| IndexError::Clobber);
        }
        Err(error) => return Err(io(error)),
    }
    sync_dir(parent)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), IndexError> {
    std::fs::File::open(dir).and_then(|dir| dir.sync_all()).map_err(io)
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), IndexError> {
    // Windows cannot open a directory as a file; its rename metadata is
    // journaled by NTFS.
    Ok(())
}

// ---------------------------------------------------------------------------
// append
// ---------------------------------------------------------------------------

enum Grown {
    Replaced(PageRef),
    Split(PageRef, PageRef),
}

/// What one append did: the new root, the pages it wrote, and the pages of
/// the previous spine the new root no longer reaches (still on disk, still
/// read by older snapshots until a publisher releases them).
#[derive(Clone, Debug)]
pub struct AppendOutcome {
    pub root: PageRef,
    pub written: Vec<PageRef>,
    pub superseded: Vec<PageRef>,
}

impl IndexStore {
    /// Append one segment after `root`. The segment must start exactly where
    /// the root's range ends; an empty index accepts any start.
    pub fn append(&self, root: Option<&PageRef>, entry: SegmentEntry) -> Result<AppendOutcome, IndexError> {
        entry.validate()?;
        let mut written = Vec::new();
        let mut superseded = Vec::new();
        if let Some(root) = root {
            root.validate()?;
        }
        let Some(root) = root else {
            let leaf = self.write_page(&IndexPage::Leaf(vec![entry]))?;
            written.push(leaf);
            return Ok(AppendOutcome { root: leaf, written, superseded });
        };
        if entry.range.start != root.range.end {
            return Err(IndexError::NotContiguous { expected: root.range.end, found: entry.range.start });
        }
        let root = match self.grow(root, entry, &mut written, &mut superseded)? {
            Grown::Replaced(page) => page,
            Grown::Split(left, right) => {
                let height = left.height + 1;
                if height as usize >= MAX_INDEX_LEVELS {
                    return Err(IndexError::Full);
                }
                let page = self.write_page(&IndexPage::Interior { height, children: vec![left, right] })?;
                written.push(page);
                page
            }
        };
        Ok(AppendOutcome { root, written, superseded })
    }

    fn grow(&self, at: &PageRef, entry: SegmentEntry, written: &mut Vec<PageRef>, superseded: &mut Vec<PageRef>) -> Result<Grown, IndexError> {
        match self.read_page(at)? {
            IndexPage::Leaf(mut entries) => {
                if entries.len() < MAX_LEAF_SEGMENTS {
                    entries.push(entry);
                    let page = self.write_page(&IndexPage::Leaf(entries))?;
                    written.push(page);
                    superseded.push(*at);
                    Ok(Grown::Replaced(page))
                } else {
                    let page = self.write_page(&IndexPage::Leaf(vec![entry]))?;
                    written.push(page);
                    Ok(Grown::Split(*at, page))
                }
            }
            IndexPage::Interior { height, mut children } => {
                let last = *children.last().ok_or(IndexError::Malformed("empty interior"))?;
                match self.grow(&last, entry, written, superseded)? {
                    Grown::Replaced(child) => {
                        *children.last_mut().unwrap() = child;
                    }
                    Grown::Split(left, right) => {
                        *children.last_mut().unwrap() = left;
                        if children.len() < MAX_FANOUT {
                            children.push(right);
                        } else {
                            let page = self.write_page(&IndexPage::Interior { height, children })?;
                            written.push(page);
                            superseded.push(*at);
                            let sibling = self.wrap(right, height, written)?;
                            return Ok(Grown::Split(page, sibling));
                        }
                    }
                }
                let page = self.write_page(&IndexPage::Interior { height, children })?;
                written.push(page);
                superseded.push(*at);
                Ok(Grown::Replaced(page))
            }
        }
    }

    /// Lift `page` to `height` through single-child interiors, so it can be
    /// the right sibling of a page of that height.
    fn wrap(&self, mut page: PageRef, height: u8, written: &mut Vec<PageRef>) -> Result<PageRef, IndexError> {
        while page.height < height {
            page = self.write_page(&IndexPage::Interior { height: page.height + 1, children: vec![page] })?;
            written.push(page);
        }
        Ok(page)
    }
}
