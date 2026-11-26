//! The immutable view of one media timeline at one publication.
//!
//! A snapshot retains the index ROOT and a few numbers, never the audio or
//! the history: its size does not grow with the recording. Positions are
//! integer source sample frames at the timeline's audio rate; video
//! timestamps are translated into the same frames.

use crate::identity::{MediaIdentity, PublicationSeq};
use crate::index::PageRef;
use crate::limits::*;
use std::path::PathBuf;

/// A rational time base: one tick is `num / den` seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimeBase {
    pub num: u32,
    pub den: u32,
}

impl TimeBase {
    /// One tick per sample frame at `rate`.
    pub fn sample_rate(rate: u32) -> Self {
        Self { num: 1, den: rate }
    }

    pub fn is_valid(&self) -> bool {
        self.num > 0 && self.den > 0
    }

    /// Ticks to 100 ns units, rounding to nearest. `None` for an invalid
    /// base or a result outside `i64`.
    pub fn ticks_to_100ns(&self, ticks: i64) -> Option<i64> {
        if !self.is_valid() {
            return None;
        }
        let n = ticks as i128 * self.num as i128 * 10_000_000;
        i64::try_from(round_div(n, self.den as i128)).ok()
    }

    /// 100 ns units to ticks, rounding to nearest. `None` for an invalid
    /// base or a result outside `i64`.
    pub fn hns_to_ticks(&self, hns: i64) -> Option<i64> {
        if !self.is_valid() {
            return None;
        }
        let n = hns as i128 * self.den as i128;
        i64::try_from(round_div(n, self.num as i128 * 10_000_000)).ok()
    }
}

fn round_div(n: i128, d: i128) -> i128 {
    if n >= 0 { (n + d / 2) / d } else { (n - d / 2) / d }
}

/// A half-open range of sample frames, `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct SampleRange {
    pub start: u64,
    pub end: u64,
}

impl SampleRange {
    pub fn new(start: u64, end: u64) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }
    /// Zero for an empty or malformed (`start > end`) range.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
    pub fn contains(&self, frame: u64) -> bool {
        frame >= self.start && frame < self.end
    }
    pub fn within(&self, outer: &SampleRange) -> bool {
        self.start >= outer.start && self.end <= outer.end
    }
    pub fn intersect(&self, other: &SampleRange) -> Option<SampleRange> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(SampleRange { start, end })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AudioCaps {
    pub sample_rate: u32,
    /// Channels of the source; readers always deliver the stereo pair.
    pub source_channels: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoCaps {
    pub width: u32,
    pub height: u32,
    /// Nominal cadence, informational: frames carry their own timestamps.
    pub fps_num: u32,
    pub fps_den: u32,
}

/// Whether the committed frontier may still advance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineState {
    /// Capture has started but no segment has been committed yet.
    Pending,
    /// At least one segment is committed; later publications may advance it.
    Recording,
    /// Final. Nothing past `committed_end` will ever exist.
    Complete,
    /// Stopped by an error at the committed frontier; what is committed stays
    /// readable.
    Failed(String),
}

impl TimelineState {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Pending | Self::Recording)
    }
}

/// Where the samples of a snapshot live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// One finished container, read through the platform decoder. The
    /// frontier is fixed.
    SingleFile { path: PathBuf, container: Container },
    /// A segment index under `store`, reached from `root` (absent while the
    /// timeline has no segment yet).
    Indexed { store: PathBuf, root: Option<PageRef> },
}

/// The container a single file is, since a cache object's name says nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Container {
    Mp4,
}

/// One immutable publication of a timeline. Cheap to clone behind an `Arc`;
/// holds no sample data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineSnapshot {
    pub media: MediaIdentity,
    pub publication: PublicationSeq,
    pub time_base: TimeBase,
    /// First frame still retained (history before it was released).
    pub retained_start: u64,
    /// End of committed, readable frames. Never beyond what was published.
    pub committed_end: u64,
    pub audio: Option<AudioCaps>,
    pub video: Option<VideoCaps>,
    pub origin: Origin,
    pub state: TimelineState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    BadTimeBase,
    BadRange,
    NoStreams,
    OverCaps(&'static str),
    IdentityChanged,
    OriginChanged,
    RootMismatch,
    Regressed(&'static str),
    AfterTerminal,
    CompletedMustBeFinal,
}

impl TimelineSnapshot {
    pub fn committed(&self) -> SampleRange {
        SampleRange { start: self.retained_start, end: self.committed_end }
    }

    pub fn validate(&self) -> Result<(), SnapshotError> {
        if !self.time_base.is_valid() {
            return Err(SnapshotError::BadTimeBase);
        }
        if self.retained_start > self.committed_end {
            return Err(SnapshotError::BadRange);
        }
        if self.state == TimelineState::Pending && self.retained_start != self.committed_end {
            return Err(SnapshotError::BadRange);
        }
        if self.state == TimelineState::Recording && self.retained_start == self.committed_end {
            return Err(SnapshotError::BadRange);
        }
        if self.audio.is_none() && self.video.is_none() {
            return Err(SnapshotError::NoStreams);
        }
        if let Some(audio) = self.audio {
            if audio.sample_rate == 0 || audio.sample_rate > MAX_SAMPLE_RATE || audio.source_channels == 0 {
                return Err(SnapshotError::OverCaps("audio format"));
            }
            if self.time_base != TimeBase::sample_rate(audio.sample_rate) {
                return Err(SnapshotError::BadTimeBase);
            }
        }
        if let Some(video) = self.video {
            if video.width == 0 || video.height == 0 || video.width > MAX_VIDEO_WIDTH || video.height > MAX_VIDEO_HEIGHT {
                return Err(SnapshotError::OverCaps("video dimensions"));
            }
        }
        if let Origin::Indexed { root, .. } = &self.origin {
            let expected = match root {
                Some(root) => root.range,
                None => SampleRange { start: self.committed_end, end: self.committed_end },
            };
            if expected.start != self.retained_start || expected.end != self.committed_end {
                return Err(SnapshotError::RootMismatch);
            }
        }
        if let MediaIdentity::Completed { .. } = self.media {
            if self.state != TimelineState::Complete {
                return Err(SnapshotError::CompletedMustBeFinal);
            }
        }
        Ok(())
    }

    /// Whether `next` may replace `self` for a consumer that loaded `self`:
    /// the same identity, formats and index store, a newer publication, a
    /// frontier that did not move backwards, and nothing after a terminal
    /// state. Structural only: that the new index root really extends the
    /// old one is checked against the pages by
    /// [`crate::index::IndexStore::verify_extends`] (the reader does so on
    /// every `Advance`). Anything else is a new load, not growth.
    pub fn check_successor(&self, next: &TimelineSnapshot) -> Result<(), SnapshotError> {
        next.validate()?;
        if next.media != self.media || next.time_base != self.time_base || next.audio != self.audio || next.video != self.video {
            return Err(SnapshotError::IdentityChanged);
        }
        let same_origin = match (&self.origin, &next.origin) {
            (Origin::Indexed { store: a, .. }, Origin::Indexed { store: b, .. }) => a == b,
            _ => false,
        };
        if !same_origin {
            return Err(SnapshotError::OriginChanged);
        }
        if self.state.is_terminal() {
            return Err(SnapshotError::AfterTerminal);
        }
        if next.publication <= self.publication {
            return Err(SnapshotError::Regressed("publication"));
        }
        if next.committed_end < self.committed_end {
            return Err(SnapshotError::Regressed("committed end"));
        }
        if next.retained_start < self.retained_start {
            return Err(SnapshotError::Regressed("retained start"));
        }
        Ok(())
    }
}

/// Why frames of a range are absent. A gap is reported, never filled with
/// invented samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GapReason {
    /// The source itself had nothing here (capture dropped, stream stopped).
    Source,
    /// The recorder could not admit what the source produced.
    Admission,
    /// The decoder produced nothing for this range (declared length beyond
    /// the decoded stream, or a hole between decoded chunks).
    Decode,
    /// Before `retained_start` or at/after `committed_end`.
    NotCommitted,
}

// ---------------------------------------------------------------------------
// stems coverage
// ---------------------------------------------------------------------------

/// Which separator produced stems.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StemQuality {
    /// Low-latency bridge (about a 2.76 s model window).
    Bridge,
    /// Full quality (about an 11 s model window).
    Full,
}

/// The model a coverage was produced by, named by the AI library that owns
/// it; this crate never runs a model.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StemModelKey(String);

impl StemModelKey {
    pub fn new(key: impl Into<String>) -> Option<Self> {
        let key = key.into();
        (!key.is_empty() && key.len() <= MAX_MODEL_KEY_BYTES).then_some(Self(key))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What stems exist for one media identity. Ranges are sorted, disjoint and
/// bounded in count; a range not listed has no stems and plays the mix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StemCoverage {
    pub media: MediaIdentity,
    pub model: StemModelKey,
    pub quality: StemQuality,
    ranges: Vec<SampleRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageError {
    Unsorted,
    TooManyRanges,
    Empty,
}

impl StemCoverage {
    pub fn new(media: MediaIdentity, model: StemModelKey, quality: StemQuality, ranges: Vec<SampleRange>) -> Result<Self, CoverageError> {
        if ranges.len() > MAX_COVERAGE_RANGES {
            return Err(CoverageError::TooManyRanges);
        }
        let mut last_end = None;
        for range in &ranges {
            if range.is_empty() {
                return Err(CoverageError::Empty);
            }
            if last_end.is_some_and(|end| range.start <= end) {
                return Err(CoverageError::Unsorted);
            }
            last_end = Some(range.end);
        }
        Ok(Self { media, model, quality, ranges })
    }

    pub fn ranges(&self) -> &[SampleRange] {
        &self.ranges
    }

    pub fn covers(&self, range: &SampleRange) -> bool {
        self.ranges.iter().any(|covered| range.within(covered))
    }

    /// How far behind `frontier` the coverage ends, in frames: the honest lag
    /// a Tail deck shows. `frontier` when nothing is covered.
    pub fn lag_behind(&self, frontier: u64) -> u64 {
        frontier.saturating_sub(self.ranges.last().map_or(0, |range| range.end))
    }
}
