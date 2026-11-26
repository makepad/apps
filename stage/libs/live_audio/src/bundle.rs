//! Prepared stem bundles: the four separated stems of one absolute source
//! range of one media, installed into a paged source's stem slots.
//!
//! A worker reserves one aggregate [`TrackLease`] on the device budget
//! *before* it allocates the four outputs (at least
//! [`stem_bundle_bytes`]`(frames)`), then finishes it with
//! [`TrackLease::finish_stem_bundle`], which checks the rate, the equal
//! lengths against the range and the actual capacities against the lease,
//! and shrinks the lease to them. Every field is private and immutable from
//! then on; a refused finish drops the PCM on the worker that called it.
//!
//! The bundle travels as an `Arc` in `PageOp::InstallStems` and comes back
//! in the page receipt when it is refused or retired; the callback never
//! drops or clones it.

use crate::deck::TrackLease;
use crate::schedule::AudioBudget;
use makepad_stage_asset_stream::{MediaIdentity, SampleRange};
use std::sync::Arc;

/// Stem bundles one paged source holds at once, over all serials.
pub const STEM_SLOTS: usize = 8;

/// The separation model a bundle came from. Immutable; part of any key a
/// host caches bundles under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StemModel {
    pub id: &'static str,
    pub checkpoint_sha256: &'static str,
}

/// Bytes four stereo i16 stems of `frames` frames occupy exactly: the
/// smallest aggregate lease a bundle of that range needs.
pub fn stem_bundle_bytes(frames: u64) -> Option<usize> {
    frames.checked_mul(4 * std::mem::size_of::<[i16; 2]>() as u64).and_then(|bytes| usize::try_from(bytes).ok())
}

/// Four stems in public order (vocals, drums, bass, other) of exactly
/// `range`, at the source rate, with the lease that pays for them.
pub struct PreparedStemBundle {
    media: MediaIdentity,
    model: StemModel,
    sample_rate: u32,
    range: SampleRange,
    stems: [Vec<[i16; 2]>; 4],
    lease: TrackLease,
}

impl PreparedStemBundle {
    pub(crate) fn new(
        media: MediaIdentity,
        model: StemModel,
        sample_rate: u32,
        range: SampleRange,
        stems: [Vec<[i16; 2]>; 4],
        lease: TrackLease,
    ) -> Self {
        Self { media, model, sample_rate, range, stems, lease }
    }

    pub fn media(&self) -> MediaIdentity {
        self.media
    }

    pub fn model(&self) -> StemModel {
        self.model
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The absolute source frames the stems cover.
    pub fn range(&self) -> SampleRange {
        self.range
    }

    /// One stem, public order; exactly `range().len()` frames.
    pub fn stem(&self, lane: usize) -> &[[i16; 2]] {
        self.stems.get(lane).map_or(&[], Vec::as_slice)
    }

    pub fn lease_bytes(&self) -> usize {
        self.lease.bytes()
    }

    pub(crate) fn leased_from(&self, budget: &Arc<AudioBudget>) -> bool {
        self.lease.is_on(budget)
    }

    /// Stem `lane` at absolute source `frame`, which the caller checked is
    /// inside `range`.
    pub(crate) fn frame(&self, lane: usize, frame: u64) -> [f32; 2] {
        let value = self.stems[lane].get((frame - self.range.start) as usize).copied().unwrap_or([0; 2]);
        [value[0] as f32 / 32768.0, value[1] as f32 / 32768.0]
    }
}
