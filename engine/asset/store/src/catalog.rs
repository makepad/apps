//! SQLite catalog: immutable asset/game revisions, candidates, aliases, refs.
//!
//! Rules enforced here, all fail-closed:
//! - Revision rows are immutable and content-addressed: the row key IS the
//!   SHA-256 of the stored canonical manifest bytes, so re-insertion is
//!   idempotent by construction and conflicting content is impossible.
//! - Every blob a manifest references must already be recorded (which means
//!   it was committed to the CAS first); a manifest can never point at bytes
//!   the store does not hold.
//! - Candidate lifecycle: staged -> published, staged/published -> quarantined,
//!   quarantined is terminal. No other transition exists. RETIREMENT reuses
//!   that terminal state rather than inventing a parallel one: retiring a
//!   revision performs exactly the quarantine transition (with its alias
//!   teardown and search refresh) and additionally stamps `retired_ms`, which
//!   is the deletion intent blob GC reads. Retiring an ASSET stamps
//!   `assets.retired_ms` and retires every revision it has, in one
//!   transaction, cost proportional to that asset — never to the catalog.
//! - Aliases are the only mutable pointers, and may only point at PUBLISHED
//!   revisions whose namespace matches the alias namespace. That invariant is
//!   maintained, not just admitted: quarantining a revision drops every alias
//!   head pointing at it in the same transaction.
//! - Every alias-head mutation (set, clear, quarantine drop) recomputes the
//!   search index's alias state (live flag, canonical alias, alias posting
//!   terms) for the affected assets and advances the search index generation,
//!   all in the mutating transaction.
//! - Game revisions pin exact asset revisions (the refs table); staging one
//!   requires every pinned asset revision to be published.

use crate::budget::Budgets;
use crate::error::{ServerError, ServerResult};
use crate::sqlite::Db;
use makepad_asset_data::limits::MAX_DOCUMENT_BYTES;
use makepad_asset_data::{
    AssetAlias, AssetId, AssetManifest, AssetRevisionId, AssetRevisionRef, BlobId, ContentLock,
    GameAlias, GameId, GameRevisionId, GameRevisionManifest,
};

/// Compare-and-swap on one alias, never an inferred global head for an asset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PublishExpectedHead {
    #[default]
    Any,
    /// No alias binding and no prior publication for the supplied asset ID.
    Absent,
    /// The alias must still name this exact asset and revision.
    Exact(AssetRevisionRef),
}

/// What retirement keeps aside so `Catalog::restore_asset` can undo it:
/// the alias heads it dropped and the annotation it removed from search.
/// Additive tables, created on every open by both the server and the
/// embedded core WITHOUT a schema version step: an older build never reads
/// them and still opens the store (a version bump would lock older builds
/// out of the user's library for two side tables).
pub const RETIREMENT_KEEP_SCHEMA: &str = "
-- The alias heads an asset retirement dropped, so restoring the asset can
-- rebind every one that is still free. Not a head: nothing resolves
-- through this table.
CREATE TABLE IF NOT EXISTS retired_asset_aliases(
    asset_id BLOB NOT NULL,
    alias TEXT NOT NULL,
    head_revision BLOB NOT NULL,
    PRIMARY KEY(asset_id, alias)
);
-- The annotation a retired asset had, kept OUTSIDE every search table so a
-- retired asset still costs no query anything, and so restoring the asset
-- (`Catalog::restore_asset`) brings its title, labels and provenance back
-- instead of an untitled row. Labels are space-joined (a label is
-- [a-z0-9_-] only). The row leaves again when the asset is restored.
CREATE TABLE IF NOT EXISTS retired_annotations(
    asset_id BLOB PRIMARY KEY,
    kind TEXT,
    visibility TEXT NOT NULL,
    owner BLOB,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    creator TEXT NOT NULL,
    artist TEXT NOT NULL,
    artist_url TEXT NOT NULL,
    album TEXT NOT NULL,
    source_url TEXT NOT NULL,
    license TEXT NOT NULL,
    license_url TEXT NOT NULL,
    generator TEXT NOT NULL,
    backend TEXT NOT NULL,
    model TEXT NOT NULL,
    prompt TEXT NOT NULL,
    provenance TEXT NOT NULL,
    categories TEXT NOT NULL,
    tags TEXT NOT NULL
);
";

pub const CATALOG_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS blobs(
    blob_id BLOB PRIMARY KEY,
    size INTEGER NOT NULL,
    created_ms INTEGER NOT NULL,
    -- When a later upload of the SAME bytes deduped against this row. The
    -- blob is old but the intent to reference it is fresh, so blob GC's
    -- grace horizon must see that recency (schema v9).
    last_ref_ms INTEGER
);
CREATE TABLE IF NOT EXISTS assets(
    asset_id BLOB PRIMARY KEY,
    namespace TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    -- Asset-level terminal state (schema v9). NULL = live. A retired asset
    -- and every one of its revisions leave every listing, alias and search
    -- row; the identity row survives so the id can never be reused.
    retired_ms INTEGER
);
CREATE TABLE IF NOT EXISTS asset_revisions(
    revision BLOB PRIMARY KEY,
    asset_id BLOB NOT NULL,
    manifest BLOB NOT NULL,
    created_ms INTEGER NOT NULL
);
-- 'Every revision of this asset, newest first' is what retirement and the
-- retention rule walk; without this index each is a full scan of every
-- revision in the store (schema v9).
CREATE INDEX IF NOT EXISTS asset_revisions_by_asset
    ON asset_revisions(asset_id, created_ms, revision);
-- Reverse edges of manifest `dependencies` (schema v16): one row per
-- (dependency asset, dependency revision, depending revision). 'Which assets
-- use this one' (the REMOVE guard) reads it by asset; 'does a live revision
-- still pin this exact revision' (retention, serving a retired dependency)
-- reads it by revision. Without it both are a decode of every manifest.
CREATE TABLE IF NOT EXISTS asset_dependency_refs(
    dep_asset_id BLOB NOT NULL,
    dep_revision BLOB NOT NULL,
    revision BLOB NOT NULL,
    PRIMARY KEY(dep_asset_id, dep_revision, revision)
);
CREATE INDEX IF NOT EXISTS asset_dependency_refs_by_revision
    ON asset_dependency_refs(dep_revision);
CREATE TABLE IF NOT EXISTS candidates(
    kind TEXT NOT NULL CHECK(kind IN ('asset','game')),
    owner_id BLOB NOT NULL,
    revision BLOB NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('staged','published','quarantined')),
    staged_ms INTEGER NOT NULL,
    published_ms INTEGER,
    quarantined_ms INTEGER,
    -- Retirement (schema v9) is quarantine PLUS a deletion intent: the
    -- state column stays the terminal 'quarantined' the lifecycle already
    -- defines, and this timestamp says the bytes may be collected. NULL on
    -- a merely quarantined (pulled, still stored) revision.
    retired_ms INTEGER,
    PRIMARY KEY(kind, owner_id, revision)
);
CREATE TABLE IF NOT EXISTS asset_aliases(
    alias TEXT PRIMARY KEY,
    asset_id BLOB NOT NULL,
    head_revision BLOB NOT NULL,
    updated_ms INTEGER NOT NULL
);
-- Reverse direction of the alias table. Every alias-head mutation asks
-- 'which aliases point at this asset' three times (liveness EXISTS, the
-- canonical MIN(alias), the posting rebuild) and quarantine deletes by
-- (asset_id, head_revision); without this index each of those is a full
-- scan of every alias in the store.
CREATE INDEX IF NOT EXISTS asset_aliases_by_asset
    ON asset_aliases(asset_id, alias);
CREATE TABLE IF NOT EXISTS games(
    game_id BLOB PRIMARY KEY,
    namespace TEXT NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS game_revisions(
    revision BLOB PRIMARY KEY,
    game_id BLOB NOT NULL,
    manifest BLOB NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS game_revision_refs(
    game_revision BLOB NOT NULL,
    asset_id BLOB NOT NULL,
    asset_revision BLOB NOT NULL,
    PRIMARY KEY(game_revision, asset_id)
);
CREATE TABLE IF NOT EXISTS game_aliases(
    alias TEXT PRIMARY KEY,
    game_id BLOB NOT NULL,
    head_revision BLOB NOT NULL,
    updated_ms INTEGER NOT NULL
);
-- Same reverse direction for games: quarantine drops alias heads by
-- (game_id, head_revision).
CREATE INDEX IF NOT EXISTS game_aliases_by_game
    ON game_aliases(game_id, alias);
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateState {
    Staged,
    Published,
    Quarantined,
}

impl CandidateState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Published => "published",
            Self::Quarantined => "quarantined",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "staged" => Some(Self::Staged),
            "published" => Some(Self::Published),
            "quarantined" => Some(Self::Quarantined),
            _ => None,
        }
    }
}

/// Server-side namespace rule. A namespace is exactly one alias segment as
/// the content contract defines it (an alias's `namespace()` is its first
/// segment), so the charset here mirrors `check_segments`: starts with a
/// lowercase letter or digit, then lowercase/digit/`-`/`_`.
pub fn validate_namespace(ns: &str) -> ServerResult<()> {
    if ns.is_empty() || ns.len() > 64 {
        return Err(ServerError::InvalidInput { what: "namespace length" });
    }
    let bytes = ns.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(ServerError::InvalidInput { what: "namespace charset" });
    }
    if !bytes.iter().all(|&b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
    }) {
        return Err(ServerError::InvalidInput { what: "namespace charset" });
    }
    Ok(())
}

pub struct Catalog<'a> {
    pub(crate) db: &'a Db,
    pub(crate) budgets: &'a Budgets,
}

impl<'a> Catalog<'a> {
    // ---- blobs -------------------------------------------------------------

    /// Record a CAS-committed blob. Idempotent for the same size; a size
    /// conflict on the same digest is corruption and refuses.
    ///
    /// A repeat record of bytes the store already holds (a deduped upload)
    /// refreshes `last_ref_ms`: the row is old, but someone just uploaded
    /// these bytes intending to reference them, and blob GC's grace horizon
    /// must not collect them out from under that publish.
    pub fn record_blob(&self, blob_id: &BlobId, size: u64, now_ms: u64) -> ServerResult<()> {
        if let Some(existing) = self.blob_size(blob_id)? {
            if existing != size {
                return Err(ServerError::SizeMismatch {
                    what: "blob record",
                    expected: existing,
                    found: size,
                });
            }
            let mut s = self.db.prepare(
                "touch blob",
                "UPDATE blobs SET last_ref_ms = ?2
                 WHERE blob_id = ?1 AND (last_ref_ms IS NULL OR last_ref_ms < ?2)",
            )?;
            s.bind_blob(1, blob_id.as_bytes())?;
            s.bind_u64(2, now_ms)?;
            s.run()?;
            return Ok(());
        }
        let mut s = self.db.prepare(
            "record blob",
            "INSERT INTO blobs(blob_id, size, created_ms) VALUES(?1, ?2, ?3)",
        )?;
        s.bind_blob(1, blob_id.as_bytes())?;
        s.bind_u64(2, size)?;
        s.bind_u64(3, now_ms)?;
        s.run()
    }

    pub fn blob_size(&self, blob_id: &BlobId) -> ServerResult<Option<u64>> {
        let mut s = self
            .db
            .prepare("blob size", "SELECT size FROM blobs WHERE blob_id = ?1")?;
        s.bind_blob(1, blob_id.as_bytes())?;
        if s.step()? {
            Ok(Some(s.column_u64(0)))
        } else {
            Ok(None)
        }
    }

    pub fn has_blob(&self, blob_id: &BlobId) -> ServerResult<bool> {
        Ok(self.blob_size(blob_id)?.is_some())
    }

    fn require_blob(&self, blob_id: &BlobId, what: &'static str) -> ServerResult<()> {
        if self.has_blob(blob_id)? {
            Ok(())
        } else {
            Err(ServerError::NotFound { what })
        }
    }

    /// A manifest's declared byte length must equal the size the CAS recorded
    /// for that digest. A manifest that lies about a size it names is refused
    /// at admission, so clients can budget downloads from manifests alone.
    pub(crate) fn require_blob_sized(
        &self,
        blob_id: &BlobId,
        declared: u64,
        missing: &'static str,
        mismatch: &'static str,
    ) -> ServerResult<()> {
        let recorded = self
            .blob_size(blob_id)?
            .ok_or(ServerError::NotFound { what: missing })?;
        if recorded != declared {
            return Err(ServerError::SizeMismatch {
                what: mismatch,
                expected: declared,
                found: recorded,
            });
        }
        Ok(())
    }

    // ---- assets ------------------------------------------------------------

    /// Register an asset identity. Idempotent for the same namespace;
    /// conflicting namespace refuses.
    pub fn register_asset(&self, asset_id: &AssetId, namespace: &str, now_ms: u64) -> ServerResult<()> {
        validate_namespace(namespace)?;
        if let Some(existing) = self.asset_namespace(asset_id)? {
            if existing != namespace {
                return Err(ServerError::Conflict { what: "asset namespace" });
            }
            // Re-registering a retired identity would be resurrection by the
            // back door; the row stays, refused.
            if self.asset_retired_ms(asset_id)?.is_some() {
                return Err(ServerError::InvalidState { what: "asset", state: "retired" });
            }
            return Ok(());
        }
        let mut s = self.db.prepare(
            "register asset",
            "INSERT INTO assets(asset_id, namespace, created_ms) VALUES(?1, ?2, ?3)",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_text(2, namespace)?;
        s.bind_u64(3, now_ms)?;
        s.run()
    }

    pub fn asset_namespace(&self, asset_id: &AssetId) -> ServerResult<Option<String>> {
        let mut s = self.db.prepare(
            "asset namespace",
            "SELECT namespace FROM assets WHERE asset_id = ?1",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        if s.step()? {
            Ok(Some(s.column_text(0)))
        } else {
            Ok(None)
        }
    }

    /// When this asset was retired, or `None` for a live (or unknown) asset.
    /// One statement, so it is safe to call inside a caller's transaction
    /// (a nested BEGIN would refuse).
    pub fn asset_retired_ms(&self, asset_id: &AssetId) -> ServerResult<Option<u64>> {
        asset_retired_in(self.db, asset_id.as_bytes())
    }

    /// Admit an asset revision from canonical manifest bytes: decode/validate
    /// through the content contract, require every referenced blob and
    /// dependency revision to exist, then store the immutable revision row and
    /// a staged candidate. Fully idempotent while the candidate is staged.
    pub fn stage_asset_revision(&self, manifest_bytes: &[u8], now_ms: u64) -> ServerResult<AssetRevisionId> {
        self.db
            .tx(|db| self.stage_asset_revision_in_tx(db, manifest_bytes, now_ms))
    }

    /// The body of [`Self::stage_asset_revision`] inside the caller's open
    /// transaction, so a multi-asset operation (pack import) can stage every
    /// revision atomically with its own records.
    pub(crate) fn stage_asset_revision_in_tx(
        &self,
        db: &Db,
        manifest_bytes: &[u8],
        now_ms: u64,
    ) -> ServerResult<AssetRevisionId> {
        if manifest_bytes.len() as u64 > self.budgets.max_manifest_bytes {
            return Err(ServerError::OverBudget {
                what: "asset manifest bytes",
                limit: self.budgets.max_manifest_bytes,
                found: manifest_bytes.len() as u64,
            });
        }
        let manifest = AssetManifest::from_canonical_bytes(manifest_bytes)?;
        let revision = AssetRevisionId::hash_of(manifest_bytes);
        if self.asset_namespace(&manifest.asset_id)?.is_none() {
            return Err(ServerError::NotFound { what: "asset for revision" });
        }
        // Retirement is terminal for the identity, not just for the
        // revisions that existed when it happened: a retired asset never
        // gains new content (which blob GC would then have to keep).
        if asset_retired_in(db, manifest.asset_id.as_bytes())?.is_some() {
            return Err(ServerError::InvalidState { what: "asset", state: "retired" });
        }
        // Every referenced blob must exist AND be exactly the size the
        // manifest declares for it; a lying byte_len is refused here.
        for f in &manifest.files {
            self.require_blob_sized(&f.blob, f.byte_len, "asset file blob", "asset file blob size")?;
        }
        if let Some(t) = &manifest.thumbnail {
            self.require_blob_sized(
                &t.blob,
                t.byte_len,
                "asset thumbnail blob",
                "asset thumbnail blob size",
            )?;
        }
        for dep in &manifest.dependencies {
            let bytes = self.asset_revision_manifest(&dep.revision)?
                .ok_or(ServerError::NotFound { what: "asset dependency revision" })?;
            if AssetManifest::from_canonical_bytes(&bytes)?.asset_id != dep.asset_id {
                return Err(ServerError::Conflict { what: "asset dependency identity" });
            }
            // Retirement is terminal: a retired revision's bytes may already
            // be collected, so a new revision may not start depending on it.
            // (Existing dependents keep it alive; see blob GC.)
            if self.revision_retired_in_tx(db, &dep.asset_id, &dep.revision)?
                || asset_retired_in(db, dep.asset_id.as_bytes())?.is_some()
            {
                return Err(ServerError::InvalidState { what: "asset dependency", state: "retired" });
            }
        }
        let mut s = db.prepare(
            "insert asset revision",
            "INSERT OR IGNORE INTO asset_revisions(revision, asset_id, manifest, created_ms)
             VALUES(?1, ?2, ?3, ?4)",
        )?;
        s.bind_blob(1, revision.as_bytes())?;
        s.bind_blob(2, manifest.asset_id.as_bytes())?;
        s.bind_blob(3, manifest_bytes)?;
        s.bind_u64(4, now_ms)?;
        s.run()?;
        drop(s);
        insert_dependency_refs(db, &revision, &manifest)?;
        self.stage_candidate("asset", manifest.asset_id.as_bytes(), revision.as_bytes(), now_ms)?;
        // A blob GC run in flight decided its referenced set before this
        // manifest existed: pin what we just made reachable, in THIS
        // transaction, so the sweep can never delete bytes a committed
        // manifest names.
        crate::gc::pin_asset_in_tx(db, &manifest)?;
        Ok(revision)
    }

    pub fn asset_revision_manifest(&self, revision: &AssetRevisionId) -> ServerResult<Option<Vec<u8>>> {
        let mut s = self.db.prepare(
            "asset revision manifest",
            "SELECT manifest FROM asset_revisions WHERE revision = ?1",
        )?;
        s.bind_blob(1, revision.as_bytes())?;
        if s.step()? {
            Ok(Some(s.column_blob(0)))
        } else {
            Ok(None)
        }
    }

    // ---- candidates --------------------------------------------------------

    fn stage_candidate(&self, kind: &'static str, owner: &[u8], revision: &[u8], now_ms: u64) -> ServerResult<()> {
        match self.candidate_state_raw(kind, owner, revision)? {
            // Re-staging identical content is a no-op only while staged;
            // published/quarantined content cannot be re-admitted.
            Some(CandidateState::Staged) => Ok(()),
            Some(CandidateState::Published) => Err(ServerError::InvalidState {
                what: "candidate",
                state: "published",
            }),
            Some(CandidateState::Quarantined) => Err(ServerError::InvalidState {
                what: "candidate",
                state: "quarantined",
            }),
            None => {
                let mut s = self.db.prepare(
                    "stage candidate",
                    "INSERT INTO candidates(kind, owner_id, revision, state, staged_ms)
                     VALUES(?1, ?2, ?3, 'staged', ?4)",
                )?;
                s.bind_text(1, kind)?;
                s.bind_blob(2, owner)?;
                s.bind_blob(3, revision)?;
                s.bind_u64(4, now_ms)?;
                s.run()
            }
        }
    }

    fn candidate_state_raw(&self, kind: &str, owner: &[u8], revision: &[u8]) -> ServerResult<Option<CandidateState>> {
        let mut s = self.db.prepare(
            "candidate state",
            "SELECT state FROM candidates WHERE kind = ?1 AND owner_id = ?2 AND revision = ?3",
        )?;
        s.bind_text(1, kind)?;
        s.bind_blob(2, owner)?;
        s.bind_blob(3, revision)?;
        if s.step()? {
            let state = s.column_text(0);
            CandidateState::parse(&state)
                .map(Some)
                .ok_or(ServerError::InvalidState { what: "candidate row", state: "unknown" })
        } else {
            Ok(None)
        }
    }

    /// Apply one candidate state transition inside the caller's transaction,
    /// so lifecycle side effects (alias drops, search liveness) land
    /// atomically with the state change.
    pub(crate) fn transition_in_tx(
        &self,
        db: &Db,
        kind: &'static str,
        owner: &[u8],
        revision: &[u8],
        allowed_from: &[CandidateState],
        to: CandidateState,
        now_ms: u64,
    ) -> ServerResult<()> {
        let cur = self
            .candidate_state_raw(kind, owner, revision)?
            .ok_or(ServerError::NotFound { what: "candidate" })?;
        if cur == to {
            // Idempotent repeat of a landed transition.
            return Ok(());
        }
        if !allowed_from.contains(&cur) {
            return Err(ServerError::InvalidState {
                what: "candidate transition",
                state: cur.as_str(),
            });
        }
        let sql = match to {
            CandidateState::Published => {
                "UPDATE candidates SET state='published', published_ms=?4
                 WHERE kind=?1 AND owner_id=?2 AND revision=?3"
            }
            CandidateState::Quarantined => {
                "UPDATE candidates SET state='quarantined', quarantined_ms=?4
                 WHERE kind=?1 AND owner_id=?2 AND revision=?3"
            }
            CandidateState::Staged => {
                return Err(ServerError::InvalidState {
                    what: "candidate transition",
                    state: "staged is admission-only",
                })
            }
        };
        let mut s = db.prepare("candidate transition", sql)?;
        s.bind_text(1, kind)?;
        s.bind_blob(2, owner)?;
        s.bind_blob(3, revision)?;
        s.bind_u64(4, now_ms)?;
        s.run()
    }

    /// Recompute the search index's alias-derived state (live flag, canonical
    /// alias, alias posting terms) for up to two assets, inside the caller's
    /// transaction, then advance the index generation so every open keyset
    /// cursor fails closed as stale. Assets without an annotation simply have
    /// no rows to update.
    fn refresh_search_live(&self, db: &Db, asset_a: &[u8], asset_b: &[u8]) -> ServerResult<()> {
        crate::search::refresh_alias_state(db, self.budgets, asset_a)?;
        if asset_b != asset_a {
            crate::search::refresh_alias_state(db, self.budgets, asset_b)?;
        }
        crate::search::bump_generation(db)
    }

    pub fn asset_candidate_state(&self, asset_id: &AssetId, revision: &AssetRevisionId) -> ServerResult<Option<CandidateState>> {
        self.candidate_state_raw("asset", asset_id.as_bytes(), revision.as_bytes())
    }

    /// Whether revision-addressed reads (manifest, thumbnail) may serve this
    /// revision. Staged and published revisions are served; a quarantined
    /// (pulled) one is not. A RETIRED revision that a live revision still
    /// depends on is served: the edit that pins it keeps playing, and the
    /// REMOVE that retired it has already been told it breaks that edit.
    pub fn asset_revision_servable(&self, asset_id: &AssetId, revision: &AssetRevisionId) -> ServerResult<bool> {
        Ok(match self.asset_candidate_state(asset_id, revision)? {
            None => false,
            Some(CandidateState::Staged | CandidateState::Published) => true,
            Some(CandidateState::Quarantined) => {
                self.revision_retired_in_tx(self.db, asset_id, revision)?
                    && pinned_by_live_dependent(self.db, revision)?
            }
        })
    }

    pub fn publish_asset(&self, asset_id: &AssetId, revision: &AssetRevisionId, now_ms: u64) -> ServerResult<()> {
        self.db.tx(|db| {
            self.transition_in_tx(
                db,
                "asset",
                asset_id.as_bytes(),
                revision.as_bytes(),
                &[CandidateState::Staged],
                CandidateState::Published,
                now_ms,
            )
        })
    }

    /// Quarantine a revision AND tear down every alias head pointing at it,
    /// atomically. Quarantine is the emergency pull: it always succeeds from
    /// staged/published, and no mutable pointer keeps resolving to pulled
    /// content afterwards. Search liveness for the asset flips in the same
    /// transaction.
    pub fn quarantine_asset(&self, asset_id: &AssetId, revision: &AssetRevisionId, now_ms: u64) -> ServerResult<()> {
        self.db.tx(|db| {
            self.transition_in_tx(
                db,
                "asset",
                asset_id.as_bytes(),
                revision.as_bytes(),
                &[CandidateState::Staged, CandidateState::Published],
                CandidateState::Quarantined,
                now_ms,
            )?;
            note_dropped_heads_in_tx(db, asset_id, revision, now_ms)?;
            let mut s = db.prepare(
                "drop quarantined alias heads",
                "DELETE FROM asset_aliases WHERE asset_id = ?1 AND head_revision = ?2",
            )?;
            s.bind_blob(1, asset_id.as_bytes())?;
            s.bind_blob(2, revision.as_bytes())?;
            s.run()?;
            drop(s);
            self.refresh_search_live(db, asset_id.as_bytes(), asset_id.as_bytes())
        })
    }

    // ---- retirement (deletion) ---------------------------------------------

    /// Retire ONE revision: the quarantine transition (alias heads pointing
    /// at it drop, search alias state refreshes) plus the deletion intent
    /// blob GC reads. Idempotent — retiring an already retired revision is a
    /// no-op that reports `false`.
    ///
    /// Cost: O(aliases of the asset), all indexed. Nothing here scales with
    /// the catalog.
    pub fn retire_revision(
        &self,
        asset_id: &AssetId,
        revision: &AssetRevisionId,
        now_ms: u64,
    ) -> ServerResult<bool> {
        self.db
            .tx(|db| self.retire_revision_in_tx(db, asset_id, revision, now_ms))
    }

    /// The body of [`Self::retire_revision`] inside the caller's open
    /// transaction, so asset retirement and the retention rule can retire
    /// many revisions atomically with their own bookkeeping.
    pub(crate) fn retire_revision_in_tx(
        &self,
        db: &Db,
        asset_id: &AssetId,
        revision: &AssetRevisionId,
        now_ms: u64,
    ) -> ServerResult<bool> {
        let state = self
            .candidate_state_raw("asset", asset_id.as_bytes(), revision.as_bytes())?
            .ok_or(ServerError::NotFound { what: "candidate" })?;
        if self.revision_retired_in_tx(db, asset_id, revision)? {
            return Ok(false);
        }
        if state != CandidateState::Quarantined {
            self.transition_in_tx(
                db,
                "asset",
                asset_id.as_bytes(),
                revision.as_bytes(),
                &[CandidateState::Staged, CandidateState::Published],
                CandidateState::Quarantined,
                now_ms,
            )?;
        }
        let mut s = db.prepare(
            "retire revision",
            "UPDATE candidates SET retired_ms = ?3
             WHERE kind='asset' AND owner_id = ?1 AND revision = ?2",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_blob(2, revision.as_bytes())?;
        s.bind_u64(3, now_ms)?;
        s.run()?;
        drop(s);
        note_dropped_heads_in_tx(db, asset_id, revision, now_ms)?;
        let mut s = db.prepare(
            "drop retired alias heads",
            "DELETE FROM asset_aliases WHERE asset_id = ?1 AND head_revision = ?2",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_blob(2, revision.as_bytes())?;
        s.run()?;
        drop(s);
        self.refresh_search_live(db, asset_id.as_bytes(), asset_id.as_bytes())?;
        Ok(true)
    }

    /// Retire an ASSET: every revision it has becomes terminal and
    /// collectable, every alias head pointing at it disappears, and its
    /// whole search footprint (annotation, labels, postings, alias postings)
    /// is deleted — a retired asset is not filtered out of search, it is
    /// absent from it, so no query pays for its existence.
    ///
    /// Idempotent: re-retiring reports `already_retired`. Cost: O(revisions
    /// + aliases + index rows OF THIS ASSET), every one of them an indexed
    /// delete; independent of catalog size.
    pub fn retire_asset(&self, asset_id: &AssetId, now_ms: u64) -> ServerResult<RetireReport> {
        self.db.tx(|db| {
            if self.asset_namespace(asset_id)?.is_none() {
                return Err(ServerError::NotFound { what: "asset" });
            }
            if asset_retired_in(db, asset_id.as_bytes())?.is_some() {
                return Ok(RetireReport { already_retired: true, ..RetireReport::default() });
            }
            let mut s = db.prepare(
                "retire asset",
                "UPDATE assets SET retired_ms = ?2 WHERE asset_id = ?1",
            )?;
            s.bind_blob(1, asset_id.as_bytes())?;
            s.bind_u64(2, now_ms)?;
            s.run()?;
            drop(s);
            // One statement for every revision of this asset: the same
            // terminal transition quarantine performs, applied to the whole
            // set (the PK's (kind, owner_id) prefix is the index).
            let mut s = db.prepare(
                "retire asset revisions",
                "UPDATE candidates
                    SET state='quarantined',
                        quarantined_ms=COALESCE(quarantined_ms, ?2),
                        retired_ms=?2
                  WHERE kind='asset' AND owner_id = ?1 AND retired_ms IS NULL",
            )?;
            s.bind_blob(1, asset_id.as_bytes())?;
            s.bind_u64(2, now_ms)?;
            s.run()?;
            drop(s);
            let revisions_retired = db.changes();
            // Keep what is about to be dropped (alias heads, annotation) out
            // of every live table, so `restore_asset` can bring it back.
            let mut s = db.prepare(
                "keep retired asset aliases",
                "INSERT OR REPLACE INTO retired_asset_aliases(asset_id, alias, head_revision)
                 SELECT asset_id, alias, head_revision FROM asset_aliases WHERE asset_id = ?1",
            )?;
            s.bind_blob(1, asset_id.as_bytes())?;
            s.run()?;
            drop(s);
            crate::search::stash_annotation_in_tx(db, asset_id.as_bytes())?;
            let mut s = db.prepare(
                "drop retired asset aliases",
                "DELETE FROM asset_aliases WHERE asset_id = ?1",
            )?;
            s.bind_blob(1, asset_id.as_bytes())?;
            s.run()?;
            drop(s);
            let aliases_dropped = db.changes();
            crate::sync_log::note_asset_state_in_tx(db, asset_id, true, now_ms)?;
            let annotation_cleared =
                crate::search::clear_annotation_in_tx(db, asset_id.as_bytes())?;
            // The alias postings are gone with the annotation, but the live
            // flag/canonical alias of any row that survives (none, by
            // construction) still has to be consistent, and every open
            // search cursor must fail closed after a catalog change.
            self.refresh_search_live(db, asset_id.as_bytes(), asset_id.as_bytes())?;
            Ok(RetireReport {
                already_retired: false,
                revisions_retired,
                aliases_dropped,
                annotation_cleared,
            })
        })
    }

    /// Undo [`Self::retire_asset`]: the asset is live again, with every
    /// revision that retirement took (the ones it quarantined, i.e. whose
    /// `quarantined_ms` is the retirement's own timestamp) back in the state
    /// it had before — published or staged. A revision that was already
    /// pulled (quarantined earlier) or retired earlier (retention, an
    /// explicit revision retire) stays as it was: restoring an asset never
    /// resurrects content someone pulled on purpose.
    ///
    /// What retirement dropped comes back: the annotation and labels it kept
    /// aside (search rows rebuilt), and every alias head it dropped that is
    /// still free and whose revision is published again.
    ///
    /// Dependencies: the restored revisions pin exact revisions of other
    /// assets (an edit's clips, motion documents, audio). Any of those that
    /// are retired are restored too, transitively — a whole retired asset by
    /// this same operation, a single revision retired on its own when its
    /// retirement was what quarantined it — so the edit plays again instead
    /// of leaning on "retired but pinned" bytes.
    ///
    /// Refuses (and changes nothing) when a revision it would restore names
    /// a blob that is no longer stored: after blob GC collected a retired
    /// asset there is nothing to restore. A GC run in flight gets the
    /// restored manifests pinned in this transaction, exactly as a publish
    /// does, so the sweep cannot collect them afterwards.
    ///
    /// `authorize` is asked with the namespace of every asset before it is
    /// restored (the asked-for one and each dependency); an error from it
    /// refuses the whole restore and changes nothing. A route passes the
    /// caller's capability check, so restoring an edit cannot revive
    /// another namespace's content its caller could not revive itself.
    ///
    /// Idempotent: restoring a live asset reports `already_live` and does
    /// nothing. Cost: O(revisions + aliases + dependencies) of the assets it
    /// restores, all indexed.
    pub fn restore_asset(
        &self,
        asset_id: &AssetId,
        now_ms: u64,
        authorize: &mut dyn FnMut(&str) -> ServerResult<()>,
    ) -> ServerResult<RestoreReport> {
        self.db.tx(|db| {
            if self.asset_namespace(asset_id)?.is_none() {
                return Err(ServerError::NotFound { what: "asset" });
            }
            if asset_retired_in(db, asset_id.as_bytes())?.is_none() {
                return Ok(RestoreReport { already_live: true, restored: Vec::new() });
            }
            let mut restored: Vec<RestoredAsset> = Vec::new();
            let mut seen_assets = std::collections::HashSet::new();
            let mut seen_revisions = std::collections::HashSet::new();
            let mut work = vec![RestoreWork::Asset(*asset_id)];
            while let Some(item) = work.pop() {
                // Revisions whose dependencies must be walked next.
                let walk = match item {
                    RestoreWork::Asset(id) => {
                        if !seen_assets.insert(id) {
                            continue;
                        }
                        let Some(retired_ms) = asset_retired_in(db, id.as_bytes())? else {
                            continue;
                        };
                        authorize(&self.asset_namespace(&id)?.ok_or(ServerError::NotFound { what: "asset" })?)?;
                        let row = self.restore_one_asset_in_tx(db, &id, retired_ms)?;
                        // A whole asset coming back is a head change; a lone
                        // revision restored as a dependency moves none.
                        crate::sync_log::note_asset_state_in_tx(db, &id, false, now_ms)?;
                        let walk = row.revisions.clone();
                        restored.push(row);
                        walk
                    }
                    RestoreWork::Revision(dep) => {
                        if !seen_revisions.insert(dep.revision) {
                            continue;
                        }
                        if !self.restore_one_revision_in_tx(db, &dep, authorize)? {
                            continue;
                        }
                        if let Some(row) = restored.iter_mut().find(|r| r.asset_id == dep.asset_id) {
                            row.revisions.push(dep.revision);
                        } else {
                            restored.push(RestoredAsset {
                                asset_id: dep.asset_id,
                                revisions: vec![dep.revision],
                                aliases_restored: 0,
                                annotation_restored: false,
                            });
                        }
                        vec![dep.revision]
                    }
                };
                for revision in walk {
                    let manifest = self.restored_manifest_in_tx(db, &revision)?;
                    for dep in &manifest.dependencies {
                        // The revision after the asset: the stack pops the
                        // asset first, so a revision retired together with
                        // its asset is already back when its own item runs.
                        work.push(RestoreWork::Revision(*dep));
                        work.push(RestoreWork::Asset(dep.asset_id));
                    }
                }
            }
            // Alias heads and annotations go back last, once every revision
            // they may point at is published again.
            for row in &mut restored {
                row.aliases_restored = self.restore_aliases_in_tx(db, &row.asset_id, now_ms)?;
                if let Some(ann) = crate::search::take_stashed_annotation_in_tx(db, row.asset_id.as_bytes())? {
                    crate::search::Search { db, budgets: self.budgets }
                        .set_annotation_in_tx(db, &row.asset_id, &ann, now_ms)?;
                    row.annotation_restored = true;
                }
                self.refresh_search_live(db, row.asset_id.as_bytes(), row.asset_id.as_bytes())?;
            }
            Ok(RestoreReport { already_live: false, restored })
        })
    }

    /// Bring one retired asset back: clear its retirement and restore the
    /// revisions that retirement quarantined. Returns the restored row (the
    /// revisions it made servable again).
    fn restore_one_asset_in_tx(
        &self,
        db: &Db,
        asset_id: &AssetId,
        retired_ms: u64,
    ) -> ServerResult<RestoredAsset> {
        let mut s = db.prepare(
            "restore asset revisions list",
            "SELECT revision FROM candidates
              WHERE kind='asset' AND owner_id = ?1 AND retired_ms = ?2 AND quarantined_ms = ?2
              ORDER BY staged_ms, revision",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_u64(2, retired_ms)?;
        let mut revisions = Vec::new();
        while s.step()? {
            revisions.push(AssetRevisionId::from_bytes(fixed32(&s.column_blob(0), "restore revision row")?));
        }
        drop(s);
        let mut s = db.prepare("restore asset", "UPDATE assets SET retired_ms = NULL WHERE asset_id = ?1")?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.run()?;
        drop(s);
        for revision in &revisions {
            self.unquarantine_revision_in_tx(db, asset_id, revision)?;
        }
        Ok(RestoredAsset {
            asset_id: *asset_id,
            revisions,
            aliases_restored: 0,
            annotation_restored: false,
        })
    }

    /// Restore ONE revision a restored revision depends on, when it is still
    /// retired and its retirement is what quarantined it. Returns whether it
    /// changed. A revision pulled before it was retired stays retired: it
    /// keeps being served to its dependents as a pinned retired revision.
    fn restore_one_revision_in_tx(
        &self,
        db: &Db,
        dep: &AssetRevisionRef,
        authorize: &mut dyn FnMut(&str) -> ServerResult<()>,
    ) -> ServerResult<bool> {
        if asset_retired_in(db, dep.asset_id.as_bytes())?.is_some() {
            return Ok(false);
        }
        let mut s = db.prepare(
            "restore revision check",
            "SELECT 1 FROM candidates
              WHERE kind='asset' AND owner_id = ?1 AND revision = ?2
                AND retired_ms IS NOT NULL AND quarantined_ms = retired_ms",
        )?;
        s.bind_blob(1, dep.asset_id.as_bytes())?;
        s.bind_blob(2, dep.revision.as_bytes())?;
        let restorable = s.step()?;
        drop(s);
        if restorable {
            authorize(&self.asset_namespace(&dep.asset_id)?.ok_or(ServerError::NotFound { what: "asset" })?)?;
            self.unquarantine_revision_in_tx(db, &dep.asset_id, &dep.revision)?;
        }
        Ok(restorable)
    }

    /// The candidate row goes back to the state it had before retirement
    /// quarantined it: published when it had been published, else staged.
    /// Its blobs must still be stored; a GC run in flight pins them.
    fn unquarantine_revision_in_tx(
        &self,
        db: &Db,
        asset_id: &AssetId,
        revision: &AssetRevisionId,
    ) -> ServerResult<()> {
        let manifest = self.restored_manifest_in_tx(db, revision)?;
        let blobs = manifest
            .files
            .iter()
            .map(|f| f.blob)
            .chain(manifest.thumbnail.as_ref().map(|t| t.blob));
        for blob in blobs {
            if self.blob_size(&blob)?.is_none() {
                return Err(ServerError::InvalidState { what: "restore asset blob", state: "collected" });
            }
        }
        let mut s = db.prepare(
            "restore revision",
            "UPDATE candidates
                SET state = CASE WHEN published_ms IS NULL THEN 'staged' ELSE 'published' END,
                    quarantined_ms = NULL,
                    retired_ms = NULL
              WHERE kind='asset' AND owner_id = ?1 AND revision = ?2",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_blob(2, revision.as_bytes())?;
        s.run()?;
        drop(s);
        crate::gc::pin_asset_in_tx(db, &manifest)
    }

    fn restored_manifest_in_tx(&self, db: &Db, revision: &AssetRevisionId) -> ServerResult<AssetManifest> {
        let mut s = db.prepare(
            "restore manifest",
            "SELECT manifest FROM asset_revisions WHERE revision = ?1",
        )?;
        s.bind_blob(1, revision.as_bytes())?;
        if !s.step()? {
            return Err(ServerError::NotFound { what: "restore revision manifest" });
        }
        Ok(AssetManifest::from_canonical_bytes(&s.column_blob(0))?)
    }

    /// Rebind the alias heads retirement kept aside, where the alias is still
    /// free and its revision is published again; the kept rows go either way.
    fn restore_aliases_in_tx(&self, db: &Db, asset_id: &AssetId, now_ms: u64) -> ServerResult<u64> {
        let mut s = db.prepare(
            "kept aliases",
            "SELECT alias, head_revision FROM retired_asset_aliases WHERE asset_id = ?1 ORDER BY alias",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        let mut kept = Vec::new();
        while s.step()? {
            kept.push((s.column_text(0), fixed32(&s.column_blob(1), "kept alias row")?));
        }
        drop(s);
        let mut restored = 0u64;
        for (text, head) in kept {
            let Ok(alias) = AssetAlias::new(text) else { continue };
            if self.resolve_asset_alias(&alias)?.is_some() {
                continue;
            }
            let revision = AssetRevisionId::from_bytes(head);
            if self.asset_candidate_state(asset_id, &revision)? != Some(CandidateState::Published) {
                continue;
            }
            self.set_asset_alias_in_tx(db, &alias, &AssetRevisionRef { asset_id: *asset_id, revision }, now_ms)?;
            restored += 1;
        }
        let mut s = db.prepare("drop kept aliases", "DELETE FROM retired_asset_aliases WHERE asset_id = ?1")?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.run()?;
        Ok(restored)
    }

    /// Every candidate revision of one asset, oldest staged first: the
    /// lifecycle view the detail route serves. Reads the candidates table
    /// through its `(kind, owner_id)` primary-key prefix, so the cost is
    /// this asset's revisions and nothing else.
    ///
    /// This is the TRUTH about revision state. The transport keeps a mirror
    /// for browse listings, but any state that can change without a route
    /// (the GC retention rule retires revisions inside the core) must be
    /// read from here or a client would see a stale lifecycle.
    pub fn asset_candidates(
        &self,
        asset_id: &AssetId,
        limit: u32,
    ) -> ServerResult<Vec<CandidateRow>> {
        let mut s = self.db.prepare(
            "asset candidates",
            "SELECT revision, state, staged_ms, published_ms, quarantined_ms, retired_ms
             FROM candidates WHERE kind='asset' AND owner_id = ?1
             ORDER BY staged_ms, revision LIMIT ?2",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_u64(2, limit as u64)?;
        let mut out = Vec::new();
        while s.step()? {
            let state = CandidateState::parse(&s.column_text(1))
                .ok_or(ServerError::InvalidState { what: "candidate row", state: "unknown" })?;
            let opt = |s: &crate::sqlite::Stmt<'_>, i: i32| {
                if s.column_is_null(i) { None } else { Some(s.column_u64(i)) }
            };
            out.push(CandidateRow {
                revision: AssetRevisionId::from_bytes(fixed32(
                    &s.column_blob(0),
                    "candidate row",
                )?),
                state,
                staged_ms: s.column_u64(2),
                published_ms: opt(&s, 3),
                quarantined_ms: opt(&s, 4),
                retired_ms: opt(&s, 5),
            });
        }
        Ok(out)
    }

    /// Whether one revision carries the deletion intent.
    pub fn revision_retired(
        &self,
        asset_id: &AssetId,
        revision: &AssetRevisionId,
    ) -> ServerResult<bool> {
        self.revision_retired_in_tx(self.db, asset_id, revision)
    }

    pub(crate) fn revision_retired_in_tx(
        &self,
        db: &Db,
        asset_id: &AssetId,
        revision: &AssetRevisionId,
    ) -> ServerResult<bool> {
        let mut s = db.prepare(
            "revision retired",
            "SELECT retired_ms FROM candidates
             WHERE kind='asset' AND owner_id = ?1 AND revision = ?2",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_blob(2, revision.as_bytes())?;
        if !s.step()? {
            return Ok(false);
        }
        Ok(!s.column_is_null(0))
    }

    /// The revisions of one asset the retention rule would retire: newest
    /// first, keep `keep`, and never touch a revision an alias head still
    /// points at — retention trims history, it never pulls what is being
    /// served, however old that head is.
    ///
    /// The `asset_revisions_by_asset` index provides the ordering, so this
    /// reads only this asset's rows.
    ///
    /// Nor does it retire a revision that another live revision depends on:
    /// an edit pins the exact clip revision it cut, and a republished clip
    /// must not break it. `sim_run` is a dry run's id, whose simulated
    /// retirements count as retired dependents (so a preview predicts what
    /// the real run, visiting assets in the same order, then does); a real
    /// run passes `None`.
    pub(crate) fn superseded_revisions_in_tx(
        &self,
        db: &Db,
        asset_id: &AssetId,
        keep: u32,
        sim_run: Option<i64>,
    ) -> ServerResult<Vec<AssetRevisionId>> {
        let mut s = db.prepare(
            "superseded revisions",
            "SELECT q.revision FROM (
                 SELECT r.revision AS revision, r.asset_id AS asset_id
                 FROM asset_revisions r
                 JOIN candidates c
                   ON c.kind='asset' AND c.owner_id = r.asset_id AND c.revision = r.revision
                 WHERE r.asset_id = ?1 AND c.retired_ms IS NULL
                 ORDER BY r.created_ms DESC, r.revision DESC
                 LIMIT -1 OFFSET ?2
             ) q
             WHERE NOT EXISTS(SELECT 1 FROM asset_aliases a
                              WHERE a.asset_id = q.asset_id AND a.head_revision = q.revision)
               AND NOT EXISTS(
                   SELECT 1 FROM asset_dependency_refs d
                   JOIN asset_revisions dr ON dr.revision = d.revision
                   JOIN candidates dc
                     ON dc.kind='asset' AND dc.owner_id = dr.asset_id AND dc.revision = dr.revision
                   JOIN assets da ON da.asset_id = dr.asset_id
                   WHERE d.dep_revision = q.revision
                     AND dc.retired_ms IS NULL AND da.retired_ms IS NULL
                     AND NOT EXISTS(SELECT 1 FROM gc_retain_sim g
                                    WHERE g.run_id = ?3 AND g.revision = d.revision))",
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_u64(2, keep as u64)?;
        s.bind_i64(3, sim_run.unwrap_or(-1))?;
        let mut out = Vec::new();
        while s.step()? {
            out.push(AssetRevisionId::from_bytes(fixed32(
                &s.column_blob(0),
                "superseded revision row",
            )?));
        }
        Ok(out)
    }

    // ---- asset aliases -----------------------------------------------------

    /// Point an alias at a published revision. Aliases are the only mutable
    /// heads in the catalog; namespace must match the asset's.
    pub fn set_asset_alias(
        &self,
        alias: &AssetAlias,
        target: &AssetRevisionRef,
        now_ms: u64,
    ) -> ServerResult<()> {
        self.db
            .tx(|db| self.set_asset_alias_in_tx(db, alias, target, now_ms))
    }

    /// The body of [`Self::set_asset_alias`] inside the caller's open
    /// transaction. Every alias-head mutation still funnels through
    /// `refresh_search_live` here, so the search invariant holds for
    /// composite operations too.
    pub(crate) fn set_asset_alias_in_tx(
        &self,
        db: &Db,
        alias: &AssetAlias,
        target: &AssetRevisionRef,
        now_ms: u64,
    ) -> ServerResult<()> {
        {
            let ns = self
                .asset_namespace(&target.asset_id)?
                .ok_or(ServerError::NotFound { what: "alias target asset" })?;
            if alias.namespace() != ns {
                return Err(ServerError::Conflict { what: "alias namespace" });
            }
            match self.asset_candidate_state(&target.asset_id, &target.revision)? {
                Some(CandidateState::Published) => {}
                Some(_) => {
                    return Err(ServerError::InvalidState {
                        what: "alias target",
                        state: "not published",
                    })
                }
                None => return Err(ServerError::NotFound { what: "alias target revision" }),
            }
            // The previous target (if this is a retarget) may lose its last
            // alias here; its search liveness must flip in this transaction.
            let mut s = db.prepare(
                "alias old target",
                "SELECT asset_id FROM asset_aliases WHERE alias = ?1",
            )?;
            s.bind_text(1, alias.as_str())?;
            let old_target = if s.step()? {
                Some(fixed16(&s.column_blob(0), "asset alias row")?)
            } else {
                None
            };
            drop(s);
            let mut s = db.prepare(
                "set asset alias",
                "INSERT INTO asset_aliases(alias, asset_id, head_revision, updated_ms)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(alias) DO UPDATE
                 SET asset_id=?2, head_revision=?3, updated_ms=?4",
            )?;
            s.bind_text(1, alias.as_str())?;
            s.bind_blob(2, target.asset_id.as_bytes())?;
            s.bind_blob(3, target.revision.as_bytes())?;
            s.bind_u64(4, now_ms)?;
            s.run()?;
            drop(s);
            crate::sync_log::note_alias_in_tx(db, alias.as_str(), &target.asset_id, Some(&target.revision), now_ms)?;
            // Transactional search reindex on alias-head mutation: recompute
            // the live flag for both affected assets from the alias table.
            self.refresh_search_live(
                db,
                target.asset_id.as_bytes(),
                old_target.as_ref().map(|o| &o[..]).unwrap_or(target.asset_id.as_bytes()),
            )
        }
    }

    /// Remove an alias head. Idempotent: returns whether the alias existed.
    /// The unpinned asset's search liveness flips in the same transaction.
    pub fn clear_asset_alias(&self, alias: &AssetAlias, now_ms: u64) -> ServerResult<bool> {
        self.db.tx(|db| {
            let mut s = db.prepare(
                "clear alias old target",
                "SELECT asset_id FROM asset_aliases WHERE alias = ?1",
            )?;
            s.bind_text(1, alias.as_str())?;
            if !s.step()? {
                return Ok(false);
            }
            let old_target = fixed16(&s.column_blob(0), "asset alias row")?;
            drop(s);
            let mut s = db.prepare(
                "clear asset alias",
                "DELETE FROM asset_aliases WHERE alias = ?1",
            )?;
            s.bind_text(1, alias.as_str())?;
            s.run()?;
            drop(s);
            crate::sync_log::note_alias_in_tx(db, alias.as_str(), &AssetId::from_bytes(old_target), None, now_ms)?;
            self.refresh_search_live(db, &old_target, &old_target)?;
            Ok(true)
        })
    }

    pub fn resolve_asset_alias(&self, alias: &AssetAlias) -> ServerResult<Option<AssetRevisionRef>> {
        let mut s = self.db.prepare(
            "resolve asset alias",
            "SELECT asset_id, head_revision FROM asset_aliases WHERE alias = ?1",
        )?;
        s.bind_text(1, alias.as_str())?;
        if !s.step()? {
            return Ok(None);
        }
        Ok(Some(AssetRevisionRef {
            asset_id: AssetId::from_bytes(fixed16(&s.column_blob(0), "asset alias row")?),
            revision: AssetRevisionId::from_bytes(fixed32(&s.column_blob(1), "asset alias row")?),
        }))
    }

    /// Check inside the publication transaction before any alias, annotation
    /// or search write. `true` means the requested target is already current
    /// and published: a lost-success retry must be a complete no-op.
    pub(crate) fn check_publish_head_in_tx(
        &self,
        alias: Option<&AssetAlias>,
        target: &AssetRevisionRef,
        expected: PublishExpectedHead,
    ) -> ServerResult<bool> {
        if expected == PublishExpectedHead::Any {
            return Ok(false);
        }
        let alias = alias.ok_or(ServerError::InvalidInput {
            what: "guarded publish requires alias",
        })?;
        if let PublishExpectedHead::Exact(previous) = expected {
            if previous.asset_id != target.asset_id {
                return Err(ServerError::Conflict { what: "publish expected asset identity" });
            }
        }
        let current = self.resolve_asset_alias(alias)?;
        if current.as_ref() == Some(target)
            && self.asset_candidate_state(&target.asset_id, &target.revision)?
                == Some(CandidateState::Published)
        {
            return Ok(true);
        }
        let matches = match expected {
            PublishExpectedHead::Any => unreachable!(),
            PublishExpectedHead::Exact(previous) => current == Some(previous),
            PublishExpectedHead::Absent => {
                if current.is_some() {
                    false
                } else {
                    // No timestamp ordering, alias inference, or candidate
                    // page limit: even an unaliased published revision counts.
                    let mut stmt = self.db.prepare(
                        "publish expected absent",
                        "SELECT 1 FROM candidates WHERE kind='asset' AND owner_id=?1
                         AND published_ms IS NOT NULL LIMIT 1",
                    )?;
                    stmt.bind_blob(1, target.asset_id.as_bytes())?;
                    !stmt.step()?
                }
            }
        };
        if !matches {
            return Err(ServerError::Conflict { what: "publish expected alias head" });
        }
        Ok(false)
    }

    // ---- games -------------------------------------------------------------

    pub fn register_game(&self, game_id: &GameId, namespace: &str, now_ms: u64) -> ServerResult<()> {
        validate_namespace(namespace)?;
        if let Some(existing) = self.game_namespace(game_id)? {
            if existing != namespace {
                return Err(ServerError::Conflict { what: "game namespace" });
            }
            return Ok(());
        }
        let mut s = self.db.prepare(
            "register game",
            "INSERT INTO games(game_id, namespace, created_ms) VALUES(?1, ?2, ?3)",
        )?;
        s.bind_blob(1, game_id.as_bytes())?;
        s.bind_text(2, namespace)?;
        s.bind_u64(3, now_ms)?;
        s.run()
    }

    pub fn game_namespace(&self, game_id: &GameId) -> ServerResult<Option<String>> {
        let mut s = self.db.prepare(
            "game namespace",
            "SELECT namespace FROM games WHERE game_id = ?1",
        )?;
        s.bind_blob(1, game_id.as_bytes())?;
        if s.step()? {
            Ok(Some(s.column_text(0)))
        } else {
            Ok(None)
        }
    }

    /// Admit a game revision. Requires the lock bytes so the pin set can be
    /// verified: the manifest's lock_blob must be the digest of `lock_bytes`,
    /// the lock's game_id must match, and every closure entry must be a
    /// PUBLISHED asset revision. Pins land in the refs table.
    pub fn stage_game_revision(
        &self,
        manifest_bytes: &[u8],
        lock_bytes: &[u8],
        now_ms: u64,
    ) -> ServerResult<GameRevisionId> {
        if manifest_bytes.len() as u64 > self.budgets.max_manifest_bytes {
            return Err(ServerError::OverBudget {
                what: "game manifest bytes",
                limit: self.budgets.max_manifest_bytes,
                found: manifest_bytes.len() as u64,
            });
        }
        let manifest = GameRevisionManifest::from_canonical_bytes(manifest_bytes)?;
        let lock = ContentLock::from_canonical_bytes(lock_bytes)?;
        let lock_digest = BlobId::hash_of(lock_bytes);
        if manifest.lock_blob != lock_digest {
            return Err(ServerError::DigestMismatch {
                what: "game lock blob",
                expected: *manifest.lock_blob.as_bytes(),
                found: *lock_digest.as_bytes(),
            });
        }
        if lock.game_id != manifest.game_id {
            return Err(ServerError::Conflict { what: "lock game_id" });
        }
        let revision = GameRevisionId::hash_of(manifest_bytes);
        self.db.tx(|db| {
            if self.game_namespace(&manifest.game_id)?.is_none() {
                return Err(ServerError::NotFound { what: "game for revision" });
            }
            // Declared byte lengths must equal the recorded CAS sizes. The
            // manifest.toml blob declares no length in the contract, so it is
            // existence-checked only; the lock's exact bytes are in hand, so
            // its recorded size must match them.
            self.require_blob_sized(
                &manifest.splash_blob,
                manifest.splash_byte_len,
                "game splash blob",
                "game splash blob size",
            )?;
            self.require_blob(&manifest.manifest_blob, "game manifest.toml blob")?;
            self.require_blob_sized(
                &manifest.lock_blob,
                lock_bytes.len() as u64,
                "game lock blob",
                "game lock blob size",
            )?;
            self.require_blob_sized(
                &manifest.thumbnail.blob,
                manifest.thumbnail.byte_len,
                "game thumbnail blob",
                "game thumbnail blob size",
            )?;
            // A pinned catalog snapshot must exist and fit the contract's
            // canonical-document bound; it declares no exact length, so the
            // bound is the only size contract available for it.
            if let Some(snapshot) = &manifest.catalog_snapshot {
                let size = self
                    .blob_size(snapshot)?
                    .ok_or(ServerError::NotFound { what: "game catalog snapshot blob" })?;
                if size == 0 {
                    return Err(ServerError::InvalidInput { what: "game catalog snapshot empty" });
                }
                if size > MAX_DOCUMENT_BYTES as u64 {
                    return Err(ServerError::OverBudget {
                        what: "game catalog snapshot bytes",
                        limit: MAX_DOCUMENT_BYTES as u64,
                        found: size,
                    });
                }
            }
            for r in &lock.closure {
                match self.asset_candidate_state(&r.asset_id, &r.revision)? {
                    Some(CandidateState::Published) => {}
                    Some(_) => {
                        return Err(ServerError::InvalidState {
                            what: "pinned asset revision",
                            state: "not published",
                        })
                    }
                    None => return Err(ServerError::NotFound { what: "pinned asset revision" }),
                }
            }
            let mut s = db.prepare(
                "insert game revision",
                "INSERT OR IGNORE INTO game_revisions(revision, game_id, manifest, created_ms)
                 VALUES(?1, ?2, ?3, ?4)",
            )?;
            s.bind_blob(1, revision.as_bytes())?;
            s.bind_blob(2, manifest.game_id.as_bytes())?;
            s.bind_blob(3, manifest_bytes)?;
            s.bind_u64(4, now_ms)?;
            s.run()?;
            drop(s);
            for r in &lock.closure {
                let mut s = db.prepare(
                    "insert game ref",
                    "INSERT OR IGNORE INTO game_revision_refs(game_revision, asset_id, asset_revision)
                     VALUES(?1, ?2, ?3)",
                )?;
                s.bind_blob(1, revision.as_bytes())?;
                s.bind_blob(2, r.asset_id.as_bytes())?;
                s.bind_blob(3, r.revision.as_bytes())?;
                s.run()?;
            }
            self.stage_candidate("game", manifest.game_id.as_bytes(), revision.as_bytes(), now_ms)?;
            // Same pin rule as asset revisions: a game revision makes its own
            // bytes reachable, so an in-flight GC run must learn about them
            // inside this transaction.
            let mut pinned = vec![
                manifest.splash_blob,
                manifest.manifest_blob,
                manifest.lock_blob,
                manifest.thumbnail.blob,
            ];
            if let Some(snapshot) = &manifest.catalog_snapshot {
                pinned.push(*snapshot);
            }
            crate::gc::pin_blobs_in_tx(db, &pinned)?;
            Ok(revision)
        })
    }

    pub fn game_candidate_state(&self, game_id: &GameId, revision: &GameRevisionId) -> ServerResult<Option<CandidateState>> {
        self.candidate_state_raw("game", game_id.as_bytes(), revision.as_bytes())
    }

    /// Exact canonical bytes for a stored game revision. Lifecycle filtering
    /// belongs to the transport, just like [`Self::asset_revision_manifest`]:
    /// internal recovery code may need to inspect a quarantined candidate,
    /// while public reads must make quarantined content indistinguishable
    /// from absent content.
    pub fn game_revision_manifest(
        &self,
        revision: &GameRevisionId,
    ) -> ServerResult<Option<Vec<u8>>> {
        let mut s = self.db.prepare(
            "game revision manifest",
            "SELECT manifest FROM game_revisions WHERE revision = ?1",
        )?;
        s.bind_blob(1, revision.as_bytes())?;
        if s.step()? {
            Ok(Some(s.column_blob(0)))
        } else {
            Ok(None)
        }
    }

    pub fn publish_game(&self, game_id: &GameId, revision: &GameRevisionId, now_ms: u64) -> ServerResult<()> {
        self.db.tx(|db| {
            self.transition_in_tx(
                db,
                "game",
                game_id.as_bytes(),
                revision.as_bytes(),
                &[CandidateState::Staged],
                CandidateState::Published,
                now_ms,
            )
        })
    }

    /// Quarantine a game revision AND drop every game alias head pointing at
    /// it, atomically — pulled games stop resolving immediately.
    pub fn quarantine_game(&self, game_id: &GameId, revision: &GameRevisionId, now_ms: u64) -> ServerResult<()> {
        self.db.tx(|db| {
            self.transition_in_tx(
                db,
                "game",
                game_id.as_bytes(),
                revision.as_bytes(),
                &[CandidateState::Staged, CandidateState::Published],
                CandidateState::Quarantined,
                now_ms,
            )?;
            let mut s = db.prepare(
                "drop quarantined game alias heads",
                "DELETE FROM game_aliases WHERE game_id = ?1 AND head_revision = ?2",
            )?;
            s.bind_blob(1, game_id.as_bytes())?;
            s.bind_blob(2, revision.as_bytes())?;
            s.run()
        })
    }

    /// The exact asset revisions a game revision pins, canonical order.
    pub fn game_revision_refs(&self, revision: &GameRevisionId) -> ServerResult<Vec<AssetRevisionRef>> {
        let mut s = self.db.prepare(
            "game revision refs",
            "SELECT asset_id, asset_revision FROM game_revision_refs
             WHERE game_revision = ?1 ORDER BY asset_id",
        )?;
        s.bind_blob(1, revision.as_bytes())?;
        let mut out = Vec::new();
        while s.step()? {
            out.push(AssetRevisionRef {
                asset_id: AssetId::from_bytes(fixed16(&s.column_blob(0), "game ref row")?),
                revision: AssetRevisionId::from_bytes(fixed32(&s.column_blob(1), "game ref row")?),
            });
        }
        Ok(out)
    }

    pub fn set_game_alias(
        &self,
        alias: &GameAlias,
        game_id: &GameId,
        revision: &GameRevisionId,
        now_ms: u64,
    ) -> ServerResult<()> {
        self.db.tx(|db| {
            let ns = self
                .game_namespace(game_id)?
                .ok_or(ServerError::NotFound { what: "alias target game" })?;
            if alias.namespace() != ns {
                return Err(ServerError::Conflict { what: "game alias namespace" });
            }
            match self.game_candidate_state(game_id, revision)? {
                Some(CandidateState::Published) => {}
                Some(_) => {
                    return Err(ServerError::InvalidState {
                        what: "game alias target",
                        state: "not published",
                    })
                }
                None => return Err(ServerError::NotFound { what: "game alias target revision" }),
            }
            let mut s = db.prepare(
                "set game alias",
                "INSERT INTO game_aliases(alias, game_id, head_revision, updated_ms)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(alias) DO UPDATE
                 SET game_id=?2, head_revision=?3, updated_ms=?4",
            )?;
            s.bind_text(1, alias.as_str())?;
            s.bind_blob(2, game_id.as_bytes())?;
            s.bind_blob(3, revision.as_bytes())?;
            s.bind_u64(4, now_ms)?;
            s.run()
        })
    }

    /// Remove a game alias head. Idempotent: returns whether it existed.
    pub fn clear_game_alias(&self, alias: &GameAlias) -> ServerResult<bool> {
        self.db.tx(|db| {
            let mut s = db.prepare(
                "clear game alias",
                "DELETE FROM game_aliases WHERE alias = ?1",
            )?;
            s.bind_text(1, alias.as_str())?;
            s.run()?;
            drop(s);
            Ok(db.changes() > 0)
        })
    }

    pub fn resolve_game_alias(&self, alias: &GameAlias) -> ServerResult<Option<(GameId, GameRevisionId)>> {
        let mut s = self.db.prepare(
            "resolve game alias",
            "SELECT game_id, head_revision FROM game_aliases WHERE alias = ?1",
        )?;
        s.bind_text(1, alias.as_str())?;
        if !s.step()? {
            return Ok(None);
        }
        Ok(Some((
            GameId::from_bytes(fixed16(&s.column_blob(0), "game alias row")?),
            GameRevisionId::from_bytes(fixed32(&s.column_blob(1), "game alias row")?),
        )))
    }
}

/// One candidate revision's full lifecycle row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateRow {
    pub revision: AssetRevisionId,
    pub state: CandidateState,
    pub staged_ms: u64,
    pub published_ms: Option<u64>,
    pub quarantined_ms: Option<u64>,
    /// Set once the revision was retired: terminal AND collectable.
    pub retired_ms: Option<u64>,
}

/// What one asset retirement removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetireReport {
    /// The asset was already retired; nothing changed.
    pub already_retired: bool,
    pub revisions_retired: u64,
    pub aliases_dropped: u64,
    pub annotation_cleared: bool,
}

/// What one asset restore brought back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// The asset was not retired; nothing changed.
    pub already_live: bool,
    /// The asset itself first, then every retired dependency restored with
    /// it, in the order they were restored.
    pub restored: Vec<RestoredAsset>,
}

/// One asset a restore made live again (or one of whose revisions it
/// restored, for a dependency that was live with a retired revision).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredAsset {
    pub asset_id: AssetId,
    /// The revisions made servable again, oldest staged first.
    pub revisions: Vec<AssetRevisionId>,
    pub aliases_restored: u64,
    pub annotation_restored: bool,
}

enum RestoreWork {
    Asset(AssetId),
    Revision(AssetRevisionRef),
}

/// Read `assets.retired_ms` inside the caller's transaction.
pub(crate) fn asset_retired_in(db: &Db, asset_id: &[u8]) -> ServerResult<Option<u64>> {
    let mut s = db.prepare(
        "asset retired",
        "SELECT retired_ms FROM assets WHERE asset_id = ?1",
    )?;
    s.bind_blob(1, asset_id)?;
    if !s.step()? || s.column_is_null(0) {
        return Ok(None);
    }
    Ok(Some(s.column_u64(0)))
}

/// Record `revision`'s dependency edges in the reverse index. Idempotent,
/// like the revision row it accompanies.
fn insert_dependency_refs(db: &Db, revision: &AssetRevisionId, manifest: &AssetManifest) -> ServerResult<()> {
    for dep in &manifest.dependencies {
        let mut s = db.prepare(
            "insert dependency ref",
            "INSERT OR IGNORE INTO asset_dependency_refs(dep_asset_id, dep_revision, revision)
             VALUES(?1, ?2, ?3)",
        )?;
        s.bind_blob(1, dep.asset_id.as_bytes())?;
        s.bind_blob(2, dep.revision.as_bytes())?;
        s.bind_blob(3, revision.as_bytes())?;
        s.run()?;
    }
    Ok(())
}

/// Whether a live (non-retired) revision of a live asset depends on exactly
/// `revision`. Such a revision is kept by retention and blob GC, and its
/// manifest keeps being served after it is retired, so the edits that pin it
/// keep playing (shown as using a removed asset). One index probe.
pub(crate) fn pinned_by_live_dependent(db: &Db, revision: &AssetRevisionId) -> ServerResult<bool> {
    let mut s = db.prepare(
        "pinned by live dependent",
        "SELECT 1 FROM asset_dependency_refs d
         JOIN asset_revisions dr ON dr.revision = d.revision
         JOIN candidates dc
           ON dc.kind='asset' AND dc.owner_id = dr.asset_id AND dc.revision = dr.revision
         JOIN assets da ON da.asset_id = dr.asset_id
         WHERE d.dep_revision = ?1 AND dc.retired_ms IS NULL AND da.retired_ms IS NULL
         LIMIT 1",
    )?;
    s.bind_blob(1, revision.as_bytes())?;
    s.step()
}

/// Fill `asset_dependency_refs` from the stored manifests of every
/// non-retired revision (the v16 migration). Retired revisions are skipped:
/// every reader of the index asks about LIVE dependents, and a retired
/// dependent never comes back. A manifest that no longer decodes under this
/// build's rules is logged and skipped rather than failing the migration,
/// which would leave the store unopenable. Keyset pages of 256 revisions,
/// inside the caller's migration transaction; returns the edges written.
pub(crate) fn backfill_dependency_refs(db: &Db) -> ServerResult<u64> {
    const PAGE: u64 = 256;
    let mut cursor: Vec<u8> = Vec::new();
    let mut edges = 0u64;
    loop {
        let mut s = db.prepare(
            "dependency backfill page",
            "SELECT r.revision, r.manifest, c.retired_ms IS NULL AND a.retired_ms IS NULL
             FROM asset_revisions r
             LEFT JOIN candidates c
               ON c.kind='asset' AND c.owner_id = r.asset_id AND c.revision = r.revision
             LEFT JOIN assets a ON a.asset_id = r.asset_id
             WHERE r.revision > ?1
             ORDER BY r.revision LIMIT ?2",
        )?;
        s.bind_blob(1, &cursor)?;
        s.bind_u64(2, PAGE)?;
        let mut rows: Vec<(Vec<u8>, Vec<u8>, bool)> = Vec::new();
        while s.step()? {
            // A revision with no candidate row counts as live (fail closed
            // towards keeping, as blob GC does).
            let live = s.column_is_null(2) || s.column_i64(2) != 0;
            rows.push((s.column_blob(0), s.column_blob(1), live));
        }
        drop(s);
        for (revision, manifest, live) in &rows {
            if !*live {
                continue;
            }
            let revision = AssetRevisionId::from_bytes(fixed32(revision, "dependency backfill revision")?);
            match AssetManifest::from_canonical_bytes(manifest) {
                Ok(manifest) => {
                    insert_dependency_refs(db, &revision, &manifest)?;
                    edges += manifest.dependencies.len() as u64;
                }
                Err(error) => eprintln!(
                    "[asset-server] dependency index: revision {revision} skipped, its manifest does not decode: {error}"
                ),
            }
        }
        if (rows.len() as u64) < PAGE {
            return Ok(edges);
        }
        cursor = rows.pop().map(|(last, _, _)| last).unwrap_or_default();
    }
}

/// A stored ID column that is not exactly its fixed width is catalog
/// corruption; refuse rather than truncate or pad.
/// Logs every alias head about to be dropped because it points at a
/// revision being pulled (quarantine, revision retirement).
fn note_dropped_heads_in_tx(db: &Db, asset_id: &AssetId, revision: &AssetRevisionId, now_ms: u64) -> ServerResult<()> {
    let mut s = db.prepare(
        "aliases at a dropped head",
        "SELECT alias FROM asset_aliases WHERE asset_id = ?1 AND head_revision = ?2 ORDER BY alias",
    )?;
    s.bind_blob(1, asset_id.as_bytes())?;
    s.bind_blob(2, revision.as_bytes())?;
    let mut aliases = Vec::new();
    while s.step()? {
        aliases.push(s.column_text(0));
    }
    drop(s);
    for alias in aliases {
        crate::sync_log::note_alias_in_tx(db, &alias, asset_id, None, now_ms)?;
    }
    Ok(())
}

pub(crate) fn fixed32(bytes: &[u8], what: &'static str) -> ServerResult<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| ServerError::SizeMismatch { what, expected: 32, found: bytes.len() as u64 })
}

pub(crate) fn fixed16(bytes: &[u8], what: &'static str) -> ServerResult<[u8; 16]> {
    bytes
        .try_into()
        .map_err(|_| ServerError::SizeMismatch { what, expected: 16, found: bytes.len() as u64 })
}
