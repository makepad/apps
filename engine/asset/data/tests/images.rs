//! Referenced images are a blob-ordered list whose presence promotes a
//! manifest to schema v7 without changing the bytes of older manifests.

mod common;

use common::*;
use makepad_asset_data::limits::{MAX_FILES_PER_ASSET, MAX_LOD};
use makepad_asset_data::*;

fn file(role: FileRole, media: MediaType, n: u8) -> AssetFile {
    AssetFile {
        role,
        tier: DeviceTier::Any,
        lod: 0,
        media,
        blob: blob(n),
        byte_len: 1,
        dims: matches!(media, MediaType::Png | MediaType::Jpeg)
            .then_some(ImageDims { width: 1, height: 1 }),
    }
}

fn model(images: Vec<AssetFile>) -> AssetManifest {
    let mut manifest = weapon_manifest();
    manifest.files = vec![file(FileRole::RenderGlb, MediaType::Glb, 0)];
    manifest.files.extend(images);
    manifest.metrics.total_bytes = manifest.files.iter().map(|f| f.byte_len).sum::<u64>()
        + manifest.thumbnail.as_ref().unwrap().byte_len;
    manifest.canonicalize();
    manifest
}

#[test]
fn images_roundtrip_in_blob_order_as_schema_7() {
    let manifest = model(vec![
        file(FileRole::Image, MediaType::Png, 3),
        file(FileRole::Image, MediaType::Png, 1),
        file(FileRole::Image, MediaType::Bin, 2),
    ]);
    let bytes = manifest.to_canonical_bytes().expect("three images encode");
    assert_eq!(&bytes[5..7], &7u16.to_be_bytes(), "images require schema v7");
    let decoded = AssetManifest::from_canonical_bytes(&bytes).expect("v7 images decode");
    assert_eq!(decoded, manifest, "all image metadata survives the roundtrip");
    assert_eq!(
        decoded.files.iter().filter(|f| f.role == FileRole::Image).map(|f| f.blob).collect::<Vec<_>>(),
        vec![blob(1), blob(2), blob(3)],
        "images canonicalize by blob id within their slot"
    );
    for schema in [4u16, 5, 6] {
        let mut older = bytes.clone();
        older[5..7].copy_from_slice(&schema.to_be_bytes());
        assert!(matches!(
            AssetManifest::from_canonical_bytes(&older),
            Err(AssetDataError::BadTag { what: "FileRole", found: 30 })
        ), "schema {schema} must refuse the Image tag");
    }
}

#[test]
fn image_blobs_and_non_repeatable_slots_stay_unique() {
    let image = file(FileRole::Image, MediaType::Bin, 1);
    let mut duplicate = image;
    for (tier, lod) in [(DeviceTier::Any, 0), (DeviceTier::High, 1)] {
        duplicate.tier = tier;
        duplicate.lod = lod;
        assert!(matches!(
            model(vec![image, duplicate]).validate(),
            Err(AssetDataError::Duplicate { what: "asset file (role, tier, lod, image blob)" })
        ), "one image blob cannot occur twice, even at different tier/LOD coordinates");
    }
    assert!(matches!(
        model(vec![
            file(FileRole::Texture, MediaType::Png, 1),
            file(FileRole::Texture, MediaType::Png, 2),
        ]).validate(),
        Err(AssetDataError::Duplicate { what: "asset file (role, tier, lod, image blob)" })
    ), "different texture blobs must still have different slots");
}

#[test]
fn only_non_repeatable_files_consume_the_slot_budget() {
    let manifest = model((1..=100).map(|n| file(FileRole::Image, MediaType::Bin, n)).collect());
    let bytes = manifest.to_canonical_bytes().expect("100 images and a render GLB fit");
    assert_eq!(AssetManifest::from_canonical_bytes(&bytes).expect("more than 64 files decode"), manifest,
        "the decoder must use the document bound for image lists");
    let mut wrong_total = manifest;
    wrong_total.metrics.total_bytes -= 1;
    assert_eq!(wrong_total.validate(), Err(AssetDataError::Mismatch { what: "metrics total_bytes vs file sum" }),
        "metrics must include every image entry");

    let mut slots = Vec::new();
    for role in [FileRole::Texture, FileRole::Albedo] {
        for tier in [DeviceTier::Any, DeviceTier::Low, DeviceTier::Medium, DeviceTier::High] {
            for lod in 0..=MAX_LOD {
                let mut entry = file(role, MediaType::Bin, 1);
                entry.tier = tier;
                entry.lod = lod;
                slots.push(entry);
            }
        }
    }
    let over = model(slots.clone());
    assert_eq!(over.files.len(), 65, "the construction has 65 distinct non-repeatable slots");
    assert_eq!(over.validate(), Err(AssetDataError::OverBudget {
        what: "asset files", limit: MAX_FILES_PER_ASSET as u64, found: 65,
    }), "non-repeatable files retain the 64-file bound");
    slots.pop();
    assert!(model(slots).validate().is_ok(), "64 distinct non-repeatable slots still fit");
}

#[test]
fn only_images_are_repeatable_and_their_media_is_explicit() {
    for media in [MediaType::Png, MediaType::Jpeg, MediaType::Bin] {
        assert!(FileRole::Image.allows(media), "Image must allow {media:?}");
    }
    assert!(!FileRole::Image.allows(MediaType::Text), "Image must refuse text");
    assert!(FileRole::Image.is_repeatable(), "Image is a list");
    for role in [
        FileRole::RenderGlb, FileRole::Lod1Glb, FileRole::Lod2Glb, FileRole::Collider,
        FileRole::AoMesh, FileRole::ShadowSdf, FileRole::Albedo, FileRole::Normal,
        FileRole::Orm, FileRole::Texture, FileRole::PreviewFront, FileRole::PreviewSide,
        FileRole::Turntable, FileRole::Audio, FileRole::Video, FileRole::Source,
        FileRole::Depth, FileRole::Splat, FileRole::AoTexture, FileRole::StemDrums,
        FileRole::StemBass, FileRole::StemVocals, FileRole::StemOther, FileRole::Lyrics,
        FileRole::DjAnalysis, FileRole::DjLoopSplat, FileRole::MediaTimeline,
        FileRole::MediaTimelinePage, FileRole::CaptionsSrt, FileRole::CaptionsVtt,
    ] {
        assert!(!role.is_repeatable(), "{role:?} must remain a slot");
    }
}
