//! Publishing a growing timeline, one committed segment at a time.
//!
//! The writer is owned by one recorder worker. Every append yields a new
//! immutable [`TimelineSnapshot`] with the next publication number; the
//! snapshot holds the index root only. Terminal states are published once
//! and refuse every later append.

use crate::identity::{MediaIdentity, PublicationSeq};
use crate::index::{AppendOutcome, IndexError, IndexStore, PageRef, SegmentEntry, SegmentKind};
use crate::timeline::{AudioCaps, Origin, TimeBase, TimelineSnapshot, TimelineState, VideoCaps};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishError {
    Index(IndexError),
    NotGrowing,
    Terminal,
    MissingStream(&'static str),
    Snapshot(crate::timeline::SnapshotError),
}

impl From<IndexError> for PublishError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}

pub struct TimelineWriter {
    store: IndexStore,
    media: MediaIdentity,
    time_base: TimeBase,
    audio: Option<AudioCaps>,
    video: Option<VideoCaps>,
    start: u64,
    root: Option<PageRef>,
    publication: PublicationSeq,
    state: TimelineState,
}

/// One publication: the snapshot consumers read and the index pages a
/// content-addressed publisher retains (written) or may release once no
/// snapshot needs them (superseded).
pub struct Published {
    pub snapshot: TimelineSnapshot,
    pub written: Vec<PageRef>,
    pub superseded: Vec<PageRef>,
}

impl TimelineWriter {
    /// A new, empty growing timeline starting at frame `start`.
    pub fn create(
        store: IndexStore,
        media: MediaIdentity,
        audio: Option<AudioCaps>,
        video: Option<VideoCaps>,
        start: u64,
    ) -> Result<(Self, TimelineSnapshot), PublishError> {
        if !media.is_growing() {
            return Err(PublishError::NotGrowing);
        }
        let audio_caps = audio.ok_or(PublishError::MissingStream("growing timelines are clocked by exact PCM"))?;
        let writer = Self {
            store,
            media,
            time_base: TimeBase::sample_rate(audio_caps.sample_rate),
            audio,
            video,
            start,
            root: None,
            publication: PublicationSeq(0),
            state: TimelineState::Pending,
        };
        let snapshot = writer.snapshot();
        snapshot.validate().map_err(PublishError::Snapshot)?;
        Ok((writer, snapshot))
    }

    pub fn snapshot(&self) -> TimelineSnapshot {
        TimelineSnapshot {
            media: self.media,
            publication: self.publication,
            time_base: self.time_base,
            retained_start: self.root.map_or(self.start, |root| root.range.start),
            committed_end: self.root.map_or(self.start, |root| root.range.end),
            audio: self.audio,
            video: self.video,
            origin: Origin::Indexed { store: self.store.dir().to_path_buf(), root: self.root },
            state: self.state.clone(),
        }
    }

    pub fn store(&self) -> &IndexStore {
        &self.store
    }

    /// Commit one finished segment. Every file it names is checked in the
    /// store (exact length and digest, bounded blocks) BEFORE the frontier
    /// advances; a missing or different file refuses the append.
    pub fn append(&mut self, entry: SegmentEntry) -> Result<Published, PublishError> {
        if self.state.is_terminal() {
            return Err(PublishError::Terminal);
        }
        entry.validate()?;
        if let SegmentKind::Media { pcm, video } = &entry.kind {
            let Some(pcm) = pcm else {
                return Err(PublishError::MissingStream("media segment without exact PCM"));
            };
            if video.is_some() && self.video.is_none() {
                return Err(PublishError::MissingStream("video segment in an audio-only timeline"));
            }
            self.store.verify_file(&pcm.file, pcm.bytes, &pcm.digest)?;
            if let Some(video) = video {
                self.store.verify_file(&video.file, video.bytes, &video.digest)?;
            }
        }
        let expected = self.root.map_or(self.start, |root| root.range.end);
        if entry.range.start != expected {
            return Err(PublishError::Index(IndexError::NotContiguous { expected, found: entry.range.start }));
        }
        let AppendOutcome { root, written, superseded } = self.store.append(self.root.as_ref(), entry)?;
        self.root = Some(root);
        self.state = TimelineState::Recording;
        self.publication.0 += 1;
        Ok(Published { snapshot: self.snapshot(), written, superseded })
    }

    /// The recording ended normally: the frontier is final.
    pub fn complete(&mut self) -> Result<TimelineSnapshot, PublishError> {
        self.terminate(TimelineState::Complete)
    }

    /// The recording stopped on an error at the committed frontier.
    pub fn fail(&mut self, reason: impl Into<String>) -> Result<TimelineSnapshot, PublishError> {
        self.terminate(TimelineState::Failed(reason.into()))
    }

    fn terminate(&mut self, state: TimelineState) -> Result<TimelineSnapshot, PublishError> {
        if self.state.is_terminal() {
            return Err(PublishError::Terminal);
        }
        self.state = state;
        self.publication.0 += 1;
        Ok(self.snapshot())
    }
}
