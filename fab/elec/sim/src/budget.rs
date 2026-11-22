//! What the live run may keep, in one place, each bound with its reason. Every bound degrades
//! detail, never span: past it the oldest stretch keeps less resolution (or, for kept takes nobody
//! plays, goes to be rendered again), it is logged once, and nothing is refused.

/// The one take's visual frames on the host (`fab_elec_simhost::take::Take`: the grid frames of
/// every net, pin and part, and the device states): about 2 MB a simulated second on a board of a
/// thousand rows, so a take of minutes stays whole and an open run of an hour still reviews from its
/// start. Past it the oldest half's frames merge in pairs (mean, minimum and maximum kept).
pub const TAKE_BYTES: usize = 256 << 20;

/// The trace's closed chunks a record keeps whole (`TraceStore::fold`): the Scope's lanes at the
/// solver's own points. Past it the oldest positions fold; the record keeps its seconds.
pub const RECORD_FINE_BYTES: usize = 64 << 20;

/// The disk the kept recordings may hold together (`TapeShelf`): temporary files, never memory,
/// about 45 minutes of one voice at 96 kHz. Past it the least recently heard take goes (its content
/// renders again when asked).
pub const KEEP_BYTES: u64 = 1 << 30;

/// The run's snapshots in memory (`Snapshots::bytes`): an edit goes back to the newest before it
/// instead of the start. Past it they thin (every other one in the oldest half).
pub const SNAPSHOT_BYTES: u64 = 256 << 20;
