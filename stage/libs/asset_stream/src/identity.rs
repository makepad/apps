//! Who a piece of media is, and who is reading it.
//!
//! A completed source is named by its exact asset revision AND the digest
//! of the bytes that revision resolved to: a new revision is a new source,
//! never a retarget of a loaded one. A growing recording is named by its
//! stable asset, the recorder incarnation that wrote it and the timeline it
//! lays down; its publications are an append-only sequence under that one
//! identity, so growth never looks like a new source and a new source never
//! looks like growth.

use makepad_asset_data::{AssetId, AssetRevisionRef, BlobId};

/// One recorder life writing one timeline. A restarted recorder is a new
/// incarnation even when it writes to the same asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimelineId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MediaIdentity {
    /// A finished file: exact revision and exact content bytes.
    Completed { revision: AssetRevisionRef, content: BlobId },
    /// A recording whose committed frontier may still advance.
    Growing { asset: AssetId, incarnation: u64, timeline: TimelineId },
}

impl MediaIdentity {
    pub fn is_growing(&self) -> bool {
        matches!(self, Self::Growing { .. })
    }
}

/// Append-only publication counter of one growing timeline. `0` is the
/// empty timeline; every published append is exactly one greater.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PublicationSeq(pub u64);

/// A reading party: a DJ deck, a VJ layer, a stem worker. Assigned by the
/// application; stable for the life of that surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConsumerId(pub u32);

/// One load of media into one consumer. `load` is the consumer's own
/// strictly increasing load counter; two consumers' counters are unrelated
/// and never compared with each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConsumerLoad {
    pub consumer: ConsumerId,
    pub load: u64,
}

/// What every reader result is tagged with. A consumer keeps the tag of
/// what it currently wants and retires any result whose tag differs: a
/// different media, a different load, or an older seek.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReadTag {
    pub media: MediaIdentity,
    pub load: ConsumerLoad,
    /// Bumped by the consumer on every seek/cue/direction change.
    pub seek_serial: u64,
}

impl ReadTag {
    /// Whether a result carrying `self` still answers what `wanted` asks.
    pub fn answers(&self, wanted: &ReadTag) -> bool {
        self == wanted
    }
}
