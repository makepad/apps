//! Caption tracks of a `Video` as their own files: `CaptionsSrt` /
//! `CaptionsVtt` roles (UTF-8 `Text`), one slot per track (`lod` = track
//! index). The roles are schema v6: a manifest that carries one is written
//! as v6, every other manifest keeps its byte-identical v4 encoding, and a
//! document claiming an older schema may not carry them.

mod common;

use common::*;
use makepad_asset_data::*;

fn file(role: FileRole, media: MediaType, lod: u8, n: u8, len: u64) -> AssetFile {
    AssetFile { role, tier: DeviceTier::Any, lod, media, blob: blob(n), byte_len: len, dims: None }
}

fn video(files: Vec<AssetFile>) -> AssetManifest {
    let total: u64 = files.iter().map(|f| f.byte_len).sum();
    AssetManifest {
        asset_id: asset_id(7),
        kind: AssetKind::Video,
        files,
        dependencies: vec![aref(3, 4)],
        thumbnail: None,
        metrics: Metrics { total_bytes: total, media_millis: 12_000, ..Metrics::default() },
        coordinate_system: CoordinateSystem {
            units_per_meter: 1.0,
            up: Axis::YPos,
            forward: Axis::ZNeg,
            pivot: Pivot::Origin,
        },
        bounds: Bounds { min: Vec3::ZERO, max: Vec3::ZERO },
        anchors: vec![],
        capabilities: Capabilities { loopable: true, ..Capabilities::default() },
        spawn_recipe: None,
        provenance: None,
        rights: cc0_rights("render", ""),
    }
}

fn render_with_two_tracks() -> AssetManifest {
    video(vec![
        file(FileRole::Video, MediaType::Mp4, 0, 1, 90_000),
        file(FileRole::CaptionsSrt, MediaType::Text, 0, 2, 400),
        file(FileRole::CaptionsSrt, MediaType::Text, 1, 3, 380),
        file(FileRole::CaptionsVtt, MediaType::Text, 0, 4, 420),
        file(FileRole::CaptionsVtt, MediaType::Text, 1, 5, 399),
    ])
}

fn schema_of(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[5], bytes[6]])
}

#[test]
fn a_render_carries_srt_and_vtt_per_track_as_schema_6() {
    let m = render_with_two_tracks();
    m.validate().unwrap();
    let bytes = m.to_canonical_bytes().unwrap();
    assert_eq!(schema_of(&bytes), 6);
    assert_eq!(AssetManifest::from_canonical_bytes(&bytes).unwrap(), m);
    assert!(FileRole::CaptionsSrt.allows(MediaType::Text));
    assert!(!FileRole::CaptionsVtt.allows(MediaType::Json));
    assert!(FileRole::CaptionsVtt.is_captions() && !FileRole::Source.is_captions());

    // Without captions the same render keeps the v4 encoding.
    let plain = video(vec![file(FileRole::Video, MediaType::Mp4, 0, 1, 90_000)]);
    assert_eq!(schema_of(&plain.to_canonical_bytes().unwrap()), CONTENT_SCHEMA_VERSION);
}

#[test]
fn caption_roles_are_video_only_text_and_one_slot_per_track() {
    let mut on_prop = render_with_two_tracks();
    on_prop.kind = AssetKind::Cut;
    on_prop.files.push(file(FileRole::Source, MediaType::Text, 0, 6, 10));
    on_prop.files.sort_by_key(|f| (f.role, f.tier, f.lod));
    on_prop.metrics.total_bytes += 10;
    assert_eq!(
        on_prop.validate(),
        Err(AssetDataError::Malformed { what: "captions on a non-video asset" })
    );

    let mut wrong_media = render_with_two_tracks();
    wrong_media.files[1].media = MediaType::Json;
    assert!(matches!(wrong_media.validate(), Err(AssetDataError::Mismatch { .. })));

    let mut twice = render_with_two_tracks();
    twice.files[2].lod = 0;
    assert!(twice.validate().is_err());
}

#[test]
fn an_older_schema_document_may_not_carry_caption_roles() {
    let bytes = render_with_two_tracks().to_canonical_bytes().unwrap();
    for older in [4u16, 5] {
        let mut claimed = bytes.clone();
        claimed[5..7].copy_from_slice(&older.to_be_bytes());
        assert!(matches!(
            AssetManifest::from_canonical_bytes(&claimed),
            Err(AssetDataError::BadTag { what: "FileRole", found: 28 | 29 })
        ));
    }
}
