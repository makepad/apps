//! Generic AV timelines for Stage: one reader for finished MP4 clips and
//! for growing recordings, the segment index a recording publishes, and the
//! DJ→VJ authority link.
//!
//! * [`budget`] — what file video costs: decoder slots, frame memory, leases.
//! * [`identity`] — exact media identity, publications, consumer loads.
//! * [`timeline`] — immutable snapshots (index root + frontier, no history),
//!   time bases, gaps, stem coverage.
//! * [`index`] — the append-only, bounded, digest-named page tree.
//! * [`sidecar`] — exact PCM sidecars (canonical WAV).
//! * [`publish`] — the recorder-side writer of a growing timeline.
//! * `reader` (feature `reader`, on by default) — the one reader handle and
//!   its two long-lived workers (audio; video and file description) that
//!   decode pages and frames.
//! * [`transport`] — the authority link record and its validation.

pub mod budget;
pub mod identity;
pub mod index;
pub mod payload;
pub mod publish;
#[cfg(feature = "reader")]
pub mod reader;
pub mod sidecar;
pub mod timeline;
pub mod transport;
pub mod window;

/// Fixed bounds. Everything a page, a read or a worker holds is capped here.
pub mod limits {
    /// Children per interior index page.
    pub const MAX_FANOUT: usize = 64;
    /// Segments per leaf page: each segment is up to two blobs (MP4 + WAV)
    /// and a leaf published as one asset must stay within 64 files.
    pub const MAX_LEAF_SEGMENTS: usize = 31;
    /// Index levels (leaf = height 0). Asset dependency depth is 8; one
    /// level is the timeline head asset and one the segment files, so the
    /// tree keeps at most six.
    pub const MAX_INDEX_LEVELS: usize = 6;
    /// Largest encoded index page.
    pub const MAX_PAGE_BYTES: usize = 64 * 1024;
    pub const MAX_FILE_NAME_BYTES: usize = 96;
    /// Largest segment file (MP4 or WAV) an index may reference.
    pub const MAX_SEGMENT_FILE_BYTES: u64 = 1 << 30;
    pub const MAX_SAMPLE_RATE: u32 = 192_000;
    pub const MAX_VIDEO_WIDTH: u32 = 4096;
    pub const MAX_VIDEO_HEIGHT: u32 = 4096;
    /// Longest single segment (frames): one minute at the highest rate.
    pub const MAX_SEGMENT_FRAMES: u64 = 60 * MAX_SAMPLE_RATE as u64;
    /// Largest audio read request.
    pub const MAX_READ_FRAMES: u64 = 1 << 17;
    /// Largest delivered PCM page.
    pub const MAX_PAGE_FRAMES: usize = 16_384;
    /// Results one request may produce (pages + coalesced gaps); a read that
    /// would need more stops early and says where (`Done::reached`).
    pub const MAX_RESULTS_PER_READ: usize = 48;
    /// Index segments one audio read may visit before stopping early.
    pub const MAX_SEGMENTS_PER_READ: usize = 32;
    /// Payload bytes sent but not yet taken by the consumer, across the
    /// reader's workers. Each worker has its own share of it (see the lane
    /// constants of `reader`), which its largest result fits; past its share
    /// a worker waits (never the UI) until the consumer drains.
    pub const MAX_IN_FLIGHT_BYTES: usize = 96 << 20;
    /// Decoded frames kept past a read's end for the next sequential read.
    pub const MAX_CARRY_FRAMES: usize = 16_384;
    /// Recycled PCM page buffers kept by the worker.
    pub const PCM_POOL: usize = 16;
    /// Loads a reader holds at once, in every state (being described,
    /// opening, open, closing), on either worker.
    pub const MAX_READER_CONSUMERS: usize = 8;
    /// Command and result queue slots of a reader, split between its workers.
    pub const READER_COMMANDS: usize = 64;
    pub const READER_RESULTS: usize = 64;
    /// A forward gap up to this many seconds is decoded through rather than
    /// sought.
    pub const SEQUENTIAL_SKIP_SECS: u64 = 2;
    /// How much earlier than a read an audio seek lands, so the chunk that
    /// overlaps the read's start is decoded and trimmed, not skipped.
    pub const AUDIO_SEEK_PREROLL_100NS: i64 = 5_000_000;
    /// Chunk timestamps within this many frames of the expected start are
    /// the same stream (100 ns rounding).
    pub const AUDIO_PTS_SLACK_FRAMES: u64 = 2;
    pub const VIDEO_SEEK_PREROLL_100NS: i64 = 5_000_000;
    pub const VIDEO_SEQUENTIAL_100NS: i64 = 10_000_000;
    pub const MAX_COVERAGE_RANGES: usize = 64;
    pub const MAX_MODEL_KEY_BYTES: usize = 64;
    pub const MAX_AUTHORITY_LINKS: usize = 8;
    pub const MAX_TAIL_PREBUFFER_SECS: u64 = 10;
}

pub use budget::{
    reader_source_bytes, source_bytes, video_size_allowed, worst_case_source_bytes, VideoBudget, VideoFrameLease, VideoLease, FRAME_RING,
    MAX_VIDEO_FILE_BYTES, MAX_VIDEO_LONG_SIDE, MAX_VIDEO_SHORT_SIDE, MAX_VIDEO_SOURCES, READER_BYTES_PER_PIXEL,
};
#[cfg(feature = "reader")]
pub use budget::{ReaderSourceError, ReaderVideoReservation, ReaderVideoSource};
pub use identity::{ConsumerId, ConsumerLoad, MediaIdentity, PublicationSeq, ReadTag, TimelineId};
pub use index::{FileName, IndexError, IndexPage, IndexStore, PageRef, SegmentEntry, SegmentKind, SidecarRef, VideoSegmentRef};
pub use publish::{PublishError, Published, TimelineWriter};
pub use payload::{PageGroup, PageGroupError, PagePermit, PcmPage, MAX_GROUP_PAGES, PAGE_BYTES, PAGE_FRAMES};
#[cfg(feature = "reader")]
pub use reader::{AvReader, OpenRefusal, Payload, ReadSource, ReaderCommand, ReaderError, ReaderResult, VideoFramePayload};
#[cfg(feature = "reader")]
pub use reader::{
    FrameGroup, FrameGroupError, UploadFrame, UploadInfo, UploadRefusal, UploadReturn, UploadStub, UploadTicket, VIDEO_DECODE_CREDITS,
    VIDEO_UPLOAD_PERMITS,
};
pub use timeline::{
    AudioCaps, Container, GapReason, Origin, SampleRange, StemCoverage, StemModelKey, StemQuality, TimeBase, TimelineSnapshot,
    TimelineState, VideoCaps,
};
pub use transport::{AuthorityLink, AuthorityLinks, AuthoritySnapshot, FollowMode, LinkError, LinkId, LoopDefinition, LoopShape, RangeEdit, RangeEditPolicy};
pub use window::{
    FrameCoverage, InstallOutcome, InstallReceipt, PagePlacement, PageRefusal, PageRetirement, PageWindow, ReadRequest, RetireReceipt,
    SettleOutcome, WindowError, WindowMotion, WindowPlan, WindowPolicy, WindowStatus,
};
