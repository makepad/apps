mod common;

use makepad_asset_data::{
    AssetDataError, AssetFile, AssetKind, AssetManifest, DeviceTier, FileRole, MediaType, Metrics,
};

fn source_manifest(kind: AssetKind) -> AssetManifest {
    let mut manifest = common::weapon_manifest();
    manifest.kind = kind;
    manifest.files = vec![AssetFile {
        role: FileRole::Source,
        tier: DeviceTier::Any,
        lod: 0,
        media: MediaType::Text,
        blob: common::blob(8),
        byte_len: 40,
        dims: None,
    }];
    manifest.dependencies.clear();
    manifest.thumbnail = None;
    manifest.metrics = Metrics {
        total_bytes: 40,
        ..Metrics::default()
    };
    manifest.capabilities.spawnable = false;
    manifest.spawn_recipe = None;
    manifest
}

#[test]
fn edit_and_app_run_round_trip_with_optional_thumbnails() {
    for kind in [AssetKind::Cut, AssetKind::AppRun, AssetKind::Flow, AssetKind::Motion, AssetKind::Storyboard, AssetKind::Score, AssetKind::Deck, AssetKind::Tree] {
        let mut manifest = source_manifest(kind);
        for with_thumbnail in [false, true] {
            if with_thumbnail {
                let thumbnail = common::thumbnail();
                manifest.metrics.total_bytes += thumbnail.byte_len;
                manifest.thumbnail = Some(thumbnail);
            }
            let bytes = manifest.to_canonical_bytes().unwrap();
            let decoded = AssetManifest::from_canonical_bytes(&bytes).unwrap();
            assert_eq!(decoded, manifest);
        }
    }
}

#[test]
fn tree_dependencies_keep_both_revisions_of_a_copied_file() {
    use makepad_asset_data::tree::Tree;
    let a = common::aref(2, 0x21);
    let b = common::aref(2, 0x22);
    let source = format!("file slides/original.splash {a}\nfile slides/copy.splash {b}\n");
    let tree = Tree::parse(&source).unwrap();
    let mut manifest = source_manifest(AssetKind::Tree);
    manifest.dependencies = tree.dependencies();
    manifest.files[0].blob = makepad_asset_data::BlobId::hash_of(source.as_bytes());
    manifest.files[0].byte_len = source.len() as u64;
    manifest.metrics.total_bytes = source.len() as u64;
    Tree::check_manifest(&manifest, source.as_bytes()).unwrap();
    assert_eq!(AssetManifest::from_canonical_bytes(&manifest.to_canonical_bytes().unwrap()).unwrap(), manifest);
    manifest.dependencies.pop();
    assert!(Tree::check_manifest(&manifest, source.as_bytes()).is_err());
    manifest.files.clear();
    manifest.metrics.total_bytes = 0;
    assert!(manifest.validate().is_err());
}

#[test]
fn edit_and_app_run_require_text_source() {
    for (kind, what) in [
        (AssetKind::Cut, "cut source role"),
        (AssetKind::AppRun, "app-run source role"),
        (AssetKind::Flow, "flow source role"),
        (AssetKind::Motion, "motion source role"),
        (AssetKind::Storyboard, "storyboard source role"),
        (AssetKind::Score, "score source role"),
        (AssetKind::Deck, "deck source role"),
    ] {
        let mut missing = source_manifest(kind);
        missing.files.clear();
        missing.metrics.total_bytes = 0;
        assert_eq!(missing.validate(), Err(AssetDataError::Missing { what }));

        let mut wrong_media = source_manifest(kind);
        wrong_media.files[0].media = MediaType::Bin;
        assert_eq!(wrong_media.validate(), Err(AssetDataError::Missing { what }));
    }
}

#[test]
fn edit_pins_dependencies_but_app_run_has_none() {
    let mut edit = source_manifest(AssetKind::Cut);
    edit.dependencies = vec![common::aref(2, 0x21), common::aref(3, 0x31)];
    let bytes = edit.to_canonical_bytes().unwrap();
    assert_eq!(AssetManifest::from_canonical_bytes(&bytes).unwrap(), edit);

    let mut run = source_manifest(AssetKind::AppRun);
    run.dependencies = edit.dependencies;
    assert_eq!(
        run.validate(),
        Err(AssetDataError::Mismatch {
            what: "app-run dependencies must be empty"
        })
    );
}

#[test]
fn a_flow_preset_may_pin_the_revisions_it_names() {
    let mut flow = source_manifest(AssetKind::Flow);
    flow.dependencies = vec![common::aref(2, 0x21)];
    flow.validate().unwrap();
    let bytes = flow.to_canonical_bytes().unwrap();
    assert_eq!(AssetManifest::from_canonical_bytes(&bytes).unwrap(), flow);
}

#[test]
fn a_motion_document_may_pin_the_media_it_uses() {
    let mut motion = source_manifest(AssetKind::Motion);
    motion.validate().unwrap();
    motion.dependencies = vec![common::aref(2, 0x21), common::aref(3, 0x31)];
    motion.validate().unwrap();
    let bytes = motion.to_canonical_bytes().unwrap();
    assert_eq!(AssetManifest::from_canonical_bytes(&bytes).unwrap(), motion);
}

#[test]
fn trees_are_not_limited_to_the_ordinary_asset_dependency_count() {
    let mut manifest = source_manifest(AssetKind::Tree);
    manifest.dependencies = (2..=100).map(|n| common::aref(n, n)).collect();
    manifest.canonicalize();
    let bytes = manifest.to_canonical_bytes().unwrap();
    assert_eq!(AssetManifest::from_canonical_bytes(&bytes).unwrap(), manifest);
}
