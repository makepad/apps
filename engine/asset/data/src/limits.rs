//! Frozen admission budgets for the content contract.
//!
//! Every count, length, and metric a manifest or DTO may carry is bounded by a
//! named constant here. Validators enforce these before publication, decoders
//! enforce them before allocation, and clients recheck them before use. There
//! is no unlimited field anywhere in the contract.

/// Hard ceiling for one canonical document (manifest, lock, plan, or DTO).
/// Blobs themselves are unbounded by this; documents only reference them.
pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// General ceiling for any length-prefixed string a decoder will allocate.
pub const MAX_STRING_BYTES: usize = 4096;

/// Human names/titles (asset anchors, components, params, game name, author).
pub const MAX_NAME_BYTES: usize = 64;
/// Game/asset description text.
pub const MAX_DESCRIPTION_BYTES: usize = 4096;
/// A string stored inside a typed `Value`.
pub const MAX_VALUE_STR_BYTES: usize = 1024;

/// Readable alias: total bytes, segment count, and per-segment bytes.
pub const MAX_ALIAS_BYTES: usize = 128;
pub const MIN_ALIAS_SEGMENTS: usize = 2;
pub const MAX_ALIAS_SEGMENTS: usize = 8;
pub const MAX_ALIAS_SEGMENT_BYTES: usize = 48;

/// Stable scene/state key: total bytes, segment count, per-segment bytes.
pub const MAX_KEY_BYTES: usize = 96;
pub const MAX_KEY_SEGMENTS: usize = 8;
pub const MAX_KEY_SEGMENT_BYTES: usize = 48;

/// Non-repeatable file slots per asset. Image lists use the document byte
/// bound so referenced images do not exhaust the slot budget.
pub const MAX_FILES_PER_ASSET: usize = 64;
pub const MAX_DEPENDENCIES_PER_ASSET: usize = 64;
/// Maximum dependency-chain depth a reader (closure resolver, static
/// export) follows. A guard against runaway recursion, not a content rule:
/// the store does not refuse deeper chains and blob GC walks any depth.
/// Sized for nested edits: a render adds one level and an edit that uses
/// another edit's render two, so 32 allows fifteen levels of nesting.
pub const MAX_DEPENDENCY_DEPTH: u32 = 32;
/// Longest catalog label (category or tag), in bytes.
pub const MAX_LABEL_BYTES: usize = 48;
/// Most labels one annotation list (categories, or tags) may carry.
pub const MAX_LABELS: usize = 24;

/// Why a catalog label (category or tag) is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelError {
    /// Empty, or longer than [`MAX_LABEL_BYTES`].
    Length,
    /// Not `^[a-z0-9][a-z0-9_-]*$` (the namespace charset: no `:`, `.`,
    /// `/`, spaces or uppercase).
    Charset,
}

/// THE catalog label rule, shared by the store (which enforces it) and the
/// client (which refuses locally, before any upload): 1..=48 bytes of
/// `^[a-z0-9][a-z0-9_-]*$`, so exact label filters are byte-deterministic.
pub fn check_label(label: &str) -> Result<(), LabelError> {
    if label.is_empty() || label.len() > MAX_LABEL_BYTES {
        return Err(LabelError::Length);
    }
    let b = label.as_bytes();
    if !b[0].is_ascii_lowercase() && !b[0].is_ascii_digit() {
        return Err(LabelError::Charset);
    }
    if !b.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_') {
        return Err(LabelError::Charset);
    }
    Ok(())
}

/// Dependents one `GET /v1/assets/{id}/dependents` page lists (the answer's
/// `total` is always exact; `after` pages on). Sized so a page fits the
/// client's JSON response budget with every title at its maximum length.
pub const MAX_DEPENDENTS_PAGE: usize = 128;
/// Anchors per asset. 64, not 32: a converted Doom map publishes its player
/// starts, walk heights, exit, keys, every door, every lift and every
/// teleport pad, and E1M8-sized maps pass 32 on doors alone — a level that
/// silently drops half its doors is worse than one that refuses.
pub const MAX_ANCHORS_PER_ASSET: usize = 64;
pub const MAX_SPAWN_PARAMS: usize = 64;
pub const MAX_PROVENANCE_PARENTS: usize = 8;

/// Byte ceilings for a file (and an asset's files together) that a caller
/// holds IN MEMORY: publish byte slots, thumbnails, pack imports. Not the
/// contract ceiling for stored content, which is [`MAX_STORED_FILE_BYTES`].
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_ASSET_BYTES: u64 = 1024 * 1024 * 1024;
/// The content contract's absolute ceiling for one stored file
/// (`AssetFile::byte_len`). The binding limit is the HOSTING store's own
/// configured blob budget (`max_blob_bytes`: 16 GiB for Stage's store,
/// 256 MiB by default), which it enforces when the bytes arrive and reports
/// in its health answer; this constant only bounds what any manifest may
/// claim, so no store limit can be configured past a sane size. Raised from
/// 512 MiB: every manifest valid before is valid now; builds older than
/// this refuse manifests naming larger files (fail closed).
pub const MAX_STORED_FILE_BYTES: u64 = 64 << 30;
/// The contract ceiling for all of one asset's stored files together
/// (`Metrics::total_bytes`).
pub const MAX_STORED_ASSET_BYTES: u64 = 128 << 30;

/// Mesh admission ceilings, invariant across visual variants.
pub const MAX_TRIANGLES: u32 = 2_000_000;
pub const MAX_VERTICES: u32 = 4_000_000;
pub const MAX_JOINTS: u16 = 256;
pub const MAX_CLIPS: u16 = 256;
pub const MAX_TEXTURE_DIM: u32 = 8192;
/// Audio/video duration ceiling in milliseconds (`Metrics::media_millis`),
/// for every media kind: six hours, so long renders, recorded sets and
/// films fit. Raised from 15 minutes: every manifest valid before is valid
/// now; builds older than this refuse longer media (fail closed). The field
/// is a `u32`, which holds about 49 days.
pub const MAX_MEDIA_MILLIS: u32 = 6 * 60 * 60 * 1000;
/// Highest LOD index a file variant may declare.
pub const MAX_LOD: u8 = 7;

/// Mandatory mesh thumbnail contract: canonical 512, accepted 256..=4096.
pub const THUMBNAIL_MIN_DIM: u32 = 256;
pub const THUMBNAIL_CANONICAL_DIM: u32 = 512;
pub const THUMBNAIL_MAX_DIM: u32 = 4096;

/// Regions one thumbnail may declare about itself. Eight is generous for
/// everything the catalog bakes today — an audio composite declares two, a
/// sprite sheet one — and keeps the added metadata a rounding error against
/// `MAX_DOCUMENT_BYTES`.
pub const MAX_THUMBNAIL_VIEWS: usize = 8;
/// Ceiling for a declared cycling rate. Above this a "frame" is shorter than
/// a display refresh, so it is a producer bug, not a fast animation.
pub const MAX_THUMBNAIL_FPS: f32 = 240.0;

/// Splash source admission ceiling for a game revision.
pub const MAX_SPLASH_SOURCE_BYTES: u64 = 1024 * 1024;

/// Content lock and content set shape.
pub const MAX_LOCK_ENTRIES: usize = 4096;
pub const MAX_LOCK_CLOSURE: usize = 16384;
pub const MAX_CONTENT_SET_SLOTS: usize = 65536;

/// Scene plan shape.
pub const MAX_SCENE_OBJECTS: usize = 65536;
pub const MAX_COMPONENTS_PER_OBJECT: usize = 32;
pub const MAX_PARAMS_PER_COMPONENT: usize = 64;
pub const MAX_STATE_SCHEMAS: usize = 4096;

/// Migration plan shape.
pub const MAX_MIGRATION_OPS: usize = 65536;
pub const MAX_STATE_MIGRATIONS: usize = 4096;
pub const MAX_MIGRATION_REASONS: usize = 256;
pub const MAX_REASON_DETAIL_BYTES: usize = 256;
pub const MAX_COMPONENT_RULES_PER_OP: usize = 32;

/// Activation DTO shape.
pub const MAX_ORIGINS: usize = 8;
pub const MAX_ORIGIN_BYTES: usize = 256;
pub const MAX_CAPABILITY_BYTES: usize = 512;
pub const MAX_DYNAMIC_SPAWNS: usize = 1024;
pub const MAX_REQUIRED_DELTA: usize = 1024;

/// `units_per_meter` sanity ceiling for a declared coordinate system.
pub const MAX_UNITS_PER_METER: f32 = 10_000.0;

/// Snapshot framing. One chunk stays well below the transport's 256 KiB frame
/// ceiling; the whole snapshot is bounded so a hostile host cannot grow a
/// joining client without limit.
pub const MAX_SNAPSHOT_CHUNK_BYTES: usize = 192 * 1024;
pub const MAX_SNAPSHOT_RECORDS_PER_SECTION: u32 = 262_144;
pub const MAX_SNAPSHOT_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Content/readiness refusal shape.
pub const MAX_REFUSAL_MISSING: usize = 64;
pub const MAX_REFUSAL_DETAIL_BYTES: usize = 256;

/// External-pack import shape. A pack path is the normalized archive-relative
/// source path of one entry file; entry keys reuse the stable-key budget but
/// are capped at fewer segments so `source/pack/key...` still fits an alias.
pub const MAX_IMPORT_ASSETS: usize = 1024;
pub const MAX_IMPORT_FILES_PER_ASSET: usize = 32;
pub const MAX_PACK_PATH_BYTES: usize = 256;
pub const MAX_PACK_VERSION_BYTES: usize = 32;
pub const MAX_IMPORT_KEY_SEGMENTS: usize = 6;

/// Processing recipe / derived variant / variant set shape.
pub const MAX_RECIPE_PARAM_BYTES: usize = 64;
pub const MAX_DERIVED_INPUTS: usize = 16;
pub const MAX_DERIVED_OUTPUTS: usize = 16;
pub const MAX_VARIANTS_PER_SET: usize = 256;
pub const MAX_RESOLVED_ENTRIES: usize = 64;
/// One aggregate realm resolution entry per asset carrying a pinned variant
/// set. Sized so the document stays under `MAX_DOCUMENT_BYTES`.
pub const MAX_REALM_RESOLUTION_ENTRIES: usize = 8192;

/// Recipe parameter domains.
pub const MIN_LOD_TRIANGLES: u32 = 8;
pub const MAX_TRANSCODE_QUALITY: u8 = 100;

/// Rights record shape: exact license identifier (SPDX-style, with version)
/// and an optional terms-revision qualifier.
pub const MAX_LICENSE_BYTES: usize = 128;
pub const MAX_LICENSE_REVISION_BYTES: usize = 64;

/// Readiness deadline ceiling for prepare transactions (10 minutes).
pub const MAX_READINESS_DEADLINE_MILLIS: u32 = 600_000;

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn labels_follow_the_namespace_charset_and_length() {
        for ok in ["music", "flow-preset", "a", "0day", "run_2026-09-27", &"x".repeat(MAX_LABEL_BYTES)] {
            assert_eq!(check_label(ok), Ok(()), "{ok}");
        }
        for bad in ["flow:preset", "Flow", "a.b", "a/b", "a b", "-lead", "_lead", "caf\u{e9}", "tab\t"] {
            assert_eq!(check_label(bad), Err(LabelError::Charset), "{bad}");
        }
        assert_eq!(check_label(""), Err(LabelError::Length));
        assert_eq!(check_label(&"x".repeat(MAX_LABEL_BYTES + 1)), Err(LabelError::Length));
    }
}
