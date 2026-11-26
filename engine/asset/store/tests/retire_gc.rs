//! Deletion: retirement of assets and revisions, and the incremental blob
//! garbage collector that reclaims what retirement unlinked.
//!
//! The properties proven here are the ones a store operator has to be able
//! to trust before deleting anything:
//! - retirement removes the asset from every read surface (alias, search,
//!   manifest, listing) and is idempotent,
//! - GC deletes exactly the unreferenced bytes and NEVER a referenced one,
//! - a dry run's numbers are the numbers a real run then deletes,
//! - a crash between "row deleted" and "file unlinked" leaves a consistent
//!   store that the next start repairs,
//! - a publish that lands mid-run is never collected out from under itself,
//! - bytes a live revision depends on (transitively) are kept even when the
//!   revisions that name them are retired, and `dependents` answers who
//!   would break,
//! - the retention rule trims superseded revisions and never a live head.

mod common;

use common::*;
use makepad_asset_data::*;
use makepad_asset_store::*;

/// Run a GC to completion with generous step and grace bounds. Tests use a
/// zero grace window on purpose: the grace window's own behaviour is proven
/// separately, and everywhere else it would just hide bytes.
fn gc_all(core: &AssetServerCore, cfg: GcConfig, now: u64) -> GcStatus {
    let status = core.gc_run(cfg, 10_000, now).unwrap();
    assert!(status.finished(), "gc did not finish within the step budget: {status:?}");
    status
}

fn collect_cfg(dry_run: bool) -> GcConfig {
    GcConfig { dry_run, grace_ms: 0, ..GcConfig::default_v1() }
}

fn alias(text: &str) -> AssetAlias {
    AssetAlias::new(text.to_string()).unwrap()
}

fn annotate(core: &AssetServerCore, id: &AssetId, title: &str, now: u64) {
    core.search()
        .set_annotation(
            id,
            &AssetAnnotation {
                title: title.into(),
                description: "fixture".into(),
                kind: Some(AssetKind::Prop),
                categories: vec![],
                tags: vec![],
                creator: "tester".into(),
                artist: String::new(),
                artist_url: String::new(),
                album: String::new(),
                source_url: String::new(),
                license: String::new(),
                license_url: String::new(),
                owner: None,
                generator: String::new(),
                backend: String::new(),
                model: String::new(),
                prompt: String::new(),
                provenance: String::new(),
                visibility: Visibility::Public,
            },
            now,
        )
        .unwrap();
}

fn search_titles(core: &AssetServerCore, text: &str) -> Vec<String> {
    let page = core
        .search()
        .search(
            &SearchQuery {
                text,
                filters: SearchFilters::default(),
                expand: false,
                page_size: 50,
                newest: false,
                facets: 0,
            },
            &SearchViewer { principal: None, scope: ViewerScope::All },
            None,
        )
        .unwrap();
    page.hits.into_iter().map(|h| h.title).collect()
}

#[test]
fn retiring_an_asset_removes_it_from_every_read_surface_and_is_idempotent() {
    let (_root, core) = open_core("retire_asset");
    let (id, rev) = publish_prop(&core, "ns", 1, b"GLB-A", b"THUMB-A", NOW);
    annotate(&core, &id, "watchtower", NOW);
    let a = alias("ns/watchtower");
    core.catalog()
        .set_asset_alias(&a, &AssetRevisionRef { asset_id: id, revision: rev }, NOW)
        .unwrap();
    assert!(core.catalog().resolve_asset_alias(&a).unwrap().is_some());
    assert_eq!(search_titles(&core, "watchtower"), vec!["watchtower".to_string()]);

    let report = core.catalog().retire_asset(&id, NOW + 1).unwrap();
    assert!(!report.already_retired);
    assert_eq!(report.revisions_retired, 1);
    assert_eq!(report.aliases_dropped, 1);
    assert!(report.annotation_cleared);

    // Alias gone, search empty, revision terminal and marked for collection.
    assert_eq!(core.catalog().resolve_asset_alias(&a).unwrap(), None);
    assert!(search_titles(&core, "watchtower").is_empty());
    // Browse mode (no text) must not see it either: the row is deleted, not
    // filtered.
    assert!(search_titles(&core, "").is_empty());
    assert_eq!(
        core.catalog().asset_candidate_state(&id, &rev).unwrap(),
        Some(CandidateState::Quarantined)
    );
    assert!(core.catalog().revision_retired(&id, &rev).unwrap());
    assert!(core.catalog().asset_retired_ms(&id).unwrap().is_some());

    // Idempotent.
    let again = core.catalog().retire_asset(&id, NOW + 2).unwrap();
    assert!(again.already_retired);
    assert_eq!(again.revisions_retired, 0);

    // Terminal for the identity: no new revision, no re-registration.
    let manifest = prop_manifest(id, b"GLB-B", b"THUMB-B");
    core.put_blob(b"GLB-B", NOW + 3).unwrap();
    core.put_blob(b"THUMB-B", NOW + 3).unwrap();
    let bytes = manifest.to_canonical_bytes().unwrap();
    assert!(matches!(
        core.catalog().stage_asset_revision(&bytes, NOW + 3),
        Err(ServerError::InvalidState { what: "asset", state: "retired" })
    ));
    assert!(matches!(
        core.catalog().register_asset(&id, "ns", NOW + 3),
        Err(ServerError::InvalidState { what: "asset", state: "retired" })
    ));
}

#[test]
fn retiring_one_revision_drops_only_its_head_and_is_idempotent() {
    let (_root, core) = open_core("retire_revision");
    let id = asset_id_n(2);
    core.catalog().register_asset(&id, "ns", NOW).unwrap();
    // Two revisions of one asset; the alias points at the newer.
    let mut revs = Vec::new();
    for (glb, thumb, t) in [(&b"V1"[..], &b"T1"[..], NOW), (&b"V2"[..], &b"T2"[..], NOW + 1)] {
        core.put_blob(glb, t).unwrap();
        core.put_blob(thumb, t).unwrap();
        let bytes = prop_manifest(id, glb, thumb).to_canonical_bytes().unwrap();
        let rev = core.catalog().stage_asset_revision(&bytes, t).unwrap();
        core.catalog().publish_asset(&id, &rev, t).unwrap();
        revs.push(rev);
    }
    let a = alias("ns/keeper");
    core.catalog()
        .set_asset_alias(&a, &AssetRevisionRef { asset_id: id, revision: revs[1] }, NOW + 1)
        .unwrap();

    // Retiring the OLD revision leaves the alias (and the asset) alone.
    assert!(core.catalog().retire_revision(&id, &revs[0], NOW + 2).unwrap());
    assert!(core.catalog().revision_retired(&id, &revs[0]).unwrap());
    assert!(!core.catalog().revision_retired(&id, &revs[1]).unwrap());
    assert!(core.catalog().asset_retired_ms(&id).unwrap().is_none());
    assert!(core.catalog().resolve_asset_alias(&a).unwrap().is_some());
    // Idempotent.
    assert!(!core.catalog().retire_revision(&id, &revs[0], NOW + 3).unwrap());

    // Retiring the head drops the alias, exactly like quarantine does.
    assert!(core.catalog().retire_revision(&id, &revs[1], NOW + 4).unwrap());
    assert_eq!(core.catalog().resolve_asset_alias(&a).unwrap(), None);
}

#[test]
fn gc_dry_run_counts_exactly_what_the_real_run_then_deletes() {
    let (_root, core) = open_core("gc_dry_run");
    // Two published assets plus one blob that no manifest ever named.
    let (kept, _) = publish_prop(&core, "ns", 3, b"KEEP-GLB", b"KEEP-THUMB", NOW);
    let (doomed, _) = publish_prop(&core, "ns", 4, b"DROP-GLB-LONGER", b"DROP-THUMB", NOW);
    let orphan = core.put_blob(b"NEVER-REFERENCED-BYTES", NOW).unwrap();
    annotate(&core, &kept, "kept", NOW);
    annotate(&core, &doomed, "doomed", NOW);
    core.catalog().retire_asset(&doomed, NOW + 1).unwrap();

    let expected_bytes = (b"DROP-GLB-LONGER".len()
        + b"DROP-THUMB".len()
        + b"NEVER-REFERENCED-BYTES".len()) as u64;

    let dry = gc_all(&core, collect_cfg(true), NOW + 2);
    assert!(dry.dry_run);
    assert_eq!(dry.unreferenced_blobs, 3);
    assert_eq!(dry.unreferenced_bytes, expected_bytes);
    assert_eq!(dry.deleted_blobs, 0);
    // Nothing moved: every blob is still readable.
    assert!(core.read_blob(&orphan.blob_id).is_ok());

    let real = gc_all(&core, collect_cfg(false), NOW + 3);
    assert_eq!(real.deleted_blobs, dry.unreferenced_blobs);
    assert_eq!(real.deleted_bytes, dry.unreferenced_bytes);

    // The kept asset's bytes are untouched and still verify.
    assert_eq!(core.read_blob(&BlobId::hash_of(b"KEEP-GLB")).unwrap(), b"KEEP-GLB");
    assert_eq!(core.read_blob(&BlobId::hash_of(b"KEEP-THUMB")).unwrap(), b"KEEP-THUMB");
    // The retired asset's bytes and the orphan are gone from BOTH the
    // catalog and the CAS.
    for gone in [
        BlobId::hash_of(b"DROP-GLB-LONGER"),
        BlobId::hash_of(b"DROP-THUMB"),
        orphan.blob_id,
    ] {
        assert!(matches!(
            core.read_blob(&gone),
            Err(ServerError::NotFound { what: "blob record" })
        ));
        assert!(!core.cas().contains(&gone), "object still on disk: {gone}");
    }

    // A second collection finds nothing left to do.
    let idle = gc_all(&core, collect_cfg(false), NOW + 4);
    assert_eq!(idle.deleted_blobs, 0);
    assert_eq!(idle.unreferenced_blobs, 0);
}

#[test]
fn gc_keeps_every_blob_a_live_revision_names_even_when_unpublished_or_shared() {
    let (_root, core) = open_core("gc_keeps_live");
    let shared_thumb = b"SHARED-THUMB".to_vec();
    // Asset A: published. Asset B: staged only (never published) — staged
    // content is not garbage, it is content that has not shipped yet.
    let (a, _) = publish_prop(&core, "ns", 5, b"A-GLB", &shared_thumb, NOW);
    let b = asset_id_n(6);
    core.put_blob(b"B-GLB", NOW).unwrap();
    core.catalog().register_asset(&b, "ns", NOW).unwrap();
    let staged_bytes = prop_manifest(b, b"B-GLB", &shared_thumb).to_canonical_bytes().unwrap();
    core.catalog().stage_asset_revision(&staged_bytes, NOW).unwrap();

    let status = gc_all(&core, collect_cfg(false), NOW + 1);
    assert_eq!(status.deleted_blobs, 0);
    for blob in [b"A-GLB".to_vec(), b"B-GLB".to_vec(), shared_thumb.clone()] {
        assert!(core.read_blob(&BlobId::hash_of(&blob)).is_ok());
    }

    // Retiring A must NOT collect the thumbnail B still names.
    core.catalog().retire_asset(&a, NOW + 2).unwrap();
    let status = gc_all(&core, collect_cfg(false), NOW + 3);
    assert_eq!(status.deleted_blobs, 1);
    assert!(matches!(
        core.read_blob(&BlobId::hash_of(b"A-GLB")),
        Err(ServerError::NotFound { .. })
    ));
    assert_eq!(core.read_blob(&BlobId::hash_of(&shared_thumb)).unwrap(), shared_thumb);
}

#[test]
fn a_publish_that_lands_mid_run_is_pinned_and_never_collected() {
    let (_root, core) = open_core("gc_pin_mid_run");
    let (retired, _) = publish_prop(&core, "ns", 7, b"OLD-GLB", b"OLD-THUMB", NOW);
    core.catalog().retire_asset(&retired, NOW + 1).unwrap();
    // Bytes that exist BEFORE the run starts and are unreferenced when the
    // mark phase reads them.
    let late = core.put_blob(b"LATE-REFERENCED", NOW).unwrap();
    let late_thumb = core.put_blob(b"LATE-THUMB", NOW).unwrap();

    // Start a run and walk it through the mark phase only.
    core.gc_begin(collect_cfg(false), NOW + 2).unwrap();
    let mut status = core.gc_status().unwrap().unwrap();
    while status.phase == GcPhase::Mark {
        status = core.gc_advance(1, NOW + 2).unwrap().unwrap();
    }
    assert_eq!(status.phase, GcPhase::Sweep);

    // A publish commits between the mark and the sweep: the manifest names
    // blobs the mark phase already decided were unreferenced.
    let new_id = asset_id_n(8);
    core.catalog().register_asset(&new_id, "ns", NOW + 2).unwrap();
    let bytes = prop_manifest(new_id, b"LATE-REFERENCED", b"LATE-THUMB")
        .to_canonical_bytes()
        .unwrap();
    let new_rev = core.catalog().stage_asset_revision(&bytes, NOW + 2).unwrap();
    core.catalog().publish_asset(&new_id, &new_rev, NOW + 2).unwrap();

    // Finish the sweep. The freshly referenced bytes survive; the retired
    // asset's do not.
    let status = core.gc_advance(10_000, NOW + 2).unwrap().unwrap();
    assert!(status.finished());
    assert_eq!(core.read_blob(&late.blob_id).unwrap(), b"LATE-REFERENCED");
    assert_eq!(core.read_blob(&late_thumb.blob_id).unwrap(), b"LATE-THUMB");
    assert!(matches!(
        core.read_blob(&BlobId::hash_of(b"OLD-GLB")),
        Err(ServerError::NotFound { .. })
    ));
}

#[test]
fn the_grace_window_protects_bytes_uploaded_before_their_manifest() {
    let (_root, core) = open_core("gc_grace");
    // An upload with no manifest yet: exactly the shape of a client that is
    // still assembling its publication.
    let fresh = core.put_blob(b"JUST-UPLOADED", NOW).unwrap();
    let cfg = GcConfig { grace_ms: 60_000, ..GcConfig::default_v1() };
    let status = gc_all(&core, cfg, NOW + 1_000);
    assert_eq!(status.deleted_blobs, 0);
    assert!(core.read_blob(&fresh.blob_id).is_ok());

    // A DEDUPED re-upload refreshes that protection: the row is old, the
    // intent to reference it is new.
    let old = core.put_blob(b"OLD-BYTES", NOW).unwrap();
    core.put_blob(b"OLD-BYTES", NOW + 10 * 60_000).unwrap();
    let status = gc_all(&core, cfg, NOW + 10 * 60_000 + 1_000);
    assert_eq!(status.deleted_blobs, 1, "only the genuinely old blob collects");
    assert!(core.read_blob(&old.blob_id).is_ok());
    assert!(matches!(
        core.read_blob(&fresh.blob_id),
        Err(ServerError::NotFound { .. })
    ));

    // Past the window it collects like anything else.
    let status = gc_all(&core, cfg, NOW + 30 * 60_000);
    assert_eq!(status.deleted_blobs, 1);
    assert!(matches!(core.read_blob(&old.blob_id), Err(ServerError::NotFound { .. })));
}

#[test]
fn a_crash_between_row_delete_and_unlink_leaves_a_consistent_store() {
    let root = test_root("gc_crash");
    let doomed_id;
    let orphan;
    {
        let core = AssetServerCore::open(&root, Budgets::default_v1()).unwrap();
        let (id, _) = publish_prop(&core, "ns", 9, b"CRASH-GLB", b"CRASH-THUMB", NOW);
        doomed_id = id;
        orphan = core.put_blob(b"CRASH-ORPHAN", NOW).unwrap().blob_id;
        core.catalog().retire_asset(&doomed_id, NOW + 1).unwrap();
        // Simulate the crash window through the catalog-only E1 seam: rows
        // and intents commit, while physical deletion has not started.
        core.gc().begin(collect_cfg(false), NOW + 2).unwrap();
        loop {
            let step = core.gc().step_catalog(NOW + 2).unwrap();
            if !step.deletes.is_empty() {
                assert_eq!(step.deletes.len(), 3);
                break;
            }
        }
    }
    // Restart: recovery resolves every intent, and the objects it unlinked
    // are the ones whose rows were gone.
    let core = AssetServerCore::open(&root, Budgets::default_v1()).unwrap();
    let report = core.recover(NOW + 2).unwrap();
    assert_eq!(report.gc_deletes_resolved, 3);
    for blob in [
        BlobId::hash_of(b"CRASH-GLB"),
        BlobId::hash_of(b"CRASH-THUMB"),
        orphan,
    ] {
        assert!(!core.cas().contains(&blob), "object survived recovery: {blob}");
    }
    // And the store is usable: a fresh publication of the same bytes works
    // (the CAS has no half-deleted object shadowing them).
    let (fresh, _) = publish_prop(&core, "ns", 10, b"CRASH-GLB", b"CRASH-THUMB", NOW + 3);
    assert_eq!(core.read_blob(&BlobId::hash_of(b"CRASH-GLB")).unwrap(), b"CRASH-GLB");
    assert!(core.catalog().asset_retired_ms(&fresh).unwrap().is_none());
}

#[test]
fn recovery_keeps_bytes_that_were_re_uploaded_before_the_unlink() {
    let root = test_root("gc_crash_reupload");
    {
        let core = AssetServerCore::open(&root, Budgets::default_v1()).unwrap();
        core.put_blob(b"RESURRECTED", NOW).unwrap();
        // Crash window as above: row gone, intent recorded, object still on
        // disk.
        core.gc().begin(collect_cfg(false), NOW + 1).unwrap();
        loop {
            let step = core.gc().step_catalog(NOW + 1).unwrap();
            if !step.deletes.is_empty() {
                assert_eq!(step.deletes.len(), 1);
                break;
            }
        }
    }
    let core = AssetServerCore::open(&root, Budgets::default_v1()).unwrap();
    // Someone uploads the same bytes again before recovery runs: the CAS
    // dedups against the object still on disk, so unlinking it now would
    // leave a catalog row with no bytes.
    core.put_blob(b"RESURRECTED", NOW + 1).unwrap();
    let report = core.recover(NOW + 2).unwrap();
    assert_eq!(report.gc_deletes_resolved, 1);
    assert_eq!(core.read_blob(&BlobId::hash_of(b"RESURRECTED")).unwrap(), b"RESURRECTED");
}

#[test]
fn retention_retires_superseded_revisions_and_never_a_live_head() {
    let (_root, core) = open_core("gc_retention");
    let id = asset_id_n(11);
    core.catalog().register_asset(&id, "ns", NOW).unwrap();
    let mut revs = Vec::new();
    for i in 0..4u8 {
        let glb = format!("RETAIN-GLB-{i}").into_bytes();
        let thumb = format!("RETAIN-THUMB-{i}").into_bytes();
        core.put_blob(&glb, NOW + i as u64).unwrap();
        core.put_blob(&thumb, NOW + i as u64).unwrap();
        let bytes = prop_manifest(id, &glb, &thumb).to_canonical_bytes().unwrap();
        let rev = core.catalog().stage_asset_revision(&bytes, NOW + i as u64).unwrap();
        core.catalog().publish_asset(&id, &rev, NOW + i as u64).unwrap();
        revs.push(rev);
    }
    // The alias pins the OLDEST revision on purpose: retention must not
    // touch what is being served, however old it is.
    let a = alias("ns/pinned");
    core.catalog()
        .set_asset_alias(&a, &AssetRevisionRef { asset_id: id, revision: revs[0] }, NOW + 4)
        .unwrap();

    let cfg = GcConfig {
        retain_keep: Some(1),
        grace_ms: 0,
        ..GcConfig::default_v1()
    };
    let status = gc_all(&core, cfg, NOW + 5);
    // Kept: the newest revision and the aliased one. Retired: the two in
    // between.
    assert_eq!(status.retired_revisions, 2);
    assert!(!core.catalog().revision_retired(&id, &revs[0]).unwrap());
    assert!(core.catalog().revision_retired(&id, &revs[1]).unwrap());
    assert!(core.catalog().revision_retired(&id, &revs[2]).unwrap());
    assert!(!core.catalog().revision_retired(&id, &revs[3]).unwrap());
    assert!(core.catalog().resolve_asset_alias(&a).unwrap().is_some());
    // Their bytes went with them, and only theirs.
    for i in [1u8, 2] {
        let glb = format!("RETAIN-GLB-{i}").into_bytes();
        assert!(matches!(
            core.read_blob(&BlobId::hash_of(&glb)),
            Err(ServerError::NotFound { .. })
        ));
    }
    for i in [0u8, 3] {
        let glb = format!("RETAIN-GLB-{i}").into_bytes();
        assert!(core.read_blob(&BlobId::hash_of(&glb)).is_ok());
    }
    assert_eq!(status.deleted_blobs, 4);
}

#[test]
fn a_retention_dry_run_previews_without_retiring_anything() {
    let (_root, core) = open_core("gc_retention_dry");
    let id = asset_id_n(12);
    core.catalog().register_asset(&id, "ns", NOW).unwrap();
    let mut revs = Vec::new();
    for i in 0..3u8 {
        let glb = format!("PREVIEW-GLB-{i}").into_bytes();
        let thumb = format!("PREVIEW-THUMB-{i}").into_bytes();
        core.put_blob(&glb, NOW + i as u64).unwrap();
        core.put_blob(&thumb, NOW + i as u64).unwrap();
        let bytes = prop_manifest(id, &glb, &thumb).to_canonical_bytes().unwrap();
        let rev = core.catalog().stage_asset_revision(&bytes, NOW + i as u64).unwrap();
        core.catalog().publish_asset(&id, &rev, NOW + i as u64).unwrap();
        revs.push(rev);
    }
    let cfg = GcConfig {
        dry_run: true,
        retain_keep: Some(1),
        grace_ms: 0,
        ..GcConfig::default_v1()
    };
    let dry = gc_all(&core, cfg, NOW + 4);
    assert_eq!(dry.retired_revisions, 2);
    assert_eq!(dry.unreferenced_blobs, 4);
    // Nothing actually changed.
    for rev in &revs {
        assert!(!core.catalog().revision_retired(&id, rev).unwrap());
    }
    for i in 0..3u8 {
        let glb = format!("PREVIEW-GLB-{i}").into_bytes();
        assert!(core.read_blob(&BlobId::hash_of(&glb)).is_ok());
    }
    // The real run then deletes exactly what the preview promised.
    let real = gc_all(&core, GcConfig { dry_run: false, ..cfg }, NOW + 5);
    assert_eq!(real.retired_revisions, dry.retired_revisions);
    assert_eq!(real.deleted_blobs, dry.unreferenced_blobs);
}

#[test]
fn one_run_at_a_time_and_a_cancel_stops_it_without_losing_the_store() {
    let (_root, core) = open_core("gc_lifecycle");
    let (id, _) = publish_prop(&core, "ns", 13, b"LIFE-GLB", b"LIFE-THUMB", NOW);
    core.catalog().retire_asset(&id, NOW + 1).unwrap();
    core.gc_begin(collect_cfg(false), NOW + 2).unwrap();
    assert!(matches!(
        core.gc_begin(collect_cfg(false), NOW + 2),
        Err(ServerError::Conflict { what: "gc run already active" })
    ));
    assert!(core.gc_cancel(NOW + 3).unwrap());
    assert!(!core.gc_cancel(NOW + 4).unwrap());
    let status = core.gc_status().unwrap().unwrap();
    assert_eq!(status.phase, GcPhase::Cancelled);
    assert!(status.finished());
    // Cancelling loses no content, and the next run still collects.
    let status = gc_all(&core, collect_cfg(false), NOW + 5);
    assert_eq!(status.deleted_blobs, 2);
}

#[test]
fn steps_are_bounded_and_a_run_resumes_from_its_durable_cursor() {
    let (_root, core) = open_core("gc_steps");
    // Six assets, retired, with a batch size of one: the run must take many
    // steps and none of them may do more than a batch of work.
    for i in 0..6u8 {
        let glb = format!("STEP-GLB-{i}").into_bytes();
        let thumb = format!("STEP-THUMB-{i}").into_bytes();
        let (id, _) = publish_prop(&core, "ns", 20 + i, &glb, &thumb, NOW);
        core.catalog().retire_asset(&id, NOW + 1).unwrap();
    }
    let cfg = GcConfig {
        grace_ms: 0,
        mark_batch: 1,
        sweep_batch: 1,
        ..GcConfig::default_v1()
    };
    core.gc_begin(cfg, NOW + 2).unwrap();
    let mut steps = 0;
    loop {
        let status = core.gc_advance(1, NOW + 2).unwrap().unwrap();
        steps += 1;
        if status.finished() {
            assert_eq!(status.deleted_blobs, 12);
            break;
        }
        assert!(steps < 200, "run did not converge");
    }
    // One batch per step means the run genuinely took many steps.
    assert!(steps > 12, "expected an incremental run, took {steps} steps");
    for i in 0..6u8 {
        let glb = format!("STEP-GLB-{i}").into_bytes();
        assert!(matches!(
            core.read_blob(&BlobId::hash_of(&glb)),
            Err(ServerError::NotFound { .. })
        ));
    }
}

#[test]
fn game_revisions_keep_their_own_bytes_alive() {
    let (_root, core) = open_core("gc_games");
    // A game revision names bytes no asset manifest does; GC must see them.
    let (asset_id, asset_rev) = publish_prop(&core, "ns", 30, b"GAME-ASSET-GLB", b"GAME-THUMB", NOW);
    let game_id = game_id_n(31);
    core.catalog().register_game(&game_id, "ns", NOW).unwrap();
    let splash = b"SPLASH-SOURCE".to_vec();
    let toml = b"[game]\nname='x'".to_vec();
    let thumb = b"GAME-CARD-PNG".to_vec();
    for bytes in [&splash, &toml, &thumb] {
        core.put_blob(bytes, NOW).unwrap();
    }
    let locked = AssetRevisionRef { asset_id, revision: asset_rev };
    let lock = ContentLock {
        game_id,
        entries: vec![LockEntry {
            alias: alias("ns/game-asset"),
            asset_id: locked.asset_id,
            revision: locked.revision,
        }],
        closure: vec![locked],
        variant_sets: Vec::new(),
    };
    let lock_bytes = lock.to_canonical_bytes().unwrap();
    core.put_blob(&lock_bytes, NOW).unwrap();
    let manifest = GameRevisionManifest {
        game_id,
        name: "Test Game".into(),
        description: "fixture".into(),
        author: "tester".into(),
        splash_blob: BlobId::hash_of(&splash),
        splash_byte_len: splash.len() as u64,
        manifest_blob: BlobId::hash_of(&toml),
        lock_blob: BlobId::hash_of(&lock_bytes),
        thumbnail: ThumbnailMeta {
            blob: BlobId::hash_of(&thumb),
            media: ThumbnailMedia::Png,
            width: 512,
            height: 512,
            byte_len: thumb.len() as u64,
            views: Vec::new(),
        },
        catalog_snapshot: None,
        search_algorithm_version: 1,
        engine_version: 1,
        protocol_version: 1,
    };
    let manifest_bytes = manifest.to_canonical_bytes().unwrap();
    let grev = core
        .catalog()
        .stage_game_revision(&manifest_bytes, &lock_bytes, NOW)
        .unwrap();
    core.catalog().publish_game(&game_id, &grev, NOW).unwrap();

    let status = gc_all(&core, collect_cfg(false), NOW + 1);
    assert_eq!(status.deleted_blobs, 0);
    for bytes in [&splash, &toml, &thumb, &lock_bytes] {
        assert!(core.read_blob(&BlobId::hash_of(bytes)).is_ok());
    }

    // The lock pins the exact asset revision like a manifest dependency:
    // retiring the asset does not collect bytes a game revision locks.
    core.catalog().retire_asset(&asset_id, NOW + 2).unwrap();
    assert_eq!(gc_all(&core, collect_cfg(false), NOW + 3).deleted_blobs, 0);
    assert!(core.read_blob(&BlobId::hash_of(b"GAME-ASSET-GLB")).is_ok());
}

// ---- dependencies ----------------------------------------------------------

fn publish_clip(core: &AssetServerCore, id_byte: u8, glb: &[u8], thumb: &[u8]) -> AssetRevisionRef {
    let (asset_id, revision) = publish_prop(core, "ns", id_byte, glb, thumb, NOW);
    AssetRevisionRef { asset_id, revision }
}

fn gone(core: &AssetServerCore, bytes: &[u8]) -> bool {
    matches!(core.read_blob(&BlobId::hash_of(bytes)), Err(ServerError::NotFound { .. }))
}

#[test]
fn gc_keeps_what_a_live_revision_depends_on_transitively_even_when_retired() {
    let (_root, core) = open_core("gc_dependencies");
    // edit -> nested edit -> clip, the clip and the nested edit retired.
    let clip = publish_clip(&core, 1, b"CLIP-GLB", b"CLIP-THUMB");
    let nested = publish_edit(&core, 2, b"NESTED-SPLASH", b"NESTED-THUMB", &[clip], NOW);
    let edit = publish_edit(&core, 3, b"EDIT-SPLASH", b"EDIT-THUMB", &[nested], NOW);
    core.catalog().retire_asset(&clip.asset_id, NOW + 1).unwrap();
    core.catalog().retire_asset(&nested.asset_id, NOW + 1).unwrap();

    // A dry run and a real run agree: nothing is garbage while the edit lives.
    assert_eq!(gc_all(&core, collect_cfg(true), NOW + 2).unreferenced_blobs, 0);
    assert_eq!(gc_all(&core, collect_cfg(false), NOW + 3).deleted_blobs, 0);
    for bytes in [&b"CLIP-GLB"[..], b"CLIP-THUMB", b"NESTED-SPLASH", b"NESTED-THUMB"] {
        assert!(!gone(&core, bytes));
    }

    // Retiring the edit releases the whole closure.
    core.catalog().retire_asset(&edit.asset_id, NOW + 4).unwrap();
    assert_eq!(gc_all(&core, collect_cfg(false), NOW + 5).deleted_blobs, 6);
    for bytes in [&b"CLIP-GLB"[..], b"NESTED-SPLASH", b"EDIT-SPLASH", b"EDIT-THUMB"] {
        assert!(gone(&core, bytes));
    }
}

#[test]
fn gc_walks_a_shared_closure_once_in_bounded_steps() {
    let (_root, core) = open_core("gc_dependency_steps");
    // Twenty live edits all pin the same clip, later retired, plus one edit
    // that pins a chain deeper than the resolver's depth limit, every level
    // retired once the next one pins it: all of it keeps its bytes (the walk
    // is bounded by distinct revisions, not depth).
    let shared = publish_clip(&core, 1, b"SHARED-GLB", b"SHARED-THUMB");
    for n in 0..20u8 {
        publish_edit(&core, 10 + n, &[b'E', n], &[b'T', n], &[shared], NOW);
    }
    core.catalog().retire_asset(&shared.asset_id, NOW).unwrap();
    let mut chain = publish_clip(&core, 2, b"DEEP-GLB", b"DEEP-THUMB");
    for level in 0..(limits::MAX_DEPENDENCY_DEPTH as u8 + 2) {
        let next = publish_edit(&core, 40 + level, &[b'C', level], &[b'D', level], &[chain], NOW);
        core.catalog().retire_asset(&chain.asset_id, NOW).unwrap();
        chain = next;
    }
    publish_edit(&core, 90, b"TOP-SPLASH", b"TOP-THUMB", &[chain], NOW);
    core.catalog().retire_asset(&chain.asset_id, NOW).unwrap();

    let cfg = GcConfig { mark_batch: 3, ..collect_cfg(false) };
    let status = gc_all(&core, cfg, NOW + 1);
    assert_eq!(status.deleted_blobs, 0);
    assert!(!gone(&core, b"SHARED-GLB"));
    assert!(!gone(&core, b"DEEP-GLB"));
}

/// Retirement is terminal: a new revision may not start depending on a
/// retired one (its bytes may already be collected), and the refusal leaves
/// nothing behind that would keep them.
#[test]
fn admission_refuses_a_retired_dependency() {
    let (_root, core) = open_core("retired_dependency");
    let clip = publish_clip(&core, 1, b"CLIP-GLB", b"CLIP-THUMB");
    core.catalog().retire_asset(&clip.asset_id, NOW + 1).unwrap();
    core.put_blob(b"EDIT-SPLASH", NOW).unwrap();
    core.put_blob(b"EDIT-THUMB", NOW).unwrap();
    let edit = edit_manifest(asset_id_n(2), b"EDIT-SPLASH", b"EDIT-THUMB", &[clip]);
    core.catalog().register_asset(&edit.asset_id, "ns", NOW).unwrap();
    let bytes = edit.to_canonical_bytes().unwrap();
    assert!(matches!(
        core.catalog().stage_asset_revision(&bytes, NOW + 2),
        Err(ServerError::InvalidState { what: "asset dependency", state: "retired" })
    ));
    // One superseded revision retired, the asset live: refused all the same.
    let (id, r1) = publish_prop(&core, "ns", 3, b"R1-GLB", b"R1-THUMB", NOW);
    core.put_blob(b"R1-THUMB-2", NOW).unwrap();
    publish_manifest(&core, &prop_manifest(id, b"R1-GLB", b"R1-THUMB-2"), NOW + 1);
    core.catalog().retire_revision(&id, &r1, NOW + 2).unwrap();
    let pinned = AssetRevisionRef { asset_id: id, revision: r1 };
    let edit = edit_manifest(asset_id_n(2), b"EDIT-SPLASH", b"EDIT-THUMB", &[pinned]);
    assert!(matches!(
        core.catalog().stage_asset_revision(&edit.to_canonical_bytes().unwrap(), NOW + 3),
        Err(ServerError::InvalidState { what: "asset dependency", state: "retired" })
    ));
    assert!(gc_all(&core, collect_cfg(false), NOW + 4).deleted_blobs >= 1);
    assert!(gone(&core, b"CLIP-GLB"));
}

/// An edit admitted during a run's sweep pins its whole closure, so bytes
/// it names are never swept, even when a dependency is retired right after.
#[test]
fn a_publish_mid_run_pins_its_dependency_closure() {
    let (_root, core) = open_core("gc_pin_dependency");
    let clip = publish_clip(&core, 1, b"CLIP-GLB", b"CLIP-THUMB");
    let old = publish_clip(&core, 3, b"OLD-GLB", b"OLD-THUMB");
    core.catalog().retire_asset(&old.asset_id, NOW + 1).unwrap();

    core.gc_begin(collect_cfg(false), NOW + 2).unwrap();
    let mut status = core.gc_status().unwrap().unwrap();
    while status.phase == GcPhase::Mark {
        status = core.gc_advance(1, NOW + 2).unwrap().unwrap();
    }
    assert_eq!(status.phase, GcPhase::Sweep);
    publish_edit(&core, 2, b"EDIT-SPLASH", b"EDIT-THUMB", &[clip], NOW + 2);
    core.catalog().retire_asset(&clip.asset_id, NOW + 2).unwrap();

    let status = core.gc_advance(10_000, NOW + 2).unwrap().unwrap();
    assert!(status.finished());
    for bytes in [&b"CLIP-GLB"[..], b"CLIP-THUMB", b"EDIT-SPLASH", b"EDIT-THUMB"] {
        assert!(!gone(&core, bytes));
    }
    assert!(gone(&core, b"OLD-GLB"));
}

/// Retention (keep the newest N) never retires a superseded revision a live
/// revision pins: republishing a clip must not break the edits that cut the
/// old one. Once nothing live pins it, the next run trims it; a dry run
/// predicts each real run exactly.
#[test]
fn retention_keeps_a_superseded_revision_a_live_edit_pins() {
    let (_root, core) = open_core("retain_pinned");
    let clip_alias = alias("ns/clip");
    let r1 = publish_clip(&core, 1, b"CLIP-1", b"CLIP-THUMB");
    core.catalog().set_asset_alias(&clip_alias, &r1, NOW).unwrap();
    let edit1 = publish_edit(&core, 2, b"EDIT-1", b"EDIT-THUMB", &[r1], NOW);
    // The clip is republished and its alias moves on.
    core.put_blob(b"CLIP-2", NOW + 1).unwrap();
    let r2 = publish_manifest(&core, &prop_manifest(r1.asset_id, b"CLIP-2", b"CLIP-THUMB"), NOW + 1);
    core.catalog().set_asset_alias(&clip_alias, &r2, NOW + 1).unwrap();

    let keep_one = |dry_run| GcConfig { retain_keep: Some(1), ..collect_cfg(dry_run) };
    let preview = gc_all(&core, keep_one(true), NOW + 2);
    let run = gc_all(&core, keep_one(false), NOW + 3);
    assert_eq!(preview.retired_revisions, run.retired_revisions);
    assert_eq!(run.retired_revisions, 0);
    assert!(!core.catalog().revision_retired(&r1.asset_id, &r1.revision).unwrap());
    assert!(core.catalog().asset_revision_servable(&r1.asset_id, &r1.revision).unwrap());
    assert!(!gone(&core, b"CLIP-1"));

    // The edit moves to r2: its old revision is trimmed in this run, and
    // r1 (visited first, still pinned then) in the next.
    core.put_blob(b"EDIT-2", NOW + 4).unwrap();
    publish_manifest(&core, &edit_manifest(edit1.asset_id, b"EDIT-2", b"EDIT-THUMB", &[r2]), NOW + 4);
    for (expected, now) in [(1, NOW + 5), (1, NOW + 7), (0, NOW + 9)] {
        let preview = gc_all(&core, keep_one(true), now);
        let run = gc_all(&core, keep_one(false), now + 1);
        assert_eq!(preview.retired_revisions, run.retired_revisions);
        assert_eq!(preview.unreferenced_blobs, run.deleted_blobs);
        assert_eq!(run.retired_revisions, expected);
    }
    assert!(core.catalog().revision_retired(&r1.asset_id, &r1.revision).unwrap());
    assert!(core.catalog().revision_retired(&edit1.asset_id, &edit1.revision).unwrap());
    assert!(gone(&core, b"CLIP-1"));
    assert!(gone(&core, b"EDIT-1"));
}

/// A retired revision stays servable (manifest, thumbnail) only while a
/// live revision pins it; a merely quarantined (pulled) one never is.
#[test]
fn a_retired_revision_is_served_only_while_a_live_revision_pins_it() {
    let (_root, core) = open_core("serve_pinned");
    let clip = publish_clip(&core, 1, b"CLIP-GLB", b"CLIP-THUMB");
    let edit = publish_edit(&core, 2, b"EDIT-SPLASH", b"EDIT-THUMB", &[clip], NOW);
    let pulled = publish_clip(&core, 3, b"PULLED-GLB", b"PULLED-THUMB");
    publish_edit(&core, 4, b"EDIT2-SPLASH", b"EDIT2-THUMB", &[pulled], NOW);
    let servable = |r: &AssetRevisionRef| core.catalog().asset_revision_servable(&r.asset_id, &r.revision).unwrap();
    assert!(servable(&clip));

    core.catalog().retire_asset(&clip.asset_id, NOW + 1).unwrap();
    assert!(servable(&clip));
    core.catalog().quarantine_asset(&pulled.asset_id, &pulled.revision, NOW + 1).unwrap();
    assert!(!servable(&pulled));

    core.catalog().retire_asset(&edit.asset_id, NOW + 2).unwrap();
    assert!(!servable(&clip));
    assert!(!servable(&edit));
}

#[test]
fn dependents_lists_live_heads_that_use_an_asset() {
    let (_root, core) = open_core("dependents");
    let page = |id: &AssetId| core.dependents(id, None, 128).unwrap();
    let clip = publish_clip(&core, 1, b"CLIP-GLB", b"CLIP-THUMB");
    let other = publish_clip(&core, 2, b"OTHER-GLB", b"OTHER-THUMB");
    assert_eq!(page(&clip.asset_id), DependentsPage { total: 0, rows: vec![], next: None });

    // Unaliased edit: its newest revision is its head.
    let a = publish_edit(&core, 3, b"A1", b"A-THUMB", &[clip], NOW);
    annotate(&core, &a.asset_id, "intro edit", NOW);
    // Aliased edit whose head uses the clip, and a newer unaliased revision
    // that does not: the alias decides.
    let b_head = publish_edit(&core, 4, b"B1", b"B-THUMB", &[clip, other], NOW);
    core.catalog().set_asset_alias(&alias("ns/b"), &b_head, NOW).unwrap();
    publish_edit(&core, 4, b"B2", b"B-THUMB", &[other], NOW + 1);
    // An edit that used the clip only in a superseded revision.
    publish_edit(&core, 5, b"C1", b"C-THUMB", &[clip], NOW);
    publish_edit(&core, 5, b"C2", b"C-THUMB", &[other], NOW + 1);
    // A retired edit.
    let d = publish_edit(&core, 6, b"D1", b"D-THUMB", &[clip], NOW);
    core.catalog().retire_asset(&d.asset_id, NOW + 2).unwrap();
    // Newest by CREATION is the head (as retention orders revisions), even
    // when an older-created revision was published last.
    let e_new = publish_edit(&core, 7, b"E2", b"E-THUMB", &[clip], NOW + 5);
    core.put_blob(b"E1", NOW).unwrap();
    let e_old = edit_manifest(asset_id_n(7), b"E1", b"E-THUMB", &[other]);
    let e_old_rev = core.catalog().stage_asset_revision(&e_old.to_canonical_bytes().unwrap(), NOW + 1).unwrap();
    core.catalog().publish_asset(&e_old.asset_id, &e_old_rev, NOW + 9).unwrap();

    let rows = page(&clip.asset_id);
    let got: Vec<_> = rows.rows.iter().map(|r| (r.asset_id, r.revision, r.kind)).collect();
    assert_eq!(
        got,
        vec![
            (a.asset_id, a.revision, AssetKind::Cut),
            (b_head.asset_id, b_head.revision, AssetKind::Cut),
            (e_new.asset_id, e_new.revision, AssetKind::Cut),
        ]
    );
    assert_eq!((rows.total, rows.next), (3, None));
    assert_eq!(rows.rows[0].title, "intro edit");
    assert!(rows.rows[0].public);
    assert_eq!(rows.rows[1].title, "");
    assert!(!rows.rows[1].public);

    let others: Vec<_> = page(&other.asset_id).rows.iter().map(|r| r.asset_id).collect();
    assert_eq!(others, vec![b_head.asset_id, asset_id_n(5)]);

    // Paging: one row at a time, `total` exact on every page.
    let first = core.dependents(&clip.asset_id, None, 1).unwrap();
    assert_eq!((first.total, first.rows.len(), first.next), (3, 1, Some(a.asset_id)));
    let second = core.dependents(&clip.asset_id, first.next.as_ref(), 1).unwrap();
    assert_eq!((second.rows[0].asset_id, second.next), (b_head.asset_id, Some(b_head.asset_id)));
    let third = core.dependents(&clip.asset_id, second.next.as_ref(), 1).unwrap();
    assert_eq!((third.rows[0].asset_id, third.next), (e_new.asset_id, None));
}

// ---- restore (un-retire) ---------------------------------------------------

fn restore(core: &AssetServerCore, id: &AssetId, now: u64) -> RestoreReport {
    core.catalog().restore_asset(id, now, &mut |_| Ok(())).unwrap()
}

#[test]
fn restoring_an_asset_brings_back_its_revisions_alias_and_annotation() {
    let (_root, core) = open_core("restore_asset");
    let (id, rev) = publish_prop(&core, "ns", 1, b"GLB-A", b"THUMB-A", NOW);
    annotate(&core, &id, "watchtower", NOW);
    let a = alias("ns/watchtower");
    core.catalog()
        .set_asset_alias(&a, &AssetRevisionRef { asset_id: id, revision: rev }, NOW)
        .unwrap();
    core.catalog().retire_asset(&id, NOW + 1).unwrap();
    assert!(search_titles(&core, "watchtower").is_empty());

    let report = restore(&core, &id, NOW + 2);
    assert!(!report.already_live);
    assert_eq!(report.restored.len(), 1);
    assert_eq!(report.restored[0].asset_id, id);
    assert_eq!(report.restored[0].revisions, vec![rev]);
    assert_eq!(report.restored[0].aliases_restored, 1);
    assert!(report.restored[0].annotation_restored);

    // Every read surface retirement removed it from has it again.
    assert!(core.catalog().asset_retired_ms(&id).unwrap().is_none());
    assert_eq!(
        core.catalog().asset_candidate_state(&id, &rev).unwrap(),
        Some(CandidateState::Published)
    );
    assert!(!core.catalog().revision_retired(&id, &rev).unwrap());
    assert!(core.catalog().asset_revision_servable(&id, &rev).unwrap());
    assert_eq!(
        core.catalog().resolve_asset_alias(&a).unwrap(),
        Some(AssetRevisionRef { asset_id: id, revision: rev })
    );
    assert_eq!(search_titles(&core, "watchtower"), vec!["watchtower".to_string()]);
    assert_eq!(search_titles(&core, ""), vec!["watchtower".to_string()]);
    assert_eq!(core.search().annotation(&id).unwrap().unwrap().description, "fixture");

    // Idempotent, and the identity takes new content again.
    assert!(restore(&core, &id, NOW + 3).already_live);
    core.put_blob(b"GLB-B", NOW + 4).unwrap();
    let bytes = prop_manifest(id, b"GLB-B", b"THUMB-A").to_canonical_bytes().unwrap();
    core.catalog().stage_asset_revision(&bytes, NOW + 4).unwrap();

    // Retire -> restore -> retire -> restore round-trips the same way.
    core.catalog().retire_asset(&id, NOW + 5).unwrap();
    let again = restore(&core, &id, NOW + 6);
    assert_eq!(again.restored[0].revisions.len(), 2);
    assert_eq!(again.restored[0].aliases_restored, 1);
    assert_eq!(search_titles(&core, "watchtower"), vec!["watchtower".to_string()]);
}

#[test]
fn restoring_keeps_what_was_pulled_or_retired_before_the_asset_was() {
    let (_root, core) = open_core("restore_keeps_pulled");
    let id = asset_id_n(2);
    core.catalog().register_asset(&id, "ns", NOW).unwrap();
    let mut revs = Vec::new();
    for (glb, t) in [(&b"V1"[..], NOW), (&b"V2"[..], NOW + 1), (&b"V3"[..], NOW + 2), (&b"V4"[..], NOW + 3)] {
        core.put_blob(glb, t).unwrap();
        core.put_blob(b"THUMB", t).unwrap();
        let bytes = prop_manifest(id, glb, b"THUMB").to_canonical_bytes().unwrap();
        revs.push(core.catalog().stage_asset_revision(&bytes, t).unwrap());
    }
    // V1 published then retired alone, V2 published then pulled, V3
    // published, V4 only staged.
    for rev in &revs[..3] {
        core.catalog().publish_asset(&id, rev, NOW + 4).unwrap();
    }
    core.catalog().retire_revision(&id, &revs[0], NOW + 5).unwrap();
    core.catalog().quarantine_asset(&id, &revs[1], NOW + 6).unwrap();
    core.catalog().retire_asset(&id, NOW + 7).unwrap();

    let report = restore(&core, &id, NOW + 8);
    assert_eq!(report.restored[0].revisions, vec![revs[2], revs[3]]);
    let state = |r| core.catalog().asset_candidate_state(&id, r).unwrap();
    assert!(core.catalog().revision_retired(&id, &revs[0]).unwrap());
    assert_eq!(state(&revs[1]), Some(CandidateState::Quarantined));
    assert!(core.catalog().revision_retired(&id, &revs[1]).unwrap());
    assert_eq!(state(&revs[2]), Some(CandidateState::Published));
    assert_eq!(state(&revs[3]), Some(CandidateState::Staged));
    assert!(!core.catalog().revision_retired(&id, &revs[3]).unwrap());
}

#[test]
fn restoring_an_edit_restores_the_retired_assets_it_depends_on() {
    let (_root, core) = open_core("restore_dependencies");
    // edit -> nested edit -> clip, all retired; plus a live prop whose one
    // pinned revision was retired on its own; plus an unrelated retired
    // asset that must stay retired.
    let clip = publish_clip(&core, 1, b"CLIP-GLB", b"CLIP-THUMB");
    let nested = publish_edit(&core, 2, b"NESTED-SPLASH", b"NESTED-THUMB", &[clip], NOW);
    let (prop, old_rev) = publish_prop(&core, "ns", 4, b"PROP-V1", b"PROP-THUMB", NOW);
    let pinned = AssetRevisionRef { asset_id: prop, revision: old_rev };
    let edit = publish_edit(&core, 3, b"EDIT-SPLASH", b"EDIT-THUMB", &[nested, pinned], NOW);
    core.put_blob(b"PROP-V2", NOW).unwrap();
    publish_manifest(&core, &prop_manifest(prop, b"PROP-V2", b"PROP-THUMB"), NOW + 1);
    core.catalog().retire_revision(&prop, &old_rev, NOW + 2).unwrap();
    let (other, _) = publish_prop(&core, "ns", 5, b"OTHER-GLB", b"OTHER-THUMB", NOW);
    for id in [edit.asset_id, nested.asset_id, clip.asset_id, other] {
        core.catalog().retire_asset(&id, NOW + 3).unwrap();
    }

    let report = restore(&core, &edit.asset_id, NOW + 4);
    let ids: Vec<AssetId> = report.restored.iter().map(|r| r.asset_id).collect();
    assert_eq!(ids[0], edit.asset_id);
    assert_eq!(ids.len(), 4);
    for id in [nested.asset_id, clip.asset_id, prop] {
        assert!(ids.contains(&id), "{id} not restored");
    }
    for r in [edit, nested, clip, pinned] {
        assert!(core.catalog().asset_retired_ms(&r.asset_id).unwrap().is_none());
        assert_eq!(
            core.catalog().asset_candidate_state(&r.asset_id, &r.revision).unwrap(),
            Some(CandidateState::Published)
        );
        assert!(!core.catalog().revision_retired(&r.asset_id, &r.revision).unwrap());
    }
    assert!(core.catalog().asset_retired_ms(&other).unwrap().is_some());
}

#[test]
fn restore_refuses_once_gc_collected_the_bytes_and_changes_nothing() {
    let (_root, core) = open_core("restore_after_gc");
    let clip = publish_clip(&core, 1, b"CLIP-GLB", b"CLIP-THUMB");
    let edit = publish_edit(&core, 2, b"EDIT-SPLASH", b"EDIT-THUMB", &[clip], NOW);
    annotate(&core, &edit.asset_id, "cut", NOW);
    core.catalog().retire_asset(&clip.asset_id, NOW + 1).unwrap();
    core.catalog().retire_asset(&edit.asset_id, NOW + 1).unwrap();
    assert_eq!(gc_all(&core, collect_cfg(false), NOW + 2).deleted_blobs, 4);

    assert!(matches!(
        core.catalog().restore_asset(&edit.asset_id, NOW + 3, &mut |_| Ok(())),
        Err(ServerError::InvalidState { what: "restore asset blob", state: "collected" })
    ));
    assert!(core.catalog().asset_retired_ms(&edit.asset_id).unwrap().is_some());
    assert!(core.catalog().asset_retired_ms(&clip.asset_id).unwrap().is_some());
    assert!(search_titles(&core, "cut").is_empty());
}

#[test]
fn a_refused_dependency_namespace_refuses_the_whole_restore() {
    let (_root, core) = open_core("restore_authorize");
    let (clip, clip_rev) = publish_prop(&core, "other", 1, b"CLIP-GLB", b"CLIP-THUMB", NOW);
    let edit = publish_edit(
        &core, 2, b"EDIT-SPLASH", b"EDIT-THUMB",
        &[AssetRevisionRef { asset_id: clip, revision: clip_rev }], NOW,
    );
    core.catalog().retire_asset(&clip, NOW + 1).unwrap();
    core.catalog().retire_asset(&edit.asset_id, NOW + 1).unwrap();

    let refused = core.catalog().restore_asset(&edit.asset_id, NOW + 2, &mut |ns| {
        if ns == "other" { Err(ServerError::Denied { capability: "asset_quarantine" }) } else { Ok(()) }
    });
    assert!(matches!(refused, Err(ServerError::Denied { .. })));
    assert!(core.catalog().asset_retired_ms(&edit.asset_id).unwrap().is_some());
    assert!(core.catalog().asset_retired_ms(&clip).unwrap().is_some());
}

#[test]
fn a_restore_during_a_gc_sweep_pins_what_it_brings_back() {
    let (_root, core) = open_core("restore_mid_run");
    let (id, _) = publish_prop(&core, "ns", 1, b"BACK-GLB", b"BACK-THUMB", NOW);
    core.catalog().retire_asset(&id, NOW + 1).unwrap();
    core.gc_begin(collect_cfg(false), NOW + 2).unwrap();
    let mut status = core.gc_status().unwrap().unwrap();
    while status.phase == GcPhase::Mark {
        status = core.gc_advance(1, NOW + 2).unwrap().unwrap();
    }
    assert_eq!(status.phase, GcPhase::Sweep);
    restore(&core, &id, NOW + 3);
    let status = core.gc_advance(10_000, NOW + 3).unwrap().unwrap();
    assert!(status.finished());
    assert_eq!(status.deleted_blobs, 0);
    assert!(!gone(&core, b"BACK-GLB"));
    assert!(!gone(&core, b"BACK-THUMB"));
}

