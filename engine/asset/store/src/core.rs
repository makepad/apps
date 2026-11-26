//! Portable, transport-free catalog core.
//!
//! This layer owns only the synchronous SQLite working set and budgets. It
//! contains no filesystem, HTTP, thread, watcher, or browser-storage code.

use crate::auth::{Auth, PrincipalId};
use crate::budget::Budgets;
use crate::catalog::{CandidateRow, CandidateState, Catalog, PublishExpectedHead, CATALOG_SCHEMA};
use crate::error::{content_err_at, ServerError, ServerResult};
use crate::gc::{Gc, GC_SCHEMA};
use crate::imports::{Imports, IMPORT_SCHEMA};
use crate::search::{kind_parse, AssetAnnotation, Search, SEARCH_SCHEMA};
use crate::sqlite::Db;
use crate::variants::{Variants, VARIANT_SCHEMA};
use makepad_asset_data::{
    AssetAlias, AssetId, AssetKind, AssetManifest, AssetRevisionId, AssetRevisionRef,
};

pub const SERVER_SCHEMA_VERSION: i64 = 24;

// GC keeps reference-blob handling fail-closed even though embedded mode can
// never create one. Keeping the empty table in the portable schema avoids a
// target-specific SQL branch in the collector.
const EMPTY_BLOBREF_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS blob_refs(
    blob_id BLOB PRIMARY KEY,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ms INTEGER,
    recorded_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS blob_refs_by_path ON blob_refs(path);
";

fn table_has_column(db: &Db, table: &str, column: &str) -> ServerResult<bool> {
    let mut stmt = db.prepare("table info", &format!("PRAGMA table_info({table})"))?;
    while stmt.step()? {
        if stmt.column_text(1) == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub struct CatalogCore {
    pub(crate) db: Db,
    pub(crate) budgets: Budgets,
}

/// One live asset whose head depends on another (see
/// [`CatalogCore::dependents`]). `public`/`owner` come from the annotation,
/// so a route can withhold a private title from everyone but its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependent {
    pub asset_id: AssetId,
    pub revision: AssetRevisionId,
    pub kind: AssetKind,
    pub title: String,
    pub public: bool,
    pub owner: Option<PrincipalId>,
}

/// One page of [`CatalogCore::dependents`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependentsPage {
    /// Every live dependent, not just this page's.
    pub total: u64,
    pub rows: Vec<Dependent>,
    /// Pass as `after` for the next page; `None` on the last one.
    pub next: Option<AssetId>,
}

impl CatalogCore {
    /// Open a fresh in-memory catalog through SQ1's `MemoryStoreSet`.
    pub fn open_memory(budgets: Budgets) -> ServerResult<Self> {
        budgets.validate()?;
        let timeout = budgets.db_busy_timeout_ms;
        let db = Db::open_memory(timeout)?;
        Self::initialize(db, budgets)
    }

    /// Open a catalog on a caller-supplied SQ1 page-store set.
    pub fn open_with<S: makepad_sqlite::PageStoreSet + 'static>(
        stores: S,
        budgets: Budgets,
    ) -> ServerResult<Self> {
        budgets.validate()?;
        let timeout = budgets.db_busy_timeout_ms;
        let db = Db::open_with(stores, timeout)?;
        Self::initialize(db, budgets)
    }

    #[cfg(all(feature = "native", not(any(target_arch = "wasm32", feature = "embedded"))))]
    pub(crate) fn from_parts(db: Db, budgets: Budgets) -> Self {
        Self { db, budgets }
    }

    fn initialize(db: Db, budgets: Budgets) -> ServerResult<Self> {
        // Embedded catalogs have one synchronous owner and use rollback
        // journal mode. Browser durability is outside the pager in E2.
        db.exec("set rollback journal", "PRAGMA journal_mode=DELETE")?;
        db.exec("set synchronous", "PRAGMA synchronous=FULL")?;
        db.exec("set foreign keys", "PRAGMA foreign_keys=ON")?;
        let version = {
            let mut stmt = db.prepare("get user_version", "PRAGMA user_version")?;
            if stmt.step()? { stmt.column_i64(0) } else { 0 }
        };
        if version != 0 && !(13..=SERVER_SCHEMA_VERSION).contains(&version) {
            return Err(ServerError::UnsupportedSchema { found: version });
        }
        db.tx(|db| {
            db.exec("create catalog schema", CATALOG_SCHEMA)?;
            db.exec("create retirement keep schema", crate::catalog::RETIREMENT_KEEP_SCHEMA)?;
            db.exec("create auth schema", crate::auth::AUTH_SCHEMA)?;
            db.exec("create search schema", SEARCH_SCHEMA)?;
            db.exec("create import schema", IMPORT_SCHEMA)?;
            db.exec("create variant schema", VARIANT_SCHEMA)?;
            db.exec("create gc schema", GC_SCHEMA)?;
            db.exec("create empty blob-ref schema", EMPTY_BLOBREF_SCHEMA)?;
            if version == 13 {
                let columns = [
                    "artist", "artist_url", "album", "source_url", "license", "license_url",
                ];
                for (column, sql) in
                    columns.into_iter().zip(crate::search::ATTRIBUTION_MIGRATION_SQL)
                {
                    if !table_has_column(db, "search_annotations", column)? {
                        db.exec("add attribution column", sql)?;
                    }
                }
            }
            // v15 folds accented text in the search index, so a catalog
            // written before it holds the wrong terms. Both older openable
            // versions need the rebuild; a fresh root has no annotations to
            // rebuild and skips it.
            if version == 13 || version == 14 {
                crate::search::reindex_postings(db)?;
            }
            // v16: edit/app-run kinds (the rebuild carries this build's whole
            // list, so it also covers v17's `flow`, v18's `motion` and v19's `storyboard`), the reverse dependency index, and a
            // GC run left in flight without the dependency frontier is
            // cancelled (see the server's v15 step).
            if version == 13 || version == 14 || version == 15 {
                db.exec("extend edit kind check", crate::search::EDIT_KIND_CHECK_REBUILD_SQL)?;
                crate::catalog::backfill_dependency_refs(db)?;
                crate::gc::cancel_runs_for_upgrade(db)?;
            }
            // v17: the kind CHECK gains `flow` (same rebuild, current list).
            if version == 16 {
                db.exec("extend flow kind check", crate::search::EDIT_KIND_CHECK_REBUILD_SQL)?;
            }
            // v18: the kind CHECK gains `motion` (same rebuild, current list).
            if version == 17 {
                db.exec("extend motion kind check", crate::search::EDIT_KIND_CHECK_REBUILD_SQL)?;
            }
            // v19: the kind CHECK gains `storyboard` (same rebuild, current list).
            if version == 18 {
                db.exec("extend storyboard kind check", crate::search::EDIT_KIND_CHECK_REBUILD_SQL)?;
            }
            // v20: the kind CHECK gains `score` (same rebuild, current list).
            if version == 19 {
                db.exec("extend score kind check", crate::search::EDIT_KIND_CHECK_REBUILD_SQL)?;
            }
            // v21: the `edit` kind is `cut` (the rebuild carries each `edit`
            // row over as `cut`).
            if version == 20 {
                db.exec("rename edit kind to cut", crate::search::EDIT_KIND_CHECK_REBUILD_SQL)?;
            }
            // v22: the kind CHECK gains `deck` (same rebuild, current list).
            if version == 21 {
                db.exec("extend deck kind check", crate::search::EDIT_KIND_CHECK_REBUILD_SQL)?;
            }
            // v23: the kind CHECK gains `tree` (document trees; same rebuild, current list).
            if version == 22 {
                db.exec("extend tree kind check", crate::search::EDIT_KIND_CHECK_REBUILD_SQL)?;
            }
            // v24: the head log store sync replicates (seeded from the heads a
            // catalog already has; a fresh catalog has none).
            db.exec("create sync schema", crate::sync_log::SYNC_SCHEMA)?;
            if version != 0 && version < 24 {
                crate::sync_log::seed_in_tx(db)?;
            }
            if version != SERVER_SCHEMA_VERSION {
                db.exec(
                    "set user_version",
                    &format!("PRAGMA user_version={SERVER_SCHEMA_VERSION}"),
                )?;
            }
            Ok(())
        })?;
        Ok(Self { db, budgets })
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    pub fn catalog(&self) -> Catalog<'_> {
        Catalog { db: &self.db, budgets: &self.budgets }
    }

    pub fn auth(&self) -> Auth<'_> {
        Auth { db: &self.db }
    }

    pub fn search(&self) -> Search<'_> {
        Search { db: &self.db, budgets: &self.budgets }
    }

    pub fn imports(&self) -> Imports<'_> {
        Imports { db: &self.db, budgets: &self.budgets }
    }

    pub fn variants(&self) -> Variants<'_> {
        Variants { db: &self.db, budgets: &self.budgets }
    }

    pub fn gc(&self) -> Gc<'_> {
        Gc { db: &self.db, budgets: &self.budgets }
    }

    /// One page of the live assets whose head revision depends on any
    /// revision of `asset_id`: the REMOVE guard's "used by N" list. A head is
    /// what an alias points at or, for an asset with no alias, its newest
    /// (by creation, as retention orders them) published revision.
    /// Superseded revisions do not count, and neither does the asset itself.
    /// One row per asset (its lowest matching head revision), ordered by
    /// asset id; `after` continues from a previous page's `next`. `total`
    /// counts every dependent, whatever the page.
    ///
    /// Answered from the reverse dependency index: the cost is the ref rows
    /// that name `asset_id` (every revision of every asset that ever used
    /// it), plus one manifest decode per LISTED row, never the catalog.
    pub fn dependents(
        &self,
        asset_id: &AssetId,
        after: Option<&AssetId>,
        limit: usize,
    ) -> ServerResult<DependentsPage> {
        const LIVE_HEAD_REFS: &str = "
             FROM asset_dependency_refs d
             JOIN asset_revisions r ON r.revision = d.revision
             JOIN assets a ON a.asset_id = r.asset_id
             JOIN candidates c
               ON c.kind = 'asset' AND c.owner_id = r.asset_id AND c.revision = r.revision
             WHERE d.dep_asset_id = ?1 AND r.asset_id <> ?1
               AND a.retired_ms IS NULL AND c.retired_ms IS NULL AND c.state = 'published'
               AND (EXISTS(SELECT 1 FROM asset_aliases h
                           WHERE h.asset_id = r.asset_id AND h.head_revision = r.revision)
                    OR (NOT EXISTS(SELECT 1 FROM asset_aliases h WHERE h.asset_id = r.asset_id)
                        AND r.revision = (
                            SELECT nr.revision FROM asset_revisions nr
                            JOIN candidates nc
                              ON nc.kind = 'asset' AND nc.owner_id = nr.asset_id
                             AND nc.revision = nr.revision
                            WHERE nr.asset_id = r.asset_id
                              AND nc.state = 'published' AND nc.retired_ms IS NULL
                            ORDER BY nr.created_ms DESC, nr.revision DESC LIMIT 1)))";
        let limit = limit.clamp(1, makepad_asset_data::limits::MAX_DEPENDENTS_PAGE);
        let total = {
            let mut s = self.db.prepare(
                "asset dependents count",
                &format!("SELECT COUNT(DISTINCT r.asset_id) {LIVE_HEAD_REFS}"),
            )?;
            s.bind_blob(1, asset_id.as_bytes())?;
            if s.step()? { s.column_u64(0) } else { 0 }
        };
        let mut s = self.db.prepare(
            "asset dependents page",
            &format!(
                "SELECT p.asset_id, p.revision, r2.manifest, sa.title, sa.visibility, sa.owner
                 FROM (SELECT r.asset_id AS asset_id, MIN(r.revision) AS revision
                       {LIVE_HEAD_REFS} AND r.asset_id > ?2
                       GROUP BY r.asset_id ORDER BY r.asset_id LIMIT ?3) p
                 JOIN asset_revisions r2 ON r2.revision = p.revision
                 LEFT JOIN search_annotations sa ON sa.asset_id = p.asset_id
                 ORDER BY p.asset_id"
            ),
        )?;
        s.bind_blob(1, asset_id.as_bytes())?;
        s.bind_blob(2, after.map(|a| a.as_bytes().as_slice()).unwrap_or(&[]))?;
        s.bind_u64(3, limit as u64 + 1)?;
        let mut rows = Vec::new();
        while s.step()? {
            let manifest = AssetManifest::from_canonical_bytes(&s.column_blob(2))?;
            let owner = if s.column_is_null(5) {
                None
            } else {
                Some(PrincipalId(crate::catalog::fixed16(&s.column_blob(5), "dependent owner")?))
            };
            rows.push(Dependent {
                asset_id: AssetId::from_bytes(crate::catalog::fixed16(
                    &s.column_blob(0),
                    "dependent asset",
                )?),
                revision: AssetRevisionId::from_bytes(crate::catalog::fixed32(
                    &s.column_blob(1),
                    "dependent revision",
                )?),
                kind: manifest.kind,
                title: if s.column_is_null(3) { String::new() } else { s.column_text(3) },
                public: !s.column_is_null(4) && s.column_text(4) == "public",
                owner,
            });
        }
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|row| row.asset_id)
        } else {
            None
        };
        Ok(DependentsPage { total, rows, next })
    }

    pub fn detail(&self, asset_id: &AssetId) -> ServerResult<Option<AssetDetail>> {
        let Some(namespace) = self.catalog().asset_namespace(asset_id)? else {
            return Ok(None);
        };
        let annotation = self.search().annotation(asset_id)?;
        let candidates = self.catalog().asset_candidates(asset_id, 512)?;
        let mut stmt = self.db.prepare(
            "asset detail aliases",
            "SELECT alias, head_revision FROM asset_aliases
             WHERE asset_id=?1 ORDER BY alias",
        )?;
        stmt.bind_blob(1, asset_id.as_bytes())?;
        let mut aliases = Vec::new();
        while stmt.step()? {
            aliases.push((
                AssetAlias::new(stmt.column_text(0))?,
                AssetRevisionRef {
                    asset_id: *asset_id,
                    revision: AssetRevisionId::from_bytes(crate::catalog::fixed32(
                        &stmt.column_blob(1),
                        "asset detail alias revision",
                    )?),
                },
            ));
        }
        Ok(Some(AssetDetail { asset_id: *asset_id, namespace, annotation, candidates, aliases }))
    }

    /// Enumerate a bounded, public-only page for a sink-independent export
    /// planner. Asset identity and published revision state are catalog truth;
    /// aliases and public/live search text are optional projections.
    pub fn public_export_page(
        &self,
        filter: PublicExportFilter<'_>,
    ) -> ServerResult<PublicExportPage> {
        if filter.limit == 0 || filter.limit > self.budgets.max_search_results {
            return Err(ServerError::OverBudget {
                what: "public export page size",
                limit: self.budgets.max_search_results as u64,
                found: filter.limit as u64,
            });
        }
        if let Some(namespace) = filter.namespace {
            crate::catalog::validate_namespace(namespace)?;
        }
        let wanted = filter.limit as usize + 1;
        let scan_limit = self.budgets.max_search_results as u64;
        let mut scan_after = filter.after;
        let mut assets = Vec::with_capacity(wanted);
        while assets.len() < wanted {
            let mut stmt = self.db.prepare(
                "public export page",
                "SELECT a.asset_id, a.namespace, a.created_ms, sa.asset_id,
                        sa.title, sa.description, sa.kind, sa.creator,
                        sa.artist, sa.artist_url, sa.album, sa.source_url,
                        sa.license, sa.license_url,
                        sa.generator, sa.backend, sa.model, sa.updated_ms
                 FROM assets a
                 LEFT JOIN search_annotations sa ON sa.asset_id=a.asset_id
                    AND sa.visibility='public' AND sa.live=1
                 WHERE a.retired_ms IS NULL
                   AND (?1 IS NULL OR a.namespace=?1)
                   AND (?2 IS NULL OR a.asset_id>?2)
                 ORDER BY a.asset_id LIMIT ?3",
            )?;
            match filter.namespace {
                Some(namespace) => stmt.bind_text(1, namespace)?,
                None => stmt.bind_null(1)?,
            }
            match scan_after {
                Some(asset_id) => stmt.bind_blob(2, asset_id.as_bytes())?,
                None => stmt.bind_null(2)?,
            }
            stmt.bind_u64(3, scan_limit)?;

            let mut base = Vec::new();
            while stmt.step()? {
                base.push((
                    AssetId::from_bytes(crate::catalog::fixed16(
                        &stmt.column_blob(0),
                        "public export asset id",
                    )?),
                    stmt.column_text(1),
                    stmt.column_u64(2),
                    !stmt.column_is_null(3),
                    stmt.column_text(4),
                    stmt.column_text(5),
                    if stmt.column_is_null(6) {
                        None
                    } else {
                        Some(kind_parse(&stmt.column_text(6)).ok_or(
                            ServerError::InvalidState {
                                what: "public export annotation kind",
                                state: "unknown",
                            },
                        )?)
                    },
                    stmt.column_text(7),
                    stmt.column_text(8),
                    stmt.column_text(9),
                    stmt.column_text(10),
                    stmt.column_text(11),
                    stmt.column_text(12),
                    stmt.column_text(13),
                    stmt.column_text(14),
                    stmt.column_text(15),
                    stmt.column_text(16),
                    stmt.column_u64(17),
                ));
            }
            drop(stmt);
            if base.is_empty() {
                break;
            }
            let scanned = base.len();
            scan_after = base.last().map(|row| row.0);

            for (
                asset_id,
                namespace,
                created_ms,
                has_public_search,
                title,
                description,
                kind,
                creator,
                artist,
                artist_url,
                album,
                source_url,
                license,
                license_url,
                generator,
                backend,
                model,
                updated_ms,
            ) in base
            {
                let mut revisions = Vec::new();
                let mut revision_stmt = self.db.prepare(
                    "public export revisions",
                    "SELECT c.revision, c.staged_ms, c.published_ms, r.manifest
                     FROM candidates c
                     JOIN asset_revisions r ON r.revision=c.revision
                        AND r.asset_id=c.owner_id
                     WHERE c.kind='asset' AND c.owner_id=?1
                       AND c.state='published' AND c.retired_ms IS NULL
                     ORDER BY c.published_ms, c.revision LIMIT ?2",
                )?;
                revision_stmt.bind_blob(1, asset_id.as_bytes())?;
                revision_stmt.bind_u64(2, self.budgets.max_search_index_terms as u64 + 1)?;
                while revision_stmt.step()? {
                    let revision = AssetRevisionId::from_bytes(crate::catalog::fixed32(
                        &revision_stmt.column_blob(0),
                        "public export revision",
                    )?);
                    let manifest_bytes = revision_stmt.column_blob(3);
                    let manifest = AssetManifest::from_canonical_bytes(&manifest_bytes).map_err(
                        |error| {
                            content_err_at(
                                "catalog.sqlite3 asset_revisions.manifest",
                                &manifest_bytes,
                                error,
                            )
                        },
                    )?;
                    if manifest.asset_id != asset_id {
                        return Err(ServerError::Conflict {
                            what: "public export revision asset",
                        });
                    }
                    revisions.push(PublicAssetRevision {
                        target: AssetRevisionRef { asset_id, revision },
                        kind: manifest.kind,
                        staged_ms: revision_stmt.column_u64(1),
                        published_ms: revision_stmt.column_u64(2),
                    });
                }
                drop(revision_stmt);
                if revisions.len() > self.budgets.max_search_index_terms as usize {
                    return Err(ServerError::OverBudget {
                        what: "public export revisions per asset",
                        limit: self.budgets.max_search_index_terms as u64,
                        found: revisions.len() as u64,
                    });
                }
                if filter
                    .kind
                    .is_some_and(|wanted| !revisions.iter().any(|row| row.kind == wanted))
                {
                    continue;
                }

            let mut aliases = Vec::new();
            let mut alias_stmt = self.db.prepare(
                "public export aliases",
                "SELECT aa.alias, aa.head_revision, aa.updated_ms, c.published_ms
                 FROM asset_aliases aa
                 JOIN candidates c ON c.kind='asset' AND c.owner_id=aa.asset_id
                    AND c.revision=aa.head_revision
                 WHERE aa.asset_id=?1 AND c.state='published' AND c.retired_ms IS NULL
                 ORDER BY aa.alias LIMIT ?2",
            )?;
            alias_stmt.bind_blob(1, asset_id.as_bytes())?;
            alias_stmt.bind_u64(2, self.budgets.max_search_index_terms as u64 + 1)?;
            while alias_stmt.step()? {
                aliases.push(PublicAliasHead {
                    alias: AssetAlias::new(alias_stmt.column_text(0))?,
                    target: AssetRevisionRef {
                        asset_id,
                        revision: AssetRevisionId::from_bytes(crate::catalog::fixed32(
                            &alias_stmt.column_blob(1),
                            "public export alias revision",
                        )?),
                    },
                    updated_ms: alias_stmt.column_u64(2),
                    published_ms: alias_stmt.column_u64(3),
                });
            }
            drop(alias_stmt);
            if aliases.len() > self.budgets.max_search_index_terms as usize {
                return Err(ServerError::OverBudget {
                    what: "public export aliases per asset",
                    limit: self.budgets.max_search_index_terms as u64,
                    found: aliases.len() as u64,
                });
            }

                let mut categories = Vec::new();
                let mut tags = Vec::new();
                let mut terms = Vec::new();
                if has_public_search {
                    let mut labels = self.db.prepare(
                        "public export labels",
                        "SELECT kind, label FROM search_labels
                         WHERE asset_id=?1 ORDER BY kind, label",
                    )?;
                    labels.bind_blob(1, asset_id.as_bytes())?;
                    while labels.step()? {
                        if labels.column_text(0) == "category" {
                            categories.push(labels.column_text(1));
                        } else {
                            tags.push(labels.column_text(1));
                        }
                    }
                    drop(labels);

                    let mut postings = self.db.prepare(
                        "public export postings",
                        "SELECT term, SUM(weight) FROM (
                             SELECT term, weight_public AS weight FROM search_postings
                             WHERE asset_id=?1 AND weight_public>0
                             UNION ALL
                             SELECT term, weight FROM search_alias_postings
                             WHERE asset_id=?1 AND weight>0
                         ) GROUP BY term ORDER BY term",
                    )?;
                    postings.bind_blob(1, asset_id.as_bytes())?;
                    while postings.step()? {
                        terms.push(PublicSearchTerm {
                            term: postings.column_text(0),
                            weight: postings.column_u64(1),
                        });
                    }
                }

                assets.push(PublicExportAsset {
                    asset_id,
                    namespace,
                    created_ms,
                    revisions,
                    aliases,
                    search: PublicSearchProjection {
                        title,
                        description,
                        kind,
                        categories,
                        tags,
                        creator,
                        artist,
                        artist_url,
                        album,
                        source_url,
                        license,
                        license_url,
                        generator,
                        backend,
                        model,
                        updated_ms,
                        terms,
                    },
                });
                if assets.len() == wanted {
                    break;
                }
            }
            if assets.len() == wanted || scanned < scan_limit as usize {
                break;
            }
        }
        let more = assets.len() > filter.limit as usize;
        assets.truncate(filter.limit as usize);
        let next = if more { assets.last().map(|row| row.asset_id) } else { None };
        Ok(PublicExportPage { assets, next })
    }

    /// Atomic catalog half of publication. Blob durability/admission must
    /// complete before this method is called.
    pub fn publish_batch(
        &self,
        items: &[PublishBatchItem],
        now_ms: u64,
    ) -> ServerResult<Vec<PublishBatchOutcome>> {
        self.publish_batch_inner(items, None, now_ms)
    }

    /// Atomic publication with explicit alias-scoped preconditions. A stale
    /// item rolls back the whole page; admitted blobs remain available for retry.
    pub fn publish_batch_guarded(
        &self,
        items: &[PublishBatchItem],
        guards: &[PublishExpectedHead],
        now_ms: u64,
    ) -> ServerResult<Vec<PublishBatchOutcome>> {
        if guards.len() != items.len() {
            return Err(ServerError::InvalidInput { what: "publish guard count" });
        }
        self.publish_batch_inner(items, Some(guards), now_ms)
    }

    fn publish_batch_inner(
        &self,
        items: &[PublishBatchItem],
        guards: Option<&[PublishExpectedHead]>,
        now_ms: u64,
    ) -> ServerResult<Vec<PublishBatchOutcome>> {
        let mut decoded: Vec<(AssetManifest, AssetRevisionId)> = Vec::with_capacity(items.len());
        for item in items {
            if item.manifest_bytes.len() as u64 > self.budgets.max_manifest_bytes {
                return Err(ServerError::OverBudget {
                    what: "asset manifest bytes",
                    limit: self.budgets.max_manifest_bytes,
                    found: item.manifest_bytes.len() as u64,
                });
            }
            let manifest = AssetManifest::from_canonical_bytes(&item.manifest_bytes)?;
            let revision = AssetRevisionId::hash_of(&item.manifest_bytes);
            if guards.is_some() {
                // A guarded page may touch each identity/alias only once, so
                // every precondition describes the same pre-commit snapshot.
                if decoded.iter().any(|(previous, _)| previous.asset_id == manifest.asset_id)
                    || item.alias.as_ref().is_some_and(|alias| {
                        items[..decoded.len()].iter().any(|i| i.alias.as_ref() == Some(alias))
                    })
                {
                    return Err(ServerError::InvalidInput { what: "duplicate guarded publication target" });
                }
            }
            if let Some(alias) = &item.alias {
                if alias.namespace() != item.namespace {
                    return Err(ServerError::Conflict { what: "alias namespace" });
                }
            }
            let candidates = self.catalog().asset_candidates(&manifest.asset_id, 512)?;
            let previous = candidates
                .iter()
                .filter(|row| {
                    row.state == CandidateState::Published && row.revision != revision
                })
                .max_by_key(|row| row.published_ms.unwrap_or(0))
                .map(|row| row.revision);
            if let Some(previous) = previous {
                if let Some(bytes) = self.catalog().asset_revision_manifest(&previous)? {
                    if let Ok(previous) = AssetManifest::from_canonical_bytes(&bytes) {
                        if previous.rights != manifest.rights {
                            return Err(ServerError::Conflict {
                                what: "published asset rights would change",
                            });
                        }
                    }
                }
            }
            decoded.push((manifest, revision));
        }

        let catalog = self.catalog();
        let search = self.search();
        self.db.tx(|db| {
            let mut unchanged = vec![false; items.len()];
            if let Some(guards) = guards {
                for (index, (item, (manifest, revision))) in items.iter().zip(&decoded).enumerate() {
                    unchanged[index] = catalog.check_publish_head_in_tx(
                        item.alias.as_ref(),
                        &AssetRevisionRef { asset_id: manifest.asset_id, revision: *revision },
                        guards[index],
                    )?;
                }
            }
            let mut outcomes = Vec::with_capacity(items.len());
            for (index, (item, (manifest, revision))) in items.iter().zip(&decoded).enumerate() {
                if unchanged[index] {
                    outcomes.push(PublishBatchOutcome {
                        asset_id: manifest.asset_id,
                        revision: *revision,
                        already_published: true,
                        unchanged: true,
                    });
                    continue;
                }
                catalog.register_asset(&manifest.asset_id, &item.namespace, now_ms)?;
                let already_published = match catalog
                    .asset_candidate_state(&manifest.asset_id, revision)?
                {
                    Some(CandidateState::Published) => true,
                    Some(CandidateState::Quarantined) => {
                        return Err(ServerError::InvalidState {
                            what: "publish batch revision",
                            state: "quarantined",
                        });
                    }
                    Some(CandidateState::Staged) => {
                        catalog.transition_in_tx(
                            db,
                            "asset",
                            manifest.asset_id.as_bytes(),
                            revision.as_bytes(),
                            &[CandidateState::Staged],
                            CandidateState::Published,
                            now_ms,
                        )?;
                        false
                    }
                    None => {
                        let staged = catalog.stage_asset_revision_in_tx(
                            db,
                            &item.manifest_bytes,
                            now_ms,
                        )?;
                        catalog.transition_in_tx(
                            db,
                            "asset",
                            manifest.asset_id.as_bytes(),
                            staged.as_bytes(),
                            &[CandidateState::Staged],
                            CandidateState::Published,
                            now_ms,
                        )?;
                        false
                    }
                };
                search.set_annotation_in_tx(db, &manifest.asset_id, &item.annotation, now_ms)?;
                if let Some(alias) = &item.alias {
                    catalog.set_asset_alias_in_tx(
                        db,
                        alias,
                        &AssetRevisionRef {
                            asset_id: manifest.asset_id,
                            revision: *revision,
                        },
                        now_ms,
                    )?;
                }
                outcomes.push(PublishBatchOutcome {
                    asset_id: manifest.asset_id,
                    revision: *revision,
                    already_published,
                    unchanged: false,
                });
            }
            Ok(outcomes)
        })
    }
}

#[derive(Clone, Debug)]
pub struct AssetDetail {
    pub asset_id: AssetId,
    pub namespace: String,
    pub annotation: Option<AssetAnnotation>,
    pub candidates: Vec<CandidateRow>,
    pub aliases: Vec<(AssetAlias, AssetRevisionRef)>,
}

#[derive(Clone, Debug)]
pub struct PublishBatchItem {
    pub namespace: String,
    pub manifest_bytes: Vec<u8>,
    pub annotation: AssetAnnotation,
    pub alias: Option<AssetAlias>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishBatchOutcome {
    pub asset_id: AssetId,
    pub revision: AssetRevisionId,
    pub already_published: bool,
    /// Guarded current-target replay: no annotation, alias, index or event write.
    pub unchanged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicAliasHead {
    pub alias: AssetAlias,
    pub target: AssetRevisionRef,
    pub updated_ms: u64,
    pub published_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicAssetRevision {
    pub target: AssetRevisionRef,
    pub kind: AssetKind,
    pub staged_ms: u64,
    pub published_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicSearchTerm {
    pub term: String,
    pub weight: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicSearchProjection {
    pub title: String,
    pub description: String,
    pub kind: Option<AssetKind>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub creator: String,
    pub artist: String,
    pub artist_url: String,
    pub album: String,
    pub source_url: String,
    pub license: String,
    pub license_url: String,
    pub generator: String,
    pub backend: String,
    pub model: String,
    pub updated_ms: u64,
    pub terms: Vec<PublicSearchTerm>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicExportAsset {
    pub asset_id: AssetId,
    pub namespace: String,
    pub created_ms: u64,
    pub revisions: Vec<PublicAssetRevision>,
    pub aliases: Vec<PublicAliasHead>,
    pub search: PublicSearchProjection,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublicExportPage {
    pub assets: Vec<PublicExportAsset>,
    pub next: Option<AssetId>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PublicExportFilter<'a> {
    pub namespace: Option<&'a str>,
    pub kind: Option<AssetKind>,
    pub after: Option<AssetId>,
    pub limit: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_asset_data::{
        AssetFile, Axis, BlobId, Bounds, Capabilities, CoordinateSystem, DerivativePolicy,
        DeviceTier, FileRole, MediaType, Metrics, Pivot, Redistribution, Rights, Vec3,
    };

    const NOW: u64 = 1_700_000_000_000;

    fn edit_manifest(asset_id: AssetId, source: &[u8], deps: Vec<AssetRevisionRef>) -> AssetManifest {
        AssetManifest {
            asset_id,
            kind: if deps.is_empty() { AssetKind::AppRun } else { AssetKind::Cut },
            files: vec![AssetFile {
                role: FileRole::Source,
                tier: DeviceTier::Any,
                lod: 0,
                media: MediaType::Text,
                blob: BlobId::hash_of(source),
                byte_len: source.len() as u64,
                dims: None,
            }],
            dependencies: deps,
            thumbnail: None,
            metrics: Metrics { total_bytes: source.len() as u64, ..Metrics::default() },
            coordinate_system: CoordinateSystem {
                units_per_meter: 1.0,
                up: Axis::YPos,
                forward: Axis::ZNeg,
                pivot: Pivot::Origin,
            },
            bounds: Bounds { min: Vec3::new(-1.0, -1.0, -1.0), max: Vec3::new(1.0, 1.0, 1.0) },
            anchors: vec![],
            capabilities: Capabilities::default(),
            spawn_recipe: None,
            provenance: None,
            rights: Rights {
                license: "CC0-1.0".into(),
                license_revision: String::new(),
                terms_digest: None,
                terms_url: String::new(),
                credits: "test".into(),
                source: String::new(),
                source_archive: None,
                redistribution: Redistribution::Allowed,
                derivatives: DerivativePolicy::Allowed,
            },
        }
    }

    fn publish(core: &CatalogCore, manifest: &AssetManifest, source: &[u8]) -> AssetRevisionRef {
        let catalog = core.catalog();
        catalog.record_blob(&BlobId::hash_of(source), source.len() as u64, NOW).unwrap();
        if catalog.asset_namespace(&manifest.asset_id).unwrap().is_none() {
            catalog.register_asset(&manifest.asset_id, "ns", NOW).unwrap();
        }
        let revision = catalog.stage_asset_revision(&manifest.to_canonical_bytes().unwrap(), NOW).unwrap();
        catalog.publish_asset(&manifest.asset_id, &revision, NOW).unwrap();
        AssetRevisionRef { asset_id: manifest.asset_id, revision }
    }

    fn count(db: &Db, sql: &str) -> i64 {
        let mut s = db.prepare("test count", sql).unwrap();
        assert!(s.step().unwrap());
        s.column_i64(0)
    }

    /// The embedded upgrade path (separate code from the server's `migrate`)
    /// from every openable older version: the reverse index is rebuilt from
    /// live manifests only, across more than one backfill page; a manifest
    /// that no longer decodes is skipped, not fatal; a GC run in flight is
    /// cancelled.
    #[test]
    fn embedded_upgrade_backfills_live_edges_and_cancels_a_run_in_flight() {
        for version in [13, 14, 15] {
            let core = CatalogCore::open_memory(Budgets::default_v1()).unwrap();
            let clip_id = AssetId::from_bytes([1; 16]);
            let clip = publish(&core, &edit_manifest(clip_id, b"RUN", vec![]), b"RUN");
            // 300 live dependents: more than one 256-revision page.
            for n in 0..300u16 {
                let mut id = [2u8; 16];
                id[..2].copy_from_slice(&n.to_be_bytes());
                let source = format!("EDIT-{n}");
                publish(&core, &edit_manifest(AssetId::from_bytes(id), source.as_bytes(), vec![clip]), source.as_bytes());
            }
            // A retired dependent: its edge is not backfilled.
            let retired_id = AssetId::from_bytes([9; 16]);
            let retired = publish(&core, &edit_manifest(retired_id, b"GONE", vec![clip]), b"GONE");
            core.catalog().retire_asset(&retired_id, NOW + 1).unwrap();
            core.gc().begin(crate::gc::GcConfig::default_v1(), NOW + 2).unwrap();
            core.gc().step_catalog(NOW + 2).unwrap();
            assert!(count(&core.db, "SELECT COUNT(*) FROM gc_marks") > 0);
            // A live revision whose stored manifest no longer decodes.
            core.db
                .exec(
                    "bad manifest",
                    "INSERT INTO asset_revisions(revision, asset_id, manifest, created_ms)
                     VALUES(X'0808080808080808080808080808080808080808080808080808080808080808',
                            X'01010101010101010101010101010101', X'00', 0)",
                )
                .unwrap();
            core.db
                .exec(
                    "rewind",
                    &format!("DELETE FROM asset_dependency_refs; PRAGMA user_version={version}"),
                )
                .unwrap();

            let CatalogCore { db, budgets } = core;
            let core = CatalogCore::initialize(db, budgets).unwrap();
            assert_eq!(count(&core.db, "PRAGMA user_version"), SERVER_SCHEMA_VERSION);
            assert_eq!(count(&core.db, "SELECT COUNT(*) FROM asset_dependency_refs"), 300);
            let page = core.dependents(&clip_id, None, 128).unwrap();
            assert_eq!((page.total, page.rows.len()), (300, 128));
            assert!(page.rows.iter().all(|row| row.kind == AssetKind::Cut));
            assert!(page.rows.iter().all(|row| row.asset_id != retired.asset_id));
            let status = core.gc().status().unwrap().unwrap();
            assert_eq!(status.phase, crate::gc::GcPhase::Cancelled);
            assert_eq!(count(&core.db, "SELECT COUNT(*) FROM gc_marks"), 0);
        }
    }

    /// v16 -> v17 on the embedded path: the kind CHECK gains `flow`.
    #[test]
    fn embedded_upgrade_from_16_accepts_the_flow_kind() {
        let core = CatalogCore::open_memory(Budgets::default_v1()).unwrap();
        let legacy = crate::search::EDIT_KIND_CHECK_REBUILD_SQL.replace(",'flow','motion','storyboard','score','deck','tree'", "");
        assert_ne!(legacy, crate::search::EDIT_KIND_CHECK_REBUILD_SQL);
        core.db.exec("legacy check", &legacy).unwrap();
        core.db
            .exec(
                "legacy row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'flow', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap_err();
        core.db.exec("rewind", "PRAGMA user_version=16").unwrap();
        let CatalogCore { db, budgets } = core;
        let core = CatalogCore::initialize(db, budgets).unwrap();
        assert_eq!(count(&core.db, "PRAGMA user_version"), SERVER_SCHEMA_VERSION);
        core.db
            .exec(
                "flow row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'flow', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap();
    }

    /// v17 -> v18 on the embedded path: the kind CHECK gains `motion`.
    #[test]
    fn embedded_upgrade_from_17_accepts_the_motion_kind() {
        let core = CatalogCore::open_memory(Budgets::default_v1()).unwrap();
        let legacy = crate::search::EDIT_KIND_CHECK_REBUILD_SQL.replace(",'motion','storyboard','score','deck','tree'", "");
        assert_ne!(legacy, crate::search::EDIT_KIND_CHECK_REBUILD_SQL);
        core.db.exec("legacy check", &legacy).unwrap();
        core.db
            .exec(
                "legacy row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'motion', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap_err();
        core.db.exec("rewind", "PRAGMA user_version=17").unwrap();
        let CatalogCore { db, budgets } = core;
        let core = CatalogCore::initialize(db, budgets).unwrap();
        assert_eq!(count(&core.db, "PRAGMA user_version"), SERVER_SCHEMA_VERSION);
        core.db
            .exec(
                "motion row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'motion', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap();
    }

    /// v18 -> v19 on the embedded path: the kind CHECK gains `storyboard`.
    #[test]
    fn embedded_upgrade_from_18_accepts_the_storyboard_kind() {
        let core = CatalogCore::open_memory(Budgets::default_v1()).unwrap();
        let legacy = crate::search::EDIT_KIND_CHECK_REBUILD_SQL.replace(",'storyboard','score','deck','tree'", "");
        assert_ne!(legacy, crate::search::EDIT_KIND_CHECK_REBUILD_SQL);
        core.db.exec("legacy check", &legacy).unwrap();
        core.db
            .exec(
                "legacy row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'storyboard', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap_err();
        core.db.exec("rewind", "PRAGMA user_version=18").unwrap();
        let CatalogCore { db, budgets } = core;
        let core = CatalogCore::initialize(db, budgets).unwrap();
        assert_eq!(count(&core.db, "PRAGMA user_version"), SERVER_SCHEMA_VERSION);
        core.db
            .exec(
                "storyboard row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'storyboard', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap();
    }

    /// v19 -> v20 on the embedded path: the kind CHECK gains `score`.
    #[test]
    fn embedded_upgrade_from_19_accepts_the_score_kind() {
        let core = CatalogCore::open_memory(Budgets::default_v1()).unwrap();
        let legacy = crate::search::EDIT_KIND_CHECK_REBUILD_SQL.replace(",'score','deck','tree'", "");
        assert_ne!(legacy, crate::search::EDIT_KIND_CHECK_REBUILD_SQL);
        core.db.exec("legacy check", &legacy).unwrap();
        core.db
            .exec(
                "legacy row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'score', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap_err();
        core.db.exec("rewind", "PRAGMA user_version=19").unwrap();
        let CatalogCore { db, budgets } = core;
        let core = CatalogCore::initialize(db, budgets).unwrap();
        assert_eq!(count(&core.db, "PRAGMA user_version"), SERVER_SCHEMA_VERSION);
        core.db
            .exec(
                "score row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'score', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap();
    }

    /// v22 -> v23 on a catalog as the user's real store has it: the v22 kind
    /// CHECK still names `show` (the deck kind before its rename) and rows of
    /// every kind that existed then; the rebuild carries `show` over as `deck`
    /// and keeps every other row.
    #[test]
    fn embedded_upgrade_from_22_carries_every_kind_and_renames_show_to_deck() {
        let core = CatalogCore::open_memory(Budgets::default_v1()).unwrap();
        let v22 = crate::search::EDIT_KIND_CHECK_REBUILD_SQL.replace(",'deck','tree'", ",'show'");
        assert_ne!(v22, crate::search::EDIT_KIND_CHECK_REBUILD_SQL);
        core.db.exec("v22 check", &v22).unwrap();
        let kinds = [
            "mesh", "character", "weapon", "vehicle", "prop", "texture", "material", "audio", "video", "skybox",
            "world", "prefab", "billboard", "game", "vjeffect", "data", "model-program", "cut", "app-run", "flow",
            "motion", "storyboard", "score", "show",
        ];
        for (i, kind) in kinds.iter().enumerate() {
            core.db
                .exec(
                    "v22 row",
                    &format!(
                        "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                           description, creator, generator, backend, model, prompt, provenance, live,
                           updated_ms)
                         VALUES(X'{:04x}', 'ns', '{kind}', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
                        i + 1
                    ),
                )
                .unwrap();
        }
        core.db.exec("rewind", "PRAGMA user_version=22").unwrap();
        let CatalogCore { db, budgets } = core;
        let core = CatalogCore::initialize(db, budgets).unwrap();
        assert_eq!(count(&core.db, "PRAGMA user_version"), SERVER_SCHEMA_VERSION);
        assert_eq!(count(&core.db, "SELECT count(*) FROM search_annotations"), kinds.len() as i64);
        assert_eq!(count(&core.db, "SELECT count(*) FROM search_annotations WHERE kind = 'show'"), 0);
        assert_eq!(count(&core.db, "SELECT count(*) FROM search_annotations WHERE kind = 'deck'"), 1);
        assert_eq!(count(&core.db, "SELECT count(*) FROM search_annotations WHERE kind = 'cut'"), 1);
    }

    /// v21 -> v22 on the embedded path: the kind CHECK gains `deck`.
    #[test]
    fn embedded_upgrade_from_21_accepts_the_deck_kind() {
        let core = CatalogCore::open_memory(Budgets::default_v1()).unwrap();
        let legacy = crate::search::EDIT_KIND_CHECK_REBUILD_SQL.replace(",'deck','tree'", "");
        assert_ne!(legacy, crate::search::EDIT_KIND_CHECK_REBUILD_SQL);
        core.db.exec("legacy check", &legacy).unwrap();
        core.db
            .exec(
                "legacy row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'deck', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap_err();
        core.db.exec("rewind", "PRAGMA user_version=21").unwrap();
        let CatalogCore { db, budgets } = core;
        let core = CatalogCore::initialize(db, budgets).unwrap();
        assert_eq!(count(&core.db, "PRAGMA user_version"), SERVER_SCHEMA_VERSION);
        core.db
            .exec(
                "score row",
                "INSERT INTO search_annotations(asset_id, namespace, kind, visibility, title,
                   description, creator, generator, backend, model, prompt, provenance, live,
                   updated_ms)
                 VALUES(X'0101', 'ns', 'deck', 'public', 't', '', '', '', '', '', '', '', 1, 1)",
            )
            .unwrap();
    }
}
