//! The head log: every head change this catalog ever made, in commit order,
//! durable — what store sync replicates (host::sync).
//!
//! Revisions and blobs are immutable and content-addressed, so two stores
//! agree once they hold the same objects and the same HEADS. Heads are the
//! only mutable state that matters for that:
//!
//! - an ALIAS head (`sandbox/games/<slug>` → asset + revision, or cleared);
//! - an ASSET's retirement (a delete hides the asset everywhere; a restore
//!   brings it back).
//!
//! Every such change appends one row here IN THE SAME TRANSACTION as the
//! change (the catalog's alias and retirement paths call [`note_alias_in_tx`]
//! and [`note_asset_state_in_tx`]). `seq` is the durable "changes since N" a
//! peer pulls; nothing here is ever deleted.
//!
//! ## Order: last writer wins on a hybrid clock
//!
//! A row carries a [`Stamp`]: `clock` (milliseconds << 10, plus a counter
//! for changes in the same millisecond) and the 16-byte id of the store
//! that made the change (its `server-id`). Stamps are totally ordered
//! (clock, then origin), so every store picks the same winner for a key.
//! The clock never runs behind a stamp this store has seen (`next` takes
//! `max(wall, last + 1)` and seeing a peer's stamp advances `last`), so a
//! change made after seeing another always orders after it, whatever the
//! wall clocks say.
//!
//! A row that loses (an older stamp arriving late, a concurrent move on
//! another store) is still kept, as history (`applied = 0`): both revisions
//! are in the catalog and the log; the newer one is the head.

use crate::catalog::{fixed16, fixed32};
use crate::error::ServerResult;
use crate::sqlite::Db;
use makepad_asset_data::{AssetId, AssetRevisionId, AssetRevisionRef};
use std::cell::Cell;

pub const SYNC_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sync_log(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    kind INTEGER NOT NULL,
    key TEXT NOT NULL,
    asset_id BLOB NOT NULL,
    revision BLOB,
    retired INTEGER NOT NULL DEFAULT 0,
    stamp INTEGER NOT NULL,
    origin BLOB NOT NULL,
    applied INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS sync_log_identity ON sync_log(kind, key, stamp, origin);
CREATE TABLE IF NOT EXISTS sync_heads(
    kind INTEGER NOT NULL,
    key TEXT NOT NULL,
    stamp INTEGER NOT NULL,
    origin BLOB NOT NULL,
    PRIMARY KEY(kind, key)
);
CREATE TABLE IF NOT EXISTS sync_meta(
    name TEXT PRIMARY KEY,
    num INTEGER,
    bytes BLOB
);
CREATE TABLE IF NOT EXISTS sync_peers(
    peer TEXT PRIMARY KEY,
    cursor INTEGER NOT NULL,
    peer_store BLOB,
    last_ok_ms INTEGER,
    last_error TEXT
);
";

pub const KIND_ALIAS: i64 = 0;
pub const KIND_ASSET: i64 = 1;

/// Milliseconds are shifted by this many bits; the low bits count changes
/// within one millisecond.
const CLOCK_SHIFT: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    pub clock: i64,
    pub origin: [u8; 16],
}

/// One head change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncRow {
    pub seq: u64,
    pub kind: i64,
    /// The alias, or the asset id in lowercase hex ([`asset_key`]).
    pub key: String,
    pub asset_id: AssetId,
    /// Alias rows: the head; `None` = cleared.
    pub revision: Option<AssetRevisionId>,
    /// Asset rows: retired (deleted) or live (restored).
    pub retired: bool,
    pub stamp: Stamp,
    /// Whether this row was the visible head when this store logged it.
    pub applied: bool,
}

impl SyncRow {
    pub fn target(&self) -> Option<AssetRevisionRef> {
        self.revision.map(|revision| AssetRevisionRef { asset_id: self.asset_id, revision })
    }
}

/// The log key of an asset-state row.
pub fn asset_key(asset_id: &AssetId) -> String {
    let mut out = String::with_capacity(32);
    for b in asset_id.as_bytes() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

thread_local! {
    /// The stamp of the peer change being applied on this (the state)
    /// thread, while [`Applying`] is held: the catalog's head paths log
    /// that stamp instead of minting a local one.
    static APPLYING: Cell<Option<Stamp>> = const { Cell::new(None) };
}

/// Held while a peer's change is applied through the catalog's ordinary
/// mutation paths (publish, alias set/clear, retire, restore): every row
/// they log carries the peer's stamp.
pub struct Applying(());

impl Applying {
    pub fn new(stamp: Stamp) -> Applying {
        APPLYING.with(|a| a.set(Some(stamp)));
        Applying(())
    }
}

impl Drop for Applying {
    fn drop(&mut self) {
        APPLYING.with(|a| a.set(None));
    }
}

fn meta_num(db: &Db, name: &str) -> ServerResult<Option<i64>> {
    let mut s = db.prepare("sync meta num", "SELECT num FROM sync_meta WHERE name = ?1")?;
    s.bind_text(1, name)?;
    Ok(if s.step()? && !s.column_is_null(0) { Some(s.column_i64(0)) } else { None })
}

fn set_meta_num(db: &Db, name: &str, value: i64) -> ServerResult<()> {
    let mut s = db.prepare(
        "sync meta set num",
        "INSERT INTO sync_meta(name, num) VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET num = ?2",
    )?;
    s.bind_text(1, name)?;
    s.bind_i64(2, value)?;
    s.run()
}

/// This store's origin id (its `server-id`); zero until the host sets it.
pub fn origin(db: &Db) -> ServerResult<[u8; 16]> {
    let mut s = db.prepare("sync origin", "SELECT bytes FROM sync_meta WHERE name = 'origin'")?;
    if s.step()? && !s.column_is_null(0) {
        return fixed16(&s.column_blob(0), "sync origin");
    }
    Ok([0; 16])
}

/// Records this store's identity. A catalog copied to another machine gets
/// that machine's id the next time its host starts, so the copies never
/// stamp changes as each other.
pub fn set_origin(db: &Db, origin: &[u8; 16]) -> ServerResult<()> {
    let mut s = db.prepare(
        "sync set origin",
        "INSERT INTO sync_meta(name, bytes) VALUES('origin', ?1) ON CONFLICT(name) DO UPDATE SET bytes = ?1",
    )?;
    s.bind_blob(1, origin)?;
    s.run()
}

/// Advance the clock past a stamp this store has seen.
fn observe_in_tx(db: &Db, clock: i64) -> ServerResult<()> {
    if meta_num(db, "clock")?.is_none_or(|last| clock > last) {
        set_meta_num(db, "clock", clock)?;
    }
    Ok(())
}

/// The stamp for a change made now: the applied peer's, or a fresh local one.
fn next_stamp_in_tx(db: &Db, now_ms: u64) -> ServerResult<Stamp> {
    if let Some(stamp) = APPLYING.with(|a| a.get()) {
        observe_in_tx(db, stamp.clock)?;
        return Ok(stamp);
    }
    let wall = (now_ms as i64) << CLOCK_SHIFT;
    let clock = match meta_num(db, "clock")? {
        Some(last) if last >= wall => last + 1,
        _ => wall,
    };
    set_meta_num(db, "clock", clock)?;
    Ok(Stamp { clock, origin: origin(db)? })
}

/// Appends `row` (its seq is ignored); `applied` also makes it the key's head.
/// A row this log already holds (same key and stamp) is a no-op: `false`.
fn insert_in_tx(db: &Db, row: &SyncRow) -> ServerResult<bool> {
    let mut s = db.prepare(
        "sync log insert",
        "INSERT OR IGNORE INTO sync_log(kind, key, asset_id, revision, retired, stamp, origin, applied)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    s.bind_i64(1, row.kind)?;
    s.bind_text(2, &row.key)?;
    s.bind_blob(3, row.asset_id.as_bytes())?;
    match &row.revision {
        Some(r) => s.bind_blob(4, r.as_bytes())?,
        None => s.bind_null(4)?,
    }
    s.bind_i64(5, row.retired as i64)?;
    s.bind_i64(6, row.stamp.clock)?;
    s.bind_blob(7, &row.stamp.origin)?;
    s.bind_i64(8, row.applied as i64)?;
    s.run()?;
    drop(s);
    if db.changes() == 0 {
        return Ok(false);
    }
    if row.applied {
        let mut s = db.prepare(
            "sync head set",
            "INSERT INTO sync_heads(kind, key, stamp, origin) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(kind, key) DO UPDATE SET stamp = ?3, origin = ?4",
        )?;
        s.bind_i64(1, row.kind)?;
        s.bind_text(2, &row.key)?;
        s.bind_i64(3, row.stamp.clock)?;
        s.bind_blob(4, &row.stamp.origin)?;
        s.run()?;
    }
    Ok(true)
}

/// The catalog moved (or cleared) an alias head in the open transaction.
pub(crate) fn note_alias_in_tx(
    db: &Db,
    alias: &str,
    asset_id: &AssetId,
    revision: Option<&AssetRevisionId>,
    now_ms: u64,
) -> ServerResult<()> {
    let stamp = next_stamp_in_tx(db, now_ms)?;
    insert_in_tx(
        db,
        &SyncRow {
            seq: 0,
            kind: KIND_ALIAS,
            key: alias.to_string(),
            asset_id: *asset_id,
            revision: revision.copied(),
            retired: false,
            stamp,
            applied: true,
        },
    )?;
    Ok(())
}

/// The catalog retired (deleted) or restored an asset in the open
/// transaction.
pub(crate) fn note_asset_state_in_tx(db: &Db, asset_id: &AssetId, retired: bool, now_ms: u64) -> ServerResult<()> {
    let stamp = next_stamp_in_tx(db, now_ms)?;
    insert_in_tx(
        db,
        &SyncRow {
            seq: 0,
            kind: KIND_ASSET,
            key: asset_key(asset_id),
            asset_id: *asset_id,
            revision: None,
            retired,
            stamp,
            applied: true,
        },
    )?;
    Ok(())
}

/// v24: the log starts from the heads the catalog already has, stamped with
/// their own update time and the zero origin ("before sync"). Two stores
/// seeded from copies of one catalog hold identical rows, so their first
/// exchange moves nothing; where they diverged, the later update wins.
/// One pass over the aliases and the retired assets, once per catalog.
pub(crate) fn seed_in_tx(db: &Db) -> ServerResult<()> {
    let mut rows = Vec::new();
    let mut s = db.prepare("sync seed aliases", "SELECT alias, asset_id, head_revision, updated_ms FROM asset_aliases")?;
    while s.step()? {
        rows.push(SyncRow {
            seq: 0,
            kind: KIND_ALIAS,
            key: s.column_text(0),
            asset_id: AssetId::from_bytes(fixed16(&s.column_blob(1), "sync seed alias")?),
            revision: Some(AssetRevisionId::from_bytes(fixed32(&s.column_blob(2), "sync seed alias")?)),
            retired: false,
            stamp: Stamp { clock: (s.column_i64(3)) << CLOCK_SHIFT, origin: [0; 16] },
            applied: true,
        });
    }
    drop(s);
    let mut s = db.prepare("sync seed retired", "SELECT asset_id, retired_ms FROM assets WHERE retired_ms IS NOT NULL")?;
    while s.step()? {
        let asset_id = AssetId::from_bytes(fixed16(&s.column_blob(0), "sync seed asset")?);
        rows.push(SyncRow {
            seq: 0,
            kind: KIND_ASSET,
            key: asset_key(&asset_id),
            asset_id,
            revision: None,
            retired: true,
            stamp: Stamp { clock: (s.column_i64(1)) << CLOCK_SHIFT, origin: [0; 16] },
            applied: true,
        });
    }
    drop(s);
    for row in &rows {
        insert_in_tx(db, row)?;
    }
    Ok(())
}

/// Whether this log already holds a row with this key and stamp.
pub fn has_row(db: &Db, kind: i64, key: &str, stamp: &Stamp) -> ServerResult<bool> {
    let mut s = db.prepare(
        "sync log has",
        "SELECT 1 FROM sync_log WHERE kind = ?1 AND key = ?2 AND stamp = ?3 AND origin = ?4",
    )?;
    s.bind_i64(1, kind)?;
    s.bind_text(2, key)?;
    s.bind_i64(3, stamp.clock)?;
    s.bind_blob(4, &stamp.origin)?;
    s.step()
}

/// The stamp of the key's visible head, if it ever had one.
pub fn head_stamp(db: &Db, kind: i64, key: &str) -> ServerResult<Option<Stamp>> {
    let mut s = db.prepare("sync head get", "SELECT stamp, origin FROM sync_heads WHERE kind = ?1 AND key = ?2")?;
    s.bind_i64(1, kind)?;
    s.bind_text(2, key)?;
    if !s.step()? {
        return Ok(None);
    }
    Ok(Some(Stamp { clock: s.column_i64(0), origin: fixed16(&s.column_blob(1), "sync head")? }))
}

/// Keeps a peer's row that does not win here as history. `false` when the
/// log already held it.
pub fn record_history(db: &Db, row: &SyncRow) -> ServerResult<bool> {
    db.tx(|db| {
        observe_in_tx(db, row.stamp.clock)?;
        insert_in_tx(db, &SyncRow { applied: false, ..row.clone() })
    })
}

/// Records a peer's winning row whose change needs no catalog mutation here
/// (the head is already what it says, e.g. both stores published the same
/// revision): it becomes the head's stamp, and nothing else moves.
pub fn record_head(db: &Db, row: &SyncRow) -> ServerResult<bool> {
    db.tx(|db| {
        observe_in_tx(db, row.stamp.clock)?;
        insert_in_tx(db, &SyncRow { applied: true, ..row.clone() })
    })
}

fn read_row(s: &crate::sqlite::Stmt<'_>) -> ServerResult<SyncRow> {
    Ok(SyncRow {
        seq: s.column_u64(0),
        kind: s.column_i64(1),
        key: s.column_text(2),
        asset_id: AssetId::from_bytes(fixed16(&s.column_blob(3), "sync row asset")?),
        revision: if s.column_is_null(4) {
            None
        } else {
            Some(AssetRevisionId::from_bytes(fixed32(&s.column_blob(4), "sync row revision")?))
        },
        retired: s.column_i64(5) != 0,
        stamp: Stamp { clock: s.column_i64(6), origin: fixed16(&s.column_blob(7), "sync row origin")? },
        applied: s.column_i64(8) != 0,
    })
}

const ROW_COLUMNS: &str = "seq, kind, key, asset_id, revision, retired, stamp, origin, applied";

/// Rows after `since`, oldest first, at most `limit`.
pub fn page(db: &Db, since: u64, limit: u32) -> ServerResult<Vec<SyncRow>> {
    let mut s = db.prepare(
        "sync log page",
        &format!("SELECT {ROW_COLUMNS} FROM sync_log WHERE seq > ?1 ORDER BY seq LIMIT ?2"),
    )?;
    s.bind_u64(1, since)?;
    s.bind_u64(2, limit as u64)?;
    let mut rows = Vec::new();
    while s.step()? {
        rows.push(read_row(&s)?);
    }
    Ok(rows)
}

/// Every row of one key, newest first: the head's history.
pub fn history(db: &Db, kind: i64, key: &str, limit: u32) -> ServerResult<Vec<SyncRow>> {
    let mut s = db.prepare(
        "sync log history",
        &format!("SELECT {ROW_COLUMNS} FROM sync_log WHERE kind = ?1 AND key = ?2 ORDER BY stamp DESC, origin DESC LIMIT ?3"),
    )?;
    s.bind_i64(1, kind)?;
    s.bind_text(2, key)?;
    s.bind_u64(3, limit as u64)?;
    let mut rows = Vec::new();
    while s.step()? {
        rows.push(read_row(&s)?);
    }
    Ok(rows)
}

/// The newest seq (0 for an empty log).
pub fn last_seq(db: &Db) -> ServerResult<u64> {
    let mut s = db.prepare("sync log last", "SELECT COALESCE(MAX(seq), 0) FROM sync_log")?;
    Ok(if s.step()? { s.column_u64(0) } else { 0 })
}

/// A peer's pull position (the last of its rows this store has applied).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerState {
    pub cursor: u64,
    pub peer_store: Option<[u8; 16]>,
    pub last_ok_ms: Option<u64>,
    pub last_error: Option<String>,
}

pub fn peer_state(db: &Db, peer: &str) -> ServerResult<PeerState> {
    let mut s = db.prepare(
        "sync peer get",
        "SELECT cursor, peer_store, last_ok_ms, last_error FROM sync_peers WHERE peer = ?1",
    )?;
    s.bind_text(1, peer)?;
    if !s.step()? {
        return Ok(PeerState::default());
    }
    Ok(PeerState {
        cursor: s.column_u64(0),
        peer_store: if s.column_is_null(1) { None } else { Some(fixed16(&s.column_blob(1), "sync peer store")?) },
        last_ok_ms: if s.column_is_null(2) { None } else { Some(s.column_u64(2)) },
        last_error: if s.column_is_null(3) { None } else { Some(s.column_text(3)) },
    })
}

/// Saves a peer's position. A peer whose store id changed (another store
/// now answers at that address) starts over from 0: its seqs are another
/// log's.
pub fn set_peer_state(db: &Db, peer: &str, state: &PeerState) -> ServerResult<()> {
    let mut s = db.prepare(
        "sync peer set",
        "INSERT INTO sync_peers(peer, cursor, peer_store, last_ok_ms, last_error) VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(peer) DO UPDATE SET cursor = ?2, peer_store = ?3, last_ok_ms = ?4, last_error = ?5",
    )?;
    s.bind_text(1, peer)?;
    s.bind_u64(2, state.cursor)?;
    match &state.peer_store {
        Some(id) => s.bind_blob(3, id)?,
        None => s.bind_null(3)?,
    }
    match state.last_ok_ms {
        Some(ms) => s.bind_u64(4, ms)?,
        None => s.bind_null(4)?,
    }
    match &state.last_error {
        Some(e) => s.bind_text(5, e)?,
        None => s.bind_null(5)?,
    }
    s.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_order_by_clock_then_origin() {
        let a = Stamp { clock: 5, origin: [9; 16] };
        let b = Stamp { clock: 6, origin: [0; 16] };
        let c = Stamp { clock: 6, origin: [1; 16] };
        assert!(a < b && b < c);
    }

    #[test]
    fn asset_keys_are_lowercase_hex() {
        let id = AssetId::from_bytes([0xab; 16]);
        assert_eq!(asset_key(&id), "ab".repeat(16));
    }
}
