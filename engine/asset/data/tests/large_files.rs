//! Stored files are bounded by the content contract's absolute ceiling
//! (`MAX_STORED_FILE_BYTES`), not by the in-memory `MAX_FILE_BYTES`: the
//! hosting store's own blob budget is the binding limit. Every manifest that
//! was valid under the old 512 MiB cap stays valid.

mod common;

use common::*;
use makepad_asset_data::limits::{
    MAX_FILE_BYTES, MAX_MEDIA_MILLIS, MAX_STORED_ASSET_BYTES, MAX_STORED_FILE_BYTES,
};
use makepad_asset_data::*;

fn video_of(len: u64) -> AssetManifest {
    AssetManifest {
        asset_id: asset_id(7),
        kind: AssetKind::Video,
        files: vec![AssetFile {
            role: FileRole::Video,
            tier: DeviceTier::Any,
            lod: 0,
            media: MediaType::Mp4,
            blob: blob(1),
            byte_len: len,
            dims: None,
        }],
        dependencies: vec![],
        thumbnail: None,
        metrics: Metrics { total_bytes: len, media_millis: 3_600_000, ..Metrics::default() },
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

#[test]
fn stored_files_may_exceed_the_in_memory_cap_up_to_the_contract_ceiling() {
    // The old cap and a Stage-sized 16 GiB render both validate and round-trip.
    for len in [MAX_FILE_BYTES, MAX_FILE_BYTES + 1, 16 << 30, MAX_STORED_FILE_BYTES] {
        let m = video_of(len);
        m.validate().unwrap();
        let bytes = m.to_canonical_bytes().unwrap();
        assert_eq!(AssetManifest::from_canonical_bytes(&bytes).unwrap(), m);
    }
    assert!(matches!(
        video_of(MAX_STORED_FILE_BYTES + 1).validate(),
        Err(AssetDataError::OverBudget { what: "file byte_len", .. })
    ));
    let mut too_much = video_of(1 << 30);
    too_much.metrics.total_bytes = MAX_STORED_ASSET_BYTES + 1;
    assert!(matches!(too_much.validate(), Err(AssetDataError::OverBudget { .. })));
}

#[test]
fn media_may_run_six_hours_and_old_durations_stay_valid() {
    assert_eq!(MAX_MEDIA_MILLIS, 6 * 60 * 60 * 1000);
    // The old 15-minute ceiling, an hour-long render and exactly six hours.
    for millis in [15 * 60 * 1000, 15 * 60 * 1000 + 1, 60 * 60 * 1000, MAX_MEDIA_MILLIS] {
        let mut m = video_of(1 << 30);
        m.metrics.media_millis = millis;
        m.validate().unwrap();
        let bytes = m.to_canonical_bytes().unwrap();
        assert_eq!(AssetManifest::from_canonical_bytes(&bytes).unwrap(), m);
    }
    let mut over = video_of(1 << 30);
    over.metrics.media_millis = MAX_MEDIA_MILLIS + 1;
    assert!(matches!(
        over.validate(),
        Err(AssetDataError::OverBudget { what: "metrics media_millis", .. })
    ));
}
